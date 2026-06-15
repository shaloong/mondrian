//! SVG-backed vector icon geometry.
//!
//! SVG is the designer-facing authoring format. Runtime widgets consume a
//! cached `VectorIcon` mesh produced by lyon tessellation and emit existing
//! triangle draw commands during paint.

use std::fmt;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

use lyon::math::{point, Point as LyonPoint};
use lyon::path::Path;
use lyon::tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertex, LineCap, LineJoin, StrokeOptions,
    StrokeTessellator, StrokeVertex, TessellationError, VertexBuffers,
};
use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;
use svgtypes::{PathParser, PathSegment};

const DEFAULT_STROKE_WIDTH: f32 = 2.0;
const TESSELLATION_TOLERANCE: f32 = 0.08;

static STATIC_SVG_ICON_CACHE: OnceLock<Mutex<std::collections::HashMap<&'static str, VectorIcon>>> =
    OnceLock::new();

/// Parsed, tessellated icon geometry ready for widget painting.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorIcon {
    view_box: Rect,
    meshes: Vec<VectorIconMesh>,
}

#[derive(Debug, Clone, PartialEq)]
struct VectorIconMesh {
    triangles: Vec<[Point; 3]>,
}

/// Error returned when SVG icon data cannot be converted to icon geometry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorIconError {
    message: String,
}

impl VectorIconError {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl fmt::Display for VectorIconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for VectorIconError {}

impl From<TessellationError> for VectorIconError {
    fn from(value: TessellationError) -> Self {
        Self::new(value.to_string())
    }
}

impl VectorIcon {
    /// Create vector icon geometry from an SVG document.
    ///
    /// The supported authoring subset is intentionally designer-friendly for
    /// UI icons: `viewBox`, nested `<g>` containers, and `<path d>` elements
    /// with `fill`, `stroke`, and `stroke-width` attributes. Paths are converted
    /// to lyon paths, then fill and stroke tessellators produce triangle meshes.
    pub fn from_svg_str(svg: &str) -> Result<Self, VectorIconError> {
        let doc =
            roxmltree::Document::parse(svg).map_err(|err| VectorIconError::new(err.to_string()))?;
        let root = doc
            .descendants()
            .find(|node| node.has_tag_name("svg"))
            .ok_or_else(|| VectorIconError::new("SVG document has no <svg> root"))?;
        let view_box = svg_view_box(root)?;
        let mut meshes = Vec::new();

        for node in doc.descendants().filter(|node| node.has_tag_name("path")) {
            let Some(data) = node.attribute("d") else {
                continue;
            };
            let path = svg_path_to_lyon(data)?;
            let stroke_enabled =
                inherited_attr(node, "stroke").is_some_and(|value| value != "none");
            let fill_enabled = inherited_attr(node, "fill") != Some("none");

            if fill_enabled {
                if let Some(mesh) = tessellate_fill(&path)? {
                    meshes.push(mesh);
                }
            }
            if stroke_enabled {
                let stroke_width = parse_svg_number(inherited_attr(node, "stroke-width"))
                    .unwrap_or(DEFAULT_STROKE_WIDTH);
                if let Some(mesh) = tessellate_stroke(&path, stroke_width)? {
                    meshes.push(mesh);
                }
            }
        }

        if meshes.is_empty() {
            return Err(VectorIconError::new(
                "SVG icon contains no supported path geometry",
            ));
        }

        Ok(Self { view_box, meshes })
    }

    /// Parse static SVG data once per `id` and return cached geometry clones.
    ///
    /// This is the preferred path for bundled icons created from
    /// `include_str!()` assets. It keeps designer-authored SVGs in source while
    /// avoiding XML parsing and tessellation during every widget tree rebuild.
    pub fn from_static_svg(id: &'static str, svg: &'static str) -> Result<Self, VectorIconError> {
        let cache =
            STATIC_SVG_ICON_CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
        {
            let guard = cache
                .lock()
                .map_err(|_| VectorIconError::new("static SVG icon cache is poisoned"))?;
            if let Some(icon) = guard.get(id) {
                return Ok(icon.clone());
            }
        }

        let icon = Self::from_svg_str(svg)?;
        let mut guard = cache
            .lock()
            .map_err(|_| VectorIconError::new("static SVG icon cache is poisoned"))?;
        let icon = guard.entry(id).or_insert(icon);
        Ok(icon.clone())
    }

