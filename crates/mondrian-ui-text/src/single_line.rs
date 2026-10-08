//! Logical caret association and visual geometry from the sole complete layout.

use mondrian_ui_core::TextCaretAffinity;
use unicode_segmentation::UnicodeSegmentation;

use crate::layout::TextLayout;
use crate::render::{
    SingleLineTextGeometry, SingleLineTextSelectionGeometry, SingleLineTextSelectionSpan,
};

/// One logical UTF-8 grapheme boundary associated with an adjacent shaped cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SingleLineTextCaret {
    /// UTF-8 byte boundary in the exact complete source, before display projection.
    pub byte_index: usize,
    /// Which logical adjacent grapheme supplies its visual position at a BiDi seam.
    pub affinity: TextCaretAffinity,
}

impl SingleLineTextCaret {
    /// Default association for programmatic logical synchronization: after the
    /// previous grapheme, or before the next at line start.
    pub const fn at_logical_boundary(byte_index: usize) -> Self {
        if byte_index == 0 {
            Self::before_next(0)
        } else {
            Self::after_previous(byte_index)
        }
    }

    /// Associate a boundary with the next logical grapheme.
    pub const fn before_next(byte_index: usize) -> Self {
        Self {
            byte_index,
            affinity: TextCaretAffinity::BeforeNext,
        }
    }

    /// Associate a boundary with the previous logical grapheme.
    pub const fn after_previous(byte_index: usize) -> Self {
        Self {
            byte_index,
            affinity: TextCaretAffinity::AfterPrevious,
        }
    }

    /// Validate the grapheme boundary and its logical edge association without shaping.
    pub fn is_valid_for(self, text: &str) -> bool {
        if !text.is_char_boundary(self.byte_index) {
            return false;
        }
        if self.byte_index == 0 {
            return self.affinity == TextCaretAffinity::BeforeNext;
        }
        if self.byte_index == text.len() {
            return self.affinity == TextCaretAffinity::AfterPrevious;
        }
        text.grapheme_indices(true).any(|(index, _)| index == self.byte_index)
    }
}

/// A complete single-line caret navigation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleLineTextNavigation {
    /// Move one visual grapheme left, or to the preceding visual whitespace word boundary.
    Left { word: bool },
    /// Move one visual grapheme right, or to the following visual whitespace word boundary.
    Right { word: bool },
    /// Move to logical line start (visual right for a pure RTL line).
    Home,
    /// Move to logical line end (visual left for a pure RTL line).
    End,
}

/// Admit representable line metrics and exact source EGC boundaries before shaping.
///
/// Raw model separators are allowed and displayed by [`SingleLineTextProjection`].
/// UTF-8/EGC/association validation always addresses the original source; numeric
/// draft admission has its own stricter canonical-content contract.
pub fn single_line_request_admitted(
    text: &str,
    font_size: f32,
    caret: SingleLineTextCaret,
    selection: Option<&std::ops::Range<usize>>,
) -> bool {
    font_size.is_finite()
        && font_size > 0.0
        && (font_size * 1.3).is_finite()
        && caret.is_valid_for(text)
        && selection.is_none_or(|range| {
            range.start <= range.end
                && SingleLineTextCaret::at_logical_boundary(range.start).is_valid_for(text)
                && SingleLineTextCaret::at_logical_boundary(range.end).is_valid_for(text)
        })
}

/// A single-line display projection that preserves the exact logical source.
///
/// Authoritative source bytes and grapheme boundaries never change. CR, LF,
/// CRLF and Unicode line/paragraph separators display as one space. This view
/// owns no second author value; unchanged text is borrowed and only separator
/// length changes need a compact byte-range map.
pub struct SingleLineTextProjection<'a> {
    source: &'a str,
    display: std::borrow::Cow<'a, str>,
    contractions: Vec<SingleLineSeparatorContraction>,
}

struct SingleLineSeparatorContraction {
    source_end: usize,
    display_end: usize,
}

