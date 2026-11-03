//! Circle / ellipse generator (GEN-09, `builtin.circle_ellipse`).
//!
//! A pure generator: it paints a soft-edged circle in pixel-local coordinates
//! and owns no geometry of its own. The ellipse arm is deferred by the design
//! (D-1); the type keeps the `CircleEllipse` name so adding it later would not
//! need a rename.
//!
//! The radius is the 50 % coverage isocontour: the inside ramp fades from the
//! radius over `feather_inner`, the outside ramp over `feather_outer`, and both
//! fall back to half the authored `edge` so a zero-feather circle still has a
//! one-pixel-wide transition and the divisions can never be zero.

use mondrian_core::BlendMode;

/// Largest accepted centre coordinate, in pixels.
pub const CIRCLE_ELLIPSE_MAX_COORDINATE: f32 = 1_000_000.0;
/// Largest accepted radius magnitude, in pixels.
pub const CIRCLE_ELLIPSE_MAX_RADIUS: f32 = 10_000.0;
/// Smallest accepted edge width, in pixels.
pub const CIRCLE_ELLIPSE_MIN_EDGE: f32 = 1.0;
/// Largest accepted edge width, in pixels.
pub const CIRCLE_ELLIPSE_MAX_EDGE: f32 = 5.0;
/// Largest accepted feather width, in pixels.
pub const CIRCLE_ELLIPSE_MAX_FEATHER: f32 = 10_000.0;
/// Largest accepted value of the lazy context slider.
pub const CIRCLE_ELLIPSE_MAX_CONTEXT_SLIDER: f32 = 10_000.0;
/// The dump default for the centre.
pub const CIRCLE_ELLIPSE_DEFAULT_CENTER: [f32; 2] = [128.0, 128.0];
/// The dump default for the radius.
pub const CIRCLE_ELLIPSE_DEFAULT_RADIUS: f32 = 75.0;
/// The dump default for the edge width.
pub const CIRCLE_ELLIPSE_DEFAULT_EDGE: f32 = 1.0;
/// The dump default for the lazy context slider.
pub const CIRCLE_ELLIPSE_DEFAULT_CONTEXT_SLIDER: f32 = 10.0;
/// The dump default for the opacity, in percent.
pub const CIRCLE_ELLIPSE_DEFAULT_OPACITY: f32 = 100.0;

/// Rows written between checkpoint callbacks.
const CHECKPOINT_STRIDE: usize = 4_096;

/// Why a [`CircleEllipseGrade`] could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircleEllipseError {
    /// The centre had a non-finite component.
    NonFiniteCenter,
    /// The centre left the documented coordinate range.
    CenterOutOfRange,
    /// The radius was not finite.
    NonFiniteRadius,
    /// The radius left the documented range.
    RadiusOutOfRange,
    /// The edge width was not finite.
    NonFiniteEdge,
    /// The edge width left the documented range.
    EdgeOutOfRange,
    /// The outer feather was not finite.
    NonFiniteFeatherOuter,
    /// The outer feather left the documented range.
    FeatherOuterOutOfRange,
    /// The inner feather was not finite.
    NonFiniteFeatherInner,
    /// The inner feather left the documented range.
    FeatherInnerOutOfRange,
    /// A colour channel was not finite.
    NonFiniteColor,
    /// A colour channel left the documented range.
    ColorOutOfRange,
    /// The opacity was not finite.
    NonFiniteOpacity,
    /// The opacity left the documented range.
    OpacityOutOfRange,
    /// The blend mode is not offered by the authoring schema.
    DissolveUnsupported,
}

