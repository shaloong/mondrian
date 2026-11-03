//! Fill generator (GEN-10, `builtin.fill`).
//!
//! A pure generator: it paints the layer with one colour and authors its own
//! coverage from an inset feather ramp. The ramp is the product of two axis
//! ramps measured from the nearest frame border, so a zero feather asks every
//! pixel for full coverage while a positive one fades in from the border.
//!
//! The ramp is measured at the pixel centre, which means the outermost
//! half-pixel ring never reaches full coverage once `feather > 0.5`. That is
//! intentional: moving the ramp inward would make `feather == 0` discontinuous,
//! and the kernel test that pins it (`t14_first_column_never_saturates`) is the
//! evidence the design asked for (design §1.8 risk 6).

use mondrian_core::BlendMode;

/// Largest accepted feather width, in pixels (the dump's 0–999 range).
pub const FILL_MAX_FEATHER: f32 = 999.0;
/// The dump default for both feather axes.
pub const FILL_DEFAULT_FEATHER: f32 = 0.0;
/// The dump default for the fill colour: pure red.
pub const FILL_DEFAULT_COLOR: [f32; 3] = [1.0, 0.0, 0.0];
/// The dump default for the opacity, as a fraction of one.
pub const FILL_DEFAULT_OPACITY: f32 = 1.0;
/// Largest accepted opacity.
pub const FILL_MAX_OPACITY: f32 = 1.0;
/// The dump default for the lazy mask index.
pub const FILL_DEFAULT_FILL_MASK: f32 = 0.0;
/// The dump default for the lazy "all masks" factor.
pub const FILL_DEFAULT_ALL_MASKS: f32 = 0.0;
/// Largest accepted value of the lazy "all masks" factor.
pub const FILL_MAX_ALL_MASKS: f32 = 1.0;

/// Opacities at or below this paint nothing at all.
///
/// The shader compares against the same constant so both sides take the early
/// out on exactly the same inputs.
const FILL_IDENTITY_OPACITY: f32 = 1.0e-6;

/// Pixels written between checkpoint callbacks.
const CHECKPOINT_STRIDE: usize = 4_096;

/// Why a [`FillGrade`] could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillError {
    /// The horizontal feather was not finite.
    NonFiniteFeatherX,
    /// The horizontal feather left the documented range.
    FeatherXOutOfRange,
    /// The vertical feather was not finite.
    NonFiniteFeatherY,
    /// The vertical feather left the documented range.
    FeatherYOutOfRange,
    /// A colour channel was not finite.
    NonFiniteColor,
    /// A colour channel left the documented range.
    ColorOutOfRange,
    /// The opacity was not finite.
    NonFiniteOpacity,
    /// The opacity left the documented range.
    OpacityOutOfRange,
}

impl std::fmt::Display for FillError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFiniteFeatherX => {
                formatter.write_str("fill horizontal feather must be finite")
            }
            Self::FeatherXOutOfRange => {
                formatter.write_str("fill horizontal feather is out of range")
            }
            Self::NonFiniteFeatherY => formatter.write_str("fill vertical feather must be finite"),
            Self::FeatherYOutOfRange => {
                formatter.write_str("fill vertical feather is out of range")
            }
            Self::NonFiniteColor => formatter.write_str("fill colour must be finite"),
            Self::ColorOutOfRange => formatter.write_str("fill colour is out of range"),
            Self::NonFiniteOpacity => formatter.write_str("fill opacity must be finite"),
            Self::OpacityOutOfRange => formatter.write_str("fill opacity is out of range"),
        }
    }
}

impl std::error::Error for FillError {}

/// The two uniform cells, in shader order.
pub type FillControls = ([f32; 4], [f32; 4]);

/// A validated fill generator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FillGrade {
    feather: [f32; 2],
    invert: bool,
    color: [f32; 3],
    opacity: f32,
}

