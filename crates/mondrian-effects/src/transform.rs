//! Transform: moves, scales, skews and rotates the frame about its centre.
//!
//! The geometry works in pixel-centre coordinates: the pixel with index
//! `(i, j)` has its centre at the continuous point `(i, j)`, so the frame
//! centre is `c = ((width - 1) / 2, (height - 1) / 2)`. A destination pixel
//! centre `p` maps forwards to
//!
//! ```text
//! q = M (p - c - anchor) + c + position
//! ```
//!
//! where `M = R · S · K` composes three steps in this order:
//!
//! - `K = I + tan(skew) · a ⊗ b` — a shear along the axis
//!   `a = (cos φ, sin φ)` and its perpendicular `b = (-sin φ, cos φ)`, with
//!   `φ = skew_axis`; both angles are in degrees,
//! - `S = diag(uniform_scale / 100 · scale_width / 100,
//!   uniform_scale / 100 · scale_height / 100)` — percentages, so the default
//!   `100` on all three factors is a no-op,
//! - `R = [[cos θ, -sin θ], [sin θ, cos θ]]` in y-down screen space, so a
//!   positive rotation turns the image clockwise on screen, matching AE.
//!
//! Sampling inverts that mapping: the source point for `p` is
//! `u = M⁻¹ (p - c - position) + anchor + c`, so `position` slides the image
//! by whole pixels while `anchor` moves the pivot. The source is read
//! bilinearly **in premultiplied space**, and positions outside the frame clamp
//! to the nearest edge sample.
//!
//! The sampling menu supersamples each destination pixel in premultiplied
//! space: `low` takes one tap, `normal` 2×2 and `high` 4×4, on the same
//! sub-pixel grid the wave warp uses.
//!
//! Opacity is applied to the averaged premultiplied sample, so it scales colour
//! and alpha together, and the result is unpremultiplied afterwards.
//!
//! `use_composition_shutter` and `shutter_angle` are accepted and validated but
//! **reserved**: motion blur would have to integrate each sample over the
//! shutter interval along a per-frame velocity, and the effect graph carries no
//! velocity — a static transform has none. Until the renderer hands effects a
//! motion vector, changing either field cannot change the output, and the
//! kernel tests pin that down.

use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest anchor or position offset from the frame centre, in pixels.
pub const TRANSFORM_MAX_CENTRE_PIXELS: f32 = 1_000_000.0;
/// Largest scale percentage the author state may declare.
pub const TRANSFORM_MAX_SCALE_PERCENT: f32 = 2_000.0;
/// Smallest scale percentage the author state may declare.
pub const TRANSFORM_MIN_SCALE_PERCENT: f32 = -2_000.0;
/// Largest skew angle in degrees; the tangent must stay finite.
pub const TRANSFORM_MAX_SKEW_DEGREES: f32 = 89.0;
/// Smallest skew angle in degrees; the tangent must stay finite.
pub const TRANSFORM_MIN_SKEW_DEGREES: f32 = -89.0;
/// Largest skew axis angle in degrees.
pub const TRANSFORM_MAX_AXIS_DEGREES: f32 = 360_000.0;
/// Largest rotation angle in degrees.
pub const TRANSFORM_MAX_ROTATION_DEGREES: f32 = 360_000.0;
/// Largest composition shutter angle in degrees.
pub const TRANSFORM_MAX_SHUTTER_DEGREES: f32 = 360.0;
/// Number of sampling qualities.
pub const TRANSFORM_SAMPLING_COUNT: usize = 3;

/// Sampling quality: how many sub-pixel taps one destination pixel averages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransformSampling {
    /// One tap per destination pixel.
    Low,
    /// 2×2 taps per destination pixel.
    Normal,
    /// 4×4 taps per destination pixel.
    High,
}

impl TransformSampling {
    /// Every sampling quality, in menu order.
    pub const ALL: [TransformSampling; TRANSFORM_SAMPLING_COUNT] =
        [Self::Low, Self::Normal, Self::High];

    /// Stable author-state key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
        }
    }

    /// Parses an author-state key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|quality| quality.as_key() == key)
    }

    /// Zero-based index handed to the GPU.
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::High => 2,
        }
    }

    /// One-based menu index used by the authoring surface.
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Taps taken along each axis.
    pub const fn samples_per_axis(self) -> u32 {
        match self {
            Self::Low => 1,
            Self::Normal => 2,
            Self::High => 4,
        }
    }
}

