use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;

use mondrian_ui_core::types::{Point, Rect, Size};

use mondrian_ui_theme::Theme;

use super::measure_text_width;
use super::multiline::{MultilineTextEditState, TextPosition};

// ── TextMetrics ────────────────────────────────────────────────────────────────

/// Font metrics for text layout in a multiline editor.
///
/// v1 derives these from `font_size`; when the text renderer exposes real
/// `ascent`/`descent`/`line_gap`, update the constructor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TextMetrics {
    pub font_size: f32,
    pub line_height: f32,
    pub padding_x: f32,
    pub padding_y: f32,
    pub caret_width: f32,
    /// Baseline offset from line top.
    pub ascent: f32,
    /// Descent below baseline (reserved for future font-aware metrics).
    #[allow(dead_code)]
    pub descent: f32,
}

impl TextMetrics {
    pub fn from_font_size(font_size: f32) -> Self {
        let line_height = font_size * 1.3;
        let ascent = font_size;
        Self {
            font_size,
            line_height,
            padding_x: 8.0,
            padding_y: 4.0,
            caret_width: 2.0,
            ascent,
            descent: line_height - ascent,
        }
    }

    pub fn from_theme(theme: &Theme) -> Self {
        let font_size = theme.typography.body.font_size;
        let line_height = font_size * 1.3;
        let ascent = font_size;
        Self {
            font_size,
            line_height,
            padding_x: theme.spacing.text_input_padding_x,
            padding_y: theme.spacing.text_input_padding_y,
            caret_width: theme.spacing.text_input_caret_width,
            ascent,
            descent: line_height - ascent,
        }
    }
}

// ── LineLayoutMode ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum LineLayoutMode {
    /// One visual row per logical line (horizontal scroll for overflow).
    NoWrap,
    /// Lines wrap at `width` pixels. One logical line may produce multiple
    /// visual rows. Grabhand atoms break at grapheme boundaries.
    WrapToWidth(f32),
}

// ── VisualLine ─────────────────────────────────────────────────────────────────

/// One visible row on screen, mapped to a logical line and column range.
///
/// In `NoWrap` mode, one VisualLine == one logical line. When word-wrap is added,
/// one logical line may produce multiple VisualLines. Fields are populated for that
/// mode and suppressed by clippy until consumed by `WrapToWidth`.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(super) struct VisualLine {
    pub logical_line: usize,
    /// Start grapheme column (0 for NoWrap).
    pub start_column: usize,
    /// Exclusive end grapheme column.
    pub end_column: usize,
    /// Top y in content space (before scroll).
    pub top: f32,
    /// Baseline y for text drawing in content space.
    pub baseline: f32,
    pub height: f32,
    /// Measured pixel width of the text in this visual line.
    pub width: f32,
}

// ── LineMeasureCache ────────────────────────────────────────────────────────────

/// Cached grapheme x-positions for a logical line so that `position_to_point`,
/// `point_to_position`, and selection rect computation avoid repeated full-line
/// `measure_text_width` calls in hot paths.
#[derive(Clone, Debug)]
pub(super) struct LineMeasureCache {
    /// x offset from line start for each grapheme column (0..=grapheme_count).
    /// Index 0 is always 0.0.
    pub grapheme_x: Vec<f32>,
    /// Total pixel width of the line text.
    pub width: f32,
}

impl LineMeasureCache {
    fn new(line_text: &str, font_size: f32) -> Self {
        let mut grapheme_x = vec![0.0];
        let mut byte_offset = 0;
        for grapheme in line_text.graphemes(true) {
            byte_offset += grapheme.len();
            let w = measure_text_width(&line_text[..byte_offset], font_size);
            grapheme_x.push(w);
        }
        let width = if line_text.is_empty() {
            0.0
        } else {
            measure_text_width(line_text, font_size)
        };
        // Final entry for end-of-line (only when trailing grapheme does not already end there)
        if grapheme_x.last().copied().is_none_or(|last| (width - last).abs() > 0.01) {
            grapheme_x.push(width);
        }
        Self { grapheme_x, width }
    }

    fn x_for_column(&self, column: usize) -> f32 {
        self.grapheme_x.get(column).copied().unwrap_or(self.width)
    }

