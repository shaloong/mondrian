//! Corner pin: pins the frame onto an authored quadrilateral.
//!
//! The corners are normalized frame coordinates: `(0, 0)` is the centre of the
//! source top-left pixel and `(1, 1)` the centre of the source bottom-right
//! pixel, so pixel `(i, j)` sits at `(i / (width - 1), j / (height - 1))`. The
//! same space describes the destination quad and the defaults — top-left
//! `(0, 0)`, top-right `(1, 0)`, bottom-left `(0, 1)`, bottom-right `(1, 1)` —
//! are exactly the identity at every frame size.
//!
//! Four point correspondences define one projective homography: a full 8-DOF
//! direct linear transform, not a bilinear map, so every authored corner is hit
//! exactly. Sampling runs backwards. A destination pixel centre `p` becomes
//! `d = (p.x / (width - 1), p.y / (height - 1))`, the normalized source point is
//! `s = H⁻¹ d` after the homogeneous divide, and the source sample is
//! `(s.x (width - 1), s.y (height - 1))`. Destination pixels whose source point
//! falls outside the unit square (up to a tiny epsilon) are fully transparent,
//! which is what pins the picture onto the quad instead of smearing its border.
//!
//! The source is read bilinearly in premultiplied space so coverage and colour
//! are filtered together. A degenerate quad, or an empty frame, passes the frame
//! through untouched instead of producing undefined samples.

use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest normalized corner coordinate the author state may declare.
pub const CORNER_PIN_MAX_COORDINATE: f32 = 10.0;
/// Smallest normalized corner coordinate the author state may declare.
pub const CORNER_PIN_MIN_COORDINATE: f32 = -10.0;
/// Number of authored quad corners.
pub const CORNER_PIN_CORNER_COUNT: usize = 4;

/// A corner-pin corner outside its declared range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerPinError {
    /// The top-left corner is not finite or exceeds the coordinate limit.
    TopLeft,
    /// The top-right corner is not finite or exceeds the coordinate limit.
    TopRight,
    /// The bottom-left corner is not finite or exceeds the coordinate limit.
    BottomLeft,
    /// The bottom-right corner is not finite or exceeds the coordinate limit.
    BottomRight,
}

impl std::fmt::Display for CornerPinError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::TopLeft => "corner pin top left must be a finite point inside [-10, 10]",
            Self::TopRight => "corner pin top right must be a finite point inside [-10, 10]",
            Self::BottomLeft => "corner pin bottom left must be a finite point inside [-10, 10]",
            Self::BottomRight => "corner pin bottom right must be a finite point inside [-10, 10]",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CornerPinError {}

/// A validated corner pin: the four authored quad corners.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CornerPinGrade {
    top_left: [f32; 2],
    top_right: [f32; 2],
    bottom_left: [f32; 2],
    bottom_right: [f32; 2],
}

impl CornerPinGrade {
    /// Validates and builds a corner pin.
    pub fn new(
        top_left: [f32; 2],
        top_right: [f32; 2],
        bottom_left: [f32; 2],
        bottom_right: [f32; 2],
    ) -> Result<Self, CornerPinError> {
        let grade = Self { top_left, top_right, bottom_left, bottom_right };
        grade.validate()?;
        Ok(grade)
    }

    fn validate(&self) -> Result<(), CornerPinError> {
        validate_corner(self.top_left, CornerPinError::TopLeft)?;
        validate_corner(self.top_right, CornerPinError::TopRight)?;
        validate_corner(self.bottom_left, CornerPinError::BottomLeft)?;
        validate_corner(self.bottom_right, CornerPinError::BottomRight)
    }

    /// Authored top-left destination corner.
    pub const fn top_left(&self) -> [f32; 2] {
        self.top_left
    }

    /// Authored top-right destination corner.
    pub const fn top_right(&self) -> [f32; 2] {
        self.top_right
    }

    /// Authored bottom-left destination corner.
    pub const fn bottom_left(&self) -> [f32; 2] {
        self.bottom_left
    }

    /// Authored bottom-right destination corner.
    pub const fn bottom_right(&self) -> [f32; 2] {
        self.bottom_right
    }

