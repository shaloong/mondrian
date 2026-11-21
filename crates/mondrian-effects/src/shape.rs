//! Shape: a vector generator that draws a rounded rectangle, circle, ellipse,
//! regular polygon, star or line into the frame.
//!
//! The effect is a pure point operation. Every output pixel depends on its own
//! coordinates and the authored settings only, so there is no neighbourhood
//! read, no scratch plane, and no ordering constraint beyond the point-op chain.
//!
//! Coordinates. Pixel centres are `(x + 0.5, y + 0.5)`. The shape centre is
//! `c = (position.x * width, position.y * height)` and the local coordinate is
//! `q = R(-θ) (p - c)`, where `θ = rotation` in radians and `R(-θ)` rotates
//! counter-clockwise in image coordinates (`y` grows downward), so a positive
//! rotation turns the shape clockwise on screen.
//!
//! Distance fields. Closed shapes report a pixel-valued field that is negative
//! inside and positive outside; the line reports unsigned distance to its spine.
//! Rectangle and circle use Euclidean signed distance. The ellipse and polygon
//! use the approximations described below:
//!
//! ```text
//! rectangle: q1 = |q| - (b - r), d = |max(q1, 0)| + min(max(q1.x, q1.y), 0) - r
//! circle:    d = |q| - min(hx, hy)
//! ellipse:   d = (|(q.x / a, q.y / b)| - 1) * min(a, b)      (approximation)
//! polygon:   d = max_i (q . n_i - ap),  n_i at (2i + 1) π / n
//! star:      d = ± min(seg(q', A, B), seg(q', A, B'))          (folded sector)
//! line:      d = seg(q, (-hx, 0), (hx, 0))
//! ```
//!
//! `hx = size.x / 2` and `hy = size.y / 2` are clamped to be non-negative, `r`
//! is `corner_radius` clamped to `min(hx, hy)`, `ap = min(hx, hy) cos(π / n)`
//! is the polygon apothem, and the star folds the polar angle into one sector
//! of half-width `π / n` before measuring against its two edges. The ellipse
//! field is a scaled radial approximation, not generally exact even on its axes.
//! The polygon's maximum half-plane field is not Euclidean distance outside
//! corners. Both preserve the zero contour but can change AA and stroke widths;
//! no distance-error bound is promised. Every shape except the line degenerates
//! to the empty set when `min(hx, hy) == 0`; the line keeps its one-pixel spine.
//!
//! Coverage. `cf = clamp(0.5 - d, 0, 1)` is the fill coverage, with a half-pixel
//! anti-aliasing radius. The stroke band is measured from the same distance:
//!
//! ```text
//! inside:  sd = max(-w - d, d)
//! centred: sd = max(-w / 2 - d, d - w / 2)
//! outside: sd = max(-d, d - w)
//! cs = w <= 0 ? 0 : clamp(0.5 - sd, 0, 1)
//! ```
//!
//! The line has no interior: its fill coverage is always zero and the stroke
//! always uses `w = max(stroke_width, 1)` with the centred band applied once to
//! its unsigned segment distance. This produces a solid round-ended capsule;
//! `stroke_width = 0` still draws a one-pixel spine.
//!
//! Compositing. CPU frames and authored colours use straight RGB plus normalized
//! alpha. The shape is assembled in premultiplied form,
//!
//! ```text
//! x_f = cf * fill_opacity / 100 * opacity / 100 * fill_alpha
//! x_s = cs * opacity / 100 * stroke_alpha
//! x   = clamp(x_s + x_f * (1 - x_s), 0, 1)
//! S   = stroke_rgb * x_s + fill_rgb * x_f * (1 - x_s)
//! ```
//!
//! and the boolean mode applies `x` and the premultiplied shape colour `S` to
//! source `F = source_rgb * a` with alpha `a`: `add` composes the shape over the
//! source, `subtract` keeps the source outside the shape, `intersect` keeps the
//! source inside it, and `exclude` is the premultiplied xor of the two. The result
//! is divided by its output alpha to restore straight RGB; exactly zero alpha
//! clears hidden RGB. True identity configurations leave the frame untouched.
//! RGB is not clamped, preserving scene-linear HDR. Alpha is normalized coverage;
//! the grading chain above owns the final display clamp.

use std::hash::{Hash, Hasher};

use thiserror::Error;

/// Documented shape size in pixels.
pub const SHAPE_DEFAULT_SIZE: [f32; 2] = [200.0, 200.0];
/// Documented shape centre, normalized to the frame.
pub const SHAPE_DEFAULT_POSITION: [f32; 2] = [0.5, 0.5];
/// Documented corner radius in pixels.
pub const SHAPE_DEFAULT_CORNER_RADIUS: f32 = 0.0;
/// Documented vertex count.
pub const SHAPE_DEFAULT_VERTICES: f32 = 5.0;
/// Documented star inner ratio.
pub const SHAPE_DEFAULT_INNER_RATIO: f32 = 0.5;
/// Documented fill colour.
pub const SHAPE_DEFAULT_FILL_COLOUR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Documented fill opacity, in percent.
pub const SHAPE_DEFAULT_FILL_OPACITY: f32 = 100.0;
/// Documented stroke width in pixels.
pub const SHAPE_DEFAULT_STROKE_WIDTH: f32 = 0.0;
/// Documented stroke colour.
pub const SHAPE_DEFAULT_STROKE_COLOUR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Documented rotation in degrees.
pub const SHAPE_DEFAULT_ROTATION: f32 = 0.0;
/// Documented shape opacity, in percent.
pub const SHAPE_DEFAULT_OPACITY: f32 = 100.0;

/// Smallest accepted shape size component, in pixels.
pub const SHAPE_MIN_SIZE: f32 = 0.0;
/// Largest accepted shape size component, in pixels (enough for 8K frames).
pub const SHAPE_MAX_SIZE: f32 = 8192.0;
/// Smallest accepted normalized position component.
pub const SHAPE_MIN_POSITION: f32 = 0.0;
/// Largest accepted normalized position component.
pub const SHAPE_MAX_POSITION: f32 = 1.0;
/// Smallest accepted corner radius, in pixels.
pub const SHAPE_MIN_CORNER_RADIUS: f32 = 0.0;
/// Largest accepted corner radius, in pixels.
pub const SHAPE_MAX_CORNER_RADIUS: f32 = 500.0;
/// Smallest accepted vertex count.
pub const SHAPE_MIN_VERTICES: f32 = 3.0;
/// Largest accepted vertex count.
pub const SHAPE_MAX_VERTICES: f32 = 20.0;
/// Smallest accepted star inner ratio.
pub const SHAPE_MIN_INNER_RATIO: f32 = 0.0;
/// Largest accepted star inner ratio.
pub const SHAPE_MAX_INNER_RATIO: f32 = 1.0;
/// Smallest accepted opacity, in percent.
pub const SHAPE_MIN_OPACITY: f32 = 0.0;
/// Largest accepted opacity, in percent.
pub const SHAPE_MAX_OPACITY: f32 = 100.0;
/// Smallest accepted stroke width, in pixels.
pub const SHAPE_MIN_STROKE_WIDTH: f32 = 0.0;
/// Largest accepted stroke width, in pixels.
pub const SHAPE_MAX_STROKE_WIDTH: f32 = 100.0;
/// Largest accepted rotation magnitude, in degrees.
pub const SHAPE_MAX_ROTATION: f32 = 360.0;

/// Smallest inner radius factor of a star, so the fold never collapses.
const MIN_STAR_INNER_RATIO: f32 = 0.001;
/// Guard against a zero-length segment denominator.
const SEGMENT_EPSILON: f32 = 1e-12;
/// The largest supported regular polygon.
const MAX_VERTICES: usize = 20;