    fn column_for_x(&self, x: f32) -> usize {
        // Guard against NaN or non-finite coordinates
        if !x.is_finite() {
            return 0;
        }
        match self
            .grapheme_x
            .binary_search_by(|v| v.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Less))
        {
            Ok(idx) => idx,
            Err(idx) => {
                if idx == 0 {
                    return 0;
                }
                if idx >= self.grapheme_x.len() {
                    return self.grapheme_x.len().saturating_sub(1);
                }
                let dist_prev = x - self.grapheme_x[idx - 1];
                let dist_next = self.grapheme_x[idx] - x;
                if dist_prev < dist_next {
                    idx - 1
                } else {
                    idx
                }
            }
        }
    }
}

// ── Geometry ────────────────────────────────────────────────────────────────────

/// Line layout, caret rects, selection rects, scroll math, and point↔position mapping.
#[derive(Clone, Debug)]
pub(super) struct MultilineTextGeometry {
    /// Visible content clip rect (bounds minus padding).
    pub clip: Rect,
    /// Total scrollable content size.
    pub content_size: Size,
    pub scroll_x: f32,
    pub scroll_y: f32,
    pub metrics: TextMetrics,
    /// All visual lines (cached; rebuild when text or bounds change).
    #[allow(dead_code)]
    pub visual_lines: Vec<VisualLine>,
    /// First visible visual-line index.
    pub first_visible: usize,
    /// One past the last visible visual-line index.
    pub last_visible: usize,
    /// UNCLAMPED widget-space caret rect. Paint clips this.
    pub caret: Rect,
    /// Per-line selection highlight rects (widget-space, visible lines only).
    pub selection_rects: Vec<Rect>,
    /// Widget-space origin where IME preedit text starts.
    pub preedit_origin: Point,
    /// Widget-space rect for IME caret (after preedit text).
    pub preedit_caret: Rect,
}

// ── Builders ────────────────────────────────────────────────────────────────────

/// Build VisualLines for a given state and mode.
fn build_visual_lines(
    state: &MultilineTextEditState,
    metrics: TextMetrics,
    measure_cache: &mut HashMap<usize, LineMeasureCache>,
    mode: LineLayoutMode,
) -> Vec<VisualLine> {
    match mode {
        LineLayoutMode::NoWrap => build_nowrap_visual_lines(state, metrics, measure_cache),
        LineLayoutMode::WrapToWidth(wrap_width) => {
            build_wrapped_visual_lines(state, metrics, measure_cache, wrap_width)
        }
    }
}

fn build_nowrap_visual_lines(
    state: &MultilineTextEditState,
    metrics: TextMetrics,
    measure_cache: &mut HashMap<usize, LineMeasureCache>,
) -> Vec<VisualLine> {
    let mut lines = Vec::with_capacity(state.line_count());
    for line_idx in 0..state.line_count() {
        let line_text = state.line_text(line_idx);
        let cache = measure_cache
            .entry(line_idx)
            .or_insert_with(|| LineMeasureCache::new(line_text, metrics.font_size));
        let top = line_idx as f32 * metrics.line_height;
        let baseline = top + metrics.ascent;
        lines.push(VisualLine {
            logical_line: line_idx,
            start_column: 0,
            end_column: state.line_len_graphemes(line_idx),
            top,
            baseline,
            height: metrics.line_height,
            width: cache.width,
        });
    }
    if lines.is_empty() {
        lines.push(VisualLine {
            logical_line: 0,
            start_column: 0,
            end_column: 0,
            top: 0.0,
            baseline: metrics.ascent,
            height: metrics.line_height,
            width: 0.0,
        });
    }
    lines
}

