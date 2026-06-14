//! Runtime glue for the custom winit UI shells.
//!
//! Widgets express side effects as `EventRequests`; this module is the app
//! shell adapter that applies those requests to winit and platform services.

#![allow(deprecated)]

use std::time::{Duration, Instant};

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_platform::{DesktopEyedropper, DesktopPoint};
use mondrian_ui_core::tree::WidgetTreeView;
use mondrian_ui_core::types::{EventResult, KeyCode, Modifiers, MouseButton, Point, Rect, UiEvent};
use mondrian_ui_core::widget::{CursorRequest, DrawCommandEncoder, ImeRequest, PaintContext};
use mondrian_ui_core::Widget;
use mondrian_ui_events::EventRouter;
use mondrian_ui_theme::Theme;
use mondrian_ui_tooltip::TooltipWidget;
use winit::event::{
    ElementState, Ime, KeyEvent, MouseButton as WinitMouseButton, MouseScrollDelta,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, NamedKey};

/// Winit-backed runtime state shared by custom UI app shells.
pub struct WinitUiRuntime {
    eyedropper: DesktopEyedropper,
    tooltip: TooltipWidget,
    last_timer_tick: Instant,
}

impl Default for WinitUiRuntime {
    fn default() -> Self {
        Self {
            eyedropper: DesktopEyedropper::new(),
            tooltip: TooltipWidget::new(),
            last_timer_tick: Instant::now(),
        }
    }
}

impl WinitUiRuntime {
    /// Create runtime state with no active platform sessions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a platform eyedropper session is active.
    pub fn is_eyedropper_active(&self) -> bool {
        self.eyedropper.is_active()
    }

    /// Current preview color for shell-rendered eyedropper affordances.
    pub fn eyedropper_preview_color(&self) -> Color {
        self.eyedropper.preview_color()
    }

    /// Route one UI event through the framework and apply shell side effects.
    pub fn route_window_event(
        &mut self,
        window: &winit::window::Window,
        router: &mut EventRouter,
        root: &mut dyn Widget,
        event: UiEvent,
        dispatch: &dyn Fn(Action),
    ) -> EventResult {
        self.tick_tooltip(window, router);
        let tooltip_was_visible = router.current_tooltip().is_some();
        let mut tree = WidgetTreeView::new(root);
        let result = router.route(event, &mut tree, dispatch);
        self.apply_router_requests(window, router, tooltip_was_visible);
        result
    }

    /// Route one winit keyboard event through the custom UI event model.
    pub fn route_keyboard_input(
        &mut self,
        window: &winit::window::Window,
        router: &mut EventRouter,
        root: &mut dyn Widget,
        event: &KeyEvent,
        modifiers: &mut Modifiers,
        dispatch: &dyn Fn(Action),
    ) -> EventResult {
        let pressed = event.state == ElementState::Pressed;
        update_modifiers_from_key(&event.logical_key, pressed, modifiers);

        let mut result = EventResult::Ignored;
        if let Some(key) = winit_key_to_keycode(&event.logical_key) {
            result = self.route_window_event(
                window,
                router,
                root,
                if pressed {
                    UiEvent::KeyDown { key, modifiers: *modifiers }
                } else {
                    UiEvent::KeyUp { key, modifiers: *modifiers }
                },
                dispatch,
            );
        }

        if pressed && !modifiers.ctrl {
            if let Some(text) = printable_key_text(event.text.as_deref()) {
                let text_result = self.route_window_event(
                    window,
                    router,
                    root,
                    UiEvent::TextInput(text),
                    dispatch,
                );
                if text_result == EventResult::Handled {
                    result = EventResult::Handled;
                }
            }
        }

        result
    }

