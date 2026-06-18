//! SVG-backed vector icon geometry.
//!
//! SVG is the designer-facing authoring format. Runtime widgets consume a
//! cached `VectorIcon` asset. SVG documents are normalized through usvg and
//! tessellated by lyon for geometry metadata and fallback painting. Normal icon
//! painting uses resvg/tiny-skia to rasterize the source SVG at the target pixel
//! size, then submits the result to the renderer image atlas.

use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

use lyon::math::{point, Point as LyonPoint};
use lyon::path::Path;
use lyon::tessellation::{
    BuffersBuilder, FillOptions, FillRule as LyonFillRule, FillTessellator, FillVertex, LineCap,
    LineJoin, StrokeOptions, StrokeTessellator, StrokeVertex, TessellationError, VertexBuffers,
};
use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;
use tiny_skia_path::{PathSegment as TinyPathSegment, Point as TinyPoint, Transform};

const TESSELLATION_TOLERANCE: f32 = 0.08;
/// Largest SVG icon edge rasterized into the renderer image atlas.
///
/// The UI image atlas is currently 2048x2048 with transparent padding around
/// each allocation. 512px keeps designer-authored icons and medium empty-state
/// glyphs on the browser-like resvg/tiny-skia path while still preventing a
/// single oversized SVG from consuming a disproportionate atlas row.
const MAX_RASTER_ICON_SIZE: u32 = 512;
/// Quality multiplier for small SVG icon rasters.
///
/// The supersampled bitmap is cached once in the renderer image atlas and drawn
/// back into the requested logical bounds with linear filtering. This improves
/// diagonal and curve coverage for small icons without changing widget layout.
const RASTER_ICON_SUPERSAMPLE: u32 = 2;

static STATIC_SVG_ICON_CACHE: OnceLock<Mutex<std::collections::HashMap<&'static str, VectorIcon>>> =
    OnceLock::new();
static RASTER_ICON_CACHE: OnceLock<Mutex<std::collections::HashMap<RasterIconKey, RasterIcon>>> =
    OnceLock::new();

/// Parsed, tessellated icon geometry ready for widget painting.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorIcon {
    view_box: Rect,
    meshes: Vec<VectorIconMesh>,
    raster_source: Option<VectorIconRasterSource>,
}

#[derive(Debug, Clone, PartialEq)]
struct VectorIconMesh {
    triangles: Vec<[Point; 3]>,
}

#[derive(Debug, Clone, PartialEq)]
struct VectorIconRasterSource {
    id: Arc<str>,
    svg: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RasterIconKey {
    id: Arc<str>,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, PartialEq)]
struct RasterIcon {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
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
    /// SVG parsing is intentionally delegated to usvg so designer-authored
    /// basic shapes, paths, relative commands, arcs, inherited paint, and
    /// transforms are normalized before this crate converts geometry to lyon.
    pub fn from_svg_str(svg: &str) -> Result<Self, VectorIconError> {
        let tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default())
            .map_err(|err| VectorIconError::new(err.to_string()))?;
        let size = tree.size();
        let view_box = Rect::new(0.0, 0.0, size.width().max(1.0), size.height().max(1.0));
        let mut meshes = Vec::new();

        collect_usvg_group(tree.root(), &mut meshes)?;

        if meshes.is_empty() {
            return Err(VectorIconError::new(
                "SVG icon contains no renderable vector geometry",
            ));
        }

        let source_id = format!("svg:{:016x}", hash_str(svg));
        Ok(Self {
            view_box,
            meshes,
            raster_source: Some(VectorIconRasterSource {
                id: Arc::from(source_id),
                svg: Arc::from(svg),
            }),
        })
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