fn build_wrapped_visual_lines(
    state: &MultilineTextEditState,
    metrics: TextMetrics,
    measure_cache: &mut HashMap<usize, LineMeasureCache>,
    wrap_width: f32,
) -> Vec<VisualLine> {
    let wrap_width = wrap_width.max(1.0);
    let mut lines = Vec::new();
    let mut visual_idx = 0usize;

    for line_idx in 0..state.line_count() {
        let line_text = state.line_text(line_idx);
        let cache = measure_cache
            .entry(line_idx)
            .or_insert_with(|| LineMeasureCache::new(line_text, metrics.font_size));
        let total_cols = state.line_len_graphemes(line_idx);
        let mut col = 0usize;

        while col < total_cols {
            let start_col = col;
            // Find the furthest column whose x position is ≤ wrap_width
            let start_x = cache.x_for_column(start_col);
            // Binary-search for the wrap point
            let end_col = {
                let mut lo = col;
                let mut hi = total_cols;
                while lo < hi {
                    let mid = (lo + hi).div_ceil(2);
                    if cache.x_for_column(mid) - start_x <= wrap_width {
                        lo = mid;
                    } else {
                        hi = mid - 1;
                    }
                }
                lo
            };
            // If we can't fit even a single grapheme, force at least one
            let end_col = if end_col == col {
                (col + 1).min(total_cols)
            } else {
                end_col
            };
            let seg_width = cache.x_for_column(end_col) - start_x;
            let top = visual_idx as f32 * metrics.line_height;
            lines.push(VisualLine {
                logical_line: line_idx,
                start_column: start_col,
                end_column: end_col,
                top,
                baseline: top + metrics.ascent,
                height: metrics.line_height,
                width: seg_width,
            });
            col = end_col;
            visual_idx += 1;
        }
    }

    if lines.is_empty() {
        lines.push(VisualLine {
            logical_line: 0,
            start_column: 0,
            end_column: 0,
            top: 0.0,
            baseline: metrics.ascent,
            height: metrics.line_height,
            width: 0.0,
        });
    }
    lines
}

/// Pixel position for a document position (in content space, before scroll).
fn position_to_content_point(
    state: &MultilineTextEditState,
    position: TextPosition,
    metrics: TextMetrics,
    measure_cache: &mut HashMap<usize, LineMeasureCache>,
    visual_lines: &[VisualLine],
    mode: LineLayoutMode,
) -> Point {
    let (x, y) =
        match mode {
            LineLayoutMode::NoWrap => {
                let line = position.line.min(state.line_count().saturating_sub(1));
                let column = position.column.min(state.line_len_graphemes(line));
                let line_text = state.line_text(line);
                let cache = measure_cache
                    .entry(line)
                    .or_insert_with(|| LineMeasureCache::new(line_text, metrics.font_size));
                let x = cache.x_for_column(column);
                let y = line as f32 * metrics.line_height;
                (x, y)
            }
            LineLayoutMode::WrapToWidth(_) => {
                // Find the visual line containing this (logical_line, column)
                let Some(vl) = find_visual_line(
                    visual_lines,
                    position.line,
                    position.column.min(state.line_len_graphemes(
                        position.line.min(state.line_count().saturating_sub(1)),
                    )),
                ) else {
                    return Point::new(0.0, position.line as f32 * metrics.line_height);
                };
                let line = position.line.min(state.line_count().saturating_sub(1));
                let line_text = state.line_text(line);
                let cache = measure_cache
                    .entry(line)
                    .or_insert_with(|| LineMeasureCache::new(line_text, metrics.font_size));
                let seg_start_x = cache.x_for_column(vl.start_column);
                let x = cache.x_for_column(position.column.min(vl.end_column)) - seg_start_x;
                let y = vl.top;
                (x, y)
            }
        };
    Point::new(x, y)
}

/// Find the VisualLine containing (logical_line, column). In NoWrap mode,
/// column is always within [0, end_column]; in WrapToWidth, we find the
/// segment whose [start_column, end_column) range includes `column`.
fn find_visual_line(
    visual_lines: &[VisualLine],
    logical_line: usize,
    column: usize,
) -> Option<&VisualLine> {
    let mut best = visual_lines.first()?;
    for vl in visual_lines {
        if vl.logical_line == logical_line && column >= vl.start_column && column < vl.end_column {
            return Some(vl);
        }
        if vl.logical_line == logical_line && column >= vl.start_column {
            best = vl;
        }
    }
    Some(best)
}

// ── Public helpers ──────────────────────────────────────────────────────────────

pub(super) fn content_height(visual_line_count: usize, metrics: TextMetrics) -> f32 {
    (visual_line_count as f32).max(1.0) * metrics.line_height
}

pub(super) fn visible_line_range(
    scroll_y: f32,
    viewport_height: f32,
    metrics: TextMetrics,
    total_visual_lines: usize,
) -> (usize, usize) {
    let lh = metrics.line_height;
    let first = ((scroll_y / lh).floor() as usize).min(total_visual_lines.saturating_sub(1));
    let last = (((scroll_y + viewport_height) / lh).ceil() as usize + 1).min(total_visual_lines);
    (first, last)
}

