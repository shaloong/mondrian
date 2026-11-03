//! Stroke: paints a closed band along the layer's alpha contour.
//!
//! The band is centred on the source frame's alpha 0.5 contour and is
//! `brush_size` pixels wide, so it straddles the contour the way a pen stroke
//! straddles the line it is drawing. Every other parameter shapes the band's
//! profile or the way it is composited:
//!
//! * `brush_hardness` moves the ink towards the outside edge of the band.
//!   A hardness of `0` gives a linear ramp from the contour to the band edge;
//!   `0.95` leaves almost all of the band solid and only the outermost pixels
//!   fade.
//! * `opacity` scales the band's alpha.
//! * `color` is the ink colour.
//! * `paint_style` picks between painting the ink over the source, replacing
//!   the band with the ink, and revealing the source inside the band.
//!
//! The distance from a pixel to the contour is numerically estimated. Eight
//! axes are scanned, each by doubling the step from `1.0` until the sampled
//! alpha crosses `0.5`, then by bisecting the bracket six times. The estimate
//! is therefore exact along the axes only up to the tap quantisation, and up
//! to roughly 8.2% on the diagonals; callers must not treat it as an analytic
//! distance field.
//!
//! When the estimated distance exceeds the band radius the pixel is left
//! untouched. That is what makes a fully transparent source frame a
//! bit-identical no-op for every paint style: it has no alpha contour at all.
//!
//! The CPU kernel works on straight alpha, like every other kernel behind the
//! spatial driver seam.

use crate::parallel::for_each_row_band;

/// Largest brush size the effect accepts, in pixels.
pub const STROKE_MAX_BRUSH_SIZE: f32 = 200.0;
/// Largest brush hardness the effect accepts. `1.0` would let the profile
/// divide by zero, so the parameter stops just short of a hard edge.
pub const STROKE_MAX_BRUSH_HARDNESS: f32 = 0.95;
/// Default brush size, in pixels.
pub const STROKE_DEFAULT_BRUSH_SIZE: f32 = 2.0;
/// Default brush hardness.
pub const STROKE_DEFAULT_BRUSH_HARDNESS: f32 = 0.75;
/// Default band opacity.
pub const STROKE_DEFAULT_OPACITY: f32 = 1.0;
/// Default ink colour, white.
pub const STROKE_DEFAULT_COLOR: [f32; 3] = [1.0, 1.0, 1.0];

/// Number of contour directions the distance scan probes.
const STROKE_DIRECTIONS: usize = 8;
/// Bisection steps taken after the first crossing tap.
const STROKE_BISECTIONS: usize = 6;
/// Pixels per checkpoint chunk. Matches the other controlled kernels.
const CHECKPOINT_STRIDE: usize = 4_096;
/// `1 / sqrt(2)`, shared by the four diagonal probes.
const SQRT_HALF: f32 = core::f32::consts::FRAC_1_SQRT_2;

/// How the ink is composited with the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StrokePaintStyle {
    /// Paint the ink over the source, keeping the source visible underneath.
    OnOriginal,
    /// Paint the ink on a transparent backdrop: the band replaces the source.
    OnTransparent,
    /// Keep the source colour and multiply its alpha by the band coverage.
    RevealOriginal,
}

impl StrokePaintStyle {
    /// Every style, in schema order.
    pub const ALL: [Self; 3] = [Self::OnOriginal, Self::OnTransparent, Self::RevealOriginal];

    /// Stable parameter key.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::OnOriginal => "on_original",
            Self::OnTransparent => "on_transparent",
            Self::RevealOriginal => "reveal_original",
        }
    }

    /// Stable raw code shared with the GPU uniform.
    #[must_use]
    pub const fn gpu_code(self) -> u32 {
        match self {
            Self::OnOriginal => 1,
            Self::OnTransparent => 2,
            Self::RevealOriginal => 3,
        }
    }

    /// Suffix of the localisation key for this option.
    #[must_use]
    pub const fn label_suffix(self) -> &'static str {
        match self {
            Self::OnOriginal => "on-original",
            Self::OnTransparent => "on-transparent",
            Self::RevealOriginal => "reveal-original",
        }
    }

    /// Authoring label, in the schema's Chinese copy.
    #[must_use]
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::OnOriginal => "在原始图像上",
            Self::OnTransparent => "在透明上",
            Self::RevealOriginal => "显示原始图像",
        }
    }

    /// Parse a stable parameter key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|style| style.key() == key)
    }
}

