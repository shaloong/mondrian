//! Shared test utilities for mondrian-ui-widgets tests.

use glam::Vec2;
use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_platform_core::NoopPlatformService;
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::shortcut::{
    ShortcutBinding, ShortcutContext, ShortcutManager, ShortcutScope,
};
use mondrian_ui_core::tooltip::{TooltipManager, TooltipState};
use mondrian_ui_core::types::{KeyCode, Modifiers, Point, Rect, WidgetId};
use mondrian_ui_core::widget::{DrawCommandEncoder, EventContext, EventRequests, PaintContext};
use mondrian_ui_core::Widget;
use mondrian_ui_theme::{Theme, ThemePreset};

use crate::VectorIcon;

#[derive(Debug, Clone)]
pub(crate) struct TextCommand {
    pub text: String,
    pub position: Point,
    pub max_width: Option<f32>,
}

#[derive(Default)]
pub(crate) struct RecordingEncoder {
    pub rects: Vec<Rect>,
    pub clips: Vec<Rect>,
    pub lines: Vec<(Point, Point, f32)>,
    pub triangle_batches: Vec<Vec<Point>>,
    pub raster_images: Vec<Rect>,
    pub colored_triangle_batches: Vec<Vec<(Point, Color)>>,
    pub texts: Vec<TextCommand>,
    clip_depth: i32,
    transform_depth: i32,
}

impl RecordingEncoder {
    pub(crate) fn assert_balanced_and_finite(&self) {
        assert_eq!(self.clip_depth, 0, "paint leaked clip stack entries");
        assert_eq!(
            self.transform_depth, 0,
            "paint leaked transform stack entries"
        );
        for rect in &self.rects {
            assert_rect_finite(*rect);
        }
        for clip in &self.clips {
            assert_rect_finite(*clip);
        }
        for (start, end, width) in &self.lines {
            assert_point_finite(*start);
            assert_point_finite(*end);
            assert!(width.is_finite());
        }
        for batch in &self.triangle_batches {
            for point in batch {
                assert_point_finite(*point);
            }
        }
        for rect in &self.raster_images {
            assert_rect_finite(*rect);
        }
        for batch in &self.colored_triangle_batches {
            for (point, _) in batch {
                assert_point_finite(*point);
            }
        }
        for text in &self.texts {
            assert_point_finite(text.position);
            if let Some(max_width) = text.max_width {
                assert!(max_width.is_finite());
                assert!(max_width > 0.0);
            }
        }
    }
}

impl DrawCommandEncoder for RecordingEncoder {
    fn push_clip(&mut self, bounds: Rect) {
        assert_rect_finite(bounds);
        self.clips.push(bounds);
        self.clip_depth += 1;
    }

    fn pop_clip(&mut self) {
        self.clip_depth -= 1;
        assert!(self.clip_depth >= 0, "paint popped an empty clip stack");
    }

    fn draw_rect(&mut self, bounds: Rect, _color: Color, _corner_radius: f32) {
        self.rects.push(bounds);
    }

    fn draw_gradient_rect(&mut self, bounds: Rect, _colors: [Color; 4], _corner_radius: f32) {
        self.rects.push(bounds);
    }

    fn draw_line(&mut self, start: Point, end: Point, width: f32, _color: Color) {
        self.lines.push((start, end, width));
    }

    fn draw_triangles(&mut self, vertices: &[Point], _color: Color) {
        self.triangle_batches.push(vertices.to_vec());
    }

    fn draw_raster_image(
        &mut self,
        _key: &str,
        bounds: Rect,
        _width: u32,
        _height: u32,
        _rgba: std::sync::Arc<[u8]>,
        _tint: Color,
    ) {
        self.raster_images.push(bounds);
    }

    fn draw_colored_triangles(&mut self, vertices: &[(Point, Color)]) {
        self.colored_triangle_batches.push(vertices.to_vec());
    }

    fn draw_colored_triangles_in_rect(
        &mut self,
        vertices: &[(Point, Color)],
        mask_bounds: Rect,
        _corner_radius: f32,
    ) {
        assert_rect_finite(mask_bounds);
        self.colored_triangle_batches.push(vertices.to_vec());
    }