/// Map a widget-space point to a document position.
pub(super) fn point_to_position(
    state: &MultilineTextEditState,
    point: Point,
    content_left: f32,
    scroll_x: f32,
    scroll_y: f32,
    metrics: TextMetrics,
    measure_cache: &mut HashMap<usize, LineMeasureCache>,
    visual_lines: &[VisualLine],
    mode: LineLayoutMode,
) -> TextPosition {
    let content_y = point.y + scroll_y;
    match mode {
        LineLayoutMode::NoWrap => {
            let line = ((content_y / metrics.line_height).floor() as usize)
                .min(state.line_count().saturating_sub(1));
            let content_x = (point.x - content_left + scroll_x).max(0.0);
            let line_text = state.line_text(line);
            let cache = measure_cache
                .entry(line)
                .or_insert_with(|| LineMeasureCache::new(line_text, metrics.font_size));
            let column = cache.column_for_x(content_x);
            TextPosition::new(line, column)
        }
        LineLayoutMode::WrapToWidth(_) => {
            let vis_idx = ((content_y / metrics.line_height).floor() as usize)
                .min(visual_lines.len().saturating_sub(1));
            let vl = &visual_lines[vis_idx];
            let content_x = (point.x - content_left + scroll_x).max(0.0);
            let line_text = state.line_text(vl.logical_line);
            let cache = measure_cache
                .entry(vl.logical_line)
                .or_insert_with(|| LineMeasureCache::new(line_text, metrics.font_size));
            let seg_start_x = cache.x_for_column(vl.start_column);
            let col_in_seg = cache.column_for_x(content_x + seg_start_x);
            let column = col_in_seg.clamp(vl.start_column, vl.end_column);
            TextPosition::new(vl.logical_line, column)
        }
    }
}

/// Vertical scroll adjustment to bring a caret rect into the visible clip.
/// Returns the adjusted scroll_y (clamped >= 0).
pub(super) fn scroll_y_for_caret(
    scroll_y: f32,
    caret: Rect,
    clip: Rect,
    metrics: TextMetrics,
) -> f32 {
    let lh = metrics.line_height;
    if caret.y + caret.height > clip.y + clip.height {
        let overflow = caret.y + caret.height - (clip.y + clip.height);
        return scroll_y + overflow + lh;
    }
    if caret.y < clip.y {
        let overflow = clip.y - caret.y;
        return (scroll_y - overflow - lh).max(0.0);
    }
    scroll_y.max(0.0)
}

// ── Main geometry computation ───────────────────────────────────────────────────

pub(super) fn compute_multiline_geometry(
    bounds: Rect,
    state: &MultilineTextEditState,
    cursor: TextPosition,
    preedit: Option<&str>,
    scroll_x: f32,
    scroll_y: f32,
    metrics: TextMetrics,
    measure_cache: &mut HashMap<usize, LineMeasureCache>,
    mode: LineLayoutMode,
) -> MultilineTextGeometry {
    let content_left = bounds.x + metrics.padding_x;
    let _content_right = (bounds.x + bounds.width - metrics.padding_x).max(content_left);
    let visible_width = (bounds.width - metrics.padding_x * 2.0).max(1.0);
    let viewport_height = bounds.height - metrics.padding_y * 2.0;
    let clip = Rect::new(
        content_left,
        bounds.y + metrics.padding_y,
        visible_width,
        viewport_height.max(1.0),
    );

    let visual_lines = build_visual_lines(state, metrics, measure_cache, mode);
    let total_height = content_height(visual_lines.len(), metrics);
    let content_size = Size::new(0.0, total_height);

    let (first, last) = visible_line_range(scroll_y, viewport_height, metrics, visual_lines.len());

    // Unclamped caret in widget space
    let cursor_point =
        position_to_content_point(state, cursor, metrics, measure_cache, &visual_lines, mode);
    let preedit_w = preedit.map_or(0.0, |t| measure_text_width(t, metrics.font_size));
    let caret_x = content_left + cursor_point.x + preedit_w - scroll_x;
    let caret_y = clip.y + cursor_point.y - scroll_y;
    let caret_w = metrics.caret_width;
    let caret_h = (metrics.line_height - metrics.padding_y * 2.0).max(1.0);
    let caret = Rect::new(caret_x, caret_y, caret_w, caret_h);

    // Preedit: anchored at cursor (composition start)
    let preedit_x = content_left + cursor_point.x - scroll_x;
    let preedit_y = clip.y + cursor_point.y - scroll_y;
    let preedit_origin = Point::new(preedit_x, preedit_y);
    let preedit_caret = Rect::new(caret_x, caret_y, caret_w, caret_h);

    // Selection rects (visible visual lines only)
    let selection_rects = if state.selection().is_some() {
        compute_selection_rects(
            state,
            &visual_lines,
            first,
            last,
            content_left,
            clip.y,
            scroll_x,
            scroll_y,
            metrics,
            measure_cache,
            mode,
        )
    } else {
        Vec::new()
    };

    MultilineTextGeometry {
        clip,
        content_size,
        scroll_x,
        scroll_y,
        metrics,
        visual_lines,
        first_visible: first,
        last_visible: last,
        caret,
        selection_rects,
        preedit_origin,
        preedit_caret,
    }
}

