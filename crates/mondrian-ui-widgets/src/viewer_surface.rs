//! Domain-light viewer surface for editor preview panels.
//!
//! The widget paints preview chrome, aspect-ratio fitting, an optional raster
//! preview frame, safe-area guides, and status metadata. App/runtime layers own
//! preview decoding and pass already-renderable frame images across this
//! domain-light boundary.

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use std::sync::Arc;

use crate::paint::{
    color_with_alpha, horizontal_stroke_rect, mix_color, soft_border, vertical_stroke_rect,
};
use crate::text_metrics::measure_single_line;
use crate::RasterImage;

const DEFAULT_WIDTH: f32 = 480.0;
const DEFAULT_HEIGHT: f32 = 270.0;
const TRANSPORT_BUTTON_SIZE: f32 = 26.0;
const TRANSPORT_BUTTON_GAP: f32 = 8.0;

/// RGBA preview image presented by [`ViewerSurface`].
pub type ViewerFrameImage = RasterImage;

/// Semantic tone for the viewer status badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerStatusTone {
    /// Neutral idle/ready state.
    Neutral,
    /// Active playback or focused preview state.
    Accent,
    /// Successful/healthy state.
    Success,
    /// Attention-needed preview state.
    Warning,
    /// Error/unavailable preview state.
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerControl {
    StepBack,
    PlayPause,
    StepForward,
}

/// Preview viewer surface.
pub struct ViewerSurface {
    id: WidgetId,
    bounds: Rect,
    title: String,
    status: String,
    status_tone: ViewerStatusTone,
    resolution_label: String,
    frame_label: String,
    duration_label: String,
    source_width: u32,
    source_height: u32,
    playing: bool,
    enabled: bool,
    frame_image: Option<ViewerFrameImage>,
    empty_message: Option<String>,
    hovered_control: Option<ViewerControl>,
    pressed_control: Option<ViewerControl>,
}

impl ViewerSurface {
    /// Create a viewer surface with a title and source dimensions.
    pub fn new(title: impl Into<String>, source_width: u32, source_height: u32) -> Self {
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            title: title.into(),
            status: "No signal".into(),
            status_tone: ViewerStatusTone::Neutral,
            resolution_label: String::new(),
            frame_label: "F0".into(),
            duration_label: String::new(),
            source_width: source_width.max(1),
            source_height: source_height.max(1),
            playing: false,
            enabled: true,
            frame_image: None,
            empty_message: None,
            hovered_control: None,
            pressed_control: None,
        }
    }

    /// Set the viewer status label.
    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = status.into();
        self
    }

    /// Set the semantic tone used for the viewer status badge.
    pub fn with_status_tone(mut self, tone: ViewerStatusTone) -> Self {
        self.status_tone = tone;
        self
    }

    /// Set the formatted source resolution label.
    pub fn with_resolution_label(mut self, label: impl Into<String>) -> Self {
        self.resolution_label = label.into();
        self
    }

    /// Set the formatted current frame label.
    pub fn with_frame_label(mut self, label: impl Into<String>) -> Self {
        self.frame_label = label.into();
        self
    }

    /// Set the formatted duration label.
    pub fn with_duration_label(mut self, label: impl Into<String>) -> Self {
        self.duration_label = label.into();
        self
    }

    /// Set whether playback is active.
    pub fn playing(mut self, playing: bool) -> Self {
        self.playing = playing;
        if playing && self.status_tone == ViewerStatusTone::Neutral {
            self.status_tone = ViewerStatusTone::Accent;
        }
        self
    }

    /// Set whether the surface represents an available preview target.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Mark the surface as unavailable.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Set the rendered preview image shown inside the fitted canvas.
    pub fn with_frame_image(mut self, frame_image: ViewerFrameImage) -> Self {
        self.frame_image = Some(frame_image);
        self
    }

    /// Set a short message painted inside the canvas when no frame is shown.
    pub fn with_empty_message(mut self, message: impl Into<String>) -> Self {
        self.empty_message = Some(message.into());
        self
    }

    /// Whether the surface represents an available preview target.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn aspect_ratio(&self) -> f32 {
        (self.source_width as f32 / self.source_height as f32).clamp(0.1, 10.0)
    }

    fn canvas_rect(&self) -> Rect {
        let chrome_top = 42.0;
        let chrome_bottom = 44.0;
        let padding = 16.0;
        let available = Rect::new(
            self.bounds.x + padding,
            self.bounds.y + chrome_top,
            (self.bounds.width - padding * 2.0).max(0.0),
            (self.bounds.height - chrome_top - chrome_bottom).max(0.0),
        );
        fit_aspect(available, self.aspect_ratio())
    }

    fn metadata_text(&self) -> String {
        let mut parts = Vec::new();
        if !self.resolution_label.is_empty() {
            parts.push(self.resolution_label.as_str());
        }
        parts.push(self.frame_label.as_str());
        if !self.duration_label.is_empty() {
            parts.push(self.duration_label.as_str());
        }
        parts.join("  |  ")
    }

    fn control_strip_rect(&self) -> Rect {
        let width = TRANSPORT_BUTTON_SIZE * 3.0 + TRANSPORT_BUTTON_GAP * 2.0;
        Rect::new(
            self.bounds.x + (self.bounds.width - width) * 0.5,
            self.bounds.y + self.bounds.height - 34.0,
            width,
            TRANSPORT_BUTTON_SIZE,
        )
    }

    fn control_rect(&self, control: ViewerControl) -> Rect {
        let strip = self.control_strip_rect();
        let index = match control {
            ViewerControl::StepBack => 0.0,
            ViewerControl::PlayPause => 1.0,
            ViewerControl::StepForward => 2.0,
        };
        Rect::new(
            strip.x + index * (TRANSPORT_BUTTON_SIZE + TRANSPORT_BUTTON_GAP),
            strip.y,
            TRANSPORT_BUTTON_SIZE,
            TRANSPORT_BUTTON_SIZE,
        )
    }

    fn control_at(&self, point: Point) -> Option<ViewerControl> {
        [
            ViewerControl::StepBack,
            ViewerControl::PlayPause,
            ViewerControl::StepForward,
        ]
        .into_iter()
        .find(|control| self.control_rect(*control).contains(point))
    }

    fn dispatch_control(&self, control: ViewerControl, ctx: &mut EventContext) {
        let action = match control {
            ViewerControl::StepBack => Action::StepBack,
            ViewerControl::PlayPause => Action::TogglePlay,
            ViewerControl::StepForward => Action::StepForward,
        };
        (ctx.dispatch)(action);
    }
}