impl FillGrade {
    /// Validates and builds a grade.
    ///
    /// The lazy `fill_mask` / `all_masks` parameters are deliberately absent:
    /// the engine has no mask contract yet, so they are authoring hints the
    /// graph builder reads and drops. They never reach this constructor, the
    /// uniform, or the effect cache key.
    ///
    /// # Errors
    ///
    /// Returns [`FillError`] when a feather, the colour, or the opacity is
    /// non-finite or leaves its documented range.
    pub fn new(
        feather_x: f32,
        feather_y: f32,
        invert: bool,
        color: [f32; 3],
        opacity: f32,
    ) -> Result<Self, FillError> {
        if !feather_x.is_finite() {
            return Err(FillError::NonFiniteFeatherX);
        }
        if !(0.0..=FILL_MAX_FEATHER).contains(&feather_x) {
            return Err(FillError::FeatherXOutOfRange);
        }
        if !feather_y.is_finite() {
            return Err(FillError::NonFiniteFeatherY);
        }
        if !(0.0..=FILL_MAX_FEATHER).contains(&feather_y) {
            return Err(FillError::FeatherYOutOfRange);
        }
        if !color[0].is_finite() || !color[1].is_finite() || !color[2].is_finite() {
            return Err(FillError::NonFiniteColor);
        }
        if !(0.0..=1.0).contains(&color[0])
            || !(0.0..=1.0).contains(&color[1])
            || !(0.0..=1.0).contains(&color[2])
        {
            return Err(FillError::ColorOutOfRange);
        }
        if !opacity.is_finite() {
            return Err(FillError::NonFiniteOpacity);
        }
        if !(0.0..=FILL_MAX_OPACITY).contains(&opacity) {
            return Err(FillError::OpacityOutOfRange);
        }
        Ok(Self {
            feather: [feather_x, feather_y],
            invert,
            color,
            opacity,
        })
    }

    /// The horizontal feather width, in pixels. Zero is a hard edge.
    #[must_use]
    pub const fn feather_x(self) -> f32 {
        self.feather[0]
    }

    /// The vertical feather width, in pixels. Zero is a hard edge.
    #[must_use]
    pub const fn feather_y(self) -> f32 {
        self.feather[1]
    }

    /// Whether the coverage is complemented before it is painted.
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

    /// Whether the grade paints nothing at all.
    ///
    /// A zero opacity cannot paint anything. Inverting a hard-edged fill is the
    /// other identity: both axis ramps take the constant `1.0` branch, so the
    /// coverage is exactly `1.0` and its complement is exactly `0.0` for every
    /// pixel. A zero feather without invert is *not* an identity — it paints
    /// the whole frame at full coverage.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.opacity <= FILL_IDENTITY_OPACITY
            || (self.invert && self.feather[0] <= 0.0 && self.feather[1] <= 0.0)
    }

    /// The two uniform cells, in shader order.
    #[must_use]
    pub fn gpu_controls(self) -> FillControls {
        (
            [self.feather[0], self.feather[1], 0.0, 0.0],
            [self.color[0], self.color[1], self.color[2], self.opacity],
        )
    }

    /// One axis of the inset ramp.
    ///
    /// A zero (or negative) feather takes the constant branch: it is not a
    /// degenerate division, it asks for full coverage.
    fn axis(inset: f32, feather: f32) -> f32 {
        if feather <= 0.0 {
            1.0
        } else {
            (inset / feather).clamp(0.0, 1.0)
        }
    }

    /// The authored coverage at a pixel index, in `0.0..=1.0`.
    ///
    /// `column` and `row` are frame-absolute pixel indices; `frame_width` and
    /// `frame_height` are the source extent, which is what the shader reads
    /// from `uniforms.geometry.zw`.
    #[must_use]
    pub fn coverage(self, column: u32, row: u32, frame_width: u32, frame_height: u32) -> f32 {
        let centre_x = column as f32 + 0.5;
        let centre_y = row as f32 + 0.5;
        let extent_x = frame_width as f32;
        let extent_y = frame_height as f32;
        // `min(cx, w - cx)` is already symmetric, so the ramp mirrors without an
        // `abs`. A coordinate outside the frame gives a negative inset, which
        // clamps to a zero ramp instead of wrapping.
        let inset_x = centre_x.min(extent_x - centre_x);
        let inset_y = centre_y.min(extent_y - centre_y);
        let mut coverage =
            Self::axis(inset_x, self.feather[0]) * Self::axis(inset_y, self.feather[1]);
        if self.invert {
            coverage = 1.0 - coverage;
        }
        coverage
    }

    /// Blends the generated colour over `pixel` at a pixel index.
    #[must_use]
    pub fn apply(
        self,
        pixel: [f32; 4],
        column: u32,
        row: u32,
        frame_width: u32,
        frame_height: u32,
    ) -> [f32; 4] {
        if self.is_identity() {
            return pixel;
        }
        let coverage = self.coverage(column, row, frame_width, frame_height);
        let alpha = coverage * self.opacity.clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return pixel;
        }
        crate::adjustment::blend_rgba_f32_pixel_seeded(
            pixel,
            [self.color[0], self.color[1], self.color[2], alpha],
            1.0,
            BlendMode::Normal,
            0,
        )
    }
}