/// A transform parameter outside its declared range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransformError {
    /// The anchor offset is not finite or exceeds the centre limit.
    Anchor,
    /// The position offset is not finite or exceeds the centre limit.
    Position,
    /// A scale percentage is not finite or exceeds the scale limit.
    Scale,
    /// The skew angle is not finite or exceeds the finite-tangent limit.
    Skew,
    /// The skew axis angle is not finite or exceeds the axis limit.
    SkewAxis,
    /// The rotation angle is not finite or exceeds the rotation limit.
    Rotation,
    /// The opacity is not inside `0..=100` percent.
    Opacity,
    /// The composition shutter angle is not inside `0..=360` degrees.
    ShutterAngle,
}

impl std::fmt::Display for TransformError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Anchor => "transform anchor must be finite and within +/-1000000 pixels",
            Self::Position => "transform position must be finite and within +/-1000000 pixels",
            Self::Scale => "transform scale must be finite and within +/-2000 percent",
            Self::Skew => "transform skew must be finite and within +/-89 degrees",
            Self::SkewAxis => "transform skew axis must be finite and within +/-360000 degrees",
            Self::Rotation => "transform rotation must be finite and within +/-360000 degrees",
            Self::Opacity => "transform opacity must be within 0..=100 percent",
            Self::ShutterAngle => "transform shutter angle must be within 0..=360 degrees",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for TransformError {}

/// A validated transform: the authored values the kernel and the GPU pass read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformGrade {
    anchor: [f32; 2],
    position: [f32; 2],
    uniform_scale: f32,
    scale_width: f32,
    scale_height: f32,
    skew_degrees: f32,
    skew_axis_degrees: f32,
    rotation_degrees: f32,
    opacity_percent: f32,
    use_composition_shutter: bool,
    shutter_angle_degrees: f32,
    sampling: TransformSampling,
}