impl<'a> SingleLineTextProjection<'a> {
    /// Derive display content from complete source without author mutation.
    pub fn new(source: &'a str) -> Self {
        if !source.contains(['\r', '\n', '\u{2028}', '\u{2029}']) {
            return Self {
                source,
                display: std::borrow::Cow::Borrowed(source),
                contractions: Vec::new(),
            };
        }
        let mut display = String::with_capacity(source.len());
        let mut contractions = Vec::new();
        let mut chars = source.char_indices().peekable();
        while let Some((start, ch)) = chars.next() {
            let mut end = start + ch.len_utf8();
            if matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
                if ch == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
                    chars.next();
                    end += 1;
                }
                display.push(' ');
                if end - start > 1 {
                    contractions.push(SingleLineSeparatorContraction {
                        source_end: end,
                        display_end: display.len(),
                    });
                }
            } else {
                display.push(ch);
            }
        }
        Self {
            source,
            display: std::borrow::Cow::Owned(display),
            contractions,
        }
    }

    /// Exact original model content addressed by caret/selection byte indices.
    pub fn source_text(&self) -> &'a str {
        self.source
    }

    /// Complete content to shape and paint as one visual line.
    pub fn display_text(&self) -> &str {
        &self.display
    }

    fn source_byte(&self, display_byte: usize) -> Option<usize> {
        if !self.display.is_char_boundary(display_byte) {
            return None;
        }
        let count = self.contractions.partition_point(|span| span.display_end <= display_byte);
        let delta = count.checked_sub(1).map_or(0, |index| {
            let span = &self.contractions[index];
            span.source_end - span.display_end
        });
        let source_byte = display_byte.checked_add(delta)?;
        self.source.is_char_boundary(source_byte).then_some(source_byte)
    }

    pub(crate) fn source_range(
        &self,
        range: std::ops::Range<usize>,
    ) -> Option<std::ops::Range<usize>> {
        if range.start > range.end {
            return None;
        }
        Some(self.source_byte(range.start)?..self.source_byte(range.end)?)
    }
}

#[derive(Debug, Clone, Copy)]
struct VisualCell {
    byte_start: usize,
    byte_end: usize,
    left: f32,
    right: f32,
    rtl: bool,
    whitespace: bool,
    group_order: usize,
    cell_order: usize,
}

impl VisualCell {
    fn left_caret(self) -> SingleLineTextCaret {
        if self.rtl {
            SingleLineTextCaret::after_previous(self.byte_end)
        } else {
            SingleLineTextCaret::before_next(self.byte_start)
        }
    }
    fn right_caret(self) -> SingleLineTextCaret {
        if self.rtl {
            SingleLineTextCaret::before_next(self.byte_start)
        } else {
            SingleLineTextCaret::after_previous(self.byte_end)
        }
    }
    fn position(self, caret: SingleLineTextCaret) -> Option<f32> {
        if caret == self.left_caret() {
            Some(self.left)
        } else if caret == self.right_caret() {
            Some(self.right)
        } else {
            None
        }
    }
    fn compare(self, other: Self) -> std::cmp::Ordering {
        self.left
            .total_cmp(&other.left)
            .then_with(|| self.right.total_cmp(&other.right))
            .then_with(|| self.group_order.cmp(&other.group_order))
            .then_with(|| self.cell_order.cmp(&other.cell_order))
    }
}

// A streaming projection of complete shaped glyph clusters. Adjacent glyphs
// for one logical cluster (including marks) provide one advance interval.
// There is no retained all-grapheme caret vector or independent prefix shape.
fn visit_cells(
    layout: &TextLayout,
    projection: &SingleLineTextProjection<'_>,
    mut visit: impl FnMut(VisualCell),
) -> Option<()> {
    for run in layout.buffer().layout_runs() {
        let mut glyph_index = 0;
        while let Some(first) = run.glyphs.get(glyph_index) {
            let first_index = glyph_index;
            let rtl = first.level.is_rtl();
            let start = first.start;
            let end = first.end;
            let mut left = first.x;
            let mut right = first.x + first.w;
            if !left.is_finite() || !right.is_finite() || first.w < 0.0 {
                return None;
            }
            glyph_index += 1;
            while let Some(next) = run.glyphs.get(glyph_index) {
                if next.start != start || next.end != end || next.level.is_rtl() != rtl {
                    break;
                }
                let next_right = next.x + next.w;
                if !next.x.is_finite() || !next_right.is_finite() || next.w < 0.0 {
                    return None;
                }
                left = left.min(next.x);
                right = right.max(next_right);
                glyph_index += 1;
            }
            // Shaping clusters address the display; editing cells address the
            // exact source. Original EGC stops remain distinct even when a
            // projected separator joins a following mark/ZWJ for display.
            let source_range = projection.source_range(start..end)?;
            let start = source_range.start;
            let cluster = projection.source_text().get(source_range)?;
            let total = cluster.graphemes(true).count();
            if total == 0 {
                continue;
            }
            let unit = (right - left) / total as f32;
            if !unit.is_finite() {
                return None;
            }
            for (ordinal, (offset, grapheme)) in cluster.grapheme_indices(true).enumerate() {
                let visual_ordinal = if rtl { total - ordinal - 1 } else { ordinal };
                let cell_left = left + unit * visual_ordinal as f32;
                let cell_right = if visual_ordinal + 1 == total {
                    right
                } else {
                    left + unit * (visual_ordinal + 1) as f32
                };
                if !cell_left.is_finite() || !cell_right.is_finite() {
                    return None;
                }
                visit(VisualCell {
                    byte_start: start + offset,
                    byte_end: start + offset + grapheme.len(),
                    left: cell_left,
                    right: cell_right,
                    rtl,
                    whitespace: grapheme.chars().all(char::is_whitespace),
                    group_order: if run.rtl {
                        usize::MAX - first_index
                    } else {
                        first_index
                    },
                    cell_order: visual_ordinal,
                });
            }
        }
    }
    Some(())
}