/// Which outline the generator draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    /// Axis-aligned rectangle with an optional corner radius.
    Rectangle,
    /// Circle inscribed in the authored size box.
    Circle,
    /// Ellipse filling the authored size box.
    Ellipse,
    /// Regular polygon with `vertices` corners.
    Polygon,
    /// Star with `vertices` points and an inner radius ratio.
    Star,
    /// Horizontal line whose thickness is the stroke width.
    Line,
}

impl ShapeKind {
    /// Every kind in display order.
    pub const ALL: [Self; 6] = [
        Self::Rectangle,
        Self::Circle,
        Self::Ellipse,
        Self::Polygon,
        Self::Star,
        Self::Line,
    ];

    /// Stable persisted key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Rectangle => "rectangle",
            Self::Circle => "circle",
            Self::Ellipse => "ellipse",
            Self::Polygon => "polygon",
            Self::Star => "star",
            Self::Line => "line",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.as_key() == key)
    }

    /// Packed tag used by the point-op uniform.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Rectangle => 0,
            Self::Circle => 1,
            Self::Ellipse => 2,
            Self::Polygon => 3,
            Self::Star => 4,
            Self::Line => 5,
        }
    }

    /// Chinese menu caption.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rectangle => "矩形",
            Self::Circle => "圆",
            Self::Ellipse => "椭圆",
            Self::Polygon => "多边形",
            Self::Star => "星形",
            Self::Line => "线",
        }
    }
}

/// Where the stroke band sits relative to the outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeStrokePosition {
    /// The band lies entirely inside the outline.
    Inside,
    /// The band straddles the outline.
    Centred,
    /// The band lies entirely outside the outline.
    Outside,
}

impl ShapeStrokePosition {
    /// Every position in display order.
    pub const ALL: [Self; 3] = [Self::Inside, Self::Centred, Self::Outside];

    /// Stable persisted key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Inside => "inside",
            Self::Centred => "centred",
            Self::Outside => "outside",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.as_key() == key)
    }

    /// Packed tag used by the point-op uniform.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Inside => 0,
            Self::Centred => 1,
            Self::Outside => 2,
        }
    }

    /// Chinese menu caption.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Inside => "内",
            Self::Centred => "居中",
            Self::Outside => "外",
        }
    }
}

/// How the shape combines with the source frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeBooleanMode {
    /// Composite the shape over the source.
    Add,
    /// Keep only the source outside the shape.
    Subtract,
    /// Keep only the source inside the shape.
    Intersect,
    /// Keep whichever side contributes, never both.
    Exclude,
}

impl ShapeBooleanMode {
    /// Every mode in display order.
    pub const ALL: [Self; 4] = [Self::Add, Self::Subtract, Self::Intersect, Self::Exclude];

    /// Stable persisted key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Intersect => "intersect",
            Self::Exclude => "exclude",
        }
    }

    /// Inverse of [`Self::as_key`].
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|value| value.as_key() == key)
    }

    /// Packed tag used by the point-op uniform.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Add => 0,
            Self::Subtract => 1,
            Self::Intersect => 2,
            Self::Exclude => 3,
        }
    }

    /// Chinese menu caption.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Add => "相加",
            Self::Subtract => "相减",
            Self::Intersect => "交集",
            Self::Exclude => "排除",
        }
    }
}

/// One authored shape configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeParameters {
    /// Which outline to draw.
    pub shape_type: ShapeKind,
    /// Bounding box of the shape, in pixels.
    pub size: [f32; 2],
    /// Shape centre, normalized to the frame.
    pub position: [f32; 2],
    /// Rectangle corner radius, in pixels.
    pub corner_radius: f32,
    /// Polygon or star vertex count.
    pub vertices: f32,
    /// Star inner radius as a fraction of the outer radius.
    pub inner_ratio: f32,
    /// Fill colour (straight RGBA); alpha multiplies fill and overall opacity.
    pub fill_colour: [f32; 4],
    /// Fill opacity, in percent.
    pub fill_opacity: f32,
    /// Stroke width, in pixels.
    pub stroke_width: f32,
    /// Stroke colour (straight RGBA); alpha multiplies overall opacity.
    pub stroke_colour: [f32; 4],
    /// Where the stroke band sits relative to the outline.
    pub stroke_position: ShapeStrokePosition,
    /// Rotation in degrees.
    pub rotation: f32,
    /// Overall shape opacity, in percent.
    pub opacity: f32,
    /// How the shape combines with the source frame.
    pub boolean_mode: ShapeBooleanMode,
}

impl Default for ShapeParameters {
    fn default() -> Self {
        Self {
            shape_type: ShapeKind::Rectangle,
            size: SHAPE_DEFAULT_SIZE,
            position: SHAPE_DEFAULT_POSITION,
            corner_radius: SHAPE_DEFAULT_CORNER_RADIUS,
            vertices: SHAPE_DEFAULT_VERTICES,
            inner_ratio: SHAPE_DEFAULT_INNER_RATIO,
            fill_colour: SHAPE_DEFAULT_FILL_COLOUR,
            fill_opacity: SHAPE_DEFAULT_FILL_OPACITY,
            stroke_width: SHAPE_DEFAULT_STROKE_WIDTH,
            stroke_colour: SHAPE_DEFAULT_STROKE_COLOUR,
            stroke_position: ShapeStrokePosition::Centred,
            rotation: SHAPE_DEFAULT_ROTATION,
            opacity: SHAPE_DEFAULT_OPACITY,
            boolean_mode: ShapeBooleanMode::Add,
        }
    }
}

/// Reports the first authored value that falls outside its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("shape.{parameter} must be finite and within its documented range")]
pub struct ShapeError {
    parameter: &'static str,
}

impl ShapeError {
    /// Stable snake_case id of the offending parameter.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// The six fused point-op uniform cells produced by
/// [`ShapeGrade::gpu_controls`].
pub type ShapeGpuControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// A validated shape configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeGrade {
    parameters: ShapeParameters,
}

impl ShapeGrade {
    /// Validates one authored configuration.
    ///
    /// Every float must be finite and inside its documented range. The vertex
    /// count is accepted as any float in `3..=20` and rounded when the kernel
    /// folds it into the polygon or star, so the authored value stays readable
    /// in the UI while the geometry stays integral.
    pub fn new(parameters: ShapeParameters) -> Result<Self, ShapeError> {
        let numeric = [
            ("size_x", parameters.size[0], SHAPE_MIN_SIZE, SHAPE_MAX_SIZE),
            ("size_y", parameters.size[1], SHAPE_MIN_SIZE, SHAPE_MAX_SIZE),
            (
                "position_x",
                parameters.position[0],
                SHAPE_MIN_POSITION,
                SHAPE_MAX_POSITION,
            ),
            (
                "position_y",
                parameters.position[1],
                SHAPE_MIN_POSITION,
                SHAPE_MAX_POSITION,
            ),
            (
                "corner_radius",
                parameters.corner_radius,
                SHAPE_MIN_CORNER_RADIUS,
                SHAPE_MAX_CORNER_RADIUS,
            ),
            (
                "vertices",
                parameters.vertices,
                SHAPE_MIN_VERTICES,
                SHAPE_MAX_VERTICES,
            ),
            (
                "inner_ratio",
                parameters.inner_ratio,
                SHAPE_MIN_INNER_RATIO,
                SHAPE_MAX_INNER_RATIO,
            ),
            (
                "fill_opacity",
                parameters.fill_opacity,
                SHAPE_MIN_OPACITY,
                SHAPE_MAX_OPACITY,
            ),
            (
                "stroke_width",
                parameters.stroke_width,
                SHAPE_MIN_STROKE_WIDTH,
                SHAPE_MAX_STROKE_WIDTH,
            ),
            (
                "rotation",
                parameters.rotation,
                -SHAPE_MAX_ROTATION,
                SHAPE_MAX_ROTATION,
            ),
            (
                "opacity",
                parameters.opacity,
                SHAPE_MIN_OPACITY,
                SHAPE_MAX_OPACITY,
            ),
        ];
        for (parameter, value, minimum, maximum) in numeric {
            if !value.is_finite() || value < minimum || value > maximum {
                return Err(ShapeError { parameter });
            }
        }
        for (parameter, colour) in [
            ("fill_colour", parameters.fill_colour),
            ("stroke_colour", parameters.stroke_colour),
        ] {
            for component in colour {
                if !component.is_finite() || !(0.0..=1.0).contains(&component) {
                    return Err(ShapeError { parameter });
                }
            }
        }
        Ok(Self { parameters })
    }