impl std::fmt::Display for CircleEllipseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFiniteCenter => formatter.write_str("circle centre must be finite"),
            Self::CenterOutOfRange => formatter.write_str("circle centre is out of range"),
            Self::NonFiniteRadius => formatter.write_str("circle radius must be finite"),
            Self::RadiusOutOfRange => formatter.write_str("circle radius is out of range"),
            Self::NonFiniteEdge => formatter.write_str("circle edge must be finite"),
            Self::EdgeOutOfRange => formatter.write_str("circle edge is out of range"),
            Self::NonFiniteFeatherOuter => {
                formatter.write_str("circle outer feather must be finite")
            }
            Self::FeatherOuterOutOfRange => {
                formatter.write_str("circle outer feather is out of range")
            }
            Self::NonFiniteFeatherInner => {
                formatter.write_str("circle inner feather must be finite")
            }
            Self::FeatherInnerOutOfRange => {
                formatter.write_str("circle inner feather is out of range")
            }
            Self::NonFiniteColor => formatter.write_str("circle colour must be finite"),
            Self::ColorOutOfRange => formatter.write_str("circle colour is out of range"),
            Self::NonFiniteOpacity => formatter.write_str("circle opacity must be finite"),
            Self::OpacityOutOfRange => formatter.write_str("circle opacity is out of range"),
            Self::DissolveUnsupported => {
                formatter.write_str("circle does not support dissolve blending")
            }
        }
    }
}

impl std::error::Error for CircleEllipseError {}

/// The four uniform cells, in shader order.
pub type CircleEllipseControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// A validated circle generator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CircleEllipseGrade {
    center: [f32; 2],
    radius: f32,
    edge: f32,
    feather_outer: f32,
    feather_inner: f32,
    invert: bool,
    color: [f32; 3],
    opacity: f32,
    blend_mode: BlendMode,
}