impl TransformGrade {
    /// Validates and builds a transform.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        anchor: [f32; 2],
        position: [f32; 2],
        uniform_scale: f32,
        scale_width: f32,
        scale_height: f32,
        skew_degrees: f32,
        skew_axis_degrees: f32,
        rotation_degrees: f32,
        opacity_percent: f32,
        use_composition_shutter: bool,
        shutter_angle_degrees: f32,
        sampling: TransformSampling,
    ) -> Result<Self, TransformError> {
        let grade = Self {
            anchor,
            position,
            uniform_scale,
            scale_width,
            scale_height,
            skew_degrees,
            skew_axis_degrees,
            rotation_degrees,
            opacity_percent,
            use_composition_shutter,
            shutter_angle_degrees,
            sampling,
        };
        grade.validate()?;
        Ok(grade)
    }

    fn validate(&self) -> Result<(), TransformError> {
        if !self.anchor.iter().all(|value| within(*value, TRANSFORM_MAX_CENTRE_PIXELS)) {
            return Err(TransformError::Anchor);
        }
        if !self.position.iter().all(|value| within(*value, TRANSFORM_MAX_CENTRE_PIXELS)) {
            return Err(TransformError::Position);
        }
        for scale in [self.uniform_scale, self.scale_width, self.scale_height] {
            if !(TRANSFORM_MIN_SCALE_PERCENT..=TRANSFORM_MAX_SCALE_PERCENT).contains(&scale)
                || !scale.is_finite()
            {
                return Err(TransformError::Scale);
            }
        }
        if !within(self.skew_degrees, TRANSFORM_MAX_SKEW_DEGREES) {
            return Err(TransformError::Skew);
        }
        if !within(self.skew_axis_degrees, TRANSFORM_MAX_AXIS_DEGREES) {
            return Err(TransformError::SkewAxis);
        }
        if !within(self.rotation_degrees, TRANSFORM_MAX_ROTATION_DEGREES) {
            return Err(TransformError::Rotation);
        }
        if !(0.0..=100.0).contains(&self.opacity_percent) || !self.opacity_percent.is_finite() {
            return Err(TransformError::Opacity);
        }
        if !(0.0..=TRANSFORM_MAX_SHUTTER_DEGREES).contains(&self.shutter_angle_degrees)
            || !self.shutter_angle_degrees.is_finite()
        {
            return Err(TransformError::ShutterAngle);
        }
        Ok(())
    }

    /// Anchor offset from the frame centre, in pixels.
    pub const fn anchor(&self) -> [f32; 2] {
        self.anchor
    }

    /// Position offset from the frame centre, in pixels.
    pub const fn position(&self) -> [f32; 2] {
        self.position
    }

    /// Scale percentage applied to both axes before the per-axis factors.
    pub const fn uniform_scale(&self) -> f32 {
        self.uniform_scale
    }

    /// Extra per-axis scale percentage for the width.
    pub const fn scale_width(&self) -> f32 {
        self.scale_width
    }

    /// Extra per-axis scale percentage for the height.
    pub const fn scale_height(&self) -> f32 {
        self.scale_height
    }

    /// Skew angle in degrees.
    pub const fn skew_degrees(&self) -> f32 {
        self.skew_degrees
    }

    /// Skew axis angle in degrees.
    pub const fn skew_axis_degrees(&self) -> f32 {
        self.skew_axis_degrees
    }

    /// Rotation angle in degrees, clockwise on screen.
    pub const fn rotation_degrees(&self) -> f32 {
        self.rotation_degrees
    }

    /// Opacity percentage applied to the premultiplied output.
    pub const fn opacity_percent(&self) -> f32 {
        self.opacity_percent
    }

    /// Whether the reserved composition shutter is enabled.
    pub const fn use_composition_shutter(&self) -> bool {
        self.use_composition_shutter
    }

    /// Reserved composition shutter angle in degrees.
    pub const fn shutter_angle_degrees(&self) -> f32 {
        self.shutter_angle_degrees
    }

    /// Sampling quality.
    pub const fn sampling(&self) -> TransformSampling {
        self.sampling
    }

    /// True when the transform maps every pixel onto itself.
    pub fn is_identity(&self) -> bool {
        self.anchor == [0.0, 0.0]
            && self.position == [0.0, 0.0]
            && self.uniform_scale == 100.0
            && self.scale_width == 100.0
            && self.scale_height == 100.0
            && self.skew_degrees == 0.0
            && self.rotation_degrees == 0.0
            && self.opacity_percent == 100.0
    }

    /// Net scale factors: `(x, y)` multipliers, so `(1.0, 1.0)` is a no-op.
    pub fn scale_factors(&self) -> (f64, f64) {
        let uniform = f64::from(self.uniform_scale) / 100.0;
        (
            uniform * f64::from(self.scale_width) / 100.0,
            uniform * f64::from(self.scale_height) / 100.0,
        )
    }

    /// Opacity as a fraction of the premultiplied output.
    fn opacity_fraction(&self) -> f64 {
        f64::from(self.opacity_percent) / 100.0
    }

    /// The forward affine in centre coordinates,
    /// `u' = M (u - anchor) + position`, as row-major
    /// `[m00, m01, m02, m10, m11, m12]`.
    ///
    /// The coefficients do not depend on the frame size: the mapping is
    /// centre-relative, and callers add the centre back themselves. The size
    /// stays in the signature because the GPU pass rebuilds the centre from the
    /// frame size and because an empty frame has no defined transform.
    pub fn forward_matrix(&self, width: u32, height: u32) -> [f64; 6] {
        self.affine(width, height)
    }

    /// The inverse of [`Self::forward_matrix`], or `None` when the transform is
    /// degenerate (a determinant under `1e-12`, as a zero scale produces) or the
    /// frame is empty.
    pub fn inverse_matrix(&self, width: u32, height: u32) -> Option<[f64; 6]> {
        if width == 0 || height == 0 {
            return None;
        }
        let [m00, m01, m02, m10, m11, m12] = self.affine(width, height);
        let determinant = m00 * m11 - m01 * m10;
        if !determinant.is_finite() || determinant.abs() < 1e-12 {
            return None;
        }
        let i00 = m11 / determinant;
        let i01 = -m01 / determinant;
        let i10 = -m10 / determinant;
        let i11 = m00 / determinant;
        let i02 = -(i00 * m02 + i01 * m12);
        let i12 = -(i10 * m02 + i11 * m12);
        let inverse = [i00, i01, i02, i10, i11, i12];
        if !inverse.iter().all(|value| value.is_finite()) {
            return None;
        }
        Some(inverse)
    }

    /// Packs the inverse matrix for the GPU spatial pass.
    ///
    /// `params = [i00, i01, i02, i10]` and
    /// `extra = [i11, i12, opacity_fraction, samples_per_axis]`. The matrix is in
    /// centre coordinates, matching [`Self::inverse_matrix`]; the shader rebuilds
    /// the centre from the frame size. `None` means the transform is degenerate
    /// and the renderer should pass the frame through.
    pub fn gpu_controls(&self, width: u32, height: u32) -> Option<([f32; 4], [f32; 4])> {
        let [i00, i01, i02, i10, i11, i12] = self.inverse_matrix(width, height)?;
        Some((
            [i00 as f32, i01 as f32, i02 as f32, i10 as f32],
            [
                i11 as f32,
                i12 as f32,
                self.opacity_fraction() as f32,
                self.sampling.samples_per_axis() as f32,
            ],
        ))
    }

    fn affine(&self, _width: u32, _height: u32) -> [f64; 6] {
        let (scale_x, scale_y) = self.scale_factors();
        let [k00, k01, k10, k11] = shear_matrix(
            f64::from(self.skew_degrees),
            f64::from(self.skew_axis_degrees),
        );
        let (sin_theta, cos_theta) = f64::from(self.rotation_degrees).to_radians().sin_cos();
        let m00 = cos_theta * scale_x * k00 - sin_theta * scale_y * k10;
        let m01 = cos_theta * scale_x * k01 - sin_theta * scale_y * k11;
        let m10 = sin_theta * scale_x * k00 + cos_theta * scale_y * k10;
        let m11 = sin_theta * scale_x * k01 + cos_theta * scale_y * k11;
        let anchor_x = f64::from(self.anchor[0]);
        let anchor_y = f64::from(self.anchor[1]);
        let offset_x = f64::from(self.position[0]) - (m00 * anchor_x + m01 * anchor_y);
        let offset_y = f64::from(self.position[1]) - (m10 * anchor_x + m11 * anchor_y);
        [m00, m01, offset_x, m10, m11, offset_y]
    }
}

