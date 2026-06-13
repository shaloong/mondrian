//! 文本输入框控件
//!
//! 单行文本编辑。支持：
//! - 光标移动、退格删除、Home/End
//! - 鼠标拖拽选择（可超出控件边界）
//! - Shift+Click / Shift+Arrow 选区扩展
//! - Ctrl+A/C/X/V 剪贴板操作
//! - Ctrl+Left/Right 按词跳转
//! - IME 多字符输入
//! - 基于 grapheme cluster 的光标（正确处理 emoji / 组合字符）
//! - 时间驱动的闪烁光标（500ms 周期）

use std::cell::{Cell, RefCell};
use std::time::Instant;

use unicode_segmentation::UnicodeSegmentation;

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_text::TextRenderer;

const DEFAULT_FONT_SIZE: f32 = 14.0;
const HORIZONTAL_PADDING: f32 = 8.0;
const VERTICAL_PADDING: f32 = 4.0;

thread_local! {
    static TEXT_METRICS: RefCell<TextRenderer> = RefCell::new(TextRenderer::new());
}

fn measure_text_width(text: &str, font_size: f32) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    TEXT_METRICS.with_borrow_mut(|renderer| renderer.measure_text(text, font_size).0)
}

/// Adapter that maps the current input value to an editor [`Action`].
pub type TextInputChangeAction = dyn Fn(&str) -> Action;

/// TextInput Widget —— 单行文本输入框
pub struct TextInput {
    id: WidgetId,
    text: String,
    placeholder: String,
    bounds: Rect,
    enabled: bool,
    /// Cursor position as grapheme cluster index.
    cursor: usize,
    focused: bool,
    /// Selection anchor as grapheme cluster index.
    selection_start: Option<usize>,
    /// Whether the mouse is pressed on this widget.
    mouse_down: bool,
    /// Blink: cursor visibility and last toggle time.
    cursor_visible: Cell<bool>,
    last_blink: Cell<Instant>,
    /// Horizontal scroll offset to keep cursor visible.
    scroll_x: Cell<f32>,
    /// IME composition text shown before the platform commits it.
    ime_preedit: String,
    on_change: Option<Box<TextInputChangeAction>>,
}

