use unicode_segmentation::UnicodeSegmentation;

use super::measure_text_width;

fn grapheme_is_whitespace(grapheme: &str) -> bool {
    grapheme.chars().all(char::is_whitespace)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TextEditState {
    pub(super) text: String,
    /// Cursor position as grapheme cluster index.
    pub(super) cursor: usize,
    /// Selection anchor as grapheme cluster index.
    pub(super) selection_start: Option<usize>,
}

impl TextEditState {
    pub(super) fn with_text(text: String) -> Self {
        let cursor = Self::grapheme_count(&text);
        Self { text, cursor, selection_start: None }
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }

    pub(super) fn set_text(&mut self, text: String) {
        self.cursor = Self::grapheme_count(&text);
        self.text = text;
        self.clear_selection();
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.clear_selection();
    }

    pub(super) fn select_all(&mut self) {
        self.cursor = self.len_graphemes();
        self.selection_start = Some(0);
    }

    /// Number of grapheme clusters in the provided string.
    pub(super) fn grapheme_count(s: &str) -> usize {
        s.graphemes(true).count()
    }

    /// Byte offset of the grapheme at `g_idx`. Returns `text.len()` if index
    /// is past the end.
    pub(super) fn grapheme_byte_idx(&self, g_idx: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .nth(g_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }

    /// Byte offset for the current cursor position.
    fn cursor_byte_idx(&self) -> usize {
        self.grapheme_byte_idx(self.cursor)
    }

    /// Total grapheme count of the text.
    pub(super) fn len_graphemes(&self) -> usize {
        Self::grapheme_count(&self.text)
    }

    pub(super) fn clear_selection(&mut self) {
        self.selection_start = None;
    }

    pub(super) fn has_selection(&self) -> bool {
        self.selection_start.is_some_and(|s| s != self.cursor)
    }

    /// Byte range [start, end) of the current selection, or None.
    pub(super) fn selection_byte_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_start?;
        if anchor == self.cursor {
            return None;
        }
        let from = anchor.min(self.cursor);
        let to = anchor.max(self.cursor);
        let byte_start = self.grapheme_byte_idx(from);
        let byte_end = self.grapheme_byte_idx(to);
        Some((byte_start, byte_end))
    }

    /// Delete selected text. Returns true if anything was deleted.
    pub(super) fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection_byte_range() else {
            return false;
        };
        self.text.replace_range(start..end, "");
        let anchor = self.selection_start.unwrap_or(self.cursor);
        self.cursor = self.cursor.min(anchor);
        let total = self.len_graphemes();
        if self.cursor > total {
            self.cursor = total;
        }
        self.clear_selection();
        true
    }

    pub(super) fn set_cursor_from_text_x(&mut self, pixel_x: f32, font_size: f32) {
        let mut best = 0;
        let mut best_dist = f32::MAX;
        let total = self.len_graphemes();
        for i in 0..=total {
            let prefix_byte = self.grapheme_byte_idx(i);
            let w = measure_text_width(&self.text[..prefix_byte], font_size);
            let dist = (pixel_x - w).abs();
            if dist < best_dist {
                best_dist = dist;
                best = i;
            }
        }
        self.cursor = best;
    }

    pub(super) fn cursor_text_x(&self, font_size: f32) -> f32 {
        if self.text.is_empty() {
            0.0
        } else {
            measure_text_width(&self.text[..self.cursor_byte_idx()], font_size)
        }
    }

    /// Delete one grapheme before the cursor (for Backspace).
    pub(super) fn delete_grapheme_before(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let target = self.cursor - 1;
        let byte_idx = self.grapheme_byte_idx(target);
        let next_byte = self.grapheme_byte_idx(self.cursor);
        self.text.replace_range(byte_idx..next_byte, "");
        self.cursor = target;
        true
    }

    /// Delete one grapheme at the cursor (for Delete).
    pub(super) fn delete_grapheme_at(&mut self) -> bool {
        let total = self.len_graphemes();
        if self.cursor >= total {
            return false;
        }
        let byte_idx = self.grapheme_byte_idx(self.cursor);
        let next_byte = self.grapheme_byte_idx(self.cursor + 1);
        self.text.replace_range(byte_idx..next_byte, "");
        true
    }

    pub(super) fn insert_normalized_at_cursor(&mut self, s: &str) {
        let count = Self::grapheme_count(s);
        let idx = self.cursor_byte_idx();
        self.text.insert_str(idx, s);
        self.cursor += count;
    }

    pub(super) fn move_cursor_to(&mut self, pos: usize) {
        self.cursor = pos;
    }

    pub(super) fn move_cursor_with_selection(&mut self, pos: usize, extend_selection: bool) {
        if extend_selection {
            if self.selection_start.is_none() {
                self.selection_start = Some(self.cursor);
            }
        } else {
            self.clear_selection();
        }
        self.move_cursor_to(pos.min(self.len_graphemes()));
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