    /// Route one winit IME event through the custom UI event model.
    pub fn route_ime_event(
        &mut self,
        window: &winit::window::Window,
        router: &mut EventRouter,
        root: &mut dyn Widget,
        event: Ime,
        dispatch: &dyn Fn(Action),
    ) -> EventResult {
        match event {
            Ime::Commit(text) => {
                self.route_window_event(window, router, root, UiEvent::ImeCommit(text), dispatch)
            }
            Ime::Preedit(text, _cursor) => {
                self.route_window_event(window, router, root, UiEvent::ImePreedit(text), dispatch)
            }
            Ime::Disabled => self.route_window_event(
                window,
                router,
                root,
                UiEvent::ImePreedit(String::new()),
                dispatch,
            ),
            Ime::Enabled => EventResult::Ignored,
        }
    }

    /// Update eyedropper preview from a window-local pointer position.
    pub fn update_eyedropper_preview_at_window_point(
        &mut self,
        window: &winit::window::Window,
        point: Point,
    ) {
        if !self.eyedropper.is_active() {
            return;
        }
        if let Some(screen) = window_to_desktop_point(window, point) {
            let _ = self.eyedropper.update_preview(screen);
        }
    }

    /// Finish an active eyedropper session at a window-local pointer position.
    pub fn finish_eyedropper_at_window_point(
        &mut self,
        window: &winit::window::Window,
        router: &mut EventRouter,
        root: &mut dyn Widget,
        point: Point,
        dispatch: &dyn Fn(Action),
    ) {
        let screen = window_to_desktop_point(window, point)
            .unwrap_or_else(|| DesktopPoint::new(point.x.round() as i32, point.y.round() as i32));
        let color = self.eyedropper.finish_at(screen);
        let _ = self.route_window_event(
            window,
            router,
            root,
            UiEvent::EyedropperSample { color },
            dispatch,
        );
    }

    /// Poll global eyedropper state for cross-window sampling.
    pub fn poll_eyedropper(
        &mut self,
        window: &winit::window::Window,
        router: &mut EventRouter,
        root: &mut dyn Widget,
        last_cursor: &mut Point,
        dispatch: &dyn Fn(Action),
    ) {
        if !self.eyedropper.is_active() {
            return;
        }

        if let Some(screen) = self.eyedropper.poll_global_cursor() {
            if let Some(local) = desktop_to_window_point(window, screen) {
                *last_cursor = local;
                let _ = self.route_window_event(
                    window,
                    router,
                    root,
                    UiEvent::MouseMove {
                        position: *last_cursor,
                        modifiers: Modifiers::none(),
                    },
                    dispatch,
                );
            }
        }

        if let Some((screen, color)) = self.eyedropper.poll_global_primary_press() {
            if let Some(local) = desktop_to_window_point(window, screen) {
                *last_cursor = local;
            }
            let _ = self.route_window_event(
                window,
                router,
                root,
                UiEvent::EyedropperSample { color },
                dispatch,
            );
        }
    }

    /// Paint shell-owned overlays after the widget tree overlay pass.
    pub fn paint_shell_overlays(
        &mut self,
        encoder: &mut dyn DrawCommandEncoder,
        theme: &Theme,
        clip_rect: Rect,
        cursor: Point,
        router: &EventRouter,
    ) {
        if self.eyedropper.is_active() {
            paint_eyedropper_overlay(encoder, cursor, self.eyedropper.preview_color());
        }

        if let Some(state) = router.current_tooltip().cloned() {
            self.tooltip.update_state(state);
        } else {
            self.tooltip.clear();
        }

        let mut ctx = PaintContext { encoder, theme, clip_rect };
        self.tooltip.paint_overlay(&mut ctx);
    }