fn find_cell(
    layout: &TextLayout,
    projection: &SingleLineTextProjection<'_>,
    caret: SingleLineTextCaret,
) -> Option<VisualCell> {
    let mut found = None;
    visit_cells(layout, projection, |cell| {
        if found.is_none() && cell.position(caret).is_some() {
            found = Some(cell);
        }
    })?;
    found
}

fn extreme_cell(
    layout: &TextLayout,
    projection: &SingleLineTextProjection<'_>,
    rightmost: bool,
    mut admits: impl FnMut(VisualCell) -> bool,
) -> Option<VisualCell> {
    let mut selected: Option<VisualCell> = None;
    visit_cells(layout, projection, |cell| {
        if admits(cell)
            && selected.is_none_or(|previous| {
                if rightmost {
                    cell.compare(previous).is_gt()
                } else {
                    cell.compare(previous).is_lt()
                }
            })
        {
            selected = Some(cell);
        }
    })?;
    selected
}

fn hit_caret(
    layout: &TextLayout,
    projection: &SingleLineTextProjection<'_>,
    x: f32,
) -> Option<SingleLineTextCaret> {
    let x = if x.is_finite() { x } else { 0.0 };
    let mut best: Option<(f32, VisualCell)> = None;
    visit_cells(layout, projection, |cell| {
        let distance = if x < cell.left {
            cell.left - x
        } else if x > cell.right {
            x - cell.right
        } else {
            0.0
        };
        if best.is_none_or(|(previous_distance, previous)| {
            distance < previous_distance
                || (distance == previous_distance
                    && previous.left == previous.right
                    && cell.left < cell.right)
        }) {
            best = Some((distance, cell));
        }
    })?;
    best.map(|(_, cell)| {
        if x < cell.left + (cell.right - cell.left) * 0.5 {
            cell.left_caret()
        } else {
            cell.right_caret()
        }
    })
}

