//! Grid generator (`builtin.grid`).
//!
//! The grade is a pure function of the pixel index: it reads no source sample,
//! allocates no scratch and is therefore a `PixelLocal` generator. [`apply`]
//! blends the generated colour over the incoming pixel with
//! [`crate::adjustment::blend_rgba_f32_pixel_seeded`], and the GPU side reads
//! the same four uniform cells through [`GridGrade::gpu_controls`].
//!
//! # Geometry
//!
//! `anchor` is the origin of the grid. The pixel centre is `index + 0.5`, the
//! offset into cell space is `(centre - anchor) / cell`, and the distance from
//! the pixel centre to the nearest cell boundary on each axis is
//! `min(within, 1 - within) * cell`. A boundary is `border` pixels wide and
//! centred on the boundary, so the input pixel is painted while its centre is
//! within `border / 2` of a boundary.
//!
//! # Coverage
//!
//! The two axes are unioned with `max`: a pixel is covered when it is close to
//! a horizontal *or* a vertical boundary, which is exactly what "draw a grid"
//! means. Feathering replaces the hard test on each axis with the ramp
//! `clamp((half + feather / 2 - distance) / feather, 0, 1)`, so the union stays
//! the maximum of the two ramps. `invert` turns the lines into the cells: the
//! coverage becomes `1 - base`, leaving the grid line transparent and the cell
//! interior painted.
//!
//! # Degenerate cells
//!
//! A cell narrower than [`GRID_DEGENERATE_CELL`] on either axis contributes no
//! coverage at all: the grid is empty rather than a solid slab. When `invert`
//! is set that empty base becomes full coverage, so a degenerate inverted grid
//! paints the whole frame instead of silently disappearing. A grade whose cell
//! is degenerate (and which is not inverted), or whose opacity is zero, is an
//! identity grade and [`apply`] returns the incoming pixel untouched.

use mondrian_core::BlendMode;

/// Largest absolute value accepted for `anchor` and `corner`, in pixels.
pub const GRID_MAX_COORDINATE: f32 = 1_000_000.0;

/// Smallest authored cell width and height, in pixels.
pub const GRID_MIN_WIDTH: f32 = 1.0;

/// Largest authored cell width and height, in pixels.
pub const GRID_MAX_WIDTH: f32 = 4_000.0;

/// Largest authored border width, in pixels.
pub const GRID_MAX_BORDER: f32 = 2_000.0;

/// Largest authored feather width and height, in pixels.
pub const GRID_MAX_FEATHER: f32 = 400.0;

/// Default opacity, in percent.
pub const GRID_DEFAULT_OPACITY: f32 = 100.0;

/// Default anchor, in pixels.
pub const GRID_DEFAULT_ANCHOR: [f32; 2] = [128.0, 128.0];

/// Default corner, in pixels. The corner delta is `25.6` on both axes.
pub const GRID_DEFAULT_CORNER: [f32; 2] = [153.6, 153.6];

/// Default cell width, in pixels.
pub const GRID_DEFAULT_WIDTH: f32 = 32.0;

/// Default cell height, in pixels.
pub const GRID_DEFAULT_HEIGHT: f32 = 24.0;

/// Default border width, in pixels.
pub const GRID_DEFAULT_BORDER: f32 = 5.0;

/// Default way the cell size is authored.
pub const GRID_DEFAULT_SIZE_FROM: GridSizeFrom = GridSizeFrom::Corner;

/// Cells smaller than this on either axis contribute no coverage.
pub const GRID_DEGENERATE_CELL: f32 = 1.0e-3;

/// Number of pixels between cooperative checkpoints inside a region pass.
const CHECKPOINT_STRIDE: usize = 4_096;

/// How the cell size is derived from the authored parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridSizeFrom {
    /// Square cells sized by the `corner - anchor` delta.
    Corner,
    /// Square cells sized by `width`; `height` is ignored.
    WidthSlider,
    /// Rectangular cells sized by `width` and `height`.
    WidthHeightSliders,
}