/// True when `value` is finite and inside `+/-limit`.
fn within(value: f32, limit: f32) -> bool {
    value.is_finite() && (-limit..=limit).contains(&value)
}

/// The shear `K = I + tan(skew) · a ⊗ b`, as row-major `[k00, k01, k10, k11]`.
fn shear_matrix(skew_degrees: f64, axis_degrees: f64) -> [f64; 4] {
    let (sin_axis, cos_axis) = axis_degrees.to_radians().sin_cos();
    let tangent = skew_degrees.to_radians().tan();
    let (axis_x, axis_y) = (cos_axis, sin_axis);
    let (perpendicular_x, perpendicular_y) = (-sin_axis, cos_axis);
    [
        1.0 + tangent * axis_x * perpendicular_x,
        tangent * axis_x * perpendicular_y,
        tangent * axis_y * perpendicular_x,
        1.0 + tangent * axis_y * perpendicular_y,
    ]
}

/// Applies the transform to a whole frame, returning `false` when the frame
/// cannot be processed.
pub fn transform_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &TransformGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_transform_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the transform in place with an explicit cancellation checkpoint.
pub(crate) fn apply_transform_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &TransformGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    let Some(inverse) = grade.inverse_matrix(width as u32, height as u32) else {
        // A degenerate transform has no inverse to sample through, so the frame
        // passes through untouched rather than producing undefined samples.
        return Ok(true);
    };

    let frame = &mut working[..pixel_count];
    premultiply_rgba_controlled(frame, checkpoint)?;

    let source = frame.to_vec();
    let [i00, i01, i02, i10, i11, i12] = inverse;
    let centre_x = (width - 1) as f64 * 0.5;
    let centre_y = (height - 1) as f64 * 0.5;
    let opacity = grade.opacity_fraction();
    let samples = grade.sampling.samples_per_axis().max(1);
    let step = 1.0 / f64::from(samples);
    let average = opacity / f64::from(samples * samples);

    checkpoint()?;
    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let mut accumulated = [0.0f64; 4];
                for sample_row in 0..samples {
                    let offset_y = (f64::from(sample_row) + 0.5) * step - 0.5;
                    for sample_column in 0..samples {
                        let offset_x = (f64::from(sample_column) + 0.5) * step - 0.5;
                        let relative_x = column as f64 + offset_x - centre_x;
                        let relative_y = row as f64 + offset_y - centre_y;
                        let sample_x = i00 * relative_x + i01 * relative_y + i02 + centre_x;
                        let sample_y = i10 * relative_x + i11 * relative_y + i12 + centre_y;
                        let texel =
                            sample_bilinear_clamped(&source, width, height, sample_x, sample_y);
                        for channel in 0..4 {
                            accumulated[channel] += f64::from(texel[channel]);
                        }
                    }
                }
                *pixel = [
                    (accumulated[0] * average) as f32,
                    (accumulated[1] * average) as f32,
                    (accumulated[2] * average) as f32,
                    (accumulated[3] * average) as f32,
                ];
            }
        }
    });

    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{
        transform_frame_f32, TransformError, TransformGrade, TransformSampling,
        TRANSFORM_MAX_AXIS_DEGREES, TRANSFORM_MAX_CENTRE_PIXELS, TRANSFORM_MAX_ROTATION_DEGREES,
        TRANSFORM_MAX_SCALE_PERCENT, TRANSFORM_MAX_SHUTTER_DEGREES, TRANSFORM_MAX_SKEW_DEGREES,
    };

    /// Builds a grade with the shutter reserved and disabled.
    #[allow(clippy::too_many_arguments)]
    fn grade(
        anchor: [f32; 2],
        position: [f32; 2],
        uniform_scale: f32,
        scale_width: f32,
        scale_height: f32,
        skew_degrees: f32,
        skew_axis_degrees: f32,
        rotation_degrees: f32,
        opacity_percent: f32,
        sampling: TransformSampling,
    ) -> TransformGrade {
        TransformGrade::new(
            anchor,
            position,
            uniform_scale,
            scale_width,
            scale_height,
            skew_degrees,
            skew_axis_degrees,
            rotation_degrees,
            opacity_percent,
            false,
            180.0,
            sampling,
        )
        .expect("the grade is inside the documented ranges")
    }

    fn identity() -> TransformGrade {
        grade(
            [0.0, 0.0],
            [0.0, 0.0],
            100.0,
            100.0,
            100.0,
            0.0,
            0.0,
            0.0,
            100.0,
            TransformSampling::Low,
        )
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
                (actual[channel] - expected[channel]).abs() <= 1.0e-5,
                "{label} channel {channel}: expected {expected:?} but got {actual:?}"
            );
        }
    }

    #[test]
    fn identity_leaves_the_frame_untouched() {
        let pixels = gradient(7, 5);
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, 7, 5, &identity()));
        assert_eq!(frame, pixels);
        let matrix = identity().forward_matrix(7, 5);
        for (actual, expected) in matrix.iter().zip([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]) {
            assert!(
                (actual - expected).abs() <= 1.0e-12,
                "identity forward matrix {matrix:?}"
            );
        }
    }

    #[test]
    fn an_integer_translation_shifts_the_frame() {
        let width = 7_usize;
        let height = 5_usize;
        let mut pixels = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                pixels.push([column as f32 + 10.0 * row as f32, 0.25, 0.5, 1.0]);
            }
        }
        let translated = grade(
            [0.0, 0.0],
            [2.0, -1.0],
            100.0,
            100.0,
            100.0,
            0.0,
            0.0,
            0.0,
            100.0,
            TransformSampling::Low,
        );
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, width, height, &translated));
        for row in 0..height {
            for column in 0..width {
                let source_column = (column as i64 - 2).clamp(0, width as i64 - 1) as usize;
                let source_row = (row as i64 + 1).clamp(0, height as i64 - 1) as usize;
                assert_pixel(
                    at(&frame, width, column, row),
                    at(&pixels, width, source_column, source_row),
                    "translated pixel",
                );
            }
        }
    }

    #[test]
    fn a_quarter_turn_moves_the_top_left_corner_to_the_top_right() {
        let width = 5_usize;
        let height = 5_usize;
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        pixels[0] = [1.0, 0.0, 0.0, 1.0];
        let rotated = grade(
            [0.0, 0.0],
            [0.0, 0.0],
            100.0,
            100.0,
            100.0,
            0.0,
            0.0,
            90.0,
            100.0,
            TransformSampling::Low,
        );
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, width, height, &rotated));
        assert_pixel(
            at(&frame, width, 4, 0),
            [1.0, 0.0, 0.0, 1.0],
            "top right after a quarter turn",
        );
        assert_pixel(
            at(&frame, width, 0, 0),
            [0.0, 0.0, 0.0, 1.0],
            "top left after a quarter turn",
        );
    }

    #[test]
    fn a_uniform_scale_of_fifty_percent_pulls_content_towards_the_centre() {
        let width = 7_usize;
        let height = 7_usize;
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        pixels[3 * width + 5] = [0.0, 1.0, 0.0, 1.0];
        let scaled = grade(
            [0.0, 0.0],
            [0.0, 0.0],
            50.0,
            100.0,
            100.0,
            0.0,
            0.0,
            0.0,
            100.0,
            TransformSampling::Low,
        );
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, width, height, &scaled));
        assert_pixel(
            at(&frame, width, 4, 3),
            [0.0, 1.0, 0.0, 1.0],
            "scaled marker",
        );
        assert_pixel(
            at(&frame, width, 5, 3),
            [0.0, 0.0, 0.0, 1.0],
            "vacated marker column",
        );
    }

    #[test]
    fn a_non_uniform_scale_stretches_each_axis_on_its_own() {
        let width = 11_usize;
        let height = 9_usize;
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        pixels[4 * width + 6] = [0.0, 0.0, 1.0, 1.0];
        pixels[6 * width + 5] = [1.0, 1.0, 0.0, 1.0];
        let scaled = grade(
            [0.0, 0.0],
            [0.0, 0.0],
            100.0,
            200.0,
            50.0,
            0.0,
            0.0,
            0.0,
            100.0,
            TransformSampling::Low,
        );
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, width, height, &scaled));
        assert_pixel(
            at(&frame, width, 7, 4),
            [0.0, 0.0, 1.0, 1.0],
            "stretched width marker",
        );
        assert_pixel(
            at(&frame, width, 5, 5),
            [1.0, 1.0, 0.0, 1.0],
            "squashed height marker",
        );
    }

    #[test]
    fn a_zero_skew_axis_shears_horizontally() {
        let width = 9_usize;
        let height = 9_usize;
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        pixels[5 * width + 5] = [1.0, 0.0, 1.0, 1.0];
        let sheared = grade(
            [0.0, 0.0],
            [0.0, 0.0],
            100.0,
            100.0,
            100.0,
            45.0,
            0.0,
            0.0,
            100.0,
            TransformSampling::Low,
        );
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, width, height, &sheared));
        // `x' = x + tan(skew) · y` for an axis along the x axis, so the marked
        // pixel one row below the centre lands one column further right.
        assert_pixel(
            at(&frame, width, 6, 5),
            [1.0, 0.0, 1.0, 1.0],
            "horizontally sheared marker",
        );
        assert_pixel(
            at(&frame, width, 4, 5),
            [0.0, 0.0, 0.0, 1.0],
            "vacated horizontally sheared column",
        );
    }

    #[test]
    fn a_ninety_degree_skew_axis_shears_vertically() {
        let width = 9_usize;
        let height = 9_usize;
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        pixels[4 * width + 5] = [1.0, 0.0, 1.0, 1.0];
        let sheared = grade(
            [0.0, 0.0],
            [0.0, 0.0],
            100.0,
            100.0,
            100.0,
            45.0,
            90.0,
            0.0,
            100.0,
            TransformSampling::Low,
        );
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, width, height, &sheared));
        // Rotating the skew axis by 90° turns the shear into
        // `y' = y - tan(skew) · x`, so the marked pixel one column right of the
        // centre lands one row higher.
        assert_pixel(
            at(&frame, width, 5, 3),
            [1.0, 0.0, 1.0, 1.0],
            "vertically sheared marker",
        );
        assert_pixel(
            at(&frame, width, 5, 4),
            [0.0, 0.0, 0.0, 1.0],
            "vacated vertically sheared row",
        );
    }

    #[test]
    fn opacity_halves_the_premultiplied_output() {
        let width = 4_usize;
        let height = 3_usize;
        let pixels = vec![[0.4, 0.6, 0.2, 0.8]; width * height];
        let faded = grade(
            [0.0, 0.0],
            [0.0, 0.0],
            100.0,
            100.0,
            100.0,
            0.0,
            0.0,
            0.0,
            50.0,
            TransformSampling::Low,
        );
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, width, height, &faded));
        // The opacity scales premultiplied colour and alpha together, so the
        // straight-alpha colour survives and only the alpha halves.
        assert_pixel(at(&frame, width, 2, 1), [0.4, 0.6, 0.2, 0.4], "faded pixel");
    }

    #[test]
    fn a_transparent_source_pixel_stays_transparent() {
        let width = 5_usize;
        let height = 5_usize;
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        pixels[2 * width + 2] = [0.9, 0.2, 0.4, 0.0];
        let scaled = grade(
            [0.0, 0.0],
            [0.0, 0.0],
            200.0,
            100.0,
            100.0,
            0.0,
            0.0,
            0.0,
            100.0,
            TransformSampling::Low,
        );
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, width, height, &scaled));
        assert_pixel(
            at(&frame, width, 2, 2),
            [0.0, 0.0, 0.0, 0.0],
            "transparent source pixel",
        );
    }

    #[test]
    fn a_zero_scale_passes_the_frame_through() {
        let pixels = gradient(6, 4);
        let collapsed = grade(
            [0.0, 0.0],
            [0.0, 0.0],
            0.0,
            100.0,
            100.0,
            0.0,
            0.0,
            0.0,
            100.0,
            TransformSampling::Low,
        );
        assert!(collapsed.inverse_matrix(6, 4).is_none());
        assert!(collapsed.gpu_controls(6, 4).is_none());
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, 6, 4, &collapsed));
        assert_eq!(frame, pixels);
    }

    #[test]
    fn out_of_frame_samples_clamp_to_the_frame_edge() {
        let width = 6_usize;
        let height = 4_usize;
        let pixels = gradient(width, height);
        let pushed = grade(
            [0.0, 0.0],
            [100.0, 0.0],
            100.0,
            100.0,
            100.0,
            0.0,
            0.0,
            0.0,
            100.0,
            TransformSampling::Low,
        );
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, width, height, &pushed));
        for row in 0..height {
            for column in 0..width {
                assert_pixel(
                    at(&frame, width, column, row),
                    at(&pixels, width, 0, row),
                    "clamped pixel",
                );
            }
        }
    }

    #[test]
    fn the_sampling_quality_supersamples_a_sub_pixel_position() {
        let width = 16_usize;
        let height = 16_usize;
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        pixels[5 * width + 5] = [1.0, 1.0, 1.0, 1.0];
        // A 25 % scale puts the centre-relative destination pixel (7, 7) on the
        // source point (5.5, 5.5): the single tap blends four texels, while the
        // 4x4 grid lands exactly on the texel centres around it.
        let low_grade = grade(
            [0.0, 0.0],
            [0.0, 0.0],
            25.0,
            100.0,
            100.0,
            0.0,
            0.0,
            0.0,
            100.0,
            TransformSampling::Low,
        );
        let normal_grade = TransformGrade { sampling: TransformSampling::Normal, ..low_grade };
        let high_grade = TransformGrade { sampling: TransformSampling::High, ..low_grade };
        let mut low = pixels.clone();
        let mut normal = pixels.clone();
        let mut high = pixels.clone();
        assert!(transform_frame_f32(&mut low, width, height, &low_grade));
        assert!(transform_frame_f32(
            &mut normal,
            width,
            height,
            &normal_grade
        ));
        assert!(transform_frame_f32(&mut high, width, height, &high_grade));
        // Only the impulse at (5, 5) is visible; the four bilinear weights are
        // 0.25 each, and only one of the sixteen high-quality taps hits it.
        assert_pixel(at(&low, width, 7, 7), [0.25, 0.25, 0.25, 1.0], "low tap");
        assert_pixel(
            at(&high, width, 7, 7),
            [0.0625, 0.0625, 0.0625, 1.0],
            "high taps",
        );
        let difference = (at(&low, width, 7, 7)[0] - at(&high, width, 7, 7)[0]).abs();
        assert!(
            difference > 0.1,
            "4x4 supersampling must differ from the single tap, got {difference}"
        );
        assert_ne!(normal, low, "the 2x2 grid must differ from the single tap");
    }

    #[test]
    fn the_shutter_fields_are_reserved_and_inert() {
        let width = 9_usize;
        let height = 7_usize;
        let pixels = gradient(width, height);
        let disabled = grade(
            [1.5, -2.0],
            [3.0, -1.0],
            130.0,
            90.0,
            110.0,
            12.0,
            25.0,
            33.0,
            80.0,
            TransformSampling::Normal,
        );
        let enabled = TransformGrade {
            use_composition_shutter: true,
            shutter_angle_degrees: 300.0,
            ..disabled
        };
        assert!(enabled.use_composition_shutter());
        assert_eq!(enabled.shutter_angle_degrees(), 300.0);

        let mut left = pixels.clone();
        let mut right = pixels.clone();
        assert!(transform_frame_f32(&mut left, width, height, &disabled));
        assert!(transform_frame_f32(&mut right, width, height, &enabled));
        assert_eq!(left, right, "reserved shutter fields must not alter output");
        assert_ne!(left, pixels, "the geometry itself must still transform");
    }

    #[test]
    fn the_inverse_matrix_undoes_the_forward_matrix() {
        let graded = grade(
            [3.5, -2.25],
            [-4.0, 6.5],
            137.0,
            88.0,
            142.0,
            21.0,
            33.0,
            47.0,
            60.0,
            TransformSampling::High,
        );
        let forward = graded.forward_matrix(1920, 1080);
        let inverse = graded.inverse_matrix(1920, 1080).expect("the transform is invertible");
        for (x, y) in [(0.0, 0.0), (960.0, 540.0), (-13.5, 42.25), (4000.0, -250.0)] {
            let mapped_x = forward[0] * x + forward[1] * y + forward[2];
            let mapped_y = forward[3] * x + forward[4] * y + forward[5];
            let back_x = inverse[0] * mapped_x + inverse[1] * mapped_y + inverse[2];
            let back_y = inverse[3] * mapped_x + inverse[4] * mapped_y + inverse[5];
            assert!(
                (back_x - x).abs() <= 1.0e-9 && (back_y - y).abs() <= 1.0e-9,
                "round trip of ({x}, {y}) landed on ({back_x}, {back_y})"
            );
        }
    }

    #[test]
    fn gpu_controls_pack_the_inverse_matrix() {
        let graded = grade(
            [2.0, -3.0],
            [5.5, 0.5],
            140.0,
            95.0,
            105.0,
            18.0,
            60.0,
            215.0,
            40.0,
            TransformSampling::High,
        );
        let inverse = graded.inverse_matrix(640, 360).expect("the transform is invertible");
        let (params, extra) = graded.gpu_controls(640, 360).expect("packed controls");
        assert_eq!(
            params,
            [
                inverse[0] as f32,
                inverse[1] as f32,
                inverse[2] as f32,
                inverse[3] as f32,
            ]
        );
        assert_eq!(extra[0], inverse[4] as f32);
        assert_eq!(extra[1], inverse[5] as f32);
        assert_eq!(extra[2], 0.4);
        assert_eq!(extra[3], 4.0);
    }

    #[test]
    fn the_kernel_mixes_premultiplied_colour() {
        let width = 6_usize;
        let height = 3_usize;
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; width * height];
        pixels[width + 2] = [1.0, 0.0, 0.0, 1.0];
        pixels[width + 3] = [0.0, 1.0, 0.0, 0.0];
        let nudged = grade(
            [0.0, 0.0],
            [-0.5, 0.0],
            100.0,
            100.0,
            100.0,
            0.0,
            0.0,
            0.0,
            100.0,
            TransformSampling::Low,
        );
        let mut frame = pixels.clone();
        assert!(transform_frame_f32(&mut frame, width, height, &nudged));
        let mixed = at(&frame, width, 2, 1);
        // Straight-alpha averaging would leak the hidden green into the mix.
        assert!(
            mixed[1] <= 1.0e-6,
            "the hidden colour must not bleed into the visible pixel: {mixed:?}"
        );
        assert_pixel(mixed, [1.0, 0.0, 0.0, 0.5], "premultiplied mix");
    }

    #[test]
    fn the_sampling_menu_round_trips() {
        assert_eq!(TransformSampling::ALL.len(), 3);
        for (index, sampling) in TransformSampling::ALL.into_iter().enumerate() {
            assert_eq!(
                TransformSampling::from_key(sampling.as_key()),
                Some(sampling)
            );
            assert_eq!(sampling.as_u32() as usize, index);
            assert_eq!(sampling.as_menu(), index as u32 + 1);
        }
        assert_eq!(TransformSampling::Low.samples_per_axis(), 1);
        assert_eq!(TransformSampling::Normal.samples_per_axis(), 2);
        assert_eq!(TransformSampling::High.samples_per_axis(), 4);
        assert_eq!(TransformSampling::from_key("huge"), None);
    }

    #[test]
    fn every_out_of_range_value_is_rejected() {
        let valid = identity();
        let cases: [(TransformGrade, TransformError); 8] = [
            (
                TransformGrade {
                    anchor: [TRANSFORM_MAX_CENTRE_PIXELS + 1.0, 0.0],
                    ..valid
                },
                TransformError::Anchor,
            ),
            (
                TransformGrade { position: [0.0, f32::NAN], ..valid },
                TransformError::Position,
            ),
            (
                TransformGrade {
                    uniform_scale: TRANSFORM_MAX_SCALE_PERCENT + 1.0,
                    ..valid
                },
                TransformError::Scale,
            ),
            (
                TransformGrade { scale_height: f32::NEG_INFINITY, ..valid },
                TransformError::Scale,
            ),
            (
                TransformGrade {
                    skew_degrees: TRANSFORM_MAX_SKEW_DEGREES + 1.0,
                    ..valid
                },
                TransformError::Skew,
            ),
            (
                TransformGrade {
                    skew_axis_degrees: TRANSFORM_MAX_AXIS_DEGREES + 1.0,
                    ..valid
                },
                TransformError::SkewAxis,
            ),
            (
                TransformGrade {
                    rotation_degrees: TRANSFORM_MAX_ROTATION_DEGREES + 1.0,
                    ..valid
                },
                TransformError::Rotation,
            ),
            (
                TransformGrade { opacity_percent: 101.0, ..valid },
                TransformError::Opacity,
            ),
        ];
        for (invalid, expected) in cases {
            assert_eq!(invalid.validate(), Err(expected), "{expected}");
            assert!(!expected.to_string().is_empty());
        }
        assert_eq!(
            TransformGrade::new(
                [0.0, 0.0],
                [0.0, 0.0],
                100.0,
                100.0,
                100.0,
                0.0,
                0.0,
                0.0,
                100.0,
                true,
                TRANSFORM_MAX_SHUTTER_DEGREES + 1.0,
                TransformSampling::Low,
            ),
            Err(TransformError::ShutterAngle)
        );
    }
}
