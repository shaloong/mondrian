//! GEN-01 渐变 Ramp: a two-colour gradient generator that can either replace the
//! image or blend over it.
//!
//! # Documented reading
//!
//! The ramp is a fused point operation: every output pixel is a function of its
//! own position and colour, with no spatial pass and no extra pass per layer.
//! With `d = end - start` and pixel index `p = (column, row)`:
//!
//! - Linear: `t = clamp(dot(p - start, d) / dot(d, d), 0, 1)`.
//! - Radial: `t = clamp(length(p - start) / length(d), 0, 1)`.
//! - A degenerate ramp (`dot(d, d) <= 1e-12`) is a constant start-colour ramp:
//!   `t = 0` everywhere, so a zero-length axis never divides by zero.
//! - Scatter jitters the position before the clamp:
//!   `t' = clamp(t + (unit_noise(column, row) - 0.5) * (scatter / 255), 0, 1)`.
//!   `unit_noise` is the deterministic integer hash the deband kernel uses, so a
//!   re-render is bit-identical and the GPU shader can mirror it exactly. With
//!   `scatter = 0` the gradient is exact: the hash is not even sampled.
//!
//! # Colour and coverage
//!
//! The gradient is `mix(start_color, end_color, t')` in straight, non-clamped
//! f32, so HDR authoring colours survive. The result replaces the source by
//! `rgb = mix(gradient, source_rgb, blend_with_original)` and the ramp never
//! authors coverage: `alpha = source_alpha`. A fully transparent pixel
//! (`source_alpha <= 0.0`) is left untouched, which matches the compositor's
//! per-stage coverage guard that short-circuits a stage at zero coverage.
//!
//! `blend_with_original >= 1.0` is the identity: the ramp contributes nothing
//! and the graph builder elides it.

use crate::noise::noise_hash;

/// Number of declared [`RampShape`] options.
pub const RAMP_SHAPE_COUNT: usize = 2;
/// Largest accepted gradient scatter.
pub const RAMP_MAX_SCATTER: f32 = 512.0;
/// Largest accepted blend with the original image.
pub const RAMP_MAX_BLEND: f32 = 1.0;
/// Largest accepted absolute gradient centre coordinate, in pixels.
pub const RAMP_MAX_CENTRE_PIXELS: f32 = 1_000_000.0;
/// Salt of the deterministic scatter hash; shared with the GPU shader.
pub const RAMP_DITHER_SALT: u32 = 0x9E37_79B9;

/// A ramp whose squared axis length is at or below this value is degenerate.
const RAMP_DEGENERATE_LENGTH_SQUARED: f32 = 1e-12;

/// The gradient shape declared by the `shape` parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RampShape {
    /// A linear gradient projected onto the start-to-end axis.
    Linear,
    /// A radial gradient measured from `start`.
    Radial,
}

impl RampShape {
    /// Every declared shape, in presentation order.
    pub const ALL: [Self; RAMP_SHAPE_COUNT] = [Self::Linear, Self::Radial];

    /// The stable author key of this shape.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Radial => "radial",
        }
    }

    /// The shape declared by a stable author key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|shape| shape.as_key() == key)
    }

    /// The one-based menu ordinal of this shape.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        match self {
            Self::Linear => 1,
            Self::Radial => 2,
        }
    }

    /// The shape at a one-based menu ordinal.
    #[must_use]
    pub const fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Linear),
            2 => Some(Self::Radial),
            _ => None,
        }
    }

    /// The zero-based ordinal packed into the GPU uniform.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Linear => 0,
            Self::Radial => 1,
        }
    }
}

/// Why a [`RampGrade`] could not be authored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RampError {
    /// The start endpoint (position or colour) is not authorable.
    Start,
    /// The end endpoint (position or colour) is not authorable.
    End,
    /// The scatter is not a finite number inside `0..=512`.
    Scatter,
    /// The blend with the original image is not a finite number inside `0..=1`.
    Blend,
}

impl std::fmt::Display for RampError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Start => "ramp start must be a finite point inside [-1000000, 1000000]",
            Self::End => "ramp end must be a finite point inside [-1000000, 1000000]",
            Self::Scatter => "ramp scatter must be a finite number inside [0, 512]",
            Self::Blend => "ramp blend with original must be a finite number inside [0, 1]",
        })
    }
}

impl std::error::Error for RampError {}

/// A validated linear or radial two-colour gradient.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RampGrade {
    start: [f32; 2],
    start_color: [f32; 3],
    end: [f32; 2],
    end_color: [f32; 3],
    shape: RampShape,
    scatter: f32,
    blend_with_original: f32,
}

