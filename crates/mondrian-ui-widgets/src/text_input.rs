//! 文本输入框控件
//!
//! 单行文本编辑，支持光标移动、退格删除、Home/End、鼠标拖拽选择。

use std::cell::Cell;

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// TextInput Widget —— 单行文本输入框
pub struct TextInput {
    id: WidgetId,
    text: String,
    placeholder: String,
    bounds: Rect,
    cursor: usize,
    focused: bool,
    cursor_visible: Cell<bool>,
    /// Selection anchor (char index). None means no selection.
    selection_start: Option<usize>,
    /// Track whether the mouse is pressed on this widget for drag-selection.
    mouse_down: bool,
}

impl TextInput {
    pub fn new(placeholder: impl Into<String>) -> Self {
        Self {
            id: WidgetId::new(),
            text: String::new(),
            placeholder: placeholder.into(),
            bounds: Rect::ZERO,
            cursor: 0,
            focused: false,
            cursor_visible: Cell::new(true),
            selection_start: None,
            mouse_down: false,
        }
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        let t = text.into();
        self.cursor = t.chars().count();
        self.text = t;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: String) {
        self.cursor = text.chars().count();
        self.text = text;
        self.clear_selection();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.clear_selection();
    }

    fn clear_selection(&mut self) {
        self.selection_start = None;
    }

    fn has_selection(&self) -> bool {
        self.selection_start.is_some_and(|s| s != self.cursor)
    }

