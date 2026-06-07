//! 文本输入框控件
//!
//! 单行文本编辑，支持光标移动、退格删除、Home/End。

use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// TextInput Widget —— 单行文本输入框
pub struct TextInput {
    id: WidgetId,
    text: String,
    #[allow(dead_code)]
    placeholder: String,
    bounds: Rect,
    cursor: usize,
    focused: bool,
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
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
}

impl Widget for TextInput {
    fn id(&self) -> WidgetId { self.id }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        Size::new(200.0, 28.0)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                self.focused = self.bounds.contains(*position);
                EventResult::Handled
            }
            UiEvent::FocusGained => {
                self.focused = true;
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.focused = false;
                EventResult::Handled
            }
            UiEvent::KeyDown { key, modifiers: _ } if self.focused => {
                match key {
                    KeyCode::Backspace => {
                        if self.cursor > 0 {
                            let idx = self.text.char_indices()
                                .nth(self.cursor - 1)
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            self.text.remove(idx);
                            self.cursor -= 1;
                        }
                        EventResult::Handled
                    }
                    KeyCode::Delete => {
                        if self.cursor < self.text.chars().count() {
                            let idx = self.text.char_indices()
                                .nth(self.cursor)
                                .map(|(i, _)| i)
                                .unwrap_or(self.text.len());
                            self.text.remove(idx);
                        }
                        EventResult::Handled
                    }
                    KeyCode::Left => {
                        if self.cursor > 0 {
                            self.cursor -= 1;
                        }
                        EventResult::Handled
                    }
                    KeyCode::Right => {
                        if self.cursor < self.text.chars().count() {
                            self.cursor += 1;
                        }
                        EventResult::Handled
                    }
                    KeyCode::Home => {
                        self.cursor = 0;
                        EventResult::Handled
                    }
                    KeyCode::End => {
                        self.cursor = self.text.chars().count();
                        EventResult::Handled
                    }
                    _ => EventResult::Ignored,
                }
            }
            UiEvent::TextInput(ch) if self.focused => {
                self.text.insert(self.cursor_char_idx(), ch.chars().next().unwrap_or(' '));
                self.cursor += 1;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let bg = if self.focused {
            tokens.popover
        } else {
            tokens.card
        };
        let border = tokens.border_for_state(self.focused);

        ctx.encoder.draw_rect(self.bounds, bg, spacing.radius_sm);
        ctx.encoder.draw_rect(self.bounds, border, 0.0);

        // Cursor bar when focused
        if self.focused {
            let cursor_x = self.bounds.x + 8.0 + self.cursor as f32 * 8.0;
            let cy = self.bounds.y + 4.0;
            let ch = self.bounds.height - 8.0;
            ctx.encoder.draw_line(
                Point::new(cursor_x, cy),
                Point::new(cursor_x, cy + ch),
                1.0,
                tokens.foreground,
            );
        }

        // Text drawn by app-level TextRenderer
        if !self.text.is_empty() {
            ctx.encoder.draw_text(&self.text, 13.0, Point::new(self.bounds.x + 8.0, self.bounds.y + 5.0), tokens.foreground);
        } else if self.focused {
            ctx.encoder.draw_text(&self.placeholder, 13.0, Point::new(self.bounds.x + 8.0, self.bounds.y + 5.0), tokens.muted_foreground);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

impl TextInput {
    fn cursor_char_idx(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{DummyFocus, DummyShortcut, DummyTooltip, make_event_ctx};

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

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        ti.event(&UiEvent::MouseDown {
            position: Point::new(100.0, 14.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        }, &mut ctx);
        assert!(ti.focused);
    }

    #[test]
    fn text_input_type_characters() {
        let mut ti = TextInput::new("ph");
        ti.focused = true;

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
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

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        ti.event(&UiEvent::KeyDown { key: KeyCode::Backspace, modifiers: Modifiers::none() }, &mut ctx);
        assert_eq!(ti.text(), "ab");
        assert_eq!(ti.cursor, 2);
    }

    #[test]
    fn text_input_left_right() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        ti.event(&UiEvent::KeyDown { key: KeyCode::Left, modifiers: Modifiers::none() }, &mut ctx);
        assert_eq!(ti.cursor, 2);
        ti.event(&UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() }, &mut ctx);
        assert_eq!(ti.cursor, 3);
    }

    #[test]
    fn text_input_home_end() {
        let mut ti = TextInput::new("ph").with_text("abc");
        ti.focused = true;
        ti.cursor = 1; // middle

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        ti.event(&UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() }, &mut ctx);
        assert_eq!(ti.cursor, 0);
        ti.event(&UiEvent::KeyDown { key: KeyCode::End, modifiers: Modifiers::none() }, &mut ctx);
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
}