        let mut icon = Self::from_svg_str(svg)?;
        icon.raster_source =
            Some(VectorIconRasterSource { id: Arc::from(id), svg: Arc::from(svg) });
        let mut guard = cache
            .lock()
            .map_err(|_| VectorIconError::new("static SVG icon cache is poisoned"))?;
        let icon = guard.entry(id).or_insert(icon);
        Ok(icon.clone())
    }

    /// Paint the icon into a square or rectangular viewport using one theme color.
    pub fn paint(&self, ctx: &mut PaintContext, bounds: Rect, color: Color) {
        let fitted = fit_view_box(self.view_box, bounds);
        if let Some((key, raster)) = self.raster_icon_for_bounds(fitted) {
            ctx.encoder.draw_raster_image(
                &key,
                fitted,
                raster.width,
                raster.height,
                raster.rgba.clone(),
                color,
            );
            return;
        }

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

    fn raster_icon_for_bounds(&self, bounds: Rect) -> Option<(String, RasterIcon)> {
        let source = self.raster_source.as_ref()?;
        let target_width = raster_target_edge(bounds.width);
        let target_height = raster_target_edge(bounds.height);
        if target_width > MAX_RASTER_ICON_SIZE || target_height > MAX_RASTER_ICON_SIZE {
            return None;
        }
        let scale = raster_supersample_scale(target_width, target_height);
        let width = target_width.checked_mul(scale)?;
        let height = target_height.checked_mul(scale)?;

        let key = RasterIconKey { id: source.id.clone(), width, height };
        let cache = RASTER_ICON_CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
        {
            let guard = cache.lock().ok()?;
            if let Some(icon) = guard.get(&key) {
                return Some((raster_draw_key(&key), icon.clone()));
            }
        }

        let raster = rasterize_svg_to_alpha_rgba(&source.svg, width, height)?;
        let mut guard = cache.lock().ok()?;
        let raster = guard.entry(key.clone()).or_insert(raster).clone();
        Some((raster_draw_key(&key), raster))
    }
}

fn raster_target_edge(logical_edge: f32) -> u32 {
    logical_edge.ceil().max(1.0) as u32
}

fn raster_supersample_scale(target_width: u32, target_height: u32) -> u32 {
    if target_width.saturating_mul(RASTER_ICON_SUPERSAMPLE) <= MAX_RASTER_ICON_SIZE
        && target_height.saturating_mul(RASTER_ICON_SUPERSAMPLE) <= MAX_RASTER_ICON_SIZE
    {
        RASTER_ICON_SUPERSAMPLE
    } else {
        1
    }
}

fn hash_str(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn raster_draw_key(key: &RasterIconKey) -> String {
    format!("vector-icon:{}:{}x{}", key.id, key.width, key.height)
}

fn rasterize_svg_to_alpha_rgba(svg: &str, width: u32, height: u32) -> Option<RasterIcon> {
    let tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default()).ok()?;
    let source_size = tree.size();
    let scale_x = width as f32 / source_size.width().max(1.0);
    let scale_y = height as f32 / source_size.height().max(1.0);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale_x, scale_y),
        &mut pixmap.as_mut(),
    );

    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for pixel in pixmap.data().chunks_exact(4) {
        rgba.extend_from_slice(&[255, 255, 255, pixel[3]]);
    }

    Some(RasterIcon { width, height, rgba: Arc::from(rgba) })
}

fn collect_usvg_group(
    group: &usvg::Group,
    meshes: &mut Vec<VectorIconMesh>,
) -> Result<(), VectorIconError> {
    for node in group.children() {
        match node {
            usvg::Node::Group(group) => collect_usvg_group(group, meshes)?,
            usvg::Node::Path(path) => collect_usvg_path(path, meshes)?,
            usvg::Node::Image(_) | usvg::Node::Text(_) => {}
        }
    }

    Ok(())
}