    /// Return the byte range [start, end) of the selection, or None.
    fn selection_byte_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_start?;
        if anchor == self.cursor {
            return None;
        }
        let from = anchor.min(self.cursor);
        let to = anchor.max(self.cursor);
        let byte_start = self.char_to_byte(from);
        let byte_end = self.char_to_byte(to);
        Some((byte_start, byte_end))
    }

    /// Delete the currently selected text (if any). Returns true if deletion happened.
    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection_byte_range() else {
            return false;
        };
        self.text.replace_range(start..end, "");
        let char_count = self.text.chars().count();
        let anchor = self.selection_start.unwrap_or(self.cursor);
        self.cursor = self.cursor.min(anchor);
        if self.cursor > char_count {
            self.cursor = char_count;
        }
        self.clear_selection();
        true
    }

    fn char_to_byte(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }

    fn cursor_byte_idx(&self) -> usize {
        self.char_to_byte(self.cursor)
    }

    /// Set cursor position from a pixel x coordinate relative to text start.
    fn set_cursor_from_x(&mut self, pixel_x: f32, font_size: f32) {
        let mut best = 0;
        let mut best_dist = f32::MAX;
        let char_count = self.text.chars().count();
        for i in 0..=char_count {
            let prefix = &self.text[..self.char_to_byte(i)];
            let w = estimate_text_width(prefix, font_size);
            let dist = (pixel_x - w).abs();
            if dist < best_dist {
                best_dist = dist;
                best = i;
            }
        }
        self.cursor = best;
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
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                let clicked = self.bounds.contains(*position);
                if clicked {
                    ctx.focus
                        .request_focus(self.id, mondrian_editor_state::state::PanelKind::Console);
                    let rel_x = position.x - (self.bounds.x + 8.0);
                    self.set_cursor_from_x(rel_x, 14.0);
                    self.clear_selection();
                    self.mouse_down = true;
                } else {
                    self.focused = false;
                    self.clear_selection();
                    self.mouse_down = false;
                }
                self.focused = clicked;
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                if self.mouse_down && self.bounds.contains(*position) {
                    if self.selection_start.is_none() {
                        self.selection_start = Some(self.cursor);
                    }
                    let rel_x = position.x - (self.bounds.x + 8.0);
                    self.set_cursor_from_x(rel_x, 14.0);
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } => {
                self.mouse_down = false;
                EventResult::Handled
            }
            UiEvent::FocusGained => {
                self.focused = true;
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.focused = false;
                self.mouse_down = false;
                EventResult::Handled
            }
            UiEvent::KeyDown { key, modifiers } if self.focused => {
                let shift = modifiers.shift;
                let ctrl = modifiers.ctrl;

                match key {
                    KeyCode::A if ctrl => {
                        self.cursor = self.text.chars().count();
                        self.selection_start = Some(0);
                        EventResult::Handled
                    }
                    KeyCode::C if ctrl => {
                        if let Some((start, end)) = self.selection_byte_range() {
                            let selected = &self.text[start..end];
                            ctx.platform.clipboard_copy(selected);
                        }
                        EventResult::Handled
                    }
                    KeyCode::V if ctrl => {
                        self.delete_selection();
                        if let Some(clip) = ctx.platform.clipboard_paste() {
                            if !clip.is_empty() {
                                let insert_idx = self.cursor_byte_idx();
                                self.text.insert_str(insert_idx, &clip);
                                self.cursor += clip.chars().count();
                            }
                        }
                        EventResult::Handled
                    }
                    KeyCode::X if ctrl => {
                        if let Some((start, end)) = self.selection_byte_range() {
                            let selected = &self.text[start..end];
                            ctx.platform.clipboard_copy(selected);
                            self.delete_selection();
                        }
                        EventResult::Handled
                    }
                    KeyCode::Backspace => {
                        if !self.delete_selection() && self.cursor > 0 {
                            let idx = self.char_to_byte(self.cursor - 1);
                            self.text.remove(idx);
                            self.cursor -= 1;
                        }
                        EventResult::Handled
                    }
                    KeyCode::Delete => {
                        if !self.delete_selection() && self.cursor < self.text.chars().count() {
                            let idx = self.char_to_byte(self.cursor);
                            self.text.remove(idx);
                        }
                        EventResult::Handled
                    }
                    KeyCode::Left => {
                        if shift {
                            if self.selection_start.is_none() {
                                self.selection_start = Some(self.cursor);
                            }
                        } else {
                            self.clear_selection();
                        }
                        if self.cursor > 0 {
                            self.cursor -= 1;
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
                        if self.cursor < self.text.chars().count() {
                            self.cursor += 1;
                        }
                        EventResult::Handled
                    }
                    KeyCode::Home => {
                        if !shift {
                            self.clear_selection();
                        } else if self.selection_start.is_none() {
                            self.selection_start = Some(self.cursor);
                        }
                        self.cursor = 0;
                        EventResult::Handled
                    }
                    KeyCode::End => {
                        if !shift {
                            self.clear_selection();
                        } else if self.selection_start.is_none() {
                            self.selection_start = Some(self.cursor);
                        }
                        self.cursor = self.text.chars().count();
                        EventResult::Handled
                    }
                    _ => EventResult::Ignored,
                }
            }
            UiEvent::TextInput(ch) if self.focused => {
                self.delete_selection();
                let insert_idx = self.cursor_byte_idx();
                self.text.insert(insert_idx, ch.chars().next().unwrap_or(' '));
                self.cursor += 1;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let font_size = ctx.theme.typography.body.font_size;

        let bg = if self.focused {
            tokens.popover
        } else {
            tokens.card
        };
        let border = tokens.border_for_state(self.focused);

        // Rounded border via larger rect behind fill
        let border_inset = 1.0;
        ctx.encoder.draw_rect(
            self.bounds.inset(-border_inset, -border_inset),
            border,
            spacing.radius_sm + border_inset,
        );
        ctx.encoder.draw_rect(self.bounds, bg, spacing.radius_sm);

        let text_x = self.bounds.x + 8.0;
        let text_y = self.bounds.y + (self.bounds.height - font_size * 1.3).max(0.0) * 0.5;

        // Selection highlight
        if let Some((byte_start, byte_end)) = self.selection_byte_range() {
            let sel_x = text_x + estimate_text_width(&self.text[..byte_start], font_size);
            let sel_w = estimate_text_width(&self.text[byte_start..byte_end], font_size);
            let sel_h = font_size * 1.3;
            let sel_y = self.bounds.y + (self.bounds.height - sel_h).max(0.0) * 0.5;
            ctx.encoder
                .draw_rect(Rect::new(sel_x, sel_y, sel_w, sel_h), tokens.primary, 0.0);
        }

        // Blinking cursor when focused
        if self.focused {
            let visible = self.cursor_visible.get();
            self.cursor_visible.set(!visible);
            if visible {
                let prefix = if self.text.is_empty() {
                    ""
                } else {
                    &self.text[..self.cursor_byte_idx()]
                };
                let cursor_x = text_x + estimate_text_width(prefix, font_size);
                let cy = self.bounds.y + 4.0;
                let ch = self.bounds.height - 8.0;
                ctx.encoder.draw_line(
                    Point::new(cursor_x, cy),
                    Point::new(cursor_x, cy + ch),
                    1.5,
                    if self.has_selection() {
                        tokens.primary
                    } else {
                        tokens.foreground
                    },
                );
            }
        }

        if !self.text.is_empty() {
            ctx.encoder.draw_text(
                &self.text,
                font_size,
                Point::new(text_x, text_y),
                tokens.foreground,
            );
        } else if !self.focused {
            ctx.encoder.draw_text(
                &self.placeholder,
                font_size,
                Point::new(text_x, text_y),
                tokens.muted_foreground,
            );
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};

    #[test]
    fn text_input_new_is_empty() {
        let ti = TextInput::new("placeholder");
        assert!(ti.text().is_empty());
        assert_eq!(ti.cursor, 0);
    }

    #[test]
    fn text_input_with_text() {
        let ti = TextInput::new("ph").with_text("hello");
        assert_eq!(ti.text(), "hello");
        assert_eq!(ti.cursor, 5);
    }

    #[test]
    fn text_input_click_gains_focus() {
        let mut ti = TextInput::new("ph");
        ti.layout(Rect::new(0.0, 0.0, 200.0, 28.0));
        assert!(!ti.focused);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        ti.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(ti.focused);
    }

    #[test]
    fn text_input_type_characters() {
        let mut ti = TextInput::new("ph");
        ti.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        ti.event(&UiEvent::TextInput("a".into()), &mut ctx);
        ti.event(&UiEvent::TextInput("b".into()), &mut ctx);
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.cursor, 2);
    }

    #[test]
    fn text_input_backspace() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        ti.event(
            &UiEvent::KeyDown {
                key: KeyCode::Backspace,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.cursor, 2);
    }

    #[test]
    fn text_input_left_right() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        ti.event(
            &UiEvent::KeyDown { key: KeyCode::Left, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(ti.cursor, 2);
        ti.event(
            &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(ti.cursor, 3);
    }

    #[test]
    fn text_input_home_end() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.cursor = 1;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        ti.event(
            &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(ti.cursor, 0);
        ti.event(
            &UiEvent::KeyDown { key: KeyCode::End, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(ti.cursor, 3);
    }

    #[test]
    fn text_input_set_text() {
        let mut ti = TextInput::new("ph");
        ti.set_text("new".into());
        assert_eq!(ti.text(), "new");
        assert_eq!(ti.cursor, 3);
    }

    #[test]
    fn text_input_clear() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.clear();
        assert!(ti.text().is_empty());
        assert_eq!(ti.cursor, 0);
    }

    #[test]
    fn text_input_has_selection() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.selection_start = Some(0);
        ti.cursor = 3;
        assert!(ti.has_selection());
    }

    #[test]
    fn text_input_no_selection_when_cursor_equals_anchor() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.selection_start = Some(2);
        ti.cursor = 2;
        assert!(!ti.has_selection());
    }

    #[test]
    fn text_input_delete_selection() {
        let mut ti = TextInput::new("ph").with_text("hello");
        ti.selection_start = Some(1); // anchor at 'e'
        ti.cursor = 4; // cursor at 'o'
        assert!(ti.delete_selection());
        assert_eq!(ti.text(), "ho");
        assert_eq!(ti.cursor, 1);
    }
}
