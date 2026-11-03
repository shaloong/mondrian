//! Checkerboard generator (`builtin.checkerboard`).
//!
//! The grade is a pure function of the pixel index: it reads no source sample,
//! allocates no scratch and is therefore a `PixelLocal` generator. [`apply`]
//! blends the generated colour over the incoming pixel with
//! [`crate::adjustment::blend_rgba_f32_pixel_seeded`], and the GPU side reads
//! the same three uniform cells through [`CheckerboardGrade::gpu_controls`].
//!
//! # Geometry
//!
//! `anchor` is the corner of the cell that contains parity zero. The pixel
//! centre is `index + 0.5`, the offset into cell space is
//! `(centre - anchor) / cell`, and the parity of a cell is
//! `floor(offset) - 2 * floor(floor(offset) / 2)`: zero for even cells, one for
//! odd cells. Cells whose parity matches on both axes are painted, which makes
//! the pattern a checkerboard rather than a set of stripes.
//!
//! # Coverage
//!
//! Feathering interpolates the checkerboard between the cells that meet at the
//! pixel: `axis` is the weight of the pixel's own parity on each axis, and
//! `coverage = axis.x * axis.y + (1 - axis.x) * (1 - axis.y)` sums the two
//! parity-equal (painted) pairs. That is the form that keeps the documented
//! phase at a hard edge — `(0, 0)` and `(1, 1)` painted, `(1, 0)` and `(0, 1)`
//! empty — while still returning exactly `0.5` on a feathered edge; the bare
//! product `axis.x * axis.y` would paint odd-odd cells only and contradicts the
//! documented phase.
//!
//! # Degenerate cells
//!
//! A cell narrower than [`CHECKERBOARD_DEGENERATE_CELL`] on either axis paints
//! nothing: the generator is empty rather than a solid slab. A grade whose cell
//! is degenerate, or whose opacity is zero, is an identity grade and
//! [`apply`] returns the incoming pixel untouched.

use mondrian_core::BlendMode;

/// Largest absolute value accepted for `anchor` and `corner`, in pixels.
pub const CHECKERBOARD_MAX_COORDINATE: f32 = 1_000_000.0;

/// Smallest authored cell width and height, in pixels.
pub const CHECKERBOARD_MIN_WIDTH: f32 = 1.0;

/// Largest authored cell width and height, in pixels.
pub const CHECKERBOARD_MAX_WIDTH: f32 = 4_000.0;

/// Largest authored feather width and height, in pixels.
pub const CHECKERBOARD_MAX_FEATHER: f32 = 400.0;

/// Default opacity, in percent.
pub const CHECKERBOARD_DEFAULT_OPACITY: f32 = 100.0;

/// Default anchor, in pixels.
pub const CHECKERBOARD_DEFAULT_ANCHOR: [f32; 2] = [128.0, 128.0];

/// Default corner, in pixels. The corner delta is `20.48` on both axes.
pub const CHECKERBOARD_DEFAULT_CORNER: [f32; 2] = [148.48, 148.48];

/// Default cell width, in pixels.
pub const CHECKERBOARD_DEFAULT_WIDTH: f32 = 16.0;

/// Default cell height, in pixels.
pub const CHECKERBOARD_DEFAULT_HEIGHT: f32 = 8.0;

/// Default way the cell size is authored.
pub const CHECKERBOARD_DEFAULT_SIZE_FROM: CheckerboardSizeFrom = CheckerboardSizeFrom::WidthSlider;

/// Cells smaller than this on either axis paint nothing.
pub const CHECKERBOARD_DEGENERATE_CELL: f32 = 1.0e-3;

/// Number of pixels between cooperative checkpoints inside a region pass.
const CHECKPOINT_STRIDE: usize = 4_096;

/// How the cell size is derived from the authored parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckerboardSizeFrom {
    /// Square cells sized by the `corner - anchor` delta.
    Corner,
    /// Square cells sized by `width`; `height` is ignored.
    WidthSlider,
    /// Rectangular cells sized by `width` and `height`.
    WidthHeightSliders,
}

impl CheckerboardSizeFrom {
    /// Every size mode, in uniform-index order.
    pub const ALL: [Self; 3] = [Self::Corner, Self::WidthSlider, Self::WidthHeightSliders];