fn collect_usvg_path(
    source: &usvg::Path,
    meshes: &mut Vec<VectorIconMesh>,
) -> Result<(), VectorIconError> {
    if !source.is_visible() {
        return Ok(());
    }

    let path = tiny_path_to_lyon(source.data(), source.abs_transform());
    let render_fill = |meshes: &mut Vec<VectorIconMesh>| -> Result<(), VectorIconError> {
        let Some(fill) = source.fill() else {
            return Ok(());
        };
        if let Some(mesh) = tessellate_fill(&path, fill_rule(fill.rule()))? {
            meshes.push(mesh);
        }
        Ok(())
    };
    let render_stroke = |meshes: &mut Vec<VectorIconMesh>| -> Result<(), VectorIconError> {
        let Some(stroke) = source.stroke() else {
            return Ok(());
        };
        let stroke_width = stroke.width().get() * stroke_scale(source.abs_transform());
        if let Some(mesh) = tessellate_stroke(
            &path,
            stroke_width,
            line_cap(stroke.linecap()),
            line_join(stroke.linejoin()),
            stroke.miterlimit().get(),
        )? {
            meshes.push(mesh);
        }
        Ok(())
    };

    match source.paint_order() {
        usvg::PaintOrder::FillAndStroke => {
            render_fill(meshes)?;
            render_stroke(meshes)?;
        }
        usvg::PaintOrder::StrokeAndFill => {
            render_stroke(meshes)?;
            render_fill(meshes)?;
        }
    }

    Ok(())
}

fn tiny_path_to_lyon(data: &tiny_skia_path::Path, transform: Transform) -> Path {
    let mut builder = Path::builder();
    let mut path_started = false;

    for segment in data.segments() {
        match segment {
            TinyPathSegment::MoveTo(point) => {
                if path_started {
                    builder.end(false);
                }
                builder.begin(transformed_point(point, transform));
                path_started = true;
            }
            TinyPathSegment::LineTo(point) => {
                builder.line_to(transformed_point(point, transform));
            }
            TinyPathSegment::QuadTo(control, end) => {
                builder.quadratic_bezier_to(
                    transformed_point(control, transform),
                    transformed_point(end, transform),
                );
            }
            TinyPathSegment::CubicTo(control_a, control_b, end) => {
                builder.cubic_bezier_to(
                    transformed_point(control_a, transform),
                    transformed_point(control_b, transform),
                    transformed_point(end, transform),
                );
            }
            TinyPathSegment::Close => {
                builder.close();
                path_started = false;
            }
        }
    }
    if path_started {
        builder.end(false);
    }
    builder.build()
}

fn transformed_point(mut value: TinyPoint, transform: Transform) -> LyonPoint {
    transform.map_point(&mut value);
    point(value.x, value.y)
}

fn fill_rule(rule: usvg::FillRule) -> LyonFillRule {
    match rule {
        usvg::FillRule::NonZero => LyonFillRule::NonZero,
        usvg::FillRule::EvenOdd => LyonFillRule::EvenOdd,
    }
}

fn line_cap(cap: usvg::LineCap) -> LineCap {
    match cap {
        usvg::LineCap::Butt => LineCap::Butt,
        usvg::LineCap::Round => LineCap::Round,
        usvg::LineCap::Square => LineCap::Square,
    }
}

fn line_join(join: usvg::LineJoin) -> LineJoin {
    match join {
        usvg::LineJoin::Miter => LineJoin::Miter,
        usvg::LineJoin::MiterClip => LineJoin::MiterClip,
        usvg::LineJoin::Round => LineJoin::Round,
        usvg::LineJoin::Bevel => LineJoin::Bevel,
    }
}

fn stroke_scale(transform: Transform) -> f32 {
    let x_scale = transform.sx.hypot(transform.ky);
    let y_scale = transform.kx.hypot(transform.sy);
    ((x_scale + y_scale) * 0.5).max(0.01)
}

fn tessellate_fill(
    path: &Path,
    fill_rule: LyonFillRule,
) -> Result<Option<VectorIconMesh>, VectorIconError> {
    let mut geometry = VertexBuffers::<Point, u32>::new();
    FillTessellator::new().tessellate_path(
        path,
        &FillOptions::default()
            .with_tolerance(TESSELLATION_TOLERANCE)
            .with_fill_rule(fill_rule),
        &mut BuffersBuilder::new(&mut geometry, |vertex: FillVertex| {
            let position = vertex.position();
            Point::new(position.x, position.y)
        }),
    )?;
    Ok(mesh_from_geometry(geometry))
}

