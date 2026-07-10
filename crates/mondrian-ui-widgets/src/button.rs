//! Button 控件
//!
//! 支持 Normal / Hovered / Pressed 三态 + 点击派发 Action。

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, EventContext, PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::{Theme, ThemePreset};
use std::cell::Cell;

use crate::paint::{centered_text_origin_y, color_with_alpha, paint_focus_ring};
use crate::text_metrics::{centered_text_x, measure_single_line};
use crate::vector_icon::VectorIcon;

/// 按钮状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Normal,
    Hovered,
    Pressed,
}

/// Button visual style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    /// Standard filled button with card background.
    Filled,
    /// Minimal transparent button for sidebar nav, toolbar items, etc.
    Minimal,
}

/// Button Widget —— 可点击的标签按钮
pub struct Button {
    id: WidgetId,
    label: String,
    leading_icon: Option<VectorIcon>,
    bounds: Rect,
    state: ButtonState,
    enabled: bool,
    pub on_click: Option<Action>,
    focused: bool,
    focus_visible: bool,
    visual: Cell<ButtonVisualTokens>,
    style: ButtonStyle,
    active: bool,
    text_left: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ButtonVisualTokens {
    padding_x: f32,
    height: f32,
    icon_gap: f32,
    icon_size: f32,
    font_size: f32,
    radius: f32,
}

impl ButtonVisualTokens {
    fn from_theme(theme: &Theme) -> Self {
        let spacing = &theme.spacing;
        Self {
            padding_x: spacing.md + spacing.border_emphasis,
            height: spacing.interact_height,
            icon_gap: spacing.sm,
            icon_size: spacing.icon_size,
            font_size: theme.typography.button.font_size,
            radius: spacing.radius_md,
        }
    }
}

impl Default for ButtonVisualTokens {
    fn default() -> Self {
        Self::from_theme(&ThemePreset::Dark.build())
    }
}

impl Button {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            id: WidgetId::new(),
            label: label.into(),
            leading_icon: None,
            bounds: Rect::ZERO,
            state: ButtonState::Normal,
            enabled: true,
            on_click: None,
            focused: false,
            focus_visible: false,
            visual: Cell::new(ButtonVisualTokens::default()),
            style: ButtonStyle::Filled,
            active: false,
            text_left: false,
        }
    }

    /// Paint a vector icon before the label.
    pub fn with_leading_icon(mut self, icon: VectorIcon) -> Self {
        self.leading_icon = Some(icon);
        self
    }

    pub fn on_click(mut self, action: Action) -> Self {
        self.on_click = Some(action);
        self
    }

    /// Set whether the button accepts user input and participates in focus.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        if !enabled {
            self.state = ButtonState::Normal;
            self.focused = false;
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

    /// Use minimal style (transparent bg, hover highlight, active state).
    pub fn minimal(mut self) -> Self {
        self.style = ButtonStyle::Minimal;
        self
    }

    /// Set the active/selected state for minimal-style buttons.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Update active state at runtime (e.g., when tab changes).
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    /// Left-align the button text instead of centering.
    pub fn text_left(mut self) -> Self {
        self.text_left = true;
        self
    }

    pub fn state(&self) -> ButtonState {
        self.state
    }

    fn activate(&self, ctx: &mut EventContext) {
        if let Some(action) = &self.on_click {
            (ctx.dispatch)(action.clone());
        }
    }

    fn clear_visual_state(&mut self) -> bool {
        let changed = self.state != ButtonState::Normal || self.focused || self.focus_visible;
        self.state = ButtonState::Normal;
        self.focused = false;
        self.focus_visible = false;
        changed
    }
}

