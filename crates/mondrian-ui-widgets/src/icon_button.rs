//! Compact icon-only button controls.

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::button::ButtonState;
use crate::paint::{color_with_alpha, paint_focus_ring};
use crate::vector_icon::VectorIcon;

const ICON_BUTTON_SIZE: f32 = 28.0;

/// A compact button that paints vector icon geometry instead of text.
pub struct IconButton {
    id: WidgetId,
    icon: VectorIcon,
    tooltip: Option<String>,
    bounds: Rect,
    state: ButtonState,
    enabled: bool,
    on_click: Option<Action>,
    focus_visible: bool,
}

impl IconButton {
    /// Create an enabled button from parsed SVG-backed vector geometry.
    pub fn new(icon: VectorIcon) -> Self {
        Self {
            id: WidgetId::new(),
            icon,
            tooltip: None,
            bounds: Rect::ZERO,
            state: ButtonState::Normal,
            enabled: true,
            on_click: None,
            focus_visible: false,
        }
    }

    /// Create an enabled button from parsed SVG-backed vector geometry.
    pub fn from_vector_icon(icon: VectorIcon) -> Self {
        Self::new(icon)
    }

    /// Set the action dispatched when the button is activated.
    pub fn on_click(mut self, action: Action) -> Self {
        self.on_click = Some(action);
        self
    }

    /// Set a hover tooltip for icon-only buttons.
    pub fn with_tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Set whether the button accepts input and participates in focus.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        if !enabled {
            self.state = ButtonState::Normal;
            self.focus_visible = false;
        }
        self
    }

    /// Disable the button.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the button is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Current interaction state.
    pub fn state(&self) -> ButtonState {
        self.state
    }

    fn activate(&self, ctx: &mut EventContext) {
        if let Some(action) = &self.on_click {
            (ctx.dispatch)(action.clone());
        }
    }
}