impl TextInput {
    pub fn new(placeholder: impl Into<String>) -> Self {
        Self {
            id: WidgetId::new(),
            text: String::new(),
            placeholder: placeholder.into(),
            bounds: Rect::ZERO,
            enabled: true,
            cursor: 0,
            focused: false,
            selection_start: None,
            mouse_down: false,
            cursor_visible: Cell::new(true),
            last_blink: Cell::new(Instant::now()),
            scroll_x: Cell::new(0.0),
            ime_preedit: String::new(),
            on_change: None,
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        let t = text.into();
        self.cursor = self.grapheme_count(&t);
        self.text = t;
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
            self.selection_start = None;
            self.ime_preedit.clear();
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
        &self.text
    }

    pub fn set_text(&mut self, text: String) {
        self.cursor = self.grapheme_count(&text);
        self.text = text;
        self.clear_selection();
        self.update_scroll(DEFAULT_FONT_SIZE);
    }

    /// Dispatch an action whenever user input changes the committed text.
    pub fn on_change(mut self, action: impl Fn(&str) -> Action + 'static) -> Self {
        self.on_change = Some(Box::new(action));
        self
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.clear_selection();
        self.update_scroll(DEFAULT_FONT_SIZE);
    }

    // ── Grapheme helpers ──────────────────────────────────────────────────

    /// Number of grapheme clusters in the current text.
    fn grapheme_count(&self, s: &str) -> usize {
        s.graphemes(true).count()
    }

    /// Byte offset of the grapheme at `g_idx`. Returns `text.len()` if index
    /// is past the end.
    fn grapheme_byte_idx(&self, g_idx: usize) -> usize {
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
    fn len_graphemes(&self) -> usize {
        self.grapheme_count(&self.text)
    }

    // ── Selection helpers ─────────────────────────────────────────────────

    fn clear_selection(&mut self) {
        self.selection_start = None;
    }

    pub fn has_selection(&self) -> bool {
        self.selection_start.is_some_and(|s| s != self.cursor)
    }

    /// Byte range [start, end) of the current selection, or None.
    pub fn selection_byte_range(&self) -> Option<(usize, usize)> {
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
    fn delete_selection(&mut self) -> bool {
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
        self.update_scroll(DEFAULT_FONT_SIZE);
        true
    }

    /// Set cursor from a pixel x-coordinate relative to text start.
    fn set_cursor_from_text_x(&mut self, pixel_x: f32, font_size: f32) {
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
        self.update_scroll(font_size);
    }

    fn visible_width(&self) -> f32 {
        (self.bounds.width - HORIZONTAL_PADDING * 2.0).max(1.0)
    }

    fn content_left(&self) -> f32 {
        self.bounds.x + HORIZONTAL_PADDING
    }

    fn content_right(&self) -> f32 {
        (self.bounds.x + self.bounds.width - HORIZONTAL_PADDING).max(self.content_left())
    }

    fn content_clip_rect(&self) -> Rect {
        Rect::new(
            self.content_left(),
            self.bounds.y,
            self.visible_width(),
            self.bounds.height,
        )
    }

    fn line_height(font_size: f32) -> f32 {
        font_size * 1.3
    }

    fn text_y(&self, font_size: f32) -> f32 {
        self.bounds.y + (self.bounds.height - Self::line_height(font_size)).max(0.0) * 0.5
    }

    fn text_x(&self) -> f32 {
        self.content_left() - self.scroll_x.get()
    }

    fn cursor_text_x(&self, font_size: f32) -> f32 {
        if self.text.is_empty() {
            0.0
        } else {
            measure_text_width(&self.text[..self.cursor_byte_idx()], font_size)
        }
    }

    fn cursor_screen_x(&self, font_size: f32) -> f32 {
        let preedit_w = if self.ime_preedit.is_empty() {
            0.0
        } else {
            measure_text_width(&self.ime_preedit, font_size)
        };
        self.text_x() + self.cursor_text_x(font_size) + preedit_w
    }

    fn cursor_area(&self, font_size: f32) -> Rect {
        let caret_width = 2.0;
        let max_x = (self.content_right() - caret_width).max(self.content_left());
        let x = self.cursor_screen_x(font_size).clamp(self.content_left(), max_x);
        Rect::new(
            x,
            self.bounds.y + VERTICAL_PADDING,
            caret_width,
            (self.bounds.height - VERTICAL_PADDING * 2.0).max(1.0),
        )
    }

    fn refresh_ime_area(&self, ctx: &mut EventContext) {
        if self.focused {
            ctx.set_ime_enabled(true, Some(self.cursor_area(DEFAULT_FONT_SIZE)));
        }
    }

    fn text_x_from_pointer(&self, position: Point) -> f32 {
        if position.x < self.content_left() {
            0.0
        } else if position.x > self.content_right() {
            measure_text_width(&self.text, DEFAULT_FONT_SIZE)
        } else {
            (position.x - self.content_left() + self.scroll_x.get()).max(0.0)
        }
    }

    fn update_scroll(&self, font_size: f32) {
        let text_w = if self.text.is_empty() {
            0.0
        } else {
            measure_text_width(&self.text, font_size)
        };
        let visible_w = self.visible_width();
        let cursor_x = self.cursor_text_x(font_size);
        let sx = self.scroll_x.get();
        // Cursor to the right of visible area → scroll left
        if cursor_x - sx > visible_w - 4.0 {
            self.scroll_x.set((cursor_x - visible_w + 4.0).min(text_w - visible_w).max(0.0));
        }
        // Cursor to the left of visible area → scroll right
        else if cursor_x - sx < 0.0 {
            self.scroll_x.set(cursor_x.max(0.0));
        }
        // Clamp: don't scroll past the text end
        let max_scroll = (text_w - visible_w).max(0.0);
        if self.scroll_x.get() > max_scroll {
            self.scroll_x.set(max_scroll);
        }
    }

    /// Delete one grapheme before the cursor (for Backspace).
    fn delete_grapheme_before(&mut self) -> bool {
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
    fn delete_grapheme_at(&mut self) -> bool {
        let total = self.len_graphemes();
        if self.cursor >= total {
            return false;
        }
        let byte_idx = self.grapheme_byte_idx(self.cursor);
        let next_byte = self.grapheme_byte_idx(self.cursor + 1);
        self.text.replace_range(byte_idx..next_byte, "");
        true
    }

    /// Insert text at cursor, updating cursor by grapheme count.
    fn insert_at_cursor(&mut self, s: &str) {
        let count = self.grapheme_count(s);
        let idx = self.cursor_byte_idx();
        self.text.insert_str(idx, s);
        self.cursor += count;
        self.update_scroll(DEFAULT_FONT_SIZE);
    }

    /// Move cursor and keep it visible.
    fn move_cursor_to(&mut self, pos: usize) {
        self.cursor = pos;
        self.update_scroll(DEFAULT_FONT_SIZE);
    }

    fn dispatch_change(&self, ctx: &mut EventContext) {
        if let Some(factory) = &self.on_change {
            (ctx.dispatch)(factory(&self.text));
        }
    }

    // ── Word navigation ───────────────────────────────────────────────────

    fn next_word_boundary(&self, from: usize) -> usize {
        let total = self.len_graphemes();
        let mut i = from;
        // Skip word characters
        while i < total {
            let b = self.grapheme_byte_idx(i);
            let nb = self.grapheme_byte_idx((i + 1).min(total));
            if &self.text[b..nb] != " " {
                i += 1;
            } else {
                break;
            }
        }
        // Skip spaces
        while i < total {
            let b = self.grapheme_byte_idx(i);
            let nb = self.grapheme_byte_idx((i + 1).min(total));
            if &self.text[b..nb] == " " {
                i += 1;
            } else {
                break;
            }
        }
        i
    }

    fn prev_word_boundary(&self, from: usize) -> usize {
        if from == 0 {
            return 0;
        }
        let total = self.len_graphemes();
        let mut i = from.min(total);
        // Skip trailing spaces
        while i > 0 {
            let b = self.grapheme_byte_idx(i - 1);
            let nb = self.grapheme_byte_idx(i.min(total));
            if &self.text[b..nb] == " " {
                i -= 1;
            } else {
                break;
            }
        }
        // Skip word characters
        while i > 0 {
            let b = self.grapheme_byte_idx(i - 1);
            let nb = self.grapheme_byte_idx(i.min(total));
            if &self.text[b..nb] != " " {
                i -= 1;
            } else {
                break;
            }
        }
        i
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
        self.update_scroll(DEFAULT_FONT_SIZE);
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.focused || self.mouse_down || !self.ime_preedit.is_empty() {
                ctx.release_pointer_capture(self.id);
                ctx.set_ime_enabled(false, None);
            }
            self.focused = false;
            self.mouse_down = false;
            self.clear_selection();
            self.ime_preedit.clear();
            return EventResult::Ignored;
        }
        match event {
            // ── Mouse ──────────────────────────────────────────────────
            UiEvent::MouseDown { position, button: MouseButton::Left, modifiers } => {
                let clicked = self.bounds.contains(*position);
                if clicked {
                    ctx.focus
                        .request_focus(self.id, mondrian_editor_state::state::PanelKind::Console);
                    let text_x = self.text_x_from_pointer(*position);
                    if modifiers.shift {
                        // Shift+Click: extend selection from anchor (or current cursor)
                        if self.selection_start.is_none() {
                            self.selection_start = Some(self.cursor);
                        }
                        self.set_cursor_from_text_x(text_x, DEFAULT_FONT_SIZE);
                    } else {
                        self.set_cursor_from_text_x(text_x, DEFAULT_FONT_SIZE);
                        self.clear_selection();
                    }
                    self.mouse_down = true;
                    self.ime_preedit.clear();
                    ctx.request_pointer_capture(self.id);
                } else {
                    self.focused = false;
                    self.clear_selection();
                    self.mouse_down = false;
                    self.ime_preedit.clear();
                    ctx.release_pointer_capture(self.id);
                    ctx.set_ime_enabled(false, None);
                    self.refresh_ime_area(ctx);
                    return EventResult::Ignored;
                }
                self.focused = clicked;
                self.refresh_ime_area(ctx);
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                if self.mouse_down {
                    if self.selection_start.is_none() {
                        self.selection_start = Some(self.cursor);
                    }
                    // Allow drag beyond bounds — clamp to valid range
                    let text_x = self.text_x_from_pointer(*position);
                    self.set_cursor_from_text_x(text_x, DEFAULT_FONT_SIZE);
                    self.refresh_ime_area(ctx);
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } => {
                self.mouse_down = false;
                ctx.release_pointer_capture(self.id);
                EventResult::Handled
            }
            // ── Focus ──────────────────────────────────────────────────
            UiEvent::FocusGained => {
                self.focused = true;
                self.cursor_visible.set(true);
                self.last_blink.set(Instant::now());
                self.update_scroll(DEFAULT_FONT_SIZE);
                self.refresh_ime_area(ctx);
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.focused = false;
                self.mouse_down = false;
                self.clear_selection();
                self.ime_preedit.clear();
                ctx.release_pointer_capture(self.id);
                ctx.set_ime_enabled(false, None);
                EventResult::Handled
            }
            // ── Keyboard ───────────────────────────────────────────────
            UiEvent::KeyDown { key, modifiers } if self.focused => {
                let shift = modifiers.shift;
                let ctrl = modifiers.ctrl;
                let before_text = self.text.clone();

                let result = match key {
                    // ── Ctrl shortcuts ─────────────────────────────────
                    KeyCode::A if ctrl => {
                        self.move_cursor_to(self.len_graphemes());
                        self.selection_start = Some(0);
                        EventResult::Handled
                    }
                    KeyCode::C if ctrl => {
                        if let Some((start, end)) = self.selection_byte_range() {
                            ctx.platform.clipboard_copy(&self.text[start..end]);
                            EventResult::Handled
                        } else {
                            EventResult::Ignored
                        }
                    }
                    KeyCode::V if ctrl => {
                        self.delete_selection();
                        if let Some(clip) = ctx.platform.clipboard_paste() {
                            if !clip.is_empty() {
                                self.insert_at_cursor(&clip);
                            }
                        }
                        EventResult::Handled
                    }
                    KeyCode::X if ctrl => {
                        if let Some((start, end)) = self.selection_byte_range() {
                            ctx.platform.clipboard_copy(&self.text[start..end]);
                            self.delete_selection();
                            EventResult::Handled
                        } else {
                            EventResult::Ignored
                        }
                    }
                    // ── Word navigation ────────────────────────────────
                    KeyCode::Left if ctrl => {
                        if !shift {
                            self.clear_selection();
                        } else if self.selection_start.is_none() {
                            self.selection_start = Some(self.cursor);
                        }
                        self.move_cursor_to(self.prev_word_boundary(self.cursor));
                        EventResult::Handled
                    }
                    KeyCode::Right if ctrl => {
                        if !shift {
                            self.clear_selection();
                        } else if self.selection_start.is_none() {
                            self.selection_start = Some(self.cursor);
                        }
                        self.move_cursor_to(self.next_word_boundary(self.cursor));
                        EventResult::Handled
                    }
                    // ── Deletion ────────────────────────────────────────
                    KeyCode::Backspace => {
                        if !self.delete_selection() {
                            self.delete_grapheme_before();
                        }
                        self.update_scroll(DEFAULT_FONT_SIZE);
                        EventResult::Handled
                    }
                    KeyCode::Delete => {
                        if !self.delete_selection() {
                            self.delete_grapheme_at();
                        }
                        self.update_scroll(DEFAULT_FONT_SIZE);
                        EventResult::Handled
                    }
                    // ── Navigation ──────────────────────────────────────
                    KeyCode::Left => {
                        if shift {
                            if self.selection_start.is_none() {
                                self.selection_start = Some(self.cursor);
                            }
                        } else {
                            self.clear_selection();
                        }
                        if self.cursor > 0 {
                            self.move_cursor_to(self.cursor - 1);
                        }
                        EventResult::Handled
                    }
                    KeyCode::Right => {
                        if shift {
                            if self.selection_start.is_none() {
                                self.selection_start = Some(self.cursor);
                            }
                        } else {
                            self.clear_selection();
                        }
                        let total = self.len_graphemes();
                        if self.cursor < total {
                            self.move_cursor_to(self.cursor + 1);
                        }
                        EventResult::Handled
                    }
                    KeyCode::Home => {
                        if shift {
                            if self.selection_start.is_none() {
                                self.selection_start = Some(self.cursor);
                            }
                        } else {
                            self.clear_selection();
                        }
                        self.move_cursor_to(0);
                        EventResult::Handled
                    }
                    KeyCode::End => {
                        if shift {
                            if self.selection_start.is_none() {
                                self.selection_start = Some(self.cursor);
                            }
                        } else {
                            self.clear_selection();
                        }
                        self.move_cursor_to(self.len_graphemes());
                        EventResult::Handled
                    }
                    _ => EventResult::Ignored,
                };
                if result == EventResult::Handled {
                    self.refresh_ime_area(ctx);
                    if self.text != before_text {
                        self.dispatch_change(ctx);
                    }
                }
                result
            }
            // ── Text input ─────────────────────────────────────────────
            UiEvent::TextInput(ch) if self.focused => {
                let before_text = self.text.clone();
                self.delete_selection();
                self.insert_at_cursor(ch);
                self.ime_preedit.clear();
                self.refresh_ime_area(ctx);
                if self.text != before_text {
                    self.dispatch_change(ctx);
                }
                EventResult::Handled
            }
            UiEvent::ImeCommit(ch) if self.focused => {
                let before_text = self.text.clone();
                self.delete_selection();
                self.ime_preedit.clear();
                self.insert_at_cursor(ch);
                self.refresh_ime_area(ctx);
                if self.text != before_text {
                    self.dispatch_change(ctx);
                }
                EventResult::Handled
            }
            UiEvent::ImePreedit(preedit) if self.focused => {
                self.ime_preedit = preedit.clone();
                self.refresh_ime_area(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let font_size = ctx.theme.typography.body.font_size;

        let bg = if !self.enabled {
            tokens.muted
        } else if self.focused {
            tokens.popover
        } else {
            tokens.card
        };
        let border = if self.enabled {
            tokens.border_for_state(self.focused)
        } else {
            tokens.border
        };
        let border_inset = 1.0;
        ctx.encoder.draw_rect(
            self.bounds.inset(-border_inset, -border_inset),
            border,
            spacing.radius_sm + border_inset,
        );
        ctx.encoder.draw_rect(self.bounds, bg, spacing.radius_sm);

        // Clip text content to padded area
        let clip = self.content_clip_rect();
        ctx.encoder.push_clip(clip);

        let sx = self.scroll_x.get();
        let text_x = self.content_left() - sx;
        let text_y = self.text_y(font_size);

        // Selection highlight
        if self.enabled {
            if let Some((byte_start, byte_end)) = self.selection_byte_range() {
                let sel_x = text_x + measure_text_width(&self.text[..byte_start], font_size);
                let sel_w = measure_text_width(&self.text[byte_start..byte_end], font_size);
                let sel_h = font_size * 1.3;
                let sel_y = self.bounds.y + (self.bounds.height - sel_h).max(0.0) * 0.5;
                ctx.encoder
                    .draw_rect(Rect::new(sel_x, sel_y, sel_w, sel_h), tokens.primary, 0.0);
            }
        }

        if !self.text.is_empty() {
            ctx.encoder.draw_text(
                &self.text,
                font_size,
                Point::new(text_x, text_y),
                if self.enabled {
                    tokens.foreground
                } else {
                    tokens.muted_foreground
                },
            );
        } else if !self.focused {
            ctx.encoder.draw_text(
                &self.placeholder,
                font_size,
                Point::new(text_x, text_y),
                tokens.muted_foreground,
            );
        }

        if self.enabled && self.focused && !self.ime_preedit.is_empty() {
            let prefix_byte = self.grapheme_byte_idx(self.cursor);
            let preedit_x = text_x + measure_text_width(&self.text[..prefix_byte], font_size);
            ctx.encoder.draw_text(
                &self.ime_preedit,
                font_size,
                Point::new(preedit_x, text_y),
                tokens.foreground,
            );
            let underline_y = text_y + font_size * 1.25;
            let underline_w = measure_text_width(&self.ime_preedit, font_size).max(4.0);
            ctx.encoder.draw_line(
                Point::new(preedit_x, underline_y),
                Point::new(preedit_x + underline_w, underline_y),
                1.0,
                tokens.primary,
            );
        }

        // Blinking cursor. Draw last so it remains visible over text/preedit.
        if self.enabled && self.focused {
            let now = Instant::now();
            let elapsed = now.duration_since(self.last_blink.get());
            if elapsed.as_millis() >= 500 {
                self.cursor_visible.set(!self.cursor_visible.get());
                self.last_blink.set(now);
            }
            if self.cursor_visible.get() {
                let cursor_area = self.cursor_area(font_size);
                let cursor_color = if self.has_selection() {
                    tokens.primary
                } else {
                    tokens.foreground
                };
                ctx.encoder.draw_rect(cursor_area, cursor_color, 0.0);
            }
        }

        ctx.encoder.pop_clip();
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_editor_state::Action;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

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

        fn draw_rect(&mut self, _bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {
            self.ops.push(PaintOp::Rect);
        }

        fn draw_line(
            &mut self,
            _start: Point,
            _end: Point,
            _width: f32,
            _color: mondrian_core::Color,
        ) {
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
        assert_eq!(ti.cursor, 0);
        assert!(!ti.has_selection());
        assert!(!ti.focused);
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
        assert_eq!(ti.cursor, 5);
    }

    #[test]
    fn with_text_cjk() {
        let ti = TextInput::new("ph").with_text("你好世界");
        assert_eq!(ti.text(), "你好世界");
        assert_eq!(ti.cursor, 4);
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
    }

    #[test]
    fn focus_lost_clears_selection() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.selection_start = Some(0);
        ti.cursor = 2;
        assert!(ti.has_selection());
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        ti.event(&UiEvent::FocusLost, &mut ctx);
        assert!(!ti.focused);
        assert!(!ti.has_selection());
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
        assert_eq!(ti.cursor, 2);
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
        assert_eq!(ti.cursor, 2);
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
        assert_eq!(ti.ime_preedit, "你");

        let result = ti.event(&UiEvent::ImeCommit("你".into()), &mut ctx);
        assert_eq!(result, EventResult::Handled);
        assert!(ti.ime_preedit.is_empty());
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
        let expected_x = ti.content_left()
            + measure_text_width("ab", DEFAULT_FONT_SIZE)
            + measure_text_width("ni", DEFAULT_FONT_SIZE);
        assert!((area.x - expected_x).abs() <= 0.1);
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
        let expected = ti.cursor_area(DEFAULT_FONT_SIZE);
        assert!((area.x - expected.x).abs() <= 0.1);
    }

    #[test]
    fn type_replaces_selection() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.focused = true;
        ti.selection_start = Some(1);
        ti.cursor = 4;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        tp(&mut ti, "X", &mut ctx);
        assert_eq!(ti.text(), "hXo");
        assert_eq!(ti.cursor, 2);
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
        ti.cursor = 2; // after the emoji
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Backspace, &mut ctx);
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.cursor, 1);
    }

    #[test]
    fn delete_deletes_grapheme() {
        let mut ti = TextInput::new("ph").with_text("a😊b");
        ti.focused = true;
        ti.cursor = 1; // before the emoji
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Delete, &mut ctx);
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.cursor, 1);
    }

    // ── Backspace / Delete ───────────────────────────────────────────────

    #[test]
    fn backspace_at_start_noop() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.cursor = 0;
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
        ti.selection_start = Some(1);
        ti.cursor = 4;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Backspace, &mut ctx);
        assert_eq!(ti.text(), "ho");
        assert_eq!(ti.cursor, 1);
    }

    #[test]
    fn delete_at_end_noop() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.cursor = 3;
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
        ti.selection_start = Some(0);
        ti.cursor = 5;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Delete, &mut ctx);
        assert_eq!(ti.text(), "");
        assert_eq!(ti.cursor, 0);
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
        assert_eq!(ti.cursor, 2);
        kd(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.cursor, 3);
    }

    #[test]
    fn home_end() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.cursor = 1;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd(&mut ti, KeyCode::Home, &mut ctx);
        assert_eq!(ti.cursor, 0);
        kd(&mut ti, KeyCode::End, &mut ctx);
        assert_eq!(ti.cursor, 3);
    }

    // ── Word navigation ──────────────────────────────────────────────────

    #[test]
    fn ctrl_left_word() {
        let mut ti = TextInput::new("ph").with_text("hello world foo");
        ti.focused = true;
        // "hello world foo" = 15 graphemes: hello(5) + space(1) + world(5) + space(1) + foo(3)
        ti.cursor = 15; // end
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd_ctrl(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.cursor, 12); // start of "foo"
        kd_ctrl(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.cursor, 6); // start of "world"
        kd_ctrl(&mut ti, KeyCode::Left, &mut ctx);
        assert_eq!(ti.cursor, 0); // start of "hello"
    }

    #[test]
    fn ctrl_right_word() {
        let mut ti = TextInput::new("ph").with_text("hello world");
        ti.focused = true;
        // "hello world" = 11 graphemes: hello(5) + space(1) + world(5)
        ti.cursor = 0;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd_ctrl(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.cursor, 6); // past "hello" + space, at "world" start
        kd_ctrl(&mut ti, KeyCode::Right, &mut ctx);
        assert_eq!(ti.cursor, 11); // past "world", at end
    }

    // ── Shift selection ──────────────────────────────────────────────────

    #[test]
    fn shift_left_selects() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.cursor = 3;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = mk_ctx(&mut f, &mut s, &mut t);
        kd_shift(&mut ti, KeyCode::Left, &mut ctx);
        assert!(ti.has_selection());
        assert_eq!(ti.selection_start, Some(3));
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
        assert_eq!(ti.cursor, 0);
        // Shift+click near end
        md_shift(&mut ti, 150.0, 14.0, &mut ctx);
        assert!(ti.has_selection());
        assert_eq!(ti.selection_start, Some(0));
        assert!(ti.cursor > 0);
    }

    // ── Selection byte range ─────────────────────────────────────────────

    #[test]
    fn selection_range_forward() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.selection_start = Some(1);
        ti.cursor = 4;
        assert_eq!(ti.selection_byte_range(), Some((1, 4)));
    }

    #[test]
    fn selection_range_reverse() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.selection_start = Some(4);
        ti.cursor = 1;
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
        assert_eq!(ti.cursor, 5);
        assert_eq!(ti.selection_start, Some(0));
    }

    #[test]
    fn ctrl_x_cuts() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.focused = true;
        ti.selection_start = Some(1);
        ti.cursor = 4;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let binding = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &binding);
        kd_ctrl(&mut ti, KeyCode::X, &mut ctx);
        assert_eq!(ti.text(), "ho");
        assert_eq!(ti.cursor, 1);
        assert!(!ti.has_selection());
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

    // ── Mouse drag ───────────────────────────────────────────────────────

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
        mm(&mut ti, 80.0, 14.0, &mut ctx);
        assert!(ti.has_selection());
        mu(&mut ti, &mut ctx);
        assert!(ti.has_selection());
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
        assert_eq!(ti.cursor, ti.len_graphemes());
    }

    #[test]
    fn layout_scrolls_long_text_to_end_cursor() {
        let mut ti = TextInput::new("ph").with_text("abcdefghijklmnopqrstuvwxyz");
        ti.layout(Rect::new(0.0, 0.0, 80.0, 28.0));

        assert!(ti.scroll_x.get() > 0.0);
        let caret = ti.cursor_area(DEFAULT_FONT_SIZE);
        assert!(caret.x >= ti.content_left() - 0.1);
        assert!(caret.x <= ti.content_right() + 0.1);
    }

    #[test]
    fn long_text_scroll_uses_renderer_metrics() {
        let text = "abcdefghijklmnopqrstuvwxyz";
        let mut ti = TextInput::new("ph").with_text(text);
        ti.layout(Rect::new(0.0, 0.0, 80.0, 28.0));

        let expected_scroll =
            (measure_text_width(text, DEFAULT_FONT_SIZE) - ti.visible_width()).max(0.0);
        assert!((ti.scroll_x.get() - expected_scroll).abs() <= 0.1);
    }

    #[test]
    fn long_text_caret_stays_inside_content_rect() {
        let mut ti = TextInput::new("ph").with_text("abcdefghijklmnopqrstuvwxyz");
        ti.layout(Rect::new(0.0, 0.0, 80.0, 28.0));

        let caret = ti.cursor_area(DEFAULT_FONT_SIZE);
        assert!(caret.x >= ti.content_left());
        assert!(caret.x + caret.width <= ti.content_right() + 0.1);
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

        assert!(ti.cursor > 0);
    }

    #[test]
    fn click_clears_selection() {
        let mut ti = TextInput::new("ph").with_text("hello");
        layout(&mut ti);
        ti.selection_start = Some(0);
        ti.cursor = 3;
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
        ti.selection_start = Some(0);
        ti.cursor = 1;
        assert!(ti.delete_selection());
        assert_eq!(ti.text(), "");
        assert_eq!(ti.cursor, 0);
        assert!(!ti.delete_selection());
    }

    #[test]
    fn set_text_clears_selection() {
        let mut ti = TextInput::new("ph").with_text("old");
        ti.selection_start = Some(0);
        ti.cursor = 2;
        ti.set_text("new".into());
        assert_eq!(ti.text(), "new");
        assert_eq!(ti.cursor, 3);
        assert!(!ti.has_selection());
    }

    #[test]
    fn clear_resets_all() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.selection_start = Some(1);
        ti.clear();
        assert!(ti.text().is_empty());
        assert_eq!(ti.cursor, 0);
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
