//! Self-hosted top chrome combining product menus, title, and window controls.
//!
//! Native window effects stay behind app-shell actions. This widget only owns
//! layout, hit testing, and drawing for the custom title/menu row.

use mondrian_editor_state::state::WorkspacePreset;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::app::ui_actions::app_shell_window_drag_action;
use crate::self_hosted::menu_bar::{MenuBar, MENU_BAR_HEIGHT};
#[cfg(test)]
use crate::self_hosted::window_controls::PlatformWindowControlStyle;
use crate::self_hosted::window_controls::{WindowControl, WindowControlEdge, WindowControls};
use crate::self_hosted::workspace_layout::SelfHostedWorkspaceLayout;

/// Height reserved for the self-hosted menu/title chrome.
pub const TITLE_BAR_HEIGHT: f32 = 34.0;

const BRAND_WIDTH: f32 = 96.0;
const MENU_WIDTH: f32 = 500.0;

/// Custom top chrome for product windows.
pub struct TitleBar {
    id: WidgetId,
    title: String,
    menu_bar: MenuBar,
    bounds: Rect,
    menu_bounds: Rect,
    title_bounds: Rect,
    window_controls: WindowControls,
    hovered_control: Option<WindowControl>,
    pressed_control: Option<WindowControl>,
}

impl TitleBar {
    /// Build title chrome around an existing menu bar.
    pub fn new(title: impl Into<String>, menu_bar: MenuBar) -> Self {
        Self {
            id: WidgetId::new(),
            title: title.into(),
            menu_bar,
            bounds: Rect::ZERO,
            menu_bounds: Rect::ZERO,
            title_bounds: Rect::ZERO,
            window_controls: WindowControls::default(),
            hovered_control: None,
            pressed_control: None,
        }
    }

    /// Build title chrome with an explicit platform-control style.
    #[cfg(test)]
    pub(crate) fn new_with_window_control_style(
        title: impl Into<String>,
        menu_bar: MenuBar,
        style: PlatformWindowControlStyle,
    ) -> Self {
        let mut title_bar = Self::new(title, menu_bar);
        title_bar.window_controls = WindowControls::new(style);
        title_bar
    }

    /// Access the embedded product menu.
    #[cfg(test)]
    pub(crate) fn menu_bar(&self) -> &MenuBar {
        &self.menu_bar
    }

    /// Mutable access to the embedded product menu for shell tests.
    #[cfg(test)]
    pub(crate) fn menu_bar_mut(&mut self) -> &mut MenuBar {
        &mut self.menu_bar
    }

    /// Refresh checked menu rows that reflect shell-local workspace state.
    pub(crate) fn refresh_shell_menu_checked_state(
        &mut self,
        workspace_preset: WorkspacePreset,
        workspace_layout: Option<&SelfHostedWorkspaceLayout>,
    ) {
        self.menu_bar.refresh_shell_checked_state(workspace_preset, workspace_layout);
    }

    /// Current titlebar bounds.
    #[cfg(test)]
    pub(crate) fn bounds(&self) -> Rect {
        self.bounds
    }

    fn control_at(&self, position: Point) -> Option<WindowControl> {
        self.window_controls.control_at(position)
    }

    #[cfg(test)]
    fn control_bounds(&self, control: WindowControl) -> Rect {
        self.window_controls.control_bounds(control)
    }

    fn is_drag_region(&self, position: Point) -> bool {
        self.bounds.contains(position)
            && !self.menu_bounds.contains(position)
            && self.control_at(position).is_none()
    }
}