/// Everything the stroke kernel needs, validated and resolution independent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeGrade {
    paint_style: StrokePaintStyle,
    color: [f32; 3],
    brush_size: f32,
    brush_hardness: f32,
    opacity: f32,
}

/// Controls in GPU uniform order: `control`, `color`, `geometry`.
pub type StrokeControls = ([u32; 4], [f32; 4], [f32; 4]);

impl StrokeGrade {
    /// Validate an authored parameter set. The six path-layout parameters
    /// (`path`, `all_masks`, `sequential_stroke`, `start`, `end`, `spacing`)
    /// are deliberately absent: they are schema-only and never reach the
    /// grade, the uniform or the render hash.
    pub fn new(
        paint_style: StrokePaintStyle,
        color: [f32; 3],
        brush_size: f32,
        brush_hardness: f32,
        opacity: f32,
    ) -> Result<Self, StrokeError> {
        if !brush_size.is_finite() || !(0.0..=STROKE_MAX_BRUSH_SIZE).contains(&brush_size) {
            return Err(StrokeError::InvalidBrushSize);
        }
        if !brush_hardness.is_finite()
            || !(0.0..=STROKE_MAX_BRUSH_HARDNESS).contains(&brush_hardness)
        {
            return Err(StrokeError::InvalidBrushHardness);
        }
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(StrokeError::InvalidOpacity);
        }
        if color
            .iter()
            .any(|channel| !channel.is_finite() || !(0.0..=1.0).contains(channel))
        {
            return Err(StrokeError::InvalidColor);
        }
        Ok(Self {
            paint_style,
            color,
            brush_size,
            brush_hardness,
            opacity,
        })
    }

    /// Ink colour.
    #[must_use]
    pub const fn color(self) -> [f32; 3] {
        self.color
    }

    /// Band width in pixels.
    #[must_use]
    pub const fn brush_size(self) -> f32 {
        self.brush_size
    }

    /// Profile hardness in `0..=0.95`.
    #[must_use]
    pub const fn brush_hardness(self) -> f32 {
        self.brush_hardness
    }

    /// Band opacity in `0..=1`.
    #[must_use]
    pub const fn opacity(self) -> f32 {
        self.opacity
    }

    /// Compositing style.
    #[must_use]
    pub const fn paint_style(self) -> StrokePaintStyle {
        self.paint_style
    }

    /// A zero-sized band or a zero-opacity band writes nothing.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.brush_size <= 1e-6 || self.opacity <= 1e-6
    }

    /// Uniform payload, in the same order as [`StrokeUniform`] on the GPU.
    #[must_use]
    pub fn gpu_controls(self) -> StrokeControls {
        (
            [self.paint_style.gpu_code(), 0, 0, 0],
            [self.color[0], self.color[1], self.color[2], self.opacity],
            [self.brush_size, self.brush_hardness, 0.0, 0.0],
        )
    }

    /// Band radius in pixels: half the stroke width.
    #[must_use]
    pub fn radius(self) -> f32 {
        self.brush_size * 0.5
    }

    /// Softness of the profile. Hardness `0` makes the profile as wide as the
    /// radius; hardness `0.95` leaves only a twentieth of the radius soft.
    #[must_use]
    pub fn softness(self) -> f32 {
        (self.radius() * (1.0 - self.brush_hardness)).max(0.5)
    }

    /// Distance from the contour over which the band is fully opaque.
    #[must_use]
    pub fn solid(self) -> f32 {
        (self.radius() - self.softness()).max(0.0)
    }

    /// Band coverage at a contour distance, before opacity.
    #[must_use]
    pub fn coverage_at_distance(self, distance: f32) -> f32 {
        if distance > self.radius() {
            return 0.0;
        }
        1.0 - ((distance - self.solid()) / self.softness()).clamp(0.0, 1.0)
    }

    /// Composite one pixel with a band coverage in `0..=1`.
    ///
    /// Coverage zero means the pixel is outside the band; callers leave the
    /// pixel untouched rather than writing it back.
    #[must_use]
    pub fn blend(self, pixel: [f32; 4], coverage: f32) -> [f32; 4] {
        let alpha = coverage * self.opacity;
        if alpha <= 0.0 {
            return pixel;
        }
        match self.paint_style {
            StrokePaintStyle::OnOriginal => {
                let source_weight = pixel[3] * (1.0 - alpha);
                let out_alpha = alpha + source_weight;
                [
                    (self.color[0] * alpha + pixel[0] * source_weight) / out_alpha,
                    (self.color[1] * alpha + pixel[1] * source_weight) / out_alpha,
                    (self.color[2] * alpha + pixel[2] * source_weight) / out_alpha,
                    out_alpha,
                ]
            }
            StrokePaintStyle::OnTransparent => [self.color[0], self.color[1], self.color[2], alpha],
            StrokePaintStyle::RevealOriginal => [pixel[0], pixel[1], pixel[2], pixel[3] * alpha],
        }
    }

    /// Apply the band to one pixel of `source` at `(column, row)`.
    #[must_use]
    pub fn apply(
        self,
        source: &[[f32; 4]],
        width: usize,
        height: usize,
        column: usize,
        row: usize,
    ) -> [f32; 4] {
        let pixel = source[row * width + column];
        if self.is_identity() {
            return pixel;
        }
        let limit = self.radius() + 1.0;
        let distance = self.edge_distance(source, width, height, column, row, limit);
        self.blend(pixel, self.coverage_at_distance(distance))
    }

    /// Numerically estimated distance from `(column, row)` to the nearest
    /// alpha 0.5 contour.
    fn edge_distance(
        self,
        source: &[[f32; 4]],
        width: usize,
        height: usize,
        column: usize,
        row: usize,
        limit: f32,
    ) -> f32 {
        let centre = source[row * width + column][3];
        let mut best = limit * 4.0;
        for direction in stroke_directions() {
            let mut step = 1.0;
            let mut low = 0.0;
            let mut high = 0.0;
            let mut found = false;
            loop {
                let (offset_x, offset_y) = stroke_offset(direction, step);
                let sample = load(
                    source,
                    width,
                    height,
                    column as i32 + offset_x,
                    row as i32 + offset_y,
                )[3];
                if (sample - 0.5) * (centre - 0.5) <= 0.0 {
                    high = step;
                    found = true;
                    break;
                }
                low = step;
                // Probe one doubling past the search limit so the bracket
                // always straddles the whole profile, then stop.
                if step > limit + 1.0 {
                    break;
                }
                step *= 2.0;
            }
            if !found {
                continue;
            }
            for _ in 0..STROKE_BISECTIONS {
                let middle = (low + high) * 0.5;
                let (offset_x, offset_y) = stroke_offset(direction, middle);
                let sample = load(
                    source,
                    width,
                    height,
                    column as i32 + offset_x,
                    row as i32 + offset_y,
                )[3];
                if (sample - 0.5) * (centre - 0.5) <= 0.0 {
                    high = middle;
                } else {
                    low = middle;
                }
            }
            best = best.min(high);
        }
        best
    }
}