    /// The stable key used by the authoring schema and the message id suffix.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Corner => "corner",
            Self::WidthSlider => "width_slider",
            Self::WidthHeightSliders => "width_height_sliders",
        }
    }

    /// The Chinese option label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Corner => "角点",
            Self::WidthSlider => "宽度滑块",
            Self::WidthHeightSliders => "宽高滑块",
        }
    }

    /// The uniform index packed into `header.y`.
    #[must_use]
    pub const fn index(self) -> u32 {
        match self {
            Self::Corner => 0,
            Self::WidthSlider => 1,
            Self::WidthHeightSliders => 2,
        }
    }

    /// Parses a stable key; unknown keys are rejected.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }
}

/// Why a [`CheckerboardGrade`] could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckerboardError {
    /// The anchor had a non-finite component.
    NonFiniteAnchor,
    /// The anchor left the documented coordinate range.
    AnchorOutOfRange,
    /// The corner had a non-finite component.
    NonFiniteCorner,
    /// The corner left the documented coordinate range.
    CornerOutOfRange,
    /// The width was not finite.
    NonFiniteWidth,
    /// The width left the documented range.
    WidthOutOfRange,
    /// The height was not finite.
    NonFiniteHeight,
    /// The height left the documented range.
    HeightOutOfRange,
    /// The feather width was not finite.
    NonFiniteFeatherWidth,
    /// The feather width left the documented range.
    FeatherWidthOutOfRange,
    /// The feather height was not finite.
    NonFiniteFeatherHeight,
    /// The feather height left the documented range.
    FeatherHeightOutOfRange,
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

impl std::fmt::Display for CheckerboardError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFiniteAnchor => formatter.write_str("checkerboard anchor must be finite"),
            Self::AnchorOutOfRange => formatter.write_str("checkerboard anchor is out of range"),
            Self::NonFiniteCorner => formatter.write_str("checkerboard corner must be finite"),
            Self::CornerOutOfRange => formatter.write_str("checkerboard corner is out of range"),
            Self::NonFiniteWidth => formatter.write_str("checkerboard width must be finite"),
            Self::WidthOutOfRange => formatter.write_str("checkerboard width is out of range"),
            Self::NonFiniteHeight => formatter.write_str("checkerboard height must be finite"),
            Self::HeightOutOfRange => formatter.write_str("checkerboard height is out of range"),
            Self::NonFiniteFeatherWidth => {
                formatter.write_str("checkerboard feather width must be finite")
            }
            Self::FeatherWidthOutOfRange => {
                formatter.write_str("checkerboard feather width is out of range")
            }
            Self::NonFiniteFeatherHeight => {
                formatter.write_str("checkerboard feather height must be finite")
            }
            Self::FeatherHeightOutOfRange => {
                formatter.write_str("checkerboard feather height is out of range")
            }
            Self::NonFiniteColor => formatter.write_str("checkerboard colour must be finite"),
            Self::ColorOutOfRange => formatter.write_str("checkerboard colour is out of range"),
            Self::NonFiniteOpacity => formatter.write_str("checkerboard opacity must be finite"),
            Self::OpacityOutOfRange => formatter.write_str("checkerboard opacity is out of range"),
            Self::DissolveUnsupported => {
                formatter.write_str("checkerboard does not support dissolve blending")
            }
        }
    }
}

impl std::error::Error for CheckerboardError {}

/// The three uniform cells consumed by the GPU branch, in packing order.
pub type CheckerboardControls = ([f32; 4], [f32; 4], [f32; 4]);

/// A validated checkerboard generator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CheckerboardGrade {
    anchor: [f32; 2],
    size_from: CheckerboardSizeFrom,
    corner: [f32; 2],
    width: f32,
    height: f32,
    feather_width: f32,
    feather_height: f32,
    color: [f32; 3],
    opacity: f32,
    blend_mode: BlendMode,
}