fn push_span(spans: &mut Vec<SingleLineTextSelectionSpan>, span: SingleLineTextSelectionSpan) {
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

pub(crate) fn complete_geometry(
    layout: &TextLayout,
    projection: &SingleLineTextProjection<'_>,
    caret: SingleLineTextCaret,
    pointer_x: Option<f32>,
    selection: Option<&std::ops::Range<usize>>,
) -> Option<SingleLineTextSelectionGeometry> {
    let text = projection.source_text();
    let (width, height) = layout.size();
    if !width.is_finite() || !height.is_finite() || width < 0.0 || height < 0.0 {
        return None;
    }
    let cell = find_cell(layout, projection, caret)?;
    let caret_x = cell.position(caret)?;
    let mut spans = Vec::new();
    if let Some(range) = selection.filter(|range| range.start < range.end) {
        visit_cells(layout, projection, |cell| {
            if cell.byte_end > range.start && cell.byte_start < range.end && cell.right > cell.left
            {
                push_span(
                    &mut spans,
                    SingleLineTextSelectionSpan { x: cell.left, width: cell.right - cell.left },
                );
            }
        })?;
        spans.sort_by(|a, b| a.x.total_cmp(&b.x));
        let mut merged = Vec::with_capacity(spans.len());
        for span in spans {
            push_span(&mut merged, span);
        }
        spans = merged;
    }
    let hit_caret = if let Some(x) = pointer_x {
        let hit = hit_caret(layout, projection, x)?;
        if !hit.is_valid_for(text) {
            return None;
        }
        Some(hit)
    } else {
        None
    };
    Some(SingleLineTextSelectionGeometry {
        line: SingleLineTextGeometry { width, height, caret_x, hit_caret },
        spans,
    })
}

pub(crate) fn navigate_complete(
    layout: &TextLayout,
    projection: &SingleLineTextProjection<'_>,
    caret: SingleLineTextCaret,
    navigation: SingleLineTextNavigation,
    collapse_selection: Option<&std::ops::Range<usize>>,
) -> Option<SingleLineTextCaret> {
    let text = projection.source_text();
    let (width, height) = layout.size();
    if !width.is_finite() || !height.is_finite() || width < 0.0 || height < 0.0 {
        return None;
    }
    match navigation {
        SingleLineTextNavigation::Home => return Some(SingleLineTextCaret::before_next(0)),
        SingleLineTextNavigation::End => {
            return Some(SingleLineTextCaret::after_previous(text.len()))
        }
        _ => {}
    }
    let right = matches!(navigation, SingleLineTextNavigation::Right { .. });
    if let Some(range) = collapse_selection.filter(|range| range.start < range.end) {
        let edge = extreme_cell(layout, projection, right, |cell| {
            cell.byte_end > range.start && cell.byte_start < range.end
        })?;
        return Some(if right {
            edge.right_caret()
        } else {
            edge.left_caret()
        });
    }
    let current = find_cell(layout, projection, caret)?;
    let inward = if right {
        caret == current.left_caret()
    } else {
        caret == current.right_caret()
    };
    let step = if inward {
        Some(current)
    } else {
        extreme_cell(layout, projection, !right, |cell| {
            if right {
                cell.compare(current).is_gt()
            } else {
                cell.compare(current).is_lt()
            }
        })
    };
    let Some(step) = step else {
        return Some(caret);
    };
    let word = matches!(
        navigation,
        SingleLineTextNavigation::Left { word: true }
            | SingleLineTextNavigation::Right { word: true }
    );
    if !word {
        return Some(if right {
            step.right_caret()
        } else {
            step.left_caret()
        });
    }

    // The existing whitespace word policy, applied to actual visual cells.
    // A bounded number of streaming scans keeps one full shape and O(n) scratch,
    // including a very long word, without repeated next-cell scans or prefixes.
    if right {
        let separator = if step.whitespace {
            Some(step)
        } else {
            extreme_cell(layout, projection, false, |cell| {
                cell.whitespace && cell.compare(step).is_gt()
            })
        };
        if let Some(separator) = separator
            && let Some(next_word) = extreme_cell(layout, projection, false, |cell| {
                !cell.whitespace && cell.compare(separator).is_gt()
            })
        {
            return Some(next_word.left_caret());
        }
        return extreme_cell(layout, projection, true, |_| true).map(VisualCell::right_caret);
    }
    let word_cell = if step.whitespace {
        extreme_cell(layout, projection, true, |cell| {
            !cell.whitespace && cell.compare(step).is_lt()
        })
    } else {
        Some(step)
    };
    let Some(word_cell) = word_cell else {
        return extreme_cell(layout, projection, false, |_| true).map(VisualCell::left_caret);
    };
    let separator = extreme_cell(layout, projection, true, |cell| {
        cell.whitespace && cell.compare(word_cell).is_lt()
    });
    extreme_cell(layout, projection, false, |cell| {
        !cell.whitespace
            && !cell.compare(word_cell).is_gt()
            && separator.is_none_or(|separator| cell.compare(separator).is_gt())
    })
    .map(VisualCell::left_caret)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn associated_single_line_admission_requires_complete_grapheme_edges_and_representable_metrics()
    {
        let text = "e\u{301}👩‍💻";
        for invalid in [
            SingleLineTextCaret::after_previous(0),
            SingleLineTextCaret::before_next(text.len()),
            SingleLineTextCaret::after_previous(1),
            SingleLineTextCaret::before_next("e\u{301}👩".len()),
            SingleLineTextCaret::before_next(usize::MAX),
        ] {
            assert!(
                !single_line_request_admitted(text, 14.0, invalid, None),
                "invalid endpoint: {invalid:?}"
            );
        }
        let start = SingleLineTextCaret::before_next(0);
        for size in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::MAX] {
            assert!(
                !single_line_request_admitted(text, size, start, None),
                "unrepresentable line metrics: {size}"
            );
        }
        // A finite representable metric is admitted without an arbitrary size cap.
        assert!(single_line_request_admitted(
            text,
            f32::MAX / 2.0,
            start,
            None
        ));
        assert!(single_line_request_admitted(text, 0.5, start, None));
        assert!(single_line_request_admitted(
            text,
            14.0,
            SingleLineTextCaret::after_previous(text.len()),
            None
        ));
        assert!(!single_line_request_admitted(
            text,
            14.0,
            start,
            Some(&(0..1))
        ));
        assert!(single_line_request_admitted(
            text,
            14.0,
            start,
            Some(&(0.."e\u{301}".len()))
        ));
        for raw in ["x\ry", "x\ny", "x\r\ny", "x\u{2028}y", "x\u{2029}y"] {
            assert!(single_line_request_admitted(raw, 14.0, start, None));
            assert!(single_line_request_admitted(
                raw,
                14.0,
                SingleLineTextCaret::after_previous(raw.len()),
                Some(&(0..raw.len()))
            ));
        }
        assert!(
            !single_line_request_admitted(
                "x\r\ny",
                14.0,
                SingleLineTextCaret::after_previous(2),
                None
            ),
            "caret cannot address the interior of source CRLF"
        );
        assert!(
            !single_line_request_admitted(
                "x\u{2028}y",
                14.0,
                SingleLineTextCaret::after_previous(2),
                None
            ),
            "caret cannot address an interior UTF-8 separator byte"
        );
        assert!(single_line_request_admitted("", 14.0, start, Some(&(0..0))));
    }
    #[test]
    fn source_projection_contract_unchanged_content_borrows_source_without_index_storage() {
        for raw in ["", "A\t\u{a0}e\u{301}👩‍💻שלום"] {
            let projection = SingleLineTextProjection::new(raw);
            assert_eq!(projection.source_text(), raw);
            assert_eq!(projection.display_text(), raw);
            assert!(matches!(&projection.display, std::borrow::Cow::Borrowed(_)));
            assert!(projection.contractions.is_empty());
            assert_eq!(projection.source_range(0..raw.len()), Some(0..raw.len()));
        }
    }

    #[test]
    fn source_projection_contract_only_length_changes_need_mapping_and_ranges_stay_exact() {
        let raw = "A\r\nB\u{2028}C\u{2029}D\rE\nF";
        let projection = SingleLineTextProjection::new(raw);
        assert_eq!(projection.source_text(), raw);
        assert_eq!(projection.display_text(), "A B C D E F");
        assert_eq!(
            projection.contractions.len(),
            3,
            "single-byte CR/LF need no stored index entry"
        );
        for (display, source) in [
            (1..2, 1..3),
            (3..4, 4..7),
            (5..6, 8..11),
            (7..8, 12..13),
            (9..10, 14..15),
        ] {
            assert_eq!(projection.source_range(display), Some(source));
        }
        assert_eq!(projection.source_range(0..11), Some(0..16));
    }

    #[test]
    fn source_projection_contract_refuses_invalid_utf8_bounds_and_never_returns_caret_zero() {
        let raw = "你\r\n👩‍💻";
        let projection = SingleLineTextProjection::new(raw);
        assert_eq!(projection.source_range(3..4), Some(3..5));
        for invalid in [
            1..2,
            4..7,
            std::ops::Range { start: 5, end: 4 },
            0..usize::MAX,
        ] {
            assert_eq!(projection.source_range(invalid), None);
        }
    }

    #[test]
    fn source_projection_contract_display_join_does_not_rewrite_source_grapheme_boundaries() {
        let raw = "a\r\n\u{301}b";
        let projection = SingleLineTextProjection::new(raw);
        assert_eq!(projection.display_text(), "a \u{301}b");
        assert_eq!(raw.graphemes(true).count(), 4);
        assert_eq!(projection.display_text().graphemes(true).count(), 3);
        assert_eq!(projection.source_range(1..2), Some(1..3));
        assert_eq!(projection.source_range(2..4), Some(3..5));
        assert!(SingleLineTextCaret::after_previous(3).is_valid_for(projection.source_text()));
        assert!(!SingleLineTextCaret::after_previous(2).is_valid_for(projection.source_text()));
    }
}
