//! 文本渲染器
//!
//! 将文字排版并生成 DrawCommand::Image 列表（每个字形指向 GlyphAtlas UV）。

use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_renderer::command::DrawCommand;

use crate::atlas::{GlyphAtlas, GlyphRasterizeOutcome};
use crate::font::FontManager;
use crate::layout::TextLayout;
use unicode_segmentation::UnicodeSegmentation;

/// Geometry from one complete advanced single-line shaping pass.
///
/// Caret and pointer coordinates refer to the same shaped text as its width;
/// kerning, ligatures and bidirectional runs are never measured as isolated prefixes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SingleLineTextGeometry {
    /// Complete shaped line width in logical pixels.
    pub width: f32,
    /// Complete shaped line height in logical pixels.
    pub height: f32,
    /// Visual caret x coordinate for the requested UTF-8 byte boundary.
    pub caret_x: f32,
    /// Logical UTF-8 byte boundary hit by the optional pointer x coordinate.
    pub hit_byte_index: Option<usize>,
}

/// One visual selected span from the complete advanced single-line layout.
///
/// Coordinates are relative to the complete line origin; disjoint BiDi spans
/// remain separate and contextual glyphs are not reshaped as substrings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SingleLineTextSelectionSpan {
    /// Visual left edge in logical pixels.
    pub x: f32,
    /// Positive visual advance width in logical pixels.
    pub width: f32,
}

/// Caret, hit and selection geometry from one complete advanced shaping pass.
#[derive(Debug, Clone, PartialEq)]
pub struct SingleLineTextSelectionGeometry {
    /// Complete line metrics and requested caret/pointer geometry.
    pub line: SingleLineTextGeometry,
    /// Selected visual advance spans, ordered from left to right.
    pub spans: Vec<SingleLineTextSelectionSpan>,
}

// Reuse cosmic-text's complete glyphs and its equal-grapheme cluster advance
// convention. In an RTL ligature logical grapheme order runs right to left;
// the selected portion must use the glyph's BiDi level, not paragraph direction.
fn selected_glyph_span(
    text: &str,
    glyph: &cosmic_text::LayoutGlyph,
    range: &std::ops::Range<usize>,
) -> Option<SingleLineTextSelectionSpan> {
    if glyph.end <= range.start || glyph.start >= range.end || glyph.w <= 0.0 {
        return None;
    }
    let cluster = text.get(glyph.start..glyph.end)?;
    let total = cluster.graphemes(true).count();
    if total == 0 {
        return None;
    }
    let mut selected_start = None;
    let mut selected_end = 0;
    for (ordinal, (offset, grapheme)) in cluster.grapheme_indices(true).enumerate() {
        let start = glyph.start + offset;
        let end = start + grapheme.len();
        if end > range.start && start < range.end {
            selected_start.get_or_insert(ordinal);
            selected_end = ordinal + 1;
        }
    }
    let selected_start = selected_start?;
    let unit = glyph.w / total as f32;
    let x = if glyph.level.is_rtl() {
        glyph.x + unit * (total - selected_end) as f32
    } else {
        glyph.x + unit * selected_start as f32
    };
    Some(SingleLineTextSelectionSpan {
        x,
        width: unit * (selected_end - selected_start) as f32,
    })
}

fn push_selection_span(
    spans: &mut Vec<SingleLineTextSelectionSpan>,
    span: SingleLineTextSelectionSpan,
) {
    // Merge adjacent glyph advances before allocating another span; large
    // contiguous selections retain one rectangle rather than one per glyph.
    const ADVANCE_ROUNDING_EPSILON: f32 = 0.001;
    if let Some(last) = spans.last_mut()
        && span.x <= last.x + last.width + ADVANCE_ROUNDING_EPSILON
        && span.x + span.width + ADVANCE_ROUNDING_EPSILON >= last.x
    {
        let right = (last.x + last.width).max(span.x + span.width);
        last.x = last.x.min(span.x);
        last.width = right - last.x;
    } else {
        spans.push(span);
    }
}

fn complete_selection_spans(
    layout: &TextLayout,
    range: Option<&std::ops::Range<usize>>,
) -> Vec<SingleLineTextSelectionSpan> {
    let Some(range) = range.filter(|range| range.start < range.end) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    for run in layout.buffer().layout_runs() {
        for glyph in run.glyphs {
            if let Some(span) = selected_glyph_span(run.text, glyph, range) {
                push_selection_span(&mut spans, span);
            }
        }
    }
    // Full layout glyph arrays need not use increasing x for every paragraph
    // direction. Merge in visual order after the allocation-saving first pass.
    spans.sort_by(|a, b| a.x.total_cmp(&b.x));
    let mut merged = Vec::with_capacity(spans.len());
    for span in spans {
        push_selection_span(&mut merged, span);
    }
    merged
}

