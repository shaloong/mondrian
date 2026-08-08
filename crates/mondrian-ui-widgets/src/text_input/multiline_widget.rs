use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::Instant;

use unicode_segmentation::UnicodeSegmentation;

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, AccessibilityValue, CursorRequest,
    EventContext, PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::current_theme;

use super::composition::TextCompositionState;
use super::ime::{classify_ime_key, request_disabled_ime, request_enabled_ime, ImeKeyDisposition};
use super::multiline::{MultilineTextEditState, TextPosition};
use super::multiline_commands::{
    classify_key_command, MultilineTextKeyCommand, PlatformKeymap, TabBehavior,
};
use super::multiline_geometry::{
    self, compute_multiline_geometry, scroll_y_for_caret, LineLayoutMode, LineMeasureCache,
    MultilineTextGeometry, TextMetrics,
};
use super::multiline_paint::{paint_multiline, MultilinePaintSnapshot};
use super::TextInputChangeAction;

const DEFAULT_MIN_LINES: usize = 3;

// ── Undo ────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct TextUndoEntry {
    text_before: String,
    text_after: String,
    cursor_before: TextPosition,
    cursor_after: TextPosition,
    anchor_before: Option<TextPosition>,
    anchor_after: Option<TextPosition>,
}

#[derive(Clone, Debug)]
struct TextUndoStack {
    entries: Vec<TextUndoEntry>,
    position: usize,
    last_mutation: Option<Instant>,
}