/// Eight unit probes, starting with the axes so the axis estimates are the
/// uncontaminated ones.
fn stroke_directions() -> [[f32; 2]; STROKE_DIRECTIONS] {
    [
        [1.0, 0.0],
        [-1.0, 0.0],
        [0.0, 1.0],
        [0.0, -1.0],
        [SQRT_HALF, SQRT_HALF],
        [-SQRT_HALF, SQRT_HALF],
        [SQRT_HALF, -SQRT_HALF],
        [-SQRT_HALF, -SQRT_HALF],
    ]
}

/// Integer tap offset for a unit direction at a fractional distance.
fn stroke_offset(direction: [f32; 2], step: f32) -> (i32, i32) {
    (
        (direction[0] * step + 0.5).floor() as i32,
        (direction[1] * step + 0.5).floor() as i32,
    )
}

/// Load a texel, clamping to the frame extent like the WGSL shader does.
fn load(source: &[[f32; 4]], width: usize, height: usize, column: i32, row: i32) -> [f32; 4] {
    let column = column.clamp(0, width as i32 - 1) as usize;
    let row = row.clamp(0, height as i32 - 1) as usize;
    source[row * width + column]
}

/// Errors returned by [`StrokeGrade::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrokeError {
    /// Brush size was not finite or outside `0..=200`.
    InvalidBrushSize,
    /// Brush hardness was not finite or outside `0..=0.95`.
    InvalidBrushHardness,
    /// Opacity was not finite or outside `0..=1`.
    InvalidOpacity,
    /// A colour channel was not finite or outside `0..=1`.
    InvalidColor,
}