    /// Paint the icon into a square or rectangular viewport using one theme color.
    pub fn paint(&self, ctx: &mut PaintContext, bounds: Rect, color: Color) {
        let fitted = fit_view_box(self.view_box, bounds);
        for mesh in &self.meshes {
            let mut vertices = Vec::with_capacity(mesh.triangles.len() * 3);
            for triangle in &mesh.triangles {
                vertices.push(map_point(triangle[0], self.view_box, fitted));
                vertices.push(map_point(triangle[1], self.view_box, fitted));
                vertices.push(map_point(triangle[2], self.view_box, fitted));
            }
            ctx.encoder.draw_triangles(&vertices, color);
        }
    }

    /// Source viewBox used for scaling.
    pub fn view_box(&self) -> Rect {
        self.view_box
    }

    /// Number of tessellated meshes.
    pub fn shape_count(&self) -> usize {
        self.meshes.len()
    }

    /// Number of triangles in the cached icon mesh.
    pub fn triangle_count(&self) -> usize {
        self.meshes.iter().map(|mesh| mesh.triangles.len()).sum()
    }
}

fn svg_view_box(root: roxmltree::Node<'_, '_>) -> Result<Rect, VectorIconError> {
    if let Some(value) = root.attribute("viewBox") {
        let view_box = svgtypes::ViewBox::from_str(value)
            .map_err(|err| VectorIconError::new(err.to_string()))?;
        return Ok(Rect::new(
            view_box.x as f32,
            view_box.y as f32,
            view_box.w.max(1.0) as f32,
            view_box.h.max(1.0) as f32,
        ));
    }

    let width = parse_svg_number(root.attribute("width")).unwrap_or(24.0);
    let height = parse_svg_number(root.attribute("height")).unwrap_or(width);
    Ok(Rect::new(0.0, 0.0, width.max(1.0), height.max(1.0)))
}

fn parse_svg_number(value: Option<&str>) -> Option<f32> {
    let raw = value?;
    let number = raw.trim().trim_end_matches("px").parse::<f32>().ok()?;
    number.is_finite().then_some(number)
}

fn inherited_attr<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.ancestors().find_map(|ancestor| ancestor.attribute(name))
}

