//! Domain-light viewer surface for editor preview panels.
//!
//! The widget paints preview chrome, aspect-ratio fitting, an optional raster
//! preview frame, safe-area guides, and status metadata. App/runtime layers own
//! preview decoding and pass already-renderable frame images across this
//! domain-light boundary.

use mondrian_core::Color;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use std::sync::Arc;

use crate::paint::{
    color_with_alpha, horizontal_stroke_rect, mix_color, soft_border, vertical_stroke_rect,
};

const DEFAULT_WIDTH: f32 = 480.0;
const DEFAULT_HEIGHT: f32 = 270.0;

/// RGBA preview image presented by [`ViewerSurface`].
#[derive(Debug, Clone)]
pub struct ViewerFrameImage {
    /// Stable image cache key for the renderer-owned raster atlas.
    pub key: String,
    /// Source image width in pixels.
    pub width: u32,
    /// Source image height in pixels.
    pub height: u32,
    /// RGBA8 pixels, row-major, `width * height * 4` bytes.
    pub rgba: Arc<[u8]>,
}

impl ViewerFrameImage {
    /// Create a preview frame image. Returns `None` for invalid dimensions or
    /// byte lengths so callers cannot silently poison the renderer atlas.
    pub fn new(
        key: impl Into<String>,
        width: u32,
        height: u32,
        rgba: impl Into<Arc<[u8]>>,
    ) -> Option<Self> {
        let rgba = rgba.into();
        let expected = width.checked_mul(height)?.checked_mul(4)? as usize;
        (width > 0 && height > 0 && rgba.len() == expected).then(|| Self {
            key: key.into(),
            width,
            height,
            rgba,
        })
    }
}

/// Preview viewer surface.
pub struct ViewerSurface {
    id: WidgetId,
    bounds: Rect,
    title: String,
    status: String,
    resolution_label: String,
    frame_label: String,
    duration_label: String,
    source_width: u32,
    source_height: u32,
    playing: bool,
    enabled: bool,
    frame_image: Option<ViewerFrameImage>,
}

impl ViewerSurface {
    /// Create a viewer surface with a title and source dimensions.
    pub fn new(title: impl Into<String>, source_width: u32, source_height: u32) -> Self {
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            title: title.into(),
            status: "No signal".into(),
            resolution_label: String::new(),
            frame_label: "F0".into(),
            duration_label: String::new(),
            source_width: source_width.max(1),
            source_height: source_height.max(1),
            playing: false,
            enabled: true,
            frame_image: None,
        }
    }

    /// Set the viewer status label.
    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = status.into();
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

    /// Whether the surface represents an available preview target.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn aspect_ratio(&self) -> f32 {
        (self.source_width as f32 / self.source_height as f32).clamp(0.1, 10.0)
    }

    fn canvas_rect(&self) -> Rect {
        let chrome_top = 42.0;
        let chrome_bottom = 34.0;
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

    fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
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

        let badge = Rect::new(
            self.bounds.x + self.bounds.width - 104.0,
            self.bounds.y + 9.0,
            88.0,
            24.0,
        );
        let badge_fill = if self.playing {
            mix_color(colors.popover, colors.primary, 0.18)
        } else {
            mix_color(colors.popover, colors.foreground, 0.045)
        };
        ctx.encoder.draw_rect(badge, soft_border(colors.border), spacing.radius_sm);
        ctx.encoder
            .draw_rect(badge.inset(1.0, 1.0), badge_fill, spacing.radius_sm - 1.0);
        ctx.push_clip(badge.inset(4.0, 0.0));
        ctx.encoder.draw_text(
            &self.status,
            typography.small.font_size,
            Point::new(badge.x + 8.0, badge.y + 5.0),
            if self.enabled {
                colors.popover_foreground
            } else {
                colors.muted_foreground
            },
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
        paint_safe_guides(ctx, canvas, self.enabled);

        let metadata = self.metadata_text();
        ctx.encoder.draw_text_box(
            &metadata,
            typography.small.font_size,
            Point::new(
                self.bounds.x + 14.0,
                self.bounds.y + self.bounds.height - 24.0,
            ),
            (self.bounds.width - 28.0).max(1.0),
            colors.muted_foreground,
        );
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
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
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
        lines: usize,
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
        fn draw_rect(&mut self, bounds: Rect, _color: Color, _corner_radius: f32) {
            self.rects.push(bounds);
        }
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {
            self.lines += 1;
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
        assert_eq!(encoder.lines, 0);
        assert!(encoder.rects.len() >= 12);
    }

    #[test]
    fn disabled_builder_marks_surface_unavailable() {
        let viewer = ViewerSurface::new("Offline", 1920, 1080).disabled();

        assert!(!viewer.is_enabled());
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