impl RampGrade {
    /// Validate and build a ramp grade.
    pub fn new(
        start: [f32; 2],
        start_color: [f32; 3],
        end: [f32; 2],
        end_color: [f32; 3],
        shape: RampShape,
        scatter: f32,
        blend_with_original: f32,
    ) -> Result<Self, RampError> {
        if !is_finite_point(start) || !is_finite_colour(start_color) {
            return Err(RampError::Start);
        }
        if !is_finite_point(end) || !is_finite_colour(end_color) {
            return Err(RampError::End);
        }
        if !scatter.is_finite() || !(0.0..=RAMP_MAX_SCATTER).contains(&scatter) {
            return Err(RampError::Scatter);
        }
        if !blend_with_original.is_finite()
            || !(0.0..=RAMP_MAX_BLEND).contains(&blend_with_original)
        {
            return Err(RampError::Blend);
        }
        Ok(Self {
            start,
            start_color,
            end,
            end_color,
            shape,
            scatter,
            blend_with_original,
        })
    }

    /// The gradient start point, in pixels.
    #[must_use]
    pub const fn start(self) -> [f32; 2] {
        self.start
    }

    /// The straight start colour.
    #[must_use]
    pub const fn start_color(self) -> [f32; 3] {
        self.start_color
    }

    /// The gradient end point, in pixels.
    #[must_use]
    pub const fn end(self) -> [f32; 2] {
        self.end
    }

    /// The straight end colour.
    #[must_use]
    pub const fn end_color(self) -> [f32; 3] {
        self.end_color
    }

    /// The declared gradient shape.
    #[must_use]
    pub const fn shape(self) -> RampShape {
        self.shape
    }

    /// The authored scatter.
    #[must_use]
    pub const fn scatter(self) -> f32 {
        self.scatter
    }

    /// The authored blend with the original image.
    #[must_use]
    pub const fn blend_with_original(self) -> f32 {
        self.blend_with_original
    }

    /// Whether this ramp contributes nothing to the image.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.blend_with_original >= RAMP_MAX_BLEND
    }

    /// The scattered, clamped gradient position at one pixel index.
    #[must_use]
    pub fn ramp_position(self, column: f32, row: f32) -> f32 {
        let delta = [self.end[0] - self.start[0], self.end[1] - self.start[1]];
        let length_squared = delta[0] * delta[0] + delta[1] * delta[1];
        let mut position = if length_squared <= RAMP_DEGENERATE_LENGTH_SQUARED {
            0.0
        } else {
            let offset = [column - self.start[0], row - self.start[1]];
            match self.shape {
                RampShape::Linear => (offset[0] * delta[0] + offset[1] * delta[1]) / length_squared,
                RampShape::Radial => {
                    (offset[0] * offset[0] + offset[1] * offset[1]).sqrt() / length_squared.sqrt()
                }
            }
        };
        if self.scatter > 0.0 {
            position += (ramp_unit_noise(column as u32, row as u32) - 0.5) * (self.scatter / 255.0);
        }
        position.clamp(0.0, 1.0)
    }

    /// The straight, non-clamped gradient colour at one position.
    #[must_use]
    pub fn colour_at(self, position: f32) -> [f32; 3] {
        [
            mix(self.start_color[0], self.end_color[0], position),
            mix(self.start_color[1], self.end_color[1], position),
            mix(self.start_color[2], self.end_color[2], position),
        ]
    }

    /// The four uniform cells this grade packs for the fused point pass.
    ///
    /// The cells are exactly:
    /// - `params`: `[start.x, start.y, end.x, end.y]`,
    /// - `color`: `[start_color.r, start_color.g, start_color.b, 0.0]`,
    /// - `extra0`: `[end_color.r, end_color.g, end_color.b, 0.0]`,
    /// - `extra1`: `[shape.as_u32() as f32, scatter, blend_with_original, 0.0]`.
    #[must_use]
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4]) {
        (
            [self.start[0], self.start[1], self.end[0], self.end[1]],
            [
                self.start_color[0],
                self.start_color[1],
                self.start_color[2],
                0.0,
            ],
            [self.end_color[0], self.end_color[1], self.end_color[2], 0.0],
            [
                self.shape.as_u32() as f32,
                self.scatter,
                self.blend_with_original,
                0.0,
            ],
        )
    }

    /// Apply the ramp to one straight-alpha pixel at one integer pixel index.
    #[must_use]
    pub fn apply(self, pixel: [f32; 4], column: u32, row: u32) -> [f32; 4] {
        if self.is_identity() || pixel[3] <= 0.0 {
            return pixel;
        }
        let gradient = self.colour_at(self.ramp_position(column as f32, row as f32));
        let blend = self.blend_with_original;
        [
            mix(gradient[0], pixel[0], blend),
            mix(gradient[1], pixel[1], blend),
            mix(gradient[2], pixel[2], blend),
            pixel[3],
        ]
    }
}