fn tessellate_stroke(
    path: &Path,
    width: f32,
    cap: LineCap,
    join: LineJoin,
    miter_limit: f32,
) -> Result<Option<VectorIconMesh>, VectorIconError> {
    let mut geometry = VertexBuffers::<Point, u32>::new();
    StrokeTessellator::new().tessellate_path(
        path,
        &StrokeOptions::default()
            .with_line_width(width.max(0.1))
            .with_line_cap(cap)
            .with_line_join(join)
            .with_miter_limit(miter_limit.max(1.0))
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
        raster_images: usize,
        raster_bounds: Vec<Rect>,
        raster_sizes: Vec<(u32, u32)>,
    }

    impl DrawCommandEncoder for PaintRecorder {
        fn push_clip(&mut self, _bounds: Rect) {}
        fn pop_clip(&mut self) {}
        fn draw_rect(&mut self, _bounds: Rect, _color: Color, _corner_radius: f32) {}
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}
        fn draw_triangles(&mut self, vertices: &[Point], _color: Color) {
            self.triangles += vertices.len();
        }
        fn draw_raster_image(
            &mut self,
            _key: &str,
            bounds: Rect,
            width: u32,
            height: u32,
            _rgba: std::sync::Arc<[u8]>,
            _tint: Color,
        ) {
            self.raster_images += 1;
            self.raster_bounds.push(bounds);
            self.raster_sizes.push((width, height));
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

        assert_eq!(recorder.raster_images, 1);
    }

    #[test]
    fn parses_basic_svg_shapes_normalized_by_usvg() {
        let icon = VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24">
                <g fill="black">
                    <rect x="2" y="2" width="6" height="6"/>
                    <circle cx="16" cy="6" r="3"/>
                </g>
                <line x1="4" y1="18" x2="20" y2="18" stroke="black" stroke-width="2"/>
            </svg>"#,
        )
        .expect("icon");

        assert_eq!(icon.shape_count(), 3);
        assert!(icon.triangle_count() > 6);
    }

    #[test]
    fn applies_group_transforms_before_tessellation() {
        let icon = VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24">
                <g transform="translate(8 4)">
                    <path d="M0 0L4 0L0 4Z" fill="black"/>
                </g>
            </svg>"#,
        )
        .expect("icon");

        let min_x = icon
            .meshes
            .iter()
            .flat_map(|mesh| mesh.triangles.iter().flatten())
            .map(|point| point.x)
            .fold(f32::INFINITY, f32::min);
        let min_y = icon
            .meshes
            .iter()
            .flat_map(|mesh| mesh.triangles.iter().flatten())
            .map(|point| point.y)
            .fold(f32::INFINITY, f32::min);

        assert!(min_x >= 8.0);
        assert!(min_y >= 4.0);
    }

    #[test]
    fn scales_strokes_when_svg_transform_scales_geometry() {
        let scale = stroke_scale(Transform::from_scale(-2.0, 3.0));

        assert!((scale - 2.5).abs() < f32::EPSILON);
    }

    #[test]
    fn paints_svg_icons_as_cached_raster_images_for_browser_like_aa() {
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

        assert_eq!(recorder.raster_images, 1);
        assert_eq!(recorder.triangles, 0);
    }

    #[test]
    fn small_svg_icons_are_supersampled_without_changing_layout_bounds() {
        let icon = VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M4 4L20 4L12 20Z" fill="black"/></svg>"#,
        )
        .expect("icon");
        let mut recorder = PaintRecorder::default();
        let mut ctx = paint_ctx(&mut recorder);

        icon.paint(
            &mut ctx,
            Rect::new(10.2, 20.6, 16.0, 16.0),
            Color::from_hex(0xFFFFFF),
        );

        assert_eq!(recorder.raster_images, 1);
        assert_eq!(
            recorder.raster_bounds,
            vec![Rect::new(10.2, 20.6, 16.0, 16.0)]
        );
        assert_eq!(recorder.raster_sizes, vec![(32, 32)]);
    }

    #[test]
    fn fractional_svg_icon_bounds_are_preserved_while_raster_size_is_ceiled() {
        let icon = VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M4 4L20 4L12 20Z" fill="black"/></svg>"#,
        )
        .expect("icon");
        let mut recorder = PaintRecorder::default();
        let mut ctx = paint_ctx(&mut recorder);

        icon.paint(
            &mut ctx,
            Rect::new(5.25, 7.75, 15.2, 15.7),
            Color::from_hex(0xFFFFFF),
        );

        assert_eq!(recorder.raster_images, 1);
        assert_eq!(
            recorder.raster_bounds,
            vec![Rect::new(5.25, 8.0, 15.2, 15.2)]
        );
        assert_eq!(recorder.raster_sizes, vec![(32, 32)]);
    }

    #[test]
    fn medium_svg_icons_stay_on_raster_path_for_smooth_edges() {
        let icon = VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M4 4L20 4L12 20Z" fill="black"/></svg>"#,
        )
        .expect("icon");
        let mut recorder = PaintRecorder::default();
        let mut ctx = paint_ctx(&mut recorder);

        icon.paint(
            &mut ctx,
            Rect::new(0.0, 0.0, 512.0, 512.0),
            Color::from_hex(0xFFFFFF),
        );

        assert_eq!(recorder.raster_images, 1);
        assert_eq!(recorder.triangles, 0);
    }

    #[test]
    fn falls_back_to_tessellated_triangles_without_raster_source() {
        let mut icon = VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M4 4L20 4L12 20Z" fill="black"/></svg>"#,
        )
        .expect("icon");
        icon.raster_source = None;
        let mut recorder = PaintRecorder::default();
        let mut ctx = paint_ctx(&mut recorder);

        icon.paint(
            &mut ctx,
            Rect::new(0.0, 0.0, 24.0, 24.0),
            Color::from_hex(0xFFFFFF),
        );

        assert!(recorder.triangles >= 3);
        assert_eq!(recorder.raster_images, 0);
    }

    #[test]
    fn falls_back_to_tessellated_triangles_for_oversized_icon_bounds() {
        let icon = VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M4 4L20 4L12 20Z" fill="black"/></svg>"#,
        )
        .expect("icon");
        let mut recorder = PaintRecorder::default();
        let mut ctx = paint_ctx(&mut recorder);

        icon.paint(
            &mut ctx,
            Rect::new(0.0, 0.0, 1024.0, 1024.0),
            Color::from_hex(0xFFFFFF),
        );

        assert!(recorder.triangles >= 3);
        assert_eq!(recorder.raster_images, 0);
    }

    #[test]
    fn raster_icon_cache_is_keyed_by_rasterized_pixel_size() {
        let icon = VectorIcon::from_static_svg(
            "test.triangle.cache",
            r#"<svg viewBox="0 0 24 24"><path d="M4 4L20 4L12 20Z" fill="black"/></svg>"#,
        )
        .expect("icon");

        let (_, small) = icon
            .raster_icon_for_bounds(Rect::new(0.2, 0.3, 15.7, 16.1))
            .expect("small raster");
        let (_, large) = icon
            .raster_icon_for_bounds(Rect::new(0.0, 0.0, 32.0, 32.0))
            .expect("large raster");

        assert_eq!((small.width, small.height), (32, 34));
        assert_eq!((large.width, large.height), (64, 64));
        assert_ne!(small.rgba.len(), large.rgba.len());
    }

    #[test]
    fn rejects_svg_without_supported_geometry() {
        let err = VectorIcon::from_svg_str(r#"<svg viewBox="0 0 24 24"></svg>"#)
            .expect_err("empty icon should fail");

        assert!(err.to_string().contains("no renderable vector geometry"));
    }
}