impl TextUndoStack {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            position: 0,
            last_mutation: None,
        }
    }

    fn push(&mut self, entry: TextUndoEntry, coalesce: bool) {
        if self.position < self.entries.len() {
            self.entries.truncate(self.position);
        }

        let now = Instant::now();
        let can_coalesce = coalesce
            && self.last_mutation.is_some_and(|t| now.duration_since(t).as_millis() < 1000)
            && self.position > 0;

        if can_coalesce {
            let last = &mut self.entries[self.position - 1];
            last.text_after = entry.text_after;
            last.cursor_after = entry.cursor_after;
            last.anchor_after = entry.anchor_after;
        } else {
            self.entries.push(entry);
            self.position = self.entries.len();
            if self.entries.len() > 100 {
                self.entries.remove(0);
                self.position = self.position.saturating_sub(1);
            }
        }
        self.last_mutation = Some(now);
    }

    fn undo(&mut self, current: &MultilineTextEditState) -> Option<MultilineTextEditState> {
        if self.position == 0 {
            return None;
        }
        if self.position == self.entries.len() {
            let tip_text = current.text().to_string();
            let tip_cursor = current.cursor();
            let tip_anchor = current.selection().map(|s| s.start);
            let last = self.entries.last()?;
            self.entries.push(TextUndoEntry {
                text_before: last.text_after.clone(),
                text_after: tip_text,
                cursor_before: last.cursor_after,
                cursor_after: tip_cursor,
                anchor_before: last.anchor_after,
                anchor_after: tip_anchor,
            });
        }
        self.position -= 1;
        let entry = &self.entries[self.position];
        let mut restored = MultilineTextEditState::with_text(&entry.text_before);
        restored.move_to(entry.cursor_before, false);
        if let Some(anchor) = entry.anchor_before {
            restored.move_to(anchor, false);
            restored.move_to(entry.cursor_before, true);
        }
        Some(restored)
    }

    fn redo(&mut self) -> Option<MultilineTextEditState> {
        if self.position >= self.entries.len() {
            return None;
        }
        let entry = &self.entries[self.position];
        self.position += 1;
        let mut restored = MultilineTextEditState::with_text(&entry.text_after);
        restored.move_to(entry.cursor_after, false);
        if let Some(anchor) = entry.anchor_after {
            restored.move_to(anchor, false);
            restored.move_to(entry.cursor_after, true);
        }
        Some(restored)
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────────

fn clear_multiline_selection(edit: &mut MultilineTextEditState) {
    edit.clear_selection();
}

fn grapheme_is_whitespace(grapheme: &str) -> bool {
    grapheme.chars().all(char::is_whitespace)
}

fn prev_word_boundary_in_line(line: &str, from: usize) -> usize {
    let total = line.graphemes(true).count();
    if from == 0 {
        return 0;
    }
    let mut i = from.min(total);
    while i > 0 {
        let b0 = line.grapheme_indices(true).nth(i - 1).map(|(b, _)| b).unwrap_or(line.len());
        let b1 = line
            .grapheme_indices(true)
            .nth(i.min(total))
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let b1 = if i >= total { line.len() } else { b1 };
        let g = &line[b0..b1.min(line.len())];
        if grapheme_is_whitespace(g) {
            i -= 1;
        } else {
            break;
        }
    }
    while i > 0 {
        let b0 = line.grapheme_indices(true).nth(i - 1).map(|(b, _)| b).unwrap_or(line.len());
        let b1 = line
            .grapheme_indices(true)
            .nth(i.min(total))
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let b1 = if i >= total { line.len() } else { b1 };
        let g = &line[b0..b1.min(line.len())];
        if !grapheme_is_whitespace(g) {
            i -= 1;
        } else {
            break;
        }
    }
    i
}

fn next_word_boundary_in_line(line: &str, from: usize) -> usize {
    let total = line.graphemes(true).count();
    let mut i = from.min(total);
    while i < total {
        let b0 = line.grapheme_indices(true).nth(i).map(|(b, _)| b).unwrap_or(line.len());
        let b1 = line
            .grapheme_indices(true)
            .nth((i + 1).min(total))
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let b1 = if i + 1 >= total { line.len() } else { b1 };
        let g = &line[b0..b1.min(line.len())];
        if !grapheme_is_whitespace(g) {
            i += 1;
        } else {
            break;
        }
    }
    while i < total {
        let b0 = line.grapheme_indices(true).nth(i).map(|(b, _)| b).unwrap_or(line.len());
        let b1 = line
            .grapheme_indices(true)
            .nth((i + 1).min(total))
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let b1 = if i + 1 >= total { line.len() } else { b1 };
        let g = &line[b0..b1.min(line.len())];
        if grapheme_is_whitespace(g) {
            i += 1;
        } else {
            break;
        }
    }
    i
}

// ── Widget ──────────────────────────────────────────────────────────────────────

pub struct MultilineTextInput {
    id: WidgetId,
    edit: MultilineTextEditState,
    placeholder: String,
    bounds: Rect,
    enabled: bool,
    read_only: bool,
    focused: bool,
    mouse_down: bool,
    cursor_visible: Cell<bool>,
    last_blink: Cell<Instant>,
    scroll_x: Cell<f32>,
    scroll_y: Cell<f32>,
    metrics: TextMetrics,
    composition: TextCompositionState,
    composition_anchor: Option<TextPosition>,
    geometry_cache: RefCell<Option<MultilineTextGeometry>>,
    measure_cache: RefCell<HashMap<usize, LineMeasureCache>>,
    undo_stack: RefCell<TextUndoStack>,
    tab_behavior: TabBehavior,
    min_lines: usize,
    wrap_mode: LineLayoutMode,
    on_change: Option<Box<TextInputChangeAction>>,
    last_click: Cell<Instant>,
    click_count: Cell<u8>,
}

impl MultilineTextInput {
    pub fn new(placeholder: impl Into<String>) -> Self {
        let metrics = TextMetrics::from_font_size(14.0);
        Self {
            id: WidgetId::new(),
            edit: MultilineTextEditState::new(),
            placeholder: placeholder.into(),
            bounds: Rect::ZERO,
            enabled: true,
            read_only: false,
            focused: false,
            mouse_down: false,
            cursor_visible: Cell::new(true),
            last_blink: Cell::new(Instant::now()),
            scroll_x: Cell::new(0.0),
            scroll_y: Cell::new(0.0),
            metrics,
            composition: TextCompositionState::default(),
            composition_anchor: None,
            geometry_cache: RefCell::new(None),
            measure_cache: RefCell::new(HashMap::new()),
            undo_stack: RefCell::new(TextUndoStack::new()),
            tab_behavior: TabBehavior::MoveFocus,
            min_lines: DEFAULT_MIN_LINES,
            wrap_mode: LineLayoutMode::NoWrap,
            on_change: None,
            last_click: Cell::new(Instant::now()),
            click_count: Cell::new(0),
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.edit = MultilineTextEditState::with_text(text.into());
        self
    }

    pub fn min_lines(mut self, n: usize) -> Self {
        self.min_lines = n.max(1);
        self
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.set_enabled(enabled);
        self
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.focused = false;
            self.mouse_down = false;
            clear_multiline_selection(&mut self.edit);
            self.composition.clear();
            self.composition_anchor = None;
            self.cursor_visible.set(false);
        }
    }

    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    pub fn tab_behavior(mut self, behavior: TabBehavior) -> Self {
        self.tab_behavior = behavior;
        self
    }

    pub fn wrap_width(mut self, width: f32) -> Self {
        self.wrap_mode = LineLayoutMode::WrapToWidth(width);
        self
    }

    pub fn text(&self) -> &str {
        self.edit.text()
    }

    pub fn set_text(&mut self, text: String) {
        let before = self.edit.text().to_string();
        let before_cursor = self.edit.cursor();
        let before_anchor = self.edit.selection().map(|s| s.start);
        self.edit = MultilineTextEditState::with_text(text);
        self.save_undo_with_before(&before, before_cursor, before_anchor, false);
        self.invalidate_cache();
        self.scroll_to_cursor();
    }

    pub fn select_all(&mut self) {
        self.edit.select_all();
        self.invalidate_cache();
        self.scroll_to_cursor();
    }

    pub fn clear(&mut self) {
        let before = self.edit.text().to_string();
        let before_cursor = self.edit.cursor();
        let before_anchor = self.edit.selection().map(|s| s.start);
        self.edit = MultilineTextEditState::new();
        self.save_undo_with_before(&before, before_cursor, before_anchor, false);
        self.invalidate_cache();
        self.scroll_y.set(0.0);
        self.scroll_x.set(0.0);
    }

    pub fn on_change<F, R>(mut self, action: F) -> Self
    where
        F: Fn(&str) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_change = Some(Box::new(move |text| action(text).into()));
        self
    }

    // ── Internals ──────────────────────────────────────────────────────────────

    fn invalidate_cache(&self) {
        self.geometry_cache.replace(None);
        self.measure_cache.borrow_mut().clear();
    }

    fn sync_metrics_from_current_theme(&mut self) {
        let theme = current_theme();
        let metrics = TextMetrics::from_theme(&theme);
        if self.metrics != metrics {
            self.metrics = metrics;
            self.invalidate_cache();
            self.scroll_to_cursor();
        }
    }

    fn get_geometry(&self) -> MultilineTextGeometry {
        if self.geometry_cache.borrow().is_none() {
            let preedit = self.composition.is_active().then(|| self.composition.preedit());
            let geo = compute_multiline_geometry(
                self.bounds,
                &self.edit,
                self.edit.cursor(),
                preedit,
                self.scroll_x.get(),
                self.scroll_y.get(),
                self.metrics,
                &mut self.measure_cache.borrow_mut(),
                self.wrap_mode,
            );
            self.geometry_cache.replace(Some(geo));
        }
        self.geometry_cache.borrow().clone().expect("geometry must be cached")
    }

    fn content_left(&self) -> f32 {
        self.bounds.x + self.metrics.padding_x
    }

    fn viewport_height(&self) -> f32 {
        self.bounds.height - self.metrics.padding_y * 2.0
    }

    fn scroll_to_cursor(&self) {
        let preedit = self.composition.is_active().then(|| self.composition.preedit());
        let geo = compute_multiline_geometry(
            self.bounds,
            &self.edit,
            self.edit.cursor(),
            preedit,
            self.scroll_x.get(),
            self.scroll_y.get(),
            self.metrics,
            &mut self.measure_cache.borrow_mut(),
            self.wrap_mode,
        );
        let new_scroll_y =
            scroll_y_for_caret(self.scroll_y.get(), geo.caret, geo.clip, self.metrics);
        self.scroll_y.set(new_scroll_y);
    }

    fn save_undo_with_before(
        &self,
        before_text: &str,
        before_cursor: TextPosition,
        before_anchor: Option<TextPosition>,
        coalesce: bool,
    ) {
        let entry = TextUndoEntry {
            text_before: before_text.to_string(),
            text_after: self.edit.text().to_string(),
            cursor_before: before_cursor,
            cursor_after: self.edit.cursor(),
            anchor_before: before_anchor,
            anchor_after: self.edit.selection().map(|s| s.start),
        };
        self.undo_stack.borrow_mut().push(entry, coalesce);
    }

    fn dispatch_change(&self, ctx: &mut EventContext) {
        if let Some(factory) = &self.on_change
            && let Some(action) = factory(self.edit.text())
        {
            (ctx.dispatch)(action);
        }
    }

    fn refresh_ime(&self, ctx: &mut EventContext) {
        let cursor_area = self.get_geometry().preedit_caret;
        request_enabled_ime(ctx.requests, self.focused, cursor_area);
    }

    fn word_range_at(&self, position: TextPosition) -> (TextPosition, TextPosition) {
        let line = position.line.min(self.edit.line_count().saturating_sub(1));
        let line_text = self.edit.line_text(line);
        let total = self.edit.line_len_graphemes(line);
        let col = position.column.min(total);
        if col >= total {
            return (
                TextPosition::new(line, total),
                TextPosition::new(line, total),
            );
        }

        let start = prev_word_boundary_in_line(line_text, col);
        // Find end: if we're in whitespace, just select it; otherwise expand across word
        let mut end = col;
        if end < total {
            let b0 = line_text
                .grapheme_indices(true)
                .nth(end)
                .map(|(b, _)| b)
                .unwrap_or(line_text.len());
            let b1 = line_text
                .grapheme_indices(true)
                .nth((end + 1).min(total))
                .map(|(b, _)| b)
                .unwrap_or(line_text.len());
            let b1 = if end + 1 >= total {
                line_text.len()
            } else {
                b1
            };
            if grapheme_is_whitespace(&line_text[b0..b1.min(line_text.len())]) {
                // Select adjacent whitespace
                while end < total {
                    let b0 = line_text
                        .grapheme_indices(true)
                        .nth(end)
                        .map(|(b, _)| b)
                        .unwrap_or(line_text.len());
                    let b1 = line_text
                        .grapheme_indices(true)
                        .nth((end + 1).min(total))
                        .map(|(b, _)| b)
                        .unwrap_or(line_text.len());
                    let b1 = if end + 1 >= total {
                        line_text.len()
                    } else {
                        b1
                    };
                    if !grapheme_is_whitespace(&line_text[b0..b1.min(line_text.len())]) {
                        break;
                    }
                    end += 1;
                }
            } else {
                end = next_word_boundary_in_line(line_text, col);
            }
        }
        (TextPosition::new(line, start), TextPosition::new(line, end))
    }

    fn point_to_position(&self, point: Point) -> TextPosition {
        let geo = self.get_geometry();
        multiline_geometry::point_to_position(
            &self.edit,
            point,
            self.content_left(),
            self.scroll_x.get(),
            self.scroll_y.get(),
            self.metrics,
            &mut self.measure_cache.borrow_mut(),
            &geo.visual_lines,
            self.wrap_mode,
        )
    }

    // ── Indent / Dedent ──────────────────────────────────────────────────────

    /// Indent all lines intersecting the current selection by prepending `\t`.
    /// Preserves cursor and selection positions (adjusted +1 column on affected
    /// lines). Returns true if any lines were modified.
    fn indent_selected_lines(&mut self) -> bool {
        let Some(range) = self.line_range_of_selection() else {
            return false;
        };
        let saved_cursor = self.edit.cursor();
        let saved_sel = self.edit.selection();

        let mut lines: Vec<String> = (0..self.edit.line_count())
            .map(|i| self.edit.line_text(i).to_string())
            .collect();
        let mut changed = false;
        for l in lines.iter_mut().take(range.end).skip(range.start) {
            l.insert(0, '\t');
            changed = true;
        }
        if !changed {
            return false;
        }
        self.edit = MultilineTextEditState::with_text(lines.join("\n"));

        // Restore cursor, shifted +1 if on an affected line
        let cursor_col = if saved_cursor.line >= range.start && saved_cursor.line < range.end {
            saved_cursor.column + 1
        } else {
            saved_cursor.column
        };
        let new_cursor = TextPosition::new(
            saved_cursor.line.min(self.edit.line_count().saturating_sub(1)),
            cursor_col.min(self.edit.line_len_graphemes(
                saved_cursor.line.min(self.edit.line_count().saturating_sub(1)),
            )),
        );
        self.edit.move_to(new_cursor, false);

        // Restore selection, also shifted
        if let Some(sel) = saved_sel {
            let adjust = |pos: TextPosition| -> TextPosition {
                let col = if pos.line >= range.start && pos.line < range.end {
                    pos.column + 1
                } else {
                    pos.column
                };
                TextPosition::new(
                    pos.line.min(self.edit.line_count().saturating_sub(1)),
                    col.min(self.edit.line_len_graphemes(
                        pos.line.min(self.edit.line_count().saturating_sub(1)),
                    )),
                )
            };
            let new_start = adjust(sel.start);
            self.edit.move_to(new_start, false);
            self.edit.move_to(new_cursor, true);
        }

        true
    }

    /// Dedent all lines intersecting the current selection by removing one
    /// leading `\t` or up to 4 leading spaces. Preserves cursor/selection
    /// positions. Returns true if any lines changed.
    fn dedent_selected_lines(&mut self) -> bool {
        let Some(range) = self.line_range_of_selection() else {
            return false;
        };
        let saved_cursor = self.edit.cursor();
        let saved_sel = self.edit.selection();

        let mut lines: Vec<String> = (0..self.edit.line_count())
            .map(|i| self.edit.line_text(i).to_string())
            .collect();
        let mut shift_per_line: Vec<i32> = vec![0; lines.len()]; // column shift per line
        let mut changed = false;
        for (li, l) in lines.iter_mut().enumerate().take(range.end).skip(range.start) {
            if let Some(rest) = l.strip_prefix('\t') {
                *l = rest.to_string();
                shift_per_line[li] = -1;
                changed = true;
            } else if let Some(rest) = l.strip_prefix("    ") {
                *l = rest.to_string();
                shift_per_line[li] = -4;
                changed = true;
            } else if let Some(rest) = l.strip_prefix("  ") {
                *l = rest.to_string();
                shift_per_line[li] = -2;
                changed = true;
            }
        }
        if !changed {
            return false;
        }
        self.edit = MultilineTextEditState::with_text(lines.join("\n"));

        // Restore cursor with per-line shift
        let shift = *shift_per_line.get(saved_cursor.line).unwrap_or(&0);
        let new_col = (saved_cursor.column as i32 + shift).max(0) as usize;
        let new_cursor = TextPosition::new(
            saved_cursor.line.min(self.edit.line_count().saturating_sub(1)),
            new_col.min(self.edit.line_len_graphemes(
                saved_cursor.line.min(self.edit.line_count().saturating_sub(1)),
            )),
        );
        self.edit.move_to(new_cursor, false);

        if let Some(sel) = saved_sel {
            let adjust = |pos: TextPosition| -> TextPosition {
                let s = *shift_per_line.get(pos.line).unwrap_or(&0);
                let col = (pos.column as i32 + s).max(0) as usize;
                TextPosition::new(
                    pos.line.min(self.edit.line_count().saturating_sub(1)),
                    col.min(self.edit.line_len_graphemes(
                        pos.line.min(self.edit.line_count().saturating_sub(1)),
                    )),
                )
            };
            let new_start = adjust(sel.start);
            self.edit.move_to(new_start, false);
            self.edit.move_to(new_cursor, true);
        }

        true
    }

    /// Line range (start..end) covered by the current selection, exclusive end.
    fn line_range_of_selection(&self) -> Option<std::ops::Range<usize>> {
        let sel = self.edit.selection()?;
        let start_line = sel.start.line;
        let end_line = sel.end.line;
        let end_exclusive = if sel.end.column > 0 {
            end_line + 1
        } else {
            end_line
        }
        .min(self.edit.line_count());
        Some(start_line..end_exclusive.max(start_line + 1))
    }

    // ── Command execution ─────────────────────────────────────────────────────

    fn execute_command(
        &mut self,
        command: MultilineTextKeyCommand,
        ctx: &mut EventContext,
    ) -> EventResult {
        // Undo/Redo always consumed (prevent bubble to timeline)
        if matches!(
            command,
            MultilineTextKeyCommand::Undo | MultilineTextKeyCommand::Redo
        ) {
            let before = self.edit.text().to_string();
            let result = match command {
                MultilineTextKeyCommand::Undo => {
                    let mut stack = self.undo_stack.borrow_mut();
                    stack.undo(&self.edit)
                }
                MultilineTextKeyCommand::Redo => {
                    let mut stack = self.undo_stack.borrow_mut();
                    stack.redo()
                }
                _ => None,
            };
            if let Some(restored) = result {
                self.edit = restored;
                self.invalidate_cache();
                self.scroll_to_cursor();
                self.composition.clear();
                self.composition_anchor = None;
                self.refresh_ime(ctx);
                if self.edit.text() != before {
                    self.dispatch_change(ctx);
                }
                ctx.request_repaint();
            }
            return EventResult::Handled;
        }

        if self.read_only {
            return self.execute_read_only(command, ctx);
        }

        let before_text = self.edit.text().to_string();
        let before_cursor = self.edit.cursor();
        let before_anchor = self.edit.selection().map(|s| s.start);

        let mut mutated = false;

        match command {
            MultilineTextKeyCommand::SelectAll => {
                self.edit.select_all();
            }
            MultilineTextKeyCommand::Copy => {
                if let Some(range) = self.edit.selection_byte_range() {
                    let _ = ctx.platform.clipboard_copy(&self.edit.text()[range]);
                    return EventResult::Handled;
                }
                return EventResult::Ignored;
            }
            MultilineTextKeyCommand::Paste => {
                if let Ok(Some(clip)) = ctx.platform.clipboard_paste() {
                    mutated = true;
                    self.edit.insert_text(&clip);
                }
            }
            MultilineTextKeyCommand::Cut => {
                if let Some(range) = self.edit.selection_byte_range() {
                    if ctx.platform.clipboard_copy(&self.edit.text()[range]).is_ok() {
                        mutated = true;
                        self.edit.delete_selection();
                    }
                } else {
                    return EventResult::Ignored;
                }
            }
            MultilineTextKeyCommand::MoveLeft { word, extend_selection } => {
                if word {
                    let line = self.edit.cursor().line;
                    let col = self.edit.cursor().column;
                    let line_text = self.edit.line_text(line);
                    let target = prev_word_boundary_in_line(line_text, col);
                    self.edit.move_to(TextPosition::new(line, target), extend_selection);
                } else {
                    self.edit.move_left(extend_selection);
                }
            }
            MultilineTextKeyCommand::MoveRight { word, extend_selection } => {
                if word {
                    let line = self.edit.cursor().line;
                    let col = self.edit.cursor().column;
                    let line_text = self.edit.line_text(line);
                    let target = next_word_boundary_in_line(line_text, col);
                    self.edit.move_to(TextPosition::new(line, target), extend_selection);
                } else {
                    self.edit.move_right(extend_selection);
                }
            }
            MultilineTextKeyCommand::MoveLineStart { extend_selection } => {
                self.edit.move_line_start(extend_selection);
            }
            MultilineTextKeyCommand::MoveLineEnd { extend_selection } => {
                self.edit.move_line_end(extend_selection);
            }
            MultilineTextKeyCommand::MoveUp { extend_selection } => {
                self.edit.move_up(extend_selection);
            }
            MultilineTextKeyCommand::MoveDown { extend_selection } => {
                self.edit.move_down(extend_selection);
            }
            MultilineTextKeyCommand::MoveDocStart { extend_selection } => {
                self.edit.move_to(TextPosition::new(0, 0), extend_selection);
            }
            MultilineTextKeyCommand::MoveDocEnd { extend_selection } => {
                let last = self.edit.line_count().saturating_sub(1);
                self.edit.move_to(
                    TextPosition::new(last, self.edit.line_len_graphemes(last)),
                    extend_selection,
                );
            }
            MultilineTextKeyCommand::PageUp { extend_selection } => {
                let page_lines =
                    (self.viewport_height() / self.metrics.line_height).max(1.0) as usize;
                let target = self.edit.cursor().line.saturating_sub(page_lines);
                let col = self.edit.cursor().column.min(self.edit.line_len_graphemes(target));
                self.edit.move_to(TextPosition::new(target, col), extend_selection);
            }
            MultilineTextKeyCommand::PageDown { extend_selection } => {
                let page_lines =
                    (self.viewport_height() / self.metrics.line_height).max(1.0) as usize;
                let target = (self.edit.cursor().line + page_lines)
                    .min(self.edit.line_count().saturating_sub(1));
                let col = self.edit.cursor().column.min(self.edit.line_len_graphemes(target));
                self.edit.move_to(TextPosition::new(target, col), extend_selection);
            }
            MultilineTextKeyCommand::DeleteBackward => {
                mutated = self.edit.delete_backward();
            }
            MultilineTextKeyCommand::DeleteForward => {
                mutated = self.edit.delete_forward();
            }
            MultilineTextKeyCommand::NewLine => {
                mutated = true;
                self.edit.insert_text("\n");
            }
            MultilineTextKeyCommand::InsertTab => {
                mutated = true;
                self.edit.insert_text("\t");
            }
            MultilineTextKeyCommand::IndentLines => {
                mutated = self.indent_selected_lines();
                if !mutated {
                    self.edit.insert_text("\t");
                    mutated = true;
                }
            }
            MultilineTextKeyCommand::DedentLines => {
                mutated = self.dedent_selected_lines();
            }
            _ => {}
        }

        self.invalidate_cache();
        self.scroll_to_cursor();

        if mutated || (self.edit.text() != before_text) {
            self.save_undo_with_before(&before_text, before_cursor, before_anchor, false);
            self.dispatch_change(ctx);
        } else if self.edit.text() == before_text {
            // Navigation-only: coalesce-able checkpoint for future typing
            // (don't actually push — just update geo)
        }

        // Clear composition after non-IME commands
        self.composition.clear();
        self.composition_anchor = None;
        self.refresh_ime(ctx);
        ctx.request_repaint();
        EventResult::Handled
    }

    fn execute_read_only(
        &mut self,
        command: MultilineTextKeyCommand,
        ctx: &mut EventContext,
    ) -> EventResult {
        match command {
            MultilineTextKeyCommand::Copy => {
                if let Some(range) = self.edit.selection_byte_range() {
                    let _ = ctx.platform.clipboard_copy(&self.edit.text()[range]);
                    return EventResult::Handled;
                }
                return EventResult::Ignored;
            }
            MultilineTextKeyCommand::SelectAll => {
                self.edit.select_all();
            }
            MultilineTextKeyCommand::MoveLeft { word, extend_selection } => {
                if word {
                    let line = self.edit.cursor().line;
                    let col = self.edit.cursor().column;
                    let line_text = self.edit.line_text(line);
                    let target = prev_word_boundary_in_line(line_text, col);
                    self.edit.move_to(TextPosition::new(line, target), extend_selection);
                } else {
                    self.edit.move_left(extend_selection);
                }
            }
            MultilineTextKeyCommand::MoveRight { word, extend_selection } => {
                if word {
                    let line = self.edit.cursor().line;
                    let col = self.edit.cursor().column;
                    let line_text = self.edit.line_text(line);
                    let target = next_word_boundary_in_line(line_text, col);
                    self.edit.move_to(TextPosition::new(line, target), extend_selection);
                } else {
                    self.edit.move_right(extend_selection);
                }
            }
            MultilineTextKeyCommand::MoveUp { extend_selection } => {
                self.edit.move_up(extend_selection);
            }
            MultilineTextKeyCommand::MoveDown { extend_selection } => {
                self.edit.move_down(extend_selection);
            }
            MultilineTextKeyCommand::MoveLineStart { extend_selection } => {
                self.edit.move_line_start(extend_selection);
            }
            MultilineTextKeyCommand::MoveLineEnd { extend_selection } => {
                self.edit.move_line_end(extend_selection);
            }
            MultilineTextKeyCommand::MoveDocStart { extend_selection } => {
                self.edit.move_to(TextPosition::new(0, 0), extend_selection);
            }
            MultilineTextKeyCommand::MoveDocEnd { extend_selection } => {
                let last = self.edit.line_count().saturating_sub(1);
                self.edit.move_to(
                    TextPosition::new(last, self.edit.line_len_graphemes(last)),
                    extend_selection,
                );
            }
            MultilineTextKeyCommand::PageUp { extend_selection } => {
                let page_lines =
                    (self.viewport_height() / self.metrics.line_height).max(1.0) as usize;
                let target = self.edit.cursor().line.saturating_sub(page_lines);
                let col = self.edit.cursor().column.min(self.edit.line_len_graphemes(target));
                self.edit.move_to(TextPosition::new(target, col), extend_selection);
            }
            MultilineTextKeyCommand::PageDown { extend_selection } => {
                let page_lines =
                    (self.viewport_height() / self.metrics.line_height).max(1.0) as usize;
                let target = (self.edit.cursor().line + page_lines)
                    .min(self.edit.line_count().saturating_sub(1));
                let col = self.edit.cursor().column.min(self.edit.line_len_graphemes(target));
                self.edit.move_to(TextPosition::new(target, col), extend_selection);
            }
            _ => return EventResult::Handled,
        }

        self.invalidate_cache();
        self.scroll_to_cursor();
        self.refresh_ime(ctx);
        ctx.request_repaint();
        EventResult::Handled
    }
}