/// Apply a ramp grade to a whole straight-alpha f32 frame.
///
/// Returns `false` when the grade is the identity or the geometry does not
/// describe the frame exactly.
pub fn ramp_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &RampGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_ramp_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Apply a ramp grade while honouring cancellation checkpoints.
///
/// The returned `bool` reports whether the pass ran, not whether any pixel
/// changed: a geometry mismatch and the identity both report `false`.
pub(crate) fn apply_ramp_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &RampGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(expected) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || pixels.len() != expected {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(false);
    }
    let mut index = 0usize;
    for chunk in pixels.chunks_mut(CHECKPOINT_STRIDE) {
        checkpoint()?;
        for pixel in chunk.iter_mut() {
            let column = (index % width) as u32;
            let row = (index / width) as u32;
            *pixel = grade.apply(*pixel, column, row);
            index += 1;
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Pixels visited between two cancellation checkpoints.
const CHECKPOINT_STRIDE: usize = 4_096;

/// The deterministic unit noise the scatter dither is built on.
///
/// The value lands in `[0, 1)` and matches the WGSL mirror bit for bit.
fn ramp_unit_noise(column: u32, row: u32) -> f32 {
    (noise_hash(column, row, RAMP_DITHER_SALT) >> 8) as f32 / 16_777_216.0
}

/// Interpolate between two values, matching the WGSL `mix` shape.
fn mix(start: f32, end: f32, position: f32) -> f32 {
    start + (end - start) * position
}

/// Whether a point is finite and inside the authorable pixel square.
fn is_finite_point(point: [f32; 2]) -> bool {
    point.iter().all(|coordinate| {
        coordinate.is_finite()
            && (-RAMP_MAX_CENTRE_PIXELS..=RAMP_MAX_CENTRE_PIXELS).contains(coordinate)
    })
}

/// Whether a colour has three finite components.
fn is_finite_colour(colour: [f32; 3]) -> bool {
    colour.iter().all(|component| component.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(
        start: [f32; 2],
        start_color: [f32; 3],
        end: [f32; 2],
        end_color: [f32; 3],
        shape: RampShape,
        scatter: f32,
        blend: f32,
    ) -> RampGrade {
        RampGrade::new(start, start_color, end, end_color, shape, scatter, blend)
            .expect("valid ramp grade")
    }

    fn default_grade() -> RampGrade {
        grade(
            [128.0, 0.0],
            [0.0, 0.0, 0.0],
            [128.0, 256.0],
            [1.0, 1.0, 1.0],
            RampShape::Linear,
            0.0,
            0.0,
        )
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1.0e-5
    }

    fn close_pixel(a: [f32; 4], b: [f32; 4]) -> bool {
        a.iter().zip(b.iter()).all(|(left, right)| close(*left, *right))
    }

    #[test]
    fn blend_one_is_the_identity_and_leaves_the_frame_bit_identical() {
        let grade = grade(
            [0.0, 0.0],
            [1.0, 0.0, 0.0],
            [4.0, 0.0],
            [0.0, 1.0, 0.0],
            RampShape::Linear,
            64.0,
            1.0,
        );
        assert!(grade.is_identity());
        let mut pixels = [[0.25, 0.5, 0.75, 1.0], [1.5, -0.5, 2.0, 0.5]];
        let before = pixels;
        assert!(!ramp_frame_f32(&mut pixels, 2, 1, &grade));
        assert_eq!(
            pixels.map(|pixel| pixel.map(f32::to_bits)),
            before.map(|pixel| pixel.map(f32::to_bits))
        );
    }

    #[test]
    fn default_grade_paints_the_start_colour_at_row_zero() {
        let pixel = default_grade().apply([0.5, 0.5, 0.5, 1.0], 128, 0);
        assert!(close_pixel(pixel, [0.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn default_grade_paints_the_end_colour_at_row_256() {
        let pixel = default_grade().apply([0.5, 0.5, 0.5, 1.0], 128, 256);
        assert!(close_pixel(pixel, [1.0, 1.0, 1.0, 1.0]));
    }

    #[test]
    fn linear_projection_follows_the_axis() {
        let forward = default_grade();
        let reversed = grade(
            [128.0, 256.0],
            [0.0, 0.0, 0.0],
            [128.0, 0.0],
            [1.0, 1.0, 1.0],
            RampShape::Linear,
            0.0,
            0.0,
        );
        for row in [0u32, 64, 128, 192, 256] {
            let expected = forward.ramp_position(128.0, row as f32);
            let flipped = reversed.ramp_position(128.0, row as f32);
            assert!(close(expected + flipped, 1.0), "row {row}");
        }
        assert!(close(forward.ramp_position(128.0, 128.0), 0.5));
        assert!(close(forward.ramp_position(400.0, 128.0), 0.5));
    }

    #[test]
    fn radial_ramp_depends_only_on_distance_from_start() {
        let radial = grade(
            [100.0, 100.0],
            [0.0, 0.0, 0.0],
            [100.0, 200.0],
            [1.0, 1.0, 1.0],
            RampShape::Radial,
            0.0,
            0.0,
        );
        let left = radial.apply([0.0, 0.0, 0.0, 1.0], 140, 100);
        let down = radial.apply([0.0, 0.0, 0.0, 1.0], 100, 140);
        assert!(close_pixel(left, down));
        assert_eq!(
            radial.ramp_position(140.0, 100.0),
            radial.ramp_position(100.0, 140.0)
        );
        assert!(close(radial.ramp_position(100.0, 150.0), 0.5));
    }

    #[test]
    fn scatter_zero_is_exact() {
        let grade = default_grade();
        for row in [0u32, 1, 100, 256] {
            let expected = (row as f32 / 256.0).clamp(0.0, 1.0);
            assert!(close(grade.ramp_position(128.0, row as f32), expected));
        }
    }

    #[test]
    fn scatter_is_deterministic_across_two_runs() {
        let grade = grade(
            [0.0, 0.0],
            [0.0, 0.0, 0.0],
            [32.0, 0.0],
            [1.0, 1.0, 1.0],
            RampShape::Linear,
            200.0,
            0.0,
        );
        let mut first = [[0.3, 0.6, 0.9, 1.0]; 64];
        let mut second = first;
        assert!(ramp_frame_f32(&mut first, 8, 8, &grade));
        assert!(ramp_frame_f32(&mut second, 8, 8, &grade));
        assert_eq!(
            first.map(|pixel| pixel.map(f32::to_bits)),
            second.map(|pixel| pixel.map(f32::to_bits))
        );
    }

    #[test]
    fn scatter_changes_the_frame() {
        let exact = grade(
            [0.0, 0.0],
            [0.0, 0.0, 0.0],
            [32.0, 0.0],
            [1.0, 1.0, 1.0],
            RampShape::Linear,
            0.0,
            0.0,
        );
        let scattered = grade(
            [0.0, 0.0],
            [0.0, 0.0, 0.0],
            [32.0, 0.0],
            [1.0, 1.0, 1.0],
            RampShape::Linear,
            120.0,
            0.0,
        );
        let mut plain = [[0.5, 0.5, 0.5, 1.0]; 16];
        let mut jittered = plain;
        assert!(ramp_frame_f32(&mut plain, 4, 4, &exact));
        assert!(ramp_frame_f32(&mut jittered, 4, 4, &scattered));
        assert_ne!(plain, jittered);
    }

    #[test]
    fn alpha_is_preserved() {
        let pixel = default_grade().apply([0.25, 0.5, 0.75, 0.375], 128, 128);
        assert_eq!(pixel[3], 0.375);
    }

    #[test]
    fn a_transparent_pixel_is_left_untouched() {
        let pixel = [0.25, 0.5, 0.75, 0.0];
        assert_eq!(default_grade().apply(pixel, 128, 128), pixel);
        let mut pixels = [[1.0, 2.0, 3.0, -0.5]];
        assert!(ramp_frame_f32(&mut pixels, 1, 1, &default_grade()));
        assert_eq!(pixels[0], [1.0, 2.0, 3.0, -0.5]);
    }

    #[test]
    fn a_degenerate_ramp_paints_the_start_colour() {
        let grade = grade(
            [10.0, 20.0],
            [0.2, 0.4, 0.6],
            [10.0, 20.0],
            [1.0, 1.0, 1.0],
            RampShape::Radial,
            0.0,
            0.0,
        );
        for (column, row) in [(0u32, 0u32), (10, 20), (500, 700)] {
            assert_eq!(grade.ramp_position(column as f32, row as f32), 0.0);
            assert!(close_pixel(
                grade.apply([0.0, 0.0, 0.0, 1.0], column, row),
                [0.2, 0.4, 0.6, 1.0]
            ));
        }
    }

    #[test]
    fn out_of_range_parameters_are_rejected() {
        let ok = RampGrade::new(
            [0.0; 2],
            [0.0; 3],
            [4.0, 0.0],
            [1.0; 3],
            RampShape::Linear,
            0.0,
            0.0,
        );
        assert!(ok.is_ok());
        assert_eq!(
            RampGrade::new(
                [f32::NAN, 0.0],
                [0.0; 3],
                [4.0, 0.0],
                [1.0; 3],
                RampShape::Linear,
                0.0,
                0.0
            ),
            Err(RampError::Start)
        );
        assert_eq!(
            RampGrade::new(
                [0.0; 2],
                [f32::INFINITY, 0.0, 0.0],
                [4.0, 0.0],
                [1.0; 3],
                RampShape::Linear,
                0.0,
                0.0
            ),
            Err(RampError::Start)
        );
        assert_eq!(
            RampGrade::new(
                [0.0; 2],
                [0.0; 3],
                [RAMP_MAX_CENTRE_PIXELS + 1.0, 0.0],
                [1.0; 3],
                RampShape::Linear,
                0.0,
                0.0
            ),
            Err(RampError::End)
        );
        assert_eq!(
            RampGrade::new(
                [0.0; 2],
                [0.0; 3],
                [4.0, 0.0],
                [1.0; 3],
                RampShape::Linear,
                RAMP_MAX_SCATTER + 1.0,
                0.0
            ),
            Err(RampError::Scatter)
        );
        assert_eq!(
            RampGrade::new(
                [0.0; 2],
                [0.0; 3],
                [4.0, 0.0],
                [1.0; 3],
                RampShape::Linear,
                0.0,
                -0.5
            ),
            Err(RampError::Blend)
        );
        assert_eq!(
            RampGrade::new(
                [0.0; 2],
                [0.0; 3],
                [4.0, 0.0],
                [1.0; 3],
                RampShape::Linear,
                f32::NAN,
                0.0
            ),
            Err(RampError::Scatter)
        );
        assert_eq!(
            RampGrade::new(
                [0.0; 2],
                [0.0; 3],
                [4.0, 0.0],
                [1.0; 3],
                RampShape::Linear,
                0.0,
                f32::INFINITY
            ),
            Err(RampError::Blend)
        );
    }

    #[test]
    fn gpu_controls_pack_the_documented_cells() {
        let grade = grade(
            [1.5, -2.5],
            [0.1, 0.2, 0.3],
            [30.0, 40.0],
            [0.4, 0.5, 0.6],
            RampShape::Radial,
            12.5,
            0.25,
        );
        let (params, color, extra0, extra1) = grade.gpu_controls();
        assert_eq!(params, [1.5, -2.5, 30.0, 40.0]);
        assert_eq!(color, [0.1, 0.2, 0.3, 0.0]);
        assert_eq!(extra0, [0.4, 0.5, 0.6, 0.0]);
        assert_eq!(extra1, [1.0, 12.5, 0.25, 0.0]);
    }

    #[test]
    fn a_one_pixel_frame_works() {
        let mut pixels = [[0.5, 0.5, 0.5, 1.0]];
        assert!(ramp_frame_f32(&mut pixels, 1, 1, &default_grade()));
        assert!(close_pixel(pixels[0], [0.0, 0.0, 0.0, 1.0]));
        assert!(!ramp_frame_f32(&mut pixels, 0, 1, &default_grade()));
        assert!(!ramp_frame_f32(&mut pixels, 2, 1, &default_grade()));
    }

    #[test]
    fn the_checkpoint_stride_bounds_cancellation_latency() {
        let mut pixels = vec![[0.5, 0.5, 0.5, 1.0]; 4];
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_ramp_f32_controlled(&mut pixels, 2, 2, &default_grade(), &mut checkpoint)
                .expect("infallible")
        );
        assert_eq!(calls, 2);

        let mut pixels = vec![[0.5, 0.5, 0.5, 1.0]; CHECKPOINT_STRIDE + 1];
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(apply_ramp_f32_controlled(
            &mut pixels,
            1,
            CHECKPOINT_STRIDE + 1,
            &default_grade(),
            &mut checkpoint
        )
        .expect("infallible"));
        assert_eq!(calls, 3);
    }
}
