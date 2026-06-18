//! Checkbox 控件
//!
//! 布尔值勾选框，点击切换状态，派发 Action。

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::text_metrics::measure_single_line;

const CHECKBOX_BOX_SIZE: f32 = 16.0;
const CHECKBOX_LABEL_X: f32 = 20.0;
const CHECKBOX_HEIGHT: f32 = 22.0;

/// Adapter that maps the current checkbox state to an editor [`Action`].
pub type CheckboxChangeAction = dyn Fn(bool) -> Action;

/// Checkbox Widget —— 可切换的勾选框
pub struct Checkbox {
    id: WidgetId,
    label: String,
    checked: bool,
    bounds: Rect,
    enabled: bool,
    hovered: bool,
    pressed: bool,
    pub on_toggle: Option<Action>,
    on_change: Option<Box<CheckboxChangeAction>>,
}

impl Checkbox {
    /// Create a checkbox with an initial checked state.
    pub fn new(label: impl Into<String>, checked: bool) -> Self {
        Self {
            id: WidgetId::new(),
            label: label.into(),
            checked,
            bounds: Rect::ZERO,
            enabled: true,
            hovered: false,
            pressed: false,
            on_toggle: None,
            on_change: None,
        }
    }

    /// Dispatch a static action whenever the checkbox toggles.
    pub fn on_toggle(mut self, action: Action) -> Self {
        self.on_toggle = Some(action);
        self
    }

    /// Dispatch a value-aware action whenever the checkbox toggles.
    pub fn on_change(mut self, action: impl Fn(bool) -> Action + 'static) -> Self {
        self.on_change = Some(Box::new(action));
        self
    }

    /// Set whether the checkbox accepts user input and participates in focus.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        if !enabled {
            self.hovered = false;
            self.pressed = false;
        }
        self
    }

    /// Disable the checkbox.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the checkbox is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Return whether the checkbox is checked.
    pub fn is_checked(&self) -> bool {
        self.checked
    }

    /// Set the checked state without dispatching actions.
    pub fn set_checked(&mut self, checked: bool) {
        self.checked = checked;
    }

    fn toggle(&mut self, ctx: &mut EventContext) {
        self.checked = !self.checked;
        if let Some(action) = &self.on_toggle {
            (ctx.dispatch)(action.clone());
        }
        if let Some(action) = &self.on_change {
            (ctx.dispatch)(action(self.checked));
        }
        ctx.request_repaint();
    }
}