impl CircleEllipseGrade {
    /// Validates and builds a grade.
    ///
    /// The lazy `context_slider` parameter is deliberately absent: it is an
    /// authoring hint that the graph builder reads and drops, so it never
    /// reaches this constructor, the uniform, or the effect cache key.
    ///
    /// # Errors
    ///
    /// Returns [`CircleEllipseError`] when a field is non-finite, leaves its
    /// documented range, or asks for dissolve blending.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        center: [f32; 2],
        radius: f32,
        edge: f32,
        feather_outer: f32,
        feather_inner: f32,
        invert: bool,
        color: [f32; 3],
        opacity: f32,
        blend_mode: BlendMode,
    ) -> Result<Self, CircleEllipseError> {
        if !center[0].is_finite() || !center[1].is_finite() {
            return Err(CircleEllipseError::NonFiniteCenter);
        }
        if center[0].abs() > CIRCLE_ELLIPSE_MAX_COORDINATE
            || center[1].abs() > CIRCLE_ELLIPSE_MAX_COORDINATE
        {
            return Err(CircleEllipseError::CenterOutOfRange);
        }
        if !radius.is_finite() {
            return Err(CircleEllipseError::NonFiniteRadius);
        }
        if !(-CIRCLE_ELLIPSE_MAX_RADIUS..=CIRCLE_ELLIPSE_MAX_RADIUS).contains(&radius) {
            return Err(CircleEllipseError::RadiusOutOfRange);
        }
        if !edge.is_finite() {
            return Err(CircleEllipseError::NonFiniteEdge);
        }
        if !(CIRCLE_ELLIPSE_MIN_EDGE..=CIRCLE_ELLIPSE_MAX_EDGE).contains(&edge) {
            return Err(CircleEllipseError::EdgeOutOfRange);
        }
        if !feather_outer.is_finite() {
            return Err(CircleEllipseError::NonFiniteFeatherOuter);
        }
        if !(0.0..=CIRCLE_ELLIPSE_MAX_FEATHER).contains(&feather_outer) {
            return Err(CircleEllipseError::FeatherOuterOutOfRange);
        }
        if !feather_inner.is_finite() {
            return Err(CircleEllipseError::NonFiniteFeatherInner);
        }
        if !(0.0..=CIRCLE_ELLIPSE_MAX_FEATHER).contains(&feather_inner) {
            return Err(CircleEllipseError::FeatherInnerOutOfRange);
        }
        if !color[0].is_finite() || !color[1].is_finite() || !color[2].is_finite() {
            return Err(CircleEllipseError::NonFiniteColor);
        }
        if !(0.0..=1.0).contains(&color[0])
            || !(0.0..=1.0).contains(&color[1])
            || !(0.0..=1.0).contains(&color[2])
        {
            return Err(CircleEllipseError::ColorOutOfRange);
        }
        if !opacity.is_finite() {
            return Err(CircleEllipseError::NonFiniteOpacity);
        }
        if !(0.0..=CIRCLE_ELLIPSE_DEFAULT_OPACITY).contains(&opacity) {
            return Err(CircleEllipseError::OpacityOutOfRange);
        }
        if matches!(blend_mode, BlendMode::Dissolve) {
            return Err(CircleEllipseError::DissolveUnsupported);
        }
        Ok(Self {
            center,
            radius,
            edge,
            feather_outer,
            feather_inner,
            invert,
            color,
            opacity: opacity / CIRCLE_ELLIPSE_DEFAULT_OPACITY,
            blend_mode,
        })
    }

    /// The centre, in pixels.
    #[must_use]
    pub const fn center(self) -> [f32; 2] {
        self.center
    }

    /// The authored radius, in pixels. The sign is irrelevant; the shader and
    /// the CPU both take the magnitude.
    #[must_use]
    pub const fn radius(self) -> f32 {
        self.radius
    }

    /// The authored edge width, in pixels.
    #[must_use]
    pub const fn edge(self) -> f32 {
        self.edge
    }

    /// The outer feather width, in pixels.
    #[must_use]
    pub const fn feather_outer(self) -> f32 {
        self.feather_outer
    }

    /// The inner feather width, in pixels.
    #[must_use]
    pub const fn feather_inner(self) -> f32 {
        self.feather_inner
    }

    /// Whether the inside and outside are swapped.
    #[must_use]
    pub const fn invert(self) -> bool {
        self.invert
    }

    /// The fill colour, straight alpha.
    #[must_use]
    pub const fn color(self) -> [f32; 3] {
        self.color
    }

    /// The opacity as a fraction of one.
    #[must_use]
    pub const fn opacity(self) -> f32 {
        self.opacity
    }

    /// The blend mode used against the incoming pixel.
    #[must_use]
    pub const fn blend_mode(self) -> BlendMode {
        self.blend_mode
    }

    /// The blend-mode index packed into `header.y`.
    #[must_use]
    pub fn blend_mode_index(self) -> u32 {
        crate::glow::blend_mode_index(self.blend_mode)
    }

    /// Whether the grade paints nothing at all.
    ///
    /// Only the opacity can make the generator an identity: a zero radius still
    /// paints a half-coverage dot, because an animation pulling the radius to
    /// zero must not make the effect vanish.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.opacity <= 1.0e-6
    }

    /// The four uniform cells, in shader order.
    #[must_use]
    pub fn gpu_controls(self) -> CircleEllipseControls {
        (
            [self.center[0], self.center[1], self.radius, self.edge],
            [self.color[0], self.color[1], self.color[2], self.opacity],
            [self.feather_outer, self.feather_inner, 0.0, 0.0],
            [0.0; 4],
        )
    }

    /// The coverage at a distance from the centre.
    fn coverage_at_distance(self, distance: f32) -> f32 {
        let radius = self.radius.abs();
        let half_edge = self.edge.max(0.0) * 0.5;
        // The edge is at least one pixel, so both ramps are at least half a
        // pixel wide: the divisions below can never be zero, and the geometric
        // radius sits at exactly 0.5 coverage on both branches.
        let inside_width = self.feather_inner.max(half_edge);
        let outside_width = self.feather_outer.max(half_edge);
        let signed = distance - radius;
        let mut coverage = if signed <= 0.0 {
            0.5 + 0.5 * (-signed / inside_width).clamp(0.0, 1.0)
        } else {
            0.5 * (1.0 - (signed / outside_width).clamp(0.0, 1.0))
        };
        if self.invert {
            coverage = 1.0 - coverage;
        }
        coverage
    }

    /// The painted coverage at a pixel index, in `0.0..=1.0`.
    #[must_use]
    pub fn coverage(self, column: u32, row: u32) -> f32 {
        let centre_x = column as f32 + 0.5;
        let centre_y = row as f32 + 0.5;
        let offset_x = centre_x - self.center[0];
        let offset_y = centre_y - self.center[1];
        // `hypot` is deliberately avoided: its last-ulp rounding differs from
        // `sqrt(dx * dx + dy * dy)` in the shader.
        let distance = (offset_x * offset_x + offset_y * offset_y).sqrt();
        self.coverage_at_distance(distance)
    }

    /// Blends the generated colour over `pixel` at a pixel index.
    #[must_use]
    pub fn apply(self, pixel: [f32; 4], column: u32, row: u32) -> [f32; 4] {
        if self.is_identity() {
            return pixel;
        }
        let coverage = self.coverage(column, row);
        let alpha = coverage * self.opacity;
        if alpha <= 0.0 {
            return pixel;
        }
        crate::adjustment::blend_rgba_f32_pixel_seeded(
            pixel,
            [self.color[0], self.color[1], self.color[2], alpha],
            1.0,
            self.blend_mode,
            0,
        )
    }
}

