use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// Grapheme column inside a logical text line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPosition {
    /// Zero-based logical line index.
    pub line: usize,
    /// Zero-based grapheme column inside `line`.
    pub column: usize,
}

impl TextPosition {
    /// Create a logical text position.
    pub const fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

/// Normalized selection range in document positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSelection {
    /// Inclusive selection start.
    pub start: TextPosition,
    /// Exclusive selection end.
    pub end: TextPosition,
}

/// Grapheme-safe multiline text editing model.
///
/// The model stores normalized `\n` line endings, exposes line/column positions
/// in grapheme clusters, and converts those positions to UTF-8 byte ranges for
/// paint, clipboard, IME, and command integration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MultilineTextEditState {
    text: String,
    line_starts: Vec<usize>,
    cursor: TextPosition,
    selection_anchor: Option<TextPosition>,
    preferred_column: Option<usize>,
}

impl MultilineTextEditState {
    /// Create an empty multiline edit state.
    pub fn new() -> Self {
        Self::with_text(String::new())
    }

    /// Create a multiline edit state from text, normalizing line endings.
    pub fn with_text(text: impl Into<String>) -> Self {
        let text = normalize_multiline_text(&text.into());
        let mut state = Self {
            text,
            line_starts: Vec::new(),
            cursor: TextPosition::default(),
            selection_anchor: None,
            preferred_column: None,
        };
        state.rebuild_line_starts();
        state.cursor = state.document_end();
        state
    }

    /// Committed document text with normalized `\n` line endings.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Current cursor position.
    pub fn cursor(&self) -> TextPosition {
        self.cursor
    }

    /// Number of logical lines. Empty documents contain one empty line.
    pub fn line_count(&self) -> usize {
        self.line_starts.len().max(1)
    }

    /// Text slice for `line`, excluding the trailing newline separator.
    pub fn line_text(&self, line: usize) -> &str {
        let range = self.line_byte_range(line);
        &self.text[range]
    }

    /// UTF-8 byte range for `line`, excluding the trailing newline separator.
    pub fn line_byte_range(&self, line: usize) -> Range<usize> {
        let line = line.min(self.line_count().saturating_sub(1));
        let start = self.line_starts.get(line).copied().unwrap_or(0);
        let end = if line + 1 < self.line_starts.len() {
            self.line_starts[line + 1].saturating_sub(1)
        } else {
            self.text.len()
        };
        start..end.max(start)
    }

    /// Grapheme count for a line.
    pub fn line_len_graphemes(&self, line: usize) -> usize {
        self.line_text(line).graphemes(true).count()
    }

    /// Convert a document position to a UTF-8 byte index.
    pub fn position_to_byte(&self, position: TextPosition) -> usize {
        let position = self.clamp_position(position);
        self.line_grapheme_byte_idx(position.line, position.column)
    }

    /// Convert a UTF-8 byte index to the nearest preceding grapheme position.
    pub fn byte_to_position(&self, byte: usize) -> TextPosition {
        let byte = previous_char_boundary(&self.text, byte.min(self.text.len()));
        let mut line = 0;
        for (idx, start) in self.line_starts.iter().enumerate() {
            if *start > byte {
                break;
            }
            line = idx;
        }
        let range = self.line_byte_range(line);
        let relative = byte.saturating_sub(range.start).min(range.end - range.start);
        let column = grapheme_column_for_relative_byte(&self.text[range], relative);
        TextPosition::new(line, column)
    }