impl Widget for ViewerSurface {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(DEFAULT_WIDTH, DEFAULT_HEIGHT))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            self.hovered_control = None;
            self.pressed_control = None;
            return EventResult::Ignored;
        }

        match event {
            UiEvent::MouseMove { position, .. } => {
                let hovered = self.control_at(*position);
                if hovered != self.hovered_control {
                    self.hovered_control = hovered;
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                hovered.map_or(EventResult::Ignored, |_| EventResult::Handled)
            }
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if let Some(control) = self.control_at(*position) {
                    self.pressed_control = Some(control);
                    self.hovered_control = Some(control);
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                let pressed = self.pressed_control.take();
                let hovered = self.control_at(*position);
                self.hovered_control = hovered;
                if let Some(control) = pressed {
                    if hovered == Some(control) {
                        self.dispatch_control(control, ctx);
                    }
                    ctx.request_repaint();
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::FocusLost => {
                self.hovered_control = None;
                self.pressed_control = None;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let typography = &ctx.theme.typography;
        let canvas = self.canvas_rect();

        ctx.encoder.draw_rect(self.bounds, colors.background, 0.0);
        ctx.encoder.draw_text_box(
            &self.title,
            typography.body.font_size,
            Point::new(self.bounds.x + 14.0, self.bounds.y + 12.0),
            (self.bounds.width - 120.0).max(32.0),
            if self.enabled {
                colors.foreground
            } else {
                colors.muted_foreground
            },
        );

        let status_width = (measure_single_line(&self.status, typography.small.font_size).0 + 24.0)
            .clamp(64.0, (self.bounds.width - 32.0).max(64.0));
        let badge = Rect::new(
            self.bounds.x + self.bounds.width - status_width - 16.0,
            self.bounds.y + 9.0,
            status_width,
            24.0,
        );
        let (badge_fill, badge_text) = status_badge_colors(self, ctx);
        ctx.encoder.draw_rect(badge, soft_border(colors.border), spacing.radius_sm);
        ctx.encoder
            .draw_rect(badge.inset(1.0, 1.0), badge_fill, spacing.radius_sm - 1.0);
        ctx.push_clip(badge.inset(4.0, 0.0));
        ctx.encoder.draw_text(
            &self.status,
            typography.small.font_size,
            Point::new(badge.x + 8.0, badge.y + 5.0),
            badge_text,
        );
        ctx.pop_clip();

        ctx.encoder.draw_rect(
            canvas.inset(-1.0, -1.0),
            soft_border(colors.border),
            spacing.radius_md,
        );
        let canvas_fill = if self.enabled {
            mix_color(colors.card, colors.foreground, 0.035)
        } else {
            mix_color(colors.card, colors.muted, 0.34)
        };
        ctx.encoder.draw_rect(canvas, canvas_fill, spacing.radius_md);
        if self.enabled {
            if let Some(frame) = &self.frame_image {
                ctx.push_clip(canvas);
                ctx.encoder.draw_raster_image(
                    &frame.key,
                    canvas,
                    frame.width,
                    frame.height,
                    Arc::clone(&frame.rgba),
                    Color::WHITE,
                );
                ctx.pop_clip();
            }
        }
        if self.frame_image.is_none() {
            if let Some(message) =
                self.empty_message.as_deref().filter(|message| !message.is_empty())
            {
                let message_rect = Rect::new(
                    canvas.x + 12.0,
                    canvas.y + (canvas.height - 22.0) * 0.5,
                    (canvas.width - 24.0).max(1.0),
                    22.0,
                );
                ctx.push_clip(canvas);
                ctx.encoder.draw_text_box(
                    message,
                    typography.body.font_size,
                    Point::new(message_rect.x, message_rect.y),
                    message_rect.width,
                    colors.muted_foreground,
                );
                ctx.pop_clip();
            }
        }
        paint_safe_guides(ctx, canvas, self.enabled);

        self.paint_transport_controls(ctx);

        let metadata = self.metadata_text();
        let control_strip = self.control_strip_rect();
        ctx.encoder.draw_text_box(
            &metadata,
            typography.small.font_size,
            Point::new(
                self.bounds.x + 14.0,
                self.bounds.y + self.bounds.height - 24.0,
            ),
            (control_strip.x - self.bounds.x - 28.0).max(1.0),
            colors.muted_foreground,
        );
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

impl ViewerSurface {
    fn paint_transport_controls(&self, ctx: &mut PaintContext) {
        for control in [
            ViewerControl::StepBack,
            ViewerControl::PlayPause,
            ViewerControl::StepForward,
        ] {
            self.paint_transport_control(ctx, control);
        }
    }

    fn paint_transport_control(&self, ctx: &mut PaintContext, control: ViewerControl) {
        let colors = &ctx.theme.colors;
        let radius = ctx.theme.spacing.radius_sm;
        let rect = self.control_rect(control);
        let pressed = self.pressed_control == Some(control);
        let hovered = self.hovered_control == Some(control);
        let bg = if !self.enabled || pressed {
            colors.muted
        } else if hovered {
            colors.accent
        } else {
            colors.card
        };
        let icon = if self.enabled {
            colors.foreground
        } else {
            colors.muted_foreground
        };

        ctx.encoder.draw_rect(rect, bg, radius);
        match control {
            ViewerControl::StepBack => {
                ctx.encoder.draw_rect(
                    Rect::new(rect.x + 7.0, rect.y + 7.0, 2.0, 12.0),
                    color_with_alpha(icon, 0.9),
                    1.0,
                );
                ctx.encoder.draw_triangles(
                    &[
                        Point::new(rect.x + 18.0, rect.y + 6.0),
                        Point::new(rect.x + 18.0, rect.y + 20.0),
                        Point::new(rect.x + 9.0, rect.y + 13.0),
                    ],
                    icon,
                );
            }
            ViewerControl::PlayPause if self.playing => {
                ctx.encoder
                    .draw_rect(Rect::new(rect.x + 8.0, rect.y + 7.0, 3.0, 12.0), icon, 1.0);
                ctx.encoder
                    .draw_rect(Rect::new(rect.x + 15.0, rect.y + 7.0, 3.0, 12.0), icon, 1.0);
            }
            ViewerControl::PlayPause => {
                ctx.encoder.draw_triangles(
                    &[
                        Point::new(rect.x + 10.0, rect.y + 6.0),
                        Point::new(rect.x + 10.0, rect.y + 20.0),
                        Point::new(rect.x + 19.0, rect.y + 13.0),
                    ],
                    icon,
                );
            }
            ViewerControl::StepForward => {
                ctx.encoder.draw_triangles(
                    &[
                        Point::new(rect.x + 8.0, rect.y + 6.0),
                        Point::new(rect.x + 8.0, rect.y + 20.0),
                        Point::new(rect.x + 17.0, rect.y + 13.0),
                    ],
                    icon,
                );
                ctx.encoder.draw_rect(
                    Rect::new(rect.x + 17.0, rect.y + 7.0, 2.0, 12.0),
                    color_with_alpha(icon, 0.9),
                    1.0,
                );
            }
        }
    }
}

fn fit_aspect(bounds: Rect, aspect: f32) -> Rect {
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return Rect::new(bounds.x, bounds.y, 0.0, 0.0);
    }
    let available_aspect = bounds.width / bounds.height;
    if available_aspect > aspect {
        let width = bounds.height * aspect;
        Rect::new(
            bounds.x + (bounds.width - width) * 0.5,
            bounds.y,
            width,
            bounds.height,
        )
    } else {
        let height = bounds.width / aspect;
        Rect::new(
            bounds.x,
            bounds.y + (bounds.height - height) * 0.5,
            bounds.width,
            height,
        )
    }
}

fn status_badge_colors(surface: &ViewerSurface, ctx: &PaintContext) -> (Color, Color) {
    let colors = &ctx.theme.colors;
    if !surface.enabled {
        return (
            mix_color(colors.popover, colors.muted, 0.28),
            colors.muted_foreground,
        );
    }
    match surface.status_tone {
        ViewerStatusTone::Neutral => (
            mix_color(colors.popover, colors.foreground, 0.045),
            colors.popover_foreground,
        ),
        ViewerStatusTone::Accent => (
            mix_color(colors.popover, colors.primary, 0.18),
            colors.primary,
        ),
        ViewerStatusTone::Success => (color_with_alpha(colors.success, 0.20), colors.success),
        ViewerStatusTone::Warning => (color_with_alpha(colors.warning, 0.20), colors.warning),
        ViewerStatusTone::Error => (color_with_alpha(colors.error, 0.20), colors.error),
    }
}

fn paint_safe_guides(ctx: &mut PaintContext, canvas: Rect, enabled: bool) {
    if canvas.width <= 0.0 || canvas.height <= 0.0 {
        return;
    }
    let colors = &ctx.theme.colors;
    let guide = color_with_alpha(colors.border, if enabled { 0.46 } else { 0.28 });
    let action = canvas.inset(canvas.width * 0.05, canvas.height * 0.05);
    let title = canvas.inset(canvas.width * 0.10, canvas.height * 0.10);
    draw_rect_outline(ctx, action, guide);
    draw_rect_outline(ctx, title, color_with_alpha(guide, 0.72));
}

fn draw_rect_outline(ctx: &mut PaintContext, rect: Rect, color: Color) {
    ctx.encoder.draw_rect(
        horizontal_stroke_rect(rect.y, rect.x, rect.width, 1.0),
        color,
        0.0,
    );
    ctx.encoder.draw_rect(
        horizontal_stroke_rect(rect.y + rect.height, rect.x, rect.width, 1.0),
        color,
        0.0,
    );
    ctx.encoder.draw_rect(
        vertical_stroke_rect(rect.x, rect.y, rect.height, 1.0),
        color,
        0.0,
    );
    ctx.encoder.draw_rect(
        vertical_stroke_rect(rect.x + rect.width, rect.y, rect.height, 1.0),
        color,
        0.0,
    );
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
        rects: Vec<Rect>,
        rect_colors: Vec<Color>,
        lines: usize,
        triangles: usize,
        texts: Vec<String>,
        raster_images: Vec<(String, Rect, u32, u32)>,
        clips: Vec<Rect>,
        clip_pops: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }
        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }
        fn draw_rect(&mut self, bounds: Rect, color: Color, _corner_radius: f32) {
            self.rects.push(bounds);
            self.rect_colors.push(color);
        }
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {
            self.lines += 1;
        }
        fn draw_triangles(&mut self, vertices: &[Point], _color: Color) {
            self.triangles += vertices.len();
        }
        fn draw_text(&mut self, text: &str, _font_size: f32, _position: Point, _color: Color) {
            self.texts.push(text.into());
        }
        fn draw_text_box(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _max_width: f32,
            _color: Color,
        ) {
            self.texts.push(text.into());
        }
        fn draw_raster_image(
            &mut self,
            key: &str,
            bounds: Rect,
            width: u32,
            height: u32,
            _rgba: Arc<[u8]>,
            _tint: Color,
        ) {
            self.raster_images.push((key.to_owned(), bounds, width, height));
        }
        fn push_translate(&mut self, _offset: glam::Vec2) {}
        fn pop_transform(&mut self) {}
    }

    #[test]
    fn canvas_preserves_source_aspect_ratio() {
        let mut viewer = ViewerSurface::new("Demo", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));

        let canvas = viewer.canvas_rect();

        assert!((canvas.width / canvas.height - 16.0 / 9.0).abs() < 0.001);
        assert!(canvas.x >= 16.0);
        assert!(canvas.y >= 42.0);
    }

    #[test]
    fn paint_draws_chrome_metadata_and_safe_guides() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080)
            .with_status("Playing")
            .with_resolution_label("1920x1080")
            .with_frame_label("F42")
            .with_duration_label("240 frames")
            .playing(true);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        assert!(encoder.texts.iter().any(|text| text == "Scene 01"));
        assert!(encoder.texts.iter().any(|text| text == "Playing"));
        assert!(encoder.texts.iter().any(|text| text.contains("1920x1080")));
        assert!(encoder.texts.iter().any(|text| text.contains("F42")));
        assert!(
            encoder
                .rect_colors
                .iter()
                .any(|color| *color == mix_color(theme.colors.popover, theme.colors.primary, 0.18)),
            "playing status should use the theme primary token"
        );
        assert_eq!(encoder.lines, 0);
        assert!(encoder.rects.len() >= 12);
    }

    #[test]
    fn disabled_empty_viewer_paints_empty_message_without_frame() {
        let mut viewer = ViewerSurface::new("Viewer", 16, 9)
            .with_status("No sequence")
            .with_empty_message("No sequence loaded")
            .disabled();
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        assert!(encoder.raster_images.is_empty());
        assert!(encoder.texts.iter().any(|text| text == "No sequence"));
        assert!(encoder.texts.iter().any(|text| text == "No sequence loaded"));
        assert_eq!(encoder.clip_pops, encoder.clips.len());
    }

    #[test]
    fn disabled_builder_marks_surface_unavailable() {
        let viewer = ViewerSurface::new("Offline", 1920, 1080).disabled();

        assert!(!viewer.is_enabled());
    }

    #[test]
    fn transport_controls_dispatch_playback_actions() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        for control in [
            ViewerControl::StepBack,
            ViewerControl::PlayPause,
            ViewerControl::StepForward,
        ] {
            let position = viewer.control_rect(control).center();
            assert_eq!(
                viewer.event(
                    &UiEvent::MouseDown {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                ),
                EventResult::Handled
            );
            assert_eq!(
                viewer.event(
                    &UiEvent::MouseUp {
                        position,
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                ),
                EventResult::Handled
            );
        }

        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::StepBack, Action::TogglePlay, Action::StepForward]
        );
    }

    #[test]
    fn disabled_viewer_transport_controls_ignore_input() {
        let mut viewer = ViewerSurface::new("Offline", 1920, 1080).disabled();
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let position = viewer.control_rect(ViewerControl::PlayPause).center();
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            viewer.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn playing_viewer_paints_pause_transport_icon() {
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080).playing(true);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        assert_eq!(encoder.triangles, 6, "step buttons still paint triangles");
        assert!(
            encoder.rects.len() >= 18,
            "playing transport should add pause-bar geometry"
        );
    }

    #[test]
    fn frame_image_rejects_invalid_rgba_payloads() {
        assert!(ViewerFrameImage::new("bad", 2, 2, vec![255; 15]).is_none());
        assert!(ViewerFrameImage::new("empty", 0, 2, Vec::<u8>::new()).is_none());
    }

    #[test]
    fn paint_draws_preview_frame_inside_canvas_clip() {
        let image =
            ViewerFrameImage::new("preview:42", 2, 2, vec![255; 16]).expect("valid preview image");
        let mut viewer = ViewerSurface::new("Scene 01", 1920, 1080).with_frame_image(image);
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let canvas = viewer.canvas_rect();
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        assert_eq!(
            encoder.raster_images,
            vec![("preview:42".to_owned(), canvas, 2, 2)]
        );
        assert!(
            encoder.clips.contains(&canvas),
            "preview image must be clipped to the fitted canvas"
        );
        assert_eq!(encoder.clip_pops, encoder.clips.len());
    }

    #[test]
    fn disabled_viewer_does_not_draw_preview_frame() {
        let image = ViewerFrameImage::new("preview:disabled", 2, 2, vec![255; 16])
            .expect("valid preview image");
        let mut viewer =
            ViewerSurface::new("Scene 01", 1920, 1080).with_frame_image(image).disabled();
        viewer.layout(Rect::new(0.0, 0.0, 500.0, 320.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 500.0, 320.0),
        };

        viewer.paint(&mut ctx);

        assert!(encoder.raster_images.is_empty());
    }
}