impl GridSizeFrom {
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

/// Why a [`GridGrade`] could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridError {
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
    /// The border was not finite.
    NonFiniteBorder,
    /// The border left the documented range.
    BorderOutOfRange,
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

impl std::fmt::Display for GridError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFiniteAnchor => formatter.write_str("grid anchor must be finite"),
            Self::AnchorOutOfRange => formatter.write_str("grid anchor is out of range"),
            Self::NonFiniteCorner => formatter.write_str("grid corner must be finite"),
            Self::CornerOutOfRange => formatter.write_str("grid corner is out of range"),
            Self::NonFiniteWidth => formatter.write_str("grid width must be finite"),
            Self::WidthOutOfRange => formatter.write_str("grid width is out of range"),
            Self::NonFiniteHeight => formatter.write_str("grid height must be finite"),
            Self::HeightOutOfRange => formatter.write_str("grid height is out of range"),
            Self::NonFiniteBorder => formatter.write_str("grid border must be finite"),
            Self::BorderOutOfRange => formatter.write_str("grid border is out of range"),
            Self::NonFiniteFeatherWidth => formatter.write_str("grid feather width must be finite"),
            Self::FeatherWidthOutOfRange => {
                formatter.write_str("grid feather width is out of range")
            }
            Self::NonFiniteFeatherHeight => {
                formatter.write_str("grid feather height must be finite")
            }
            Self::FeatherHeightOutOfRange => {
                formatter.write_str("grid feather height is out of range")
            }
            Self::NonFiniteColor => formatter.write_str("grid colour must be finite"),
            Self::ColorOutOfRange => formatter.write_str("grid colour is out of range"),
            Self::NonFiniteOpacity => formatter.write_str("grid opacity must be finite"),
            Self::OpacityOutOfRange => formatter.write_str("grid opacity is out of range"),
            Self::DissolveUnsupported => {
                formatter.write_str("grid does not support dissolve blending")
            }
        }
    }
}

impl std::error::Error for GridError {}