#[derive(Clone, Copy)]
struct GlyphRenderPolicy {
    allow_atlas_page_reset: bool,
    clip: Option<Rect>,
}

#[derive(Default)]
struct TextResolveViewport {
    clips: Vec<Rect>,
    translations: Vec<Point>,
}

impl TextResolveViewport {
    fn translation(&self) -> Point {
        self.translations.iter().fold(Point::ZERO, |sum, offset| {
            Point::new(sum.x + offset.x, sum.y + offset.y)
        })
    }
    fn apply(&mut self, command: &DrawCommand) {
        match command {
            DrawCommand::PushClip { bounds } => {
                let translation = self.translation();
                let physical = Rect::new(
                    bounds.x + translation.x,
                    bounds.y + translation.y,
                    bounds.width,
                    bounds.height,
                );
                let valid = physical.x.is_finite()
                    && physical.y.is_finite()
                    && physical.width.is_finite()
                    && physical.height.is_finite()
                    && physical.width > 0.0
                    && physical.height > 0.0;
                let clip = if valid {
                    self.clips.last().map_or(physical, |parent| parent.intersection(&physical))
                } else {
                    Rect::ZERO
                };
                self.clips.push(clip);
            }
            DrawCommand::PopClip => {
                self.clips.pop();
            }
            DrawCommand::PushTranslate { offset } => {
                self.translations.push(if offset.x.is_finite() && offset.y.is_finite() {
                    Point::new(offset.x, offset.y)
                } else {
                    Point::ZERO
                });
            }
            DrawCommand::PopTransform => {
                self.translations.pop();
            }
            _ => {}
        }
    }
    fn local_clip(&self) -> Option<Rect> {
        let translation = self.translation();
        self.clips.last().map(|clip| {
            Rect::new(
                clip.x - translation.x,
                clip.y - translation.y,
                clip.width,
                clip.height,
            )
        })
    }
}

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
        self.layout_and_render_with_policy(
            text,
            font_size,
            position,
            color,
            max_width,
            GlyphRenderPolicy { allow_atlas_page_reset, clip: None },
        )
    }

    fn layout_and_render_with_policy(
        &mut self,
        text: &str,
        font_size: f32,
        position: Point,
        color: Color,
        max_width: Option<f32>,
        policy: GlyphRenderPolicy,
    ) -> ResolvedTextCommands {
        if policy.clip.is_some_and(|clip| clip.width <= 0.0 || clip.height <= 0.0) {
            return ResolvedTextCommands {
                commands: Vec::new(),
                stats: TextResolveStats {
                    atlas_generation: self.atlas.generation(),
                    ..TextResolveStats::default()
                },
            };
        }
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
        // Whitespace has layout advance but no image. Use each run's text:
        // glyph byte offsets are line-relative for multiline layouts.
        let atlas_glyphs = layout.runs().into_iter().flat_map(|run| {
            let text = run.text;
            let line_y = run.line_y;
            run.glyphs
                .iter()
                .filter(move |glyph| {
                    !text.get(glyph.start..glyph.end).is_some_and(|span| {
                        !span.is_empty() && span.chars().all(char::is_whitespace)
                    })
                })
                .map(move |glyph| (line_y, glyph))
        });
        for (line_y, glyph) in atlas_glyphs {
            stats.glyphs_requested = stats.glyphs_requested.saturating_add(1);
            match self.atlas.get_or_rasterize_with_clip(
                font_system,
                glyph,
                policy.allow_atlas_page_reset,
                Point::new(position.x, position.y + line_y),
                policy.clip,
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
                GlyphRasterizeOutcome::Clipped => {}
                GlyphRasterizeOutcome::Missing => {
                    stats.missing_glyphs = stats.missing_glyphs.saturating_add(1);
                }
            }
        }
        stats.atlas_generation = self.atlas.generation();
        ResolvedTextCommands { commands, stats }
    }

    /// Shape the complete normalized single line once for width, caret and pointer geometry.
    ///
    /// Returns `None` before shaping for an out-of-range or non-UTF-8 cursor
    /// boundary, or a non-finite/non-positive font size. Pointer hits return byte
    /// boundaries from the same advanced layout; callers map them to their own
    /// logical grapheme coordinates. No draft or glyph context is truncated.
    pub fn single_line_geometry(
        &mut self,
        text: &str,
        font_size: f32,
        cursor_byte_index: usize,
        pointer_x: Option<f32>,
    ) -> Option<SingleLineTextGeometry> {
        self.single_line_selection_geometry(text, font_size, cursor_byte_index, pointer_x, None)
            .map(|geometry| geometry.line)
    }

    /// Shape complete text once for its line, caret, pointer and selection geometry.
    ///
    /// Selection is an ordered UTF-8 byte range within `text`; all overlapping
    /// graphemes use their complete shaped cluster advances. Mixed BiDi ranges
    /// can yield multiple visual spans. Invalid cursor/range/font requests return
    /// `None` before shaping. Empty selections produce no spans. No text context
    /// is truncated and no selected prefix or substring is independently shaped.
    pub fn single_line_selection_geometry(
        &mut self,
        text: &str,
        font_size: f32,
        cursor_byte_index: usize,
        pointer_x: Option<f32>,
        selection_byte_range: Option<std::ops::Range<usize>>,
    ) -> Option<SingleLineTextSelectionGeometry> {
        if !text.is_char_boundary(cursor_byte_index)
            || !font_size.is_finite()
            || font_size <= 0.0
            || selection_byte_range.as_ref().is_some_and(|range| {
                range.start > range.end
                    || !text.is_char_boundary(range.start)
                    || !text.is_char_boundary(range.end)
            })
        {
            return None;
        }
        if text.is_empty() {
            return Some(SingleLineTextSelectionGeometry {
                line: SingleLineTextGeometry {
                    width: 0.0,
                    height: 0.0,
                    caret_x: 0.0,
                    hit_byte_index: pointer_x.map(|_| 0),
                },
                spans: Vec::new(),
            });
        }
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(cosmic_text::Weight::NORMAL);
        let layout =
            TextLayout::new_single_line(&mut self.font_manager.font_system, text, attrs, font_size);
        let (width, height) = layout.size();
        let caret_x = layout
            .buffer()
            .cursor_position(&cosmic_text::Cursor::new(0, cursor_byte_index))
            .map_or(0.0, |(x, _)| x);
        let hit_byte_index = pointer_x.and_then(|x| {
            layout
                .buffer()
                .hit(
                    if x.is_finite() { x } else { 0.0 },
                    layout.line_height() * 0.5,
                )
                .map(|cursor| cursor.index)
        });
        Some(SingleLineTextSelectionGeometry {
            line: SingleLineTextGeometry { width, height, caret_x, hit_byte_index },
            spans: complete_selection_spans(&layout, selection_byte_range.as_ref()),
        })
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
        let mut viewport = TextResolveViewport::default();
        for cmd in commands.iter().cloned() {
            viewport.apply(&cmd);
            match cmd {
                DrawCommand::Text { text, style, position, max_width, color } => {
                    let mut glyph_result = text_renderer.layout_and_render_with_policy(
                        &text,
                        style.font_size,
                        position,
                        color,
                        max_width,
                        GlyphRenderPolicy {
                            allow_atlas_page_reset,
                            clip: viewport.local_clip(),
                        },
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
    fn whitespace_is_not_reported_as_a_missing_glyph() {
        let mut renderer = TextRenderer::new();
        for text in ["A B", "   ", "A\u{00a0}B"] {
            let resolved = renderer.layout_and_render_with_stats(
                text,
                14.0,
                Point::new(0.0, 0.0),
                Color::WHITE,
                None,
                true,
            );
            assert_eq!(resolved.stats.missing_glyphs, 0, "text: {text:?}");
            let visible_glyphs = if text.trim().is_empty() { 0 } else { 2 };
            assert_eq!(resolved.commands.len(), visible_glyphs);
            assert_eq!(resolved.stats.glyphs_resolved as usize, visible_glyphs);
        }
    }

    #[test]
    fn whitespace_preserves_spacing_and_multiline_visible_glyphs() {
        let mut renderer = TextRenderer::new();
        let spaced_width = renderer.measure_text("A B", 14.0).0;
        let compact_width = renderer.measure_text("AB", 14.0).0;
        assert!(spaced_width > compact_width);
        let resolved = renderer.layout_and_render_with_stats(
            " A \n B",
            14.0,
            Point::new(0.0, 0.0),
            Color::WHITE,
            Some(80.0),
            true,
        );
        assert_eq!(resolved.stats.missing_glyphs, 0);
        let bounds = image_bounds(&resolved.commands);
        assert_eq!(bounds.len(), 2);
        assert!(bounds[1].y > bounds[0].y);
    }

    #[test]
    fn visible_glyph_allocation_failure_remains_missing() {
        let mut renderer = TextRenderer::new();
        renderer.atlas = GlyphAtlas::new(1);
        let resolved = renderer.layout_and_render_with_stats(
            "A B",
            14.0,
            Point::new(0.0, 0.0),
            Color::WHITE,
            None,
            false,
        );
        assert_eq!(resolved.stats.glyphs_requested, 2);
        assert_eq!(resolved.stats.missing_glyphs, 2);
        assert!(resolved.commands.is_empty());
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
                    apply_limit_buckets: false,
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

            let mapped = slice.get_mapped_range().expect("text readback mapped range");
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

    #[test]
    fn complete_large_single_line_keeps_every_glyph_and_finite_metrics() {
        let text = "x".repeat(512 * 1024);
        let mut renderer = TextRenderer::new();
        let (width, height) = renderer.measure_text(&text, 14.0);
        assert!(width.is_finite() && width > 100_000.0);
        assert!(height.is_finite() && height > 0.0);
        let layout = TextLayout::new_single_line(
            &mut renderer.font_manager.font_system,
            &text,
            cosmic_text::Attrs::new().family(cosmic_text::Family::SansSerif),
            14.0,
        );
        let glyphs = layout.glyphs();
        assert_eq!(
            glyphs.len(),
            text.len(),
            "the complete draft must remain shaped"
        );
        assert_eq!(glyphs.iter().map(|glyph| glyph.end).max(), Some(text.len()));
        assert_eq!(text.len(), 512 * 1024);
    }

    #[test]
    fn clipped_long_line_emits_only_visible_ink_with_nested_transforms() {
        let mut renderer = TextRenderer::new();
        let text = "MW".repeat(4096);
        let style = mondrian_ui_theme::typography::TextStyle {
            font_size: 14.0,
            line_height: 18.2,
            font_weight: mondrian_ui_theme::typography::FontWeight::Regular,
            letter_spacing: 0.0,
        };
        let outer = Rect::new(100.0, 200.0, 240.0, 80.0);
        let inner_local = Rect::new(20.0, 10.0, 100.0, 40.0);
        let translation = Point::new(100.0, 200.0);
        let expected_clip = Rect::new(120.0, 210.0, 100.0, 40.0);
        let resolved = resolve_text_commands(
            vec![
                DrawCommand::PushClip { bounds: outer },
                DrawCommand::PushTranslate { offset: [translation.x, translation.y].into() },
                DrawCommand::PushClip { bounds: inner_local },
                DrawCommand::Text {
                    text,
                    style,
                    position: Point::new(-20_000.0, 30.0),
                    max_width: None,
                    color: Color::WHITE,
                },
                DrawCommand::PopClip,
                DrawCommand::PopTransform,
                DrawCommand::PopClip,
            ],
            &mut renderer,
        );
        let images = image_bounds(&resolved.commands);
        assert!(
            !images.is_empty(),
            "visible middle of complete shaped line must render"
        );
        assert!(
            images.len() <= 32,
            "clip must bound draw commands, got {}",
            images.len()
        );
        for bounds in &images {
            let physical = Rect::new(
                bounds.x + translation.x,
                bounds.y + translation.y,
                bounds.width,
                bounds.height,
            );
            assert!(
                physical.intersects(&expected_clip),
                "ink outside nested viewport was emitted: {physical:?}"
            );
        }
        assert_eq!(resolved.stats.missing_glyphs, 0);
        assert_eq!(resolved.stats.atlas_page_resets, 0);
        assert_eq!(
            resolved
                .commands
                .iter()
                .filter(|command| matches!(command, DrawCommand::PushClip { .. }))
                .count(),
            2
        );
        assert_eq!(
            resolved
                .commands
                .iter()
                .filter(|command| matches!(command, DrawCommand::PopClip))
                .count(),
            2
        );
    }

    #[test]
    fn empty_effective_clip_does_not_shape_or_allocate_large_text() {
        let mut renderer = TextRenderer::new();
        let style = mondrian_ui_theme::typography::TextStyle {
            font_size: 14.0,
            line_height: 18.2,
            font_weight: mondrian_ui_theme::typography::FontWeight::Regular,
            letter_spacing: 0.0,
        };
        let resolved = resolve_text_commands(
            vec![
                DrawCommand::PushClip { bounds: Rect::new(0.0, 0.0, 100.0, 40.0) },
                DrawCommand::PushClip { bounds: Rect::new(500.0, 500.0, 100.0, 40.0) },
                DrawCommand::Text {
                    text: "x".repeat(512 * 1024),
                    style,
                    position: Point::new(0.0, 20.0),
                    max_width: None,
                    color: Color::WHITE,
                },
                DrawCommand::PopClip,
                DrawCommand::PopClip,
            ],
            &mut renderer,
        );
        assert!(image_bounds(&resolved.commands).is_empty());
        assert_eq!(resolved.stats.glyphs_requested, 0);
        assert!(renderer.take_pending_uploads().is_empty());
    }

    #[test]
    fn complete_line_geometry_rejects_invalid_cursor_and_font_before_shaping() {
        let mut renderer = TextRenderer::new();
        for (text, cursor) in [("你", 1), ("你", 2), ("你", 4), ("x", usize::MAX), ("", 1)] {
            assert!(
                renderer.single_line_geometry(text, 14.0, cursor, Some(2.0)).is_none(),
                "invalid UTF-8/extent cannot masquerade as caret zero"
            );
        }
        for size in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(renderer.single_line_geometry("x", size, 0, None).is_none());
        }
        let empty =
            renderer.single_line_geometry("", 14.0, 0, Some(0.0)).expect("valid empty line");
        assert_eq!(
            empty,
            SingleLineTextGeometry {
                width: 0.0,
                height: 0.0,
                caret_x: 0.0,
                hit_byte_index: Some(0)
            }
        );
    }

    #[test]
    fn complete_line_geometry_preserves_rtl_caret_and_grapheme_hit_boundaries() {
        let mut renderer = TextRenderer::new();
        let text = "שלום";
        let end = renderer
            .single_line_geometry(text, 14.0, text.len(), Some(0.1))
            .expect("valid complete line");
        assert!(end.width > 0.0 && end.height > 0.0);
        assert!(end.caret_x.abs() <= 0.1);
        assert_eq!(end.hit_byte_index, Some(text.len()));
        let start =
            renderer.single_line_geometry(text, 14.0, 0, None).expect("valid logical start");
        assert!((start.caret_x - start.width).abs() <= 0.1);
        let text = "A👩‍💻e\u{301}B";
        let end = renderer
            .single_line_geometry(text, 14.0, text.len(), Some(f32::INFINITY))
            .expect("valid emoji line");
        assert!((end.caret_x - end.width).abs() <= 0.1);
        assert!(end.hit_byte_index.is_some_and(|index| text.is_char_boundary(index)));
    }

    #[test]
    fn complete_selected_geometry_rejects_invalid_range_before_shape() {
        let mut renderer = TextRenderer::new();
        for range in [1..3, 0..4, std::ops::Range { start: 3, end: 0 }] {
            assert!(renderer
                .single_line_selection_geometry("你", 14.0, 0, None, Some(range))
                .is_none());
        }
        assert!(renderer
            .single_line_selection_geometry("你", f32::NAN, 0, None, Some(0..3))
            .is_none());
        let empty = renderer
            .single_line_selection_geometry("", 14.0, 0, None, Some(0..0))
            .expect("valid empty line and selection");
        assert!(empty.spans.is_empty());
        assert_eq!(empty.line.width, 0.0);
    }

    #[test]
    fn complete_selected_geometry_arabic_partial_cluster_matches_complete_caret_advances() {
        let mut renderer = TextRenderer::new();
        let text = "لا";
        let start = renderer.single_line_geometry(text, 24.0, 0, None).expect("RTL line start");
        let end = renderer
            .single_line_geometry(text, 24.0, "ل".len(), None)
            .expect("RTL grapheme boundary");
        let selected = renderer
            .single_line_selection_geometry(text, 24.0, "ل".len(), None, Some(0.."ل".len()))
            .expect("complete partial RTL selection");
        assert!(
            start.caret_x > end.caret_x,
            "the actual complete Arabic glyph run must be RTL"
        );
        assert_eq!(selected.spans.len(), 1);
        assert!((selected.spans[0].x - end.caret_x).abs() <= 0.1);
        assert!(
            (selected.spans[0].width - (start.caret_x - end.caret_x)).abs() <= 0.1,
            "partial ligature coverage must agree with full cluster caret geometry"
        );
        assert_eq!(selected.line, end);
    }

    #[test]
    fn complete_selected_geometry_coalesces_long_contiguous_advances() {
        let mut renderer = TextRenderer::new();
        let text = "x".repeat(131_072);
        let selected = renderer
            .single_line_selection_geometry(&text, 14.0, text.len(), None, Some(0..text.len()))
            .expect("complete large selected line");
        assert_eq!(
            selected.spans.len(),
            1,
            "a contiguous large selection must not return a rectangle per glyph"
        );
        assert!(selected.line.width.is_finite() && selected.line.width > 0.0);
        assert!((selected.spans[0].x).abs() <= 0.1);
        assert!((selected.spans[0].width - selected.line.width).abs() <= 0.1);
    }
}