impl Widget for Button {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let visual = self.visual.get();
        let (label_width, _) = measure_single_line(&self.label, visual.font_size);
        let icon_width = self.leading_icon.as_ref().map_or(0.0, |_| visual.icon_size);
        let icon_gap = if self.leading_icon.is_some() && !self.label.is_empty() {
            visual.icon_gap
        } else {
            0.0
        };
        let preferred = Size::new(
            icon_width + icon_gap + label_width + visual.padding_x * 2.0,
            visual.height,
        );
        constraint.constrain(preferred)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.clear_visual_state() {
                ctx.request_repaint();
            }
            return EventResult::Ignored;
        }
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                self.state = ButtonState::Pressed;
                self.focus_visible = false;
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                if self.state == ButtonState::Pressed {
                    if self.bounds.contains(*position) {
                        self.activate(ctx);
                    }
                    self.state = ButtonState::Normal;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::MouseMove { position, .. } if self.state != ButtonState::Pressed => {
                let was_hovered = self.state == ButtonState::Hovered;
                let now_inside = self.bounds.contains(*position);
                if now_inside && !was_hovered {
                    self.state = ButtonState::Hovered;
                    ctx.request_repaint();
                } else if !now_inside && was_hovered {
                    self.state = ButtonState::Normal;
                    ctx.request_repaint();
                }
                EventResult::Ignored
            }
            UiEvent::FocusGained { source } => {
                self.state = ButtonState::Hovered;
                self.focused = true;
                self.focus_visible = source.is_focus_visible();
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                if self.clear_visual_state() {
                    ctx.request_repaint();
                }
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, modifiers }
                if *modifiers == Modifiers::none() =>
            {
                self.state = ButtonState::Pressed;
                self.activate(ctx);
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::KeyUp { key: KeyCode::Enter | KeyCode::Space, .. } => {
                if self.state == ButtonState::Pressed {
                    self.state = ButtonState::Hovered;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.visual.set(ButtonVisualTokens::from_theme(ctx.theme));
        let visual = self.visual.get();
        let tokens = &ctx.theme.colors;
        let font_size = visual.font_size;

        let bg = if !self.enabled {
            tokens.muted
        } else if self.style == ButtonStyle::Minimal {
            if self.active {
                color_with_alpha(tokens.foreground, 0.09)
            } else {
                match self.state {
                    ButtonState::Normal => Color::TRANSPARENT,
                    ButtonState::Hovered => color_with_alpha(tokens.foreground, 0.06),
                    ButtonState::Pressed => color_with_alpha(tokens.foreground, 0.04),
                }
            }
        } else {
            match self.state {
                ButtonState::Normal => tokens.card,
                ButtonState::Hovered => tokens.accent,
                ButtonState::Pressed => tokens.muted,
            }
        };

        ctx.encoder.draw_rect(self.bounds, bg, visual.radius);
        if self.focus_visible {
            paint_focus_ring(ctx, self.bounds, visual.radius);
        }

        let content = self.bounds.inset(visual.padding_x, 0.0);
        let icon_size = visual.icon_size.min(content.height).max(1.0);
        let icon_color = if self.enabled {
            tokens.foreground
        } else {
            tokens.muted_foreground
        };

        if let Some(icon) = &self.leading_icon {
            let (label_width, _) = measure_single_line(&self.label, font_size);
            let gap = if self.label.is_empty() {
                0.0
            } else {
                visual.icon_gap
            };
            let desired_width = icon_size + gap + label_width;
            let content_x = content.x + (content.width - desired_width).max(0.0) * 0.5;
            let icon_rect = Rect::new(
                content_x,
                self.bounds.y + (self.bounds.height - icon_size).max(0.0) * 0.5,
                icon_size,
                icon_size,
            );
            icon.paint(ctx, icon_rect, icon_color);

            if !self.label.is_empty() {
                let content_right = content.x + content.width;
                let text_clip_x = (icon_rect.x + icon_rect.width + gap).min(content_right);
                let text_clip = Rect::new(
                    text_clip_x,
                    content.y,
                    (content_right - text_clip_x).max(0.0),
                    content.height,
                );
                paint_button_label(
                    ctx,
                    &self.label,
                    text_clip,
                    text_clip.x,
                    font_size,
                    icon_color,
                );
            }
        } else if !self.label.is_empty() {
            let tx = if self.text_left {
                content.x
            } else {
                centered_text_x(content.x, content.width, &self.label, font_size)
            };
            paint_button_label(ctx, &self.label, content, tx, font_size, icon_color);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }

    fn accessibility(&self) -> Option<AccessibilityNode> {
        Some(
            AccessibilityNode::new(self.id, AccessibilityRole::Button)
                .with_name(self.label.clone())
                .with_state(AccessibilityState {
                    focusable: self.enabled,
                    focused: self.focused,
                    disabled: !self.enabled,
                    pressed: Some(self.state == ButtonState::Pressed),
                    ..AccessibilityState::default()
                }),
        )
    }
}

fn paint_button_label(
    ctx: &mut PaintContext,
    label: &str,
    clip: Rect,
    text_x: f32,
    font_size: f32,
    color: mondrian_core::Color,
) {
    let ty = centered_text_origin_y(clip, ctx.theme.typography.button.line_height);
    ctx.push_clip(clip);
    ctx.encoder.draw_text(label, font_size, Point::new(text_x, ty), color);
    ctx.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

    #[derive(Default)]
    struct PaintRecorder {
        rects: Vec<Rect>,
        rect_radii: Vec<f32>,
        clips: Vec<Rect>,
        clip_pops: usize,
        triangles: usize,
        raster_images: usize,
        texts: Vec<String>,
        text_font_sizes: Vec<f32>,
    }

    impl DrawCommandEncoder for PaintRecorder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, bounds: Rect, _color: mondrian_core::Color, corner_radius: f32) {
            self.rects.push(bounds);
            self.rect_radii.push(corner_radius);
        }

        fn draw_line(
            &mut self,
            _start: Point,
            _end: Point,
            _width: f32,
            _color: mondrian_core::Color,
        ) {
        }

        fn draw_text(
            &mut self,
            text: &str,
            font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.into());
            self.text_font_sizes.push(font_size);
        }

        fn draw_triangles(&mut self, vertices: &[Point], _color: mondrian_core::Color) {
            self.triangles += vertices.len();
        }

        fn draw_raster_image(
            &mut self,
            _key: &str,
            _bounds: Rect,
            _width: u32,
            _height: u32,
            _color_space: mondrian_ui_core::RasterImageColorSpace,
            _rgba: std::sync::Arc<[u8]>,
            _tint: mondrian_core::Color,
        ) {
            self.raster_images += 1;
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn test_icon() -> VectorIcon {
        VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M6 12L18 12" fill="none" stroke="black"/></svg>"#,
        )
        .expect("svg icon")
    }

    fn event_ctx_with_capture<'a>(
        focus: &'a mut DummyFocus,
        shortcut: &'a mut DummyShortcut,
        tooltip: &'a mut DummyTooltip,
        dispatch_fn: &'a dyn Fn(Action),
    ) -> EventContext<'a> {
        make_event_ctx(focus, shortcut, tooltip, dispatch_fn)
    }

    #[test]
    fn button_new_is_normal() {
        let b = Button::new("Click");
        assert_eq!(b.state(), ButtonState::Normal);
    }

    #[test]
    fn button_measure_non_empty() {
        let b = Button::new("Hello");
        let s = b.measure(LayoutConstraint::LOOSE);
        assert!(s.width > 0.0);
        assert!(s.height > 0.0);
    }

    #[test]
    fn button_measure_includes_leading_icon_and_gap() {
        let theme = ThemePreset::Dark.build();
        let visual = ButtonVisualTokens::from_theme(&theme);
        let plain = Button::new("Hello").measure(LayoutConstraint::LOOSE);
        let icon = Button::new("Hello")
            .with_leading_icon(test_icon())
            .measure(LayoutConstraint::LOOSE);

        assert!(icon.width > plain.width + visual.icon_size);
        assert_eq!(icon.height, plain.height);
    }

    #[test]
    fn button_paint_clips_long_label_to_inner_text_area() {
        let mut b = Button::new("A very long button label");
        b.layout(Rect::new(10.0, 20.0, 80.0, 28.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = PaintRecorder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 200.0, 100.0),
        };

        b.paint(&mut ctx);

        assert_eq!(encoder.texts, vec!["A very long button label"]);
        assert_eq!(encoder.clips, vec![Rect::new(22.0, 20.0, 56.0, 28.0)]);
        assert_eq!(encoder.clip_pops, 1);
    }

    #[test]
    fn button_visual_metrics_follow_theme_tokens() {
        let mut theme = ThemePreset::Dark.build();
        theme.spacing.md = 14.0;
        theme.spacing.sm = 7.0;
        theme.spacing.border_emphasis = 3.0;
        theme.spacing.interact_height = 34.0;
        theme.spacing.icon_size = 18.0;
        theme.spacing.radius_md = 9.0;
        theme.typography.button.font_size = 13.5;
        let visual = ButtonVisualTokens::from_theme(&theme);
        let mut b = Button::new("Tokenized").with_leading_icon(test_icon());
        b.layout(Rect::new(10.0, 20.0, 160.0, visual.height));
        let mut encoder = PaintRecorder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 240.0, 120.0),
        };

        b.paint(&mut ctx);

        let measured = b.measure(LayoutConstraint::LOOSE);
        assert_eq!(measured.height, visual.height);
        assert!(measured.width > visual.padding_x * 2.0 + visual.icon_size + visual.icon_gap);
        assert_eq!(
            encoder.rects[0],
            Rect::new(10.0, 20.0, 160.0, visual.height)
        );
        assert_eq!(encoder.rect_radii[0], visual.radius);
        assert_eq!(encoder.texts, vec!["Tokenized"]);
        assert_eq!(encoder.text_font_sizes, vec![visual.font_size]);
        assert_eq!(
            encoder.clips[0].height, visual.height,
            "label clip should use the tokenized control height"
        );
    }

    #[test]
    fn button_paints_leading_icon_and_clips_label_after_icon() {
        let mut b = Button::new("Remove").with_leading_icon(test_icon());
        b.layout(Rect::new(10.0, 20.0, 96.0, 28.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = PaintRecorder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 200.0, 100.0),
        };

        b.paint(&mut ctx);

        assert!(encoder.triangles > 0 || encoder.raster_images > 0);
        assert_eq!(encoder.texts, vec!["Remove"]);
        assert_eq!(encoder.clip_pops, 1);
        assert!(encoder.clips[0].x > 22.0);
        assert!(encoder.clips[0].width < 72.0);
    }

    #[test]
    fn button_mouse_down_in_bounds_sets_pressed() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(b.state(), ButtonState::Pressed);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn button_mouse_down_outside_bounds_ignored() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        let r = b.event(
            &UiEvent::MouseDown {
                position: Point::new(200.0, 200.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Ignored);
        assert_eq!(b.state(), ButtonState::Normal);
    }

    #[test]
    fn button_click_dispatches_action() {
        let mut b = Button::new("OK").on_click(Action::Play);
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        ctx.requests.repaint = false;
        b.event(
            &UiEvent::MouseUp {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(b.state(), ButtonState::Normal);
        assert!(ctx.requests.repaint);
        let actions = cell.into_inner();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], Action::Play);
    }

    #[test]
    fn disabled_button_ignores_mouse_and_focus() {
        let mut b = Button::new("OK").on_click(Action::Play).disabled();
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = b.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(b.state(), ButtonState::Normal);
        assert!(!b.can_focus());
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn button_accessibility_exposes_role_name_and_disabled_state() {
        let button = Button::new("Render").disabled();

        let node = button.accessibility().expect("button should expose accessibility");

        assert_eq!(node.role, AccessibilityRole::Button);
        assert_eq!(node.name.as_deref(), Some("Render"));
        assert!(!node.state.focusable);
        assert!(node.state.disabled);
        assert_eq!(node.state.pressed, Some(false));
    }

    #[test]
    fn disabled_button_clears_stale_visual_state_and_repaints() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        b.state = ButtonState::Pressed;
        b.focused = true;
        b.focus_visible = true;
        b.enabled = false;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let result = b.event(
            &UiEvent::MouseMove {
                position: Point::new(50.0, 15.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(b.state(), ButtonState::Normal);
        assert!(!b.focused);
        assert!(!b.focus_visible);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn button_release_outside_no_click() {
        let mut b = Button::new("OK").on_click(Action::Play);
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        b.event(
            &UiEvent::MouseUp {
                position: Point::new(200.0, 200.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn button_focus_gained_sets_hovered() {
        let mut b = Button::new("OK");
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        assert_eq!(b.state(), ButtonState::Hovered);
        assert!(b.focused);
        assert!(b.focus_visible);
        assert!(b.accessibility().unwrap().state.focused);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn button_pointer_focus_is_accessible_without_focus_ring() {
        let mut b = Button::new("OK");
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(&UiEvent::focus_gained_pointer(), &mut ctx);

        assert_eq!(b.state(), ButtonState::Hovered);
        assert!(b.focused);
        assert!(!b.focus_visible);
        assert!(b.accessibility().unwrap().state.focused);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn button_focus_lost_clears_hovered() {
        let mut b = Button::new("OK");
        b.state = ButtonState::Hovered;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(&UiEvent::FocusLost, &mut ctx);
        assert_eq!(b.state(), ButtonState::Normal);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn button_no_click_without_action() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        b.event(
            &UiEvent::MouseUp {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn button_enter_dispatches_action_and_releases_on_key_up() {
        let mut b = Button::new("OK").on_click(Action::Play);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = b.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(b.state(), ButtonState::Pressed);
        assert_eq!(cell.borrow().as_slice(), &[Action::Play]);
        assert!(ctx.requests.repaint);
        ctx.requests.repaint = false;

        let result = b.event(
            &UiEvent::KeyUp { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(b.state(), ButtonState::Hovered);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn button_keyboard_activation_ignores_modified_key_down() {
        let mut b = Button::new("OK").on_click(Action::Play);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        for (key, modifiers) in [
            (KeyCode::Enter, Modifiers::ctrl()),
            (KeyCode::Space, Modifiers::shift()),
        ] {
            assert_eq!(
                b.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert_eq!(b.state(), ButtonState::Normal);
        }
        assert!(cell.borrow().is_empty());
    }

    #[test]
    fn button_key_up_releases_press_even_when_modifiers_changed() {
        let mut b = Button::new("OK").on_click(Action::Play);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        assert_eq!(
            b.event(
                &UiEvent::KeyDown { key: KeyCode::Space, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            b.event(
                &UiEvent::KeyUp { key: KeyCode::Space, modifiers: Modifiers::shift() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(b.state(), ButtonState::Hovered);
        assert_eq!(cell.borrow().as_slice(), &[Action::Play]);
    }

    #[test]
    fn button_space_without_action_is_handled_but_dispatches_nothing() {
        let mut b = Button::new("OK");
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = b.event(
            &UiEvent::KeyDown { key: KeyCode::Space, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(b.state(), ButtonState::Pressed);
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn button_mouse_move_in_sets_hovered() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = b.event(
            &UiEvent::MouseMove {
                position: Point::new(50.0, 15.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Ignored); // hover change does not stop propagation
        assert_eq!(b.state(), ButtonState::Hovered);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn button_mouse_move_out_clears_hovered() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        b.state = ButtonState::Hovered;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = b.event(
            &UiEvent::MouseMove {
                position: Point::new(200.0, 15.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Ignored);
        assert_eq!(b.state(), ButtonState::Normal);
        assert!(ctx.requests.repaint);
    }

    #[test]
    fn button_mouse_move_during_press_no_hover_change() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        b.state = ButtonState::Pressed;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = b.event(
            &UiEvent::MouseMove {
                position: Point::new(200.0, 15.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        // During press, MouseMove does NOT change state (guard: self.state != Pressed)
        assert_eq!(r, EventResult::Ignored);
        assert_eq!(b.state(), ButtonState::Pressed);
        assert!(!ctx.requests.repaint);
    }
}