    /// Documented defaults.
    pub fn documented_defaults() -> Self {
        Self::new(ShapeParameters::default()).expect("the documented shape defaults are in range")
    }

    /// The authored values.
    pub const fn parameters(&self) -> &ShapeParameters {
        &self.parameters
    }

    /// Whether the configuration leaves every pixel untouched.
    ///
    /// Zero shape coverage is the identity for Add, Subtract and Exclude, but
    /// clears the source for Intersect. No Intersect configuration is assumed
    /// to cover every possible frame. Lines ignore fill and retain a one-pixel
    /// stroke even at zero authored width.
    pub const fn is_identity(&self) -> bool {
        if matches!(self.parameters.boolean_mode, ShapeBooleanMode::Intersect) {
            return false;
        }
        let is_line = matches!(self.parameters.shape_type, ShapeKind::Line);
        let empty_fill =
            is_line || self.parameters.fill_opacity <= 0.0 || self.parameters.fill_colour[3] <= 0.0;
        let empty_stroke = self.parameters.stroke_colour[3] <= 0.0
            || (self.parameters.stroke_width <= 0.0 && !is_line);
        self.parameters.opacity <= 0.0
            || (empty_fill && empty_stroke)
            || (!is_line && (self.parameters.size[0] <= 0.0 || self.parameters.size[1] <= 0.0))
    }

    /// Relative cost of one point pass.
    pub const fn cost(&self) -> u8 {
        1
    }

    /// Packs the grade into the fused point-op uniform cells.
    ///
    /// The cells are `[half width, half height, corner radius, vertices]`,
    /// `[inner ratio, stroke width, rotation in radians, opacity]`,
    /// `[fill opacity, kind tag, stroke position tag, boolean tag]`,
    /// `[fill rgba]`, `[stroke rgba]`, and `[position x, position y, 0, 0]`.
    /// Rotation is converted from degrees to radians here. Frame size is not
    /// repeated: the renderer supplies it through `uniforms.geometry.zw`.
    pub fn gpu_controls(&self) -> ShapeGpuControls {
        let parameters = &self.parameters;
        let geometry = [
            (parameters.size[0] * 0.5).max(0.0),
            (parameters.size[1] * 0.5).max(0.0),
            parameters.corner_radius.clamp(
                0.0,
                (parameters.size[0].min(parameters.size[1]) * 0.5).max(0.0),
            ),
            parameters.vertices.round().clamp(SHAPE_MIN_VERTICES, SHAPE_MAX_VERTICES),
        ];
        let outline = [
            parameters.inner_ratio.clamp(SHAPE_MIN_INNER_RATIO, SHAPE_MAX_INNER_RATIO),
            parameters.stroke_width,
            parameters.rotation.to_radians(),
            parameters.opacity / 100.0,
        ];
        let modes = [
            parameters.fill_opacity / 100.0,
            f32::from(parameters.shape_type.as_u8()),
            f32::from(parameters.stroke_position.as_u8()),
            f32::from(parameters.boolean_mode.as_u8()),
        ];
        let centre = [parameters.position[0], parameters.position[1], 0.0, 0.0];
        (
            geometry,
            outline,
            modes,
            parameters.fill_colour,
            parameters.stroke_colour,
            centre,
        )
    }

    /// Feeds the whole configuration into a graph or plan fingerprint.
    pub(crate) fn hash_state<H: Hasher>(&self, state: &mut H) {
        let parameters = &self.parameters;
        for value in [
            parameters.size[0],
            parameters.size[1],
            parameters.position[0],
            parameters.position[1],
            parameters.corner_radius,
            parameters.vertices,
            parameters.inner_ratio,
            parameters.fill_opacity,
            parameters.stroke_width,
            parameters.rotation,
            parameters.opacity,
        ] {
            value.to_bits().hash(state);
        }
        for component in parameters.fill_colour.into_iter().chain(parameters.stroke_colour) {
            component.to_bits().hash(state);
        }
        parameters.shape_type.as_u8().hash(state);
        parameters.stroke_position.as_u8().hash(state);
        parameters.boolean_mode.as_u8().hash(state);
    }
}

