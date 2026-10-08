use crate::text_metrics::{
    navigate_single_line, single_line_geometry, single_line_selection_geometry,
};
use mondrian_ui_core::{TextCaretAffinity, TextInputCaretPosition};
use mondrian_ui_text::{SingleLineTextCaret, SingleLineTextNavigation};
use unicode_segmentation::UnicodeSegmentation;

fn grapheme_is_whitespace(grapheme: &str) -> bool {
    grapheme.chars().all(char::is_whitespace)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TextEditState {
    pub(super) text: String,
    pub(super) cursor: TextInputCaretPosition,
    pub(super) selection_start: Option<TextInputCaretPosition>,
}

impl TextEditState {
    pub(super) fn with_text(text: String) -> Self {
        // Programmatic source is authoritative model content. Single-line
        // display projection must never apply the user input/paste edit policy.
        let cursor = TextInputCaretPosition::at_logical_boundary(Self::grapheme_count(&text));
        Self { text, cursor, selection_start: None }
    }
    pub(super) fn text(&self) -> &str {
        &self.text
    }
    pub(super) fn set_text(&mut self, text: String) {
        *self = Self::with_text(text);
    }
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
    pub(super) fn select_all(&mut self) {
        self.cursor = TextInputCaretPosition::at_logical_boundary(self.len_graphemes());
        self.selection_start = Some(TextInputCaretPosition::default());
    }
    pub(super) fn grapheme_count(s: &str) -> usize {
        s.graphemes(true).count()
    }
    pub(super) fn grapheme_byte_idx(&self, index: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .nth(index)
            .map(|(byte, _)| byte)
            .unwrap_or(self.text.len())
    }
    fn byte_caret(&self) -> SingleLineTextCaret {
        SingleLineTextCaret {
            byte_index: self.grapheme_byte_idx(self.cursor.grapheme_index),
            affinity: self.cursor.affinity,
        }
    }
    pub(super) fn position_admitted(position: TextInputCaretPosition, count: usize) -> bool {
        position.grapheme_index <= count
            && (position.grapheme_index != 0 || position.affinity == TextCaretAffinity::BeforeNext)
            && (position.grapheme_index != count
                || count == 0
                || position.affinity == TextCaretAffinity::AfterPrevious)
    }
    pub(super) fn len_graphemes(&self) -> usize {
        Self::grapheme_count(&self.text)
    }
    pub(super) fn clear_selection(&mut self) {
        self.selection_start = None;
    }
    pub(super) fn has_selection(&self) -> bool {
        self.selection_start
            .is_some_and(|anchor| anchor.grapheme_index != self.cursor.grapheme_index)
    }
    pub(super) fn selection_byte_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_start?.grapheme_index;
        let cursor = self.cursor.grapheme_index;
        (anchor != cursor).then(|| {
            (
                self.grapheme_byte_idx(anchor.min(cursor)),
                self.grapheme_byte_idx(anchor.max(cursor)),
            )
        })
    }
    pub(super) fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection_byte_range() else {
            return false;
        };
        let association = self
            .selection_start
            .filter(|anchor| anchor.grapheme_index < self.cursor.grapheme_index)
            .map_or(self.cursor.affinity, |anchor| anchor.affinity);
        self.text.replace_range(start..end, "");
        self.set_forward_byte_endpoint(start, association);
        self.clear_selection();
        true
    }
    pub(super) fn line_geometry(
        &self,
        font_size: f32,
        pointer_x: Option<f32>,
    ) -> Option<mondrian_ui_text::SingleLineTextGeometry> {
        single_line_geometry(&self.text, font_size, self.byte_caret(), pointer_x)
    }
    pub(super) fn selection_geometry(
        &self,
        font_size: f32,
    ) -> Option<mondrian_ui_text::SingleLineTextSelectionGeometry> {
        single_line_selection_geometry(
            &self.text,
            font_size,
            self.byte_caret(),
            self.selection_byte_range().map(|(start, end)| start..end),
        )
    }
    fn set_byte_caret(&mut self, caret: SingleLineTextCaret) {
        if !caret.is_valid_for(&self.text) {
            return;
        }
        self.cursor = TextInputCaretPosition {
            grapheme_index: Self::grapheme_count(&self.text[..caret.byte_index]),
            affinity: caret.affinity,
        };
    }
    pub(super) fn set_cursor_from_text_x(&mut self, x: f32, font_size: f32) {
        if let Some(caret) = self.line_geometry(font_size, Some(x)).and_then(|line| line.hit_caret)
        {
            self.set_byte_caret(caret);
        }
    }
    pub(super) fn cursor_text_x(&self, font_size: f32) -> f32 {
        self.line_geometry(font_size, None).map_or(0.0, |line| line.caret_x)
    }
    pub(super) fn navigate(
        &mut self,
        font_size: f32,
        navigation: SingleLineTextNavigation,
        extend: bool,
    ) {
        let collapse = if extend {
            None
        } else {
            self.selection_byte_range().map(|(start, end)| start..end)
        };
        if let Some(caret) = navigate_single_line(
            &self.text,
            font_size,
            self.byte_caret(),
            navigation,
            collapse,
        ) {
            if extend {
                self.selection_start.get_or_insert(self.cursor);
            } else {
                self.clear_selection();
            }
            self.set_byte_caret(caret);
        }
    }
    pub(super) fn delete_grapheme_before(&mut self) -> bool {
        let cursor = self.cursor.grapheme_index;
        if cursor == 0 {
            return false;
        }
        let start = self.grapheme_byte_idx(cursor - 1);
        let end = self.grapheme_byte_idx(cursor);
        self.text.replace_range(start..end, "");
        self.set_forward_byte_endpoint(start, self.cursor.affinity);
        true
    }
    pub(super) fn delete_grapheme_at(&mut self) -> bool {
        let cursor = self.cursor.grapheme_index;
        if cursor >= self.len_graphemes() {
            return false;
        }
        let start = self.grapheme_byte_idx(cursor);
        let end = self.grapheme_byte_idx(cursor + 1);
        self.text.replace_range(start..end, "");
        self.set_forward_byte_endpoint(start, self.cursor.affinity);
        true
    }
    pub(super) fn insert_normalized_at_cursor(&mut self, text: &str) {
        let byte = self.byte_caret().byte_index;
        self.text.insert_str(byte, text);
        self.set_forward_byte_endpoint(byte + text.len(), TextCaretAffinity::AfterPrevious);
    }
    fn set_forward_byte_endpoint(&mut self, byte: usize, association: TextCaretAffinity) {
        // Complete-text segmentation is essential: a combining mark or ZWJ
        // can join graphemes on either side of the inserted byte interval.
        let mut next = 0;
        let mut boundary = self.text.len();
        for (start, _) in self.text.grapheme_indices(true) {
            if start >= byte {
                boundary = start;
                break;
            }
            next += 1;
        }
        self.cursor = TextInputCaretPosition::at_logical_boundary(next);
        // Preserve a surviving side of a BiDi seam only at an unchanged
        // interior boundary. A joined EGC's forward endpoint belongs to it.
        if boundary == byte && byte != 0 && byte != self.text.len() {
            self.cursor.affinity = association;
        }
    }
    pub(super) fn move_cursor_to(&mut self, position: usize) {
        self.cursor =
            TextInputCaretPosition::at_logical_boundary(position.min(self.len_graphemes()));
    }
    pub(super) fn move_cursor_with_selection(&mut self, position: usize, extend: bool) {
        if extend {
            self.selection_start.get_or_insert(self.cursor);
        } else {
            self.clear_selection();
        }
        self.move_cursor_to(position);
    }
    pub(super) fn next_word_boundary(&self, from: usize) -> usize {
        let total = self.len_graphemes();
        let mut i = from;
        while i < total {
            let b = self.grapheme_byte_idx(i);
            let nb = self.grapheme_byte_idx((i + 1).min(total));
            if !grapheme_is_whitespace(&self.text[b..nb]) {
                i += 1;
            } else {
                break;
            }
        }
        while i < total {
            let b = self.grapheme_byte_idx(i);
            let nb = self.grapheme_byte_idx((i + 1).min(total));
            if grapheme_is_whitespace(&self.text[b..nb]) {
                i += 1;
            } else {
                break;
            }
        }
        i
    }

    pub(super) fn prev_word_boundary(&self, from: usize) -> usize {
        if from == 0 {
            return 0;
        }
        let total = self.len_graphemes();
        let mut i = from.min(total);
        while i > 0 {
            let b = self.grapheme_byte_idx(i - 1);
            let nb = self.grapheme_byte_idx(i.min(total));
            if grapheme_is_whitespace(&self.text[b..nb]) {
                i -= 1;
            } else {
                break;
            }
        }
        while i > 0 {
            let b = self.grapheme_byte_idx(i - 1);
            let nb = self.grapheme_byte_idx(i.min(total));
            if !grapheme_is_whitespace(&self.text[b..nb]) {
                i -= 1;
            } else {
                break;
            }
        }
        i
    }
}

impl super::history::TextHistoryState for TextEditState {
    type Position = TextInputCaretPosition;
    fn history_text(&self) -> &str {
        &self.text
    }
    fn history_cursor(&self) -> TextInputCaretPosition {
        self.cursor
    }
    fn history_anchor(&self) -> Option<TextInputCaretPosition> {
        self.selection_start
    }
    fn history_restore(
        text: &str,
        cursor: TextInputCaretPosition,
        anchor: Option<TextInputCaretPosition>,
    ) -> Self {
        Self {
            text: text.to_owned(),
            cursor,
            selection_start: anchor,
        }
    }
}