    fn draw_text(&mut self, text: &str, _font_size: f32, position: Point, _color: Color) {
        self.texts
            .push(TextCommand { text: text.to_string(), position, max_width: None });
    }

    fn draw_text_box(
        &mut self,
        text: &str,
        _font_size: f32,
        position: Point,
        max_width: f32,
        _color: Color,
    ) {
        self.texts.push(TextCommand {
            text: text.to_string(),
            position,
            max_width: Some(max_width),
        });
    }

    fn push_translate(&mut self, offset: Vec2) {
        assert!(offset.x.is_finite());
        assert!(offset.y.is_finite());
        self.transform_depth += 1;
    }

    fn pop_transform(&mut self) {
        self.transform_depth -= 1;
        assert!(
            self.transform_depth >= 0,
            "paint popped an empty transform stack"
        );
    }
}

pub(crate) fn theme() -> Theme {
    ThemePreset::Dark.build()
}

pub(crate) fn paint_widget(widget: &dyn Widget, clip_rect: Rect) -> RecordingEncoder {
    let theme = theme();
    let mut encoder = RecordingEncoder::default();
    {
        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        widget.paint(&mut ctx);
        widget.paint_overlay(&mut ctx);
    }
    encoder.assert_balanced_and_finite();
    encoder
}

pub(crate) fn assert_rect_finite(rect: Rect) {
    assert!(rect.x.is_finite());
    assert!(rect.y.is_finite());
    assert!(rect.width.is_finite());
    assert!(rect.height.is_finite());
    assert!(rect.width >= 0.0);
    assert!(rect.height >= 0.0);
}

pub(crate) fn assert_point_finite(point: Point) {
    assert!(point.x.is_finite());
    assert!(point.y.is_finite());
}

pub(crate) fn custom_action(_name: &str) -> Action {
    Action::ToggleFullscreen
}

pub(crate) fn test_icon() -> VectorIcon {
    VectorIcon::from_svg_str(
        r#"<svg viewBox="0 0 16 16" xmlns="http://www.w3.org/2000/svg">
            <rect x="2" y="2" width="12" height="12" fill="black"/>
        </svg>"#,
    )
    .expect("test icon should parse")
}

pub(crate) struct DummyFocus;
impl FocusManager for DummyFocus {
    fn focused_widget(&self) -> Option<WidgetId> {
        None
    }
    fn focused_panel(&self) -> Option<mondrian_editor_state::state::PanelKind> {
        None
    }
    fn request_focus(&mut self, _: WidgetId) {}
    fn release_focus(&mut self, _: WidgetId) {}
    fn clear_focus(&mut self) {}
}

pub(crate) struct DummyShortcut;
impl ShortcutManager for DummyShortcut {
    fn register(&mut self, _: ShortcutScope, _: ShortcutBinding, _: Action) {}
    fn unregister(&mut self, _: ShortcutScope, _: &ShortcutBinding) {}
    fn resolve(&self, _: KeyCode, _: Modifiers, _: ShortcutContext) -> Option<Action> {
        None
    }
    fn clear_scope(&mut self, _: ShortcutScope) {}
    fn clear_all(&mut self) {}
}

pub(crate) struct DummyTooltip;
impl TooltipManager for DummyTooltip {
    fn show(&mut self, _: String, _: Point) {}
    fn hide(&mut self) {}
    fn current(&self) -> Option<&TooltipState> {
        None
    }
    fn update(&mut self, _: u64) {}
}

pub(crate) fn make_event_ctx<'a>(
    focus: &'a mut dyn FocusManager,
    shortcut: &'a mut dyn ShortcutManager,
    tooltip: &'a mut dyn TooltipManager,
    dispatch: &'a dyn Fn(Action),
) -> EventContext<'a> {
    let requests: &'a mut EventRequests = Box::leak(Box::new(EventRequests::default()));
    EventContext {
        focus,
        shortcut,
        tooltip,
        dispatch,
        platform: &NoopPlatformService,
        requests,
    }
}
