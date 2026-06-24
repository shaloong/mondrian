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
/// each allocation. 1024px keeps large designer-authored empty-state glyphs on
/// the browser-like resvg/tiny-skia path while still preventing very large
/// illustrations from monopolizing the atlas.
const MAX_RASTER_ICON_SIZE: u32 = 1024;
/// Maximum quality multiplier used while rasterizing small SVG icons.
///
/// SVGs are rendered into a temporary high-resolution pixmap and box-filtered
/// back to the requested target pixel size before they enter the renderer image
/// atlas. This gives diagonals and curves stable coverage without asking the GPU
/// to minify icon atlases with point-like linear samples.
const MAX_RASTER_ICON_SUPERSAMPLE: u32 = 4;

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
        let supersample = raster_supersample_scale(target_width, target_height);
        let key = RasterIconKey {
            id: source.id.clone(),
            width: target_width,
            height: target_height,
        };
        let cache = RASTER_ICON_CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
        {
            let guard = cache.lock().ok()?;
            if let Some(icon) = guard.get(&key) {
                return Some((raster_draw_key(&key), icon.clone()));
            }
        }

        let raster =
            rasterize_svg_to_alpha_rgba(&source.svg, target_width, target_height, supersample)?;
        let mut guard = cache.lock().ok()?;
        let raster = guard.entry(key.clone()).or_insert(raster).clone();
        Some((raster_draw_key(&key), raster))
    }
}

fn raster_target_edge(logical_edge: f32) -> u32 {
    logical_edge.ceil().max(1.0) as u32
}

fn raster_supersample_scale(target_width: u32, target_height: u32) -> u32 {
    for scale in (2..=MAX_RASTER_ICON_SUPERSAMPLE).rev() {
        if target_width.saturating_mul(scale) <= MAX_RASTER_ICON_SIZE
            && target_height.saturating_mul(scale) <= MAX_RASTER_ICON_SIZE
        {
            return scale;
        }
    }
    1
}

fn hash_str(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn raster_draw_key(key: &RasterIconKey) -> String {
    format!("vector-icon:{}:{}x{}", key.id, key.width, key.height)
}

fn rasterize_svg_to_alpha_rgba(
    svg: &str,
    width: u32,
    height: u32,
    supersample: u32,
) -> Option<RasterIcon> {
    let supersample = supersample.max(1);
    let render_width = width.checked_mul(supersample)?;
    let render_height = height.checked_mul(supersample)?;
    let tree = usvg::Tree::from_data(svg.as_bytes(), &usvg::Options::default()).ok()?;
    let source_size = tree.size();
    let scale_x = render_width as f32 / source_size.width().max(1.0);
    let scale_y = render_height as f32 / source_size.height().max(1.0);
    let mut pixmap = tiny_skia::Pixmap::new(render_width, render_height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale_x, scale_y),
        &mut pixmap.as_mut(),
    );

    let rgba = if supersample == 1 {
        alpha_rgba_from_tiny_skia_data(pixmap.data())
    } else {
        downsample_alpha_rgba_box(
            pixmap.data(),
            render_width,
            render_height,
            width,
            height,
            supersample,
        )?
    };

    Some(RasterIcon { width, height, rgba: Arc::from(rgba) })
}

fn alpha_rgba_from_tiny_skia_data(pixels: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(pixels.len());
    for pixel in pixels.chunks_exact(4) {
        rgba.extend_from_slice(&[255, 255, 255, pixel[3]]);
    }
    rgba
}