    /// True when the quad is the exact unit square.
    pub fn is_identity(&self) -> bool {
        self.top_left == [0.0, 0.0]
            && self.top_right == [1.0, 0.0]
            && self.bottom_left == [0.0, 1.0]
            && self.bottom_right == [1.0, 1.0]
    }

    /// The four corners as top-left, top-right, bottom-left, bottom-right.
    pub const fn corners(&self) -> [[f32; 2]; 4] {
        [
            self.top_left,
            self.top_right,
            self.bottom_left,
            self.bottom_right,
        ]
    }

    /// The normalized-space homography mapping the unit source square onto the
    /// authored quad, row-major with `h22 == 1`.
    ///
    /// `None` means the four corners do not define a homography (a collapsed or
    /// otherwise singular quad).
    ///
    /// The mapping is normalized, so the coefficients do not depend on the frame
    /// size. The size stays in the signature because the kernel and the renderer
    /// always have it at hand and pass it through unchanged.
    pub fn homography(&self, width: u32, height: u32) -> Option<[f64; 9]> {
        let _ = (width, height);
        solve_homography([
            [f64::from(self.top_left[0]), f64::from(self.top_left[1])],
            [f64::from(self.top_right[0]), f64::from(self.top_right[1])],
            [
                f64::from(self.bottom_left[0]),
                f64::from(self.bottom_left[1]),
            ],
            [
                f64::from(self.bottom_right[0]),
                f64::from(self.bottom_right[1]),
            ],
        ])
    }

    /// The inverse of [`Self::homography`], normalized so its `i22 == 1`.
    ///
    /// `None` means the quad is degenerate: a singular DLT, a non-finite
    /// coefficient, a determinant under `1e-12` or an `i22` under `1e-12`.
    pub fn inverse_homography(&self, width: u32, height: u32) -> Option<[f64; 9]> {
        let homography = self.homography(width, height)?;
        invert_homography(&homography)
    }

    /// Packs the inverse for the GPU spatial pass.
    ///
    /// `params = [i00, i01, i02, i10]` and `extra = [i11, i12, i20, i21]`; the
    /// matrix is in normalized frame coordinates with `i22 == 1` implied. `None`
    /// means the quad is degenerate and the renderer should pass the frame
    /// through.
    pub fn gpu_controls(&self, width: u32, height: u32) -> Option<([f32; 4], [f32; 4])> {
        if width < 2 || height < 2 {
            // The CPU kernel passes frames without a pixel pair through
            // untouched, so the renderer must hand the source back as well.
            return None;
        }
        let [i00, i01, i02, i10, i11, i12, i20, i21, _i22] =
            self.inverse_homography(width, height)?;
        Some((
            [i00 as f32, i01 as f32, i02 as f32, i10 as f32],
            [i11 as f32, i12 as f32, i20 as f32, i21 as f32],
        ))
    }
}

/// True when `value` is finite and inside the declared coordinate range.
fn within_coordinate(value: f32) -> bool {
    value.is_finite() && (CORNER_PIN_MIN_COORDINATE..=CORNER_PIN_MAX_COORDINATE).contains(&value)
}

fn validate_corner(corner: [f32; 2], error: CornerPinError) -> Result<(), CornerPinError> {
    if corner.iter().all(|value| within_coordinate(*value)) {
        Ok(())
    } else {
        Err(error)
    }
}

/// The normalized source corners, in the same order as [`CornerPinGrade::corners`].
const SOURCE_CORNERS: [[f64; 2]; CORNER_PIN_CORNER_COUNT] =
    [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];

/// Solves the 8-DOF DLT for four destination corners, with `h22 = 1`.
fn solve_homography(corners: [[f64; 2]; CORNER_PIN_CORNER_COUNT]) -> Option<[f64; 9]> {
    const SIZE: usize = 8;
    let mut system = [[0.0_f64; 9]; SIZE];
    for (index, (source, destination)) in SOURCE_CORNERS.iter().zip(corners).enumerate() {
        let [x, y] = *source;
        let [u, v] = destination;
        system[2 * index] = [x, y, 1.0, 0.0, 0.0, 0.0, -x * u, -y * u, u];
        system[2 * index + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -x * v, -y * v, v];
    }
    let solution = solve_linear_system(&mut system)?;
    let homography = [
        solution[0],
        solution[1],
        solution[2],
        solution[3],
        solution[4],
        solution[5],
        solution[6],
        solution[7],
        1.0,
    ];
    homography.iter().all(|value| value.is_finite()).then_some(homography)
}

