//! Text input widgets: single-line and multiline.
//!
//! ## Single-line editor (`TextInput`)
//!
//! Cursor navigation, backspace/delete, Home/End, mouse drag selection
//! (extendable beyond widget bounds), Shift+Click/Shift+Arrow selection
//! expansion, Ctrl+A/C/X/V clipboard, Ctrl+Left/Right word navigation,
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
use std::cell::{Cell, RefCell};
use std::time::Instant;

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, AccessibilityValue, CursorRequest,
    EventContext, PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_text::TextRenderer;
use mondrian_ui_theme::{current_theme, Theme};

mod commands;
mod composition;
mod edit;
mod geometry;
mod ime;
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
use ime::{classify_ime_key, request_disabled_ime, request_enabled_ime, ImeKeyDisposition};
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

thread_local! {
    static TEXT_METRICS: RefCell<TextRenderer> = RefCell::new(TextRenderer::new());
}

fn measure_text_width(text: &str, font_size: f32) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    TEXT_METRICS.with_borrow_mut(|renderer| renderer.measure_text(text, font_size).0)
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
pub type TextInputChangeAction = dyn Fn(&str) -> Action;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmptyTextCommitPolicy {
    PreserveSelection,
    ReplaceSelection,
}

/// TextInput Widget —— 单行文本输入框
pub struct TextInput {
    id: WidgetId,
    edit: TextEditState,
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
}

