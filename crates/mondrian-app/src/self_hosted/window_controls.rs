//! Platform-aware custom window controls for the self-hosted product chrome.
//!
//! Real OS titlebar buttons cannot be embedded portably while keeping a fully
//! custom title/menu row. This module keeps the self-drawn controls behind a
//! platform style boundary so layout, hit testing, and visual treatment can
//! follow Windows, macOS, and Linux conventions without leaking into `TitleBar`.

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::PaintContext;

use crate::app::ui_actions::{
    app_shell_quit_action, app_shell_window_minimize_action,
    app_shell_window_toggle_maximize_action,
};

/// User-facing window commands exposed by the custom product chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowControl {
    /// Minimize the native window.
    Minimize,
    /// Toggle the native maximized/restored state.
    ToggleMaximize,
    /// Close the application window.
    Close,
}

impl WindowControl {
    pub(crate) fn action(self) -> Action {
        match self {
            Self::Minimize => app_shell_window_minimize_action(),
            Self::ToggleMaximize => app_shell_window_toggle_maximize_action(),
            Self::Close => app_shell_quit_action(),
        }
    }
}

/// Which edge hosts the platform's primary window controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowControlEdge {
    /// Controls sit before the app title/menu content.
    Leading,
    /// Controls sit after the app title/menu content.
    Trailing,
}

/// Compile-time platform treatment for self-drawn window controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlatformWindowControlStyle {
    /// Windows 10/11 style right-aligned rectangular hit targets.
    Windows,
    /// macOS traffic-light controls on the left.
    MacOs,
    /// Conservative Linux client-side decoration style.
    Linux,
}

impl PlatformWindowControlStyle {
    /// Style matching the current target OS.
    pub(crate) const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    pub(crate) const fn edge(self) -> WindowControlEdge {
        match self {
            Self::MacOs => WindowControlEdge::Leading,
            Self::Windows | Self::Linux => WindowControlEdge::Trailing,
        }
    }

    const fn button_count(self) -> usize {
        3
    }

    pub(crate) const fn total_width(self) -> f32 {
        match self {
            Self::Windows => 138.0,
            Self::MacOs => 78.0,
            Self::Linux => 126.0,
        }
    }

    const fn button_width(self) -> f32 {
        match self {
            Self::Windows => 46.0,
            Self::MacOs => 22.0,
            Self::Linux => 42.0,
        }
    }

    const fn order(self) -> [WindowControl; 3] {
        match self {
            Self::MacOs => [
                WindowControl::Close,
                WindowControl::Minimize,
                WindowControl::ToggleMaximize,
            ],
            Self::Windows | Self::Linux => [
                WindowControl::Minimize,
                WindowControl::ToggleMaximize,
                WindowControl::Close,
            ],
        }
    }
}

/// Layout, hit-testing, and painting state for the product window controls.
#[derive(Debug, Clone)]
pub(crate) struct WindowControls {
    style: PlatformWindowControlStyle,
    bounds: Rect,
    control_bounds: [Rect; 3],
}

impl Default for WindowControls {
    fn default() -> Self {
        Self::new(PlatformWindowControlStyle::current())
    }
}

impl WindowControls {
    pub(crate) fn new(style: PlatformWindowControlStyle) -> Self {
        Self {
            style,
            bounds: Rect::ZERO,
            control_bounds: [Rect::ZERO; 3],
        }
    }

    pub(crate) fn edge(&self) -> WindowControlEdge {
        self.style.edge()
    }

    pub(crate) fn total_width(&self) -> f32 {
        self.style.total_width()
    }

    pub(crate) fn bounds(&self) -> Rect {
        self.bounds
    }

    pub(crate) fn control_bounds(&self, control: WindowControl) -> Rect {
        self.style
            .order()
            .into_iter()
            .zip(self.control_bounds)
            .find_map(|(candidate, bounds)| (candidate == control).then_some(bounds))
            .unwrap_or(Rect::ZERO)
    }

    pub(crate) fn layout(&mut self, title_bounds: Rect) {
        let width = self.total_width().min(title_bounds.width);
        let x = match self.edge() {
            WindowControlEdge::Leading => title_bounds.x,
            WindowControlEdge::Trailing => title_bounds.x + title_bounds.width - width,
        };
        self.bounds = Rect::new(x, title_bounds.y, width, title_bounds.height);

        match self.style {
            PlatformWindowControlStyle::MacOs => self.layout_macos(),
            PlatformWindowControlStyle::Windows | PlatformWindowControlStyle::Linux => {
                self.layout_rectangular()
            }
        }
    }

    pub(crate) fn control_at(&self, position: Point) -> Option<WindowControl> {
        self.style
            .order()
            .into_iter()
            .zip(self.control_bounds)
            .find_map(|(control, bounds)| bounds.contains(position).then_some(control))
    }