    /// Current selection as normalized document positions.
    pub fn selection(&self) -> Option<TextSelection> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor {
            return None;
        }
        let (start, end) = if anchor < self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        Some(TextSelection { start, end })
    }

    /// Current selection as a UTF-8 byte range.
    pub fn selection_byte_range(&self) -> Option<Range<usize>> {
        let selection = self.selection()?;
        Some(self.position_to_byte(selection.start)..self.position_to_byte(selection.end))
    }

    /// Select the entire document.
    pub fn select_all(&mut self) {
        self.cursor = self.document_end();
        self.selection_anchor = Some(TextPosition::default());
        self.preferred_column = None;
    }

    /// Move the cursor to a clamped position.
    pub fn move_to(&mut self, position: TextPosition, extend_selection: bool) {
        self.update_selection_anchor(extend_selection);
        self.cursor = self.clamp_position(position);
        self.preferred_column = None;
    }

    /// Drop the selection without moving the cursor. After this call,
    /// [`selection`] returns `None`.
    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    /// Move one grapheme to the left, crossing line boundaries.
    pub fn move_left(&mut self, extend_selection: bool) {
        self.update_selection_anchor(extend_selection);
        self.cursor = self.previous_position(self.cursor);
        self.preferred_column = None;
    }

    /// Move one grapheme to the right, crossing line boundaries.
    pub fn move_right(&mut self, extend_selection: bool) {
        self.update_selection_anchor(extend_selection);
        self.cursor = self.next_position(self.cursor);
        self.preferred_column = None;
    }

    /// Move to the start of the current line.
    pub fn move_line_start(&mut self, extend_selection: bool) {
        self.move_to(TextPosition::new(self.cursor.line, 0), extend_selection);
    }

    /// Move to the end of the current line.
    pub fn move_line_end(&mut self, extend_selection: bool) {
        self.move_to(
            TextPosition::new(self.cursor.line, self.line_len_graphemes(self.cursor.line)),
            extend_selection,
        );
    }

    /// Move one logical line up while preserving the preferred grapheme column.
    pub fn move_up(&mut self, extend_selection: bool) {
        self.move_vertical(-1, extend_selection);
    }

    /// Move one logical line down while preserving the preferred grapheme column.
    pub fn move_down(&mut self, extend_selection: bool) {
        self.move_vertical(1, extend_selection);
    }

    /// Insert text at the cursor, replacing any selection.
    ///
    /// All incoming line endings are normalized to `\n`.
    pub fn insert_text(&mut self, text: &str) {
        let text = normalize_multiline_text(text);
        self.delete_selection();
        let start = self.position_to_byte(self.cursor);
        self.text.insert_str(start, &text);
        self.rebuild_line_starts();
        self.cursor = self.byte_to_position(start + text.len());
        self.selection_anchor = None;
        self.preferred_column = None;
    }

    /// Delete the active selection.
    pub fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection_byte_range() else {
            return false;
        };
        let start_position = self.byte_to_position(range.start);
        self.text.replace_range(range, "");
        self.rebuild_line_starts();
        self.cursor = self.clamp_position(start_position);
        self.selection_anchor = None;
        self.preferred_column = None;
        true
    }

    /// Delete the grapheme before the cursor, joining lines when needed.
    pub fn delete_backward(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let previous = self.previous_position(self.cursor);
        if previous == self.cursor {
            return false;
        }
        let start = self.position_to_byte(previous);
        let end = self.position_to_byte(self.cursor);
        self.text.replace_range(start..end, "");
        self.rebuild_line_starts();
        self.cursor = self.clamp_position(previous);
        self.preferred_column = None;
        true
    }

    /// Delete the grapheme at the cursor, joining lines when needed.
    pub fn delete_forward(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        let next = self.next_position(self.cursor);
        if next == self.cursor {
            return false;
        }
        let start = self.position_to_byte(self.cursor);
        let end = self.position_to_byte(next);
        self.text.replace_range(start..end, "");
        self.rebuild_line_starts();
        self.cursor = self.clamp_position(self.cursor);
        self.preferred_column = None;
        true
    }

    fn rebuild_line_starts(&mut self) {
        self.line_starts.clear();
        self.line_starts.push(0);
        for (idx, ch) in self.text.char_indices() {
            if ch == '\n' {
                self.line_starts.push(idx + ch.len_utf8());
            }
        }
    }

    fn document_end(&self) -> TextPosition {
        let line = self.line_count().saturating_sub(1);
        TextPosition::new(line, self.line_len_graphemes(line))
    }

    fn clamp_position(&self, position: TextPosition) -> TextPosition {
        let line = position.line.min(self.line_count().saturating_sub(1));
        let column = position.column.min(self.line_len_graphemes(line));
        TextPosition::new(line, column)
    }

    fn line_grapheme_byte_idx(&self, line: usize, column: usize) -> usize {
        let range = self.line_byte_range(line);
        self.text[range.clone()]
            .grapheme_indices(true)
            .nth(column)
            .map(|(idx, _)| range.start + idx)
            .unwrap_or(range.end)
    }

    fn previous_position(&self, position: TextPosition) -> TextPosition {
        let position = self.clamp_position(position);
        if position.column > 0 {
            return TextPosition::new(position.line, position.column - 1);
        }
        if position.line == 0 {
            return position;
        }
        let previous_line = position.line - 1;
        TextPosition::new(previous_line, self.line_len_graphemes(previous_line))
    }

    fn next_position(&self, position: TextPosition) -> TextPosition {
        let position = self.clamp_position(position);
        if position.column < self.line_len_graphemes(position.line) {
            return TextPosition::new(position.line, position.column + 1);
        }
        if position.line + 1 >= self.line_count() {
            return position;
        }
        TextPosition::new(position.line + 1, 0)
    }

    fn move_vertical(&mut self, direction: i32, extend_selection: bool) {
        self.update_selection_anchor(extend_selection);
        let preferred_column = self.preferred_column.unwrap_or(self.cursor.column);
        let target_line = if direction < 0 {
            self.cursor.line.saturating_sub(direction.unsigned_abs() as usize)
        } else {
            self.cursor
                .line
                .saturating_add(direction as usize)
                .min(self.line_count().saturating_sub(1))
        };
        self.cursor = TextPosition::new(
            target_line,
            preferred_column.min(self.line_len_graphemes(target_line)),
        );
        self.preferred_column = Some(preferred_column);
    }

    fn update_selection_anchor(&mut self, extend_selection: bool) {
        if extend_selection {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor);
            }
        } else {
            self.selection_anchor = None;
        }
    }
}