    /// Advance shell timers and schedule the next native wakeup if needed.
    pub fn drive_timers(
        &mut self,
        window: &winit::window::Window,
        router: &mut EventRouter,
        elwt: &ActiveEventLoop,
    ) {
        self.tick_tooltip(window, router);
        if let Some(ms) = router.next_tooltip_update_in_ms() {
            let now = Instant::now();
            if ms == 0 {
                elwt.set_control_flow(ControlFlow::Poll);
            } else {
                elwt.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(ms)));
            }
        }
    }

    fn tick_tooltip(&mut self, window: &winit::window::Window, router: &mut EventRouter) {
        let now = Instant::now();
        let delta_ms = now.saturating_duration_since(self.last_timer_tick).as_millis();
        self.last_timer_tick = now;
        let tooltip_was_visible = router.current_tooltip().is_some();
        router.update_tooltip(delta_ms.min(u128::from(u64::MAX)) as u64);
        if router.current_tooltip().is_some()
            || (tooltip_was_visible && router.current_tooltip().is_none())
        {
            window.request_redraw();
        }
    }

    fn apply_router_requests(
        &mut self,
        window: &winit::window::Window,
        router: &mut EventRouter,
        tooltip_was_visible: bool,
    ) {
        if tooltip_was_visible && router.current_tooltip().is_none() {
            window.request_redraw();
        }
        if let Some(request) = router.take_ime_request() {
            apply_ime_request(window, request);
        }
        if let Some(cursor) = router.take_cursor_request() {
            apply_cursor_request(window, cursor);
        }
        if let Some(request) = router.take_eyedropper_request() {
            if request.active {
                let fallback = request
                    .hotspot
                    .and_then(|point| window_to_desktop_point(window, point))
                    .or_else(|| window_to_desktop_point(window, Point::ZERO))
                    .unwrap_or_else(|| DesktopPoint::new(0, 0));
                self.eyedropper.begin_at_cursor_or(fallback);
            } else {
                self.eyedropper.cancel();
            }
        }
        if router.take_repaint_request() {
            window.request_redraw();
        }
    }
}

/// Convert winit logical keys into Mondrian UI key codes.
pub fn winit_key_to_keycode(key: &Key) -> Option<KeyCode> {
    match key {
        Key::Named(named) => match named {
            NamedKey::Backspace => Some(KeyCode::Backspace),
            NamedKey::Delete => Some(KeyCode::Delete),
            NamedKey::ArrowLeft => Some(KeyCode::Left),
            NamedKey::ArrowRight => Some(KeyCode::Right),
            NamedKey::ArrowUp => Some(KeyCode::Up),
            NamedKey::ArrowDown => Some(KeyCode::Down),
            NamedKey::Home => Some(KeyCode::Home),
            NamedKey::End => Some(KeyCode::End),
            NamedKey::PageUp => Some(KeyCode::PageUp),
            NamedKey::PageDown => Some(KeyCode::PageDown),
            NamedKey::Enter => Some(KeyCode::Enter),
            NamedKey::Space => Some(KeyCode::Space),
            NamedKey::Tab => Some(KeyCode::Tab),
            NamedKey::Escape => Some(KeyCode::Escape),
            NamedKey::F1 => Some(KeyCode::F1),
            NamedKey::F2 => Some(KeyCode::F2),
            NamedKey::F3 => Some(KeyCode::F3),
            NamedKey::F4 => Some(KeyCode::F4),
            NamedKey::F5 => Some(KeyCode::F5),
            NamedKey::F6 => Some(KeyCode::F6),
            NamedKey::F7 => Some(KeyCode::F7),
            NamedKey::F8 => Some(KeyCode::F8),
            NamedKey::F9 => Some(KeyCode::F9),
            NamedKey::F10 => Some(KeyCode::F10),
            NamedKey::F11 => Some(KeyCode::F11),
            NamedKey::F12 => Some(KeyCode::F12),
            _ => None,
        },
        Key::Character(ch) => match ch.as_str().to_ascii_lowercase().as_str() {
            "a" => Some(KeyCode::A),
            "b" => Some(KeyCode::B),
            "c" => Some(KeyCode::C),
            "d" => Some(KeyCode::D),
            "e" => Some(KeyCode::E),
            "f" => Some(KeyCode::F),
            "g" => Some(KeyCode::G),
            "h" => Some(KeyCode::H),
            "i" => Some(KeyCode::I),
            "j" => Some(KeyCode::J),
            "k" => Some(KeyCode::K),
            "l" => Some(KeyCode::L),
            "m" => Some(KeyCode::M),
            "n" => Some(KeyCode::N),
            "o" => Some(KeyCode::O),
            "p" => Some(KeyCode::P),
            "q" => Some(KeyCode::Q),
            "r" => Some(KeyCode::R),
            "s" => Some(KeyCode::S),
            "t" => Some(KeyCode::T),
            "u" => Some(KeyCode::U),
            "v" => Some(KeyCode::V),
            "w" => Some(KeyCode::W),
            "x" => Some(KeyCode::X),
            "y" => Some(KeyCode::Y),
            "z" => Some(KeyCode::Z),
            "0" => Some(KeyCode::Digit0),
            "1" => Some(KeyCode::Digit1),
            "2" => Some(KeyCode::Digit2),
            "3" => Some(KeyCode::Digit3),
            "4" => Some(KeyCode::Digit4),
            "5" => Some(KeyCode::Digit5),
            "6" => Some(KeyCode::Digit6),
            "7" => Some(KeyCode::Digit7),
            "8" => Some(KeyCode::Digit8),
            "9" => Some(KeyCode::Digit9),
            _ => None,
        },
        _ => None,
    }
}