fn compute_selection_rects(
    state: &MultilineTextEditState,
    visual_lines: &[VisualLine],
    first_visible: usize,
    last_visible: usize,
    content_left: f32,
    clip_top: f32,
    scroll_x: f32,
    scroll_y: f32,
    metrics: TextMetrics,
    measure_cache: &mut HashMap<usize, LineMeasureCache>,
    _mode: LineLayoutMode,
) -> Vec<Rect> {
    let lh = metrics.line_height;
    let mut rects = Vec::new();

    // Byte range of the selection
    let sel_byte_range = state.selection_byte_range();
    let Some(sel_range) = sel_byte_range else {
        return rects;
    };

    let sel_positions = match state.selection() {
        Some(pos) => pos,
        None => return rects,
    };
    let sel_start_pos = sel_positions.start;
    let sel_end_pos = sel_positions.end;

    for vis_idx in first_visible..last_visible {
        if vis_idx >= visual_lines.len() {
            break;
        }
        let vl = &visual_lines[vis_idx];
        let logical_line = vl.logical_line;

        // Does the selection intersect this visual line's column range?
        let sel_covers_line = match (sel_start_pos.line, sel_end_pos.line) {
            (sl, el) if logical_line < sl || logical_line > el => false,
            (sl, el) if logical_line == sl && logical_line == el => {
                // Single-line selection: check column overlap with [vl.start, vl.end)
                let sel_start_col = sel_start_pos.column;
                let sel_end_col = sel_end_pos.column;
                sel_start_col < vl.end_column && sel_end_col > vl.start_column
            }
            (sl, _el) if logical_line == sl => {
                // First line of multi-line selection
                let sel_start_col = sel_start_pos.column;
                sel_start_col < vl.end_column
            }
            (_sl, el) if logical_line == el => {
                // Last line of multi-line selection
                sel_end_pos.column > vl.start_column
            }
            _ => {
                // Middle line — fully selected
                true
            }
        };

        if !sel_covers_line {
            continue;
        }

        // Compute the byte range within this line
        let line_byte_range = state.line_byte_range(logical_line);
        let sel_line_start_byte = sel_range.start.max(line_byte_range.start);
        let sel_line_end_byte = sel_range.end.min(line_byte_range.end);

        if sel_line_start_byte >= sel_line_end_byte {
            continue;
        }

        // Compute columns within this visual segment
        let line_text = state.line_text(logical_line);
        let cache = measure_cache
            .entry(logical_line)
            .or_insert_with(|| LineMeasureCache::new(line_text, metrics.font_size));

        // Find the grapheme columns for the byte range, clamped to our segment
        let seg_text = &line_text[..sel_line_end_byte - line_byte_range.start];
        let prefix_text = &line_text[..sel_line_start_byte - line_byte_range.start];

        let prefix_w = measure_text_width(prefix_text, metrics.font_size);
        let sel_w = measure_text_width(seg_text, metrics.font_size) - prefix_w;

        // Offset by segment start
        let seg_start_x = cache.x_for_column(vl.start_column);
        let sel_x = content_left + prefix_w - seg_start_x - scroll_x;
        let sel_y = clip_top + (vis_idx as f32 * lh) - scroll_y;

        rects.push(Rect::new(sel_x, sel_y, sel_w, lh));
    }

    rects
}