/// Applies the shape generator to one tightly packed straight-alpha f32 frame.
pub fn shape_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ShapeGrade,
) -> bool {
    apply_shape_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<_, std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Cancellation-aware shape kernel over one tightly packed frame.
pub(crate) fn apply_shape_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ShapeGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    checkpoint()?;

    let parameters = &grade.parameters;
    let kind = parameters.shape_type;
    let is_line = matches!(kind, ShapeKind::Line);
    let half_width = (parameters.size[0] * 0.5).max(0.0);
    let half_height = (parameters.size[1] * 0.5).max(0.0);
    let centre = [
        parameters.position[0] * width as f32,
        parameters.position[1] * height as f32,
    ];
    let angle = parameters.rotation.to_radians();
    let (sin_angle, cos_angle) = angle.sin_cos();

    // Every shape but the line collapses to the empty set once either half
    // extent vanishes; the line keeps its one-pixel spine instead.
    let degenerate = half_width.min(half_height) <= 0.0 && !is_line;

    let corner_radius = parameters.corner_radius.clamp(0.0, half_width.min(half_height).max(0.0));
    let sides = parameters.vertices.round().clamp(SHAPE_MIN_VERTICES, SHAPE_MAX_VERTICES) as usize;
    let apothem = half_width.min(half_height) * (std::f32::consts::PI / sides as f32).cos();
    let mut normals = [[0.0_f32; 2]; MAX_VERTICES];
    for (index, normal) in normals.iter_mut().take(sides).enumerate() {
        let normal_angle = (2.0 * index as f32 + 1.0) * std::f32::consts::PI / sides as f32;
        *normal = [normal_angle.cos(), normal_angle.sin()];
    }

    let star_radius = half_width.min(half_height);
    let star_inner =
        star_radius * parameters.inner_ratio.clamp(MIN_STAR_INNER_RATIO, SHAPE_MAX_INNER_RATIO);
    let sector = std::f32::consts::PI / sides as f32;
    let period = sector + sector;
    let star_vertex = [star_radius, 0.0];
    let star_inner_upper = [star_inner * sector.cos(), star_inner * sector.sin()];
    let star_inner_lower = [star_inner * sector.cos(), -star_inner * sector.sin()];

    let stroke_width = if is_line {
        parameters.stroke_width.max(1.0)
    } else {
        parameters.stroke_width
    };
    let half_stroke = stroke_width * 0.5;
    let stroke_position = if is_line {
        ShapeStrokePosition::Centred
    } else {
        parameters.stroke_position
    };

    let fill_gain = parameters.fill_opacity / 100.0;
    let shape_gain = parameters.opacity / 100.0;
    let fill_colour = parameters.fill_colour;
    let stroke_colour = parameters.stroke_colour;
    let boolean_mode = parameters.boolean_mode;

    crate::parallel::for_each_row_band(working, width, 0..height, |row_start, band| {
        for (index, pixel) in band.iter_mut().enumerate() {
            let row = row_start + index / width;
            let column = index % width;
            let x = column as f32 + 0.5;
            let y = row as f32 + 0.5;
            let dx = x - centre[0];
            let dy = y - centre[1];
            let qx = dx * cos_angle + dy * sin_angle;
            let qy = dy * cos_angle - dx * sin_angle;

            let distance = if degenerate {
                f32::INFINITY
            } else {
                match kind {
                    ShapeKind::Rectangle => {
                        let inner_x = qx.abs() - (half_width - corner_radius);
                        let inner_y = qy.abs() - (half_height - corner_radius);
                        let outside_x = inner_x.max(0.0);
                        let outside_y = inner_y.max(0.0);
                        let outside = (outside_x * outside_x + outside_y * outside_y).sqrt();
                        outside + inner_x.max(inner_y).min(0.0) - corner_radius
                    }
                    ShapeKind::Circle => (qx * qx + qy * qy).sqrt() - half_width.min(half_height),
                    ShapeKind::Ellipse => {
                        let axis_x = half_width.max(f32::MIN_POSITIVE);
                        let axis_y = half_height.max(f32::MIN_POSITIVE);
                        let unit_x = qx / axis_x;
                        let unit_y = qy / axis_y;
                        ((unit_x * unit_x + unit_y * unit_y).sqrt() - 1.0) * axis_x.min(axis_y)
                    }
                    ShapeKind::Polygon => {
                        normals.iter().take(sides).fold(f32::NEG_INFINITY, |furthest, normal| {
                            furthest.max(qx * normal[0] + qy * normal[1] - apothem)
                        })
                    }
                    ShapeKind::Star => {
                        let radius = (qx * qx + qy * qy).sqrt();
                        let angle = qy.atan2(qx);
                        let folded = angle.rem_euclid(period);
                        let within = if folded > sector {
                            period - folded
                        } else {
                            folded
                        };
                        let point = [radius * within.cos(), radius * within.sin()];
                        let upper = point_segment_distance(point, star_vertex, star_inner_upper);
                        let lower = point_segment_distance(point, star_vertex, star_inner_lower);
                        let closest = upper.min(lower);
                        let inside = cross(
                            star_inner_upper[0] - star_vertex[0],
                            star_inner_upper[1] - star_vertex[1],
                            point[0] - star_vertex[0],
                            point[1] - star_vertex[1],
                        ) * cross(
                            star_inner_upper[0] - star_vertex[0],
                            star_inner_upper[1] - star_vertex[1],
                            -star_vertex[0],
                            -star_vertex[1],
                        ) > 0.0;
                        if inside {
                            -closest
                        } else {
                            closest
                        }
                    }
                    ShapeKind::Line => {
                        point_segment_distance([qx, qy], [-half_width, 0.0], [half_width, 0.0])
                    }
                }
            };

            let fill_coverage = if is_line {
                0.0
            } else {
                (0.5 - distance).clamp(0.0, 1.0)
            };
            let stroke_coverage = if stroke_width <= 0.0 {
                0.0
            } else {
                let signed = match stroke_position {
                    ShapeStrokePosition::Inside => (-stroke_width - distance).max(distance),
                    ShapeStrokePosition::Centred => {
                        (-half_stroke - distance).max(distance - half_stroke)
                    }
                    ShapeStrokePosition::Outside => (-distance).max(distance - stroke_width),
                };
                (0.5 - signed).clamp(0.0, 1.0)
            };

            let fill_alpha = fill_coverage * fill_gain * shape_gain * fill_colour[3];
            let stroke_alpha = stroke_coverage * shape_gain * stroke_colour[3];
            let coverage = (stroke_alpha + fill_alpha * (1.0 - stroke_alpha)).clamp(0.0, 1.0);
            // At zero coverage the four modes disagree: `intersect` must clear
            // the source rather than leave it alone, so only the three modes
            // that reduce to the identity here may skip the pixel.
            if coverage <= 0.0 && boolean_mode != ShapeBooleanMode::Intersect {
                if pixel[3] == 0.0 {
                    *pixel = [0.0; 4];
                }
                continue;
            }
            let shape_colour = [
                stroke_colour[0] * stroke_alpha
                    + fill_colour[0] * fill_alpha * (1.0 - stroke_alpha),
                stroke_colour[1] * stroke_alpha
                    + fill_colour[1] * fill_alpha * (1.0 - stroke_alpha),
                stroke_colour[2] * stroke_alpha
                    + fill_colour[2] * fill_alpha * (1.0 - stroke_alpha),
            ];

            // The CPU working seam is straight alpha. Associate the source for
            // Porter-Duff algebra, then unassociate once at the output seam.
            let alpha = pixel[3].clamp(0.0, 1.0);
            let (source_weight, shape_weight) = match boolean_mode {
                ShapeBooleanMode::Add => (1.0 - coverage, 1.0),
                ShapeBooleanMode::Subtract => (1.0 - coverage, 0.0),
                ShapeBooleanMode::Intersect => (coverage, 0.0),
                ShapeBooleanMode::Exclude => (1.0 - coverage, 1.0 - alpha),
            };
            let source_alpha = alpha * source_weight;
            let output_alpha = source_alpha + coverage * shape_weight;
            *pixel = crate::coverage::straight_rgba_from_premultiplied([
                pixel[0] * source_alpha + shape_colour[0] * shape_weight,
                pixel[1] * source_alpha + shape_colour[1] * shape_weight,
                pixel[2] * source_alpha + shape_colour[2] * shape_weight,
                output_alpha,
            ]);
        }
    });

    checkpoint()?;
    Ok(true)
}

/// Two-dimensional cross product `a × b`.
fn cross(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    ax * by - ay * bx
}

/// Distance from `point` to the segment `a -> b`.
///
/// The degenerate branch is explicit rather than a clamped denominator so a
/// zero-length segment reports the distance to its endpoint instead of a
/// division artifact.
fn point_segment_distance(point: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [point[0] - a[0], point[1] - a[1]];
    let denominator = ab[0] * ab[0] + ab[1] * ab[1];
    let offset = if denominator <= SEGMENT_EPSILON {
        [0.0, 0.0]
    } else {
        let projection = (ap[0] * ab[0] + ap[1] * ab[1]) / denominator;
        let clamped = projection.clamp(0.0, 1.0);
        [ab[0] * clamped, ab[1] * clamped]
    };
    let dx = ap[0] - offset[0];
    let dy = ap[1] - offset[1];
    (dx * dx + dy * dy).sqrt()
}

#[cfg(test)]
mod tests {
    // The fixtures start from a valid grade and then mutate the one parameter
    // under test, which reads clearer than a full literal.
    #![allow(clippy::field_reassign_with_default)]

    use super::*;

    fn valid_grade(parameters: ShapeParameters) -> ShapeGrade {
        ShapeGrade::new(parameters).expect("valid shape grade")
    }