fn normalize_multiline_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                output.push('\n');
            }
            '\u{2028}' | '\u{2029}' => output.push('\n'),
            _ => output.push(ch),
        }
    }
    output
}

fn previous_char_boundary(text: &str, byte: usize) -> usize {
    if byte >= text.len() {
        return text.len();
    }
    let mut candidate = byte;
    while candidate > 0 && !text.is_char_boundary(candidate) {
        candidate -= 1;
    }
    candidate
}

fn grapheme_column_for_relative_byte(line: &str, relative_byte: usize) -> usize {
    let relative_byte = relative_byte.min(line.len());
    for (column, (start, grapheme)) in line.grapheme_indices(true).enumerate() {
        if relative_byte <= start || relative_byte < start + grapheme.len() {
            return column;
        }
    }
    line.graphemes(true).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_line_endings_and_preserves_trailing_empty_lines() {
        let edit = MultilineTextEditState::with_text("a\r\nb\rc\u{2028}d\u{2029}");

        assert_eq!(edit.text(), "a\nb\nc\nd\n");
        assert_eq!(edit.line_count(), 5);
        assert_eq!(edit.line_text(0), "a");
        assert_eq!(edit.line_text(3), "d");
        assert_eq!(edit.line_text(4), "");
        assert_eq!(edit.cursor(), TextPosition::new(4, 0));
    }

    #[test]
    fn maps_line_column_positions_to_utf8_bytes_without_splitting_graphemes() {
        let edit = MultilineTextEditState::with_text("a👩‍💻e\nनमस्ते");

        assert_eq!(edit.line_len_graphemes(0), 3);
        assert_eq!(edit.position_to_byte(TextPosition::new(0, 1)), "a".len());
        assert_eq!(edit.position_to_byte(TextPosition::new(0, 2)), "a👩‍💻".len());
        assert_eq!(
            edit.byte_to_position("a👩".len()),
            TextPosition::new(0, 1),
            "byte offsets inside an emoji ZWJ cluster snap to the preceding grapheme"
        );
        assert_eq!(
            edit.byte_to_position("a👩‍💻e\n".len()),
            TextPosition::new(1, 0)
        );
        assert_eq!(
            edit.position_to_byte(TextPosition::new(99, 99)),
            edit.text().len(),
            "out-of-range positions clamp to document end"
        );
    }

    #[test]
    fn horizontal_and_vertical_navigation_crosses_lines_safely() {
        let mut edit = MultilineTextEditState::with_text("ab\ncde\nf");

        edit.move_to(TextPosition::new(1, 2), false);
        edit.move_left(false);
        assert_eq!(edit.cursor(), TextPosition::new(1, 1));
        edit.move_left(false);
        edit.move_left(false);
        assert_eq!(edit.cursor(), TextPosition::new(0, 2));

        edit.move_to(TextPosition::new(0, 2), false);
        edit.move_down(false);
        assert_eq!(edit.cursor(), TextPosition::new(1, 2));
        edit.move_down(false);
        assert_eq!(edit.cursor(), TextPosition::new(2, 1));
        edit.move_up(false);
        assert_eq!(
            edit.cursor(),
            TextPosition::new(1, 2),
            "vertical movement preserves the preferred column after clamping through a short line"
        );

        edit.move_line_start(false);
        assert_eq!(edit.cursor(), TextPosition::new(1, 0));
        edit.move_line_end(false);
        assert_eq!(edit.cursor(), TextPosition::new(1, 3));
    }

    #[test]
    fn selection_byte_ranges_can_span_lines_and_grapheme_clusters() {
        let mut edit = MultilineTextEditState::with_text("a👩‍💻\ncd");

        edit.move_to(TextPosition::new(0, 1), false);
        edit.move_to(TextPosition::new(1, 1), true);

        let selection = edit.selection().expect("selection should span lines");
        assert_eq!(selection.start, TextPosition::new(0, 1));
        assert_eq!(selection.end, TextPosition::new(1, 1));
        let range = edit.selection_byte_range().expect("selection byte range");
        assert_eq!(&edit.text()[range], "👩‍💻\nc");
    }

    #[test]
    fn insert_replaces_selection_and_normalizes_pasted_line_endings() {
        let mut edit = MultilineTextEditState::with_text("hello\nworld");

        edit.move_to(TextPosition::new(0, 2), false);
        edit.move_to(TextPosition::new(1, 2), true);
        edit.insert_text("X\r\nY");

        assert_eq!(edit.text(), "heX\nYrld");
        assert_eq!(edit.cursor(), TextPosition::new(1, 1));
        assert!(edit.selection().is_none());
    }

    #[test]
    fn delete_backward_and_forward_join_lines_at_boundaries() {
        let mut edit = MultilineTextEditState::with_text("ab\ncd");

        edit.move_to(TextPosition::new(1, 0), false);
        assert!(edit.delete_backward());
        assert_eq!(edit.text(), "abcd");
        assert_eq!(edit.cursor(), TextPosition::new(0, 2));

        edit.move_to(TextPosition::new(0, 2), false);
        assert!(edit.delete_forward());
        assert_eq!(edit.text(), "abd");
        assert_eq!(edit.cursor(), TextPosition::new(0, 2));
        assert!(edit.delete_forward());
        assert_eq!(edit.text(), "ab");
        assert_eq!(edit.cursor(), TextPosition::new(0, 2));
        assert!(!edit.delete_forward());
    }

    #[test]
    fn select_all_and_delete_selection_keep_an_empty_single_line_document() {
        let mut edit = MultilineTextEditState::with_text("a\nb");

        edit.select_all();
        assert_eq!(edit.selection_byte_range(), Some(0..3));
        assert!(edit.delete_selection());

        assert_eq!(edit.text(), "");
        assert_eq!(edit.line_count(), 1);
        assert_eq!(edit.cursor(), TextPosition::new(0, 0));
        assert!(edit.selection().is_none());
    }
}