fn svg_path_to_lyon(data: &str) -> Result<Path, VectorIconError> {
    let mut builder = Path::builder();
    let mut cursor = point(0.0, 0.0);
    let mut start = point(0.0, 0.0);
    let mut path_started = false;
    let mut last_cubic_control: Option<LyonPoint> = None;
    let mut last_quadratic_control: Option<LyonPoint> = None;

    for segment in PathParser::from(data) {
        let segment = segment.map_err(|err| VectorIconError::new(err.to_string()))?;
        match segment {
            PathSegment::MoveTo { abs, x, y } => {
                if path_started {
                    builder.end(false);
                }
                cursor = resolve_point(cursor, abs, x, y);
                start = cursor;
                builder.begin(cursor);
                path_started = true;
                last_cubic_control = None;
                last_quadratic_control = None;
            }
            PathSegment::LineTo { abs, x, y } => {
                cursor = resolve_point(cursor, abs, x, y);
                builder.line_to(cursor);
                last_cubic_control = None;
                last_quadratic_control = None;
            }
            PathSegment::HorizontalLineTo { abs, x } => {
                cursor = if abs {
                    point(x as f32, cursor.y)
                } else {
                    point(cursor.x + x as f32, cursor.y)
                };
                builder.line_to(cursor);
                last_cubic_control = None;
                last_quadratic_control = None;
            }
            PathSegment::VerticalLineTo { abs, y } => {
                cursor = if abs {
                    point(cursor.x, y as f32)
                } else {
                    point(cursor.x, cursor.y + y as f32)
                };
                builder.line_to(cursor);
                last_cubic_control = None;
                last_quadratic_control = None;
            }
            PathSegment::CurveTo { abs, x1, y1, x2, y2, x, y } => {
                let c1 = resolve_point(cursor, abs, x1, y1);
                let c2 = resolve_point(cursor, abs, x2, y2);
                let end = resolve_point(cursor, abs, x, y);
                builder.cubic_bezier_to(c1, c2, end);
                cursor = end;
                last_cubic_control = Some(c2);
                last_quadratic_control = None;
            }
            PathSegment::SmoothCurveTo { abs, x2, y2, x, y } => {
                let c1 = last_cubic_control
                    .map(|control| reflect_point(control, cursor))
                    .unwrap_or(cursor);
                let c2 = resolve_point(cursor, abs, x2, y2);
                let end = resolve_point(cursor, abs, x, y);
                builder.cubic_bezier_to(c1, c2, end);
                cursor = end;
                last_cubic_control = Some(c2);
                last_quadratic_control = None;
            }
            PathSegment::Quadratic { abs, x1, y1, x, y } => {
                let c = resolve_point(cursor, abs, x1, y1);
                let end = resolve_point(cursor, abs, x, y);
                builder.quadratic_bezier_to(c, end);
                cursor = end;
                last_quadratic_control = Some(c);
                last_cubic_control = None;
            }
            PathSegment::SmoothQuadratic { abs, x, y } => {
                let c = last_quadratic_control
                    .map(|control| reflect_point(control, cursor))
                    .unwrap_or(cursor);
                let end = resolve_point(cursor, abs, x, y);
                builder.quadratic_bezier_to(c, end);
                cursor = end;
                last_quadratic_control = Some(c);
                last_cubic_control = None;
            }
            PathSegment::EllipticalArc { abs, x, y, .. } => {
                cursor = resolve_point(cursor, abs, x, y);
                builder.line_to(cursor);
                last_cubic_control = None;
                last_quadratic_control = None;
            }
            PathSegment::ClosePath { .. } => {
                builder.close();
                cursor = start;
                path_started = false;
                last_cubic_control = None;
                last_quadratic_control = None;
            }
        }
    }
    if path_started {
        builder.end(false);
    }
    Ok(builder.build())
}

fn resolve_point(cursor: LyonPoint, abs: bool, x: f64, y: f64) -> LyonPoint {
    if abs {
        point(x as f32, y as f32)
    } else {
        point(cursor.x + x as f32, cursor.y + y as f32)
    }
}

fn reflect_point(point: LyonPoint, around: LyonPoint) -> LyonPoint {
    lyon::math::point(around.x * 2.0 - point.x, around.y * 2.0 - point.y)
}

fn tessellate_fill(path: &Path) -> Result<Option<VectorIconMesh>, VectorIconError> {
    let mut geometry = VertexBuffers::<Point, u32>::new();
    FillTessellator::new().tessellate_path(
        path,
        &FillOptions::default().with_tolerance(TESSELLATION_TOLERANCE),
        &mut BuffersBuilder::new(&mut geometry, |vertex: FillVertex| {
            let position = vertex.position();
            Point::new(position.x, position.y)
        }),
    )?;
    Ok(mesh_from_geometry(geometry))
}

fn tessellate_stroke(path: &Path, width: f32) -> Result<Option<VectorIconMesh>, VectorIconError> {
    let mut geometry = VertexBuffers::<Point, u32>::new();
    StrokeTessellator::new().tessellate_path(
        path,
        &StrokeOptions::default()
            .with_line_width(width.max(0.1))
            .with_line_cap(LineCap::Round)
            .with_line_join(LineJoin::Round)
            .with_tolerance(TESSELLATION_TOLERANCE),
        &mut BuffersBuilder::new(&mut geometry, |vertex: StrokeVertex| {
            let position = vertex.position();
            Point::new(position.x, position.y)
        }),
    )?;
    Ok(mesh_from_geometry(geometry))
}