    pub(crate) fn paint(
        &self,
        ctx: &mut PaintContext,
        hovered_control: Option<WindowControl>,
        pressed_control: Option<WindowControl>,
    ) {
        for control in self.style.order() {
            match self.style {
                PlatformWindowControlStyle::Windows => {
                    self.paint_windows_control(control, ctx, hovered_control, pressed_control)
                }
                PlatformWindowControlStyle::MacOs => {
                    self.paint_macos_control(control, ctx, hovered_control, pressed_control)
                }
                PlatformWindowControlStyle::Linux => {
                    self.paint_linux_control(control, ctx, hovered_control, pressed_control)
                }
            }
        }
    }

    fn layout_rectangular(&mut self) {
        let width = self.style.button_width();
        for index in 0..self.style.button_count() {
            self.control_bounds[index] = Rect::new(
                self.bounds.x + index as f32 * width,
                self.bounds.y,
                width,
                self.bounds.height,
            );
        }
    }

    fn layout_macos(&mut self) {
        let hit = self.style.button_width();
        let start_x = self.bounds.x + 10.0;
        for index in 0..self.style.button_count() {
            self.control_bounds[index] = Rect::new(
                start_x + index as f32 * hit,
                self.bounds.y,
                hit,
                self.bounds.height,
            );
        }
    }

    fn paint_windows_control(
        &self,
        control: WindowControl,
        ctx: &mut PaintContext,
        hovered_control: Option<WindowControl>,
        pressed_control: Option<WindowControl>,
    ) {
        let rect = self.control_bounds(control);
        let colors = &ctx.theme.colors;
        let hovered = hovered_control == Some(control);
        let pressed = pressed_control == Some(control);
        let mut fill = if control == WindowControl::Close && hovered {
            colors.error
        } else if pressed {
            colors.accent
        } else if hovered {
            colors.secondary
        } else {
            colors.background.lerp(colors.card, 0.42)
        };
        if !hovered && !pressed {
            fill.a = 0.0;
        }
        ctx.encoder.draw_rect(rect, fill, 0.0);

        let icon_color = if control == WindowControl::Close && hovered {
            colors.primary_foreground
        } else {
            colors.card_foreground
        };
        paint_geometric_icon(ctx, rect.center(), control, 1.25, icon_color);
    }

    fn paint_linux_control(
        &self,
        control: WindowControl,
        ctx: &mut PaintContext,
        hovered_control: Option<WindowControl>,
        pressed_control: Option<WindowControl>,
    ) {
        let rect = self.control_bounds(control);
        let colors = &ctx.theme.colors;
        let hovered = hovered_control == Some(control);
        let pressed = pressed_control == Some(control);
        let mut fill = if pressed {
            colors.accent
        } else if hovered {
            colors.secondary
        } else {
            colors.background.lerp(colors.card, 0.42)
        };
        if !hovered && !pressed {
            fill.a = 0.0;
        }
        ctx.encoder.draw_rect(rect.inset(4.0, 4.0), fill, 4.0);
        paint_geometric_icon(ctx, rect.center(), control, 1.2, colors.card_foreground);
    }

    fn paint_macos_control(
        &self,
        control: WindowControl,
        ctx: &mut PaintContext,
        hovered_control: Option<WindowControl>,
        pressed_control: Option<WindowControl>,
    ) {
        let rect = self.control_bounds(control);
        let colors = &ctx.theme.colors;
        let hovered = hovered_control == Some(control);
        let pressed = pressed_control == Some(control);
        let center = rect.center();
        let mut fill = match control {
            WindowControl::Close => Color { r: 1.0, g: 0.37, b: 0.34, a: 1.0 },
            WindowControl::Minimize => Color { r: 1.0, g: 0.76, b: 0.25, a: 1.0 },
            WindowControl::ToggleMaximize => Color { r: 0.18, g: 0.78, b: 0.31, a: 1.0 },
        };
        if pressed {
            fill = fill.lerp(colors.background, 0.28);
        }
        ctx.encoder.draw_rect(
            Rect::new(center.x - 6.0, center.y - 6.0, 12.0, 12.0),
            fill,
            6.0,
        );

        if hovered || pressed {
            let glyph = colors.background.lerp(colors.card_foreground, 0.72);
            paint_geometric_icon(ctx, center, control, 1.1, glyph);
        }
    }
}