/// Gaussian elimination with partial pivoting and back substitution.
fn solve_linear_system(system: &mut [[f64; 9]; 8]) -> Option<[f64; 8]> {
    const SIZE: usize = 8;
    for column in 0..SIZE {
        let mut pivot = column;
        let mut largest = system[column][column].abs();
        for (row, entries) in system.iter().enumerate().skip(column + 1) {
            let magnitude = entries[column].abs();
            if magnitude > largest {
                largest = magnitude;
                pivot = row;
            }
        }
        if largest <= 1e-12 {
            return None;
        }
        system.swap(column, pivot);
        for row in (column + 1)..SIZE {
            let factor = system[row][column] / system[column][column];
            if factor == 0.0 {
                continue;
            }
            let (pivot_rows, target_rows) = system.split_at_mut(row);
            let pivot_row = &pivot_rows[column];
            let target = &mut target_rows[0];
            for (value, pivot_value) in
                target[column..=SIZE].iter_mut().zip(&pivot_row[column..=SIZE])
            {
                *value -= factor * pivot_value;
            }
        }
    }

    let mut solution = [0.0_f64; SIZE];
    for row in (0..SIZE).rev() {
        let mut value = system[row][SIZE];
        for (column, coefficient) in solution.iter().enumerate().skip(row + 1) {
            value -= system[row][column] * coefficient;
        }
        solution[row] = value / system[row][row];
    }
    Some(solution)
}

/// Inverts a normalized homography by the adjugate and normalizes `i22` to one.
fn invert_homography(homography: &[f64; 9]) -> Option<[f64; 9]> {
    let [h00, h01, h02, h10, h11, h12, h20, h21, h22] = *homography;
    let determinant = h00 * (h11 * h22 - h12 * h21) - h01 * (h10 * h22 - h12 * h20)
        + h02 * (h10 * h21 - h11 * h20);
    if !determinant.is_finite() || determinant.abs() <= 1e-12 {
        return None;
    }
    let scale = 1.0 / determinant;
    let inverse = [
        (h11 * h22 - h12 * h21) * scale,
        (h02 * h21 - h01 * h22) * scale,
        (h01 * h12 - h02 * h11) * scale,
        (h12 * h20 - h10 * h22) * scale,
        (h00 * h22 - h02 * h20) * scale,
        (h02 * h10 - h00 * h12) * scale,
        (h10 * h21 - h11 * h20) * scale,
        (h01 * h20 - h00 * h21) * scale,
        (h00 * h11 - h01 * h10) * scale,
    ];
    if !inverse.iter().all(|value| value.is_finite()) {
        return None;
    }
    let i22 = inverse[8];
    if i22.abs() < 1e-12 {
        return None;
    }
    let normalization = 1.0 / i22;
    let mut normalized = [0.0_f64; 9];
    for (slot, value) in normalized.iter_mut().zip(inverse) {
        *slot = value * normalization;
    }
    Some(normalized)
}