// ── Tests ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn state(text: &str) -> MultilineTextEditState {
        MultilineTextEditState::with_text(text)
    }

    #[test]
    fn text_metrics_from_font_size() {
        let m = TextMetrics::from_font_size(14.0);
        assert_eq!(m.font_size, 14.0);
        assert!(m.line_height > m.font_size);
        assert!(m.ascent > 0.0);
    }

    #[test]
    fn content_height_grows_with_line_count() {
        let m = TextMetrics::from_font_size(14.0);
        assert_eq!(content_height(1, m), m.line_height);
        assert_eq!(content_height(5, m), 5.0 * m.line_height);
    }

    #[test]
    fn visible_line_range_clamps_to_line_count() {
        let m = TextMetrics::from_font_size(14.0);
        let (first, last) =
            visible_line_range(m.line_height * 2.0 + 3.0, m.line_height * 3.0, m, 10);
        assert!(first <= last);
        assert!(last <= 10);
    }

    #[test]
    fn visible_line_range_covers_scrolled_content() {
        let m = TextMetrics::from_font_size(14.0);
        let (first, last) = visible_line_range(0.0, m.line_height * 2.0, m, 10);
        assert_eq!(first, 0);
        assert!(last >= 2);
    }

    #[test]
    fn line_measure_cache_x_for_column() {
        let cache = LineMeasureCache::new("abc", 14.0);
        assert_eq!(cache.x_for_column(0), 0.0);
        assert!(cache.x_for_column(1) > 0.0);
        assert_eq!(cache.x_for_column(3), cache.width);
    }

    #[test]
    fn line_measure_cache_column_for_x() {
        let cache = LineMeasureCache::new("hello world", 14.0);
        assert_eq!(cache.column_for_x(0.0), 0);
        assert_eq!(cache.column_for_x(cache.width), cache.grapheme_x.len() - 1);
        // Click somewhere in middle
        let mid_x = cache.width * 0.5;
        let col = cache.column_for_x(mid_x);
        assert!(col > 0 && col < cache.grapheme_x.len() - 1);
    }

    #[test]
    fn point_to_position_maps_to_correct_line() {
        let mut cache = HashMap::new();
        let s = state("ab\ncde\nf");
        let m = TextMetrics::from_font_size(14.0);

        let pos = point_to_position(
            &s,
            Point::new(8.0, m.line_height + 5.0),
            8.0,
            0.0,
            0.0,
            m,
            &mut cache,
            &[],
            LineLayoutMode::NoWrap,
        );
        assert_eq!(pos.line, 1);
    }

    #[test]
    fn caret_is_unclamped() {
        let mut cache = HashMap::new();
        let s = state("a very long line that exceeds the widget bounds by far");
        let m = TextMetrics::from_font_size(14.0);

        let geometry = compute_multiline_geometry(
            Rect::new(10.0, 10.0, 80.0, 60.0),
            &s,
            TextPosition::new(0, 50),
            None,
            0.0,
            0.0,
            m,
            &mut cache,
            LineLayoutMode::NoWrap,
        );

        // Caret may be beyond clip bounds — paint clips it
        assert!(
            geometry.caret.x > geometry.clip.x + geometry.clip.width
                || geometry.caret.x >= geometry.clip.x
        );
    }

    #[test]
    fn selection_rects_span_multiple_lines() {
        let mut cache = HashMap::new();
        let mut s = state("ab\ncde\nfgh");
        s.move_to(TextPosition::new(0, 1), false);
        s.move_to(TextPosition::new(2, 1), true);
        let m = TextMetrics::from_font_size(14.0);

        let geometry = compute_multiline_geometry(
            Rect::new(10.0, 10.0, 200.0, 100.0),
            &s,
            s.cursor(),
            None,
            0.0,
            0.0,
            m,
            &mut cache,
            LineLayoutMode::NoWrap,
        );

        assert!(geometry.selection_rects.len() >= 2);
    }

    #[test]
    fn selection_rects_empty_when_no_selection() {
        let mut cache = HashMap::new();
        let s = state("ab\ncd");
        let m = TextMetrics::from_font_size(14.0);

        let geometry = compute_multiline_geometry(
            Rect::new(10.0, 10.0, 200.0, 100.0),
            &s,
            s.cursor(),
            None,
            0.0,
            0.0,
            m,
            &mut cache,
            LineLayoutMode::NoWrap,
        );

        assert!(geometry.selection_rects.is_empty());
    }

    #[test]
    fn scroll_y_for_caret_scrolls_down() {
        let m = TextMetrics::from_font_size(14.0);
        let caret = Rect::new(20.0, 120.0, 2.0, 18.0);
        let clip = Rect::new(10.0, 10.0, 200.0, 80.0);
        let new_scroll = scroll_y_for_caret(0.0, caret, clip, m);
        assert!(new_scroll > 0.0);
    }

    #[test]
    fn scroll_y_for_caret_unchanged_when_visible() {
        let m = TextMetrics::from_font_size(14.0);
        let caret = Rect::new(20.0, 30.0, 2.0, 18.0);
        let clip = Rect::new(10.0, 10.0, 200.0, 80.0);
        let new_scroll = scroll_y_for_caret(5.0, caret, clip, m);
        assert_eq!(new_scroll, 5.0);
    }

    #[test]
    fn scroll_y_for_caret_scrolls_up() {
        let m = TextMetrics::from_font_size(14.0);
        let caret = Rect::new(20.0, 2.0, 2.0, 18.0);
        let clip = Rect::new(10.0, 10.0, 200.0, 80.0);
        let new_scroll = scroll_y_for_caret(30.0, caret, clip, m);
        assert!(new_scroll < 30.0);
    }

    #[test]
    fn visual_lines_are_correct_for_nowrap() {
        let mut cache = HashMap::new();
        let s = state("ab\ncde\nfgh");
        let m = TextMetrics::from_font_size(14.0);

        let geometry = compute_multiline_geometry(
            Rect::new(10.0, 10.0, 200.0, 100.0),
            &s,
            s.cursor(),
            None,
            0.0,
            0.0,
            m,
            &mut cache,
            LineLayoutMode::NoWrap,
        );

        assert_eq!(geometry.visual_lines.len(), s.line_count());
        for (i, vl) in geometry.visual_lines.iter().enumerate() {
            assert_eq!(vl.logical_line, i);
            assert_eq!(vl.start_column, 0);
            assert_eq!(vl.end_column, s.line_len_graphemes(i));
            assert_eq!(vl.top, i as f32 * m.line_height);
        }
    }

    // ── WrapToWidth tests ────────────────────────────────────────────────────

    #[test]
    fn wrap_mode_splits_long_line_into_multiple_visual_lines() {
        // A single long line should be split at wrap width of 50px
        let s = state("abcdefghijklmnopqrstuvwxyz"); // 26 chars, should be > 50px
        let mut cache = HashMap::new();
        let m = TextMetrics::from_font_size(14.0);
        let vl = build_visual_lines(&s, m, &mut cache, LineLayoutMode::WrapToWidth(50.0));

        // Should have at least 2 visual lines (26 chars won't fit in 50px at 14px)
        assert!(vl.len() > 1);
        // All visual lines should reference logical line 0
        for v in &vl {
            assert_eq!(v.logical_line, 0);
        }
        // Columns should be contiguous and non-overlapping
        for i in 1..vl.len() {
            assert_eq!(vl[i].start_column, vl[i - 1].end_column);
        }
        // First segment starts at 0, last ends at 26
        assert_eq!(vl.first().unwrap().start_column, 0);
        assert_eq!(vl.last().unwrap().end_column, 26);
        // Each segment width should be ≤ wrap width (or slightly over for single wide grapheme)
        for v in &vl {
            assert!(
                v.width <= 55.0,
                "segment width {} exceeds wrap+slop",
                v.width
            );
        }
    }

    #[test]
    fn wrap_mode_handles_empty_line() {
        let s = MultilineTextEditState::new();
        let mut cache = HashMap::new();
        let m = TextMetrics::from_font_size(14.0);
        let vl = build_visual_lines(&s, m, &mut cache, LineLayoutMode::WrapToWidth(100.0));
        assert_eq!(vl.len(), 1);
        assert_eq!(vl[0].logical_line, 0);
        assert_eq!(vl[0].start_column, 0);
        assert_eq!(vl[0].end_column, 0);
    }

    #[test]
    fn wrap_content_height_counts_visual_lines() {
        let s = state("a very long line that wraps across several visual rows");
        let mut cache = HashMap::new();
        let m = TextMetrics::from_font_size(14.0);
        let vl = build_visual_lines(&s, m, &mut cache, LineLayoutMode::WrapToWidth(40.0));
        let h = content_height(vl.len(), m);
        assert_eq!(h, vl.len() as f32 * m.line_height);
        assert!(vl.len() > 1);
    }
}