/// The generated colour and coverage at a pixel index.
///
/// Empty pixels are transparent black: the generator contributes no colour
/// where the coverage is zero, so the value is directly usable as a source.
#[must_use]
pub fn circle_ellipse_pixel(column: u32, row: u32, grade: &CircleEllipseGrade) -> [f32; 4] {
    if grade.is_identity() {
        return [0.0; 4];
    }
    let coverage = grade.coverage(column, row);
    if coverage <= 0.0 {
        return [0.0; 4];
    }
    [
        grade.color()[0],
        grade.color()[1],
        grade.color()[2],
        coverage,
    ]
}

/// Applies the generator to a whole frame.
///
/// Returns `false` when `width * height` overflows or the buffer is too short.
#[must_use]
pub fn circle_ellipse_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &CircleEllipseGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_circle_ellipse_f32_region_controlled(pixels, 0, 0, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies the generator to a region whose origin is given in frame-absolute
/// pixel coordinates.
///
/// # Errors
///
/// Propagates the first error returned by `checkpoint`, leaving already written
/// chunks in place.
pub(crate) fn apply_circle_ellipse_f32_region_controlled<E>(
    pixels: &mut [[f32; 4]],
    region_x: usize,
    region_y: usize,
    width: usize,
    height: usize,
    grade: &CircleEllipseGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || pixels.len() < pixel_count {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let rows_per_chunk = (CHECKPOINT_STRIDE / width).max(1);
    let mut start = 0usize;
    while start < height {
        checkpoint()?;
        let rows = rows_per_chunk.min(height - start);
        let end = start + rows;
        crate::parallel::for_each_row_band(
            &mut pixels[start * width..end * width],
            width,
            0..rows,
            |first_row, band| {
                for (offset, chunk) in band.chunks_mut(width).enumerate() {
                    let row = (region_y + start + first_row + offset) as u32;
                    for (column, pixel) in chunk.iter_mut().enumerate() {
                        let column = (region_x + column) as u32;
                        *pixel = grade.apply(*pixel, column, row);
                    }
                }
            },
        );
        start = end;
    }
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    const WHITE: [f32; 3] = [1.0, 1.0, 1.0];

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.0; 4]; width * height]
    }

    #[allow(clippy::too_many_arguments)]
    fn new_grade(
        center: [f32; 2],
        radius: f32,
        edge: f32,
        feather_outer: f32,
        feather_inner: f32,
        invert: bool,
        color: [f32; 3],
        opacity: f32,
        blend_mode: BlendMode,
    ) -> CircleEllipseGrade {
        CircleEllipseGrade::new(
            center,
            radius,
            edge,
            feather_outer,
            feather_inner,
            invert,
            color,
            opacity,
            blend_mode,
        )
        .expect("test grade should validate")
    }

    fn default_grade() -> CircleEllipseGrade {
        new_grade(
            CIRCLE_ELLIPSE_DEFAULT_CENTER,
            CIRCLE_ELLIPSE_DEFAULT_RADIUS,
            CIRCLE_ELLIPSE_DEFAULT_EDGE,
            0.0,
            0.0,
            false,
            WHITE,
            CIRCLE_ELLIPSE_DEFAULT_OPACITY,
            BlendMode::Normal,
        )
    }

    #[test]
    fn defaults_match_the_dump() {
        let grade = default_grade();
        assert_eq!(grade.center(), CIRCLE_ELLIPSE_DEFAULT_CENTER);
        assert_eq!(grade.radius(), 75.0);
        assert_eq!(grade.edge(), 1.0);
        assert_eq!(grade.feather_outer(), 0.0);
        assert_eq!(grade.feather_inner(), 0.0);
        assert!(!grade.invert());
        assert_eq!(grade.color(), WHITE);
        assert_eq!(grade.opacity(), 1.0);
        assert_eq!(grade.blend_mode(), BlendMode::Normal);
        assert_eq!(grade.blend_mode_index(), 0);
        assert!(!grade.is_identity());
        assert_eq!(
            grade.gpu_controls(),
            (
                [128.0, 128.0, 75.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
                [0.0; 4],
                [0.0; 4],
            )
        );
        assert_eq!(CIRCLE_ELLIPSE_DEFAULT_CONTEXT_SLIDER, 10.0);
        assert_eq!(CIRCLE_ELLIPSE_MAX_CONTEXT_SLIDER, 10_000.0);
        assert_eq!(CIRCLE_ELLIPSE_MAX_RADIUS, 10_000.0);
        assert_eq!(CIRCLE_ELLIPSE_MIN_EDGE, 1.0);
        assert_eq!(CIRCLE_ELLIPSE_MAX_EDGE, 5.0);
        assert_eq!(CIRCLE_ELLIPSE_MAX_FEATHER, 10_000.0);
        // The documented default frame: the radius sits between columns 202
        // and 203 at row 128.
        assert_eq!(grade.coverage(128, 128), 1.0);
        // At (202, 128) the pixel centre is (202.5, 128.5), so
        // d = sqrt(74.5^2 + 0.5^2) = 74.5017 and the inside ramp gives
        // 0.5 + 0.5 * (0.4983 / 0.5) = 0.9983, the number design §3.7 prints.
        assert!((grade.coverage(202, 128) - 0.998_322).abs() <= 1.0e-5);
        assert_eq!(grade.coverage(203, 128), 0.0);
    }

    #[test]
    fn radius_boundary_is_half_coverage() {
        let grade = new_grade(
            [0.5, 0.5],
            5.0,
            1.0,
            0.0,
            0.0,
            false,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        // The pixel centred on (5.5, 0.5) is exactly five pixels from (0.5, 0.5).
        assert_eq!(grade.coverage(5, 0), 0.5);
        // Axis directions: four pixels sit exactly on the radius.
        let axis = new_grade(
            [8.5, 8.5],
            5.0,
            1.0,
            0.0,
            0.0,
            false,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        for (column, row) in [(13, 8), (3, 8), (8, 13), (8, 3)] {
            let coverage = axis.coverage(column, row);
            assert!(
                (coverage - 0.5).abs() <= 1.0e-6,
                "({column}, {row}) gave {coverage}"
            );
        }
        // Diagonal directions: choose the radius whose squared distance is the
        // integer 50 so the boundary lands exactly on the pixels as well.
        let radius = 50.0f32.sqrt();
        let diagonal = new_grade(
            [8.5, 8.5],
            radius,
            1.0,
            0.0,
            0.0,
            false,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        for (column, row) in [(13, 13), (3, 13), (13, 3), (3, 3)] {
            let coverage = diagonal.coverage(column, row);
            assert!(
                (coverage - 0.5).abs() <= 1.0e-6,
                "({column}, {row}) gave {coverage}"
            );
        }
    }

    #[test]
    fn hard_edge_ramp_equals_edge_width() {
        let grade = new_grade(
            [0.5, 0.5],
            5.0,
            4.0,
            0.0,
            0.0,
            false,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        // edge = 4 means a two-pixel ramp on either side of the radius.
        assert_eq!(grade.coverage(3, 0), 1.0);
        assert_eq!(grade.coverage(5, 0), 0.5);
        assert_eq!(grade.coverage(7, 0), 0.0);
        assert!((grade.coverage(4, 0) - 0.75).abs() <= 1.0e-6);
        assert!((grade.coverage(6, 0) - 0.25).abs() <= 1.0e-6);
    }

    #[test]
    fn inner_feather_only_softens_inside() {
        let base = new_grade(
            [0.5, 0.5],
            50.0,
            1.0,
            0.0,
            0.0,
            false,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        let inner = new_grade(
            [0.5, 0.5],
            50.0,
            1.0,
            0.0,
            10.0,
            false,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        // The inside ramp now spans ten pixels, so it reaches full coverage
        // only ten pixels in; the radius itself is still the 50 % isocontour.
        assert_eq!(inner.coverage_at_distance(40.0), 1.0);
        assert_eq!(inner.coverage_at_distance(45.0), 0.75);
        assert_eq!(inner.coverage_at_distance(50.0), 0.5);
        assert!((inner.coverage_at_distance(49.0) - 0.55).abs() <= 1.0e-6);
        // Outside the radius the edge still owns the ramp, unchanged.
        assert_eq!(
            inner.coverage_at_distance(50.25),
            base.coverage_at_distance(50.25)
        );
        assert!((inner.coverage_at_distance(50.25) - 0.25).abs() <= 1.0e-6);
    }

    #[test]
    fn outer_feather_only_softens_outside() {
        let base = new_grade(
            [0.5, 0.5],
            5.0,
            1.0,
            0.0,
            0.0,
            false,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        let outer = new_grade(
            [0.5, 0.5],
            5.0,
            1.0,
            10.0,
            0.0,
            false,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        // Five pixels past the radius the outside ramp is half spent.
        assert_eq!(outer.coverage_at_distance(10.0), 0.25);
        assert_eq!(outer.coverage_at_distance(15.0), 0.0);
        assert!((outer.coverage_at_distance(6.0) - 0.45).abs() <= 1.0e-6);
        // Inside the radius the edge still owns the ramp, unchanged.
        assert_eq!(
            outer.coverage_at_distance(4.75),
            base.coverage_at_distance(4.75)
        );
        assert_eq!(outer.coverage_at_distance(4.75), 0.75);
    }

    #[test]
    fn invert_keeps_the_boundary_at_half_coverage() {
        let grade = new_grade(
            [0.5, 0.5],
            5.0,
            2.0,
            0.0,
            0.0,
            true,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        assert_eq!(grade.coverage(5, 0), 0.5);
        assert_eq!(grade.coverage(4, 0), 0.0);
        assert_eq!(grade.coverage(6, 0), 1.0);
    }

    #[test]
    fn invert_of_the_centre_is_empty() {
        let grade = new_grade(
            [128.5, 128.5],
            75.0,
            1.0,
            0.0,
            0.0,
            true,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        assert_eq!(grade.coverage(128, 128), 0.0);
        let pixel = [0.25, 0.5, 0.75, 1.0];
        assert_eq!(grade.apply(pixel, 128, 128), pixel);
    }

    #[test]
    fn negative_radius_matches_positive_bitwise() {
        let positive = new_grade(
            [16.0, 16.0],
            9.5,
            5.0,
            3.0,
            2.0,
            false,
            [0.2, 0.4, 0.6],
            73.0,
            BlendMode::Screen,
        );
        let negative = new_grade(
            [16.0, 16.0],
            -9.5,
            5.0,
            3.0,
            2.0,
            false,
            [0.2, 0.4, 0.6],
            73.0,
            BlendMode::Screen,
        );
        let mut positive_pixels = frame(32, 32);
        let mut negative_pixels = frame(32, 32);
        assert!(circle_ellipse_frame_f32(
            &mut positive_pixels,
            32,
            32,
            &positive
        ));
        assert!(circle_ellipse_frame_f32(
            &mut negative_pixels,
            32,
            32,
            &negative
        ));
        for (left, right) in positive_pixels.iter().zip(&negative_pixels) {
            for channel in 0..4 {
                assert_eq!(left[channel].to_bits(), right[channel].to_bits());
            }
        }
    }

    #[test]
    fn radius_zero_is_a_soft_dot() {
        // The centre pixel sits exactly on the centre: a zero signed distance
        // is half coverage, so the dot stays visible instead of vanishing.
        let grade = new_grade(
            [1.5, 1.5],
            0.0,
            1.0,
            0.0,
            0.0,
            false,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        assert!(!grade.is_identity());
        assert_eq!(grade.coverage(1, 1), 0.5);
        assert_eq!(grade.coverage(1, 0), 0.0);
        assert_eq!(grade.coverage_at_distance(0.5), 0.0);
        let mut pixels = frame(3, 3);
        assert!(circle_ellipse_frame_f32(&mut pixels, 3, 3, &grade));
        assert_eq!(pixels[4], [1.0, 1.0, 1.0, 0.5]);
        for (index, pixel) in pixels.iter().enumerate() {
            if index != 4 {
                assert_eq!(*pixel, [0.0; 4]);
            }
        }
    }

    #[test]
    fn opacity_zero_is_identity() {
        let grade = new_grade(
            [16.0, 16.0],
            9.0,
            3.0,
            2.0,
            1.0,
            false,
            WHITE,
            0.0,
            BlendMode::Multiply,
        );
        assert!(grade.is_identity());
        assert_eq!(grade.opacity(), 0.0);
        let pixel = [0.1, 0.2, 0.3, 0.4];
        assert_eq!(grade.apply(pixel, 16, 16), pixel);
        let mut pixels = frame(8, 8);
        assert!(circle_ellipse_frame_f32(&mut pixels, 8, 8, &grade));
        assert!(pixels.iter().all(|pixel| *pixel == [0.0; 4]));
        assert_eq!(circle_ellipse_pixel(16, 16, &grade), [0.0; 4]);
        let epsilon = new_grade(
            [16.0, 16.0],
            9.0,
            3.0,
            2.0,
            1.0,
            false,
            WHITE,
            0.000_001,
            BlendMode::Normal,
        );
        assert!(epsilon.is_identity());
    }

    #[test]
    fn region_offset_uses_frame_absolute_coordinates() {
        let grade = new_grade(
            [24.0, 20.0],
            12.0,
            3.0,
            2.0,
            1.0,
            false,
            [0.8, 0.6, 0.4],
            90.0,
            BlendMode::Normal,
        );
        let mut full = frame(32, 32);
        assert!(circle_ellipse_frame_f32(&mut full, 32, 32, &grade));
        let mut patch = frame(16, 16);
        let mut checkpoint = || Ok::<(), Infallible>(());
        assert!(apply_circle_ellipse_f32_region_controlled(
            &mut patch,
            8,
            8,
            16,
            16,
            &grade,
            &mut checkpoint
        )
        .expect("the checkpoint never fails"));
        for row in 0..16 {
            for column in 0..16 {
                assert_eq!(patch[row * 16 + column], full[(row + 8) * 32 + column + 8]);
            }
        }
        assert!(patch.iter().any(|pixel| pixel[3] > 0.0));
    }

    #[test]
    fn checkpoint_stride_writes_in_chunks() {
        let grade = new_grade(
            [0.5, 0.5],
            5_000.0,
            5.0,
            0.0,
            0.0,
            false,
            WHITE,
            100.0,
            BlendMode::Normal,
        );
        // 4096x2 uses one row per chunk: two chunk checkpoints plus the final
        // one.
        let mut pixels = frame(4_096, 2);
        let mut checkpoints = 0usize;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), Infallible>(())
        };
        assert!(apply_circle_ellipse_f32_region_controlled(
            &mut pixels,
            0,
            0,
            4_096,
            2,
            &grade,
            &mut checkpoint
        )
        .expect("the checkpoint never fails"));
        assert_eq!(checkpoints, 3);
        assert!(pixels.iter().all(|pixel| pixel[3] == 1.0));

        let mut pixels = frame(4_096, 2);
        let mut checkpoints = 0usize;
        let mut abort = || {
            checkpoints += 1;
            if checkpoints == 2 {
                Err("stop")
            } else {
                Ok(())
            }
        };
        let result = apply_circle_ellipse_f32_region_controlled(
            &mut pixels,
            0,
            0,
            4_096,
            2,
            &grade,
            &mut abort,
        );
        assert_eq!(result, Err("stop"));
        assert_eq!(checkpoints, 2);
        // The first chunk was written before the abort; the second row was not.
        assert!(pixels[..4_096].iter().all(|pixel| pixel[3] == 1.0));
        assert!(pixels[4_096..].iter().all(|pixel| *pixel == [0.0; 4]));
    }
}