fn downsample_alpha_rgba_box(
    pixels: &[u8],
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
    supersample: u32,
) -> Option<Vec<u8>> {
    if supersample == 0
        || source_width != target_width.checked_mul(supersample)?
        || source_height != target_height.checked_mul(supersample)?
    {
        return None;
    }
    let expected_len = source_width.checked_mul(source_height)?.checked_mul(4)? as usize;
    if pixels.len() != expected_len {
        return None;
    }

    let mut rgba = Vec::with_capacity(target_width as usize * target_height as usize * 4);
    let sample_count = supersample.checked_mul(supersample)?;
    for y in 0..target_height {
        for x in 0..target_width {
            let mut alpha_sum = 0u32;
            for sy in 0..supersample {
                let source_y = y * supersample + sy;
                for sx in 0..supersample {
                    let source_x = x * supersample + sx;
                    let offset = ((source_y * source_width + source_x) * 4 + 3) as usize;
                    alpha_sum += u32::from(pixels[offset]);
                }
            }
            let alpha = ((alpha_sum + sample_count / 2) / sample_count).min(255) as u8;
            rgba.extend_from_slice(&[255, 255, 255, alpha]);
        }
    }
    Some(rgba)
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
    use mondrian_ui_renderer::{DrawCommand, DrawEncoder, UiRenderFrameStats, UiRenderer};
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

    fn renderer_paint_ctx<'a>(encoder: &'a mut DrawEncoder) -> PaintContext<'a> {
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
    fn raster_supersample_scale_prioritizes_small_icon_quality() {
        assert_eq!(raster_supersample_scale(16, 16), 4);
        assert_eq!(raster_supersample_scale(128, 128), 4);
        assert_eq!(raster_supersample_scale(256, 256), 4);
        assert_eq!(raster_supersample_scale(512, 512), 2);
        assert_eq!(raster_supersample_scale(1024, 1024), 1);
    }

    #[test]
    fn downsample_alpha_rgba_box_averages_supersampled_coverage() {
        let source = vec![
            0, 0, 0, 255, //
            0, 0, 0, 0, //
            0, 0, 0, 0, //
            0, 0, 0, 0,
        ];

        let rgba = downsample_alpha_rgba_box(&source, 2, 2, 1, 1, 2).expect("downsample");

        assert_eq!(rgba, vec![255, 255, 255, 64]);
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
    fn offscreen_svg_icon_rasterization_reaches_renderer_image_atlas() {
        let Some(mut harness) = OffscreenHarness::new(48, 48) else {
            return;
        };
        let icon = VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 24 24"><path d="M6 6H18V18H6Z" fill="black"/></svg>"#,
        )
        .expect("icon");
        let mut encoder = DrawEncoder::new();
        {
            let mut ctx = renderer_paint_ctx(&mut encoder);
            icon.paint(
                &mut ctx,
                Rect::new(8.0, 8.0, 32.0, 32.0),
                Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 },
            );
        }

        let pixels = harness.render(encoder.finish());
        let center = pixel(&pixels, 48, 24, 24);
        let outside = pixel(&pixels, 48, 8, 8);

        assert!(
            center[1] >= 180 && center[0] <= 32 && center[2] <= 32 && center[3] >= 180,
            "SVG icon center should render as green tinted raster image, got {center:?}"
        );
        assert_eq!(
            outside[3], 0,
            "transparent SVG icon bounds outside the path should remain transparent, got {outside:?}"
        );

        let stats = harness.last_stats.expect("render should record stats");
        assert!(stats.uploaded_raster_images);
        assert_eq!(stats.failed_raster_images, 0);
        assert!(stats.image_atlas_entries >= 1);
    }

    #[test]
    fn small_svg_icons_are_prefiltered_without_changing_layout_bounds() {
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
        assert_eq!(recorder.raster_sizes, vec![(16, 16)]);
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
        assert_eq!(recorder.raster_sizes, vec![(16, 16)]);
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
    fn large_svg_icons_stay_on_raster_path_for_smooth_edges() {
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

        assert_eq!(recorder.raster_images, 1);
        assert_eq!(recorder.triangles, 0);
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
            Rect::new(0.0, 0.0, 1025.0, 1025.0),
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

        assert_eq!((small.width, small.height), (16, 17));
        assert_eq!((large.width, large.height), (32, 32));
        assert_ne!(small.rgba.len(), large.rgba.len());
    }

    #[test]
    fn rejects_svg_without_supported_geometry() {
        let err = VectorIcon::from_svg_str(r#"<svg viewBox="0 0 24 24"></svg>"#)
            .expect_err("empty icon should fail");

        assert!(err.to_string().contains("no renderable vector geometry"));
    }

    struct OffscreenHarness {
        device: wgpu::Device,
        queue: wgpu::Queue,
        renderer: UiRenderer,
        texture: wgpu::Texture,
        size: (u32, u32),
        last_stats: Option<UiRenderFrameStats>,
    }

    impl OffscreenHarness {
        fn new(width: u32, height: u32) -> Option<Self> {
            let instance = wgpu::Instance::new(
                wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
            );
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    compatible_surface: None,
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                }))
                .ok()?;
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                    .ok()?;
            let format = wgpu::TextureFormat::Rgba8Unorm;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ui_vector_icon_offscreen_test_target"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let renderer = UiRenderer::new(&device, format);

            Some(Self {
                device,
                queue,
                renderer,
                texture,
                size: (width, height),
                last_stats: None,
            })
        }

        fn render(&mut self, commands: Vec<DrawCommand>) -> Vec<u8> {
            let view = self.texture.create_view(&wgpu::TextureViewDescriptor::default());
            let stats = self.renderer.render_resolved_commands(
                &self.device,
                &self.queue,
                &view,
                &commands,
                self.size,
            );
            self.last_stats = Some(stats);
            self.readback()
        }

        fn readback(&self) -> Vec<u8> {
            let (width, height) = self.size;
            let bytes_per_pixel = 4u32;
            let unpadded_bytes_per_row = width * bytes_per_pixel;
            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
            let buffer_size = padded_bytes_per_row as u64 * height as u64;

            let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui_vector_icon_offscreen_test_readback"),
                size: buffer_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ui_vector_icon_offscreen_test_readback_encoder"),
            });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bytes_per_row),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            self.queue.submit([encoder.finish()]);

            let (tx, rx) = std::sync::mpsc::channel();
            let slice = readback.slice(..);
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            let _ =
                self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
            rx.recv()
                .expect("readback map callback should run")
                .expect("readback map should succeed");

            let mapped = slice.get_mapped_range();
            let mut out = vec![0u8; width as usize * height as usize * 4];
            for row in 0..height as usize {
                let src_start = row * padded_bytes_per_row as usize;
                let src_end = src_start + unpadded_bytes_per_row as usize;
                let dst_start = row * unpadded_bytes_per_row as usize;
                let dst_end = dst_start + unpadded_bytes_per_row as usize;
                out[dst_start..dst_end].copy_from_slice(&mapped[src_start..src_end]);
            }
            drop(mapped);
            readback.unmap();
            out
        }
    }

    fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * width + x) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    }
}