impl Widget for Checkbox {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let (label_width, _) = measure_single_line(&self.label, 14.0);
        let preferred = Size::new(CHECKBOX_LABEL_X + label_width, CHECKBOX_HEIGHT);
        constraint.constrain(preferred)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            self.hovered = false;
            self.pressed = false;
            return EventResult::Ignored;
        }
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                self.pressed = true;
                EventResult::Handled
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                if self.pressed && self.bounds.contains(*position) {
                    self.toggle(ctx);
                }
                self.pressed = false;
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                self.hovered = self.bounds.contains(*position);
                EventResult::Ignored
            }
            UiEvent::FocusGained => {
                self.hovered = true;
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.hovered = false;
                self.pressed = false;
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, .. } => {
                self.pressed = true;
                self.toggle(ctx);
                EventResult::Handled
            }
            UiEvent::KeyUp { key: KeyCode::Enter | KeyCode::Space, .. } => {
                if self.pressed {
                    self.pressed = false;
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let box_rect = Rect::new(
            (self.bounds.x + 2.0).round(),
            (self.bounds.y + (self.bounds.height - CHECKBOX_BOX_SIZE) * 0.5).round(),
            CHECKBOX_BOX_SIZE,
            CHECKBOX_BOX_SIZE,
        );

        // Fill color
        let fill = if !self.enabled {
            tokens.muted
        } else if self.checked {
            tokens.primary
        } else if self.hovered {
            tokens.accent
        } else {
            tokens.card
        };

        // Border color
        let border_color = if !self.enabled {
            tokens.border
        } else if self.checked || self.hovered {
            tokens.primary
        } else {
            tokens.border
        };

        // Rounded border: draw slightly larger rounded rect behind fill
        let border_inset = 1.0;
        let border_rect = box_rect.inset(-border_inset, -border_inset);
        ctx.encoder
            .draw_rect(border_rect, border_color, spacing.radius_sm + border_inset);
        // Fill on top
        ctx.encoder.draw_rect(box_rect, fill, spacing.radius_sm);

        // Check mark — one filled shape, not two independent stroked lines.
        if self.checked {
            let vertices = checkmark_triangles(box_rect);
            let check_color = if self.enabled {
                tokens.primary_foreground
            } else {
                tokens.muted_foreground
            };
            ctx.encoder.draw_triangles(&vertices, check_color);
        }

        // Label text
        if !self.label.is_empty() {
            let font_size = ctx.theme.typography.body.font_size;
            let text_clip = Rect::new(
                self.bounds.x + CHECKBOX_LABEL_X,
                self.bounds.y,
                (self.bounds.width - CHECKBOX_LABEL_X).max(0.0),
                self.bounds.height,
            );
            let tx = text_clip.x;
            let ty = self.bounds.y + (self.bounds.height - font_size * 1.3).max(0.0) * 0.5;
            ctx.push_clip(text_clip);
            ctx.encoder.draw_text(
                &self.label,
                font_size,
                Point::new(tx, ty),
                if self.enabled {
                    tokens.foreground
                } else {
                    tokens.muted_foreground
                },
            );
            ctx.pop_clip();
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }
}

fn checkmark_triangles(box_rect: Rect) -> [Point; 12] {
    let p0 = Point::new(box_rect.x + 3.0, box_rect.y + 9.0);
    let p1 = Point::new(box_rect.x + 5.0, box_rect.y + 7.0);
    let p2 = Point::new(box_rect.x + 7.0, box_rect.y + 9.0);
    let p3 = Point::new(box_rect.x + 12.0, box_rect.y + 4.0);
    let p4 = Point::new(box_rect.x + 14.0, box_rect.y + 6.0);
    let p5 = Point::new(box_rect.x + 7.0, box_rect.y + 13.0);

    [p5, p0, p1, p5, p1, p2, p5, p2, p3, p3, p4, p5]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingEncoder {
        lines: Vec<(Point, Point, f32)>,
        triangles: Vec<Point>,
        clips: Vec<Rect>,
        clip_pops: usize,
        texts: Vec<String>,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, _bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {}

        fn draw_line(
            &mut self,
            start: Point,
            end: Point,
            width: f32,
            _color: mondrian_core::Color,
        ) {
            self.lines.push((start, end, width));
        }

        fn draw_triangles(&mut self, vertices: &[Point], _color: mondrian_core::Color) {
            self.triangles.extend_from_slice(vertices);
        }

        fn draw_text(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.into());
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn checked_action(checked: bool) -> Action {
        Action::Custom {
            namespace: "test.checkbox".into(),
            name: format!("checked:{checked}"),
            payload: Default::default(),
        }
    }

    #[test]
    fn checkbox_new_unchecked() {
        let cb = Checkbox::new("Option", false);
        assert!(!cb.is_checked());
    }

    #[test]
    fn checkbox_new_checked() {
        let cb = Checkbox::new("Option", true);
        assert!(cb.is_checked());
    }

    #[test]
    fn checkbox_click_toggles() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let pos = Point::new(50.0, 11.0);
        cb.event(
            &UiEvent::MouseDown {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(!cb.is_checked()); // not yet toggled on MouseDown
        cb.event(
            &UiEvent::MouseUp {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(cb.is_checked()); // toggled on MouseUp
    }

    #[test]
    fn checkbox_click_twice() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let pos = Point::new(50.0, 11.0);
        let md = UiEvent::MouseDown {
            position: pos,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        };
        let mu = UiEvent::MouseUp {
            position: pos,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        };
        cb.event(&md, &mut ctx);
        cb.event(&mu, &mut ctx);
        assert!(cb.is_checked());
        cb.event(&md, &mut ctx);
        cb.event(&mu, &mut ctx);
        assert!(!cb.is_checked());
    }

    #[test]
    fn checkbox_release_outside_no_toggle() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        cb.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 11.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        cb.event(
            &UiEvent::MouseUp {
                position: Point::new(200.0, 200.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(!cb.is_checked());
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn checkbox_set_checked() {
        let mut cb = Checkbox::new("Opt", false);
        cb.set_checked(true);
        assert!(cb.is_checked());
    }

    #[test]
    fn disabled_checkbox_ignores_mouse_and_focus() {
        let mut cb = Checkbox::new("Opt", false).on_toggle(Action::TogglePlay).disabled();
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = cb.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 11.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert!(!cb.is_checked());
        assert!(!cb.can_focus());
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn checkbox_checked_paints_one_filled_checkmark_shape() {
        let mut cb = Checkbox::new("Opt", true);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 100.0, 22.0),
        };

        cb.paint(&mut ctx);

        assert!(encoder.lines.is_empty());
        assert_eq!(encoder.triangles.len(), 12);
        for point in &encoder.triangles {
            assert_eq!(point.x.fract(), 0.0);
            assert_eq!(point.y.fract(), 0.0);
            assert!((2.0..=18.0).contains(&point.x));
            assert!((3.0..=19.0).contains(&point.y));
        }
    }

    #[test]
    fn checkbox_paint_clips_long_label_to_remaining_bounds() {
        let mut cb = Checkbox::new("A very long checkbox label", false);
        cb.layout(Rect::new(10.0, 20.0, 80.0, 22.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 200.0, 100.0),
        };

        cb.paint(&mut ctx);

        assert_eq!(encoder.texts, vec!["A very long checkbox label"]);
        assert_eq!(encoder.clips, vec![Rect::new(30.0, 20.0, 60.0, 22.0)]);
        assert_eq!(encoder.clip_pops, 1);
    }

    #[test]
    fn checkbox_on_toggle_dispatches() {
        let mut cb = Checkbox::new("Opt", false).on_toggle(Action::TogglePlay);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let pos = Point::new(50.0, 11.0);
        cb.event(
            &UiEvent::MouseDown {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        cb.event(
            &UiEvent::MouseUp {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(cell.into_inner(), vec![Action::TogglePlay]);
    }

    #[test]
    fn checkbox_on_change_dispatches_new_state_and_requests_repaint() {
        let mut cb = Checkbox::new("Opt", false).on_change(checked_action);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let pos = Point::new(50.0, 11.0);
        cb.event(
            &UiEvent::MouseDown {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        cb.event(
            &UiEvent::MouseUp {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(cell.borrow().as_slice(), &[checked_action(true)]);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn checkbox_static_and_value_actions_can_dispatch_together() {
        let mut cb = Checkbox::new("Opt", false)
            .on_toggle(Action::TogglePlay)
            .on_change(checked_action);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let pos = Point::new(50.0, 11.0);
        cb.event(
            &UiEvent::MouseDown {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        cb.event(
            &UiEvent::MouseUp {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            cell.borrow().as_slice(),
            &[Action::TogglePlay, checked_action(true)]
        );
    }

    #[test]
    fn checkbox_space_toggles_and_dispatches() {
        let mut cb = Checkbox::new("Opt", false).on_toggle(Action::TogglePlay);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = cb.event(
            &UiEvent::KeyDown { key: KeyCode::Space, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(cb.is_checked());
        assert!(cb.pressed);
        assert_eq!(cell.borrow().as_slice(), &[Action::TogglePlay]);
        assert!(ctx.requests.repaint);

        let result = cb.event(
            &UiEvent::KeyUp { key: KeyCode::Space, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(!cb.pressed);
    }

    #[test]
    fn checkbox_enter_without_action_toggles_without_dispatch() {
        let mut cb = Checkbox::new("Opt", false);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = cb.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(cb.is_checked());
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn checkbox_mouse_move_in_sets_hovered() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = cb.event(
            &UiEvent::MouseMove {
                position: Point::new(50.0, 11.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Ignored);
        assert!(cb.hovered);
    }

    #[test]
    fn checkbox_mouse_move_out_clears_hovered() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));
        cb.hovered = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = cb.event(
            &UiEvent::MouseMove {
                position: Point::new(200.0, 11.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Ignored);
        assert!(!cb.hovered);
    }

    #[test]
    fn checkbox_mouse_move_same_state_no_rehandle() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));
        cb.hovered = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = cb.event(
            &UiEvent::MouseMove {
                position: Point::new(50.0, 11.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Ignored);
        assert!(cb.hovered);
    }
}