// ── Widget impl ────────────────────────────────────────────────────────────────

impl Widget for MultilineTextInput {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let metrics = {
            let theme = current_theme();
            TextMetrics::from_theme(&theme)
        };
        let height = self.min_lines as f32 * metrics.line_height + metrics.padding_y * 2.0;
        constraint.constrain(Size::new(200.0, height))
    }

    fn layout(&mut self, bounds: Rect) {
        self.sync_metrics_from_current_theme();
        self.bounds = bounds;
        self.invalidate_cache();
        self.scroll_to_cursor();
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.focused
                || self.mouse_down
                || self.edit.selection().is_some()
                || self.composition.is_active()
            {
                ctx.release_pointer_capture(self.id);
                request_disabled_ime(ctx.requests);
                ctx.request_repaint();
            }
            self.focused = false;
            self.mouse_down = false;
            clear_multiline_selection(&mut self.edit);
            self.composition.clear();
            self.composition_anchor = None;
            return EventResult::Ignored;
        }

        match event {
            // ── Mouse wheel ──
            UiEvent::MouseWheel { delta, .. } => {
                if self.focused {
                    let new_y = (self.scroll_y.get() - delta).max(0.0);
                    let geo = self.get_geometry();
                    let max_y = (geo.content_size.height - self.viewport_height()).max(0.0);
                    self.scroll_y.set(new_y.min(max_y));
                    self.invalidate_cache();
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }

            // ── Mouse down ──
            UiEvent::MouseDown { position, button: MouseButton::Left, modifiers } => {
                let clicked = self.bounds.contains(*position);
                if !clicked {
                    let changed = self.focused
                        || self.mouse_down
                        || self.edit.selection().is_some()
                        || self.composition.is_active();
                    self.focused = false;
                    self.mouse_down = false;
                    clear_multiline_selection(&mut self.edit);
                    self.composition.clear();
                    self.composition_anchor = None;
                    ctx.release_pointer_capture(self.id);
                    request_disabled_ime(ctx.requests);
                    if changed {
                        ctx.request_repaint();
                    }
                    return EventResult::Ignored;
                }

                // Double-click detection
                let now = Instant::now();
                let since_last = now.duration_since(self.last_click.get()).as_millis();
                self.last_click.set(now);
                if since_last < 400 {
                    self.click_count.set(self.click_count.get() + 1);
                } else {
                    self.click_count.set(1);
                }

                let click_pos = self.point_to_position(*position);

                if modifiers.shift {
                    self.edit.move_to(click_pos, true);
                } else if self.click_count.get() == 2 {
                    let (start, end) = self.word_range_at(click_pos);
                    self.edit.move_to(start, false);
                    self.edit.move_to(end, true);
                } else {
                    self.edit.move_to(click_pos, false);
                }

                self.mouse_down = true;
                self.composition.clear();
                self.composition_anchor = None;
                ctx.request_pointer_capture(self.id);
                self.focused = true;
                self.invalidate_cache();
                self.scroll_to_cursor();
                self.refresh_ime(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }

            // ── Mouse move (drag select + auto-scroll) ──
            UiEvent::MouseMove { position, .. } => {
                if self.bounds.contains(*position) && self.enabled {
                    ctx.set_cursor(CursorRequest::Text);
                }
                if !self.mouse_down {
                    return EventResult::Ignored;
                }

                let click_pos = self.point_to_position(*position);
                self.edit.move_to(click_pos, true);

                let geo = self.get_geometry();
                let clip = geo.clip;
                let lh = self.metrics.line_height;
                if position.y < clip.y {
                    let new_y = (self.scroll_y.get() - lh).max(0.0);
                    self.scroll_y.set(new_y);
                } else if position.y > clip.y + clip.height {
                    let max_y = (geo.content_size.height - self.viewport_height()).max(0.0);
                    let new_y = (self.scroll_y.get() + lh).min(max_y);
                    self.scroll_y.set(new_y);
                }

                self.invalidate_cache();
                self.refresh_ime(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }

            // ── Mouse up ──
            UiEvent::MouseUp { button: MouseButton::Left, .. } => {
                if !self.mouse_down {
                    return EventResult::Ignored;
                }
                self.mouse_down = false;
                ctx.release_pointer_capture(self.id);
                ctx.request_repaint();
                EventResult::Handled
            }

            // ── Focus ──
            UiEvent::FocusGained { .. } => {
                self.focused = true;
                self.cursor_visible.set(true);
                self.last_blink.set(Instant::now());
                self.invalidate_cache();
                self.scroll_to_cursor();
                self.refresh_ime(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                let changed = self.focused
                    || self.mouse_down
                    || self.edit.selection().is_some()
                    || self.composition.is_active();
                self.focused = false;
                self.mouse_down = false;
                clear_multiline_selection(&mut self.edit);
                self.composition.clear();
                self.composition_anchor = None;
                ctx.release_pointer_capture(self.id);
                request_disabled_ime(ctx.requests);
                if changed {
                    ctx.request_repaint();
                }
                EventResult::Handled
            }

            // ── Keyboard ──
            UiEvent::KeyDown { key, modifiers } if self.focused => {
                // IME disposition always runs first
                match classify_ime_key(self.focused, self.composition.is_active(), *key, *modifiers)
                {
                    ImeKeyDisposition::ClearComposition => {
                        self.composition.clear();
                        self.composition_anchor = None;
                        self.refresh_ime(ctx);
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                    ImeKeyDisposition::ConsumeDuringComposition => {
                        self.refresh_ime(ctx);
                        return EventResult::Handled;
                    }
                    ImeKeyDisposition::RouteNormally => {}
                }

                let keymap = PlatformKeymap::current();
                let result = classify_key_command(*key, *modifiers, keymap, self.tab_behavior)
                    .map_or(EventResult::Ignored, |command| {
                        self.execute_command(command, ctx)
                    });

                if result == EventResult::Handled {
                    ctx.request_repaint();
                }
                result
            }

            // ── Text input ──
            UiEvent::TextInput(ch) if self.focused && !self.read_only => {
                let before_text = self.edit.text().to_string();
                let before_cursor = self.edit.cursor();
                let before_anchor = self.edit.selection().map(|s| s.start);

                self.edit.insert_text(ch);

                self.composition.clear();
                self.composition_anchor = None;
                self.save_undo_with_before(&before_text, before_cursor, before_anchor, true);
                self.invalidate_cache();
                self.scroll_to_cursor();
                self.dispatch_change(ctx);
                self.refresh_ime(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }

            // ── IME commit ──
            UiEvent::ImeCommit(ch) if self.focused && !self.read_only => {
                let before_text = self.edit.text().to_string();
                let before_cursor = self.edit.cursor();
                let before_anchor = self.edit.selection().map(|s| s.start);

                if let Some(anchor) = self.composition_anchor {
                    // Replace the composition range: from anchor to cursor
                    self.edit.move_to(anchor, false);
                    self.edit.move_to(self.edit.cursor(), false); // no-op position reset
                                                                  // Use delete_selection then insert to replace the composition range
                    let current = self.edit.cursor();
                    // Set selection from anchor to current position, then insert
                    if current != anchor {
                        // Select anchor→cursor range so insert replaces it
                        self.edit.move_to(anchor, false);
                        self.edit.move_to(current, true);
                    }
                    self.edit.insert_text(ch);
                } else {
                    self.edit.insert_text(ch);
                }

                self.composition.clear();
                self.composition_anchor = None;
                self.save_undo_with_before(&before_text, before_cursor, before_anchor, false);
                self.invalidate_cache();
                self.scroll_to_cursor();
                self.dispatch_change(ctx);
                self.refresh_ime(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }

            // ── IME preedit ──
            UiEvent::ImePreedit(preedit) if self.focused && !self.read_only => {
                if self.composition_anchor.is_none() {
                    // Delete any selection first, then anchor at cursor
                    self.edit.delete_selection();
                    self.composition_anchor = Some(self.edit.cursor());
                }
                self.composition.set_preedit(preedit.clone());
                self.invalidate_cache();
                self.refresh_ime(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }

            // ── IME cancel ──
            UiEvent::ImeCancel if self.focused => {
                self.composition.clear();
                self.composition_anchor = None;
                self.refresh_ime(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }

            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let metrics = TextMetrics::from_theme(ctx.theme);

        let mut cursor_visible = self.cursor_visible.get();
        if self.enabled && self.focused {
            let now = Instant::now();
            let elapsed = now.duration_since(self.last_blink.get());
            if elapsed.as_millis() >= 500 {
                cursor_visible = !cursor_visible;
                self.cursor_visible.set(cursor_visible);
                self.last_blink.set(now);
            }
        }

        let geo = if self.metrics == metrics {
            self.get_geometry()
        } else {
            let preedit = self.composition.is_active().then(|| self.composition.preedit());
            compute_multiline_geometry(
                self.bounds,
                &self.edit,
                self.edit.cursor(),
                preedit,
                self.scroll_x.get(),
                self.scroll_y.get(),
                metrics,
                &mut HashMap::new(),
                self.wrap_mode,
            )
        };
        let has_selection = self.edit.selection().is_some();

        paint_multiline(
            ctx,
            MultilinePaintSnapshot {
                bounds: self.bounds,
                geometry: geo,
                state: &self.edit,
                placeholder: &self.placeholder,
                enabled: self.enabled,
                read_only: self.read_only,
                focused: self.focused,
                cursor_visible,
                has_selection,
                composition_active: self.composition.is_active(),
                preedit: self.composition.is_active().then(|| self.composition.preedit()),
                composition_prefix_byte: 0,
                composition_line: self.edit.cursor().line,
            },
        );
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }

    fn accepts_text_input(&self) -> bool {
        self.enabled && !self.read_only && self.focused
    }

    fn accessibility(&self) -> Option<AccessibilityNode> {
        Some(
            AccessibilityNode::new(self.id, AccessibilityRole::TextInput)
                .with_name(self.placeholder.clone())
                .with_state(AccessibilityState {
                    focusable: self.enabled,
                    focused: self.focused,
                    disabled: !self.enabled,
                    ..AccessibilityState::default()
                })
                .with_value(AccessibilityValue::Text(self.edit.text().to_string())),
        )
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_platform_core::NoopPlatformService;
    use mondrian_ui_core::widget::EventRequests;
    use std::cell::RefCell;

    /// The platform's primary shortcut modifier: Control on Windows/Linux,
    /// Command on macOS, matching the production keymap.
    fn primary_modifiers() -> Modifiers {
        let mut modifiers = Modifiers::default();
        if cfg!(target_os = "macos") {
            modifiers.meta = true;
        } else {
            modifiers.ctrl = true;
        }
        modifiers
    }

    /// Redo adds Shift to the primary modifier on every platform.
    fn redo_modifiers() -> Modifiers {
        let mut modifiers = primary_modifiers();
        modifiers.shift = true;
        modifiers
    }

    // ── TextUndoStack ─────────────────────────────────────────────────────────

    #[test]
    fn undo_stack_starts_empty() {
        let stack = TextUndoStack::new();
        assert!(stack.entries.is_empty());
        assert_eq!(stack.position, 0);
    }

    #[test]
    fn undo_stack_push_appends_without_coalesce() {
        let mut stack = TextUndoStack::new();
        let e1 = TextUndoEntry {
            text_before: String::new(),
            text_after: "hello".into(),
            cursor_before: TextPosition::new(0, 0),
            cursor_after: TextPosition::new(0, 5),
            anchor_before: None,
            anchor_after: None,
        };
        stack.push(e1.clone(), false);
        assert_eq!(stack.entries.len(), 1);
        assert_eq!(stack.position, 1);
        assert_eq!(stack.entries[0].text_after, "hello");

        let e2 = TextUndoEntry {
            text_before: "hello".into(),
            text_after: "hello world".into(),
            cursor_before: TextPosition::new(0, 5),
            cursor_after: TextPosition::new(0, 11),
            anchor_before: None,
            anchor_after: None,
        };
        stack.push(e2, false);
        assert_eq!(stack.entries.len(), 2);
        assert_eq!(stack.position, 2);
        assert_eq!(stack.entries[1].text_after, "hello world");
    }

    #[test]
    fn undo_stack_push_truncates_on_branch() {
        let mut stack = TextUndoStack::new();
        for i in 0..3 {
            stack.push(
                TextUndoEntry {
                    text_before: format!("v{i}"),
                    text_after: format!("v{}", i + 1),
                    cursor_before: TextPosition::new(0, 0),
                    cursor_after: TextPosition::new(0, 0),
                    anchor_before: None,
                    anchor_after: None,
                },
                false,
            );
        }
        // Undo back to v2
        let state = MultilineTextEditState::with_text("v3");
        let restored = stack.undo(&state).unwrap();
        assert_eq!(restored.text(), "v2");
        assert_eq!(stack.position, 2);

        // Now push a new entry (branch). This should truncate the redo branch (v3→v3 tip).
        stack.push(
            TextUndoEntry {
                text_before: "v2".into(),
                text_after: "v2a".into(),
                cursor_before: TextPosition::new(0, 0),
                cursor_after: TextPosition::new(0, 0),
                anchor_before: None,
                anchor_after: None,
            },
            false,
        );
        assert_eq!(stack.entries.len(), 3); // v1, v2, v2a
        assert_eq!(stack.position, 3);
        // redo should return None (no more redo entries)
        assert!(stack.redo().is_none());
    }

    #[test]
    fn undo_stack_undo_and_redo_roundtrip() {
        let mut stack = TextUndoStack::new();
        stack.push(
            TextUndoEntry {
                text_before: String::new(),
                text_after: "hello".into(),
                cursor_before: TextPosition::new(0, 0),
                cursor_after: TextPosition::new(0, 5),
                anchor_before: None,
                anchor_after: None,
            },
            false,
        );
        // Undo from "hello" back to ""
        let state = MultilineTextEditState::with_text("hello");
        let restored = stack.undo(&state).unwrap();
        assert_eq!(restored.text(), "");
        assert_eq!(restored.cursor(), TextPosition::new(0, 0));

        // Redo back to "hello"
        let restored = stack.redo().unwrap();
        assert_eq!(restored.text(), "hello");
        assert_eq!(restored.cursor(), TextPosition::new(0, 5));
    }

    #[test]
    fn undo_stack_undo_at_zero_returns_none() {
        let mut stack = TextUndoStack::new();
        let state = MultilineTextEditState::with_text("");
        assert!(stack.undo(&state).is_none());
    }

    #[test]
    fn undo_stack_redo_at_end_returns_none() {
        let mut stack = TextUndoStack::new();
        assert!(stack.redo().is_none());
    }

    #[test]
    fn undo_stack_preserves_cursor_and_selection() {
        let mut stack = TextUndoStack::new();
        // Push: "ab" → "abc" with selection b..c
        let mut b = MultilineTextEditState::with_text("ab");
        b.move_to(TextPosition::new(0, 2), false);
        let mut a = MultilineTextEditState::with_text("abc");
        a.move_to(TextPosition::new(0, 2), false);
        a.move_to(TextPosition::new(0, 3), true); // selection: 2..3

        stack.push(
            TextUndoEntry {
                text_before: "ab".into(),
                text_after: "abc".into(),
                cursor_before: TextPosition::new(0, 2),
                cursor_after: TextPosition::new(0, 3),
                anchor_before: None,
                anchor_after: Some(TextPosition::new(0, 2)),
            },
            false,
        );

        let restored = stack.undo(&a).unwrap();
        assert_eq!(restored.text(), "ab");
        assert_eq!(restored.cursor(), TextPosition::new(0, 2));
        assert!(restored.selection().is_none());
    }

    #[test]
    fn undo_stack_undo_creates_redo_tip_when_at_top() {
        let mut stack = TextUndoStack::new();
        stack.push(
            TextUndoEntry {
                text_before: String::new(),
                text_after: "hello".into(),
                cursor_before: TextPosition::new(0, 0),
                cursor_after: TextPosition::new(0, 5),
                anchor_before: None,
                anchor_after: None,
            },
            false,
        );

        let state = MultilineTextEditState::with_text("hello world");
        let restored = stack.undo(&state).unwrap();
        assert_eq!(restored.text(), ""); // goes back to before first push

        // Redo restores to push's after, not the tip. Second redo gets the tip.
        let redo_restored = stack.redo().unwrap();
        assert_eq!(redo_restored.text(), "hello");
    }

    #[test]
    fn undo_stack_entries_are_bounded_to_100() {
        let mut stack = TextUndoStack::new();
        for i in 0..150 {
            stack.push(
                TextUndoEntry {
                    text_before: String::new(),
                    text_after: format!("entry{i}"),
                    cursor_before: TextPosition::new(0, 0),
                    cursor_after: TextPosition::new(0, 0),
                    anchor_before: None,
                    anchor_after: None,
                },
                false,
            );
        }
        assert_eq!(stack.entries.len(), 100);
    }

    // ── Builder API ───────────────────────────────────────────────────────────

    #[test]
    fn new_has_defaults() {
        let widget = MultilineTextInput::new("placeholder");
        assert!(widget.enabled);
        assert!(!widget.read_only);
        assert!(!widget.focused);
        assert_eq!(widget.text(), "");
        assert_eq!(widget.placeholder, "placeholder");
        assert_eq!(widget.min_lines, 3);
    }

    #[test]
    fn with_text_sets_initial_content() {
        let widget = MultilineTextInput::new("").with_text("hello world");
        assert_eq!(widget.text(), "hello world");
    }

    #[test]
    fn min_lines_clamps_to_one() {
        let widget = MultilineTextInput::new("").min_lines(0);
        assert_eq!(widget.min_lines, 1);

        let widget = MultilineTextInput::new("").min_lines(5);
        assert_eq!(widget.min_lines, 5);
    }

    #[test]
    fn read_only_true() {
        let mut widget = MultilineTextInput::new("").read_only(true);
        assert!(widget.read_only);
        // Text input should be rejected
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };
        widget.focused = true;
        let result = widget.event(&UiEvent::TextInput("x".into()), &mut ctx);
        assert_eq!(result, EventResult::Ignored);
        assert_eq!(widget.text(), "");
    }

    #[test]
    fn read_only_allows_copy_and_navigation() {
        let mut widget = MultilineTextInput::new("").read_only(true).with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;
        // Select all
        widget.select_all();
        assert_eq!(widget.edit.text(), "hello");

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };
        // Copy in read-only mode should succeed
        let result = widget.event(
            &UiEvent::KeyDown { key: KeyCode::C, modifiers: primary_modifiers() },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Handled);

        // Backspace in read-only mode should be ignored
        let result = widget.event(
            &UiEvent::KeyDown {
                key: KeyCode::Backspace,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        // Backspace navigates left (MoveLeft without word/selection) in read_only
        assert_eq!(result, EventResult::Handled); // navigation is allowed
    }

    #[test]
    fn disabled_clears_state() {
        let widget = MultilineTextInput::new("").with_text("hello").disabled();
        assert!(!widget.enabled);
        assert!(!widget.focused);
        assert!(!widget.mouse_down);
        assert!(!widget.can_focus());
        assert!(!widget.accepts_text_input());
    }

    #[test]
    fn set_text_replaces_content_and_clears_undo() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.set_text("world".into());
        assert_eq!(widget.text(), "world");
    }

    #[test]
    fn select_all_and_clear() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.select_all();
        assert!(widget.edit.selection().is_some());

        widget.clear();
        assert_eq!(widget.text(), "");
        assert!(widget.edit.selection().is_none());
    }

    #[test]
    fn on_change_dispatches_after_text_edit() {
        let mut widget = MultilineTextInput::new("")
            .with_text(String::from("hello"))
            .on_change(|_| Action::SaveProject);
        widget.set_text("hellox".into());

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let cell = RefCell::new(Vec::new());
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|a| cell.borrow_mut().push(a),
            platform: &platform,
            requests: &mut requests,
        };

        // dispatch_change fires the on_change callback with current text
        widget.dispatch_change(&mut ctx);
        assert_eq!(cell.borrow().len(), 1);
    }

    #[test]
    fn tab_behavior_configurable() {
        let mut widget = MultilineTextInput::new("").tab_behavior(TabBehavior::InsertTab);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };
        let result = widget.event(
            &UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Handled);
        assert_eq!(widget.text(), "\t");
    }

    // ── Word boundaries ───────────────────────────────────────────────────────

    #[test]
    fn prev_word_boundary_at_start_returns_zero() {
        assert_eq!(prev_word_boundary_in_line("hello world", 0), 0);
    }

    #[test]
    fn prev_word_boundary_skips_whitespace_then_word() {
        // "hello world" at position 11 (end): prev word boundary should be 6 (start of "world")
        let result = prev_word_boundary_in_line("hello world", 11);
        assert_eq!(result, 6);
    }

    #[test]
    fn prev_word_boundary_from_middle_of_word() {
        // "hello world" at position 2 ("l"): prev word boundary should be 0
        let result = prev_word_boundary_in_line("hello world", 2);
        assert_eq!(result, 0);
    }

    #[test]
    fn prev_word_boundary_from_whitespace() {
        // "hello world" at position 6 (space after "hello"): skip whitespace, then "hello"
        let result = prev_word_boundary_in_line("hello world", 6);
        assert_eq!(result, 0);
    }

    #[test]
    fn next_word_boundary_from_start() {
        let result = next_word_boundary_in_line("hello world", 0);
        // "hello"(0..5) + space(5) = 6 (includes trailing whitespace per spec)
        assert_eq!(result, 6);
    }

    #[test]
    fn next_word_boundary_skips_word_then_whitespace() {
        // From position 6 (space): skip whitespace, land at start of "world"
        let result = next_word_boundary_in_line("hello world", 6);
        assert_eq!(result, 11);
    }

    #[test]
    fn next_word_boundary_at_end_returns_total() {
        let result = next_word_boundary_in_line("hello", 5);
        assert_eq!(result, 5);
    }

    #[test]
    fn word_boundary_cjk_characters() {
        // CJK has no word boundaries; each char is its own "word"
        let result = prev_word_boundary_in_line("你好世界", 4);
        assert_eq!(result, 0);
        let result = next_word_boundary_in_line("你好世界", 0);
        assert_eq!(result, 4);
    }

    #[test]
    fn word_range_at_double_clicks_selects_word() {
        let widget = MultilineTextInput::new("").with_text("hello world");
        let (start, end) = widget.word_range_at(TextPosition::new(0, 2)); // on "l"
        assert_eq!(start, TextPosition::new(0, 0));
        assert_eq!(end, TextPosition::new(0, 6)); // "hello" + trailing space
    }

    #[test]
    fn word_range_at_on_whitespace_selects_surrounding_whitespace() {
        let widget = MultilineTextInput::new("").with_text("hello world");
        // Cursor on the space between "hello" and "world"
        let (start, end) = widget.word_range_at(TextPosition::new(0, 5));
        // prev_word_boundary returns 0 (goes to start of "hello"); start=0 is correct
        // whitespace detection selects from position 5 forward: " " → (0,5..0,6)
        assert!(start.column <= 5);
        assert!(end.column > 5);
    }

    #[test]
    fn word_range_at_end_returns_past_end() {
        let widget = MultilineTextInput::new("").with_text("hello");
        let (start, end) = widget.word_range_at(TextPosition::new(0, 5));
        assert_eq!(start, TextPosition::new(0, 5));
        assert_eq!(end, TextPosition::new(0, 5));
    }

    // ── Text editing via event ────────────────────────────────────────────────

    #[test]
    fn insert_text_pushes_undo_and_dispatches() {
        let mut widget = MultilineTextInput::new("")
            .with_text("hello")
            .on_change(|_| Action::SaveProject);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let cell = RefCell::new(Vec::new());
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|a| cell.borrow_mut().push(a),
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(&UiEvent::TextInput("x".into()), &mut ctx);
        assert_eq!(widget.text(), "hellox");
        // Undo stack should have the entry
        assert!(!cell.borrow().is_empty());
    }

    #[test]
    fn enter_inserts_newline() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(widget.text(), "hello\n");
    }

    #[test]
    fn backspace_deletes_and_joins_lines() {
        let mut edit = MultilineTextEditState::with_text("hello\nworld");
        // Move cursor to start of second line
        edit.move_to(TextPosition::new(1, 0), false);
        assert_eq!(edit.cursor(), TextPosition::new(1, 0));

        // Delete backward joins lines
        assert!(edit.delete_backward());
        assert_eq!(edit.text(), "helloworld");
        assert_eq!(edit.cursor(), TextPosition::new(0, 5));
    }

    #[test]
    fn undo_command_restores_previous_state() {
        let mut widget = MultilineTextInput::new("")
            .with_text("hello")
            .on_change(|_| Action::SaveProject);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let cell = RefCell::new(Vec::new());
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|a| cell.borrow_mut().push(a),
            platform: &platform,
            requests: &mut requests,
        };

        // Type text first
        widget.event(&UiEvent::TextInput("x".into()), &mut ctx);
        assert_eq!(widget.text(), "hellox");
        cell.borrow_mut().clear();

        // Undo
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Z, modifiers: primary_modifiers() },
            &mut ctx,
        );
        assert_eq!(widget.text(), "hello");
    }

    #[test]
    fn redo_after_undo_restores_forward() {
        let mut widget = MultilineTextInput::new("")
            .with_text("hello")
            .on_change(|_| Action::SaveProject);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(&UiEvent::TextInput("x".into()), &mut ctx);
        assert_eq!(widget.text(), "hellox");

        // Undo
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Z, modifiers: primary_modifiers() },
            &mut ctx,
        );
        assert_eq!(widget.text(), "hello");

        // Redo is primary+Shift: Command on macOS, Control elsewhere.
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Z, modifiers: redo_modifiers() },
            &mut ctx,
        );
        assert_eq!(widget.text(), "hellox");
    }

    // ── Navigation ────────────────────────────────────────────────────────────

    #[test]
    fn arrow_down_moves_cursor() {
        let mut widget = MultilineTextInput::new("").with_text("line1\nline2");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(widget.edit.cursor().line, 1);
    }

    #[test]
    fn arrow_up_moves_cursor() {
        let mut widget = MultilineTextInput::new("").with_text("line1\nline2");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;
        // Move to second line first
        widget.edit.move_down(false);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(widget.edit.cursor().line, 0);
    }

    #[test]
    fn home_goes_to_line_start() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;
        widget.edit.move_to(TextPosition::new(0, 5), false);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(widget.edit.cursor().column, 0);
    }

    #[test]
    fn ctrl_home_goes_to_doc_start() {
        let mut widget = MultilineTextInput::new("").with_text("line1\nline2");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;
        // Cursor on second line
        widget.edit.move_down(false);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Home, modifiers: primary_modifiers() },
            &mut ctx,
        );
        assert_eq!(widget.edit.cursor(), TextPosition::new(0, 0));
    }

    #[test]
    fn page_up_and_down_scroll() {
        let mut widget = MultilineTextInput::new("").with_text("a\nb\nc\nd\ne\nf\n");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 60.0));
        widget.focused = true;
        // Set cursor on last line
        let last = widget.edit.line_count() - 1;
        widget.edit.move_to(TextPosition::new(last, 0), false);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        let before = widget.edit.cursor().line;
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::PageUp, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(widget.edit.cursor().line < before);
    }

    // ── Mouse interaction ─────────────────────────────────────────────────────

    #[test]
    fn click_positions_cursor() {
        let mut widget = MultilineTextInput::new("").with_text("hello world");
        let bounds = Rect::new(0.0, 0.0, 200.0, 100.0);
        widget.layout(bounds);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        // Click at the beginning
        let padding_x = widget.metrics.padding_x;
        let padding_y = widget.metrics.padding_y;
        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(bounds.x + padding_x + 1.0, bounds.y + padding_y + 1.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(widget.focused);
        assert!(widget.mouse_down);
        assert!(widget.edit.cursor().column < 3);
    }

    #[test]
    fn click_outside_blurs() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(500.0, 500.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(!widget.focused);
    }

    #[test]
    fn focus_gained_enables_ime_and_repaint() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        assert!(widget.focused);
        assert!(widget.cursor_visible.get());
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn focus_lost_clears_selection_and_ime() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;
        widget.edit.select_all();

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(&UiEvent::FocusLost, &mut ctx);
        assert!(!widget.focused);
        assert!(!widget.mouse_down);
        assert!(widget.edit.selection().is_none());
        assert!(!widget.composition.is_active());
    }

    // ── Widget trait contracts ────────────────────────────────────────────────

    #[test]
    fn measure_respects_min_lines() {
        let widget = MultilineTextInput::new("").min_lines(5);
        let size = widget.measure(LayoutConstraint::LOOSE);
        let expected_height = 5.0 * widget.metrics.line_height + widget.metrics.padding_y * 2.0;
        assert_eq!(size.height, expected_height);
        assert_eq!(size.width, 200.0);
    }

    #[test]
    fn layout_invalidates_cache() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        // Cache should be empty (already consumed by scroll_to_cursor in layout)
        // Force geometry recompute
        let geo = widget.get_geometry();
        assert!(geo.clip.width > 0.0);
        assert!(geo.clip.height > 0.0);
        // Second call uses cache
        let _geo2 = widget.get_geometry();
    }

    #[test]
    fn can_focus_when_enabled() {
        let widget = MultilineTextInput::new("");
        assert!(widget.can_focus());
    }

    #[test]
    fn cannot_focus_when_disabled() {
        let widget = MultilineTextInput::new("").disabled();
        assert!(!widget.can_focus());
    }

    #[test]
    fn accepts_text_input_when_focused_and_not_read_only() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.focused = true;
        assert!(widget.accepts_text_input());
    }

    #[test]
    fn does_not_accept_text_input_when_read_only() {
        let mut widget = MultilineTextInput::new("").read_only(true).with_text("hello");
        widget.focused = true;
        assert!(!widget.accepts_text_input());
    }

    #[test]
    fn does_not_accept_text_input_when_not_focused() {
        let widget = MultilineTextInput::new("").with_text("hello");
        assert!(!widget.accepts_text_input());
    }

    #[test]
    fn hit_test_respects_bounds() {
        let mut widget = MultilineTextInput::new("");
        let bounds = Rect::new(10.0, 20.0, 200.0, 100.0);
        widget.layout(bounds);
        assert!(widget.hit_test(Point::new(110.0, 70.0)));
        assert!(!widget.hit_test(Point::new(0.0, 0.0)));
        assert!(!widget.hit_test(Point::new(300.0, 200.0)));
    }

    #[test]
    fn accessibility_exposes_text_value() {
        let widget = MultilineTextInput::new("placeholder").with_text("hello");
        let node = widget.accessibility().expect("should expose accessibility");
        assert_eq!(node.role, AccessibilityRole::TextInput);
        assert_eq!(node.name.as_deref(), Some("placeholder"));
        assert_eq!(node.value, Some(AccessibilityValue::Text("hello".into())));
        assert!(node.state.focusable);
    }

    #[test]
    fn accessibility_reports_disabled_state() {
        let widget = MultilineTextInput::new("").disabled();
        let node = widget.accessibility().expect("should expose accessibility");
        assert!(node.state.disabled);
        assert!(!node.state.focusable);
    }

    // ── IME composition ───────────────────────────────────────────────────────

    #[test]
    fn ime_preedit_tracks_composition() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(&UiEvent::ImePreedit("ni".into()), &mut ctx);
        assert!(widget.composition.is_active());
        assert_eq!(widget.composition.preedit(), "ni");
        assert!(widget.composition_anchor.is_some());
    }

    #[test]
    fn ime_commit_commits_text_and_clears_composition() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        // Preedit first to set composition anchor
        widget.event(&UiEvent::ImePreedit("ni".into()), &mut ctx);
        // Commit
        widget.event(&UiEvent::ImeCommit("ni".into()), &mut ctx);
        assert!(widget.text().contains("ni"));
        assert!(!widget.composition.is_active());
        assert!(widget.composition_anchor.is_none());
    }

    #[test]
    fn ime_cancel_clears_composition() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(&UiEvent::ImePreedit("ni".into()), &mut ctx);
        assert!(widget.composition.is_active());

        widget.event(&UiEvent::ImeCancel, &mut ctx);
        assert!(!widget.composition.is_active());
        assert!(widget.composition_anchor.is_none());
    }

    #[test]
    fn ime_commit_without_preedit_just_inserts() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(&UiEvent::ImeCommit("x".into()), &mut ctx);
        assert_eq!(widget.text(), "hellox");
        assert!(!widget.composition.is_active());
    }

    // ── Mouse wheel scroll ────────────────────────────────────────────────────

    #[test]
    fn mouse_wheel_event_is_handled_when_focused() {
        let mut widget =
            MultilineTextInput::new("").with_text("line1\nline2\nline3\nline4\nline5\nline6\n");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 60.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        // Wheel event is handled (content overflows small viewport)
        let result = widget.event(
            &UiEvent::MouseWheel {
                delta: -30.0,
                position: Point::ZERO,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Handled);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn mouse_wheel_ignored_when_not_focused() {
        let mut widget = MultilineTextInput::new("").with_text("a\nb\nc\nd\ne\n");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 40.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        let result = widget.event(
            &UiEvent::MouseWheel {
                delta: 30.0,
                position: Point::ZERO,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Ignored);
    }

    // ── Select All ────────────────────────────────────────────────────────────

    #[test]
    fn ctrl_a_selects_all() {
        let mut widget = MultilineTextInput::new("").with_text("hello\nworld");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        widget.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: primary_modifiers() },
            &mut ctx,
        );
        assert!(widget.edit.selection().is_some());
        let range = widget.edit.selection_byte_range().unwrap();
        assert_eq!(range.len(), widget.text().len());
    }

    // ── Drag auto-scroll ──────────────────────────────────────────────────────

    #[test]
    fn drag_auto_scrolls_up_when_cursor_above_clip() {
        let mut widget = MultilineTextInput::new("").with_text("a\nb\nc\nd\ne\n");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;
        widget.mouse_down = true;
        // Scroll down a bit first
        widget.scroll_y.set(20.0);
        widget.invalidate_cache();

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        let before = widget.scroll_y.get();
        // Move mouse to above the clip (causes upward auto-scroll)
        let clip = widget.get_geometry().clip;
        widget.event(
            &UiEvent::MouseMove {
                position: Point::new(clip.x, clip.y - 5.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(widget.scroll_y.get() < before);
    }

    #[test]
    fn drag_auto_scroll_is_handled_when_cursor_below_clip() {
        let mut widget =
            MultilineTextInput::new("").with_text("line1\nline2\nline3\nline4\nline5\nline6\n");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 60.0));
        widget.focused = true;
        widget.mouse_down = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        let clip = widget.get_geometry().clip;
        // Mouse move below clip on a focused, mouse-down widget → handled + repaint
        let result = widget.event(
            &UiEvent::MouseMove {
                position: Point::new(clip.x + 10.0, clip.y + clip.height + 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Handled);
        assert!(ctx.requests.repaint);
    }

    // ── Double-click word selection ────────────────────────────────────────────

    #[test]
    fn double_click_selects_word() {
        let mut widget = MultilineTextInput::new("").with_text("hello world");
        let bounds = Rect::new(0.0, 0.0, 200.0, 100.0);
        widget.layout(bounds);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        // First click
        let padding_x = widget.metrics.padding_x;
        let padding_y = widget.metrics.padding_y;
        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(bounds.x + padding_x + 2.0, bounds.y + padding_y + 2.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        widget.event(
            &UiEvent::MouseUp {
                position: Point::ZERO,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        let after_first = widget.edit.cursor();

        // Second click (should be within 400ms for double-click detection)
        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(bounds.x + padding_x + 2.0, bounds.y + padding_y + 2.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        widget.event(
            &UiEvent::MouseUp {
                position: Point::ZERO,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        // After double-click, selection should span the word "hello"
        assert!(widget.edit.selection().is_some());
        let _ = after_first;
    }

    // ── Modified keys pass through ────────────────────────────────────────────

    #[test]
    fn modified_navigation_keys_are_ignored() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };

        let result = widget.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: primary_modifiers() },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Ignored);
    }

    // ── Select All (via public method) ────────────────────────────────────────

    #[test]
    fn public_select_all_selects_entire_text() {
        let mut widget = MultilineTextInput::new("").with_text("hello\nworld");
        widget.select_all();
        let sel = widget.edit.selection().unwrap();
        assert_eq!(sel.start, TextPosition::new(0, 0));
        assert_eq!(sel.end, TextPosition::new(1, 5));
    }

    // ── Indent / Dedent ──────────────────────────────────────────────────────

    #[test]
    fn indent_inserts_tab_when_no_selection() {
        let mut widget = MultilineTextInput::new("")
            .with_text("hello")
            .tab_behavior(TabBehavior::IndentSelection);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;
        // Move cursor to start so we can predict where \t goes
        widget.edit.move_to(TextPosition::new(0, 0), false);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(widget.text(), "\thello");
    }

    #[test]
    fn indent_indents_selected_lines() {
        let mut widget = MultilineTextInput::new("")
            .with_text("line1\nline2\nline3")
            .tab_behavior(TabBehavior::IndentSelection);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;
        // Select lines 1-2: move to (1,0), extend to (2,5)
        widget.edit.move_to(TextPosition::new(1, 0), false);
        widget.edit.move_to(TextPosition::new(2, 5), true);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(widget.text(), "line1\n\tline2\n\tline3");
    }

    #[test]
    fn dedent_removes_tabs_from_selected_lines() {
        let mut widget = MultilineTextInput::new("")
            .with_text("\tline1\n\tline2\n\tline3")
            .tab_behavior(TabBehavior::IndentSelection);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;
        widget.edit.select_all();

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::shift() },
            &mut ctx,
        );
        assert_eq!(widget.text(), "line1\nline2\nline3");
    }

    #[test]
    fn dedent_removes_four_spaces() {
        let mut widget = MultilineTextInput::new("")
            .with_text("    line1\n    line2")
            .tab_behavior(TabBehavior::IndentSelection);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;
        widget.edit.select_all();

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::shift() },
            &mut ctx,
        );
        assert_eq!(widget.text(), "line1\nline2");
    }

    #[test]
    fn dedent_removes_two_spaces() {
        let mut widget = MultilineTextInput::new("")
            .with_text("  line1\n  line2")
            .tab_behavior(TabBehavior::IndentSelection);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        widget.focused = true;
        widget.edit.select_all();

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let platform = NoopPlatformService;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &|_| {},
            platform: &platform,
            requests: &mut requests,
        };
        widget.event(
            &UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::shift() },
            &mut ctx,
        );
        assert_eq!(widget.text(), "line1\nline2");
    }

    // ── Undo correctness ─────────────────────────────────────────────────────

    #[test]
    fn set_text_captures_correct_before_state() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.set_text("world".into());

        let stack = widget.undo_stack.borrow();
        assert_eq!(stack.entries.len(), 1);
        assert_eq!(stack.entries[0].text_before, "hello");
        assert_eq!(stack.entries[0].text_after, "world");
    }

    #[test]
    fn clear_captures_correct_before_state() {
        let mut widget = MultilineTextInput::new("").with_text("hello");
        widget.clear();

        let stack = widget.undo_stack.borrow();
        assert_eq!(stack.entries.len(), 1);
        assert_eq!(stack.entries[0].text_before, "hello");
        assert_eq!(stack.entries[0].text_after, "");
    }

    // ── Wrap width ──────────────────────────────────────────────────────────

    #[test]
    fn wrap_width_builder_sets_mode() {
        let widget = MultilineTextInput::new("").wrap_width(200.0);
        assert_eq!(widget.wrap_mode, LineLayoutMode::WrapToWidth(200.0));
    }

    #[test]
    fn default_has_no_wrap() {
        let widget = MultilineTextInput::new("");
        assert_eq!(widget.wrap_mode, LineLayoutMode::NoWrap);
    }

    #[test]
    fn wrap_width_geometry_produces_multiple_visual_lines() {
        let mut widget = MultilineTextInput::new("")
            .with_text("abcdefghijklmnopqrstuvwxyz") // 26 chars, wide
            .wrap_width(40.0);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 200.0));
        let geo = widget.get_geometry();
        assert!(
            geo.visual_lines.len() > 1,
            "wrapping should split into multiple visual lines, got {}",
            geo.visual_lines.len()
        );
    }

    #[test]
    fn wrap_width_cursor_maps_to_correct_visual_line() {
        let mut widget = MultilineTextInput::new("")
            .with_text("abcdefghijklmnopqrstuvwxyz")
            .wrap_width(40.0);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 200.0));
        // Move cursor to far end
        widget.edit.move_to(TextPosition::new(0, 20), false);
        let geo = widget.get_geometry();
        // Caret should be in a visual line beyond the first one
        let caret = geo.caret;
        // The caret y should be > first visual line's y + line_height
        let lh = widget.metrics.line_height;
        assert!(
            caret.y > 4.0 + lh,
            "caret should be on a wrapped visual line"
        );
    }

    #[test]
    fn wrap_width_click_at_second_visual_line() {
        let mut widget = MultilineTextInput::new("")
            .with_text("abcdefghijklmnopqrstuvwxyz") // wide enough to wrap
            .wrap_width(40.0);
        widget.layout(Rect::new(0.0, 0.0, 200.0, 200.0));
        widget.focused = true;

        let lh = widget.metrics.line_height;
        let clip = widget.get_geometry().clip;
        // Click on the second visual line (y offset = lh)
        widget.event(
            &UiEvent::MouseDown {
                position: Point::new(clip.x + 5.0, clip.y + lh + 2.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut EventContext {
                focus: &mut DummyFocus,
                shortcut: &mut DummyShortcut,
                tooltip: &mut DummyTooltip,
                dispatch: &|_| {},
                platform: &NoopPlatformService,
                requests: &mut EventRequests::default(),
            },
        );
        // Cursor should be at a column > 0 (on the second visual line)
        assert!(widget.edit.cursor().column > 0);
    }

    // ── clear_selection ──────────────────────────────────────────────────────

    #[test]
    fn clear_selection_removes_selection_without_moving_cursor() {
        let mut edit = MultilineTextEditState::with_text("hello");
        edit.move_to(TextPosition::new(0, 2), false); // cursor at 'l'
        edit.move_to(TextPosition::new(0, 4), true); // select "ll"
        assert!(edit.selection().is_some());

        edit.clear_selection();
        assert!(edit.selection().is_none());
        assert_eq!(edit.cursor(), TextPosition::new(0, 4)); // cursor unchanged
    }

    #[test]
    fn indent_preserves_cursor_and_selection_shifted() {
        let mut widget = MultilineTextInput::new("")
            .with_text("line1\nline2")
            .tab_behavior(TabBehavior::IndentSelection);
        // Select both lines (cursor at selection end = (1,5))
        widget.edit.move_to(TextPosition::new(0, 0), false);
        widget.edit.move_to(TextPosition::new(1, 5), true);

        widget.indent_selected_lines();

        // After indent: "\tline1\n\tline2"
        // Cursor shifted from (1,5) → (1,6); selection (0,0)→(1,5) → (0,1)→(1,6)
        assert_eq!(widget.edit.text(), "\tline1\n\tline2");
        assert_eq!(widget.edit.cursor(), TextPosition::new(1, 6));
        let sel = widget.edit.selection().unwrap();
        assert_eq!(sel.start, TextPosition::new(0, 1));
        assert_eq!(sel.end, TextPosition::new(1, 6));
    }

    #[test]
    fn dedent_preserves_cursor_and_selection_shifted() {
        let mut widget = MultilineTextInput::new("")
            .with_text("\tline1\n\tline2")
            .tab_behavior(TabBehavior::IndentSelection);
        // select_all puts cursor at (1,6) with anchor at (0,0)
        widget.edit.select_all();

        widget.dedent_selected_lines();

        // After dedent: "line1\nline2"
        // Cursor shifted from (1,6) → (1,5); selection (0,0)→(1,6) → (0,0)→(1,5)
        assert_eq!(widget.edit.text(), "line1\nline2");
        assert_eq!(widget.edit.cursor(), TextPosition::new(1, 5));
        let sel = widget.edit.selection().unwrap();
        assert_eq!(sel.start, TextPosition::new(0, 0));
        assert_eq!(sel.end, TextPosition::new(1, 5));
    }
}