impl Widget for TitleBar {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(800.0, TITLE_BAR_HEIGHT))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = Rect::new(bounds.x, bounds.y, bounds.width, TITLE_BAR_HEIGHT);
        self.window_controls.layout(self.bounds);
        let controls_width = self.window_controls.total_width().min(self.bounds.width);
        let content_left = match self.window_controls.edge() {
            WindowControlEdge::Leading => self.bounds.x + controls_width + 8.0,
            WindowControlEdge::Trailing => self.bounds.x + 12.0,
        };
        let content_right = match self.window_controls.edge() {
            WindowControlEdge::Leading => self.bounds.x + self.bounds.width - 12.0,
            WindowControlEdge::Trailing => self.window_controls.bounds().x,
        };

        let menu_x = content_left + BRAND_WIDTH;
        let available_before_controls = (content_right - menu_x - 8.0).max(0.0);
        let menu_width = MENU_WIDTH.min(available_before_controls);
        let menu_y = self.bounds.y + (TITLE_BAR_HEIGHT - MENU_BAR_HEIGHT) * 0.5;
        self.menu_bounds = Rect::new(menu_x, menu_y, menu_width, MENU_BAR_HEIGHT);
        self.menu_bar.layout(self.menu_bounds);

        let title_x = (menu_x + menu_width + 12.0).min(content_right);
        let title_width = (content_right - title_x - 12.0).max(0.0);
        self.title_bounds = Rect::new(title_x, self.bounds.y, title_width, TITLE_BAR_HEIGHT);
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if self.menu_bar.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }

        match event {
            UiEvent::MouseMove { position, .. } => {
                let next = self.control_at(*position);
                if next != self.hovered_control {
                    self.hovered_control = next;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if let Some(control) = self.control_at(*position) {
                    self.pressed_control = Some(control);
                    self.hovered_control = Some(control);
                    ctx.request_pointer_capture(self.id);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                if self.is_drag_region(*position) {
                    (ctx.dispatch)(app_shell_window_drag_action());
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                if let Some(control) = self.pressed_control.take() {
                    ctx.release_pointer_capture(self.id);
                    self.hovered_control = self.control_at(*position);
                    if self.hovered_control == Some(control) {
                        (ctx.dispatch)(control.action());
                    }
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::FocusLost => {
                self.hovered_control = None;
                self.pressed_control = None;
                EventResult::Ignored
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        ctx.encoder
            .draw_rect(self.bounds, colors.background.lerp(colors.card, 0.42), 0.0);
        let border_y = self.bounds.y + self.bounds.height - 1.0;
        ctx.encoder.draw_line(
            Point::new(self.bounds.x, border_y),
            Point::new(self.bounds.x + self.bounds.width, border_y),
            1.0,
            colors.border,
        );

        ctx.encoder.draw_text(
            "Mondrian",
            13.0,
            Point::new(self.brand_x(), self.bounds.y + 10.0),
            colors.card_foreground,
        );
        self.menu_bar.paint(ctx);

        let previous_clip = ctx.clip_rect;
        ctx.clip_rect = previous_clip.intersection(&self.title_bounds);
        ctx.push_clip(self.title_bounds);
        ctx.encoder.draw_text(
            &self.title,
            12.0,
            Point::new(self.title_bounds.x, self.title_bounds.y + 10.0),
            colors.muted_foreground,
        );
        ctx.pop_clip();
        ctx.clip_rect = previous_clip;

        self.window_controls.paint(ctx, self.hovered_control, self.pressed_control);
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        self.menu_bar.paint_overlay(ctx);
    }

    fn overlay_hit_test(&self, point: Point) -> bool {
        self.menu_bar.overlay_hit_test(point)
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        1
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        (index == 0).then_some(&self.menu_bar as &dyn Widget)
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        (index == 0).then_some(&mut self.menu_bar as &mut dyn Widget)
    }
}

impl TitleBar {
    fn brand_x(&self) -> f32 {
        match self.window_controls.edge() {
            WindowControlEdge::Leading => {
                self.window_controls.bounds().x + self.window_controls.bounds().width + 8.0
            }
            WindowControlEdge::Trailing => self.bounds.x + 12.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::app_shell_quit_action;
    use crate::self_hosted::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::{DrawCommandEncoder, EventRequests, PointerCaptureRequest};
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Recorder {
        rects: Vec<Rect>,
        lines: usize,
        texts: Vec<String>,
    }

    impl DrawCommandEncoder for Recorder {
        fn push_clip(&mut self, _bounds: Rect) {}
        fn pop_clip(&mut self) {}
        fn draw_rect(&mut self, bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {
            self.rects.push(bounds);
        }
        fn draw_line(
            &mut self,
            _start: Point,
            _end: Point,
            _width: f32,
            _color: mondrian_core::Color,
        ) {
            self.lines += 1;
        }
        fn draw_text(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.to_owned());
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn title_bar() -> TitleBar {
        let mut bar = TitleBar::new("Demo Project", MenuBar::default());
        bar.layout(Rect::new(0.0, 0.0, 1000.0, TITLE_BAR_HEIGHT));
        bar
    }

    fn title_bar_with_style(style: PlatformWindowControlStyle) -> TitleBar {
        let mut bar =
            TitleBar::new_with_window_control_style("Demo Project", MenuBar::default(), style);
        bar.layout(Rect::new(0.0, 0.0, 1000.0, TITLE_BAR_HEIGHT));
        bar
    }

    #[test]
    fn title_bar_lays_out_menu_and_window_controls_in_one_row() {
        let bar = title_bar();

        assert_eq!(bar.bounds(), Rect::new(0.0, 0.0, 1000.0, TITLE_BAR_HEIGHT));
        assert_eq!(bar.menu_bar().bounds().y, 3.0);
        assert!(bar.menu_bar().bounds().x > 90.0);
        assert_eq!(
            bar.control_bounds(WindowControl::Close).width,
            PlatformWindowControlStyle::current().total_width() / 3.0
        );
        assert!(bar.control_bounds(WindowControl::Close).x > bar.menu_bar().bounds().x);
    }

    #[test]
    fn title_bar_offsets_content_for_macos_leading_window_controls() {
        let bar = title_bar_with_style(PlatformWindowControlStyle::MacOs);

        assert!(bar.control_bounds(WindowControl::Close).x < bar.menu_bar().bounds().x);
        assert!(bar.menu_bar().bounds().x >= PlatformWindowControlStyle::MacOs.total_width());
        assert!(bar.is_drag_region(Point::new(860.0, 14.0)));
    }

    #[test]
    fn title_bar_window_control_dispatches_app_shell_command_on_release() {
        let mut bar = title_bar();
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcuts,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        let close = bar.control_bounds(WindowControl::Close).center();

        assert_eq!(
            bar.event(
                &UiEvent::MouseDown {
                    position: close,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(bar.id()))
        );
        assert_eq!(
            bar.event(
                &UiEvent::MouseUp {
                    position: close,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[app_shell_quit_action()]);
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(bar.id()))
        );
    }

    #[test]
    fn title_bar_drag_region_dispatches_native_drag_request() {
        let mut bar = title_bar();
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcuts,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = bar.event(
            &UiEvent::MouseDown {
                position: Point::new(740.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(
            actions.borrow().as_slice(),
            &[app_shell_window_drag_action()]
        );
    }

    #[test]
    fn title_bar_paints_platform_window_controls() {
        let bar = title_bar();
        let mut recorder = Recorder::default();
        let theme = ThemePreset::Dark.build();
        let mut ctx = PaintContext {
            encoder: &mut recorder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 1000.0, 200.0),
        };

        bar.paint(&mut ctx);

        assert!(recorder.texts.iter().any(|text| text == "Mondrian"));
        assert!(recorder.texts.iter().any(|text| text == "Demo Project"));
        assert!(
            recorder.lines >= 7,
            "window controls should be painted as platform geometry"
        );
    }
}