impl TextInput {
    pub fn new(placeholder: impl Into<String>) -> Self {
        Self {
            id: WidgetId::new(),
            edit: TextEditState::default(),
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
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.edit = TextEditState::with_text(text.into());
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

    pub fn text(&self) -> &str {
        self.edit.text()
    }

    pub fn set_text(&mut self, text: String) {
        self.edit.set_text(text);
        self.update_scroll(TextInputMetrics::current());
    }

    /// Select the whole committed text.
    pub fn select_all(&mut self) {
        self.edit.select_all();
        self.update_scroll(TextInputMetrics::current());
    }

    /// Dispatch an action whenever user input changes the committed text.
    pub fn on_change(mut self, action: impl Fn(&str) -> Action + 'static) -> Self {
        self.on_change = Some(Box::new(action));
        self
    }

    pub fn clear(&mut self) {
        self.edit.clear();
        self.update_scroll(TextInputMetrics::current());
    }

    // ── Grapheme helpers ──────────────────────────────────────────────────

    /// Byte offset of the grapheme at `g_idx`. Returns `text.len()` if index
    /// is past the end.
    fn grapheme_byte_idx(&self, g_idx: usize) -> usize {
        self.edit.grapheme_byte_idx(g_idx)
    }

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

    fn visible_width(&self) -> f32 {
        self.text_geometry(TextInputMetrics::current()).visible_width
    }

    fn content_left(&self) -> f32 {
        self.text_geometry(TextInputMetrics::current()).content_left
    }

    fn content_right(&self) -> f32 {
        self.text_geometry(TextInputMetrics::current()).content_right
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
        pointer_text_x_from_geometry(
            position,
            self.content_left(),
            self.content_right(),
            self.scroll_x.get(),
            measure_text_width(self.edit.text(), metrics.font_size),
        )
    }

    fn update_scroll(&self, metrics: TextInputMetrics) {
        let text_w = if self.edit.text().is_empty() {
            0.0
        } else {
            measure_text_width(self.edit.text(), metrics.font_size)
        };
        let visible_w = self.visible_width();
        let cursor_x = self.cursor_text_x(metrics);
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
        if let Some(factory) = &self.on_change {
            (ctx.dispatch)(factory(self.edit.text()));
        }
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
            TextInputKeyCommand::SelectAll => {
                self.move_cursor_to(self.len_graphemes());
                self.edit.selection_start = Some(0);
                EventResult::Handled
            }
            TextInputKeyCommand::Copy => {
                if let Some((start, end)) = self.selection_byte_range() {
                    let _ = ctx.platform.clipboard_copy(&self.edit.text()[start..end]);
                    EventResult::Handled
                } else {
                    EventResult::Ignored
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
                    EventResult::Ignored
                }
            }
            TextInputKeyCommand::MoveLeft { word, extend_selection } => {
                let target = if word {
                    self.prev_word_boundary(self.edit.cursor)
                } else {
                    self.edit.cursor.saturating_sub(1)
                };
                self.move_cursor_with_selection(target, extend_selection);
                EventResult::Handled
            }
            TextInputKeyCommand::MoveRight { word, extend_selection } => {
                let target = if word {
                    self.next_word_boundary(self.edit.cursor)
                } else {
                    self.edit.cursor + 1
                };
                self.move_cursor_with_selection(target, extend_selection);
                EventResult::Handled
            }
            TextInputKeyCommand::MoveHome { extend_selection } => {
                self.move_cursor_with_selection(0, extend_selection);
                EventResult::Handled
            }
            TextInputKeyCommand::MoveEnd { extend_selection } => {
                self.move_cursor_with_selection(self.len_graphemes(), extend_selection);
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
                ctx.release_pointer_capture(self.id);
                ctx.request_repaint();
                EventResult::Handled
            }
            // ── Focus ──────────────────────────────────────────────────
            UiEvent::FocusGained => {
                self.focused = true;
                self.cursor_visible.set(true);
                self.last_blink.set(Instant::now());
                self.update_scroll(metrics);
                self.refresh_ime_area(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::FocusLost => {
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

                let before_text = self.edit.text.clone();
                let result = classify_key_command(*key, *modifiers)
                    .map_or(EventResult::Ignored, |command| {
                        self.execute_key_command(command, ctx)
                    });
                if result == EventResult::Handled {
                    self.refresh_ime_area(ctx);
                    if self.edit.text != before_text {
                        self.dispatch_change(ctx);
                    }
                    ctx.request_repaint();
                }
                result
            }
            // ── Text input ─────────────────────────────────────────────
            UiEvent::TextInput(ch) if self.focused => {
                let text_changed =
                    self.commit_text_at_cursor(ch, EmptyTextCommitPolicy::ReplaceSelection);
                self.composition.clear();
                self.refresh_ime_area(ctx);
                if text_changed {
                    self.dispatch_change(ctx);
                }
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::ImeCommit(ch) if self.focused => {
                self.composition.clear();
                let text_changed =
                    self.commit_text_at_cursor(ch, EmptyTextCommitPolicy::ReplaceSelection);
                self.refresh_ime_area(ctx);
                if text_changed {
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

    fn paint(&self, ctx: &mut PaintContext) {
        let metrics = TextInputMetrics::from_theme(ctx.theme);
        let geometry = self.text_geometry(metrics);
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

        paint_text_input(
            ctx,
            TextInputPaintSnapshot {
                bounds: self.bounds,
                geometry,
                enabled: self.enabled,
                focused: self.focused,
                cursor_visible,
                has_selection: self.has_selection(),
                text: self.edit.text(),
                placeholder: &self.placeholder,
                selection_byte_range: self.selection_byte_range(),
                cursor_prefix_byte: self.grapheme_byte_idx(self.edit.cursor),
                preedit: self.composition.is_active().then(|| self.composition.preedit()),
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
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_editor_state::Action;
    use mondrian_platform_core::{ClipboardError, FileFilter, PlatformService};
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
    fn kd_ctrl(ti: &mut TextInput, key: KeyCode, ctx: &mut EventContext) {
        ti.event(&UiEvent::KeyDown { key, modifiers: Modifiers::ctrl() }, ctx);
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

        fn open_file_dialog(&self, _title: &str, _filters: &[FileFilter]) -> Option<Vec<PathBuf>> {
            None
        }

        fn save_file_dialog(
            &self,
            _title: &str,
            _default_name: &str,
            _filters: &[FileFilter],
        ) -> Option<PathBuf> {
            None
        }

        fn open_folder_dialog(&self, _title: &str) -> Option<PathBuf> {
            None
        }

        fn open_url(&self, _url: &str) {}

        fn reveal_in_file_manager(&self, _path: &Path) {}

        fn send_notification(&self, _title: &str, _body: &str) {}
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
        assert_eq!(ti.edit.cursor, 0);
        assert!(!ti.has_selection());
        assert!(!ti.focused);
    }

    #[test]
    fn text_edit_state_deletes_selection_and_collapses_cursor_to_start() {
        let mut edit = TextEditState::with_text("before after".into());
        edit.selection_start = Some(7);
        edit.cursor = 12;

        assert_eq!(edit.selection_byte_range(), Some((7, 12)));
        assert!(edit.delete_selection());
        assert_eq!(edit.text(), "before ");
        assert_eq!(edit.cursor, 7);
        assert!(!edit.has_selection());
    }

    #[test]
    fn text_edit_state_deletes_whole_grapheme_clusters() {
        let mut edit = TextEditState::with_text("a👨‍👩‍👧‍👦b".into());
        edit.cursor = 2;

        assert_eq!(edit.len_graphemes(), 3);
        assert!(edit.delete_grapheme_before());
        assert_eq!(edit.text(), "ab");
        assert_eq!(edit.cursor, 1);
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
    fn with_text() {
        let ti = TextInput::new("ph").with_text("hello");
        assert_eq!(ti.text(), "hello");
        assert_eq!(ti.edit.cursor, 5);
    }

    #[test]
    fn with_text_cjk() {
        let ti = TextInput::new("ph").with_text("你好世界");
        assert_eq!(ti.text(), "你好世界");
        assert_eq!(ti.edit.cursor, 4);
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
        ti.edit.selection_start = Some(0);
        ti.edit.cursor = 2;
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
        assert_eq!(ti.edit.cursor, 2);
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
        assert_eq!(ti.edit.cursor, 2);
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
        ti.edit.selection_start = Some(7);
        ti.edit.cursor = 12;
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

        kd_ctrl(&mut ti, KeyCode::V, &mut ctx);

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
        ti.edit.selection_start = Some(7);
        ti.edit.cursor = 12;
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

        kd_ctrl(&mut ti, KeyCode::V, &mut ctx);

        assert_eq!(ti.text(), "before after");
        assert_eq!(ti.selection_byte_range(), Some((7, 12)));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn paste_failure_preserves_selection_without_dispatching_change() {
        let platform = ClipboardPlatform::paste_failure();
        let mut ti = TextInput::new("ph").with_text("before after").on_change(change_action);
        ti.focused = true;
        ti.edit.selection_start = Some(7);
        ti.edit.cursor = 12;
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

        kd_ctrl(&mut ti, KeyCode::V, &mut ctx);

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
        ti.edit.cursor = 3;
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
            (KeyCode::A, Modifiers::ctrl()),
        ] {
            assert_eq!(
                ti.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Handled
            );
            assert_eq!(ti.text(), "abc");
            assert_eq!(ti.edit.cursor, 3);
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
        ti.edit.cursor = 3;
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
        ti.edit.cursor = 3;
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
        ti.edit.selection_start = Some(7);
        ti.edit.cursor = 12;
        ti.composition.set_preedit("候选".into());
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = ti.event(&UiEvent::ImeCommit("中".into()), &mut ctx);

        assert_eq!(result, EventResult::Handled);
        assert_eq!(ti.text(), "before 中");
        assert_eq!(ti.edit.cursor, 8);
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
        ti.edit.selection_start = Some(1);
        ti.edit.cursor = 2;
        ti.composition.set_preedit("候选".into());
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch);

        let result = ti.event(&UiEvent::ImeCommit(String::new()), &mut ctx);

        assert_eq!(result, EventResult::Handled);
        assert_eq!(ti.text(), "ac");
        assert_eq!(ti.edit.cursor, 1);
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
        ti.edit.selection_start = Some(1);
        ti.edit.cursor = 4;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        tp(&mut ti, "X", &mut ctx);
        assert_eq!(ti.text(), "hXo");
        assert_eq!(ti.edit.cursor, 2);
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
        ti.edit.cursor = 2; // after the emoji
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Backspace, &mut ctx);
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.edit.cursor, 1);
    }

    #[test]
    fn delete_deletes_grapheme() {
        let mut ti = TextInput::new("ph").with_text("a😊b");
        ti.focused = true;
        ti.edit.cursor = 1; // before the emoji
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Delete, &mut ctx);
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.edit.cursor, 1);
    }

    // ── Backspace / Delete ───────────────────────────────────────────────

    #[test]
    fn backspace_at_start_noop() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.edit.cursor = 0;
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
        ti.edit.selection_start = Some(1);
        ti.edit.cursor = 4;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Backspace, &mut ctx);
        assert_eq!(ti.text(), "ho");
        assert_eq!(ti.edit.cursor, 1);
    }

    #[test]
    fn delete_at_end_noop() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.edit.cursor = 3;
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
        ti.edit.selection_start = Some(0);
        ti.edit.cursor = 5;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Delete, &mut ctx);
        assert_eq!(ti.text(), "");
        assert_eq!(ti.edit.cursor, 0);
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
        assert_eq!(ti.edit.cursor, 2);
        kd(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor, 3);
    }

    #[test]
    fn home_end() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.edit.cursor = 1;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Home, &mut ctx);
        assert_eq!(ti.edit.cursor, 0);
        kd(&mut ti, KeyCode::End, &mut ctx);
        assert_eq!(ti.edit.cursor, 3);
    }

    // ── Word navigation ──────────────────────────────────────────────────

    #[test]
    fn ctrl_left_word() {
        let mut ti = TextInput::new("ph").with_text("hello world foo");
        ti.focused = true;
        // "hello world foo" = 15 graphemes: hello(5) + space(1) + world(5) + space(1) + foo(3)
        ti.edit.cursor = 15; // end
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd_ctrl(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor, 12); // start of "foo"
        kd_ctrl(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor, 6); // start of "world"
        kd_ctrl(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor, 0); // start of "hello"
    }

    #[test]
    fn ctrl_right_word() {
        let mut ti = TextInput::new("ph").with_text("hello world");
        ti.focused = true;
        // "hello world" = 11 graphemes: hello(5) + space(1) + world(5)
        ti.edit.cursor = 0;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd_ctrl(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor, 6); // past "hello" + space, at "world" start
        kd_ctrl(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor, 11); // past "world", at end
    }

    #[test]
    fn ctrl_shift_word_navigation_extends_selection() {
        let mut ti = TextInput::new("ph").with_text("hello world");
        ti.focused = true;
        ti.edit.cursor = 11;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        kd_mod(
            &mut ti,
            KeyCode::Left,
            Modifiers { ctrl: true, shift: true, ..Default::default() },
            &mut ctx,
        );

        assert_eq!(ti.edit.cursor, 6);
        assert_eq!(ti.edit.selection_start, Some(11));
        assert_eq!(ti.selection_byte_range(), Some((6, 11)));
    }

    #[test]
    fn ctrl_word_navigation_uses_unicode_whitespace_boundaries() {
        let mut ti = TextInput::new("ph").with_text("alpha\tbeta\n\u{00A0}gamma");
        ti.focused = true;
        ti.edit.cursor = ti.len_graphemes();
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        kd_ctrl(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor, 12);
        kd_ctrl(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor, 6);
        kd_ctrl(&mut ti, KeyCode::Home, &mut ctx);
        kd_ctrl(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor, 6);
        kd_ctrl(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor, 12);
    }

    // ── Shift selection ──────────────────────────────────────────────────

    #[test]
    fn shift_left_selects() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.edit.cursor = 3;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd_shift(&mut ti, KeyCode::Left, &mut ctx);
        assert!(ti.has_selection());
        assert_eq!(ti.edit.selection_start, Some(3));
    }

    #[test]
    fn shift_navigation_at_boundaries_preserves_empty_anchor() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.edit.cursor = 0;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        kd_shift(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.edit.cursor, 0);
        assert_eq!(ti.edit.selection_start, Some(0));
        assert!(!ti.has_selection());

        ti.edit.cursor = ti.len_graphemes();
        ti.clear_selection();
        kd_shift(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.edit.cursor, 3);
        assert_eq!(ti.edit.selection_start, Some(3));
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
        assert_eq!(ti.edit.cursor, 0);
        // Shift+click near end
        md_shift(&mut ti, 150.0, 14.0, &mut ctx);
        assert!(ti.has_selection());
        assert_eq!(ti.edit.selection_start, Some(0));
        assert!(ti.edit.cursor > 0);
    }