    fn filled(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.2, 0.4, 0.6, 1.0]; width * height]
    }

    fn pixel(frame: &[[f32; 4]], width: usize, column: usize, row: usize) -> [f32; 4] {
        frame[row * width + column]
    }

    fn render(width: usize, height: usize, grade: &ShapeGrade) -> Vec<[f32; 4]> {
        let mut frame = filled(width, height);
        assert!(shape_frame_f32(&mut frame, width, height, grade));
        frame
    }

    fn assert_close(actual: f32, expected: f32, tolerance: f32) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn documented_defaults_are_the_documented_shape() {
        let grade = ShapeGrade::documented_defaults();
        assert_eq!(grade.parameters().shape_type, ShapeKind::Rectangle);
        assert_eq!(grade.parameters().size, [200.0, 200.0]);
        assert_eq!(grade.parameters().position, [0.5, 0.5]);
        assert_eq!(grade.parameters().corner_radius, 0.0);
        assert_eq!(grade.parameters().vertices, 5.0);
        assert_eq!(grade.parameters().inner_ratio, 0.5);
        assert_eq!(grade.parameters().fill_colour, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(grade.parameters().fill_opacity, 100.0);
        assert_eq!(grade.parameters().stroke_width, 0.0);
        assert_eq!(grade.parameters().stroke_colour, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            grade.parameters().stroke_position,
            ShapeStrokePosition::Centred
        );
        assert_eq!(grade.parameters().rotation, 0.0);
        assert_eq!(grade.parameters().opacity, 100.0);
        assert_eq!(grade.parameters().boolean_mode, ShapeBooleanMode::Add);
        assert_eq!(grade.cost(), 1);
        assert!(!grade.is_identity());
    }

    #[test]
    fn rectangle_covers_the_inside_and_anti_aliases_the_edge() {
        let mut parameters = ShapeParameters::default();
        parameters.size = [9.0, 9.0];
        let grade = valid_grade(parameters);
        let frame = render(16, 16, &grade);

        // Centre pixel: three and a half pixels inside the outline.
        assert_eq!(pixel(&frame, 16, 8, 8), [1.0, 1.0, 1.0, 1.0]);
        // The outline lands exactly on the centre of column 3 / row 8.
        assert_close(pixel(&frame, 16, 3, 8)[0], 0.6, 1e-3);
        assert_close(pixel(&frame, 16, 3, 8)[1], 0.7, 1e-3);
        assert_close(pixel(&frame, 16, 3, 8)[2], 0.8, 1e-3);
        assert_close(pixel(&frame, 16, 3, 8)[3], 1.0, 1e-6);
        // One pixel further out the coverage is gone and the source survives.
        assert_eq!(pixel(&frame, 16, 2, 8), [0.2, 0.4, 0.6, 1.0]);
        assert_eq!(pixel(&frame, 16, 8, 2), [0.2, 0.4, 0.6, 1.0]);
    }

    #[test]
    fn rectangle_corner_radius_rounds_the_corner_off() {
        let mut square = ShapeParameters::default();
        square.size = [9.0, 9.0];
        let square = valid_grade(square);
        let mut rounded = ShapeParameters::default();
        rounded.size = [9.0, 9.0];
        rounded.corner_radius = 100.0;
        let rounded = valid_grade(rounded);

        let sharp = render(16, 16, &square);
        let round = render(16, 16, &rounded);
        // The corner pixel is fully covered by the square and clipped away by
        // the radius, which the parameter validator would otherwise accept.
        assert_eq!(pixel(&sharp, 16, 4, 4), [1.0, 1.0, 1.0, 1.0]);
        // The radius is clamped to min(hx, hy) = 4.5, so the distance at the
        // corner is sqrt(3.5² + 3.5²) - 4.5 = 0.4497 and the coverage 0.0503.
        assert_close(pixel(&round, 16, 4, 4)[0], 0.2402, 1e-3);
        assert_close(pixel(&round, 16, 4, 4)[2], 0.6201, 1e-3);
    }

    #[test]
    fn circle_radius_follows_the_shorter_axis() {
        let mut parameters = ShapeParameters::default();
        parameters.shape_type = ShapeKind::Circle;
        parameters.size = [10.0, 10.0];
        let grade = valid_grade(parameters);
        let frame = render(16, 16, &grade);

        // Centre of pixel (12, 8) is 4.5 px from the centre: 0.9723 coverage.
        assert_close(pixel(&frame, 16, 12, 8)[0], 0.2 + 0.8 * 0.9723, 1e-3);
        // Centre of pixel (13, 8) is 5.5227 px away, past the 5 px radius.
        assert_eq!(pixel(&frame, 16, 13, 8), [0.2, 0.4, 0.6, 1.0]);
    }

    #[test]
    fn ellipse_uses_the_documented_scaled_radial_field() {
        let mut parameters = ShapeParameters::default();
        parameters.shape_type = ShapeKind::Ellipse;
        parameters.size = [12.0, 6.0];
        let grade = valid_grade(parameters);
        let frame = render(16, 16, &grade);

        // Near the major axis: q = (5.5, 0.5), k = 0.93169, d = -0.2049.
        assert_close(pixel(&frame, 16, 13, 8)[0], 0.2 + 0.8 * 0.7049, 1e-3);
        // Near the minor axis: q = (0.5, 2.5), k = 0.83749, d = -0.4875.
        assert_close(pixel(&frame, 16, 8, 10)[0], 0.2 + 0.8 * 0.9875, 1e-3);
        // Outside near the minor axis: q = (0.5, 3.5), k = 1.16908, d = 0.5072.
        assert_eq!(pixel(&frame, 16, 8, 11), [0.2, 0.4, 0.6, 1.0]);
    }

    #[test]
    fn polygon_three_five_and_twenty_reach_their_circumscribed_circle() {
        // Frame 33x33 puts the shape centre on the pixel centre 16.5, so the
        // probe pixels sit exactly on the local axes.
        let probe = |vertices: f32, column: usize| {
            let mut parameters = ShapeParameters::default();
            parameters.shape_type = ShapeKind::Polygon;
            parameters.size = [20.0, 20.0];
            parameters.vertices = vertices;
            let grade = valid_grade(parameters);
            let frame = render(33, 33, &grade);
            pixel(&frame, 33, column, 16)[0]
        };

        // Every polygon touches its circumscribed circle at every vertex, and
        // the first vertex points at +X: d = 0 on the outline.
        for vertices in [3.0, 5.0, 20.0] {
            assert_close(probe(vertices, 26), 0.2 + 0.8 * 0.5, 1e-3);
        }
        // On the -X side the boundary is the apothem. At q = (-8, 0) the
        // signed distance is 8 - 10 cos(π/n), which separates all three:
        // triangle +3 (outside), pentagon -0.09 (just inside), 20-gon -1.88.
        assert_close(probe(3.0, 8), 0.2, 1e-3);
        assert_close(probe(5.0, 8), 0.2 + 0.8 * 0.59017, 1e-3);
        assert_close(probe(20.0, 8), 1.0, 1e-3);
    }

    #[test]
    fn star_inner_ratio_moves_the_fold() {
        let mut wide = ShapeParameters::default();
        wide.shape_type = ShapeKind::Star;
        wide.size = [20.0, 20.0];
        wide.inner_ratio = 0.5;
        let wide = valid_grade(wide);
        let mut narrow = *wide.parameters();
        narrow.inner_ratio = 0.1;
        let narrow = valid_grade(narrow);

        let wide_frame = render(33, 33, &wide);
        let narrow_frame = render(33, 33, &narrow);
        // q = (3, 3) folds into the 36° sector at |q| = 4.24. With an inner
        // radius of 5 the point is inside the body; at 1 it is beyond the edge.
        assert_close(pixel(&wide_frame, 33, 19, 19)[0], 1.0, 1e-6);
        assert_eq!(pixel(&narrow_frame, 33, 19, 19), [0.2, 0.4, 0.6, 1.0]);
        // The outer vertex at (10, 0) is exactly on the outline in both.
        assert_close(pixel(&wide_frame, 33, 26, 16)[0], 0.2 + 0.8 * 0.5, 1e-3);
        assert_close(pixel(&narrow_frame, 33, 26, 16)[0], 0.2 + 0.8 * 0.5, 1e-3);
        // One pixel past the vertex is outside.
        assert_eq!(pixel(&wide_frame, 33, 27, 16), [0.2, 0.4, 0.6, 1.0]);
    }

    #[test]
    fn line_keeps_a_one_pixel_spine_at_zero_width() {
        let mut parameters = ShapeParameters::default();
        parameters.shape_type = ShapeKind::Line;
        parameters.size = [20.0, 20.0];
        parameters.stroke_width = 0.0;
        parameters.stroke_colour = [0.0, 0.0, 0.0, 1.0];
        let grade = valid_grade(parameters);
        let mut frame = vec![[1.0, 1.0, 1.0, 1.0]; 33 * 33];
        assert!(shape_frame_f32(&mut frame, 33, 33, &grade));

        // The unsigned distance is zero on the spine: half-width is applied
        // only once, so exactly the centre row is covered for a 1 px capsule.
        assert_close(pixel(&frame, 33, 16, 16)[0], 0.0, 1e-6);
        assert_close(pixel(&frame, 33, 16, 17)[0], 1.0, 1e-6);
        assert_close(pixel(&frame, 33, 16, 15)[0], 1.0, 1e-6);
        assert_eq!(pixel(&frame, 33, 16, 18), [1.0, 1.0, 1.0, 1.0]);
        // The line has no fill: the fill colour never appears.
        assert_eq!(pixel(&frame, 33, 16, 16)[3], 1.0);
    }

    #[test]
    fn zero_size_leaves_every_shape_but_the_line_empty() {
        for kind in [
            ShapeKind::Rectangle,
            ShapeKind::Circle,
            ShapeKind::Ellipse,
            ShapeKind::Polygon,
            ShapeKind::Star,
        ] {
            let mut parameters = ShapeParameters::default();
            parameters.shape_type = kind;
            parameters.size = [0.0, 0.0];
            parameters.stroke_width = 4.0;
            let grade = valid_grade(parameters);
            let mut frame = filled(16, 16);
            assert!(shape_frame_f32(&mut frame, 16, 16, &grade));
            assert_eq!(frame, filled(16, 16), "{kind:?} must stay empty");
        }

        let mut parameters = ShapeParameters::default();
        parameters.shape_type = ShapeKind::Line;
        parameters.size = [0.0, 0.0];
        let grade = valid_grade(parameters);
        let mut frame = filled(16, 16);
        assert!(shape_frame_f32(&mut frame, 16, 16, &grade));
        // A zero-length spine still stamps one pixel at the centre.
        assert_ne!(pixel(&frame, 16, 8, 8), [0.2, 0.4, 0.6, 1.0]);
    }

    #[test]
    fn stroke_positions_split_inside_centred_and_outside() {
        // Columns 2, 3, 4 sit at q.x = -5.5, -4.5, -3.5 with the outline at
        // q.x = ±4.5, so d is +1, 0 and -1: the three positions must be exact
        // mirror images of each other. The source is black and the stroke
        // white, so the red channel is the stroke coverage itself.
        let probe = |position: ShapeStrokePosition| {
            let mut parameters = ShapeParameters::default();
            parameters.size = [9.0, 9.0];
            parameters.fill_opacity = 0.0;
            parameters.stroke_width = 2.0;
            parameters.stroke_colour = [1.0, 1.0, 1.0, 1.0];
            parameters.stroke_position = position;
            let grade = valid_grade(parameters);
            let mut frame = vec![[0.0, 0.0, 0.0, 1.0]; 16 * 16];
            assert!(shape_frame_f32(&mut frame, 16, 16, &grade));
            [
                pixel(&frame, 16, 2, 8)[0],
                pixel(&frame, 16, 3, 8)[0],
                pixel(&frame, 16, 4, 8)[0],
            ]
        };

        let inside = probe(ShapeStrokePosition::Inside);
        assert_close(inside[0], 0.0, 1e-6);
        assert_close(inside[1], 0.5, 1e-6);
        assert_close(inside[2], 1.0, 1e-6);

        let centred = probe(ShapeStrokePosition::Centred);
        assert_close(centred[0], 0.5, 1e-6);
        assert_close(centred[1], 1.0, 1e-6);
        assert_close(centred[2], 0.5, 1e-6);

        let outside = probe(ShapeStrokePosition::Outside);
        assert_close(outside[0], 1.0, 1e-6);
        assert_close(outside[1], 0.5, 1e-6);
        assert_close(outside[2], 0.0, 1e-6);
    }

    #[test]
    fn boolean_modes_return_straight_alpha_results() {
        // Frame 8x1, shape 5x2 centred at x = 4: coverage is
        // [0, 0.5, 1, 1, 1, 1, 0.5, 0] across the row.
        let mut shape = ShapeParameters::default();
        shape.size = [5.0, 2.0];
        shape.fill_colour = [1.0, 1.0, 1.0, 1.0];
        shape.fill_opacity = 100.0;
        shape.opacity = 100.0;

        let run = |mode: ShapeBooleanMode| {
            let mut parameters = shape;
            parameters.boolean_mode = mode;
            let grade = valid_grade(parameters);
            let mut frame = vec![[0.2, 0.4, 0.6, 1.0]; 8];
            // A straight-alpha source with half alpha inside the shape.
            frame[2] = [0.2, 0.4, 0.6, 0.5];
            assert!(shape_frame_f32(&mut frame, 8, 1, &grade));
            frame
        };

        let add = run(ShapeBooleanMode::Add);
        assert_close(add[1][0], 0.6, 1e-6);
        assert_close(add[1][1], 0.7, 1e-6);
        assert_close(add[1][2], 0.8, 1e-6);
        assert_close(add[1][3], 1.0, 1e-6);
        assert_eq!(add[2], [1.0, 1.0, 1.0, 1.0]);

        let subtract = run(ShapeBooleanMode::Subtract);
        assert_close(subtract[1][0], 0.2, 1e-6);
        assert_close(subtract[1][3], 0.5, 1e-6);
        assert_eq!(subtract[2], [0.0, 0.0, 0.0, 0.0]);

        let intersect = run(ShapeBooleanMode::Intersect);
        assert_close(intersect[1][0], 0.2, 1e-6);
        assert_close(intersect[1][3], 0.5, 1e-6);
        assert_eq!(intersect[2], [0.2, 0.4, 0.6, 0.5]);
        assert_eq!(intersect[0], [0.0, 0.0, 0.0, 0.0]);

        let exclude = run(ShapeBooleanMode::Exclude);
        assert_close(exclude[1][0], 0.2, 1e-6);
        // Opaque source on both sides of the shape: the xor alpha is half.
        assert_close(exclude[1][3], 0.5, 1e-6);
        // Half-alpha source inside the shape keeps straight white at half alpha.
        assert_eq!(exclude[2], [1.0, 1.0, 1.0, 0.5]);
    }

    #[test]
    fn semitransparent_straight_hdr_source_matches_f64_porter_duff_reference() {
        for mode in ShapeBooleanMode::ALL {
            for source_alpha in [0.0, 0.5, 1.0, 1.0 / 65_535.0] {
                for shape_alpha in [0.25, 0.5, 1.0] {
                    let mut parameters = ShapeParameters::default();
                    parameters.fill_colour = [0.8, 0.2, 0.4, shape_alpha];
                    parameters.boolean_mode = mode;
                    let source = [4.0, -0.25, 0.5, source_alpha];
                    let mut frame = [source];
                    assert!(shape_frame_f32(&mut frame, 1, 1, &valid_grade(parameters)));

                    let a = f64::from(source_alpha);
                    let x = f64::from(shape_alpha);
                    let out_alpha = match mode {
                        ShapeBooleanMode::Add => x + a * (1.0 - x),
                        ShapeBooleanMode::Subtract => a * (1.0 - x),
                        ShapeBooleanMode::Intersect => a * x,
                        ShapeBooleanMode::Exclude => a * (1.0 - x) + x * (1.0 - a),
                    };
                    for channel in 0..3 {
                        let f = f64::from(source[channel]) * a;
                        let s = f64::from(parameters.fill_colour[channel]) * x;
                        let associated = match mode {
                            ShapeBooleanMode::Add => s + f * (1.0 - x),
                            ShapeBooleanMode::Subtract => f * (1.0 - x),
                            ShapeBooleanMode::Intersect => f * x,
                            ShapeBooleanMode::Exclude => f * (1.0 - x) + s * (1.0 - a),
                        };
                        let expected = if out_alpha > 0.0 {
                            associated / out_alpha
                        } else {
                            0.0
                        };
                        assert_close(frame[0][channel], expected as f32, 1e-6);
                    }
                    assert_close(frame[0][3], out_alpha as f32, 1e-6);
                }
            }
        }
    }

    #[test]
    fn fill_colour_alpha_multiplies_both_opacities_without_darkening_straight_rgb() {
        let mut parameters = ShapeParameters::default();
        parameters.fill_colour = [0.8, 0.2, 0.4, 0.5];
        parameters.fill_opacity = 50.0;
        parameters.opacity = 50.0;
        let mut frame = [[9.0, 8.0, 7.0, 0.0]];
        assert!(shape_frame_f32(&mut frame, 1, 1, &valid_grade(parameters)));
        assert_eq!(frame[0], [0.8, 0.2, 0.4, 0.125]);
    }

    #[test]
    fn line_stroke_colour_alpha_multiplies_opacity_and_ignores_fill() {
        let mut parameters = ShapeParameters::default();
        parameters.shape_type = ShapeKind::Line;
        parameters.stroke_colour = [0.2, 0.4, 0.8, 0.25];
        parameters.opacity = 50.0;
        let mut frame = [[9.0, 8.0, 7.0, 0.0]];
        assert!(shape_frame_f32(&mut frame, 1, 1, &valid_grade(parameters)));
        assert_eq!(frame[0], [0.2, 0.4, 0.8, 0.125]);
    }

    #[test]
    fn translucent_stroke_composites_over_translucent_fill() {
        let mut parameters = ShapeParameters::default();
        parameters.size = [2.0, 2.0];
        parameters.stroke_width = 4.0;
        parameters.fill_colour = [1.0, 0.0, 0.0, 0.5];
        parameters.stroke_colour = [0.0, 0.0, 1.0, 0.25];
        let mut frame = [[0.0; 4]];
        assert!(shape_frame_f32(&mut frame, 1, 1, &valid_grade(parameters)));
        for (actual, expected) in frame[0].into_iter().zip([0.6, 0.0, 0.4, 0.625]) {
            assert_close(actual, expected, 1e-6);
        }
    }

    #[test]
    fn intersect_with_zero_opacity_or_empty_shape_clears_instead_of_being_identity() {
        for kind in ShapeKind::ALL {
            for empty_case in 0..4 {
                let mut parameters = ShapeParameters::default();
                parameters.shape_type = kind;
                parameters.boolean_mode = ShapeBooleanMode::Intersect;
                match empty_case {
                    0 => parameters.opacity = 0.0,
                    1 => {
                        parameters.fill_opacity = 0.0;
                        parameters.stroke_colour[3] = 0.0;
                    }
                    2 => {
                        parameters.fill_colour[3] = 0.0;
                        parameters.stroke_colour[3] = 0.0;
                    }
                    _ if kind != ShapeKind::Line => parameters.size = [0.0, 10.0],
                    _ => continue, // A zero-length line is still a capsule.
                }
                let grade = valid_grade(parameters);
                assert!(!grade.is_identity(), "{kind:?}, case {empty_case}");
                let mut frame = [[0.8, 0.4, 0.2, 0.5], [9.0, 8.0, 7.0, 0.0]];
                assert!(shape_frame_f32(&mut frame, 2, 1, &grade));
                assert_eq!(frame, [[0.0; 4]; 2], "{kind:?}, case {empty_case}");
            }
        }
    }

    #[test]
    fn nonidentity_pass_clears_hidden_rgb_even_outside_coverage() {
        for mode in ShapeBooleanMode::ALL {
            let mut parameters = ShapeParameters::default();
            parameters.size = [2.0, 2.0];
            parameters.boolean_mode = mode;
            let mut frame = [[9.0, 8.0, 7.0, 0.0]; 5];
            assert!(shape_frame_f32(&mut frame, 5, 1, &valid_grade(parameters)));
            assert_eq!(frame[0], [0.0; 4], "{mode:?}");
            assert_eq!(frame[4], [0.0; 4], "{mode:?}");
        }
    }

    #[test]
    fn wide_line_is_a_solid_capsule_with_half_width_applied_once() {
        for position in ShapeStrokePosition::ALL {
            let mut parameters = ShapeParameters::default();
            parameters.shape_type = ShapeKind::Line;
            parameters.size = [8.0, 0.0];
            parameters.stroke_width = 6.0;
            parameters.stroke_position = position; // Ignored for a line.
            parameters.stroke_colour = [1.0; 4];
            let mut frame = vec![[0.0, 0.0, 0.0, 1.0]; 33 * 33];
            assert!(shape_frame_f32(
                &mut frame,
                33,
                33,
                &valid_grade(parameters)
            ));
            for row in 14..=18 {
                assert_eq!(pixel(&frame, 33, 16, row), [1.0; 4]);
            }
            assert_close(pixel(&frame, 33, 16, 19)[0], 0.5, 1e-6);
            assert_close(pixel(&frame, 33, 16, 20)[0], 0.0, 1e-6);
            // Endpoint at x=20: the round cap has the same radius as the body.
            assert_close(pixel(&frame, 33, 22, 16)[0], 1.0, 1e-6);
            assert_close(pixel(&frame, 33, 23, 16)[0], 0.5, 1e-6);
            assert_close(pixel(&frame, 33, 24, 16)[0], 0.0, 1e-6);
            assert_close(pixel(&frame, 33, 22, 18)[0], 3.5 - 8.0_f32.sqrt(), 1e-6);
        }
    }

    #[test]
    fn zero_opacity_is_the_identity() {
        let mut parameters = ShapeParameters::default();
        parameters.opacity = 0.0;
        let grade = valid_grade(parameters);
        assert!(grade.is_identity());
        let mut frame = filled(16, 16);
        assert!(shape_frame_f32(&mut frame, 16, 16, &grade));
        assert_eq!(frame, filled(16, 16));
    }

    #[test]
    fn no_fill_and_no_stroke_is_the_identity_but_a_line_is_not() {
        let mut parameters = ShapeParameters::default();
        parameters.fill_opacity = 0.0;
        parameters.stroke_width = 0.0;
        assert!(valid_grade(parameters).is_identity());

        parameters.shape_type = ShapeKind::Line;
        assert!(!valid_grade(parameters).is_identity());
    }

    #[test]
    fn a_quarter_turn_swaps_the_axes() {
        let mut parameters = ShapeParameters::default();
        parameters.size = [10.0, 4.0];
        let upright = valid_grade(parameters);
        parameters.rotation = 90.0;
        let turned = valid_grade(parameters);

        // (8, 3) is 4.5 px above the centre: outside a 2 px half height, inside
        // the 5 px half width only once the shape is turned on its side.
        assert_eq!(
            pixel(&render(16, 16, &upright), 16, 8, 3),
            [0.2, 0.4, 0.6, 1.0]
        );
        assert_eq!(
            pixel(&render(16, 16, &turned), 16, 8, 3),
            [1.0, 1.0, 1.0, 1.0]
        );
        // The turned shape is now 4 px wide, so the horizontal probe flips.
        assert_eq!(
            pixel(&render(16, 16, &upright), 16, 11, 8),
            [1.0, 1.0, 1.0, 1.0]
        );
        assert_eq!(
            pixel(&render(16, 16, &turned), 16, 11, 8),
            [0.2, 0.4, 0.6, 1.0]
        );
    }

    #[test]
    fn the_default_position_puts_the_shape_at_the_frame_centre() {
        let mut parameters = ShapeParameters::default();
        parameters.size = [4.0, 4.0];
        let centred = valid_grade(parameters);
        parameters.position = [0.25, 0.25];
        let offset = valid_grade(parameters);

        let frame = render(16, 16, &centred);
        assert_eq!(pixel(&frame, 16, 8, 8), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(pixel(&frame, 16, 7, 7), [1.0, 1.0, 1.0, 1.0]);
        // The same probe pixel is 4.5 px away from the offset centre.
        let frame = render(16, 16, &offset);
        assert_eq!(pixel(&frame, 16, 8, 8), [0.2, 0.4, 0.6, 1.0]);
        assert_eq!(pixel(&frame, 16, 4, 4), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn out_of_range_parameters_are_reported_by_name() {
        let mut parameters = ShapeParameters::default();
        parameters.size = [9000.0, 10.0];
        assert_eq!(
            ShapeGrade::new(parameters).unwrap_err().parameter(),
            "size_x"
        );

        parameters = ShapeParameters::default();
        parameters.corner_radius = f32::NAN;
        assert_eq!(
            ShapeGrade::new(parameters).unwrap_err().parameter(),
            "corner_radius"
        );

        parameters = ShapeParameters::default();
        parameters.vertices = 2.0;
        assert_eq!(
            ShapeGrade::new(parameters).unwrap_err().parameter(),
            "vertices"
        );

        parameters = ShapeParameters::default();
        parameters.rotation = 361.0;
        assert_eq!(
            ShapeGrade::new(parameters).unwrap_err().parameter(),
            "rotation"
        );

        parameters = ShapeParameters::default();
        parameters.fill_colour = [1.0, 1.0, 1.5, 1.0];
        assert_eq!(
            ShapeGrade::new(parameters).unwrap_err().parameter(),
            "fill_colour"
        );

        parameters = ShapeParameters::default();
        parameters.opacity = -1.0;
        assert_eq!(
            ShapeGrade::new(parameters).unwrap_err().parameter(),
            "opacity"
        );
    }

    #[test]
    fn every_menu_key_round_trips() {
        for kind in ShapeKind::ALL {
            assert_eq!(ShapeKind::from_key(kind.as_key()), Some(kind));
        }
        for position in ShapeStrokePosition::ALL {
            assert_eq!(
                ShapeStrokePosition::from_key(position.as_key()),
                Some(position)
            );
        }
        for mode in ShapeBooleanMode::ALL {
            assert_eq!(ShapeBooleanMode::from_key(mode.as_key()), Some(mode));
        }
        assert_eq!(ShapeKind::from_key("unknown"), None);
        assert_eq!(ShapeStrokePosition::from_key("unknown"), None);
        assert_eq!(ShapeBooleanMode::from_key("unknown"), None);
        assert_eq!(ShapeKind::Line.as_u8(), 5);
        assert_eq!(ShapeStrokePosition::Outside.as_u8(), 2);
        assert_eq!(ShapeBooleanMode::Exclude.as_u8(), 3);
    }

    #[test]
    fn hash_state_tracks_every_parameter() {
        use std::collections::hash_map::DefaultHasher;

        let hash = |grade: &ShapeGrade| {
            let mut hasher = DefaultHasher::new();
            grade.hash_state(&mut hasher);
            hasher.finish()
        };

        let base = ShapeGrade::documented_defaults();
        assert_eq!(hash(&base), hash(&ShapeGrade::documented_defaults()));

        let mut changed = *base.parameters();
        changed.shape_type = ShapeKind::Star;
        assert_ne!(hash(&base), hash(&valid_grade(changed)));

        let mut changed = *base.parameters();
        changed.fill_colour[2] = 0.5;
        assert_ne!(hash(&base), hash(&valid_grade(changed)));

        let mut changed = *base.parameters();
        changed.boolean_mode = ShapeBooleanMode::Intersect;
        assert_ne!(hash(&base), hash(&valid_grade(changed)));
    }

    #[test]
    fn the_six_control_cells_carry_the_documented_fields() {
        let mut parameters = ShapeParameters::default();
        parameters.shape_type = ShapeKind::Star;
        parameters.size = [300.0, 200.0];
        parameters.position = [0.25, 0.75];
        parameters.corner_radius = 40.0;
        parameters.vertices = 7.0;
        parameters.inner_ratio = 0.4;
        parameters.stroke_width = 6.0;
        parameters.stroke_position = ShapeStrokePosition::Outside;
        parameters.boolean_mode = ShapeBooleanMode::Subtract;
        parameters.rotation = 90.0;
        parameters.opacity = 50.0;
        parameters.fill_opacity = 25.0;
        let grade = valid_grade(parameters);

        let (geometry, outline, modes, fill, stroke, centre) = grade.gpu_controls();
        assert_eq!(geometry, [150.0, 100.0, 40.0, 7.0]);
        assert_eq!(outline[0], 0.4);
        assert_eq!(outline[1], 6.0);
        assert_close(outline[2], std::f32::consts::FRAC_PI_2, 1e-6);
        assert_eq!(outline[3], 0.5);
        assert_eq!(modes[0], 0.25);
        assert_eq!(modes[1], 4.0);
        assert_eq!(modes[2], 2.0);
        assert_eq!(modes[3], 1.0);
        assert_eq!(fill, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(stroke, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(centre, [0.25, 0.75, 0.0, 0.0]);
    }

    #[test]
    fn a_short_or_empty_frame_is_rejected() {
        let grade = ShapeGrade::documented_defaults();
        let mut frame = filled(4, 4);
        assert!(!shape_frame_f32(&mut frame, 8, 8, &grade));
        assert!(!shape_frame_f32(&mut frame, 0, 4, &grade));
        assert!(!shape_frame_f32(&mut frame, 4, 0, &grade));
    }

    #[test]
    fn the_checkpoint_brackets_the_row_bands() {
        let mut parameters = ShapeParameters::default();
        parameters.size = [9.0, 9.0];
        let grade = valid_grade(parameters);
        let mut frame = filled(16, 16);
        let mut calls = 0;
        let result = apply_shape_f32_controlled(&mut frame, 16, 16, &grade, &mut || {
            calls += 1;
            Ok::<_, std::convert::Infallible>(())
        });
        assert!(result.unwrap());
        assert_eq!(calls, 2);
    }

    #[test]
    fn checkpoint_errors_propagate() {
        let mut parameters = ShapeParameters::default();
        parameters.size = [9.0, 9.0];
        let grade = valid_grade(parameters);
        let mut frame = filled(16, 16);
        let result = apply_shape_f32_controlled(&mut frame, 16, 16, &grade, &mut || {
            Err::<(), &'static str>("cancelled")
        });
        assert_eq!(result.unwrap_err(), "cancelled");
    }
}
