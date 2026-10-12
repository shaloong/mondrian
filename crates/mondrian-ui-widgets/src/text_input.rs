//! Text input widgets: single-line and multiline.
//!
//! ## Single-line editor (`TextInput`)
//!
//! Cursor navigation, backspace/delete, Home/End, mouse drag selection
//! (extendable beyond widget bounds), Shift+Click/Shift+Arrow selection
//! expansion, platform primary clipboard shortcuts and word navigation,
//! grapheme-safe cursor (emoji / combining marks), IME multi-codepoint
//! input, timed blinking cursor (500 ms cycle).
//!
//! ## Multiline editor (`MultilineTextInput`)
//!
//! Line/column document model with grapheme-safe navigation, selection
//! spanning multiple lines, vertical scrolling, IME preedit across line
//! boundaries, clipboard cut/copy/paste, undo/redo, Home/End/PageUp/PageDown,
//! word navigation, platform-aware keybindings (Win/Lin/Mac), tab behavior,
//! read-only mode, double-click word selection, and drag auto-scroll.

use std::borrow::Cow;
use std::cell::Cell;
use std::time::Instant;

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, AccessibilityValue, CursorRequest,
    EventContext, PaintContext,
};
use mondrian_ui_core::{EventResult, TextInputCaretPosition, UiEvent, Widget};
use mondrian_ui_text::SingleLineTextNavigation;
use mondrian_ui_theme::{current_theme, Theme};

mod commands;
mod composition;
mod edit;
mod geometry;
mod history;
mod ime;
mod keymap;
mod multiline;
mod multiline_commands;
mod multiline_geometry;
mod multiline_paint;
mod multiline_widget;
mod paint;

use commands::{classify_key_command, TextInputKeyCommand};
use composition::TextCompositionState;
use edit::TextEditState;
use geometry::{
    compute_text_geometry, scroll_offset_after_cursor,
    text_x_from_pointer as pointer_text_x_from_geometry, TextInputGeometry,
};
use history::{TextUndoEntry, TextUndoStack};
use ime::{classify_ime_key, request_disabled_ime, request_enabled_ime, ImeKeyDisposition};
use keymap::PlatformKeymap;
pub use multiline::{MultilineTextEditState, TextPosition, TextSelection};
pub use multiline_commands::TabBehavior;
pub use multiline_widget::MultilineTextInput;
use paint::{paint_text_input, TextInputPaintSnapshot};

const DEFAULT_FONT_SIZE: f32 = 14.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TextInputMetrics {
    font_size: f32,
    padding_x: f32,
    padding_y: f32,
    caret_width: f32,
}

impl TextInputMetrics {
    fn from_theme(theme: &Theme) -> Self {
        Self {
            font_size: theme.typography.body.font_size,
            padding_x: theme.spacing.text_input_padding_x,
            padding_y: theme.spacing.text_input_padding_y,
            caret_width: theme.spacing.text_input_caret_width,
        }
    }

    fn current() -> Self {
        let theme = current_theme();
        Self::from_theme(&theme)
    }
}

impl Default for TextInputMetrics {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE,
            padding_x: 8.0,
            padding_y: 4.0,
            caret_width: 2.0,
        }
    }
}

fn measure_text_width(text: &str, font_size: f32) -> f32 {
    crate::text_metrics::measure_single_line_at_size(text, font_size).0
}

fn normalize_single_line_input(input: &str) -> Cow<'_, str> {
    let mut output = None;
    let mut previous_was_cr = false;
    for (idx, ch) in input.char_indices() {
        match ch {
            '\r' => {
                let output = output.get_or_insert_with(|| {
                    let mut normalized = String::with_capacity(input.len());
                    normalized.push_str(&input[..idx]);
                    normalized
                });
                output.push(' ');
                previous_was_cr = true;
            }
            '\n' => {
                let output = output.get_or_insert_with(|| {
                    let mut normalized = String::with_capacity(input.len());
                    normalized.push_str(&input[..idx]);
                    normalized
                });
                if !previous_was_cr {
                    output.push(' ');
                }
                previous_was_cr = false;
            }
            '\u{2028}' | '\u{2029}' => {
                let output = output.get_or_insert_with(|| {
                    let mut normalized = String::with_capacity(input.len());
                    normalized.push_str(&input[..idx]);
                    normalized
                });
                output.push(' ');
                previous_was_cr = false;
            }
            _ => {
                if let Some(output) = output.as_mut() {
                    output.push(ch);
                }
                previous_was_cr = false;
            }
        }
    }
    output.map_or(Cow::Borrowed(input), Cow::Owned)
}

/// Adapter that maps the current input value to an editor [`Action`].
pub type TextInputChangeAction = dyn Fn(&str) -> Option<Action>;

/// Adapter that maps one committed text-edit interaction to an editor [`Action`].
pub type TextInputCommitAction = dyn Fn(&str) -> Option<Action>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyTextCommitPolicy {
    PreserveSelection,
    ReplaceSelection,
}

/// TextInput Widget —— 单行文本输入框
pub struct TextInput {
    id: WidgetId,
    edit: TextEditState,
    undo_stack: TextUndoStack<TextEditState>,
    placeholder: String,
    bounds: Rect,
    enabled: bool,
    focused: bool,
    /// Whether the mouse is pressed on this widget.
    mouse_down: bool,
    /// Blink: cursor visibility and last toggle time.
    cursor_visible: Cell<bool>,
    last_blink: Cell<Instant>,
    /// Horizontal scroll offset to keep cursor visible.
    scroll_x: Cell<f32>,
    composition: TextCompositionState,
    on_change: Option<Box<TextInputChangeAction>>,
    on_commit: Option<Box<TextInputCommitAction>>,
    edit_session_origin: Option<String>,
}

impl TextInput {
    pub fn new(placeholder: impl Into<String>) -> Self {
        Self {
            id: WidgetId::new(),
            edit: TextEditState::default(),
            undo_stack: TextUndoStack::new(),
            placeholder: placeholder.into(),
            bounds: Rect::ZERO,
            enabled: true,
            focused: false,
            mouse_down: false,
            cursor_visible: Cell::new(true),
            last_blink: Cell::new(Instant::now()),
            scroll_x: Cell::new(0.0),
            composition: TextCompositionState::default(),
            on_change: None,
            on_commit: None,
            edit_session_origin: None,
        }
    }