fn paint_geometric_icon(
    ctx: &mut PaintContext,
    center: Point,
    control: WindowControl,
    line_width: f32,
    color: Color,
) {
    match control {
        WindowControl::Minimize => {
            ctx.encoder.draw_line(
                Point::new(center.x - 5.0, center.y + 1.0),
                Point::new(center.x + 5.0, center.y + 1.0),
                line_width,
                color,
            );
        }
        WindowControl::ToggleMaximize => {
            let r = Rect::new(center.x - 5.0, center.y - 5.0, 10.0, 10.0);
            ctx.encoder
                .draw_line(r.min(), Point::new(r.x + r.width, r.y), line_width, color);
            ctx.encoder
                .draw_line(r.min(), Point::new(r.x, r.y + r.height), line_width, color);
            ctx.encoder.draw_line(
                Point::new(r.x + r.width, r.y),
                Point::new(r.x + r.width, r.y + r.height),
                line_width,
                color,
            );
            ctx.encoder.draw_line(
                Point::new(r.x, r.y + r.height),
                Point::new(r.x + r.width, r.y + r.height),
                line_width,
                color,
            );
        }
        WindowControl::Close => {
            ctx.encoder.draw_line(
                Point::new(center.x - 5.0, center.y - 5.0),
                Point::new(center.x + 5.0, center.y + 5.0),
                line_width,
                color,
            );
            ctx.encoder.draw_line(
                Point::new(center.x + 5.0, center.y - 5.0),
                Point::new(center.x - 5.0, center.y + 5.0),
                line_width,
                color,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::widget::{DrawCommandEncoder, PaintContext};
    use mondrian_ui_theme::ThemePreset;

    #[derive(Default)]
    struct Recorder {
        rects: Vec<Rect>,
        lines: usize,
    }

    impl DrawCommandEncoder for Recorder {
        fn push_clip(&mut self, _bounds: Rect) {}
        fn pop_clip(&mut self) {}
        fn draw_rect(&mut self, bounds: Rect, _color: Color, _corner_radius: f32) {
            self.rects.push(bounds);
        }
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {
            self.lines += 1;
        }
        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}
        fn push_translate(&mut self, _offset: glam::Vec2) {}
        fn pop_transform(&mut self) {}
    }

    fn controls(style: PlatformWindowControlStyle) -> WindowControls {
        let mut controls = WindowControls::new(style);
        controls.layout(Rect::new(0.0, 0.0, 1000.0, 34.0));
        controls
    }

    #[test]
    fn platform_styles_choose_expected_edges_and_widths() {
        assert_eq!(
            PlatformWindowControlStyle::Windows.edge(),
            WindowControlEdge::Trailing
        );
        assert_eq!(
            PlatformWindowControlStyle::MacOs.edge(),
            WindowControlEdge::Leading
        );
        assert_eq!(
            PlatformWindowControlStyle::Linux.edge(),
            WindowControlEdge::Trailing
        );
        assert_eq!(PlatformWindowControlStyle::Windows.total_width(), 138.0);
        assert_eq!(PlatformWindowControlStyle::MacOs.total_width(), 78.0);
    }

    #[test]
    fn windows_controls_are_right_aligned_in_system_order() {
        let controls = controls(PlatformWindowControlStyle::Windows);

        assert_eq!(controls.bounds().x, 862.0);
        assert_eq!(controls.control_bounds(WindowControl::Minimize).x, 862.0);
        assert_eq!(
            controls.control_bounds(WindowControl::ToggleMaximize).x,
            908.0
        );
        assert_eq!(controls.control_bounds(WindowControl::Close).x, 954.0);
        assert_eq!(
            controls.control_at(Point::new(978.0, 16.0)),
            Some(WindowControl::Close)
        );
    }

    #[test]
    fn macos_controls_are_left_aligned_in_traffic_light_order() {
        let controls = controls(PlatformWindowControlStyle::MacOs);

        assert_eq!(controls.bounds().x, 0.0);
        assert_eq!(controls.control_bounds(WindowControl::Close).x, 10.0);
        assert_eq!(controls.control_bounds(WindowControl::Minimize).x, 32.0);
        assert_eq!(
            controls.control_bounds(WindowControl::ToggleMaximize).x,
            54.0
        );
        assert_eq!(
            controls.control_at(Point::new(18.0, 16.0)),
            Some(WindowControl::Close)
        );
    }

    #[test]
    fn linux_controls_use_trailing_compact_hit_targets() {
        let controls = controls(PlatformWindowControlStyle::Linux);

        assert_eq!(controls.bounds().x, 874.0);
        assert_eq!(controls.control_bounds(WindowControl::Minimize).width, 42.0);
        assert_eq!(controls.control_bounds(WindowControl::Close).x, 958.0);
    }

    #[test]
    fn platform_controls_paint_geometry_for_all_styles() {
        for style in [
            PlatformWindowControlStyle::Windows,
            PlatformWindowControlStyle::MacOs,
            PlatformWindowControlStyle::Linux,
        ] {
            let controls = controls(style);
            let mut recorder = Recorder::default();
            let theme = ThemePreset::Dark.build();
            let mut ctx = PaintContext {
                encoder: &mut recorder,
                theme: &theme,
                clip_rect: Rect::new(0.0, 0.0, 1000.0, 100.0),
            };

            controls.paint(
                &mut ctx,
                Some(WindowControl::Close),
                Some(WindowControl::Close),
            );

            assert!(
                recorder.rects.len() >= 3,
                "{style:?} should paint three hit targets or circles"
            );
            assert!(
                recorder.lines >= 2,
                "{style:?} should paint visible glyph geometry"
            );
        }
    }
}