/// Convert a winit scroll delta into Mondrian's UI scroll-space delta.
///
/// Positive returned values move scroll offsets downward/rightward. Winit
/// reports positive line or pixel deltas for wheel-up gestures on common PC
/// input devices, so the sign is inverted once at the shell boundary.
pub fn winit_scroll_delta_to_ui_delta(delta: MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => -y * 20.0,
        MouseScrollDelta::PixelDelta(position) => -(position.y as f32),
    }
}

/// Convert a winit mouse button into Mondrian's UI mouse button model.
pub fn winit_mouse_button_to_ui_button(button: WinitMouseButton) -> MouseButton {
    match button {
        WinitMouseButton::Left => MouseButton::Left,
        WinitMouseButton::Right => MouseButton::Right,
        WinitMouseButton::Middle => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn update_modifiers_from_key(key: &Key, pressed: bool, modifiers: &mut Modifiers) {
    match key {
        Key::Named(NamedKey::Control) => modifiers.ctrl = pressed,
        Key::Named(NamedKey::Alt) | Key::Named(NamedKey::AltGraph) => modifiers.alt = pressed,
        Key::Named(NamedKey::Shift) => modifiers.shift = pressed,
        _ => {}
    }
}

fn printable_key_text(text: Option<&str>) -> Option<String> {
    let text = text?;
    (!text.is_empty() && !text.chars().any(char::is_control)).then(|| text.to_string())
}

/// Paint the shell-owned eyedropper magnifier.
pub fn paint_eyedropper_overlay(
    encoder: &mut dyn DrawCommandEncoder,
    cursor: Point,
    preview: Color,
) {
    encoder.draw_rect(
        Rect::new(cursor.x - 47.0, cursor.y - 67.0, 96.0, 96.0),
        Color { r: 0.12, g: 0.12, b: 0.14, a: 1.0 },
        48.0,
    );
    encoder.draw_rect(
        Rect::new(cursor.x - 41.0, cursor.y - 61.0, 84.0, 84.0),
        preview,
        42.0,
    );
    encoder.draw_rect(
        Rect::new(cursor.x - 2.0, cursor.y - 62.0, 4.0, 4.0),
        Color::BLACK,
        2.0,
    );
}

fn apply_cursor_request(window: &winit::window::Window, cursor: CursorRequest) {
    let icon = match cursor {
        CursorRequest::Crosshair => winit::window::CursorIcon::Crosshair,
        CursorRequest::Default => winit::window::CursorIcon::Default,
    };
    window.set_cursor_icon(icon);
}

fn apply_ime_request(window: &winit::window::Window, request: ImeRequest) {
    window.set_ime_allowed(request.enabled);
    if request.enabled {
        if let Some(area) = request.cursor_area {
            window.set_ime_cursor_area(
                winit::dpi::PhysicalPosition::new(area.x as f64, area.y as f64),
                winit::dpi::PhysicalSize::new(
                    area.width.max(1.0) as u32,
                    area.height.max(1.0) as u32,
                ),
            );
        }
    }
}

fn window_to_desktop_point(window: &winit::window::Window, point: Point) -> Option<DesktopPoint> {
    let origin = window.inner_position().ok()?;
    Some(DesktopPoint::new(
        origin.x + point.x.round() as i32,
        origin.y + point.y.round() as i32,
    ))
}

fn desktop_to_window_point(window: &winit::window::Window, point: DesktopPoint) -> Option<Point> {
    let origin = window.inner_position().ok()?;
    Some(Point::new(
        (point.x - origin.x) as f32,
        (point.y - origin.y) as f32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_named_navigation_keys() {
        assert_eq!(
            winit_key_to_keycode(&Key::Named(NamedKey::Escape)),
            Some(KeyCode::Escape)
        );
        assert_eq!(
            winit_key_to_keycode(&Key::Named(NamedKey::ArrowLeft)),
            Some(KeyCode::Left)
        );
        assert_eq!(
            winit_key_to_keycode(&Key::Named(NamedKey::F12)),
            Some(KeyCode::F12)
        );
    }

    #[test]
    fn maps_uppercase_character_shortcuts() {
        assert_eq!(
            winit_key_to_keycode(&Key::Character("A".into())),
            Some(KeyCode::A)
        );
        assert_eq!(
            winit_key_to_keycode(&Key::Character("z".into())),
            Some(KeyCode::Z)
        );
        assert_eq!(
            winit_key_to_keycode(&Key::Character("5".into())),
            Some(KeyCode::Digit5)
        );
    }

    #[test]
    fn printable_text_rejects_control_sequences() {
        assert_eq!(printable_key_text(Some("你")).as_deref(), Some("你"));
        assert!(printable_key_text(Some("\u{8}")).is_none());
        assert!(printable_key_text(Some("")).is_none());
        assert!(printable_key_text(None).is_none());
    }

    #[test]
    fn modifier_tracking_updates_on_key_edges() {
        let mut modifiers = Modifiers::none();

        update_modifiers_from_key(&Key::Named(NamedKey::Control), true, &mut modifiers);
        update_modifiers_from_key(&Key::Named(NamedKey::Shift), true, &mut modifiers);
        assert_eq!(
            modifiers,
            Modifiers { ctrl: true, shift: true, ..Modifiers::none() }
        );

        update_modifiers_from_key(&Key::Named(NamedKey::Control), false, &mut modifiers);
        assert_eq!(modifiers, Modifiers { shift: true, ..Modifiers::none() });
    }

    #[test]
    fn scroll_delta_conversion_uses_pc_scroll_direction() {
        assert_eq!(
            winit_scroll_delta_to_ui_delta(MouseScrollDelta::LineDelta(0.0, 1.0)),
            -20.0
        );
        assert_eq!(
            winit_scroll_delta_to_ui_delta(MouseScrollDelta::LineDelta(0.0, -2.0)),
            40.0
        );
        assert_eq!(
            winit_scroll_delta_to_ui_delta(MouseScrollDelta::PixelDelta(
                winit::dpi::PhysicalPosition::new(0.0, 12.5)
            )),
            -12.5
        );
    }

    #[test]
    fn maps_winit_mouse_buttons() {
        assert_eq!(
            winit_mouse_button_to_ui_button(WinitMouseButton::Left),
            MouseButton::Left
        );
        assert_eq!(
            winit_mouse_button_to_ui_button(WinitMouseButton::Right),
            MouseButton::Right
        );
        assert_eq!(
            winit_mouse_button_to_ui_button(WinitMouseButton::Middle),
            MouseButton::Middle
        );
        assert_eq!(
            winit_mouse_button_to_ui_button(WinitMouseButton::Other(7)),
            MouseButton::Left
        );
    }
}