/// Applies the generator to a whole frame.
///
/// Returns `false` when `width * height` overflows, the buffer is too short, or
/// the extent does not fit the frame coordinates.
#[must_use]
pub fn fill_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &FillGrade,
) -> bool {
    let (Ok(frame_width), Ok(frame_height)) = (u32::try_from(width), u32::try_from(height)) else {
        return false;
    };
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_fill_f32_region_controlled(
        pixels,
        0,
        0,
        width,
        height,
        frame_width,
        frame_height,
        grade,
        &mut checkpoint,
    )
    .unwrap_or(false)
}

/// Applies the generator to a region whose origin is given in frame-absolute
/// pixel coordinates.
///
/// `width` / `height` are the region size; `frame_width` / `frame_height` are
/// the source extent the feather ramp is measured against, so a ROI crop keeps
/// the same coordinates as the fused point shader.
///
/// Returns `false` when the region cannot address the buffer or the frame.
///
/// # Errors
///
/// Propagates the first error returned by `checkpoint`, leaving already written
/// chunks in place.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_fill_f32_region_controlled<E>(
    pixels: &mut [[f32; 4]],
    region_x: usize,
    region_y: usize,
    width: usize,
    height: usize,
    frame_width: u32,
    frame_height: u32,
    grade: &FillGrade,
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
                        *pixel = grade.apply(*pixel, column, row, frame_width, frame_height);
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

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.0; 4]; width * height]
    }

    fn grade(
        feather_x: f32,
        feather_y: f32,
        invert: bool,
        color: [f32; 3],
        opacity: f32,
    ) -> FillGrade {
        FillGrade::new(feather_x, feather_y, invert, color, opacity)
            .expect("test grade should validate")
    }

    fn render(width: usize, height: usize, grade: &FillGrade, base: &[[f32; 4]]) -> Vec<[f32; 4]> {
        let mut pixels = base.to_vec();
        assert!(fill_frame_f32(&mut pixels, width, height, grade));
        pixels
    }

    fn assert_bitwise_equal(left: &[[f32; 4]], right: &[[f32; 4]]) {
        assert_eq!(left.len(), right.len());
        for (index, (left, right)) in left.iter().zip(right).enumerate() {
            for channel in 0..4 {
                assert_eq!(
                    left[channel].to_bits(),
                    right[channel].to_bits(),
                    "pixel {index} channel {channel}"
                );
            }
        }
    }

    #[test]
    fn t2_painted_values_are_exact() {
        // Design §1.1.5: 4x3, feather_x 2, feather_y 0, opacity 0.5, blue.
        let grade = grade(2.0, 0.0, false, [0.0, 0.0, 1.0], 0.5);
        let pixels = render(4, 3, &grade, &frame(4, 3));
        let alpha = [0.125f32, 0.375, 0.375, 0.125];
        for row in 0..3 {
            for column in 0..4 {
                let pixel = pixels[row * 4 + column];
                assert_eq!(pixel[0].to_bits(), 0.0f32.to_bits());
                assert_eq!(pixel[1].to_bits(), 0.0f32.to_bits());
                assert_eq!(pixel[2].to_bits(), 1.0f32.to_bits());
                assert_eq!(
                    pixel[3].to_bits(),
                    alpha[column].to_bits(),
                    "column {column} of row {row}"
                );
            }
        }
    }

    #[test]
    fn t3_horizontal_axis_is_symmetric() {
        let grade = grade(4.0, 0.0, false, [0.0, 0.0, 1.0], 1.0);
        let pixels = render(8, 1, &grade, &frame(8, 1));
        let expected = [0.125f32, 0.375, 0.625, 0.875, 0.875, 0.625, 0.375, 0.125];
        for (column, expected) in expected.iter().enumerate() {
            assert_eq!(
                pixels[column][3].to_bits(),
                expected.to_bits(),
                "column {column}"
            );
        }
        for column in 0..4 {
            assert_eq!(pixels[column], pixels[7 - column], "mirror at {column}");
        }
    }

    #[test]
    fn t4_hard_edge_is_full_coverage() {
        // A zero feather takes the constant branch, so the coverage is exactly
        // 1.0 for every pixel and the straight-alpha over forces the alpha to 1
        // even on a fully transparent base.
        let grade = grade(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0);
        for column in 0..2 {
            for row in 0..2 {
                assert_eq!(grade.coverage(column, row, 2, 2), 1.0);
            }
        }
        let pixels = render(2, 2, &grade, &frame(2, 2));
        for pixel in &pixels {
            assert_eq!(*pixel, [1.0, 0.0, 0.0, 1.0]);
        }
    }

    #[test]
    fn t5_invert_is_exact_complement() {
        let normal = grade(4.0, 0.0, false, [0.0, 1.0, 0.0], 1.0);
        let inverted = grade(4.0, 0.0, true, [0.0, 1.0, 0.0], 1.0);
        let base = frame(8, 1);
        let plain = render(8, 1, &normal, &base);
        let complement = render(8, 1, &inverted, &base);
        for column in 0..8 {
            let alpha = plain[column][3];
            assert_eq!(
                complement[column][3].to_bits(),
                (1.0 - alpha).to_bits(),
                "column {column}"
            );
        }
    }

    #[test]
    fn t6_vertical_axis() {
        let grade = grade(0.0, 2.0, false, [0.0, 0.0, 1.0], 1.0);
        let pixels = render(3, 4, &grade, &frame(3, 4));
        let expected = [0.25f32, 0.75, 0.75, 0.25];
        for (row, expected) in expected.iter().enumerate() {
            for column in 0..3 {
                assert_eq!(
                    pixels[row * 3 + column][3].to_bits(),
                    expected.to_bits(),
                    "row {row} column {column}"
                );
            }
        }
    }

    #[test]
    fn t7_product_of_axes() {
        let grade = grade(2.0, 2.0, false, [0.0, 0.0, 1.0], 1.0);
        let pixels = render(4, 4, &grade, &frame(4, 4));
        let alpha = |column: usize, row: usize| pixels[row * 4 + column][3];
        assert_eq!(alpha(0, 0).to_bits(), 0.0625f32.to_bits());
        assert_eq!(alpha(3, 0).to_bits(), 0.0625f32.to_bits());
        assert_eq!(alpha(0, 1).to_bits(), 0.1875f32.to_bits());
        assert_eq!(alpha(1, 3).to_bits(), 0.1875f32.to_bits());
        assert_eq!(alpha(1, 1).to_bits(), 0.5625f32.to_bits());
        // A fill is not a partition: the coverage is the product of the axes, so
        // the four-by-four sum is (0.25 + 0.75 + 0.75 + 0.25)^2 = 4.0.
        let total: f32 = pixels.iter().map(|pixel| pixel[3]).sum();
        assert_eq!(total, 4.0);
    }

    #[test]
    fn t8_transparent_base_gains_coverage() {
        // The CPU mirror of the shader's coverage registration: nothing in the
        // base frame can contribute, so the generator must paint on its own.
        let base = frame(2, 2);
        let pixels = render(2, 2, &grade(0.0, 0.0, false, [1.0, 0.0, 0.0], 1.0), &base);
        for pixel in &pixels {
            assert_eq!(*pixel, [1.0, 0.0, 0.0, 1.0]);
        }
        let invisible = render(2, 2, &grade(0.0, 0.0, false, [1.0, 0.0, 0.0], 0.0), &base);
        assert_bitwise_equal(&invisible, &base);
    }

    #[test]
    fn t9_identity_by_opacity() {
        let epsilon = grade(3.0, 3.0, false, [0.2, 0.4, 0.6], 1.0e-6);
        assert!(epsilon.is_identity());
        let base = frame(4, 4);
        assert_bitwise_equal(&render(4, 4, &epsilon, &base), &base);

        // The next representable opacity is already past the threshold, so the
        // identity cannot swallow a slider that is merely almost zero.
        let next = f32::from_bits(1.0e-6f32.to_bits() + 1);
        assert!(next > 1.0e-6);
        let visible = grade(3.0, 3.0, false, [0.2, 0.4, 0.6], next);
        assert!(!visible.is_identity());
        let pixels = render(4, 4, &visible, &base);
        assert!(
            pixels.iter().any(|pixel| pixel[3] > 0.0),
            "the first opacity past the threshold must paint"
        );
    }

    #[test]
    fn t10_identity_by_invert_hard_edge() {
        // Both axes take the constant 1.0 branch, so the complement is exactly
        // 0.0 for every pixel: the frame must come back bitwise unchanged.
        let grade = grade(0.0, 0.0, true, [1.0, 0.0, 0.0], 1.0);
        assert!(grade.is_identity());
        assert_eq!(grade.coverage(3, 7, 8, 16), 0.0);
        let base = vec![
            [0.1, 0.2, 0.3, 0.4],
            [0.5, 0.6, 0.7, 0.8],
            [0.0; 4],
            [1.0; 4],
        ];
        assert_bitwise_equal(&render(2, 2, &grade, &base), &base);
    }

    #[test]
    fn t11_region_offsets_are_frame_absolute() {
        let grade = grade(3.0, 2.0, false, [0.0, 0.0, 1.0], 1.0);
        let mut full = frame(6, 5);
        assert!(fill_frame_f32(&mut full, 6, 5, &grade));

        let mut region = frame(3, 2);
        let mut checkpoint = || Ok::<(), Infallible>(());
        assert!(apply_fill_f32_region_controlled(
            &mut region,
            2,
            1,
            3,
            2,
            6,
            5,
            &grade,
            &mut checkpoint,
        )
        .expect("the region renders"));
        for row in 0..2 {
            for column in 0..3 {
                let local = region[row * 3 + column];
                let absolute = full[(row + 1) * 6 + column + 2];
                assert_eq!(
                    local[3].to_bits(),
                    absolute[3].to_bits(),
                    "({column}, {row})"
                );
            }
        }

        // Passing the region extent as the frame extent would re-centre the
        // ramp, which is exactly the bug the frame-absolute signature prevents.
        let mut local_frame = frame(3, 2);
        assert!(apply_fill_f32_region_controlled(
            &mut local_frame,
            2,
            1,
            3,
            2,
            3,
            2,
            &grade,
            &mut checkpoint,
        )
        .expect("the mis-scaled region still renders"));
        assert_ne!(local_frame[0][3].to_bits(), region[0][3].to_bits());
    }

    #[test]
    fn t12_checkpoint_stride_writes_in_chunks() {
        struct Aborted;

        let grade = grade(4.0, 4.0, false, [1.0, 0.0, 0.0], 1.0);

        // 8 wide / 4096 stride = 512 rows per chunk, so two chunks plus the
        // closing checkpoint.
        let mut pixels = frame(8, 1024);
        let mut calls = 0;
        let mut counter = || {
            calls += 1;
            Ok::<(), Infallible>(())
        };
        assert!(apply_fill_f32_region_controlled(
            &mut pixels,
            0,
            0,
            8,
            1024,
            8,
            1024,
            &grade,
            &mut counter,
        )
        .expect("the frame renders"));
        assert_eq!(calls, 3);

        // The second checkpoint guards row 512, so aborting there must leave the
        // first chunk written and the second one untouched.
        let mut pixels = frame(8, 1024);
        let mut calls = 0;
        let mut abort = || {
            calls += 1;
            if calls == 2 {
                Err(Aborted)
            } else {
                Ok(())
            }
        };
        let result = apply_fill_f32_region_controlled(
            &mut pixels,
            0,
            0,
            8,
            1024,
            8,
            1024,
            &grade,
            &mut abort,
        );
        assert!(result.is_err());
        assert_eq!(calls, 2);
        assert_ne!(pixels[0][3].to_bits(), 0.0f32.to_bits());
        assert_ne!(pixels[511 * 8][3].to_bits(), 0.0f32.to_bits());
        assert_eq!(pixels[512 * 8][3].to_bits(), 0.0f32.to_bits());
        assert_eq!(pixels[1023 * 8][3].to_bits(), 0.0f32.to_bits());
    }

    #[test]
    fn t14_first_column_never_saturates() {
        // Feather is the distance to the frame border, so a one-pixel-wide frame
        // caps every pixel centre at inset 0.5: the whole column is half
        // coverage, never full. Design §1.8 risk 6 pins this on purpose.
        let grade = grade(1.0, 0.0, false, [1.0, 0.0, 0.0], 1.0);
        let pixels = render(1, 64, &grade, &frame(1, 64));
        for (row, pixel) in pixels.iter().enumerate() {
            assert_eq!(pixel[3].to_bits(), 0.5f32.to_bits(), "row {row}");
        }
    }
}