impl std::fmt::Display for StrokeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBrushSize => {
                formatter.write_str("stroke brush size must be finite and within 0..=200")
            }
            Self::InvalidBrushHardness => {
                formatter.write_str("stroke brush hardness must be finite and within 0..=0.95")
            }
            Self::InvalidOpacity => {
                formatter.write_str("stroke opacity must be finite and within 0..=1")
            }
            Self::InvalidColor => {
                formatter.write_str("stroke colour components must be finite and within 0..=1")
            }
        }
    }
}

impl std::error::Error for StrokeError {}

/// Apply a stroke to a whole frame in place. Returns whether the kernel ran.
#[must_use]
pub fn stroke_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &StrokeGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_stroke_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Checkpointed kernel used by the render planner.
///
/// The source frame is snapshotted before any pixel is written: a neighbour
/// read from another row band must not observe a half-written band.
pub(crate) fn apply_stroke_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &StrokeGrade,
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
    let source = pixels[..pixel_count].to_vec();
    let rows_per_chunk = (CHECKPOINT_STRIDE / width).max(1);
    let mut start = 0;
    while start < height {
        checkpoint()?;
        let end = (start + rows_per_chunk).min(height);
        let rows = end - start;
        for_each_row_band(
            &mut pixels[start * width..end * width],
            width,
            0..rows,
            |first_row, band| {
                let band_rows = band.len() / width;
                for row in 0..band_rows {
                    let frame_row = start + first_row + row;
                    for column in 0..width {
                        let index = row * width + column;
                        let pixel = source[frame_row * width + column];
                        let limit = grade.radius() + 1.0;
                        let distance =
                            grade.edge_distance(&source, width, height, column, frame_row, limit);
                        let coverage = grade.coverage_at_distance(distance);
                        if coverage <= 0.0 {
                            continue;
                        }
                        band[index] = grade.blend(pixel, coverage);
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

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.0; 4]; width * height]
    }

    fn default_grade(paint_style: StrokePaintStyle) -> StrokeGrade {
        StrokeGrade::new(
            paint_style,
            [1.0, 1.0, 1.0],
            STROKE_DEFAULT_BRUSH_SIZE,
            STROKE_DEFAULT_BRUSH_HARDNESS,
            STROKE_DEFAULT_OPACITY,
        )
        .expect("the default parameters validate")
    }

    /// A vertical half plane: opaque left of the contour, transparent right.
    fn half_plane(width: usize, height: usize, contour: usize) -> Vec<[f32; 4]> {
        let mut pixels = frame(width, height);
        for row in 0..height {
            for column in 0..width {
                let alpha = if column < contour { 1.0 } else { 0.0 };
                pixels[row * width + column] = [0.25, 0.5, 0.75, alpha];
            }
        }
        pixels
    }

    #[test]
    fn stroke_transparent_frame_is_unchanged_for_every_paint_style() {
        for style in StrokePaintStyle::ALL {
            let mut pixels = frame(9, 7);
            let original = pixels.clone();
            assert!(stroke_frame_f32(&mut pixels, 9, 7, &default_grade(style)));
            assert_eq!(
                pixels, original,
                "a transparent frame has no contour, so {style:?} must not write"
            );
        }
    }

    #[test]
    fn stroke_opaque_frame_is_unchanged() {
        for style in StrokePaintStyle::ALL {
            let mut pixels = vec![[0.125, 0.25, 0.5, 1.0]; 9 * 7];
            let original = pixels.clone();
            assert!(stroke_frame_f32(&mut pixels, 9, 7, &default_grade(style)));
            assert_eq!(pixels, original, "an opaque frame has no contour");
        }
    }

    #[test]
    fn stroke_band_is_centred_on_the_contour_and_is_brush_wide() {
        let source = half_plane(41, 5, 20);
        let mut pixels = source.clone();
        let mut sharp = default_grade(StrokePaintStyle::OnTransparent);
        sharp.brush_size = 8.0;
        sharp.brush_hardness = 0.75;
        assert!(stroke_frame_f32(&mut pixels, 41, 5, &sharp));
        // A pixel is inside the band only if the kernel rewrote it: the opaque
        // side of the fixture already has alpha one, so alpha alone cannot
        // tell a covered pixel from an untouched one.
        let covered = |column: usize| pixels[2 * 41 + column] != source[2 * 41 + column];
        let columns = (0..41).filter(|column| covered(*column)).collect::<Vec<_>>();
        assert!(
            columns.iter().any(|column| *column < 20) && columns.iter().any(|column| *column > 20),
            "the band must straddle the contour, got {columns:?}"
        );
        assert!(
            !columns.contains(&0) && !columns.contains(&40),
            "an eight pixel brush cannot reach the frame edge, got {columns:?}"
        );
        assert!(
            (14..=26).contains(&columns[0]) && (14..=26).contains(columns.last().unwrap()),
            "the band is centred on column 20, got {columns:?}"
        );
    }

    #[test]
    fn stroke_width_scales_with_the_brush_size() {
        let wide = {
            let mut pixels = half_plane(81, 3, 40);
            let mut grade = default_grade(StrokePaintStyle::OnTransparent);
            grade.brush_size = 40.0;
            grade.brush_hardness = 0.0;
            assert!(stroke_frame_f32(&mut pixels, 81, 3, &grade));
            pixels
        };
        let narrow = {
            let mut pixels = half_plane(81, 3, 40);
            let mut grade = default_grade(StrokePaintStyle::OnTransparent);
            grade.brush_size = 20.0;
            grade.brush_hardness = 0.0;
            assert!(stroke_frame_f32(&mut pixels, 81, 3, &grade));
            pixels
        };
        let coverage = |pixels: &[[f32; 4]], column: usize| pixels[81 + column][3];
        assert!(
            coverage(&wide, 50) > 0.4,
            "radius 20 still covers ten pixels out"
        );
        assert_eq!(
            coverage(&narrow, 52),
            0.0,
            "radius 10 cannot reach twelve pixels out"
        );
        assert!(
            coverage(&wide, 55) > 0.15,
            "radius 20 reaches fifteen pixels out"
        );
        assert_eq!(
            coverage(&narrow, 59),
            0.0,
            "radius 10 cannot reach nineteen pixels out"
        );
    }

    #[test]
    fn stroke_hardness_sharpens_the_profile() {
        let hardness = |value: f32| {
            let mut pixels = half_plane(81, 3, 40);
            let mut grade = default_grade(StrokePaintStyle::OnTransparent);
            grade.brush_size = 40.0;
            grade.brush_hardness = value;
            assert!(stroke_frame_f32(&mut pixels, 81, 3, &grade));
            pixels
        };
        let soft = hardness(0.0);
        let hard = hardness(0.95);
        let at = |pixels: &[[f32; 4]], column: usize| pixels[81 + column][3];
        assert_eq!(
            at(&hard, 53),
            1.0,
            "the hard profile is solid near the contour"
        );
        assert!(
            at(&soft, 53) < 0.5,
            "the soft profile is a ramp, got {}",
            at(&soft, 53)
        );
        assert!(
            at(&hard, 57) > 0.9,
            "the hard profile stays solid almost to the radius, got {}",
            at(&hard, 57)
        );
        assert!(
            at(&soft, 57) < 0.4,
            "the soft profile has faded well before the radius, got {}",
            at(&soft, 57)
        );
        assert_eq!(at(&hard, 61), 0.0, "nothing past the radius");
    }

    #[test]
    fn stroke_coverage_follows_the_distance_profile_on_a_ramp() {
        // A linear alpha ramp: alpha = 0.5 + 0.125 * (60.5 - x_centre), so the
        // contour sits at x = 60 and the analytic distance is 8 * |alpha - 0.5|.
        let mut pixels = frame(121, 3);
        for row in 0..3 {
            for column in 0..121 {
                let alpha = (0.5 + 0.125 * (60.5 - (column as f32 + 0.5))).clamp(0.0, 1.0);
                pixels[row * 121 + column] = [0.5, 0.5, 0.5, alpha];
            }
        }
        let mut grade = default_grade(StrokePaintStyle::OnTransparent);
        grade.brush_size = 40.0;
        grade.brush_hardness = 0.0;
        // A red ink distinguishes a rewritten pixel from the untouched grey
        // ramp: on the opaque side the source alpha alone cannot.
        grade.color = [0.25, 0.0, 0.0];
        assert!(stroke_frame_f32(&mut pixels, 121, 3, &grade));
        let at = |column: usize| pixels[121 + column][3];
        let covered = |column: usize| pixels[121 + column][1] == 0.0;
        assert!(
            at(60) >= 0.9,
            "the contour pixel is almost solid, got {}",
            at(60)
        );
        for offset in 0..=17 {
            let expected = 1.0 - offset as f32 / 20.0;
            for column in [60 + offset, 60 - offset] {
                assert!(covered(column), "column {column} is inside the band");
                let coverage = at(column);
                assert!(
                    (coverage - expected).abs() <= 0.12,
                    "column {column} should be about {expected}, got {coverage}"
                );
            }
        }
        for offset in [22, 30, 50] {
            assert!(!covered(60 + offset), "no coverage past the radius");
            assert!(!covered(60 - offset), "no coverage past the radius");
        }
    }

    #[test]
    fn stroke_blend_matches_the_straight_alpha_contract() {
        let mut grade = default_grade(StrokePaintStyle::OnOriginal);
        grade.color = [0.0, 0.0, 1.0];
        let source = [1.0, 0.0, 0.0, 0.2];
        let blended = grade.blend(source, 0.5);
        let source_weight = 0.2 * 0.5;
        let out_alpha = 0.5 + source_weight;
        assert!((blended[3] - out_alpha).abs() <= 1e-6);
        assert!((blended[0] - source_weight / out_alpha).abs() <= 1e-6);
        assert!((blended[2] - 0.5 / out_alpha).abs() <= 1e-6);

        let transparent = default_grade(StrokePaintStyle::OnTransparent);
        assert_eq!(
            transparent.blend([0.0, 0.0, 0.0, 0.0], 0.25),
            [1.0, 1.0, 1.0, 0.25]
        );

        let reveal = default_grade(StrokePaintStyle::RevealOriginal);
        assert_eq!(
            reveal.blend([0.25, 0.5, 0.75, 0.8], 0.25),
            [0.25, 0.5, 0.75, 0.2]
        );

        assert_eq!(grade.blend([1.0, 2.0, 3.0, 4.0], 0.0), [1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn stroke_on_original_leaves_the_outer_edge_alone() {
        let mut pixels = half_plane(41, 5, 20);
        let original = pixels.clone();
        let mut grade = default_grade(StrokePaintStyle::OnOriginal);
        grade.brush_size = 6.0;
        assert!(stroke_frame_f32(&mut pixels, 41, 5, &grade));
        assert_eq!(
            pixels[0], original[0],
            "the frame edge keeps its source texel"
        );
        let centre = pixels[2 * 41 + 19];
        assert_eq!(centre[3], 1.0, "an opaque source plus ink stays opaque");
        assert_eq!(
            [centre[0], centre[1], centre[2]],
            [1.0, 1.0, 1.0],
            "the ink wins"
        );
    }

    #[test]
    fn stroke_reveal_original_multiplies_the_source_alpha() {
        let mut pixels = half_plane(41, 5, 20);
        let mut grade = default_grade(StrokePaintStyle::RevealOriginal);
        grade.brush_size = 6.0;
        assert!(stroke_frame_f32(&mut pixels, 41, 5, &grade));
        let centre = pixels[2 * 41 + 19];
        assert_eq!(
            [centre[0], centre[1], centre[2]],
            [0.25, 0.5, 0.75],
            "colour is kept"
        );
        assert!(
            centre[3] > 0.0 && centre[3] <= 1.0,
            "alpha is scaled by the band"
        );
    }

    #[test]
    fn stroke_handles_a_one_pixel_frame() {
        let mut pixels = vec![[0.5, 0.5, 0.5, 1.0]];
        let mut grade = default_grade(StrokePaintStyle::OnOriginal);
        grade.brush_size = 2.0;
        assert!(stroke_frame_f32(&mut pixels, 1, 1, &grade));
        assert_eq!(pixels, vec![[0.5, 0.5, 0.5, 1.0]]);
    }

    #[test]
    fn stroke_rejects_out_of_range_parameters() {
        assert_eq!(
            StrokeGrade::new(StrokePaintStyle::OnOriginal, [1.0; 3], 201.0, 0.5, 1.0),
            Err(StrokeError::InvalidBrushSize)
        );
        assert_eq!(
            StrokeGrade::new(StrokePaintStyle::OnOriginal, [1.0; 3], 2.0, 0.96, 1.0),
            Err(StrokeError::InvalidBrushHardness)
        );
        assert_eq!(
            StrokeGrade::new(StrokePaintStyle::OnOriginal, [1.0; 3], 2.0, 0.5, 1.01),
            Err(StrokeError::InvalidOpacity)
        );
        assert_eq!(
            StrokeGrade::new(StrokePaintStyle::OnOriginal, [1.5; 3], 2.0, 0.5, 1.0),
            Err(StrokeError::InvalidColor)
        );
        assert!(StrokeGrade::new(StrokePaintStyle::OnOriginal, [1.0; 3], 2.0, 0.95, 1.0).is_ok());
    }

    #[test]
    fn stroke_identity_parameters_skip_the_kernel() {
        let mut pixels = half_plane(41, 5, 20);
        let original = pixels.clone();
        let mut grade = default_grade(StrokePaintStyle::OnTransparent);
        grade.brush_size = 0.0;
        assert!(stroke_frame_f32(&mut pixels, 41, 5, &grade));
        assert_eq!(pixels, original);

        grade.brush_size = 20.0;
        grade.opacity = 0.0;
        assert!(stroke_frame_f32(&mut pixels, 41, 5, &grade));
        assert_eq!(pixels, original);
    }

    #[test]
    fn stroke_checkpoints_every_chunk_and_propagates_errors() {
        let mut pixels = half_plane(41, 300, 20);
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), ()>(())
        };
        let mut grade = default_grade(StrokePaintStyle::OnTransparent);
        grade.brush_size = 4.0;
        assert!(
            apply_stroke_f32_controlled(&mut pixels, 41, 300, &grade, &mut checkpoint).unwrap()
        );
        // 4096 / 41 = 99 rows per chunk, so 300 rows take four chunks plus the
        // trailing checkpoint.
        assert_eq!(calls, 5);

        let mut pixels = half_plane(41, 10, 20);
        let mut calls = 0;
        let mut failing = || {
            calls += 1;
            Err::<(), ()>(())
        };
        assert!(apply_stroke_f32_controlled(&mut pixels, 41, 10, &grade, &mut failing).is_err());
        assert_eq!(calls, 1);
    }
}