impl Widget for IconButton {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(ICON_BUTTON_SIZE, ICON_BUTTON_SIZE))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            self.state = ButtonState::Normal;
            self.focus_visible = false;
            return EventResult::Ignored;
        }

        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                self.state = ButtonState::Pressed;
                self.focus_visible = false;
                EventResult::Handled
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                if self.state == ButtonState::Pressed {
                    if self.bounds.contains(*position) {
                        self.activate(ctx);
                    }
                    self.state = ButtonState::Normal;
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::MouseMove { position, .. } if self.state != ButtonState::Pressed => {
                let was_hovered = self.state == ButtonState::Hovered;
                let now_hovered = self.bounds.contains(*position);
                self.state = if now_hovered {
                    ButtonState::Hovered
                } else {
                    ButtonState::Normal
                };
                if now_hovered {
                    if let Some(tooltip) = &self.tooltip {
                        ctx.tooltip.show(
                            tooltip.clone(),
                            Point::new(self.bounds.x, self.bounds.y + self.bounds.height),
                        );
                    }
                } else if was_hovered && self.tooltip.is_some() {
                    ctx.tooltip.hide();
                }
                if was_hovered != (self.state == ButtonState::Hovered) {
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::FocusGained => {
                self.focus_visible = true;
                self.state = ButtonState::Hovered;
                if let Some(tooltip) = &self.tooltip {
                    ctx.tooltip.show(
                        tooltip.clone(),
                        Point::new(self.bounds.x, self.bounds.y + self.bounds.height),
                    );
                }
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.focus_visible = false;
                self.state = ButtonState::Normal;
                if self.tooltip.is_some() {
                    ctx.tooltip.hide();
                }
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, .. } => {
                self.state = ButtonState::Pressed;
                EventResult::Handled
            }
            UiEvent::KeyUp { key: KeyCode::Enter | KeyCode::Space, .. } => {
                if self.state == ButtonState::Pressed {
                    self.activate(ctx);
                    self.state = ButtonState::Hovered;
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
        let bg = if !self.enabled {
            tokens.muted
        } else {
            match self.state {
                ButtonState::Normal => tokens.card,
                ButtonState::Hovered => tokens.accent,
                ButtonState::Pressed => tokens.muted,
            }
        };
        let icon_color = if self.enabled {
            tokens.foreground
        } else {
            tokens.muted_foreground
        };

        ctx.encoder.draw_rect(self.bounds, bg, spacing.radius_md);
        if self.focus_visible {
            paint_focus_ring(ctx, self.bounds, spacing.radius_md);
        }
        let icon_color = color_with_alpha(icon_color, 0.92);
        self.icon.paint(ctx, self.bounds.inset(5.0, 5.0), icon_color);
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
    use mondrian_core::Color;
    use mondrian_ui_core::tooltip::{TooltipManager, TooltipState};
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

    #[derive(Default)]
    struct PaintRecorder {
        lines: usize,
        triangles: usize,
        raster_images: usize,
        texts: Vec<String>,
    }

    #[derive(Default)]
    struct TooltipRecorder {
        current: Option<TooltipState>,
        hide_count: usize,
    }

    impl TooltipManager for TooltipRecorder {
        fn show(&mut self, text: String, position: Point) {
            self.current = Some(TooltipState { text, position, visible: true });
        }

        fn hide(&mut self) {
            self.current = None;
            self.hide_count += 1;
        }

        fn current(&self) -> Option<&TooltipState> {
            self.current.as_ref()
        }

        fn update(&mut self, _delta_ms: u64) {}
    }

    impl DrawCommandEncoder for PaintRecorder {
        fn push_clip(&mut self, _bounds: Rect) {}
        fn pop_clip(&mut self) {}
        fn draw_rect(&mut self, _bounds: Rect, _color: Color, _corner_radius: f32) {}
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {
            self.lines += 1;
        }
        fn draw_triangles(&mut self, vertices: &[Point], _color: Color) {
            self.triangles += vertices.len();
        }
        fn draw_raster_image(
            &mut self,
            _key: &str,
            _bounds: Rect,
            _width: u32,
            _height: u32,
            _rgba: std::sync::Arc<[u8]>,
            _tint: Color,
        ) {
            self.raster_images += 1;
        }
        fn draw_text(&mut self, text: &str, _font_size: f32, _position: Point, _color: Color) {
            self.texts.push(text.to_owned());
        }
        fn push_translate(&mut self, _offset: glam::Vec2) {}
        fn pop_transform(&mut self) {}
    }

    fn paint_ctx<'a>(encoder: &'a mut PaintRecorder) -> PaintContext<'a> {
        let theme: &'static mondrian_ui_theme::Theme =
            Box::leak(Box::new(ThemePreset::Dark.build()));
        PaintContext {
            encoder,
            theme,
            clip_rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
        }
    }

    fn test_icon() -> VectorIcon {
        VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M6 12L18 12" fill="none" stroke="black"/></svg>"#,
        )
        .expect("svg icon")
    }

    #[test]
    fn icon_button_dispatches_action_on_click() {
        let mut button = IconButton::new(test_icon()).on_click(Action::Play);
        button.layout(Rect::new(0.0, 0.0, 28.0, 28.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            button.event(
                &UiEvent::MouseDown {
                    position: Point::new(12.0, 12.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            button.event(
                &UiEvent::MouseUp {
                    position: Point::new(12.0, 12.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[Action::Play]);
    }

    #[test]
    fn disabled_icon_button_ignores_input_and_focus() {
        let mut button = IconButton::new(test_icon()).on_click(Action::Play).disabled();
        button.layout(Rect::new(0.0, 0.0, 28.0, 28.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            button.event(&UiEvent::FocusGained, &mut ctx),
            EventResult::Ignored
        );
        assert_eq!(
            button.event(
                &UiEvent::MouseDown {
                    position: Point::new(12.0, 12.0),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        assert!(actions.borrow().is_empty());
        assert!(!button.can_focus());
    }

    #[test]
    fn icon_button_paints_vector_icon_without_text() {
        let mut button = IconButton::new(test_icon());
        button.layout(Rect::new(0.0, 0.0, 28.0, 28.0));
        let mut recorder = PaintRecorder::default();
        let mut ctx = paint_ctx(&mut recorder);

        button.paint(&mut ctx);

        assert_eq!(recorder.lines, 0);
        assert!(recorder.triangles > 0 || recorder.raster_images > 0);
        assert!(recorder.texts.is_empty());
    }

    #[test]
    fn icon_button_shows_and_hides_hover_tooltip() {
        let mut button = IconButton::new(test_icon()).with_tooltip("Remove effect");
        button.layout(Rect::new(10.0, 20.0, 28.0, 28.0));
        let dispatch = |_| {};
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = TooltipRecorder::default();
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            button.event(
                &UiEvent::MouseMove {
                    position: Point::new(18.0, 28.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        let current = ctx.tooltip.current().expect("tooltip requested");
        assert_eq!(current.text, "Remove effect");
        assert_eq!(current.position, Point::new(10.0, 48.0));

        assert_eq!(
            button.event(
                &UiEvent::MouseMove {
                    position: Point::new(100.0, 28.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(ctx.tooltip.current().is_none());
    }

    #[test]
    fn disabled_icon_button_does_not_request_tooltip() {
        let mut button = IconButton::new(test_icon()).with_tooltip("Remove effect").disabled();
        button.layout(Rect::new(10.0, 20.0, 28.0, 28.0));
        let dispatch = |_| {};
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = TooltipRecorder::default();
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            button.event(
                &UiEvent::MouseMove {
                    position: Point::new(18.0, 28.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert!(ctx.tooltip.current().is_none());
    }
}