fn mesh_from_geometry(geometry: VertexBuffers<Point, u32>) -> Option<VectorIconMesh> {
    let mut triangles = Vec::with_capacity(geometry.indices.len() / 3);
    for indices in geometry.indices.chunks_exact(3) {
        let triangle = [
            *geometry.vertices.get(indices[0] as usize)?,
            *geometry.vertices.get(indices[1] as usize)?,
            *geometry.vertices.get(indices[2] as usize)?,
        ];
        triangles.push(triangle);
    }
    (!triangles.is_empty()).then_some(VectorIconMesh { triangles })
}

fn fit_view_box(view_box: Rect, bounds: Rect) -> Rect {
    let scale =
        (bounds.width / view_box.width.max(1.0)).min(bounds.height / view_box.height.max(1.0));
    let width = view_box.width * scale;
    let height = view_box.height * scale;
    Rect::new(
        bounds.x + (bounds.width - width) * 0.5,
        bounds.y + (bounds.height - height) * 0.5,
        width,
        height,
    )
}

fn map_point(point: Point, view_box: Rect, fitted: Rect) -> Point {
    let sx = fitted.width / view_box.width.max(1.0);
    let sy = fitted.height / view_box.height.max(1.0);
    Point::new(
        fitted.x + (point.x - view_box.x) * sx,
        fitted.y + (point.y - view_box.y) * sy,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;

    #[derive(Default)]
    struct PaintRecorder {
        triangles: usize,
    }

    impl DrawCommandEncoder for PaintRecorder {
        fn push_clip(&mut self, _bounds: Rect) {}
        fn pop_clip(&mut self) {}
        fn draw_rect(&mut self, _bounds: Rect, _color: Color, _corner_radius: f32) {}
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}
        fn draw_triangles(&mut self, vertices: &[Point], _color: Color) {
            self.triangles += vertices.len();
        }
        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}
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

    #[test]
    fn parses_stroked_svg_path_into_tessellated_icon_mesh() {
        let icon = VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M6 12L18 12" fill="none" stroke="black"/></svg>"#,
        )
        .expect("icon");

        assert_eq!(icon.view_box(), Rect::new(0.0, 0.0, 24.0, 24.0));
        assert_eq!(icon.shape_count(), 1);
        assert!(icon.triangle_count() > 0);
    }

    #[test]
    fn static_svg_icons_are_cached_by_id() {
        let first = VectorIcon::from_static_svg(
            "test.minus",
            r#"<svg viewBox="0 0 24 24"><path d="M6 12L18 12" fill="none" stroke="black"/></svg>"#,
        )
        .expect("first icon");
        let second = VectorIcon::from_static_svg(
            "test.minus",
            r#"<svg viewBox="0 0 24 24"><path d="M1 1L23 23" fill="none" stroke="black"/></svg>"#,
        )
        .expect("cached icon");

        assert_eq!(first, second);
    }

    #[test]
    fn parses_relative_and_curve_path_segments() {
        let icon = VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M4 12c4 -8 12 -8 16 0" fill="none" stroke="black"/></svg>"#,
        )
        .expect("icon");
        let mut recorder = PaintRecorder::default();
        let mut ctx = paint_ctx(&mut recorder);

        icon.paint(
            &mut ctx,
            Rect::new(0.0, 0.0, 24.0, 24.0),
            Color::from_hex(0xFFFFFF),
        );

        assert!(recorder.triangles > 0);
    }

    #[test]
    fn paints_closed_fill_as_tessellated_triangles() {
        let icon = VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M4 4L20 4L12 20Z" fill="black"/></svg>"#,
        )
        .expect("icon");
        let mut recorder = PaintRecorder::default();
        let mut ctx = paint_ctx(&mut recorder);

        icon.paint(
            &mut ctx,
            Rect::new(0.0, 0.0, 24.0, 24.0),
            Color::from_hex(0xFFFFFF),
        );

        assert!(recorder.triangles >= 3);
    }

    #[test]
    fn rejects_svg_without_supported_geometry() {
        let err = VectorIcon::from_svg_str(r#"<svg viewBox="0 0 24 24"></svg>"#)
            .expect_err("empty icon should fail");

        assert!(err.to_string().contains("no supported path"));
    }
}