    /// Set the initial single-line value without dispatching a user edit.
    ///
    /// Preserve exact model bytes, including line separators. The complete
    /// single-line display projection never changes the authoritative value;
    /// normalization applies only to user text/IME commits and paste.
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.edit = TextEditState::with_text(text.into());
        self.clear_local_history();
        self.sync_edit_session_origin_to_programmatic_text();
        self
    }

    /// Set whether the input accepts text, pointer, IME, and focus input.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.set_enabled(enabled);
        self
    }

    /// Set whether the input accepts text, pointer, IME, and focus input.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.cancel_edit_session();
            self.focused = false;
            self.mouse_down = false;
            self.edit.clear_selection();
            self.composition.clear();
            self.cursor_visible.set(false);
        }
    }

    /// Disable the input.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the input is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Whether this input currently owns keyboard text entry.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn text(&self) -> &str {
        self.edit.text()
    }

    /// Replace the hint without resetting the draft, selection or IME composition.
    pub fn set_placeholder(&mut self, placeholder: impl Into<String>) {
        self.placeholder = placeholder.into();
    }

    pub(crate) fn can_inspect_draft(&self) -> bool {
        self.enabled && self.focused && !self.mouse_down && !self.composition.is_active()
    }

    pub(crate) fn inspection_draft(
        &self,
    ) -> Option<mondrian_ui_core::TextInputDraftInspectionState> {
        self.can_inspect_draft()
            .then(|| mondrian_ui_core::TextInputDraftInspectionState {
                text: self.edit.text.clone(),
                cursor: self.edit.cursor,
                selection_anchor: self.edit.selection_start,
                history: mondrian_ui_core::TextInputHistoryInspectionState {
                    entries: self
                        .undo_stack
                        .entries
                        .iter()
                        .map(|entry| mondrian_ui_core::TextInputHistoryInspectionEntry {
                            text_before: entry.text_before.clone(),
                            text_after: entry.text_after.clone(),
                            cursor_before: entry.cursor_before,
                            cursor_after: entry.cursor_after,
                            anchor_before: entry.anchor_before,
                            anchor_after: entry.anchor_after,
                        })
                        .collect(),
                    position: self.undo_stack.position,
                    owns_input: self.undo_stack.owns_input(),
                },
            })
    }

    pub(crate) fn restore_numeric_input_inspection(
        &mut self,
        id: WidgetId,
        focused: bool,
        draft: Option<&mondrian_ui_core::TextInputDraftInspectionState>,
    ) -> bool {
        if !self.enabled || self.mouse_down || self.composition.is_active() {
            return false;
        }
        if let Some(draft) = draft {
            let count = TextEditState::grapheme_count(&draft.text);
            if !focused
                || !TextEditState::position_admitted(draft.cursor, count)
                || draft
                    .selection_anchor
                    .is_some_and(|anchor| !TextEditState::position_admitted(anchor, count))
                || normalize_single_line_input(&draft.text).as_ref() != draft.text.as_str()
            {
                return false;
            }
            let history = &draft.history;
            if history.entries.len() > mondrian_ui_core::widget::TEXT_INPUT_HISTORY_MAX_ENTRIES
                || history.position > history.entries.len()
                || (!history.owns_input && !history.entries.is_empty())
                || history
                    .entries
                    .iter()
                    .try_fold(0usize, |bytes, entry| {
                        bytes
                            .checked_add(entry.text_before.capacity())?
                            .checked_add(entry.text_after.capacity())
                    })
                    .is_none_or(|bytes| {
                        bytes > mondrian_ui_core::widget::TEXT_INPUT_HISTORY_MAX_BYTES
                    })
                || history.entries.iter().any(|entry| {
                    !Self::valid_history_snapshot(
                        &entry.text_before,
                        entry.cursor_before,
                        entry.anchor_before,
                    ) || !Self::valid_history_snapshot(
                        &entry.text_after,
                        entry.cursor_after,
                        entry.anchor_after,
                    )
                })
                || history.entries.windows(2).any(|pair| pair[0].text_after != pair[1].text_before)
                || (!history.entries.is_empty()
                    && draft.text.as_str()
                        != if history.position == 0 {
                            history.entries[0].text_before.as_str()
                        } else {
                            history.entries[history.position - 1].text_after.as_str()
                        })
            {
                return false;
            }
            let entries = history
                .entries
                .iter()
                .map(|entry| TextUndoEntry {
                    text_before: entry.text_before.clone(),
                    text_after: entry.text_after.clone(),
                    cursor_before: entry.cursor_before,
                    cursor_after: entry.cursor_after,
                    anchor_before: entry.anchor_before,
                    anchor_after: entry.anchor_after,
                })
                .collect();
            let Some(restored_history) =
                TextUndoStack::from_inspection(entries, history.position, history.owns_input)
            else {
                return false;
            };
            self.undo_stack = restored_history;
            self.edit = TextEditState {
                text: draft.text.clone(),
                cursor: draft.cursor,
                selection_start: draft.selection_anchor,
            };
        }
        self.id = id;
        self.focused = focused;
        self.cursor_visible.set(focused);
        self.last_blink.set(Instant::now());
        self.update_scroll(TextInputMetrics::current());
        true
    }

    fn valid_history_snapshot(
        text: &str,
        cursor: TextInputCaretPosition,
        anchor: Option<TextInputCaretPosition>,
    ) -> bool {
        let count = TextEditState::grapheme_count(text);
        TextEditState::position_admitted(cursor, count)
            && anchor.is_none_or(|anchor| TextEditState::position_admitted(anchor, count))
            && normalize_single_line_input(text).as_ref() == text
    }
    pub(crate) fn owns_local_history(&self) -> bool {
        self.undo_stack.owns_input()
    }
    pub(crate) fn clear_local_history(&mut self) {
        self.undo_stack = TextUndoStack::new();
    }
    pub(crate) fn is_local_history_chord(key: KeyCode, modifiers: Modifiers) -> bool {
        matches!(
            classify_key_command(key, modifiers, PlatformKeymap::current()),
            Some(TextInputKeyCommand::Undo | TextInputKeyCommand::Redo)
        )
    }
    fn save_local_history(&mut self, before: TextEditState, coalesce: bool) {
        if before.text == self.edit.text {
            return;
        }
        self.undo_stack.push(
            TextUndoEntry {
                text_before: before.text,
                text_after: self.edit.text.clone(),
                cursor_before: before.cursor,
                cursor_after: self.edit.cursor,
                anchor_before: before.selection_start,
                anchor_after: self.edit.selection_start,
            },
            coalesce,
        );
    }
    /// Whether an IME preedit owns keyboard input before committing text.
    pub fn is_composing(&self) -> bool {
        self.composition.is_active()
    }

    /// Replace text from an external authoritative model.
    ///
    /// Preserve the exact supplied source. Line separators display through the
    /// same complete single-line projection as caret/selection/navigation,
    /// rather than applying the normalization policy for user input or paste.
    ///
    /// In transactional mode this also resets the focused session origin, so
    /// a later focus loss cannot publish the programmatic synchronization as a
    /// user edit.
    pub fn set_text(&mut self, text: String) {
        self.edit.set_text(text);
        self.clear_local_history();
        self.sync_edit_session_origin_to_programmatic_text();
        self.update_scroll(TextInputMetrics::current());
    }

    /// Select the whole committed text.
    pub fn select_all(&mut self) {
        self.edit.select_all();
        self.update_scroll(TextInputMetrics::current());
    }

    /// Dispatch an action whenever user input changes the committed text.
    ///
    /// This live-change mode is mutually exclusive with [`Self::on_commit`];
    /// whichever builder is called last defines the dispatch policy.
    pub fn on_change<F, R>(mut self, action: F) -> Self
    where
        F: Fn(&str) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_change = Some(Box::new(move |text| action(text).into()));
        self.on_commit = None;
        self.edit_session_origin = None;
        self
    }

    /// Dispatch one action when an editing session commits.
    ///
    /// Text and IME input remain local while focused. Enter or focus loss emits
    /// at most one action and only when text differs from the focus-session
    /// origin. Escape restores that origin without dispatching. This
    /// transactional mode is mutually exclusive with [`Self::on_change`];
    /// whichever builder is called last defines the dispatch policy.
    pub fn on_commit<F, R>(mut self, action: F) -> Self
    where
        F: Fn(&str) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_commit = Some(Box::new(move |text| action(text).into()));
        self.on_change = None;
        self.edit_session_origin = None;
        self
    }

    pub fn clear(&mut self) {
        self.edit.clear();
        self.clear_local_history();
        self.sync_edit_session_origin_to_programmatic_text();
        self.update_scroll(TextInputMetrics::current());
    }

    // ── Grapheme helpers ──────────────────────────────────────────────────

    /// Total grapheme count of the text.
    fn len_graphemes(&self) -> usize {
        self.edit.len_graphemes()
    }

    // ── Selection helpers ─────────────────────────────────────────────────

    fn clear_selection(&mut self) {
        self.edit.clear_selection();
    }

    pub fn has_selection(&self) -> bool {
        self.edit.has_selection()
    }

    /// Byte range [start, end) of the current selection, or None.
    pub fn selection_byte_range(&self) -> Option<(usize, usize)> {
        self.edit.selection_byte_range()
    }

    /// Delete selected text. Returns true if anything was deleted.
    fn delete_selection(&mut self) -> bool {
        let deleted = self.edit.delete_selection();
        if deleted {
            self.update_scroll(TextInputMetrics::current());
        }
        deleted
    }

    /// Set cursor from a pixel x-coordinate relative to text start.
    fn set_cursor_from_text_x(&mut self, pixel_x: f32, metrics: TextInputMetrics) {
        self.edit.set_cursor_from_text_x(pixel_x, metrics.font_size);
        self.update_scroll(metrics);
    }

    fn text_geometry(&self, metrics: TextInputMetrics) -> TextInputGeometry {
        compute_text_geometry(
            self.bounds,
            self.scroll_x.get(),
            self.cursor_text_x(metrics),
            self.composition.is_active().then(|| self.composition.preedit()),
            metrics,
        )
    }

    #[cfg(test)]
    fn visible_width(&self) -> f32 {
        compute_text_geometry(
            self.bounds,
            self.scroll_x.get(),
            0.0,
            None,
            TextInputMetrics::current(),
        )
        .visible_width
    }

    #[cfg(test)]
    fn content_left(&self) -> f32 {
        compute_text_geometry(
            self.bounds,
            self.scroll_x.get(),
            0.0,
            None,
            TextInputMetrics::current(),
        )
        .content_left
    }

    #[cfg(test)]
    fn content_right(&self) -> f32 {
        compute_text_geometry(
            self.bounds,
            self.scroll_x.get(),
            0.0,
            None,
            TextInputMetrics::current(),
        )
        .content_right
    }

    fn cursor_text_x(&self, metrics: TextInputMetrics) -> f32 {
        self.edit.cursor_text_x(metrics.font_size)
    }

    fn cursor_area(&self, metrics: TextInputMetrics) -> Rect {
        self.text_geometry(metrics).caret
    }

    fn refresh_ime_area(&self, ctx: &mut EventContext) {
        request_enabled_ime(
            ctx.requests,
            self.focused,
            self.cursor_area(TextInputMetrics::current()),
        );
    }

    fn text_x_from_pointer(&self, position: Point) -> f32 {
        let metrics = TextInputMetrics::current();
        let Some(line) = self.edit.line_geometry(metrics.font_size, None) else {
            return 0.0;
        };
        let geometry = compute_text_geometry(
            self.bounds,
            self.scroll_x.get(),
            line.caret_x,
            None,
            metrics,
        );
        pointer_text_x_from_geometry(
            position,
            geometry.content_left,
            geometry.content_right,
            self.scroll_x.get(),
            line.width,
        )
    }

    fn update_scroll(&self, metrics: TextInputMetrics) {
        let Some(line) = self.edit.line_geometry(metrics.font_size, None) else {
            return;
        };
        let text_w = line.width;
        let geometry = compute_text_geometry(
            self.bounds,
            self.scroll_x.get(),
            line.caret_x,
            None,
            metrics,
        );
        let visible_w = geometry.visible_width;
        let cursor_x = line.caret_x;
        self.scroll_x.set(scroll_offset_after_cursor(
            self.scroll_x.get(),
            text_w,
            visible_w,
            cursor_x,
        ));
    }

    /// Delete one grapheme before the cursor (for Backspace).
    fn delete_grapheme_before(&mut self) -> bool {
        self.edit.delete_grapheme_before()
    }

    /// Delete one grapheme at the cursor (for Delete).
    fn delete_grapheme_at(&mut self) -> bool {
        self.edit.delete_grapheme_at()
    }

    fn insert_normalized_at_cursor(&mut self, s: &str) {
        self.edit.insert_normalized_at_cursor(s);
        self.update_scroll(TextInputMetrics::current());
    }

    fn commit_text_at_cursor(&mut self, input: &str, empty_policy: EmptyTextCommitPolicy) -> bool {
        let normalized = normalize_single_line_input(input);
        if normalized.is_empty() && empty_policy == EmptyTextCommitPolicy::PreserveSelection {
            return false;
        }

        let before_text = self.edit.text.clone();
        self.delete_selection();
        self.clear_selection();
        if !normalized.is_empty() {
            self.insert_normalized_at_cursor(&normalized);
        }
        before_text != self.edit.text
    }

    /// Move cursor and keep it visible.
    fn move_cursor_to(&mut self, pos: usize) {
        self.edit.move_cursor_to(pos);
        self.update_scroll(TextInputMetrics::current());
    }

    fn move_cursor_with_selection(&mut self, pos: usize, extend_selection: bool) {
        self.edit.move_cursor_with_selection(pos, extend_selection);
        self.update_scroll(TextInputMetrics::current());
    }

    fn dispatch_change(&self, ctx: &mut EventContext) {
        if let Some(factory) = &self.on_change
            && let Some(action) = factory(self.edit.text())
        {
            (ctx.dispatch)(action);
        }
    }

    fn begin_edit_session(&mut self) {
        if self.on_commit.is_some() && self.edit_session_origin.is_none() {
            self.edit_session_origin = Some(self.edit.text.clone());
        }
    }

    fn sync_edit_session_origin_to_programmatic_text(&mut self) {
        if self.on_commit.is_some() && self.focused {
            self.edit_session_origin = Some(self.edit.text.clone());
        } else {
            self.edit_session_origin = None;
        }
    }

    fn commit_edit_session(&mut self, ctx: &mut EventContext) {
        if self.on_commit.is_some() {
            self.clear_local_history();
        }
        let changed =
            self.edit_session_origin.take().is_some_and(|origin| origin != self.edit.text);
        if changed
            && let Some(factory) = &self.on_commit
            && let Some(action) = factory(self.edit.text())
        {
            (ctx.dispatch)(action);
        }
    }

    fn cancel_edit_session(&mut self) -> bool {
        self.clear_local_history();
        let Some(origin) = self.edit_session_origin.take() else {
            return false;
        };
        if self.edit.text == origin {
            return false;
        }
        self.edit.set_text(origin);
        self.update_scroll(TextInputMetrics::current());
        true
    }

    fn finish_focus(&mut self, ctx: &mut EventContext) {
        self.focused = false;
        self.mouse_down = false;
        self.clear_selection();
        self.composition.clear();
        ctx.release_pointer_capture(self.id);
        ctx.focus.release_focus(self.id);
        request_disabled_ime(ctx.requests);
        ctx.request_repaint();
    }

    // ── Word navigation ───────────────────────────────────────────────────

    fn next_word_boundary(&self, from: usize) -> usize {
        self.edit.next_word_boundary(from)
    }

    fn prev_word_boundary(&self, from: usize) -> usize {
        self.edit.prev_word_boundary(from)
    }

    fn execute_key_command(
        &mut self,
        command: TextInputKeyCommand,
        ctx: &mut EventContext,
    ) -> EventResult {
        match command {
            TextInputKeyCommand::Undo | TextInputKeyCommand::Redo => {
                let restored = if command == TextInputKeyCommand::Undo {
                    self.undo_stack.undo(&self.edit)
                } else {
                    self.undo_stack.redo()
                };
                if let Some(restored) = restored {
                    self.edit = restored;
                    self.update_scroll(TextInputMetrics::current());
                }
                EventResult::Handled
            }
            TextInputKeyCommand::DeleteWordBackward => {
                if !self.delete_selection() {
                    let target = self.prev_word_boundary(self.edit.cursor.grapheme_index);
                    if target != self.edit.cursor.grapheme_index {
                        self.move_cursor_with_selection(target, true);
                        self.delete_selection();
                    }
                }
                self.update_scroll(TextInputMetrics::current());
                EventResult::Handled
            }
            TextInputKeyCommand::DeleteWordForward => {
                if !self.delete_selection() {
                    let target = self.next_word_boundary(self.edit.cursor.grapheme_index);
                    if target != self.edit.cursor.grapheme_index {
                        self.move_cursor_with_selection(target, true);
                        self.delete_selection();
                    }
                }
                self.update_scroll(TextInputMetrics::current());
                EventResult::Handled
            }
            TextInputKeyCommand::SelectAll => {
                self.move_cursor_to(self.len_graphemes());
                self.edit.selection_start = Some(TextInputCaretPosition::default());
                EventResult::Handled
            }
            TextInputKeyCommand::Copy => {
                if let Some((start, end)) = self.selection_byte_range() {
                    let _ = ctx.platform.clipboard_copy(&self.edit.text()[start..end]);
                    EventResult::Handled
                } else {
                    EventResult::Handled
                }
            }
            TextInputKeyCommand::Paste => {
                if let Ok(Some(clip)) = ctx.platform.clipboard_paste() {
                    self.commit_text_at_cursor(&clip, EmptyTextCommitPolicy::PreserveSelection);
                }
                EventResult::Handled
            }
            TextInputKeyCommand::Cut => {
                if let Some((start, end)) = self.selection_byte_range() {
                    if ctx.platform.clipboard_copy(&self.edit.text()[start..end]).is_ok() {
                        self.delete_selection();
                    }
                    EventResult::Handled
                } else {
                    EventResult::Handled
                }
            }
            TextInputKeyCommand::MoveLeft { word, extend_selection } => {
                self.edit.navigate(
                    TextInputMetrics::current().font_size,
                    SingleLineTextNavigation::Left { word },
                    extend_selection,
                );
                self.update_scroll(TextInputMetrics::current());
                EventResult::Handled
            }
            TextInputKeyCommand::MoveRight { word, extend_selection } => {
                self.edit.navigate(
                    TextInputMetrics::current().font_size,
                    SingleLineTextNavigation::Right { word },
                    extend_selection,
                );
                self.update_scroll(TextInputMetrics::current());
                EventResult::Handled
            }
            TextInputKeyCommand::MoveHome { extend_selection } => {
                self.edit.navigate(
                    TextInputMetrics::current().font_size,
                    SingleLineTextNavigation::Home,
                    extend_selection,
                );
                self.update_scroll(TextInputMetrics::current());
                EventResult::Handled
            }
            TextInputKeyCommand::MoveEnd { extend_selection } => {
                self.edit.navigate(
                    TextInputMetrics::current().font_size,
                    SingleLineTextNavigation::End,
                    extend_selection,
                );
                self.update_scroll(TextInputMetrics::current());
                EventResult::Handled
            }
            TextInputKeyCommand::DeleteBackward => {
                if !self.delete_selection() {
                    self.delete_grapheme_before();
                }
                self.update_scroll(TextInputMetrics::current());
                EventResult::Handled
            }
            TextInputKeyCommand::DeleteForward => {
                if !self.delete_selection() {
                    self.delete_grapheme_at();
                }
                self.update_scroll(TextInputMetrics::current());
                EventResult::Handled
            }
        }
    }
}