/// The four uniform cells consumed by the GPU branch, in packing order.
pub type GridControls = ([f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// A validated grid generator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridGrade {
    anchor: [f32; 2],
    size_from: GridSizeFrom,
    corner: [f32; 2],
    width: f32,
    height: f32,
    border: f32,
    feather_width: f32,
    feather_height: f32,
    invert: bool,
    color: [f32; 3],
    opacity: f32,
    blend_mode: BlendMode,
}

impl GridGrade {
    /// Validates and builds a grade.
    ///
    /// `width` and `height` accept the degenerate range down to zero so the
    /// identity path is representable; the authoring schema and the graph
    /// builder enforce [`GRID_MIN_WIDTH`].
    ///
    /// # Errors
    ///
    /// Returns [`GridError`] when a field is non-finite, leaves its documented
    /// range, or asks for dissolve blending.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        anchor: [f32; 2],
        size_from: GridSizeFrom,
        corner: [f32; 2],
        width: f32,
        height: f32,
        border: f32,
        feather_width: f32,
        feather_height: f32,
        invert: bool,
        color: [f32; 3],
        opacity: f32,
        blend_mode: BlendMode,
    ) -> Result<Self, GridError> {
        if !anchor[0].is_finite() || !anchor[1].is_finite() {
            return Err(GridError::NonFiniteAnchor);
        }
        if anchor[0].abs() > GRID_MAX_COORDINATE || anchor[1].abs() > GRID_MAX_COORDINATE {
            return Err(GridError::AnchorOutOfRange);
        }
        if !corner[0].is_finite() || !corner[1].is_finite() {
            return Err(GridError::NonFiniteCorner);
        }
        if corner[0].abs() > GRID_MAX_COORDINATE || corner[1].abs() > GRID_MAX_COORDINATE {
            return Err(GridError::CornerOutOfRange);
        }
        if !width.is_finite() {
            return Err(GridError::NonFiniteWidth);
        }
        if !(0.0..=GRID_MAX_WIDTH).contains(&width) {
            return Err(GridError::WidthOutOfRange);
        }
        if !height.is_finite() {
            return Err(GridError::NonFiniteHeight);
        }
        if !(0.0..=GRID_MAX_WIDTH).contains(&height) {
            return Err(GridError::HeightOutOfRange);
        }
        if !border.is_finite() {
            return Err(GridError::NonFiniteBorder);
        }
        if !(0.0..=GRID_MAX_BORDER).contains(&border) {
            return Err(GridError::BorderOutOfRange);
        }
        if !feather_width.is_finite() {
            return Err(GridError::NonFiniteFeatherWidth);
        }
        if !(0.0..=GRID_MAX_FEATHER).contains(&feather_width) {
            return Err(GridError::FeatherWidthOutOfRange);
        }
        if !feather_height.is_finite() {
            return Err(GridError::NonFiniteFeatherHeight);
        }
        if !(0.0..=GRID_MAX_FEATHER).contains(&feather_height) {
            return Err(GridError::FeatherHeightOutOfRange);
        }
        if !color[0].is_finite() || !color[1].is_finite() || !color[2].is_finite() {
            return Err(GridError::NonFiniteColor);
        }
        if !(0.0..=1.0).contains(&color[0])
            || !(0.0..=1.0).contains(&color[1])
            || !(0.0..=1.0).contains(&color[2])
        {
            return Err(GridError::ColorOutOfRange);
        }
        if !opacity.is_finite() {
            return Err(GridError::NonFiniteOpacity);
        }
        if !(0.0..=GRID_DEFAULT_OPACITY).contains(&opacity) {
            return Err(GridError::OpacityOutOfRange);
        }
        if matches!(blend_mode, BlendMode::Dissolve) {
            return Err(GridError::DissolveUnsupported);
        }
        Ok(Self {
            anchor,
            size_from,
            corner,
            width,
            height,
            border,
            feather_width,
            feather_height,
            invert,
            color,
            opacity: opacity / GRID_DEFAULT_OPACITY,
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
    pub const fn size_from(self) -> GridSizeFrom {
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

    /// The authored border width, in pixels.
    #[must_use]
    pub const fn border(self) -> f32 {
        self.border
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

    /// Whether the lines and cells are inverted.
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

    /// The blend-mode index packed into `header.z`.
    #[must_use]
    pub fn blend_mode_index(self) -> u32 {
        crate::glow::blend_mode_index(self.blend_mode)
    }

    /// The effective cell size in pixels, per axis.
    ///
    /// A coincident `corner` and `anchor` yields a zero cell, which the
    /// degenerate test rejects; the cell is never clamped here so the CPU and
    /// the shader agree on exactly which cells are degenerate.
    #[must_use]
    pub fn cell_size(self) -> [f32; 2] {
        match self.size_from {
            GridSizeFrom::Corner => [
                (self.corner[0] - self.anchor[0]).abs(),
                (self.corner[1] - self.anchor[1]).abs(),
            ],
            GridSizeFrom::WidthSlider => [self.width, self.width],
            GridSizeFrom::WidthHeightSliders => [self.width, self.height],
        }
    }

    /// Whether either cell extent is below [`GRID_DEGENERATE_CELL`].
    #[must_use]
    pub fn has_degenerate_cell(self) -> bool {
        let cell = self.cell_size();
        cell[0] < GRID_DEGENERATE_CELL || cell[1] < GRID_DEGENERATE_CELL
    }

    /// Whether the grade paints nothing at all.
    ///
    /// A degenerate cell is only an identity while the grid is not inverted:
    /// inverted, the empty line coverage becomes a full-frame fill.
    #[must_use]
    pub fn is_identity(self) -> bool {
        if self.opacity <= 1.0e-6 {
            return true;
        }
        self.has_degenerate_cell() && !self.invert
    }

    /// The four uniform cells, in shader order.
    #[must_use]
    pub fn gpu_controls(self) -> GridControls {
        (
            [
                self.anchor[0],
                self.anchor[1],
                self.corner[0],
                self.corner[1],
            ],
            [self.color[0], self.color[1], self.color[2], self.opacity],
            [self.width, self.height, self.border, 0.0],
            [self.feather_width, self.feather_height, 0.0, 0.0],
        )
    }

    /// The single-axis line weight at a distance from the boundary.
    fn axis_coverage(self, distance: f32, feather: f32) -> f32 {
        // A zero-width border draws nothing at all. Without this guard a pixel
        // centre that lands exactly on a cell boundary would still trip the hard
        // `distance <= half` test with `half == 0`, and a feathered grid would
        // paint a half-intensity line along every boundary, because the ramp
        // starts at `half + feather * 0.5`.
        if self.border <= 0.0 {
            return 0.0;
        }
        let half = self.border * 0.5;
        if feather <= 0.0 {
            return if distance <= half { 1.0 } else { 0.0 };
        }
        ((half + feather * 0.5 - distance) / feather).clamp(0.0, 1.0)
    }

    /// The painted coverage at a pixel index, in `0.0..=1.0`.
    #[must_use]
    pub fn coverage(self, column: u32, row: u32) -> f32 {
        if self.has_degenerate_cell() {
            return if self.invert { 1.0 } else { 0.0 };
        }
        // The degenerate cells returned above; the clamp only keeps the
        // division well defined for the cells that remain.
        let cell = self.cell_size().map(|extent| extent.max(GRID_DEGENERATE_CELL));
        let centre_x = column as f32 + 0.5;
        let centre_y = row as f32 + 0.5;
        let offset_x = (centre_x - self.anchor[0]) / cell[0];
        let offset_y = (centre_y - self.anchor[1]) / cell[1];
        let within_x = offset_x - offset_x.floor();
        let within_y = offset_y - offset_y.floor();
        let distance_x = within_x.min(1.0 - within_x) * cell[0];
        let distance_y = within_y.min(1.0 - within_y) * cell[1];
        let base = self
            .axis_coverage(distance_x, self.feather_width)
            .max(self.axis_coverage(distance_y, self.feather_height));
        if self.invert {
            1.0 - base
        } else {
            base
        }
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
pub fn grid_pixel(column: u32, row: u32, grade: &GridGrade) -> [f32; 4] {
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
pub fn grid_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &GridGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_grid_f32_region_controlled(pixels, 0, 0, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies the generator to a region whose origin is given in frame-absolute
/// pixel coordinates.
///
/// # Errors
///
/// Propagates the first error returned by `checkpoint`, leaving already written
/// chunks in place.
pub(crate) fn apply_grid_f32_region_controlled<E>(
    pixels: &mut [[f32; 4]],
    region_x: usize,
    region_y: usize,
    width: usize,
    height: usize,
    grade: &GridGrade,
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
        size_from: GridSizeFrom,
        corner: [f32; 2],
        width: f32,
        height: f32,
        border: f32,
        feather_width: f32,
        feather_height: f32,
        invert: bool,
        color: [f32; 3],
        opacity: f32,
        blend_mode: BlendMode,
    ) -> GridGrade {
        GridGrade::new(
            anchor,
            size_from,
            corner,
            width,
            height,
            border,
            feather_width,
            feather_height,
            invert,
            color,
            opacity,
            blend_mode,
        )
        .expect("the test grade must be valid")
    }

    fn default_grade() -> GridGrade {
        new_grade(
            GRID_DEFAULT_ANCHOR,
            GRID_DEFAULT_SIZE_FROM,
            GRID_DEFAULT_CORNER,
            GRID_DEFAULT_WIDTH,
            GRID_DEFAULT_HEIGHT,
            GRID_DEFAULT_BORDER,
            0.0,
            0.0,
            false,
            [1.0, 1.0, 1.0],
            GRID_DEFAULT_OPACITY,
            BlendMode::Normal,
        )
    }

    fn close(left: f32, right: f32) -> bool {
        (left - right).abs() <= 1.0e-6
    }

    #[test]
    fn grid_defaults_match_the_dump() {
        let grade = default_grade();
        let (params, color, extra0, extra1) = grade.gpu_controls();
        assert_eq!(params, [128.0, 128.0, 153.6, 153.6]);
        assert_eq!(color, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(extra0, [32.0, 24.0, 5.0, 0.0]);
        assert_eq!(extra1, [0.0; 4]);
        assert_eq!(grade.anchor(), GRID_DEFAULT_ANCHOR);
        assert_eq!(grade.size_from(), GridSizeFrom::Corner);
        assert_eq!(grade.size_from().index(), 0);
        assert_eq!(grade.corner(), GRID_DEFAULT_CORNER);
        assert_eq!(grade.width(), GRID_DEFAULT_WIDTH);
        assert_eq!(grade.height(), GRID_DEFAULT_HEIGHT);
        assert_eq!(grade.border(), GRID_DEFAULT_BORDER);
        assert_eq!(grade.feather_width(), 0.0);
        assert_eq!(grade.feather_height(), 0.0);
        assert!(!grade.invert());
        assert_eq!(grade.color(), [1.0, 1.0, 1.0]);
        assert_eq!(grade.opacity(), 1.0);
        assert_eq!(grade.blend_mode(), BlendMode::Normal);
        // 153.6 - 128 in f32 is 25.600006, so the cell is compared with a
        // tolerance rather than bit-exactly.
        assert!((grade.cell_size()[0] - 25.6).abs() <= 1.0e-4);
        assert!((grade.cell_size()[1] - 25.6).abs() <= 1.0e-4);
        assert_eq!(GridSizeFrom::ALL.len(), 3);
        assert_eq!(
            GridSizeFrom::ALL.map(GridSizeFrom::as_key),
            ["corner", "width_slider", "width_height_sliders"]
        );
        assert_eq!(
            GridSizeFrom::from_key("width_height_sliders"),
            Some(GridSizeFrom::WidthHeightSliders)
        );
        assert_eq!(GridSizeFrom::from_key("unknown"), None);
        assert!(!grade.is_identity());
    }

    #[test]
    fn grid_default_frame_matches_the_documented_lines() {
        let grade = default_grade();
        assert_eq!(grid_pixel(128, 128, &grade), [1.0, 1.0, 1.0, 1.0]);
        // (140, 128) is 12.5px from the vertical boundary but only 0.5px from
        // the horizontal one, so it sits on the horizontal line.
        assert_eq!(grid_pixel(140, 128, &grade), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(grid_pixel(140, 140, &grade), [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn grid_hard_edge_stays_binary() {
        let grade = new_grade(
            [0.0, 0.0],
            GridSizeFrom::WidthHeightSliders,
            GRID_DEFAULT_CORNER,
            4.0,
            4.0,
            1.0,
            0.0,
            0.0,
            false,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        let mut pixels = frame(32, 32);
        assert!(grid_frame_f32(&mut pixels, 32, 32, &grade));
        let mut painted = 0;
        let mut empty = 0;
        for (index, pixel) in pixels.iter().enumerate() {
            assert_eq!(pixel[3], pixel[3].round(), "pixel {index} is not binary");
            if pixel[3] == 1.0 {
                painted += 1;
                assert_eq!(*pixel, [1.0, 1.0, 1.0, 1.0]);
            } else {
                empty += 1;
                assert_eq!(*pixel, [0.0, 0.0, 0.0, 0.0]);
            }
        }
        assert!(
            painted > 0 && empty > 0,
            "the frame must show lines and cells"
        );
    }

    #[test]
    fn grid_soft_union_equals_nearest_boundary() {
        let grade = new_grade(
            [3.0, 5.0],
            GridSizeFrom::WidthHeightSliders,
            GRID_DEFAULT_CORNER,
            16.0,
            11.0,
            3.0,
            6.0,
            6.0,
            false,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        let cell = grade.cell_size();
        let half = grade.border() * 0.5;
        let feather = grade.feather_width();
        let mut state = 0x1234_5678_u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        };
        for sample in 0..200 {
            let column = next() % 256;
            let row = next() % 256;
            let offset_x = (column as f32 + 0.5 - grade.anchor()[0]) / cell[0];
            let offset_y = (row as f32 + 0.5 - grade.anchor()[1]) / cell[1];
            let within_x = offset_x - offset_x.floor();
            let within_y = offset_y - offset_y.floor();
            let distance_x = within_x.min(1.0 - within_x) * cell[0];
            let distance_y = within_y.min(1.0 - within_y) * cell[1];
            // The union of two identical-slope ramps is the ramp of the
            // nearest boundary, so both spellings must agree.
            let nearest = distance_x.min(distance_y);
            let expected = ((half + feather * 0.5 - nearest) / feather).clamp(0.0, 1.0);
            let actual = grade.coverage(column, row);
            assert!(
                close(actual, expected),
                "sample {sample} at ({column}, {row}) disagrees: {actual} vs {expected}"
            );
        }
    }

    #[test]
    fn grid_invert_swaps_line_and_cell() {
        let grade = new_grade(
            GRID_DEFAULT_ANCHOR,
            GridSizeFrom::Corner,
            GRID_DEFAULT_CORNER,
            GRID_DEFAULT_WIDTH,
            GRID_DEFAULT_HEIGHT,
            GRID_DEFAULT_BORDER,
            0.0,
            0.0,
            true,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        assert_eq!(grid_pixel(128, 128, &grade), [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(grid_pixel(140, 140, &grade), [1.0, 1.0, 1.0, 1.0]);
        assert!(!grade.is_identity());
    }

    #[test]
    fn grid_border_zero_without_invert_is_empty() {
        let grade = new_grade(
            [0.0, 0.0],
            GridSizeFrom::WidthHeightSliders,
            GRID_DEFAULT_CORNER,
            8.0,
            8.0,
            0.0,
            0.0,
            0.0,
            false,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        let mut pixels = frame(16, 16);
        assert!(grid_frame_f32(&mut pixels, 16, 16, &grade));
        assert!(pixels.iter().all(|pixel| *pixel == [0.0; 4]));
        assert_eq!(grid_pixel(4, 4, &grade), [0.0; 4]);
    }

    #[test]
    fn grid_border_zero_with_invert_is_solid() {
        let grade = new_grade(
            [0.0, 0.0],
            GridSizeFrom::WidthHeightSliders,
            GRID_DEFAULT_CORNER,
            8.0,
            8.0,
            0.0,
            0.0,
            0.0,
            true,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        let mut pixels = frame(16, 16);
        assert!(grid_frame_f32(&mut pixels, 16, 16, &grade));
        assert!(pixels.iter().all(|pixel| *pixel == [1.0, 1.0, 1.0, 1.0]));
    }

    #[test]
    fn grid_border_zero_with_feather_is_empty() {
        // A feathered grid with no border must still draw nothing: the ramp
        // starts at `half + feather * 0.5`, so without an explicit guard every
        // cell boundary would receive a half-intensity line.
        let grade = new_grade(
            GRID_DEFAULT_ANCHOR,
            GridSizeFrom::WidthHeightSliders,
            GRID_DEFAULT_CORNER,
            8.0,
            8.0,
            0.0,
            4.0,
            4.0,
            false,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        let mut pixels = frame(16, 16);
        assert!(grid_frame_f32(&mut pixels, 16, 16, &grade));
        assert!(pixels.iter().all(|pixel| *pixel == [0.0; 4]));
    }

    #[test]
    fn grid_size_from_corner_uses_25_6() {
        let grade = default_grade();
        assert!((grade.cell_size()[0] - 25.6).abs() <= 1.0e-4);
        assert!((grade.cell_size()[1] - 25.6).abs() <= 1.0e-4);
        assert_eq!(grid_pixel(128, 128, &grade), [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(grid_pixel(140, 140, &grade), [0.0, 0.0, 0.0, 0.0]);
        // The width and height sliders do not reach the corner mode.
        let slider = new_grade(
            GRID_DEFAULT_ANCHOR,
            GridSizeFrom::WidthSlider,
            GRID_DEFAULT_CORNER,
            10.0,
            40.0,
            GRID_DEFAULT_BORDER,
            0.0,
            0.0,
            false,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        assert_eq!(slider.cell_size(), [10.0, 10.0]);
    }

    #[test]
    fn grid_degenerate_cell_with_invert_is_solid() {
        let grade = new_grade(
            GRID_DEFAULT_ANCHOR,
            GridSizeFrom::WidthHeightSliders,
            GRID_DEFAULT_CORNER,
            0.0005,
            0.0005,
            GRID_DEFAULT_BORDER,
            0.0,
            0.0,
            true,
            [1.0, 1.0, 1.0],
            100.0,
            BlendMode::Normal,
        );
        assert!(grade.has_degenerate_cell());
        assert!(
            !grade.is_identity(),
            "an inverted degenerate grid still paints"
        );
        assert_eq!(grade.coverage(3, 3), 1.0);
        let mut pixels = frame(8, 8);
        assert!(grid_frame_f32(&mut pixels, 8, 8, &grade));
        assert!(pixels.iter().all(|pixel| *pixel == [1.0, 1.0, 1.0, 1.0]));

        let coincident = new_grade(
            [5.0, 5.0],
            GridSizeFrom::Corner,
            [5.0, 5.0],
            GRID_DEFAULT_WIDTH,
            GRID_DEFAULT_HEIGHT,
            GRID_DEFAULT_BORDER,
            0.0,
            0.0,
            false,
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
    fn grid_zero_opacity_is_identity() {
        let grade = new_grade(
            GRID_DEFAULT_ANCHOR,
            GridSizeFrom::Corner,
            GRID_DEFAULT_CORNER,
            GRID_DEFAULT_WIDTH,
            GRID_DEFAULT_HEIGHT,
            GRID_DEFAULT_BORDER,
            2.0,
            2.0,
            false,
            [1.0, 0.5, 0.25],
            0.0,
            BlendMode::Multiply,
        );
        assert!(grade.is_identity());
        let mut pixels = frame(8, 8);
        assert!(grid_frame_f32(&mut pixels, 8, 8, &grade));
        assert!(pixels.iter().all(|pixel| *pixel == [0.0; 4]));
        assert_eq!(
            grade.apply([0.1, 0.2, 0.3, 0.4], 128, 128),
            [0.1, 0.2, 0.3, 0.4]
        );
    }

    #[test]
    fn grid_region_matches_the_full_frame() {
        let grade = new_grade(
            [96.0, 96.0],
            GridSizeFrom::WidthHeightSliders,
            GRID_DEFAULT_CORNER,
            7.0,
            5.0,
            3.0,
            3.0,
            2.0,
            false,
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
            apply_grid_f32_region_controlled(&mut full, 0, 0, 32, 32, &grade, &mut checkpoint),
            Ok(true)
        ));
        assert!(matches!(
            apply_grid_f32_region_controlled(&mut patch, 8, 8, 16, 16, &grade, &mut checkpoint),
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
    fn grid_checkpoints_once_per_chunk() {
        let grade = default_grade();
        let mut reference = frame(4_096, 2);
        let mut passthrough = || Ok::<(), std::convert::Infallible>(());
        assert!(matches!(
            apply_grid_f32_region_controlled(
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
        // Both rows sit on the horizontal line that crosses every column, so
        // every pixel is painted; this only needs to be a non-trivial reference
        // for the chunk comparison below.
        assert!(
            reference.iter().all(|pixel| *pixel == [1.0, 1.0, 1.0, 1.0]),
            "the reference frame must be painted by the generator"
        );

        let mut pixels = frame(4_096, 2);
        let mut checkpoints = 0usize;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(matches!(
            apply_grid_f32_region_controlled(&mut pixels, 0, 0, 4_096, 2, &grade, &mut checkpoint),
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
        let result =
            apply_grid_f32_region_controlled(&mut aborted, 0, 0, 4_096, 2, &grade, &mut aborting);
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
