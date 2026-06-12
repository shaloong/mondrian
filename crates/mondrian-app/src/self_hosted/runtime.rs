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
use mondrian_ui_core::types::{EventResult, Modifiers, Point, Rect, UiEvent};
use mondrian_ui_core::widget::{CursorRequest, DrawCommandEncoder, ImeRequest, PaintContext};
use mondrian_ui_core::Widget;
use mondrian_ui_events::EventRouter;
use mondrian_ui_theme::Theme;
use mondrian_ui_tooltip::TooltipWidget;
use winit::event_loop::{ActiveEventLoop, ControlFlow};

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