impl Widget for TextInput {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(200.0, 28.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.update_scroll(TextInputMetrics::current());
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.focused
                || self.mouse_down
                || self.has_selection()
                || self.composition.is_active()
            {
                ctx.release_pointer_capture(self.id);
                request_disabled_ime(ctx.requests);
                ctx.request_repaint();
            }
            self.focused = false;
            self.mouse_down = false;
            self.clear_selection();
            self.composition.clear();
            return EventResult::Ignored;
        }
        let metrics = TextInputMetrics::current();
        match event {
            // ── Mouse ──────────────────────────────────────────────────
            UiEvent::MouseDown { position, button: MouseButton::Left, modifiers } => {
                let clicked = self.bounds.contains(*position);
                if clicked {
                    self.begin_edit_session();
                    let text_x = self.text_x_from_pointer(*position);
                    if modifiers.shift {
                        // Shift+Click: extend selection from anchor (or current cursor)
                        if self.edit.selection_start.is_none() {
                            self.edit.selection_start = Some(self.edit.cursor);
                        }
                        self.set_cursor_from_text_x(text_x, metrics);
                    } else {
                        self.set_cursor_from_text_x(text_x, metrics);
                        self.clear_selection();
                    }
                    self.mouse_down = true;
                    self.composition.clear();
                    ctx.request_pointer_capture(self.id);
                    ctx.request_repaint();
                } else {
                    self.commit_edit_session(ctx);
                    let changed = self.focused
                        || self.mouse_down
                        || self.has_selection()
                        || self.composition.is_active();
                    self.focused = false;
                    self.clear_selection();
                    self.mouse_down = false;
                    self.composition.clear();
                    ctx.release_pointer_capture(self.id);
                    request_disabled_ime(ctx.requests);
                    self.refresh_ime_area(ctx);
                    if changed {
                        ctx.request_repaint();
                    }
                    return EventResult::Ignored;
                }
                self.focused = clicked;
                self.refresh_ime_area(ctx);
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                if self.bounds.contains(*position) && self.enabled {
                    ctx.set_cursor(CursorRequest::Text);
                }
                if self.mouse_down {
                    if self.edit.selection_start.is_none() {
                        self.edit.selection_start = Some(self.edit.cursor);
                    }
                    // Allow drag beyond bounds — clamp to valid range
                    let text_x = self.text_x_from_pointer(*position);
                    self.set_cursor_from_text_x(text_x, metrics);
                    self.refresh_ime_area(ctx);
                    ctx.request_repaint();
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } => {
                if !self.mouse_down {
                    return EventResult::Ignored;
                }
                self.mouse_down = false;
                if !self.has_selection() {
                    self.clear_selection();
                }
                ctx.release_pointer_capture(self.id);
                ctx.request_repaint();
                EventResult::Handled
            }
            // ── Focus ──────────────────────────────────────────────────
            UiEvent::FocusGained { .. } => {
                self.begin_edit_session();
                self.focused = true;
                self.cursor_visible.set(true);
                self.last_blink.set(Instant::now());
                self.update_scroll(metrics);
                self.refresh_ime_area(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.commit_edit_session(ctx);
                let changed = self.focused
                    || self.mouse_down
                    || self.has_selection()
                    || self.composition.is_active();
                self.focused = false;
                self.mouse_down = false;
                self.clear_selection();
                self.composition.clear();
                ctx.release_pointer_capture(self.id);
                request_disabled_ime(ctx.requests);
                if changed {
                    ctx.request_repaint();
                }
                EventResult::Handled
            }
            // ── Keyboard ───────────────────────────────────────────────
            UiEvent::KeyDown { key, modifiers } if self.focused => {
                match classify_ime_key(self.focused, self.composition.is_active(), *key, *modifiers)
                {
                    ImeKeyDisposition::ClearComposition => {
                        self.composition.clear();
                        self.refresh_ime_area(ctx);
                        ctx.request_repaint();
                        return EventResult::Handled;
                    }
                    ImeKeyDisposition::ConsumeDuringComposition => {
                        self.refresh_ime_area(ctx);
                        return EventResult::Handled;
                    }
                    ImeKeyDisposition::RouteNormally => {}
                }

                if *modifiers == Modifiers::none() && *key == KeyCode::Enter {
                    if self.on_commit.is_none() {
                        return EventResult::Ignored;
                    }
                    self.commit_edit_session(ctx);
                    self.finish_focus(ctx);
                    return EventResult::Handled;
                }
                if *modifiers == Modifiers::none()
                    && *key == KeyCode::Escape
                    && self.on_commit.is_some()
                {
                    self.cancel_edit_session();
                    self.finish_focus(ctx);
                    return EventResult::Handled;
                }

                let before = self.edit.clone();
                let command = classify_key_command(*key, *modifiers, PlatformKeymap::current());
                let result = command.map_or(EventResult::Ignored, |command| {
                    self.execute_key_command(command, ctx)
                });
                if result == EventResult::Handled {
                    self.refresh_ime_area(ctx);
                    if self.edit.text != before.text {
                        if !matches!(
                            command,
                            Some(TextInputKeyCommand::Undo | TextInputKeyCommand::Redo)
                        ) {
                            self.save_local_history(before, false);
                        }
                        self.dispatch_change(ctx);
                    }
                    ctx.request_repaint();
                }
                result
            }
            // ── Text input ─────────────────────────────────────────────
            UiEvent::TextInput(ch) if self.focused => {
                let before = self.edit.clone();
                let text_changed =
                    self.commit_text_at_cursor(ch, EmptyTextCommitPolicy::ReplaceSelection);
                self.composition.clear();
                self.refresh_ime_area(ctx);
                if text_changed {
                    self.save_local_history(before, true);
                    self.dispatch_change(ctx);
                }
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::ImeCommit(ch) if self.focused => {
                let before = self.edit.clone();
                self.composition.clear();
                let text_changed =
                    self.commit_text_at_cursor(ch, EmptyTextCommitPolicy::ReplaceSelection);
                self.refresh_ime_area(ctx);
                if text_changed {
                    self.save_local_history(before, false);
                    self.dispatch_change(ctx);
                }
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::ImePreedit(preedit) if self.focused => {
                self.composition.set_preedit(preedit.clone());
                self.refresh_ime_area(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::ImeCancel if self.focused => {
                self.composition.clear();
                self.refresh_ime_area(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn commit_pending_input(&mut self, ctx: &mut EventContext) -> bool {
        if !self.focused || !self.enabled || self.composition.is_active() {
            return false;
        }
        let changed = self
            .edit_session_origin
            .as_ref()
            .is_some_and(|origin| origin != &self.edit.text);
        self.commit_edit_session(ctx);
        self.begin_edit_session();
        changed
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let metrics = TextInputMetrics::from_theme(ctx.theme);
        let selection = self.edit.selection_geometry(metrics.font_size);
        let caret_text_x = selection.as_ref().map_or(0.0, |selection| selection.line.caret_x);
        let geometry = compute_text_geometry(
            self.bounds,
            self.scroll_x.get(),
            caret_text_x,
            self.composition.is_active().then(|| self.composition.preedit()),
            metrics,
        );
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

        let display = mondrian_ui_text::SingleLineTextProjection::new(self.edit.text());
        paint_text_input(
            ctx,
            TextInputPaintSnapshot {
                bounds: self.bounds,
                geometry,
                enabled: self.enabled,
                focused: self.focused,
                cursor_visible,
                has_selection: self.has_selection(),
                text: display.display_text(),
                placeholder: &self.placeholder,
                selection_spans: selection
                    .as_ref()
                    .map_or(&[], |selection| selection.spans.as_slice()),
                caret_text_x,
                preedit: self.composition.is_active().then(|| self.composition.preedit()),
            },
        );
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn edit_command_available(
        &self,
        command: mondrian_ui_core::widget::EditCommand,
    ) -> Option<bool> {
        use mondrian_ui_core::widget::EditCommand;
        Some(
            self.enabled
                && !self.composition.is_active()
                && (command == EditCommand::Paste || self.has_selection()),
        )
    }

    fn edit_command(
        &mut self,
        command: mondrian_ui_core::widget::EditCommand,
        ctx: &mut EventContext,
    ) -> EventResult {
        use mondrian_ui_core::widget::EditCommand;
        if self.edit_command_available(command) != Some(true) {
            return EventResult::Handled;
        }
        let command = match command {
            EditCommand::Cut => TextInputKeyCommand::Cut,
            EditCommand::Copy => TextInputKeyCommand::Copy,
            EditCommand::Paste => TextInputKeyCommand::Paste,
        };
        let before = self.edit.clone();
        self.execute_key_command(command, ctx);
        self.refresh_ime_area(ctx);
        if self.edit.text != before.text {
            self.save_local_history(before, false);
            self.dispatch_change(ctx);
        }
        ctx.request_repaint();
        EventResult::Handled
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }

    fn accepts_text_input(&self) -> bool {
        self.enabled && self.focused
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
                .with_value(AccessibilityValue::Text(self.edit.text.clone())),
        )
    }

    fn accessibility_bounds(&self) -> Option<Rect> {
        Some(self.bounds)
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
    use crate::test_utils::{
        make_event_ctx, primary_text_modifiers, word_text_modifiers, DummyFocus, DummyShortcut,
        DummyTooltip,
    };
    use mondrian_editor_state::Action;
    use mondrian_platform_core::{
        ClipboardError, FileDialogError, FileDialogOutcome, FileFilter, FileRevealError,
        PlatformService,
    };
    use mondrian_ui_core::widget::{DrawCommandEncoder, EventRequests};
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    fn layout(ti: &mut TextInput) {
        ti.layout(Rect::new(0.0, 0.0, 200.0, 28.0));
    }
    fn mk_ctx<'a>(
        f: &'a mut DummyFocus,
        s: &'a mut DummyShortcut,
        t: &'a mut DummyTooltip,
    ) -> EventContext<'a> {
        make_event_ctx(f, s, t, &|_| {})
    }
    fn md(ti: &mut TextInput, x: f32, y: f32, ctx: &mut EventContext) {
        ti.event(
            &UiEvent::MouseDown {
                position: Point::new(x, y),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            ctx,
        );
    }
    fn md_shift(ti: &mut TextInput, x: f32, y: f32, ctx: &mut EventContext) {
        ti.event(
            &UiEvent::MouseDown {
                position: Point::new(x, y),
                button: MouseButton::Left,
                modifiers: Modifiers::shift(),
            },
            ctx,
        );
    }
    fn mm(ti: &mut TextInput, x: f32, y: f32, ctx: &mut EventContext) {
        ti.event(
            &UiEvent::MouseMove {
                position: Point::new(x, y),
                modifiers: Modifiers::none(),
            },
            ctx,
        );
    }
    fn mu(ti: &mut TextInput, ctx: &mut EventContext) {
        ti.event(
            &UiEvent::MouseUp {
                position: Point::ZERO,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            ctx,
        );
    }
    fn kd(ti: &mut TextInput, key: KeyCode, ctx: &mut EventContext) {
        ti.event(&UiEvent::KeyDown { key, modifiers: Modifiers::none() }, ctx);
    }
    fn kd_shift(ti: &mut TextInput, key: KeyCode, ctx: &mut EventContext) {
        ti.event(
            &UiEvent::KeyDown { key, modifiers: Modifiers::shift() },
            ctx,
        );
    }
    fn kd_primary(ti: &mut TextInput, key: KeyCode, ctx: &mut EventContext) {
        ti.event(
            &UiEvent::KeyDown { key, modifiers: primary_text_modifiers() },
            ctx,
        );
    }
    fn kd_word(ti: &mut TextInput, key: KeyCode, ctx: &mut EventContext) {
        ti.event(
            &UiEvent::KeyDown { key, modifiers: word_text_modifiers() },
            ctx,
        );
    }
    fn kd_mod(ti: &mut TextInput, key: KeyCode, modifiers: Modifiers, ctx: &mut EventContext) {
        ti.event(&UiEvent::KeyDown { key, modifiers }, ctx);
    }
    fn tp(ti: &mut TextInput, ch: &str, ctx: &mut EventContext) {
        ti.event(&UiEvent::TextInput(ch.to_string()), ctx);
    }
    fn change_action(text: &str) -> Action {
        Action::Custom {
            namespace: "test.text_input".into(),
            name: format!("change:{text}"),
            payload: Default::default(),
        }
    }

    struct ClipboardPlatform {
        paste_result: Result<Option<String>, ClipboardError>,
        copy_result: Result<(), ClipboardError>,
    }

    impl ClipboardPlatform {
        fn with_text(text: impl Into<String>) -> Self {
            Self {
                paste_result: Ok(Some(text.into())),
                copy_result: Ok(()),
            }
        }

        fn paste_failure() -> Self {
            Self {
                paste_result: Err(ClipboardError::ReadFailed),
                copy_result: Ok(()),
            }
        }

        fn copy_failure() -> Self {
            Self {
                paste_result: Ok(None),
                copy_result: Err(ClipboardError::WriteFailed),
            }
        }
    }

    impl PlatformService for ClipboardPlatform {
        fn clipboard_copy(&self, _text: &str) -> Result<(), ClipboardError> {
            self.copy_result
        }

        fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
            self.paste_result.clone()
        }

        fn open_file_dialog(
            &self,
            _title: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<Vec<PathBuf>>, FileDialogError> {
            Err(FileDialogError::Unavailable)
        }

        fn save_file_dialog(
            &self,
            _title: &str,
            _default_name: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<PathBuf>, FileDialogError> {
            Err(FileDialogError::Unavailable)
        }

        fn reveal_in_file_manager(&self, _path: &Path) -> Result<(), FileRevealError> {
            Err(FileRevealError::Unavailable)
        }
    }

    #[derive(Debug, PartialEq)]
    enum PaintOp {
        PushClip,
        PopClip,
        Rect,
        Text(String),
        Line,
    }

    #[derive(Default)]
    struct RecordingEncoder {
        ops: Vec<PaintOp>,
        clips: Vec<Rect>,
        rects: Vec<Rect>,
        lines: Vec<(Point, Point, f32)>,
        texts: Vec<(String, Point)>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
            self.ops.push(PaintOp::PushClip);
        }

        fn pop_clip(&mut self) {
            self.ops.push(PaintOp::PopClip);
        }

        fn draw_rect(&mut self, bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {
            self.rects.push(bounds);
            self.ops.push(PaintOp::Rect);
        }

        fn draw_line(
            &mut self,
            start: Point,
            end: Point,
            width: f32,
            _color: mondrian_core::Color,
        ) {
            self.lines.push((start, end, width));
            self.ops.push(PaintOp::Line);
        }

        fn draw_text(
            &mut self,
            text: &str,
            _font_size: f32,
            position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push((text.into(), position));
            self.ops.push(PaintOp::Text(text.into()));
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    // ── Construction ─────────────────────────────────────────────────────

    #[test]
    fn new_is_empty() {
        let ti = TextInput::new("ph");
        assert!(ti.text().is_empty());
        assert_eq!(ti.edit.cursor.grapheme_index, 0);
        assert!(!ti.has_selection());
        assert!(!ti.focused);
    }

    #[test]
    fn text_edit_state_deletes_selection_and_collapses_cursor_to_start() {
        let mut edit = TextEditState::with_text("before after".into());
        edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(7));
        edit.cursor = TextInputCaretPosition::at_logical_boundary(12);

        assert_eq!(edit.selection_byte_range(), Some((7, 12)));
        assert!(edit.delete_selection());
        assert_eq!(edit.text(), "before ");
        assert_eq!(edit.cursor.grapheme_index, 7);
        assert!(!edit.has_selection());
    }

    #[test]
    fn text_edit_state_deletes_whole_grapheme_clusters() {
        let mut edit = TextEditState::with_text("a👨‍👩‍👧‍👦b".into());
        edit.cursor = TextInputCaretPosition::at_logical_boundary(2);

        assert_eq!(edit.len_graphemes(), 3);
        assert!(edit.delete_grapheme_before());
        assert_eq!(edit.text(), "ab");
        assert_eq!(edit.cursor.grapheme_index, 1);
    }

    #[test]
    fn text_input_accessibility_exposes_name_value_and_state() {
        let mut ti = TextInput::new("Search assets").with_text("clip");
        ti.focused = true;

        let node = ti.accessibility().expect("text input should expose accessibility");

        assert_eq!(node.role, AccessibilityRole::TextInput);
        assert_eq!(node.name.as_deref(), Some("Search assets"));
        assert!(node.state.focusable);
        assert!(node.state.focused);
        assert_eq!(node.value, Some(AccessibilityValue::Text("clip".into())));
    }

    #[test]
    fn disabled_input_ignores_mouse_and_does_not_enable_ime() {
        let mut ti = TextInput::new("ph").with_text("abc").disabled();
        layout(&mut ti);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        let result = ti.event(
            &UiEvent::MouseDown {
                position: Point::new(20.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert!(!ti.focused);
        assert!(!ti.can_focus());
        assert_eq!(ti.text(), "abc");
        assert!(ctx.requests.ime.is_none());
    }

    #[test]
    fn on_change_dispatches_after_text_input() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = TextInput::new("ph").on_change(change_action);
        layout(&mut input);

        md(&mut input, 12.0, 12.0, &mut ctx);
        tp(&mut input, "你", &mut ctx);

        assert_eq!(input.text(), "你");
        assert_eq!(actions.borrow().as_slice(), &[change_action("你")]);
    }

    #[test]
    fn on_change_ignores_cursor_navigation() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = TextInput::new("ph").with_text("hello").on_change(change_action);
        layout(&mut input);

        md(&mut input, 12.0, 12.0, &mut ctx);
        kd(&mut input, KeyCode::Left, &mut ctx);
        kd(&mut input, KeyCode::Home, &mut ctx);

        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn on_change_dispatches_for_ime_commit_not_preedit() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = TextInput::new("ph").on_change(change_action);
        layout(&mut input);

        md(&mut input, 12.0, 12.0, &mut ctx);
        input.event(&UiEvent::ImePreedit("ni".into()), &mut ctx);
        assert!(actions.borrow().is_empty());

        input.event(&UiEvent::ImeCommit("你".into()), &mut ctx);

        assert_eq!(input.text(), "你");
        assert_eq!(actions.borrow().as_slice(), &[change_action("你")]);
    }

    #[test]
    fn form_submission_commits_text_once_and_establishes_the_next_cancel_baseline() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = TextInput::new("ph").with_text("Bus 1").on_commit(change_action);
        layout(&mut input);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        input.select_all();
        tp(&mut input, "Dialogue", &mut ctx);
        input.event(&UiEvent::ImePreedit("99".to_owned()), &mut ctx);
        assert!(!input.commit_pending_input(&mut ctx));
        assert!(actions.borrow().is_empty());
        input.event(&UiEvent::ImeCancel, &mut ctx);
        assert!(input.commit_pending_input(&mut ctx));
        assert!(!input.commit_pending_input(&mut ctx));
        assert!(input.is_focused());
        assert_eq!(actions.borrow().as_slice(), &[change_action("Dialogue")]);
        input.select_all();
        tp(&mut input, "Discarded", &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(input.text(), "Dialogue");
        assert!(!input.is_focused());
        assert!(!input.commit_pending_input(&mut ctx));
        input.event(&UiEvent::FocusLost, &mut ctx);
        assert_eq!(actions.borrow().as_slice(), &[change_action("Dialogue")]);
    }

    #[test]
    fn on_commit_keeps_typing_local_and_dispatches_once_on_enter() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = TextInput::new("ph").with_text("Bus 1").on_commit(change_action);
        layout(&mut input);

        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        input.select_all();
        tp(&mut input, "Dialogue", &mut ctx);
        assert_eq!(input.text(), "Dialogue");
        assert!(actions.borrow().is_empty());

        assert_eq!(
            input.event(
                &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(actions.borrow().as_slice(), &[change_action("Dialogue")]);
        assert!(!input.is_focused());

        input.event(&UiEvent::FocusLost, &mut ctx);
        assert_eq!(actions.borrow().as_slice(), &[change_action("Dialogue")]);
    }

    #[test]
    fn on_commit_dispatches_composed_text_once_on_focus_loss() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = TextInput::new("ph").on_commit(change_action);
        layout(&mut input);

        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        input.event(&UiEvent::ImePreedit("dui hua".into()), &mut ctx);
        input.event(&UiEvent::ImeCommit("对话".into()), &mut ctx);
        assert!(actions.borrow().is_empty());

        input.event(&UiEvent::FocusLost, &mut ctx);
        assert_eq!(actions.borrow().as_slice(), &[change_action("对话")]);
    }

    #[test]
    fn on_commit_escape_restores_origin_without_dispatching() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = TextInput::new("ph").with_text("Original").on_commit(change_action);
        layout(&mut input);

        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        input.select_all();
        tp(&mut input, "Draft", &mut ctx);
        kd(&mut input, KeyCode::Escape, &mut ctx);

        assert_eq!(input.text(), "Original");
        assert!(!input.is_focused());
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn on_commit_ignores_unchanged_sessions_and_disabling_cancels_draft() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = TextInput::new("ph").with_text("Original").on_commit(change_action);
        layout(&mut input);

        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::Enter, &mut ctx);
        assert!(actions.borrow().is_empty());

        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        input.set_text("External".to_owned());
        input.event(&UiEvent::FocusLost, &mut ctx);
        assert_eq!(input.text(), "External");
        assert!(actions.borrow().is_empty());

        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        input.select_all();
        tp(&mut input, "Draft", &mut ctx);
        input.set_enabled(false);

        assert_eq!(input.text(), "External");
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn with_text() {
        let ti = TextInput::new("ph").with_text("hello");
        assert_eq!(ti.text(), "hello");
        assert_eq!(ti.edit.cursor.grapheme_index, 5);
    }

    #[test]
    fn with_text_cjk() {
        let ti = TextInput::new("ph").with_text("你好世界");
        assert_eq!(ti.text(), "你好世界");
        assert_eq!(ti.edit.cursor.grapheme_index, 4);
    }

    // ── Focus ────────────────────────────────────────────────────────────

    #[test]
    fn click_inside_focuses() {
        let mut ti = TextInput::new("ph");
        layout(&mut ti);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        md(&mut ti, 100.0, 14.0, &mut ctx);
        assert!(ti.focused);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn click_outside_blurs() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        layout(&mut ti);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        let result = ti.event(
            &UiEvent::MouseDown {
                position: Point::new(300.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(!ti.focused);
        assert_eq!(result, EventResult::Ignored);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn focus_lost_clears_selection() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(0));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(2);
        assert!(ti.has_selection());
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        ti.event(&UiEvent::FocusLost, &mut ctx);
        assert!(!ti.focused);
        assert!(!ti.has_selection());
        assert!(ctx.requests.repaint);
    }

    // ── Typing / IME ─────────────────────────────────────────────────────

    #[test]
    fn type_chars() {
        let mut ti = TextInput::new("ph");
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        tp(&mut ti, "a", &mut ctx);
        tp(&mut ti, "b", &mut ctx);
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.edit.cursor.grapheme_index, 2);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn type_multi_char_ime() {
        let mut ti = TextInput::new("ph");
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        tp(&mut ti, "你好", &mut ctx);
        assert_eq!(ti.text(), "你好");
        assert_eq!(ti.edit.cursor.grapheme_index, 2);
    }

    #[test]
    fn text_input_normalizes_line_breaks_for_single_line_editing() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = TextInput::new("ph").on_change(change_action);
        input.focused = true;

        input.event(
            &UiEvent::TextInput("a\nb\r\nc\u{2028}d\u{2029}e".to_owned()),
            &mut ctx,
        );

        assert_eq!(input.text(), "a b c d e");
        assert_eq!(actions.borrow().as_slice(), &[change_action("a b c d e")]);
    }

    #[test]
    fn paste_normalizes_line_breaks_for_single_line_editing() {
        let platform = ClipboardPlatform::with_text("first\r\nsecond\u{2028}third");
        let mut ti = TextInput::new("ph").with_text("before after").on_change(change_action);
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(7));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(12);
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut focus,
            shortcut: &mut shortcuts,
            tooltip: &mut tooltip,
            dispatch: &dispatch,
            platform: &platform,
            requests: &mut requests,
        };

        kd_primary(&mut ti, KeyCode::V, &mut ctx);

        assert_eq!(ti.text(), "before first second third");
        assert_eq!(
            actions.borrow().as_slice(),
            &[change_action("before first second third")]
        );
    }

    #[test]
    fn paste_empty_clipboard_preserves_selection_without_dispatching_change() {
        let platform = ClipboardPlatform::with_text("");
        let mut ti = TextInput::new("ph").with_text("before after").on_change(change_action);
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(7));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(12);
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut focus,
            shortcut: &mut shortcuts,
            tooltip: &mut tooltip,
            dispatch: &dispatch,
            platform: &platform,
            requests: &mut requests,
        };

        kd_primary(&mut ti, KeyCode::V, &mut ctx);

        assert_eq!(ti.text(), "before after");
        assert_eq!(ti.selection_byte_range(), Some((7, 12)));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn paste_failure_preserves_selection_without_dispatching_change() {
        let platform = ClipboardPlatform::paste_failure();
        let mut ti = TextInput::new("ph").with_text("before after").on_change(change_action);
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(7));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(12);
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut focus,
            shortcut: &mut shortcuts,
            tooltip: &mut tooltip,
            dispatch: &dispatch,
            platform: &platform,
            requests: &mut requests,
        };

        kd_primary(&mut ti, KeyCode::V, &mut ctx);

        assert_eq!(ti.text(), "before after");
        assert_eq!(ti.selection_byte_range(), Some((7, 12)));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn text_composition_state_tracks_preedit_activity() {
        let mut composition = TextCompositionState::default();
        assert!(!composition.is_active());
        assert_eq!(composition.preedit(), "");

        composition.set_preedit("ni".into());
        assert!(composition.is_active());
        assert_eq!(composition.preedit(), "ni");

        composition.clear();
        assert!(!composition.is_active());
        assert_eq!(composition.preedit(), "");
    }

    #[test]
    fn ime_preedit_is_stored_until_commit() {
        let mut ti = TextInput::new("ph").with_text("ni");
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        let result = ti.event(&UiEvent::ImePreedit("你".into()), &mut ctx);
        assert_eq!(result, EventResult::Handled);
        assert_eq!(ti.composition.preedit(), "你");

        let result = ti.event(&UiEvent::ImeCommit("你".into()), &mut ctx);
        assert_eq!(result, EventResult::Handled);
        assert!(!ti.composition.is_active());
    }

    #[test]
    fn ime_preedit_moves_caret_after_composition_text() {
        let mut ti = TextInput::new("ph").with_text("ab");
        layout(&mut ti);
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        let result = ti.event(&UiEvent::ImePreedit("ni".into()), &mut ctx);

        assert_eq!(result, EventResult::Handled);
        let area = ctx
            .requests
            .ime
            .expect("preedit should update IME cursor area")
            .cursor_area
            .expect("preedit should keep IME cursor visible");
        let metrics = TextInputMetrics::default();
        let expected_x = ti.content_left()
            + measure_text_width("ab", metrics.font_size)
            + measure_text_width("ni", metrics.font_size);
        assert!((area.x - expected_x).abs() <= 0.1);
    }

    #[test]
    fn ime_preedit_owns_keydown_without_mutating_committed_text() {
        let mut ti = TextInput::new("ph").with_text("abc").on_change(change_action);
        layout(&mut ti);
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(3);
        ti.composition.set_preedit("ni".into());
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        for (key, modifiers) in [
            (KeyCode::Backspace, Modifiers::none()),
            (KeyCode::Delete, Modifiers::none()),
            (KeyCode::Left, Modifiers::none()),
            (KeyCode::Right, Modifiers::none()),
            (KeyCode::A, primary_text_modifiers()),
        ] {
            assert_eq!(
                ti.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Handled
            );
            assert_eq!(ti.text(), "abc");
            assert_eq!(ti.edit.cursor.grapheme_index, 3);
            assert_eq!(ti.composition.preedit(), "ni");
            assert!(!ti.has_selection());
        }
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn ime_preedit_escape_clears_composition_without_committing_text() {
        let mut ti = TextInput::new("ph").with_text("abc").on_change(change_action);
        layout(&mut ti);
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(3);
        ti.composition.set_preedit("ni".into());
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = ti.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(ti.text(), "abc");
        assert!(!ti.composition.is_active());
        assert!(ctx.requests.repaint);
        assert!(ctx.requests.ime.is_some_and(|ime| ime.enabled));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn ime_cancel_clears_composition_without_committing_text() {
        let mut ti = TextInput::new("ph").with_text("abc").on_change(change_action);
        layout(&mut ti);
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(3);
        ti.composition.set_preedit("ni".into());
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = ti.event(&UiEvent::ImeCancel, &mut ctx);

        assert_eq!(result, EventResult::Handled);
        assert_eq!(ti.text(), "abc");
        assert!(!ti.composition.is_active());
        assert!(ctx.requests.repaint);
        assert!(ctx.requests.ime.is_some_and(|ime| ime.enabled));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn ime_commit_replaces_selection_and_clears_preedit() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut ti = TextInput::new("ph").with_text("before after").on_change(change_action);
        layout(&mut ti);
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(7));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(12);
        ti.composition.set_preedit("候选".into());
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = ti.event(&UiEvent::ImeCommit("中".into()), &mut ctx);

        assert_eq!(result, EventResult::Handled);
        assert_eq!(ti.text(), "before 中");
        assert_eq!(ti.edit.cursor.grapheme_index, 8);
        assert!(!ti.has_selection());
        assert!(!ti.composition.is_active());
        assert_eq!(actions.borrow().as_slice(), &[change_action("before 中")]);
        assert!(ctx.requests.ime.is_some_and(|ime| ime.enabled));
    }

    #[test]
    fn empty_ime_commit_replaces_selection_and_dispatches_when_text_changes() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut ti = TextInput::new("ph").with_text("abc").on_change(change_action);
        layout(&mut ti);
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(1));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(2);
        ti.composition.set_preedit("候选".into());
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = ti.event(&UiEvent::ImeCommit(String::new()), &mut ctx);

        assert_eq!(result, EventResult::Handled);
        assert_eq!(ti.text(), "ac");
        assert_eq!(ti.edit.cursor.grapheme_index, 1);
        assert!(!ti.has_selection());
        assert!(!ti.composition.is_active());
        assert_eq!(actions.borrow().as_slice(), &[change_action("ac")]);
        assert!(ctx.requests.repaint);
        assert!(ctx.requests.ime.is_some_and(|ime| ime.enabled));
    }

    #[test]
    fn mouse_down_requests_ime_at_caret() {
        let mut ti = TextInput::new("ph").with_text("abc");
        layout(&mut ti);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        md(&mut ti, 8.0, 14.0, &mut ctx);

        let ime = ctx.requests.ime.expect("mouse down should enable IME");
        assert!(ime.enabled);
        let area = ime.cursor_area.expect("IME should receive a caret rect");
        assert!((area.x - ti.content_left()).abs() <= 0.1);
        assert!(area.height > 0.0);
    }

    #[test]
    fn key_navigation_updates_ime_caret_area() {
        let mut ti = TextInput::new("ph").with_text("abc");
        layout(&mut ti);
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        kd(&mut ti, KeyCode::Left, &mut ctx);

        let area = ctx
            .requests
            .ime
            .expect("handled key navigation should update IME")
            .cursor_area
            .expect("IME should receive a caret rect");
        let expected = ti.cursor_area(TextInputMetrics::default());
        assert!((area.x - expected.x).abs() <= 0.1);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn type_replaces_selection() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(1));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(4);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        tp(&mut ti, "X", &mut ctx);
        assert_eq!(ti.text(), "hXo");
        assert_eq!(ti.edit.cursor.grapheme_index, 2);
        assert!(!ti.has_selection());
    }

    // ── Grapheme cluster ─────────────────────────────────────────────────

    #[test]
    fn emoji_grapheme_cluster() {
        let ti = TextInput::new("ph").with_text("👨‍👩‍👧‍👦abc");
        // Family emoji is one grapheme cluster (multiple chars/bytes)
        assert_eq!(ti.len_graphemes(), 4); // family + a + b + c
    }

    #[test]
    fn backspace_deletes_grapheme() {
        let mut ti = TextInput::new("ph").with_text("a😊b");
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(2); // after the emoji
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Backspace, &mut ctx);
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.edit.cursor.grapheme_index, 1);
    }

    #[test]
    fn delete_deletes_grapheme() {
        let mut ti = TextInput::new("ph").with_text("a😊b");
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(1); // before the emoji
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Delete, &mut ctx);
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.edit.cursor.grapheme_index, 1);
    }

    // ── Backspace / Delete ───────────────────────────────────────────────

    #[test]
    fn backspace_at_start_noop() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(0);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Backspace, &mut ctx);
        assert_eq!(ti.text(), "abc");
    }

    #[test]
    fn backspace_deletes_selection() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(1));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(4);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Backspace, &mut ctx);
        assert_eq!(ti.text(), "ho");
        assert_eq!(ti.edit.cursor.grapheme_index, 1);
    }

    #[test]
    fn delete_at_end_noop() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(3);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Delete, &mut ctx);
        assert_eq!(ti.text(), "abc");
    }

    #[test]
    fn delete_selection_select_all() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(0));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(5);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Delete, &mut ctx);
        assert_eq!(ti.text(), "");
        assert_eq!(ti.edit.cursor.grapheme_index, 0);
    }

    // ── Arrow / Home / End ───────────────────────────────────────────────

    #[test]
    fn left_right_navigate() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 2);
        kd(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 3);
    }

    #[test]
    fn home_end() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(1);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Home, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 0);
        kd(&mut ti, KeyCode::End, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 3);
    }

    // ── Word navigation ──────────────────────────────────────────────────

    #[test]
    fn native_word_left() {
        let mut ti = TextInput::new("ph").with_text("hello world foo");
        ti.focused = true;
        // "hello world foo" = 15 graphemes: hello(5) + space(1) + world(5) + space(1) + foo(3)
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(15); // end
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd_word(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 12); // start of "foo"
        kd_word(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 6); // start of "world"
        kd_word(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 0); // start of "hello"
    }

    #[test]
    fn native_word_right() {
        let mut ti = TextInput::new("ph").with_text("hello world");
        ti.focused = true;
        // "hello world" = 11 graphemes: hello(5) + space(1) + world(5)
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(0);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd_word(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 6); // past "hello" + space, at "world" start
        kd_word(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 11); // past "world", at end
    }

    #[test]
    fn native_word_navigation_extends_selection() {
        let mut ti = TextInput::new("ph").with_text("hello world");
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(11);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        kd_mod(
            &mut ti,
            KeyCode::Left,
            Modifiers { shift: true, ..word_text_modifiers() },
            &mut ctx,
        );

        assert_eq!(ti.edit.cursor.grapheme_index, 6);
        assert_eq!(
            ti.edit.selection_start.map(|caret| caret.grapheme_index),
            Some(11)
        );
        assert_eq!(ti.selection_byte_range(), Some((6, 11)));
    }

    #[test]
    fn native_word_navigation_uses_unicode_whitespace_boundaries() {
        let mut ti = TextInput::new("ph").with_text("alpha\tbeta\n\u{00A0}gamma");
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(ti.len_graphemes());
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        kd_word(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 12);
        kd_word(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 6);
        kd(&mut ti, KeyCode::Home, &mut ctx);
        kd_word(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 6);
        kd_word(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 12);
    }

    // ── Shift selection ──────────────────────────────────────────────────

    #[test]
    fn shift_left_selects() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(3);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd_shift(&mut ti, KeyCode::Left, &mut ctx);
        assert!(ti.has_selection());
        assert_eq!(
            ti.edit.selection_start.map(|caret| caret.grapheme_index),
            Some(3)
        );
    }

    #[test]
    fn shift_navigation_at_boundaries_preserves_empty_anchor() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(0);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        kd_shift(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 0);
        assert_eq!(
            ti.edit.selection_start.map(|caret| caret.grapheme_index),
            Some(0)
        );
        assert!(!ti.has_selection());

        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(ti.len_graphemes());
        ti.clear_selection();
        kd_shift(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 3);
        assert_eq!(
            ti.edit.selection_start.map(|caret| caret.grapheme_index),
            Some(3)
        );
        assert!(!ti.has_selection());
    }

    #[test]
    fn shift_click_extends() {
        let mut ti = TextInput::new("ph").with_text("hello world");
        layout(&mut ti);
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        // First click at start
        md(&mut ti, 8.0, 14.0, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 0);
        // Shift+click near end
        md_shift(&mut ti, 150.0, 14.0, &mut ctx);
        assert!(ti.has_selection());
        assert_eq!(
            ti.edit.selection_start.map(|caret| caret.grapheme_index),
            Some(0)
        );
        assert!(ti.edit.cursor.grapheme_index > 0);
    }

    // ── Selection byte range ─────────────────────────────────────────────

    #[test]
    fn selection_range_forward() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(1));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(4);
        assert_eq!(ti.selection_byte_range(), Some((1, 4)));
    }

    #[test]
    fn selection_range_reverse() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(4));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(1);
        assert_eq!(ti.selection_byte_range(), Some((1, 4)));
    }

    // ── Platform primary shortcuts ───────────────────────────────────────

    #[test]
    fn primary_a_select_all() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd_primary(&mut ti, KeyCode::A, &mut ctx);
        assert_eq!(ti.edit.cursor.grapheme_index, 5);
        assert_eq!(
            ti.edit.selection_start.map(|caret| caret.grapheme_index),
            Some(0)
        );
    }

    #[test]
    fn primary_x_cuts() {
        let platform = ClipboardPlatform::with_text("");
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(1));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(4);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let binding = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &binding,
            platform: &platform,
            requests: &mut requests,
        };
        kd_primary(&mut ti, KeyCode::X, &mut ctx);
        assert_eq!(ti.text(), "ho");
        assert_eq!(ti.edit.cursor.grapheme_index, 1);
        assert!(!ti.has_selection());
    }

    #[test]
    fn primary_x_preserves_selection_when_clipboard_copy_fails() {
        let platform = ClipboardPlatform::copy_failure();
        let mut ti = TextInput::new("ph").with_text("hello").on_change(change_action);
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(1));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(4);
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut focus,
            shortcut: &mut shortcuts,
            tooltip: &mut tooltip,
            dispatch: &dispatch,
            platform: &platform,
            requests: &mut requests,
        };

        let result = ti.event(
            &UiEvent::KeyDown {
                key: KeyCode::X,
                modifiers: primary_text_modifiers(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(ti.text(), "hello");
        assert_eq!(ti.selection_byte_range(), Some((1, 4)));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn semantic_clipboard_commands_preserve_unicode_and_refuse_failed_cut() {
        use mondrian_ui_core::widget::EditCommand;
        for fail in [false, true] {
            let platform = if fail {
                ClipboardPlatform::copy_failure()
            } else {
                ClipboardPlatform::with_text("🙂你好")
            };
            let mut input = TextInput::new("name").with_text("A🙂中B").on_change(change_action);
            input.focused = true;
            input.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(1));
            input.edit.cursor = TextInputCaretPosition::at_logical_boundary(3);
            let actions = RefCell::new(Vec::new());
            let dispatch = |action| actions.borrow_mut().push(action);
            let mut focus = DummyFocus;
            let mut shortcut = DummyShortcut;
            let mut tooltip = DummyTooltip;
            let mut requests = EventRequests::default();
            let mut ctx = EventContext {
                focus: &mut focus,
                shortcut: &mut shortcut,
                tooltip: &mut tooltip,
                dispatch: &dispatch,
                platform: &platform,
                requests: &mut requests,
            };
            assert_eq!(input.edit_command_available(EditCommand::Cut), Some(true));
            assert_eq!(
                input.edit_command(EditCommand::Cut, &mut ctx),
                EventResult::Handled
            );
            if fail {
                assert_eq!(input.text(), "A🙂中B");
                assert_eq!(input.selection_byte_range(), Some((1, 8)));
                assert!(actions.borrow().is_empty());
            } else {
                assert_eq!(input.text(), "AB");
                assert_eq!(actions.borrow().len(), 1);
                assert_eq!(input.edit_command_available(EditCommand::Cut), Some(false));
                input.edit_command(EditCommand::Cut, &mut ctx);
                assert_eq!(
                    actions.borrow().len(),
                    1,
                    "empty Cut is owned without a Clip fallback"
                );
                input.edit_command(EditCommand::Paste, &mut ctx);
                assert_eq!(input.text(), "A🙂你好B");
                assert_eq!(actions.borrow().len(), 2);
            }
        }
    }

    #[test]
    fn primary_c_without_selection_is_owned_noop() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        let result = ti.event(
            &UiEvent::KeyDown {
                key: KeyCode::C,
                modifiers: primary_text_modifiers(),
            },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Handled);
    }

    #[test]
    fn modified_text_editing_chords_are_ignored_for_shortcut_routing() {
        let platform = ClipboardPlatform::with_text("paste");
        let mut ti = TextInput::new("ph").with_text("hello world").on_change(change_action);
        ti.focused = true;
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(0));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(5);
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = EventContext {
            focus: &mut focus,
            shortcut: &mut shortcuts,
            tooltip: &mut tooltip,
            dispatch: &dispatch,
            platform: &platform,
            requests: &mut requests,
        };

        for (key, modifiers) in [
            (
                KeyCode::A,
                Modifiers { ctrl: true, alt: true, ..Default::default() },
            ),
            (
                KeyCode::C,
                Modifiers { ctrl: true, meta: true, ..Default::default() },
            ),
            (
                KeyCode::V,
                Modifiers { ctrl: true, shift: true, ..Default::default() },
            ),
            (
                KeyCode::X,
                Modifiers { ctrl: true, alt: true, ..Default::default() },
            ),
            (
                KeyCode::Left,
                Modifiers {
                    ctrl: cfg!(target_os = "macos"),
                    alt: !cfg!(target_os = "macos"),
                    ..Default::default()
                },
            ),
            (
                KeyCode::Right,
                Modifiers {
                    ctrl: cfg!(target_os = "macos"),
                    meta: true,
                    ..Default::default()
                },
            ),
            (
                KeyCode::Home,
                Modifiers {
                    ctrl: true,
                    alt: true,
                    shift: true,
                    ..Default::default()
                },
            ),
            (
                KeyCode::End,
                Modifiers {
                    ctrl: cfg!(target_os = "macos"),
                    meta: true,
                    shift: true,
                    ..Default::default()
                },
            ),
            (
                KeyCode::Backspace,
                Modifiers {
                    ctrl: cfg!(target_os = "macos"),
                    alt: !cfg!(target_os = "macos"),
                    ..Default::default()
                },
            ),
            (
                KeyCode::Delete,
                Modifiers {
                    ctrl: cfg!(target_os = "macos"),
                    meta: true,
                    ..Default::default()
                },
            ),
        ] {
            assert_eq!(
                ti.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert_eq!(ti.text(), "hello world");
            assert_eq!(ti.edit.cursor.grapheme_index, 5);
            assert_eq!(ti.selection_byte_range(), Some((0, 5)));
        }
        assert!(actions.borrow().is_empty());
    }

    // ── Mouse drag ───────────────────────────────────────────────────────

    #[test]
    fn idle_mouse_up_is_ignored_for_sibling_event_routing() {
        let mut ti = TextInput::new("ph").with_text("hello");
        layout(&mut ti);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        let result = ti.event(
            &UiEvent::MouseUp {
                position: Point::new(40.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert!(ctx.requests.pointer_capture.is_none());
        assert!(!ti.mouse_down);
    }

    #[test]
    fn click_with_stationary_mouse_move_does_not_select_first_typed_character() {
        let mut ti = TextInput::new("ph");
        layout(&mut ti);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        md(&mut ti, 80.0, 14.0, &mut ctx);
        mm(&mut ti, 80.0, 14.0, &mut ctx);
        mu(&mut ti, &mut ctx);
        assert_eq!(ti.edit.selection_start, None);

        tp(&mut ti, "a", &mut ctx);
        assert!(!ti.has_selection());
        tp(&mut ti, "b", &mut ctx);
        assert_eq!(ti.text(), "ab");
    }

    #[test]
    fn drag_selects() {
        let mut ti = TextInput::new("ph").with_text("hello world");
        layout(&mut ti);
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        md(&mut ti, 8.0, 14.0, &mut ctx);
        assert!(!ti.has_selection());
        ctx.requests.repaint = false;
        mm(&mut ti, 80.0, 14.0, &mut ctx);
        assert!(ti.has_selection());
        assert!(ctx.requests.repaint);
        ctx.requests.repaint = false;
        mu(&mut ti, &mut ctx);
        assert!(ti.has_selection());
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn drag_beyond_bounds_continues() {
        let mut ti = TextInput::new("ph").with_text("hello world");
        layout(&mut ti);
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        md(&mut ti, 8.0, 14.0, &mut ctx);
        // Drag way past the right edge
        mm(&mut ti, 500.0, 14.0, &mut ctx);
        assert!(ti.has_selection());
        // Should have selected to end
        assert_eq!(ti.edit.cursor.grapheme_index, ti.len_graphemes());
    }

    #[test]
    fn layout_scrolls_long_text_to_end_cursor() {
        let mut ti = TextInput::new("ph").with_text("abcdefghijklmnopqrstuvwxyz");
        ti.layout(Rect::new(0.0, 0.0, 80.0, 28.0));

        assert!(ti.scroll_x.get() > 0.0);
        let caret = ti.cursor_area(TextInputMetrics::default());
        assert!(caret.x >= ti.content_left() - 0.1);
        assert!(caret.x <= ti.content_right() + 0.1);
    }

    #[test]
    fn long_text_scroll_uses_renderer_metrics() {
        let text = "abcdefghijklmnopqrstuvwxyz";
        let mut ti = TextInput::new("ph").with_text(text);
        ti.layout(Rect::new(0.0, 0.0, 80.0, 28.0));

        let metrics = TextInputMetrics::default();
        let expected_scroll =
            (measure_text_width(text, metrics.font_size) - ti.visible_width()).max(0.0);
        assert!((ti.scroll_x.get() - expected_scroll).abs() <= 0.1);
    }

    #[test]
    fn long_text_caret_stays_inside_content_rect() {
        let mut ti = TextInput::new("ph").with_text("abcdefghijklmnopqrstuvwxyz");
        ti.layout(Rect::new(0.0, 0.0, 80.0, 28.0));

        let caret = ti.cursor_area(TextInputMetrics::default());
        assert!(caret.x >= ti.content_left());
        assert!(caret.x + caret.width <= ti.content_right() + 0.1);
    }

    #[test]
    fn text_geometry_keeps_clip_text_and_ime_caret_in_one_coordinate_space() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(2);
        ti.composition.set_preedit("ni".into());
        ti.layout(Rect::new(10.0, 20.0, 160.0, 32.0));

        let metrics = TextInputMetrics::default();
        let geometry = ti.text_geometry(metrics);
        let expected_left = 18.0;
        let expected_width = 144.0;
        let expected_caret_x = expected_left
            + measure_text_width("ab", metrics.font_size)
            + measure_text_width("ni", metrics.font_size);

        assert_eq!(
            geometry.clip,
            Rect::new(expected_left, 20.0, expected_width, 32.0)
        );
        assert_eq!(geometry.text_origin.x, expected_left);
        assert_eq!(geometry.content_left, expected_left);
        assert_eq!(geometry.content_right, 162.0);
        assert_eq!(geometry.visible_width, expected_width);
        assert!((geometry.caret.x - expected_caret_x).abs() <= 0.1);
        assert_eq!(geometry.caret.y, 24.0);
        assert_eq!(geometry.caret.width, 2.0);
        assert_eq!(geometry.caret.height, 24.0);
    }

    #[test]
    fn click_uses_scroll_offset_for_long_text() {
        let mut ti = TextInput::new("ph").with_text("abcdefghijklmnopqrstuvwxyz");
        ti.layout(Rect::new(0.0, 0.0, 80.0, 28.0));
        assert!(ti.scroll_x.get() > 0.0);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        let click_x = ti.content_left();

        md(&mut ti, click_x, 14.0, &mut ctx);

        assert!(ti.edit.cursor.grapheme_index > 0);
    }

    #[test]
    fn click_clears_selection() {
        let mut ti = TextInput::new("ph").with_text("hello");
        layout(&mut ti);
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(0));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(3);
        assert!(ti.has_selection());
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        md(&mut ti, 8.0, 14.0, &mut ctx);
        assert!(!ti.has_selection());
    }

    // ── Edge cases ───────────────────────────────────────────────────────

    #[test]
    fn empty_backspace_delete_noop() {
        let mut ti = TextInput::new("ph");
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Backspace, &mut ctx);
        kd(&mut ti, KeyCode::Delete, &mut ctx);
        assert_eq!(ti.text(), "");
    }

    #[test]
    fn delete_selection_at_boundary() {
        let mut ti = TextInput::new("ph").with_text("a");
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(0));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(1);
        assert!(ti.delete_selection());
        assert_eq!(ti.text(), "");
        assert_eq!(ti.edit.cursor.grapheme_index, 0);
        assert!(!ti.delete_selection());
    }

    #[test]
    fn set_text_clears_selection() {
        let mut ti = TextInput::new("ph").with_text("old");
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(0));
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(2);
        ti.set_text("new".into());
        assert_eq!(ti.text(), "new");
        assert_eq!(ti.edit.cursor.grapheme_index, 3);
        assert!(!ti.has_selection());
    }

    #[test]
    fn clear_resets_all() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(1));
        ti.clear();
        assert!(ti.text().is_empty());
        assert_eq!(ti.edit.cursor.grapheme_index, 0);
        assert!(!ti.has_selection());
    }

    #[test]
    fn measure_respects_constraint() {
        let ti = TextInput::new("ph");
        let sz = ti.measure(LayoutConstraint::tight(50.0, 20.0));
        assert_eq!(sz, Size::new(50.0, 20.0));
    }

    #[test]
    fn hit_test_respects_bounds() {
        let mut ti = TextInput::new("ph");
        ti.layout(Rect::new(10.0, 10.0, 200.0, 28.0));
        assert!(ti.hit_test(Point::new(110.0, 24.0)));
        assert!(!ti.hit_test(Point::new(0.0, 0.0)));
    }

    #[test]
    fn paint_draws_caret_after_text() {
        let ti = {
            let mut input = TextInput::new("ph").with_text("abc");
            input.focused = true;
            input.layout(Rect::new(0.0, 0.0, 200.0, 28.0));
            input.cursor_visible.set(true);
            input.last_blink.set(Instant::now());
            input
        };
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            pixels_per_point: 1.0,
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 200.0, 28.0),
        };

        ti.paint(&mut ctx);

        let text_idx = encoder
            .ops
            .iter()
            .position(|op| *op == PaintOp::Text("abc".into()))
            .expect("paint should emit text");
        let pop_idx = encoder
            .ops
            .iter()
            .position(|op| *op == PaintOp::PopClip)
            .expect("paint should pop content clip");
        let caret_idx = encoder.ops[..pop_idx]
            .iter()
            .rposition(|op| *op == PaintOp::Rect)
            .expect("paint should draw caret rect");
        assert!(caret_idx > text_idx);
    }

    #[test]
    fn paint_handles_cjk_emoji_selection_and_ime_preedit_in_one_clip() {
        let mut ti = TextInput::new("ph").with_text("A你😊B");
        ti.focused = true;
        ti.edit.cursor = TextInputCaretPosition::at_logical_boundary(2);
        ti.edit.selection_start = Some(TextInputCaretPosition::at_logical_boundary(4));
        ti.composition.set_preedit("拼音".into());
        ti.cursor_visible.set(true);
        ti.last_blink.set(Instant::now());
        ti.layout(Rect::new(0.0, 0.0, 240.0, 30.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            pixels_per_point: 1.0,
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 240.0, 30.0),
        };

        ti.paint(&mut ctx);

        let push_idx = encoder
            .ops
            .iter()
            .position(|op| *op == PaintOp::PushClip)
            .expect("paint should push the text content clip");
        let pop_idx = encoder
            .ops
            .iter()
            .position(|op| *op == PaintOp::PopClip)
            .expect("paint should pop the text content clip");
        let selection_idx = encoder.ops[push_idx..pop_idx]
            .iter()
            .position(|op| *op == PaintOp::Rect)
            .map(|offset| push_idx + offset)
            .expect("paint should draw selected CJK/emoji range before text");
        let text_idx = encoder
            .ops
            .iter()
            .position(|op| *op == PaintOp::Text("A你😊B".into()))
            .expect("paint should draw committed mixed-script text");
        let preedit_idx = encoder
            .ops
            .iter()
            .position(|op| *op == PaintOp::Text("拼音".into()))
            .expect("paint should draw IME preedit text");
        let underline_idx = encoder
            .ops
            .iter()
            .position(|op| *op == PaintOp::Line)
            .expect("paint should underline IME preedit text");
        assert!(selection_idx < text_idx, "ops={:?}", encoder.ops);
        assert!(text_idx < preedit_idx, "ops={:?}", encoder.ops);
        assert!(preedit_idx < underline_idx, "ops={:?}", encoder.ops);

        let clip = encoder.clips[0];
        let selection_rect_ordinal =
            encoder.ops[..=selection_idx].iter().filter(|op| **op == PaintOp::Rect).count() - 1;
        let selection_rect = encoder.rects[selection_rect_ordinal];
        assert!(selection_rect.width > 0.0);
        assert!(selection_rect.height > 0.0);
        assert!(selection_rect.x >= clip.x - 0.1);
        assert!(selection_rect.x + selection_rect.width <= clip.x + clip.width + 0.1);

        let (_, preedit_pos) = encoder
            .texts
            .iter()
            .find(|(text, _)| text == "拼音")
            .expect("preedit text should be recorded");
        assert!(preedit_pos.x >= clip.x - 0.1);
        assert!(preedit_pos.x <= clip.x + clip.width + 0.1);

        let (underline_start, underline_end, underline_width) = encoder.lines[0];
        assert!(underline_width > 0.0);
        assert!(underline_end.x > underline_start.x);
        assert!(underline_start.x >= clip.x - 0.1);
        assert!(underline_end.x <= clip.x + clip.width + 0.1);
    }

    #[test]
    fn paint_clips_text_to_padded_content_rect() {
        let mut ti = TextInput::new("ph").with_text("abcdefghijklmnopqrstuvwxyz");
        ti.layout(Rect::new(10.0, 20.0, 100.0, 28.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            pixels_per_point: 1.0,
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 200.0, 100.0),
        };

        ti.paint(&mut ctx);

        assert_eq!(encoder.clips[0], Rect::new(18.0, 20.0, 84.0, 28.0));
    }

    #[test]
    fn long_text_paint_relies_on_clip_when_scrolled() {
        let mut ti = TextInput::new("ph").with_text("abcdefghijklmnopqrstuvwxyz");
        ti.layout(Rect::new(0.0, 0.0, 80.0, 28.0));
        ti.scroll_x.set(40.0);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            pixels_per_point: 1.0,
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 200.0, 100.0),
        };

        ti.paint(&mut ctx);

        let (_, text_pos) = encoder
            .texts
            .iter()
            .find(|(text, _)| text == "abcdefghijklmnopqrstuvwxyz")
            .expect("paint should emit long text");
        assert!(text_pos.x < 0.0);
        assert_eq!(encoder.clips[0], Rect::new(8.0, 0.0, 64.0, 28.0));
    }

    #[test]
    fn local_single_line_undo_redo_restores_replacement_selection_and_owns_empty_history() {
        let mut input = TextInput::new("Text").with_text("旧值 👩‍💻e\u{301}");
        layout(&mut input);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let undo = UiEvent::KeyDown {
            key: KeyCode::Z,
            modifiers: primary_text_modifiers(),
        };
        let redo = UiEvent::KeyDown {
            key: KeyCode::Z,
            modifiers: Modifiers { shift: true, ..primary_text_modifiers() },
        };
        assert_eq!(
            input.event(&undo, &mut ctx),
            EventResult::Handled,
            "even an empty text history must not undo a selected Clip"
        );
        kd_primary(&mut input, KeyCode::A, &mut ctx);
        tp(&mut input, "replacement", &mut ctx);
        assert_eq!(input.event(&undo, &mut ctx), EventResult::Handled);
        assert_eq!(input.text(), "旧值 👩‍💻e\u{301}");
        assert_eq!(
            input.selection_byte_range(),
            Some((0, input.text().len())),
            "local Undo restores the user's original replacement selection"
        );
        assert_eq!(input.event(&redo, &mut ctx), EventResult::Handled);
        assert_eq!(input.text(), "replacement");
        assert!(!input.has_selection());
        assert_eq!(input.event(&undo, &mut ctx), EventResult::Handled);
        tp(&mut input, "new branch", &mut ctx);
        input.event(&redo, &mut ctx);
        assert_eq!(
            input.text(),
            "new branch",
            "new typing after Undo truncates the abandoned local Redo branch"
        );
    }

    #[test]
    fn local_single_line_word_delete_is_grapheme_safe_selection_first_and_atomic() {
        let mut input = TextInput::new("Text").with_text("alpha\u{2003}👩‍💻e\u{301} omega");
        layout(&mut input);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let undo = UiEvent::KeyDown {
            key: KeyCode::Z,
            modifiers: primary_text_modifiers(),
        };
        let word_delete = |key| UiEvent::KeyDown { key, modifiers: word_text_modifiers() };
        assert_eq!(
            input.event(&word_delete(KeyCode::Backspace), &mut ctx),
            EventResult::Handled
        );
        assert_eq!(input.text(), "alpha\u{2003}👩‍💻e\u{301} ");
        assert_eq!(input.event(&undo, &mut ctx), EventResult::Handled);
        assert_eq!(
            input.text(),
            "alpha\u{2003}👩‍💻e\u{301} omega",
            "one Undo reverses the whole word deletion"
        );
        kd(&mut input, KeyCode::Home, &mut ctx);
        assert_eq!(
            input.event(&word_delete(KeyCode::Delete), &mut ctx),
            EventResult::Handled
        );
        assert_eq!(
            input.text(),
            "👩‍💻e\u{301} omega",
            "Unicode whitespace and composite graphemes remain intact"
        );
        input.event(&undo, &mut ctx);
        kd(&mut input, KeyCode::Home, &mut ctx);
        assert_eq!(
            input.event(&word_delete(KeyCode::Backspace), &mut ctx),
            EventResult::Handled
        );
        assert_eq!(
            input.text(),
            "alpha\u{2003}👩‍💻e\u{301} omega",
            "empty boundary is a consumed no-op"
        );
        kd(&mut input, KeyCode::End, &mut ctx);
        kd_shift(&mut input, KeyCode::Left, &mut ctx);
        assert_eq!(
            input.event(&word_delete(KeyCode::Backspace), &mut ctx),
            EventResult::Handled
        );
        assert_eq!(
            input.text(),
            "alpha\u{2003}👩‍💻e\u{301} omeg",
            "a selected grapheme wins over the word boundary"
        );
        input.event(&undo, &mut ctx);
        assert_eq!(
            input.selection_byte_range().map(|(a, b)| &input.text()[a..b]),
            Some("a")
        );
    }

    #[test]
    fn local_single_line_external_model_replacement_clears_undo_and_redo() {
        let mut input = TextInput::new("Text").with_text("original");
        layout(&mut input);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        tp(&mut input, " draft", &mut ctx);
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        input.set_text("fresh canonical model".to_owned());
        for modifiers in [
            primary_text_modifiers(),
            Modifiers { shift: true, ..primary_text_modifiers() },
        ] {
            assert_eq!(
                input.event(&UiEvent::KeyDown { key: KeyCode::Z, modifiers }, &mut ctx),
                EventResult::Handled
            );
            assert_eq!(input.text(), "fresh canonical model");
        }
    }

    #[test]
    fn local_single_line_empty_replacement_selection_is_cleared_before_typing() {
        let mut input = TextInput::new("Text");
        layout(&mut input);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd_primary(&mut input, KeyCode::A, &mut ctx);
        tp(&mut input, "e\u{301}👩‍💻", &mut ctx);
        assert!(
            !input.has_selection(),
            "typing into an empty replacement selection must not select the newly inserted text"
        );
        kd(&mut input, KeyCode::Backspace, &mut ctx);
        assert_eq!(
            input.text(),
            "e\u{301}",
            "ordinary Backspace deletes only the last composite grapheme"
        );
        kd(&mut input, KeyCode::Home, &mut ctx);
        kd_word(&mut input, KeyCode::Backspace, &mut ctx);
        tp(&mut input, "x", &mut ctx);
        assert_eq!(input.text(), "xe\u{301}");
        assert!(!input.has_selection(), "a word-delete no-op must not create a zero-length selection that expands during later typing");
    }

    #[test]
    fn complete_rtl_line_end_caret_is_at_visible_left_edge() {
        let mut input = TextInput::new("Text").with_text("שלום");
        input.layout(Rect::new(0.0, 0.0, 240.0, 28.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        assert_eq!(
            input.event(&UiEvent::focus_gained_keyboard(), &mut ctx),
            EventResult::Handled
        );
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        input.paint(&mut PaintContext {
            pixels_per_point: 1.0,
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 300.0, 80.0),
        });
        let caret = encoder.rects.last().expect("focused editor must paint its caret");
        assert!(
            (caret.x - encoder.clips[0].x).abs() <= 0.1,
            "logical end of a complete RTL line is its left edge: {caret:?}"
        );
        assert_eq!(input.text(), "שלום");
    }

    #[test]
    fn clicking_visible_left_of_rtl_line_inserts_at_logical_end() {
        let mut input = TextInput::new("Text").with_text("שלום");
        input.layout(Rect::new(0.0, 0.0, 240.0, 28.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        input.event(
            &UiEvent::MouseDown {
                position: Point::new(8.1, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        input.event(
            &UiEvent::MouseUp {
                position: Point::new(8.1, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput("!".into()), &mut ctx);
        assert_eq!(
            input.text(),
            "שלום!",
            "pixel hit must preserve complete RTL logical coordinates"
        );
        assert!(!input.has_selection());
    }

    // Pixel reference: the public complete advanced TextLayout, including its
    // actual font fallback and visual glyph coordinates. This is deliberately
    // independent of TextInput's prefix/substring measurements and new APIs.
    fn complete_selection_reference(
        text: &str,
        font_size: f32,
        glyph_byte_start: usize,
        from_x: f32,
    ) -> (f32, f32, Vec<(f32, f32)>) {
        let attrs_owner = mondrian_ui_text::font::FontManager::new();
        let mut font_owner = mondrian_ui_text::font::FontManager::new();
        let layout = mondrian_ui_text::layout::TextLayout::new_single_line(
            &mut font_owner.font_system,
            text,
            attrs_owner.default_attrs(),
            font_size,
        );
        let runs = layout.runs();
        assert_eq!(
            runs.len(),
            1,
            "the public fixture must be a single complete line"
        );
        let run = &runs[0];
        let glyph = run
            .glyphs
            .iter()
            .find(|glyph| glyph.start == glyph_byte_start)
            .expect("reference must have the requested complete shaped glyph");
        assert!(
            glyph.level.is_rtl() && glyph.w > 0.0,
            "pixel fixture requires an actual positive RTL glyph advance"
        );
        let left_hit_x = glyph.x + glyph.w * 0.1;
        let right_hit_x = glyph.x + glyph.w * 0.9;
        let start = layout
            .buffer()
            .hit(
                if from_x == f32::MAX {
                    right_hit_x
                } else {
                    from_x
                },
                layout.line_height() * 0.5,
            )
            .expect("reference start hit");
        let end = layout
            .buffer()
            .hit(left_hit_x, layout.line_height() * 0.5)
            .expect("reference end hit");
        let spans = run.highlight(start.min(end), start.max(end)).collect();
        (left_hit_x, right_hit_x, spans)
    }

    fn paint_complete_selection(input: &TextInput) -> RecordingEncoder {
        input.cursor_visible.set(true);
        input.last_blink.set(Instant::now());
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        input.paint(&mut PaintContext {
            pixels_per_point: 1.0,
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 400.0, 100.0),
        });
        encoder
    }

    fn assert_complete_selection_pixels(input: &TextInput, spans: &[(f32, f32)]) {
        let encoder = paint_complete_selection(input);
        let text_origin = encoder
            .texts
            .iter()
            .find(|(text, _)| text == input.text())
            .expect("the complete committed text must still be painted")
            .1;
        // Two background rectangles precede the clip; a focused caret follows
        // text. Every intervening rectangle is an actual selection span.
        assert_eq!(
            encoder.rects.len(),
            spans.len() + 3,
            "selection must paint exactly its complete shaped visual spans"
        );
        for (actual, (x, width)) in encoder.rects[2..encoder.rects.len() - 1].iter().zip(spans) {
            assert!(
                (actual.x - (text_origin.x + x)).abs() <= 0.1,
                "actual selection starts at {actual:?}; complete shaped span starts at {x}"
            );
            assert!(
                (actual.width - width).abs() <= 0.1,
                "actual selection width {actual:?}; complete shaped width {width}"
            );
        }
        assert_eq!(encoder.clips.len(), 1);
        assert_eq!(
            encoder.ops.iter().filter(|op| **op == PaintOp::PopClip).count(),
            1
        );
    }

    #[test]
    fn complete_selection_partial_rtl_highlight_matches_full_shaped_pixels() {
        let text = "שלום";
        let metrics = TextInputMetrics::current();
        let (left_x, _, expected) =
            complete_selection_reference(text, metrics.font_size, 0, f32::MAX);
        assert_eq!(expected.len(), 1);
        assert!(
            expected[0].0 > 0.0,
            "first logical RTL character occupies the visual right"
        );
        let mut input = TextInput::new("Text").with_text(text);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::Home, &mut ctx);
        md_shift(&mut input, metrics.padding_x + left_x, 14.0, &mut ctx);
        mu(&mut input, &mut ctx);
        assert_eq!(input.selection_byte_range(), Some((0, "ש".len())));
        assert_complete_selection_pixels(&input, &expected);
        assert_eq!(input.text(), text);
    }

    #[test]
    fn complete_selection_mixed_bidi_paints_disjoint_full_shaped_pixels() {
        let text = "abc אבג xyz";
        let metrics = TextInputMetrics::current();
        let (left_x, _, expected) =
            complete_selection_reference(text, metrics.font_size, "abc ".len(), -100.0);
        assert_eq!(expected.len(), 2,
            "selected LTR prefix and first logical RTL glyph are separated by unselected visual ink");
        let mut input = TextInput::new("Text").with_text(text);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::Home, &mut ctx);
        md_shift(&mut input, metrics.padding_x + left_x, 14.0, &mut ctx);
        mu(&mut input, &mut ctx);
        assert_eq!(input.selection_byte_range(), Some((0, "abc א".len())));
        assert_complete_selection_pixels(&input, &expected);
    }

    #[test]
    fn complete_selection_rtl_preedit_uses_complete_caret_pixel_origin() {
        let text = "שלום";
        let mut input = TextInput::new("Text").with_text(text);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::End, &mut ctx);
        input.event(&UiEvent::ImePreedit("ni".into()), &mut ctx);
        let encoder = paint_complete_selection(&input);
        let committed_origin =
            encoder.texts.iter().find(|(s, _)| s == text).expect("complete text").1;
        let preedit_origin = encoder.texts.iter().find(|(s, _)| s == "ni").expect("preedit").1;
        assert!(
            (preedit_origin.x - committed_origin.x).abs() <= 0.1,
            "logical RTL end composition belongs at the same complete shaped left edge"
        );
        assert!(input.undo_stack.entries.is_empty());
        assert_eq!(input.text(), text);
    }

    #[test]
    fn complete_selection_reverse_pointer_selection_is_not_an_edit_and_undo_restores_it() {
        let text = "שלום";
        let metrics = TextInputMetrics::current();
        let (left_x, right_x, _) =
            complete_selection_reference(text, metrics.font_size, 0, f32::MAX);
        let mut input = TextInput::new("Text").with_text(text).on_change(change_action);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        md(&mut input, metrics.padding_x + left_x, 14.0, &mut ctx);
        mu(&mut input, &mut ctx);
        md_shift(&mut input, metrics.padding_x + right_x, 14.0, &mut ctx);
        mu(&mut input, &mut ctx);
        assert_eq!(input.edit.cursor.grapheme_index, 0);
        assert_eq!(
            input.edit.selection_start.map(|caret| caret.grapheme_index),
            Some(1)
        );
        assert!(
            actions.borrow().is_empty(),
            "navigation/selection dispatches no text edit callback"
        );
        assert!(
            input.undo_stack.entries.is_empty(),
            "selection creates no text Undo entry"
        );
        tp(&mut input, "X", &mut ctx);
        assert_eq!(input.text(), "Xלום");
        assert_eq!(actions.borrow().len(), 1);
        assert_eq!(input.undo_stack.entries.len(), 1);
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), text);
        assert_eq!(input.edit.cursor.grapheme_index, 0);
        assert_eq!(
            input.edit.selection_start.map(|caret| caret.grapheme_index),
            Some(1),
            "Undo restores the actual reverse anchor"
        );
        assert_eq!(
            actions.borrow().len(),
            2,
            "live TextInput Undo retains the existing on_change contract"
        );
    }

    #[test]
    fn visual_single_line_rtl_right_moves_from_logical_end_without_author_edit() {
        let text = "שלום";
        let mut input = TextInput::new("Text").with_text(text).on_change(change_action);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::End, &mut ctx);
        let before = paint_complete_selection(&input).rects.last().copied().expect("caret before");
        kd(&mut input, KeyCode::Right, &mut ctx);
        let after = paint_complete_selection(&input).rects.last().copied().expect("caret after");
        assert!(
            after.x > before.x,
            "Right must move visually right in pure RTL: {before:?} -> {after:?}"
        );
        assert!(actions.borrow().is_empty());
        assert!(input.undo_stack.entries.is_empty());
        tp(&mut input, "!", &mut ctx);
        assert_eq!(input.text(), "שלו!ם");
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), text);
    }

    #[test]
    fn visual_single_line_mixed_bidi_right_follows_selected_glyph_run_direction() {
        let text = "abc אבג xyz";
        let metrics = TextInputMetrics::current();
        let (_, right_x, _) =
            complete_selection_reference(text, metrics.font_size, "abc א".len(), -100.0);
        let mut input = TextInput::new("Text").with_text(text).on_change(change_action);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        md(&mut input, metrics.padding_x + right_x, 14.0, &mut ctx);
        mu(&mut input, &mut ctx);
        assert_eq!(
            input.edit.cursor.grapheme_index, 5,
            "pointer is before the logical Bet glyph in an LTR paragraph"
        );
        let before = paint_complete_selection(&input).rects.last().copied().expect("caret before");
        kd(&mut input, KeyCode::Right, &mut ctx);
        let after = paint_complete_selection(&input).rects.last().copied().expect("caret after");
        assert!(
            after.x > before.x,
            "Right must follow visual RTL glyph run even in LTR paragraph: {before:?} -> {after:?}"
        );
        assert!(actions.borrow().is_empty());
        assert!(input.undo_stack.entries.is_empty());
        tp(&mut input, "!", &mut ctx);
        assert_eq!(input.text(), "abc !אבג xyz");
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), text);
    }

    #[test]
    fn visual_single_line_pointer_keeps_ltr_side_of_shared_bidi_boundary() {
        let text = "abc אבג xyz";
        let metrics = TextInputMetrics::current();
        let attrs_owner = mondrian_ui_text::font::FontManager::new();
        let mut font_owner = mondrian_ui_text::font::FontManager::new();
        let reference = mondrian_ui_text::layout::TextLayout::new_single_line(
            &mut font_owner.font_system,
            text,
            attrs_owner.default_attrs(),
            metrics.font_size,
        );
        let glyphs = reference.glyphs();
        let space = glyphs
            .iter()
            .find(|glyph| glyph.start == "abc".len())
            .expect("actual complete LTR whitespace advance before embedded RTL run");
        assert!(!space.level.is_rtl() && space.w > 0.0);
        let hit_x = space.x + space.w * 0.9;
        let expected_caret_x = space.x + space.w;
        let mut input = TextInput::new("Text").with_text(text).on_change(change_action);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        md(&mut input, metrics.padding_x + hit_x, 14.0, &mut ctx);
        mu(&mut input, &mut ctx);
        assert_eq!(
            input.edit.cursor.grapheme_index, 4,
            "logical boundary immediately after LTR whitespace"
        );
        let painted = paint_complete_selection(&input);
        let caret = painted.rects.last().expect("actual widget caret");
        let text_origin = painted.texts.iter().find(|(s, _)| s == text).expect("complete text").1;
        assert!((caret.x - (text_origin.x + expected_caret_x)).abs() <= 0.1,
            "pointer caret must stay associated with hit LTR space, not jump across RTL word: {caret:?}");
        assert!(actions.borrow().is_empty());
        assert!(input.undo_stack.entries.is_empty());
        tp(&mut input, "!", &mut ctx);
        assert_eq!(input.text(), "abc !אבג xyz");
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), text);
    }

    #[test]
    fn visual_single_line_rtl_shift_right_selects_one_grapheme_and_undo_restores_direction() {
        let text = "שלום";
        let metrics = TextInputMetrics::current();
        let (_, _, expected) =
            complete_selection_reference(text, metrics.font_size, "שלו".len(), f32::MAX);
        let mut input = TextInput::new("Text").with_text(text).on_change(change_action);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::End, &mut ctx);
        kd_shift(&mut input, KeyCode::Right, &mut ctx);
        assert_eq!(
            input.selection_byte_range(),
            Some(("שלו".len(), text.len()))
        );
        assert!(actions.borrow().is_empty());
        assert!(input.undo_stack.entries.is_empty());
        assert_complete_selection_pixels(&input, &expected);
        tp(&mut input, "X", &mut ctx);
        assert_eq!(input.text(), "שלוX");
        assert_eq!(actions.borrow().len(), 1);
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), text);
        assert_eq!(
            input.selection_byte_range(),
            Some(("שלו".len(), text.len()))
        );
        assert_complete_selection_pixels(&input, &expected);
        assert_eq!(actions.borrow().len(), 2);
    }

    #[test]
    fn visual_single_line_rtl_home_end_and_plain_arrow_collapse_use_visual_selection_edge() {
        let text = "שלום";
        let mut input = TextInput::new("Text").with_text(text);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::End, &mut ctx);
        let end = paint_complete_selection(&input)
            .rects
            .last()
            .copied()
            .expect("logical End caret");
        kd_shift(&mut input, KeyCode::Home, &mut ctx);
        assert_eq!(input.selection_byte_range(), Some((0, text.len())));
        let home = paint_complete_selection(&input)
            .rects
            .last()
            .copied()
            .expect("logical Home caret");
        assert!(
            home.x > end.x,
            "RTL Home is logical start at right; End is logical end at left"
        );
        kd(&mut input, KeyCode::Left, &mut ctx);
        assert!(!input.has_selection());
        let collapsed =
            paint_complete_selection(&input).rects.last().copied().expect("collapsed caret");
        assert!(
            (collapsed.x - end.x).abs() <= 0.1,
            "plain Left collapses to selected visual left edge without another character move"
        );
        tp(&mut input, "!", &mut ctx);
        assert_eq!(input.text(), "שלום!");
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), text);
        assert!(
            !input.has_selection(),
            "navigation was complete before the one reversible insertion"
        );
    }

    #[test]
    fn visual_single_line_ltr_right_collapses_selection_without_skipping_a_character() {
        let mut input = TextInput::new("Text").with_text("abcdef");
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::Home, &mut ctx);
        for _ in 0..3 {
            kd_shift(&mut input, KeyCode::Right, &mut ctx);
        }
        assert_eq!(input.selection_byte_range(), Some((0, 3)));
        kd(&mut input, KeyCode::Right, &mut ctx);
        assert!(!input.has_selection());
        assert!(input.undo_stack.entries.is_empty());
        tp(&mut input, "!", &mut ctx);
        assert_eq!(
            input.text(),
            "abc!def",
            "plain Right collapses at the selected right edge"
        );
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), "abcdef");
    }

    #[test]
    fn visual_single_line_arabic_ligature_shift_navigation_preserves_logical_grapheme() {
        let text = "لا";
        let mut input = TextInput::new("Text").with_text(text);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::End, &mut ctx);
        kd_shift(&mut input, KeyCode::Right, &mut ctx);
        assert_eq!(
            input.selection_byte_range(),
            Some(("ل".len(), text.len())),
            "one visual step into the complete Arabic cluster selects only Alef"
        );
        tp(&mut input, "X", &mut ctx);
        assert_eq!(input.text(), "لX");
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), text);
        assert_eq!(input.selection_byte_range(), Some(("ل".len(), text.len())));
        // The existing full grapheme contract also applies to combining marks
        // and emoji; navigation must never expose a UTF-8 or ZWJ interior.
        input.set_text("e\u{301}👩‍💻".to_owned());
        kd_shift(&mut input, KeyCode::Left, &mut ctx);
        assert_eq!(
            input.selection_byte_range(),
            Some(("e\u{301}".len(), input.text().len()))
        );
        tp(&mut input, "X", &mut ctx);
        assert_eq!(input.text(), "e\u{301}X");
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), "e\u{301}👩‍💻");
    }

    #[test]
    fn visual_single_line_zero_advance_boundary_is_owned_and_not_skipped() {
        let text = "a\u{200b}ב";
        let mut input = TextInput::new("Text").with_text(text);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcut, &mut tooltip);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::Home, &mut ctx);
        kd(&mut input, KeyCode::Right, &mut ctx);
        let after_a = paint_complete_selection(&input).rects.last().copied().expect("after a");
        kd(&mut input, KeyCode::Right, &mut ctx);
        let after_control = paint_complete_selection(&input)
            .rects
            .last()
            .copied()
            .expect("after zero-width space");
        assert!(
            (after_a.x - after_control.x).abs() <= 0.1,
            "a zero-advance grapheme is a finite logical caret step at the same pixel"
        );
        kd(&mut input, KeyCode::Right, &mut ctx);
        let after_rtl_cell = paint_complete_selection(&input)
            .rects
            .last()
            .copied()
            .expect("RTL visual cell right edge");
        assert!(
            after_rtl_cell.x > after_control.x,
            "the next visual step traverses the actual RTL glyph, not a logical skip fallback"
        );
        assert!(input.undo_stack.entries.is_empty());
        tp(&mut input, "!", &mut ctx);
        assert_eq!(input.text(), "a\u{200b}!ב");
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), text);
    }

    #[test]
    fn visual_single_line_sequential_egc_input_has_complete_result_endpoint_and_reversible_text() {
        for (fragments, complete) in [
            (vec!["e", "\u{301}"], "e\u{301}"),
            (vec!["👩", "\u{200d}", "💻"], "👩‍💻"),
        ] {
            let mut input = TextInput::new("Text").on_change(change_action);
            input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
            let actions = RefCell::new(Vec::new());
            let dispatch = |action: Action| actions.borrow_mut().push(action);
            let mut focus = DummyFocus;
            let mut shortcut = DummyShortcut;
            let mut tooltip = DummyTooltip;
            let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
            input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
            for fragment in &fragments {
                tp(&mut input, fragment, &mut ctx);
            }
            assert_eq!(input.text(), complete);
            assert_eq!(
                actions.borrow().len(),
                fragments.len(),
                "one callback per actual input mutation"
            );
            kd(&mut input, KeyCode::Left, &mut ctx);
            assert_eq!(
                actions.borrow().len(),
                fragments.len(),
                "navigation is not an edit"
            );
            tp(&mut input, "X", &mut ctx);
            assert_eq!(
                input.text(),
                format!("X{complete}"),
                "one Left from the complete joined EGC must move before it"
            );
            kd_primary(&mut input, KeyCode::Z, &mut ctx);
            assert_eq!(input.text(), complete);
            let redo = UiEvent::KeyDown {
                key: KeyCode::Z,
                modifiers: Modifiers { shift: true, ..primary_text_modifiers() },
            };
            input.event(&redo, &mut ctx);
            assert_eq!(input.text(), format!("X{complete}"));
            kd_primary(&mut input, KeyCode::Z, &mut ctx);
            kd(&mut input, KeyCode::Delete, &mut ctx);
            assert_eq!(
                input.text(),
                "",
                "Undo restores a caret before the complete joined EGC"
            );
        }
    }

    #[test]
    fn visual_single_line_zwj_inserted_between_existing_emoji_snaps_forward_in_complete_result() {
        let mut input = TextInput::new("Text").with_text("👩💻").on_change(change_action);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::End, &mut ctx);
        kd(&mut input, KeyCode::Left, &mut ctx);
        tp(&mut input, "\u{200d}", &mut ctx);
        assert_eq!(input.text(), "👩‍💻");
        assert_eq!(actions.borrow().len(), 1);
        kd(&mut input, KeyCode::Left, &mut ctx);
        tp(&mut input, "X", &mut ctx);
        assert_eq!(
            input.text(),
            "X👩‍💻",
            "insertion byte endpoint inside the joined complete EGC must snap forward to its end"
        );
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), "👩‍💻");
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), "👩💻");
        let redo = UiEvent::KeyDown {
            key: KeyCode::Z,
            modifiers: Modifiers { shift: true, ..primary_text_modifiers() },
        };
        input.event(&redo, &mut ctx);
        assert_eq!(input.text(), "👩‍💻");
        tp(&mut input, "Y", &mut ctx);
        assert_eq!(input.text(), "Y👩‍💻",
            "Redo restores the actual before-Undo navigation endpoint, not an invalid fragment count");
    }

    #[test]
    fn visual_single_line_rtl_word_left_uses_visual_word_boundary_and_keeps_edit_ownership() {
        let text = "שלום עולם";
        let mut input = TextInput::new("Text").with_text(text).on_change(change_action);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::Home, &mut ctx);
        let before = paint_complete_selection(&input).rects.last().copied().expect("RTL Home");
        kd_word(&mut input, KeyCode::Left, &mut ctx);
        let after = paint_complete_selection(&input)
            .rects
            .last()
            .copied()
            .expect("visual word boundary");
        assert!(
            after.x < before.x,
            "physical word Left must advance toward RTL first word end"
        );
        assert!(actions.borrow().is_empty());
        assert!(input.undo_stack.entries.is_empty());
        tp(&mut input, "X", &mut ctx);
        assert_eq!(input.text(), "שלוםX עולם");
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), text);
    }

    // The numeric wrapper exposes exactly the same public Widget paint/event
    // and inspection seams as the shell. A genuine repeated focus event pins
    // the caret blink without reaching through NumberInput's private fields.
    fn paint_visual_number(
        input: &mut crate::NumberInput,
        ctx: &mut EventContext,
    ) -> RecordingEncoder {
        input.event(&UiEvent::focus_gained_keyboard(), ctx);
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        input.paint(&mut PaintContext {
            pixels_per_point: 1.0,
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 400.0, 100.0),
        });
        encoder
    }

    #[test]
    fn visual_single_line_numeric_inspection_preserves_bidi_association_and_local_undo_with_fresh_callback(
    ) {
        let text = "abc אבג xyz";
        let metrics = TextInputMetrics::current();
        let attrs_owner = mondrian_ui_text::font::FontManager::new();
        let mut font_owner = mondrian_ui_text::font::FontManager::new();
        let shaped = mondrian_ui_text::layout::TextLayout::new_single_line(
            &mut font_owner.font_system,
            text,
            attrs_owner.default_attrs(),
            metrics.font_size,
        );
        let runs = shaped.runs();
        let space = runs[0]
            .glyphs
            .iter()
            .find(|glyph| glyph.start == 3 && glyph.end == 4)
            .expect("complete LTR space before the Hebrew run");
        assert!(!space.level.is_rtl() && space.w > 0.0);
        let expected_x = space.x + space.w;
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let key = |key, modifiers| UiEvent::KeyDown { key, modifiers };
        let primary = primary_text_modifiers();
        let redo = Modifiers { shift: true, ..primary };
        let mut original =
            crate::NumberInput::new(12.0_f64, 0.0, 100.0).on_change(|_| Action::Play);
        original.set_control_inspection_key(Some("complete-numeric-caret".to_owned()));
        original.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        original.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        original.event(&key(KeyCode::A, primary), &mut ctx);
        original.event(&UiEvent::TextInput(text.to_owned()), &mut ctx);
        original.event(&key(KeyCode::Home, Modifiers::none()), &mut ctx);
        let before = paint_visual_number(&mut original, &mut ctx);
        let origin_x =
            before.texts.iter().find(|(value, _)| value == text).expect("full draft").1.x;
        let point = Point::new(origin_x + space.x + space.w * 0.9, 14.0);
        for event in [
            UiEvent::MouseDown {
                position: point,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            UiEvent::MouseUp {
                position: point,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
        ] {
            original.event(&event, &mut ctx);
        }
        let retained = original
            .control_inspection_state()
            .expect("completed pointer allows an admitted numeric keyboard draft");
        let mut fresh = crate::NumberInput::new(12.0_f64, 0.0, 100.0).on_change(|_| Action::Pause);
        fresh.set_control_inspection_key(Some("complete-numeric-caret".to_owned()));
        fresh.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        assert!(fresh.restore_control_inspection_state(&retained));
        let painted = paint_visual_number(&mut fresh, &mut ctx);
        let origin_x = painted
            .texts
            .iter()
            .find(|(value, _)| value == text)
            .expect("fresh full draft")
            .1
            .x;
        let caret = painted.rects.last().expect("fresh associated caret");
        assert!((caret.x - origin_x - expected_x).abs() <= 0.1,
            "same logical byte seam must keep its original LTR cluster association through refresh: {caret:?}");
        fresh.event(&UiEvent::TextInput("Q".to_owned()), &mut ctx);
        assert_eq!(fresh.text(), "abc Qאבג xyz");
        fresh.event(&key(KeyCode::Z, primary), &mut ctx);
        assert_eq!(fresh.text(), text);
        let undone = paint_visual_number(&mut fresh, &mut ctx);
        let origin_x =
            undone.texts.iter().find(|(value, _)| value == text).expect("undone draft").1.x;
        let caret = undone.rects.last().expect("Undo associated caret");
        assert!(
            (caret.x - origin_x - expected_x).abs() <= 0.1,
            "Undo must retain the same visual association, not only the logical offset"
        );
        fresh.event(&key(KeyCode::Z, redo), &mut ctx);
        assert_eq!(fresh.text(), "abc Qאבג xyz");
        assert!(
            actions.borrow().is_empty(),
            "numeric draft and local history are not author commits"
        );
        let mut changed = crate::NumberInput::new(13.0_f64, 0.0, 100.0);
        changed.set_control_inspection_key(Some("complete-numeric-caret".to_owned()));
        let new_id = changed.id();
        assert!(!changed.restore_control_inspection_state(&retained));
        assert_eq!(changed.id(), new_id);
        assert_eq!(changed.text(), "13");
        let mut rebound = crate::NumberInput::new(12.0_f64, 0.0, 100.0);
        rebound.set_control_inspection_key(Some("another-numeric-owner".to_owned()));
        assert!(!rebound.restore_control_inspection_state(&retained));
        fresh.event(&key(KeyCode::A, primary), &mut ctx);
        fresh.event(&UiEvent::TextInput("25".to_owned()), &mut ctx);
        fresh.event(&key(KeyCode::Enter, Modifiers::none()), &mut ctx);
        assert_eq!(fresh.text(), "25");
        assert_eq!(
            &*actions.borrow(),
            &[Action::Pause],
            "one confirmation, using only the fresh callback"
        );
        assert_eq!(
            fresh.event(&key(KeyCode::Z, primary), &mut ctx),
            EventResult::Ignored
        );
        assert_eq!(
            fresh.text(),
            "25",
            "confirmed value releases local Undo to the author boundary"
        );
    }

    #[test]
    fn visual_single_line_numeric_sequential_egc_draft_admits_refresh_and_reversible_complete_endpoint(
    ) {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let key = |key, modifiers| UiEvent::KeyDown { key, modifiers };
        let primary = primary_text_modifiers();
        let mut original =
            crate::NumberInput::new(12.0_f64, 0.0, 100.0).on_change(|_| Action::Play);
        original.set_control_inspection_key(Some("joined-numeric-draft".to_owned()));
        original.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        original.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        original.event(&key(KeyCode::A, primary), &mut ctx);
        for fragment in ["👩", "\u{200d}", "💻"] {
            original.event(&UiEvent::TextInput(fragment.to_owned()), &mut ctx);
        }
        assert_eq!(original.text(), "👩‍💻");
        let retained = original
            .control_inspection_state()
            .expect("complete-result EGC endpoint must admit its owned draft/history");
        let mut fresh = crate::NumberInput::new(12.0_f64, 0.0, 100.0).on_change(|_| Action::Pause);
        fresh.set_control_inspection_key(Some("joined-numeric-draft".to_owned()));
        fresh.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        assert!(fresh.restore_control_inspection_state(&retained));
        fresh.event(&key(KeyCode::Left, Modifiers::none()), &mut ctx);
        fresh.event(&UiEvent::TextInput("X".to_owned()), &mut ctx);
        assert_eq!(fresh.text(), "X👩‍💻");
        fresh.event(&key(KeyCode::Z, primary), &mut ctx);
        assert_eq!(fresh.text(), "👩‍💻");
        fresh.event(&key(KeyCode::Delete, Modifiers::none()), &mut ctx);
        assert_eq!(
            fresh.text(),
            "",
            "Undo restores the complete before-EGC endpoint after draft refresh"
        );
        assert!(actions.borrow().is_empty());
        fresh.event(&UiEvent::TextInput("25".to_owned()), &mut ctx);
        fresh.event(&key(KeyCode::Enter, Modifiers::none()), &mut ctx);
        assert_eq!(&*actions.borrow(), &[Action::Pause]);
    }

    #[test]
    fn associated_single_line_logical_delete_keeps_surviving_bidi_association_and_actual_undo_pixels(
    ) {
        let original = "abc אבג xyz";
        let reduced = "abc בג xyz";
        let metrics = TextInputMetrics::current();
        let reference = |text: &str| {
            let attrs_owner = mondrian_ui_text::font::FontManager::new();
            let mut font_owner = mondrian_ui_text::font::FontManager::new();
            let shaped = mondrian_ui_text::layout::TextLayout::new_single_line(
                &mut font_owner.font_system,
                text,
                attrs_owner.default_attrs(),
                metrics.font_size,
            );
            let glyphs = shaped.glyphs();
            let first = glyphs
                .iter()
                .find(|glyph| glyph.start == 4 && glyph.end == 6)
                .expect("actual first complete RTL glyph after the LTR seam");
            assert!(first.level.is_rtl() && first.w > 0.0);
            (first.x + first.w * 0.9, first.x + first.w)
        };
        let (hit, original_right) = reference(original);
        let (_, reduced_right) = reference(reduced);
        let mut input = TextInput::new("Text").with_text(original).on_change(change_action);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        md(&mut input, metrics.padding_x + hit, 14.0, &mut ctx);
        mu(&mut input, &mut ctx);
        kd(&mut input, KeyCode::Delete, &mut ctx);
        assert_eq!(input.text(), reduced);
        assert_eq!(
            input.edit.cursor.affinity,
            mondrian_ui_core::TextCaretAffinity::BeforeNext
        );
        let painted = paint_complete_selection(&input);
        let origin = painted
            .texts
            .iter()
            .find(|(text, _)| text == reduced)
            .expect("complete reduced draft")
            .1
            .x;
        assert!(
            (painted.rects.last().expect("remaining RTL association").x - origin - reduced_right)
                .abs()
                <= 0.1
        );
        assert_eq!(actions.borrow().len(), 1);
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), original);
        let painted = paint_complete_selection(&input);
        let origin = painted
            .texts
            .iter()
            .find(|(text, _)| text == original)
            .expect("complete undone draft")
            .1
            .x;
        assert!(
            (painted.rects.last().expect("original RTL association").x - origin - original_right)
                .abs()
                <= 0.1
        );
        assert_eq!(actions.borrow().len(), 2);
    }

    #[test]
    fn associated_single_line_deletion_joining_egcs_uses_complete_result_endpoint_and_real_undo() {
        let mut input = TextInput::new("Text").with_text("👩‍X💻").on_change(change_action);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::End, &mut ctx);
        kd(&mut input, KeyCode::Left, &mut ctx);
        kd(&mut input, KeyCode::Backspace, &mut ctx);
        assert_eq!(input.text(), "👩‍💻");
        assert_eq!(actions.borrow().len(), 1);
        assert_eq!(input.edit.cursor.grapheme_index, 1);
        assert!(TextEditState::position_admitted(
            input.edit.cursor,
            input.len_graphemes()
        ));
        kd(&mut input, KeyCode::Left, &mut ctx);
        tp(&mut input, "Q", &mut ctx);
        assert_eq!(input.text(), "Q👩‍💻");
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), "👩‍💻");
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(input.text(), "👩‍X💻");
        kd(&mut input, KeyCode::Delete, &mut ctx);
        assert_eq!(
            input.text(),
            "👩‍X",
            "Undo restores the actual logical caret after X, before Laptop"
        );
    }

    #[test]
    fn source_single_line_constructor_preserves_model_bytes_and_keeps_native_word_navigation() {
        for separator in ["\r", "\n", "\r\n", "\u{2028}", "\u{2029}", "\t", "\u{00A0}"] {
            let expected = format!("left{separator}right");
            let actions = RefCell::new(Vec::new());
            let dispatch = |action: Action| actions.borrow_mut().push(action);
            let mut focus = DummyFocus;
            let mut shortcuts = DummyShortcut;
            let mut tooltip = DummyTooltip;
            let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
            let mut input = TextInput::new("Text")
                .with_text(format!("left{separator}right"))
                .on_commit(change_action);
            layout(&mut input);
            input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
            assert_eq!(
                input.text(),
                expected,
                "programmatic separator {separator:?}"
            );
            kd_word(&mut input, KeyCode::Left, &mut ctx);
            assert_eq!(
                input.edit.cursor.grapheme_index, 5,
                "whole-line navigation must be admitted"
            );
            kd_word(&mut input, KeyCode::Right, &mut ctx);
            assert_eq!(input.edit.cursor.grapheme_index, 10);
            kd(&mut input, KeyCode::Enter, &mut ctx);
            assert!(
                actions.borrow().is_empty(),
                "construction and navigation are not an author edit"
            );
        }
    }

    #[test]
    fn source_single_line_set_text_establishes_exact_focus_cancel_and_history_baseline() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = TextInput::new("Text").with_text("Old").on_commit(change_action);
        layout(&mut input);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        input.select_all();
        tp(&mut input, "old local draft", &mut ctx);
        assert!(input.owns_local_history());
        input.set_text("👩‍💻e\u{301}\r\nשלום\u{2028}X\u{2029}\t\u{00A0}end".to_owned());
        let raw_source = "👩‍💻e\u{301}\r\nשלום\u{2028}X\u{2029}\t\u{00A0}end";
        assert_eq!(input.text(), raw_source);
        assert!(!input.has_selection());
        assert!(
            !input.owns_local_history(),
            "external baseline replaces local history without authoring"
        );
        input.select_all();
        tp(&mut input, "new local draft", &mut ctx);
        kd(&mut input, KeyCode::Escape, &mut ctx);
        assert_eq!(
            input.text(),
            raw_source,
            "Escape restores the exact raw model baseline"
        );
        assert!(!input.is_focused());
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::Enter, &mut ctx);
        input.event(&UiEvent::FocusLost, &mut ctx);
        assert!(
            actions.borrow().is_empty(),
            "model synchronization, Escape and unchanged confirmation never dispatch"
        );
    }

    #[test]
    fn source_single_line_model_ingress_and_user_input_have_distinct_author_contracts() {
        let raw = "A\r\nB\rC\nD\u{2028}E\u{2029}F\t\u{00A0}👩‍💻e\u{301}";
        let expected = "A B C D E F\t\u{00A0}👩‍💻e\u{301}";
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut committed = TextInput::new("Text").on_change(change_action);
        committed.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        tp(&mut committed, raw, &mut ctx);
        assert_eq!(committed.text(), expected);
        assert_eq!(&*actions.borrow(), &[change_action(expected)]);
        let mut programmatic = TextInput::new("Text").with_text(raw).on_change(change_action);
        assert_eq!(programmatic.text(), raw);
        programmatic.set_text(raw.to_owned());
        assert_eq!(programmatic.text(), raw);
        assert_eq!(
            &*actions.borrow(),
            &[change_action(expected)],
            "programmatic ingress has no user callback"
        );
    }

    #[test]
    fn source_single_line_numeric_dto_refuses_raw_model_separators_without_guessing_positions() {
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = mk_ctx(&mut focus, &mut shortcuts, &mut tooltip);
        let mut raw = TextInput::new("Text").with_text("left\r\nright");
        raw.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        assert_eq!(
            raw.text(),
            "left\r\nright",
            "a declared single-line view preserves authoritative source"
        );
        let raw_draft = raw.inspection_draft().expect("raw focused source snapshot");
        let mut raw_refused = TextInput::new("Text").with_text("fresh baseline");
        assert!(!raw_refused.restore_numeric_input_inspection(raw.id(), true, Some(&raw_draft)), "numeric draft admission remains canonical even though a general single-line model can preserve raw separators");
        assert_eq!(raw_refused.text(), "fresh baseline");
        let mut input = TextInput::new("Text").with_text("left right");
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let draft = input.inspection_draft().expect("canonical numeric draft");
        let mut fresh = TextInput::new("Text").with_text("fresh baseline");
        assert!(fresh.restore_numeric_input_inspection(input.id(), true, Some(&draft)));
        assert_eq!(fresh.text(), "left right");
        for invalid in [
            "left\rright",
            "left\nright",
            "left\r\nright",
            "left\u{2028}right",
            "left\u{2029}right",
        ] {
            let mut malformed = draft.clone();
            malformed.text = invalid.to_owned();
            let mut refused = TextInput::new("Text").with_text("fresh baseline");
            let fresh_id = refused.id();
            assert!(
                !refused.restore_numeric_input_inspection(input.id(), true, Some(&malformed)),
                "never normalize an untrusted numeric payload and guess its saved endpoints"
            );
            assert_eq!(refused.text(), "fresh baseline");
            assert_eq!(refused.id(), fresh_id);
            assert!(!refused.is_focused());
        }
    }

    #[test]
    fn source_single_line_paint_uses_complete_projection_without_changing_model_or_pixels() {
        let raw = "e\u{301}\r\n\u{301}👩\n\u{200d}💻 שלום\u{2028}X\u{2029}\t\u{00a0}Z";
        let displayed = "e\u{301} \u{301}👩 \u{200d}💻 שלום X \t\u{00a0}Z";
        let mut input = TextInput::new("Text").with_text(raw).on_commit(change_action);
        let mut reference = TextInput::new("Text").with_text(displayed);
        input.layout(Rect::new(0.0, 0.0, 380.0, 28.0));
        reference.layout(Rect::new(0.0, 0.0, 380.0, 28.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        reference.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        assert_eq!(input.text(), raw, "model source is not a user input event");
        assert_eq!(
            input.accessibility().expect("source value").value,
            Some(AccessibilityValue::Text(raw.to_owned()))
        );
        let actual = paint_complete_selection(&input);
        let expected = paint_complete_selection(&reference);
        assert_eq!(actual.texts.len(), 1);
        assert_eq!(
            actual.texts[0].0, displayed,
            "actual draw content is the complete one-line projection"
        );
        assert_eq!(actual.texts[0].1, expected.texts[0].1);
        let caret = actual.rects.last().expect("source caret");
        let expected_caret = expected.rects.last().expect("reference caret");
        assert!((caret.x - expected_caret.x).abs() < 0.01);
        assert!((caret.y - expected_caret.y).abs() < 0.01);
        assert!(!input.commit_pending_input(&mut ctx));
        assert!(
            actions.borrow().is_empty(),
            "projection/navigation never authors a normalized value"
        );
    }

    #[test]
    fn source_single_line_selection_keeps_crlf_and_combining_source_stops_and_real_undo() {
        let raw = "a\r\n\u{301}b";
        let mut input = TextInput::new("Text").with_text(raw).on_change(change_action);
        input.layout(Rect::new(0.0, 0.0, 300.0, 28.0));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        kd(&mut input, KeyCode::Home, &mut ctx);
        kd(&mut input, KeyCode::Right, &mut ctx);
        kd_shift(&mut input, KeyCode::Right, &mut ctx);
        assert_eq!(
            input.selection_byte_range(),
            Some((1, 3)),
            "CRLF is one original source grapheme, not a replacement author space"
        );
        assert_eq!(&input.text()[1..3], "\r\n");
        assert_eq!(input.edit.cursor.grapheme_index, 2);
        kd_shift(&mut input, KeyCode::Right, &mut ctx);
        assert_eq!(input.selection_byte_range(), Some((1, 5)), "a following source combining grapheme remains reachable although the display joins it to space");
        assert!(
            actions.borrow().is_empty(),
            "visual selection does not edit"
        );
        tp(&mut input, "X", &mut ctx);
        assert_eq!(input.text(), "aXb");
        assert_eq!(&*actions.borrow(), &[change_action("aXb")]);
        kd_primary(&mut input, KeyCode::Z, &mut ctx);
        assert_eq!(
            input.text(),
            raw,
            "local Undo restores exact original bytes"
        );
        assert_eq!(input.selection_byte_range(), Some((1, 5)));
        assert_eq!(
            &*actions.borrow(),
            &[change_action("aXb"), change_action(raw)]
        );
    }
}