impl CheckerboardGrade {
    /// Validates and builds a grade.
    ///
    /// `width` and `height` accept the degenerate range down to zero so the
    /// identity path is representable; the authoring schema and the graph
    /// builder enforce [`CHECKERBOARD_MIN_WIDTH`].
    ///
    /// # Errors
    ///
    /// Returns [`CheckerboardError`] when a field is non-finite, leaves its
    /// documented range, or asks for dissolve blending.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        anchor: [f32; 2],
        size_from: CheckerboardSizeFrom,
        corner: [f32; 2],
        width: f32,
        height: f32,
        feather_width: f32,
        feather_height: f32,
        color: [f32; 3],
        opacity: f32,
        blend_mode: BlendMode,
    ) -> Result<Self, CheckerboardError> {
        if !anchor[0].is_finite() || !anchor[1].is_finite() {
            return Err(CheckerboardError::NonFiniteAnchor);
        }
        if anchor[0].abs() > CHECKERBOARD_MAX_COORDINATE
            || anchor[1].abs() > CHECKERBOARD_MAX_COORDINATE
        {
            return Err(CheckerboardError::AnchorOutOfRange);
        }
        if !corner[0].is_finite() || !corner[1].is_finite() {
            return Err(CheckerboardError::NonFiniteCorner);
        }
        if corner[0].abs() > CHECKERBOARD_MAX_COORDINATE
            || corner[1].abs() > CHECKERBOARD_MAX_COORDINATE
        {
            return Err(CheckerboardError::CornerOutOfRange);
        }
        if !width.is_finite() {
            return Err(CheckerboardError::NonFiniteWidth);
        }
        if !(0.0..=CHECKERBOARD_MAX_WIDTH).contains(&width) {
            return Err(CheckerboardError::WidthOutOfRange);
        }
        if !height.is_finite() {
            return Err(CheckerboardError::NonFiniteHeight);
        }
        if !(0.0..=CHECKERBOARD_MAX_WIDTH).contains(&height) {
            return Err(CheckerboardError::HeightOutOfRange);
        }
        if !feather_width.is_finite() {
            return Err(CheckerboardError::NonFiniteFeatherWidth);
        }
        if !(0.0..=CHECKERBOARD_MAX_FEATHER).contains(&feather_width) {
            return Err(CheckerboardError::FeatherWidthOutOfRange);
        }
        if !feather_height.is_finite() {
            return Err(CheckerboardError::NonFiniteFeatherHeight);
        }
        if !(0.0..=CHECKERBOARD_MAX_FEATHER).contains(&feather_height) {
            return Err(CheckerboardError::FeatherHeightOutOfRange);
        }
        if !color[0].is_finite() || !color[1].is_finite() || !color[2].is_finite() {
            return Err(CheckerboardError::NonFiniteColor);
        }
        if !(0.0..=1.0).contains(&color[0])
            || !(0.0..=1.0).contains(&color[1])
            || !(0.0..=1.0).contains(&color[2])
        {
            return Err(CheckerboardError::ColorOutOfRange);
        }
        if !opacity.is_finite() {
            return Err(CheckerboardError::NonFiniteOpacity);
        }
        if !(0.0..=CHECKERBOARD_DEFAULT_OPACITY).contains(&opacity) {
            return Err(CheckerboardError::OpacityOutOfRange);
        }
        if matches!(blend_mode, BlendMode::Dissolve) {
            return Err(CheckerboardError::DissolveUnsupported);
        }
        Ok(Self {
            anchor,
            size_from,
            corner,
            width,
            height,
            feather_width,
            feather_height,
            color,
            opacity: opacity / CHECKERBOARD_DEFAULT_OPACITY,
            blend_mode,
        })
    }

    /// The anchor, in pixels.
    #[must_use]
    pub const fn anchor(self) -> [f32; 2] {
        self.anchor
    }

    /// How the cell size is authored.
    #[must_use]
    pub const fn size_from(self) -> CheckerboardSizeFrom {
        self.size_from
    }

    /// The corner, in pixels.
    #[must_use]
    pub const fn corner(self) -> [f32; 2] {
        self.corner
    }

    /// The authored cell width, in pixels.
    #[must_use]
    pub const fn width(self) -> f32 {
        self.width
    }

    /// The authored cell height, in pixels.
    #[must_use]
    pub const fn height(self) -> f32 {
        self.height
    }

    /// The feather width, in pixels.
    #[must_use]
    pub const fn feather_width(self) -> f32 {
        self.feather_width
    }

    /// The feather height, in pixels.
    #[must_use]
    pub const fn feather_height(self) -> f32 {
        self.feather_height
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

    /// The blend-mode index packed into `header.z`.
    #[must_use]
    pub fn blend_mode_index(self) -> u32 {
        crate::glow::blend_mode_index(self.blend_mode)
    }

    /// The effective cell size in pixels, per axis.
    #[must_use]
    pub fn cell_size(self) -> [f32; 2] {
        match self.size_from {
            CheckerboardSizeFrom::Corner => [
                (self.corner[0] - self.anchor[0]).abs(),
                (self.corner[1] - self.anchor[1]).abs(),
            ],
            CheckerboardSizeFrom::WidthSlider => [self.width, self.width],
            CheckerboardSizeFrom::WidthHeightSliders => [self.width, self.height],
        }
    }

    /// Whether the grade paints nothing at all.
    #[must_use]
    pub fn is_identity(self) -> bool {
        if self.opacity <= 1.0e-6 {
            return true;
        }
        let cell = self.cell_size();
        cell[0] < CHECKERBOARD_DEGENERATE_CELL || cell[1] < CHECKERBOARD_DEGENERATE_CELL
    }

    /// The three uniform cells, in shader order.
    #[must_use]
    pub fn gpu_controls(self) -> CheckerboardControls {
        (
            [
                self.anchor[0],
                self.anchor[1],
                self.corner[0],
                self.corner[1],
            ],
            [self.color[0], self.color[1], self.color[2], self.opacity],
            [
                self.width,
                self.height,
                self.feather_width,
                self.feather_height,
            ],
        )
    }

    /// The painted coverage at a pixel index, in `0.0..=1.0`.
    #[must_use]
    pub fn coverage(self, column: u32, row: u32) -> f32 {
        let cell = self.cell_size();
        if cell[0] < CHECKERBOARD_DEGENERATE_CELL || cell[1] < CHECKERBOARD_DEGENERATE_CELL {
            return 0.0;
        }
        let centre_x = column as f32 + 0.5;
        let centre_y = row as f32 + 0.5;
        let offset_x = (centre_x - self.anchor[0]) / cell[0];
        let offset_y = (centre_y - self.anchor[1]) / cell[1];
        let cell_x = offset_x.floor();
        let cell_y = offset_y.floor();
        let parity_x = cell_x - 2.0 * (cell_x * 0.5).floor();
        let parity_y = cell_y - 2.0 * (cell_y * 0.5).floor();
        let within_x = offset_x - cell_x;
        let within_y = offset_y - cell_y;
        let distance_x = within_x.min(1.0 - within_x) * cell[0];
        let distance_y = within_y.min(1.0 - within_y) * cell[1];
        let axis_x = 0.5 + (parity_x - 0.5) * edge_ramp(distance_x, self.feather_width);
        let axis_y = 0.5 + (parity_y - 0.5) * edge_ramp(distance_y, self.feather_height);
        axis_x * axis_y + (1.0 - axis_x) * (1.0 - axis_y)
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

/// The feathered edge weight for one axis; `1.0` is a hard edge.
fn edge_ramp(distance: f32, feather: f32) -> f32 {
    if feather <= 0.0 {
        return 1.0;
    }
    (distance / (feather * 0.5)).clamp(0.0, 1.0)
}

/// The generated colour and coverage at a pixel index.
///
/// Empty pixels are transparent black: the generator contributes no colour
/// where the coverage is zero, so the value is directly usable as a source.
#[must_use]
pub fn checkerboard_pixel(column: u32, row: u32, grade: &CheckerboardGrade) -> [f32; 4] {
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
pub fn checkerboard_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &CheckerboardGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_checkerboard_f32_region_controlled(pixels, 0, 0, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies the generator to a region whose origin is given in frame-absolute
/// pixel coordinates.
///
/// # Errors
///
/// Propagates the first error returned by `checkpoint`, leaving already written
/// chunks in place.
pub(crate) fn apply_checkerboard_f32_region_controlled<E>(
    pixels: &mut [[f32; 4]],
    region_x: usize,
    region_y: usize,
    width: usize,
    height: usize,
    grade: &CheckerboardGrade,
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

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.0; 4]; width * height]
    }

    fn new_grade(
        anchor: [f32; 2],
        size_from: CheckerboardSizeFrom,
        corner: [f32; 2],
        width: f32,
        height: f32,
        feather_width: f32,
        feather_height: f32,
        color: [f32; 3],
        opacity: f32,
        blend_mode: BlendMode,
    ) -> CheckerboardGrade {
        CheckerboardGrade::new(
            anchor,
            size_from,
            corner,
            width,
            height,
            feather_width,
            feather_height,
            color,
            opacity,
            blend_mode,
        )
        .expect("the test grade must be valid")
    }

    fn default_grade() -> CheckerboardGrade {
        new_grade(
            CHECKERBOARD_DEFAULT_ANCHOR,
            CHECKERBOARD_DEFAULT_SIZE_FROM,
            CHECKERBOARD_DEFAULT_CORNER,
            CHECKERBOARD_DEFAULT_WIDTH,
            CHECKERBOARD_DEFAULT_HEIGHT,
            0.0,
            0.0,
            [1.0, 1.0, 1.0],
            CHECKERBOARD_DEFAULT_OPACITY,
            BlendMode::Normal,
        )
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() <= 1.0e-6
    }

    #[test]
    fn checkerboard_defaults_match_the_dump() {
        let grade = default_grade();
        let (params, color, extra0) = grade.gpu_controls();
        assert_eq!(params, [128.0, 128.0, 148.48, 148.48]);
        assert_eq!(color, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(extra0, [16.0, 8.0, 0.0, 0.0]);
        assert_eq!(grade.size_from(), CheckerboardSizeFrom::WidthSlider);
        assert_eq!(grade.size_from().index(), 1);
        assert_eq!(grade.blend_mode(), BlendMode::Normal);
        assert_eq!(CheckerboardSizeFrom::ALL.len(), 3);
        assert_eq!(
            CheckerboardSizeFrom::ALL.map(CheckerboardSizeFrom::as_key),
            ["corner", "width_slider", "width_height_sliders"]
        );
        assert_eq!(
            CheckerboardSizeFrom::from_key("width_height_sliders"),
            Some(CheckerboardSizeFrom::WidthHeightSliders)
        );
        assert_eq!(CheckerboardSizeFrom::from_key("unknown"), None);
        assert_eq!(grade.cell_size(), [16.0, 16.0]);
    }

    #[test]
    fn checkerboard_default_frame_matches_the_documented_cells() {
        let grade = default_grade();
        assert_eq!(checkerboard_pixel(128, 128, &grade), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(checkerboard_pixel(144, 128, &grade), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(checkerboard_pixel(128, 144, &grade), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(checkerboard_pixel(144, 144, &grade), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn checkerboard_feathered_edges_partition_coverage() {
        let grade = new_grade(
            CHECKERBOARD_DEFAULT_ANCHOR,
            CheckerboardSizeFrom::WidthSlider,
            CHECKERBOARD_DEFAULT_CORNER,
            16.0,
            8.0,
            8.0,
            8.0,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        let left = grade.coverage(143, 128);
        let right = grade.coverage(144, 128);
        assert!(
            close(left + right, 1.0),
            "horizontal neighbours must partition coverage: {left} + {right}"
        );
        let above = grade.coverage(144, 127);
        let below = grade.coverage(144, 128);
        assert!(
            close(above + below, 1.0),
            "vertical neighbours must partition coverage: {above} + {below}"
        );
        assert!(right > 0.0 && right < 1.0, "the edge must be soft: {right}");
    }

    #[test]
    fn checkerboard_hard_edge_is_binary() {
        let grade = new_grade(
            CHECKERBOARD_DEFAULT_ANCHOR,
            CheckerboardSizeFrom::WidthSlider,
            CHECKERBOARD_DEFAULT_CORNER,
            4.0,
            4.0,
            0.0,
            0.0,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        let mut pixels = frame(32, 32);
        assert!(checkerboard_frame_f32(&mut pixels, 32, 32, &grade));
        let mut painted = 0;
        let mut empty = 0;
        for (index, pixel) in pixels.iter().enumerate() {
            assert_eq!(pixel[3], pixel[3].round(), "pixel {index} is not binary");
            assert_eq!(pixel[0], pixel[3]);
            if pixel[3] == 1.0 {
                painted += 1;
            } else {
                empty += 1;
                assert_eq!(*pixel, [0.0, 0.0, 0.0, 0.0]);
            }
        }
        assert!(painted > 0 && empty > 0, "the frame must show both cells");
    }

    #[test]
    fn checkerboard_transparent_base_keeps_only_covered_pixels() {
        let grade = default_grade();
        let mut pixels = frame(160, 160);
        assert!(checkerboard_frame_f32(&mut pixels, 160, 160, &grade));
        assert_eq!(pixels[128 * 160 + 128], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(pixels[128 * 160 + 144], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(pixels[144 * 160 + 128], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(pixels[144 * 160 + 144], [1.0, 1.0, 1.0, 1.0]);
        let mut painted = vec![[0.2, 0.3, 0.4, 0.5]; 160 * 160];
        assert!(checkerboard_frame_f32(&mut painted, 160, 160, &grade));
        assert_eq!(painted[128 * 160 + 128], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(painted[128 * 160 + 144], [0.2, 0.3, 0.4, 0.5]);
    }

    #[test]
    fn checkerboard_corner_mode_uses_the_corner_delta() {
        let grade = new_grade(
            [0.0, 0.0],
            CheckerboardSizeFrom::Corner,
            [10.0, 20.0],
            16.0,
            8.0,
            0.0,
            0.0,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        assert_eq!(grade.cell_size(), [10.0, 20.0]);
        assert_eq!(checkerboard_pixel(0, 0, &grade), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(checkerboard_pixel(10, 0, &grade), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(checkerboard_pixel(0, 20, &grade), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(checkerboard_pixel(10, 20, &grade), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn checkerboard_width_slider_makes_square_cells() {
        let grade = new_grade(
            [0.0, 0.0],
            CheckerboardSizeFrom::WidthSlider,
            [148.48, 148.48],
            10.0,
            40.0,
            0.0,
            0.0,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        assert_eq!(grade.cell_size(), [10.0, 10.0]);
        assert_eq!(checkerboard_pixel(0, 10, &grade), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(checkerboard_pixel(0, 20, &grade), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn checkerboard_width_height_sliders_use_both_axes() {
        let grade = new_grade(
            [0.0, 0.0],
            CheckerboardSizeFrom::WidthHeightSliders,
            [148.48, 148.48],
            10.0,
            40.0,
            0.0,
            0.0,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        assert_eq!(grade.cell_size(), [10.0, 40.0]);
        assert_eq!(checkerboard_pixel(0, 10, &grade), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(checkerboard_pixel(0, 40, &grade), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(checkerboard_pixel(0, 80, &grade), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn checkerboard_degenerate_cell_is_identity() {
        let grade = new_grade(
            CHECKERBOARD_DEFAULT_ANCHOR,
            CheckerboardSizeFrom::WidthHeightSliders,
            CHECKERBOARD_DEFAULT_CORNER,
            0.0005,
            0.0005,
            0.0,
            0.0,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        assert!(grade.is_identity());
        assert_eq!(grade.coverage(0, 0), 0.0);
        let mut pixels = frame(8, 8);
        pixels[0] = [0.5, 0.25, 0.125, 0.75];
        assert!(checkerboard_frame_f32(&mut pixels, 8, 8, &grade));
        assert!(pixels
            .iter()
            .all(|pixel| *pixel == [0.0, 0.0, 0.0, 0.0] || *pixel == [0.5, 0.25, 0.125, 0.75]));
        let coincident = new_grade(
            [5.0, 5.0],
            CheckerboardSizeFrom::Corner,
            [5.0, 5.0],
            16.0,
            8.0,
            0.0,
            0.0,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        assert!(coincident.is_identity());
        assert_eq!(
            coincident.apply([0.1, 0.2, 0.3, 0.4], 4, 4),
            [0.1, 0.2, 0.3, 0.4]
        );
    }

    #[test]
    fn checkerboard_zero_opacity_is_identity() {
        let grade = new_grade(
            CHECKERBOARD_DEFAULT_ANCHOR,
            CheckerboardSizeFrom::WidthSlider,
            CHECKERBOARD_DEFAULT_CORNER,
            16.0,
            8.0,
            2.0,
            2.0,
            [1.0, 0.5, 0.25],
            0.0,
            BlendMode::Multiply,
        );
        assert!(grade.is_identity());
        let mut pixels = frame(8, 8);
        assert!(checkerboard_frame_f32(&mut pixels, 8, 8, &grade));
        assert!(pixels.iter().all(|pixel| *pixel == [0.0; 4]));
        assert_eq!(
            grade.apply([0.1, 0.2, 0.3, 0.4], 128, 128),
            [0.1, 0.2, 0.3, 0.4]
        );
    }

    #[test]
    fn checkerboard_region_matches_the_full_frame() {
        let grade = new_grade(
            [96.0, 96.0],
            CheckerboardSizeFrom::WidthHeightSliders,
            CHECKERBOARD_DEFAULT_CORNER,
            7.0,
            5.0,
            3.0,
            2.0,
            [0.75, 0.5, 0.25],
            80.0,
            BlendMode::Screen,
        );
        let mut full = frame(32, 32);
        for row in 0..32 {
            for column in 0..32 {
                full[row * 32 + column] = [column as f32 * 0.01, row as f32 * 0.01, 0.25, 0.5];
            }
        }
        let mut patch = frame(16, 16);
        for row in 0..16 {
            for column in 0..16 {
                patch[row * 16 + column] = [
                    (8 + column) as f32 * 0.01,
                    (8 + row) as f32 * 0.01,
                    0.25,
                    0.5,
                ];
            }
        }
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert!(matches!(
            apply_checkerboard_f32_region_controlled(
                &mut full,
                0,
                0,
                32,
                32,
                &grade,
                &mut checkpoint
            ),
            Ok(true)
        ));
        assert!(matches!(
            apply_checkerboard_f32_region_controlled(
                &mut patch,
                8,
                8,
                16,
                16,
                &grade,
                &mut checkpoint
            ),
            Ok(true)
        ));
        for row in 0..16 {
            for column in 0..16 {
                assert_eq!(
                    full[(8 + row) * 32 + 8 + column],
                    patch[row * 16 + column],
                    "region pixel ({column}, {row}) must match the full pass"
                );
            }
        }
    }

    #[test]
    fn checkerboard_checkpoints_once_per_chunk() {
        let grade = default_grade();
        let mut reference = frame(4_096, 2);
        let mut passthrough = || Ok::<(), std::convert::Infallible>(());
        assert!(matches!(
            apply_checkerboard_f32_region_controlled(
                &mut reference,
                0,
                0,
                4_096,
                2,
                &grade,
                &mut passthrough
            ),
            Ok(true)
        ));
        assert!(
            reference.iter().any(|pixel| pixel[3] == 0.0),
            "the row must cross a cell boundary"
        );

        let mut pixels = frame(4_096, 2);
        let mut checkpoints = 0usize;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(matches!(
            apply_checkerboard_f32_region_controlled(
                &mut pixels,
                0,
                0,
                4_096,
                2,
                &grade,
                &mut checkpoint
            ),
            Ok(true)
        ));
        assert_eq!(checkpoints, 3, "one checkpoint per chunk plus the trailer");
        assert_eq!(pixels, reference);

        let mut aborted = frame(4_096, 2);
        let mut calls = 0usize;
        let mut aborting = || {
            calls += 1;
            if calls == 2 {
                Err("aborted")
            } else {
                Ok(())
            }
        };
        let result = apply_checkerboard_f32_region_controlled(
            &mut aborted,
            0,
            0,
            4_096,
            2,
            &grade,
            &mut aborting,
        );
        assert_eq!(result, Err("aborted"));
        assert_eq!(calls, 2);
        assert_eq!(
            aborted[..4_096],
            reference[..4_096],
            "the first chunk must be fully written before the second checkpoint"
        );
        assert!(aborted[4_096..].iter().all(|pixel| *pixel == [0.0; 4]));
    }
}