    // ── Selection byte range ─────────────────────────────────────────────

    #[test]
    fn selection_range_forward() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.edit.selection_start = Some(1);
        ti.edit.cursor = 4;
        assert_eq!(ti.selection_byte_range(), Some((1, 4)));
    }

    #[test]
    fn selection_range_reverse() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.edit.selection_start = Some(4);
        ti.edit.cursor = 1;
        assert_eq!(ti.selection_byte_range(), Some((1, 4)));
    }

    // ── Ctrl shortcuts ───────────────────────────────────────────────────

    #[test]
    fn ctrl_a_select_all() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd_ctrl(&mut ti, KeyCode::A, &mut ctx);
        assert_eq!(ti.edit.cursor, 5);
        assert_eq!(ti.edit.selection_start, Some(0));
    }

    #[test]
    fn ctrl_x_cuts() {
        let platform = ClipboardPlatform::with_text("");
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.focused = true;
        ti.edit.selection_start = Some(1);
        ti.edit.cursor = 4;
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
        kd_ctrl(&mut ti, KeyCode::X, &mut ctx);
        assert_eq!(ti.text(), "ho");
        assert_eq!(ti.edit.cursor, 1);
        assert!(!ti.has_selection());
    }

    #[test]
    fn ctrl_x_preserves_selection_when_clipboard_copy_fails() {
        let platform = ClipboardPlatform::copy_failure();
        let mut ti = TextInput::new("ph").with_text("hello").on_change(change_action);
        ti.focused = true;
        ti.edit.selection_start = Some(1);
        ti.edit.cursor = 4;
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
            &UiEvent::KeyDown { key: KeyCode::X, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(ti.text(), "hello");
        assert_eq!(ti.selection_byte_range(), Some((1, 4)));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn ctrl_c_without_selection_is_ignored_for_global_copy() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.focused = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);

        let result = ti.event(
            &UiEvent::KeyDown { key: KeyCode::C, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        assert_eq!(result, EventResult::Ignored);
    }

    #[test]
    fn modified_text_editing_chords_are_ignored_for_shortcut_routing() {
        let platform = ClipboardPlatform::with_text("paste");
        let mut ti = TextInput::new("ph").with_text("hello world").on_change(change_action);
        ti.focused = true;
        ti.edit.selection_start = Some(0);
        ti.edit.cursor = 5;
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
            (KeyCode::Left, Modifiers { alt: true, ..Default::default() }),
            (
                KeyCode::Right,
                Modifiers { meta: true, ..Default::default() },
            ),
            (
                KeyCode::Home,
                Modifiers { alt: true, shift: true, ..Default::default() },
            ),
            (
                KeyCode::End,
                Modifiers { meta: true, shift: true, ..Default::default() },
            ),
            (
                KeyCode::Backspace,
                Modifiers { alt: true, ..Default::default() },
            ),
            (
                KeyCode::Delete,
                Modifiers { meta: true, ..Default::default() },
            ),
        ] {
            assert_eq!(
                ti.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert_eq!(ti.text(), "hello world");
            assert_eq!(ti.edit.cursor, 5);
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
        assert_eq!(ti.edit.cursor, ti.len_graphemes());
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
        ti.edit.cursor = 2;
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

        assert!(ti.edit.cursor > 0);
    }

    #[test]
    fn click_clears_selection() {
        let mut ti = TextInput::new("ph").with_text("hello");
        layout(&mut ti);
        ti.edit.selection_start = Some(0);
        ti.edit.cursor = 3;
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
        ti.edit.selection_start = Some(0);
        ti.edit.cursor = 1;
        assert!(ti.delete_selection());
        assert_eq!(ti.text(), "");
        assert_eq!(ti.edit.cursor, 0);
        assert!(!ti.delete_selection());
    }

    #[test]
    fn set_text_clears_selection() {
        let mut ti = TextInput::new("ph").with_text("old");
        ti.edit.selection_start = Some(0);
        ti.edit.cursor = 2;
        ti.set_text("new".into());
        assert_eq!(ti.text(), "new");
        assert_eq!(ti.edit.cursor, 3);
        assert!(!ti.has_selection());
    }

    #[test]
    fn clear_resets_all() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.edit.selection_start = Some(1);
        ti.clear();
        assert!(ti.text().is_empty());
        assert_eq!(ti.edit.cursor, 0);
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
        ti.edit.cursor = 2;
        ti.edit.selection_start = Some(4);
        ti.composition.set_preedit("拼音".into());
        ti.cursor_visible.set(true);
        ti.last_blink.set(Instant::now());
        ti.layout(Rect::new(0.0, 0.0, 240.0, 30.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
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
}