/// Applies the corner pin to a whole frame, returning `false` when the frame
/// cannot be processed.
pub fn corner_pin_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &CornerPinGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_corner_pin_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the corner pin in place with an explicit cancellation checkpoint.
pub(crate) fn apply_corner_pin_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &CornerPinGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if width < 2 || height < 2 {
        // Normalized coordinates need at least two samples per axis, so a
        // one-pixel frame has no defined quad and passes through.
        return Ok(true);
    }
    let Some(inverse) = grade.inverse_homography(width as u32, height as u32) else {
        // A degenerate quad has no inverse to sample through, so the frame
        // passes through untouched rather than producing undefined samples.
        return Ok(true);
    };

    let frame = &mut working[..pixel_count];
    premultiply_rgba_controlled(frame, checkpoint)?;

    let source = frame.to_vec();
    let [i00, i01, i02, i10, i11, i12, i20, i21, i22] = inverse;
    let maximum_x = (width - 1) as f64;
    let maximum_y = (height - 1) as f64;
    let aspect = maximum_x / maximum_y;
    let inverse_aspect = maximum_y / maximum_x;

    checkpoint()?;
    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let normalized_x = column as f64 / maximum_x;
                let normalized_y = row as f64 / maximum_y;
                let denominator = i20 * normalized_x + i21 * normalized_y + i22;
                if !denominator.is_finite() || denominator.abs() <= 1e-12 {
                    *pixel = [0.0; 4];
                    continue;
                }
                // Denormalise inside the numerator: scaling the normalised
                // sample back up would round the position twice, and an exact
                // texel hit has to stay exact or a fully transparent pixel
                // mixes in a sliver of its neighbour.
                let sample_x = (i00 * column as f64 + i01 * row as f64 * aspect + i02 * maximum_x)
                    / denominator;
                let sample_y =
                    (i10 * column as f64 * inverse_aspect + i11 * row as f64 + i12 * maximum_y)
                        / denominator;
                let normalized_source_x = sample_x / maximum_x;
                let normalized_source_y = sample_y / maximum_y;
                let inside = -1e-6..=1.0 + 1e-6;
                if !normalized_source_x.is_finite()
                    || !normalized_source_y.is_finite()
                    || !inside.contains(&normalized_source_x)
                    || !inside.contains(&normalized_source_y)
                {
                    *pixel = [0.0; 4];
                    continue;
                }
                let texel = sample_bilinear_clamped(&source, width, height, sample_x, sample_y);
                let mut accumulated = [0.0_f64; 4];
                for (channel, value) in accumulated.iter_mut().enumerate() {
                    *value += f64::from(texel[channel]);
                }
                *pixel = accumulated.map(|channel| channel as f32);
            }
        }
    });

    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{corner_pin_frame_f32, CornerPinError, CornerPinGrade};

    const IDENTITY_MATRIX: [f64; 9] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

    fn grade(
        top_left: [f32; 2],
        top_right: [f32; 2],
        bottom_left: [f32; 2],
        bottom_right: [f32; 2],
    ) -> CornerPinGrade {
        CornerPinGrade::new(top_left, top_right, bottom_left, bottom_right)
            .expect("the test corners should validate")
    }

    fn identity() -> CornerPinGrade {
        grade([0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0])
    }

    fn gradient(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                let value = 0.01 * (column + 3 * row) as f32;
                pixels.push([value, 0.5 - 0.37 * value, 0.15 + 0.8 * value, 1.0]);
            }
        }
        pixels
    }

    fn at(pixels: &[[f32; 4]], width: usize, column: usize, row: usize) -> [f32; 4] {
        pixels[row * width + column]
    }

    fn assert_pixel(actual: [f32; 4], expected: [f32; 4], label: &str) {
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= 1e-5,
                "{label} channel {channel}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    fn multiply(left: &[f64; 9], right: &[f64; 9]) -> [f64; 9] {
        let mut product = [0.0_f64; 9];
        for row in 0..3 {
            for column in 0..3 {
                let mut sum = 0.0_f64;
                for index in 0..3 {
                    sum += left[row * 3 + index] * right[index * 3 + column];
                }
                product[row * 3 + column] = sum;
            }
        }
        product
    }

    #[test]
    fn identity_round_trip_is_bit_identical() {
        let (width, height) = (9, 7);
        let pixels = gradient(width, height);
        let mut frame = pixels.clone();
        assert!(corner_pin_frame_f32(&mut frame, width, height, &identity()));
        assert_eq!(frame, pixels);
        assert_eq!(
            identity().homography(width as u32, height as u32),
            Some(IDENTITY_MATRIX)
        );
    }

    #[test]
    fn translated_quad_shifts_the_frame() {
        let (width, height) = (9, 7);
        let pixels = gradient(width, height);
        let mut frame = pixels.clone();
        let shifted = grade([0.25, 0.5], [1.25, 0.5], [0.25, 1.5], [1.25, 1.5]);
        assert!(corner_pin_frame_f32(&mut frame, width, height, &shifted));
        assert_pixel(
            at(&frame, width, 2, 3),
            at(&pixels, width, 0, 0),
            "shifted origin",
        );
        assert_pixel(
            at(&frame, width, 5, 4),
            at(&pixels, width, 3, 1),
            "shifted interior",
        );
        assert_pixel(
            at(&frame, width, 8, 6),
            at(&pixels, width, 6, 3),
            "shifted far corner",
        );
        assert_pixel(at(&frame, width, 0, 0), [0.0; 4], "uncovered top left");
        assert_pixel(at(&frame, width, 1, 0), [0.0; 4], "uncovered top edge");
    }

    #[test]
    fn doubled_quad_zooms_into_the_top_left_quadrant() {
        let (width, height) = (9, 7);
        let pixels = gradient(width, height);
        let mut frame = pixels.clone();
        let doubled = grade([0.0, 0.0], [2.0, 0.0], [0.0, 2.0], [2.0, 2.0]);
        assert!(corner_pin_frame_f32(&mut frame, width, height, &doubled));
        assert_pixel(
            at(&frame, width, 0, 0),
            at(&pixels, width, 0, 0),
            "source origin",
        );
        assert_pixel(
            at(&frame, width, 4, 2),
            at(&pixels, width, 2, 1),
            "source middle",
        );
        assert_pixel(
            at(&frame, width, 8, 6),
            at(&pixels, width, 4, 3),
            "source quadrant corner",
        );
    }

    #[test]
    fn mirrored_quad_flips_horizontally() {
        let (width, height) = (9, 7);
        let pixels = gradient(width, height);
        let mut frame = pixels.clone();
        let mirrored = grade([1.0, 0.0], [0.0, 0.0], [1.0, 1.0], [0.0, 1.0]);
        assert!(corner_pin_frame_f32(&mut frame, width, height, &mirrored));
        assert_pixel(
            at(&frame, width, 0, 0),
            at(&pixels, width, 8, 0),
            "flipped left edge",
        );
        assert_pixel(
            at(&frame, width, 2, 3),
            at(&pixels, width, 6, 3),
            "flipped interior",
        );
        assert_pixel(
            at(&frame, width, 8, 6),
            at(&pixels, width, 0, 6),
            "flipped right edge",
        );
    }

    #[test]
    fn authored_corners_sample_their_source_corners() {
        let (width, height) = (9, 9);
        let pixels = gradient(width, height);
        let mut frame = pixels.clone();
        let quad = grade([0.25, 0.25], [0.75, 0.25], [0.25, 0.75], [0.75, 0.75]);
        assert!(corner_pin_frame_f32(&mut frame, width, height, &quad));
        assert_pixel(
            at(&frame, width, 2, 2),
            at(&pixels, width, 0, 0),
            "authored top left",
        );
        assert_pixel(
            at(&frame, width, 6, 2),
            at(&pixels, width, 8, 0),
            "authored top right",
        );
        assert_pixel(
            at(&frame, width, 2, 6),
            at(&pixels, width, 0, 8),
            "authored bottom left",
        );
        assert_pixel(
            at(&frame, width, 6, 6),
            at(&pixels, width, 8, 8),
            "authored bottom right",
        );
    }

    #[test]
    fn trapezoid_projection_narrows_the_top_edge() {
        let (width, height) = (9, 7);
        let pixels = gradient(width, height);
        let mut frame = pixels.clone();
        let trapezoid = grade([0.25, 0.0], [0.75, 0.0], [0.0, 1.0], [1.0, 1.0]);
        assert!(corner_pin_frame_f32(&mut frame, width, height, &trapezoid));
        assert_pixel(
            at(&frame, width, 4, 0),
            at(&pixels, width, 4, 0),
            "narrow top centre",
        );
        assert_pixel(
            at(&frame, width, 4, 6),
            at(&pixels, width, 4, 6),
            "wide bottom centre",
        );
        assert_pixel(
            at(&frame, width, 0, 6),
            at(&pixels, width, 0, 6),
            "wide bottom left",
        );
        assert_pixel(at(&frame, width, 0, 0), [0.0; 4], "left of the top edge");
        assert_pixel(at(&frame, width, 8, 0), [0.0; 4], "right of the top edge");
    }

    #[test]
    fn destination_pixels_outside_the_quad_are_transparent() {
        let (width, height) = (9, 7);
        let pixels = gradient(width, height);
        let mut frame = pixels.clone();
        let inset = grade([0.5, 0.5], [0.75, 0.5], [0.5, 0.75], [0.75, 0.75]);
        assert!(corner_pin_frame_f32(&mut frame, width, height, &inset));
        assert_pixel(at(&frame, width, 0, 0), [0.0; 4], "top left outside");
        assert_pixel(at(&frame, width, 8, 6), [0.0; 4], "bottom right outside");
        assert_pixel(at(&frame, width, 0, 3), [0.0; 4], "left outside");
        assert_pixel(at(&frame, width, 3, 3), [0.0; 4], "just outside");
        assert!(
            at(&frame, width, 5, 4)[3] > 0.99,
            "the pixel inside the quad should stay opaque"
        );
    }

    #[test]
    fn fully_transparent_source_pixels_stay_transparent() {
        let (width, height) = (5, 5);
        let mut pixels = gradient(width, height);
        pixels[2 * width + 3] = [1.0, 0.0, 0.0, 0.0];
        let mut frame = pixels.clone();
        assert!(corner_pin_frame_f32(&mut frame, width, height, &identity()));
        assert_pixel(
            at(&frame, width, 3, 2),
            [0.0; 4],
            "hidden colour must not survive",
        );
        let mirrored = grade([1.0, 0.0], [0.0, 0.0], [1.0, 1.0], [0.0, 1.0]);
        let mut flipped = pixels.clone();
        assert!(corner_pin_frame_f32(&mut flipped, width, height, &mirrored));
        assert_pixel(
            at(&flipped, width, 1, 2),
            [0.0; 4],
            "hidden colour stays hidden after the mirror",
        );
    }

    #[test]
    fn premultiplied_mixing_does_not_bleed_hidden_colour() {
        let (width, height) = (5, 3);
        let mut pixels = gradient(width, height);
        pixels[width + 1] = [0.0, 1.0, 0.0, 0.0];
        pixels[width + 2] = [1.0, 0.0, 0.0, 1.0];
        let mut frame = pixels.clone();
        let doubled = grade([-0.5, 0.0], [1.5, 0.0], [-0.5, 1.0], [1.5, 1.0]);
        assert!(corner_pin_frame_f32(&mut frame, width, height, &doubled));
        let mixed = at(&frame, width, 1, 1);
        assert!(
            (mixed[0] - 1.0).abs() <= 1e-5,
            "red channel: expected 1.0, got {}",
            mixed[0]
        );
        assert!(
            mixed[1].abs() <= 1e-5,
            "green must not bleed through the transparent neighbour, got {}",
            mixed[1]
        );
        assert!(
            (mixed[3] - 0.5).abs() <= 1e-5,
            "alpha: expected 0.5, got {}",
            mixed[3]
        );
    }

    #[test]
    fn degenerate_width_passes_the_frame_through() {
        let pixels = vec![[0.25, 0.5, 0.75, 0.5]; 4];
        let mut frame = pixels.clone();
        let shifted = grade([0.0, 0.25], [1.0, 0.25], [0.0, 0.75], [1.0, 0.75]);
        assert!(corner_pin_frame_f32(&mut frame, 1, 4, &shifted));
        assert_eq!(frame, pixels);
    }

    #[test]
    fn collapsed_quad_passes_the_frame_through() {
        let (width, height) = (9, 7);
        let pixels = gradient(width, height);
        let collapsed = grade([0.5, 0.5], [0.5, 0.5], [0.5, 0.5], [0.5, 0.5]);
        assert!(collapsed.homography(width as u32, height as u32).is_none());
        assert!(collapsed.inverse_homography(width as u32, height as u32).is_none());
        let mut frame = pixels.clone();
        assert!(corner_pin_frame_f32(&mut frame, width, height, &collapsed));
        assert_eq!(frame, pixels);
    }

    #[test]
    fn homography_and_inverse_compose_to_the_identity() {
        let trapezoid = grade([0.12, 0.08], [0.94, -0.05], [0.02, 0.9], [1.1, 1.05]);
        let homography =
            trapezoid.homography(23, 19).expect("the trapezoid should define a homography");
        let inverse = trapezoid.inverse_homography(23, 19).expect("the trapezoid should invert");
        assert!(
            (inverse[8] - 1.0).abs() <= 1e-12,
            "the inverse should be normalized to h22 == 1, got {}",
            inverse[8]
        );
        let product = multiply(&homography, &inverse);
        let scale = product[8];
        assert!(scale.abs() > 1e-6, "the product should not be degenerate");
        for (index, expected) in IDENTITY_MATRIX.iter().enumerate() {
            let actual = product[index] / scale;
            assert!(
                (actual - expected).abs() <= 1e-9,
                "composed entry {index}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn gpu_controls_pack_the_normalized_inverse() {
        let trapezoid = grade([-0.1, 0.15], [1.2, 0.0], [0.05, 0.95], [0.85, 1.3]);
        let inverse = trapezoid.inverse_homography(23, 19).expect("the trapezoid should invert");
        let (params, extra) = trapezoid.gpu_controls(23, 19).expect("the trapezoid should pack");
        assert_eq!(
            params,
            [
                inverse[0] as f32,
                inverse[1] as f32,
                inverse[2] as f32,
                inverse[3] as f32,
            ]
        );
        assert_eq!(
            extra,
            [
                inverse[4] as f32,
                inverse[5] as f32,
                inverse[6] as f32,
                inverse[7] as f32,
            ]
        );
        assert!((inverse[8] - 1.0).abs() <= 1e-12);
    }

    #[test]
    fn new_rejects_corners_outside_the_declared_range() {
        let defaults = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
        let cases = [
            ([10.5, 0.0], CornerPinError::TopLeft),
            ([f32::NAN, 0.0], CornerPinError::TopRight),
            ([0.0, -10.5], CornerPinError::BottomLeft),
            ([f32::INFINITY, 0.0], CornerPinError::BottomRight),
        ];
        for (index, (bad, expected)) in cases.into_iter().enumerate() {
            let mut corners = defaults;
            corners[index] = bad;
            let error = CornerPinGrade::new(corners[0], corners[1], corners[2], corners[3])
                .expect_err("the out-of-range corner should be rejected");
            assert_eq!(error, expected);
        }
        assert_eq!(
            CornerPinError::TopLeft.to_string(),
            "corner pin top left must be a finite point inside [-10, 10]"
        );
    }

    #[test]
    fn is_identity_is_exact() {
        assert!(identity().is_identity());
        let near = [
            [[0.0, 0.0001], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
            [[0.0, 0.0], [0.9999, 0.0], [0.0, 1.0], [1.0, 1.0]],
            [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0001], [1.0, 1.0]],
            [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [0.9999, 1.0]],
        ];
        for corners in near {
            let grade = grade(corners[0], corners[1], corners[2], corners[3]);
            assert!(!grade.is_identity());
        }
    }

    #[test]
    fn homography_hits_every_authored_corner() {
        let quad = grade([-0.2, 0.1], [1.1, -0.15], [0.05, 0.85], [0.9, 1.2]);
        let homography = quad.homography(31, 17).expect("the quad should define a homography");
        let [h00, h01, h02, h10, h11, h12, h20, h21, h22] = homography;
        let sources = [[0.0_f64, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
        for (source, destination) in sources.into_iter().zip(quad.corners()) {
            let [x, y] = source;
            let denominator = h20 * x + h21 * y + h22;
            let mapped = [
                (h00 * x + h01 * y + h02) / denominator,
                (h10 * x + h11 * y + h12) / denominator,
            ];
            assert!(
                (mapped[0] - f64::from(destination[0])).abs() <= 1e-9,
                "corner x: expected {}, got {}",
                destination[0],
                mapped[0]
            );
            assert!(
                (mapped[1] - f64::from(destination[1])).abs() <= 1e-9,
                "corner y: expected {}, got {}",
                destination[1],
                mapped[1]
            );
        }
    }
}
