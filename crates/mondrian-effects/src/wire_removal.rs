//! Wire removal: clone pixels from a parallel displacement across an authored line.
//!
//! The effect list names this entry `祛线` (wire removal) and gives it two points, a
//! style, a thickness, a slope, a mirror blend and a frame offset. The effect exists
//! to make a rigging wire or cable disappear: the author marks the line the wire
//! crosses, and every pixel inside a band around that line is replaced by a sample
//! taken from a parallel displacement across it, so the background on both sides of
//! the wire supplies clean content.
//!
//! # Band
//!
//! The directed line runs from `point_a` to `point_b`; its normal points to the
//! left of that direction (the same convention as [`crate::SplitGrade`]). For a
//! pixel `p` the signed distance is `s = (p - A) · n`, positive on the normal side.
//! A pixel belongs to the band when `|s| <= Thickness / 2`, a closed interval: for
//! `Thickness = 2` the pixel centres with `s ∈ {-1, 0, 1}` are patched, three rows
//! for a horizontal line. Outside the band the frame is left bit for bit alone.
//!
//! This is an infinite slab, not a segment: membership only tests the perpendicular
//! distance, so the parts of the line beyond `point_a` and `point_b` are patched as
//! well. Real wires do not stop at the marked points, and an end-capped segment
//! would leave a visible stub at each end. The cost is that the author must shorten
//! the line when only its middle section should be repaired.
//!
//! # Displacement
//!
//! The clone offset is `u * D` with `k = 2 * Slope - 1`, `u = normalize(n + tang * k)`
//! and `D = Thickness` **in pixels**. `Slope` only rotates `u` through at most ±45°
//! and never changes the length of the displacement, and `D` is never multiplied by
//! the frame width or height: the default `Thickness = 2` moves the sample by
//! exactly two pixels on every frame size. Code review guard: `width`, `height`,
//! `project_geometry` and the pixel aspect ratio must not appear on the `u * D`
//! side, and `Thickness` must reach the kernel only as `T / 2` (the band) and `D`
//! (the displacement).
//!
//! # Styles and mirror blend
//!
//! `Left` samples `p + u * D`, `Right` samples `p - u * D`, `Both` averages one tap
//! from each side in pre-multiplied space, and `Cross` samples the side opposite the
//! pixel's own (`sigma = +1` when `s >= 0`, else `-1`).
//!
//! `MirrorBlend` blends the *sample position*, not the colour: the mirrored position
//! is `m = p - 2 * s * n` and the final target is `q + (m - q) * MirrorBlend`. That
//! keeps the tap count at one (two for `Both`) while still sliding continuously to a
//! pure mirror image. Zero is a pure clone, one a pure mirror. One known consequence
//! of the definition, documented rather than fixed: the centre row `s = 0` mirrors
//! onto itself, so with the default blend a quarter of the original pixel survives
//! on that row. It is not a bug; set `MirrorBlend = 0` to remove it.
//!
//! # Edges and frame offset
//!
//! The band edge is a hard threshold with no feather, so a pixel whose `s` lands
//! exactly on `Thickness / 2` may resolve differently on the CPU and the GPU; parity
//! tests avoid the boundary row, like the hard seam of the split effect. Sampling
//! clamps to the frame edge and never wraps or invents coverage. A degenerate line
//! (`A == B`) or `Thickness <= 0` is the identity.
//!
//! `frame_offset` is validated and stored but v1 never samples it: the builder has no
//! frame rate, the band mask has no carrier node in the render graph, and a temporal
//! tap would push the effect onto a pure CPU temporal graph. The parameter exists so
//! the authored schema is complete and survives project round-tripping; the kernel
//! and the shader both ignore it.

use crate::parallel::for_each_row_band;
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest band thickness the author surface can express, in pixels.
pub const WIRE_REMOVAL_MAX_THICKNESS: f32 = 100.0;
/// Largest absolute coordinate the author surface accepts for either point.
pub const WIRE_REMOVAL_MAX_COORDINATE: f32 = 1_000_000.0;
/// Largest absolute frame offset the author surface accepts, in frames.
pub const WIRE_REMOVAL_MAX_FRAME_OFFSET: i32 = 120;

/// Shortest line that still defines a direction, in pixels.
const GEOMETRY_EPSILON: f64 = 1.0e-6;

/// Which side of the line supplies the clone samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalStyle {
    /// Sample `p + u * D`, the normal side of the line.
    Left,
    /// Sample `p - u * D`, the side opposite the normal.
    Right,
    /// Average one tap from each side.
    Both,
    /// Sample the side opposite the pixel's own side.
    Cross,
}

impl RemovalStyle {
    /// Every style in authoring menu order.
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Both, Self::Cross];

    /// Stable persistence key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Both => "both",
            Self::Cross => "cross",
        }
    }

    /// Parses a persistence key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            "both" => Some(Self::Both),
            "cross" => Some(Self::Cross),
            _ => None,
        }
    }

    /// Zero-based value used by uniforms and hashes.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Both => 2,
            Self::Cross => 3,
        }
    }

    /// One-based authoring menu value.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        self.as_u32() as i64 + 1
    }

    /// Parses a one-based authoring menu value.
    #[must_use]
    pub fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Left),
            2 => Some(Self::Right),
            3 => Some(Self::Both),
            4 => Some(Self::Cross),
            _ => None,
        }
    }

    /// Menu label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "左侧",
            Self::Right => "右侧",
            Self::Both => "双侧",
            Self::Cross => "交叉",
        }
    }
}

/// Failures raised when validating a wire removal grade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireRemovalError {
    /// One of the two line points is not finite or lies outside the declared range.
    Point,
    /// The band thickness is not finite or lies outside the declared range.
    Thickness,
    /// The slope is not finite or lies outside the declared range.
    Slope,
    /// The mirror blend is not finite or lies outside the declared range.
    MirrorBlend,
    /// The frame offset lies outside the declared range.
    FrameOffset,
}

impl std::fmt::Display for WireRemovalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Point => formatter.write_str("wire removal line point is outside the range"),
            Self::Thickness => {
                formatter.write_str("wire removal thickness is outside the declared range")
            }
            Self::Slope => formatter.write_str("wire removal slope is outside the declared range"),
            Self::MirrorBlend => {
                formatter.write_str("wire removal mirror blend is outside the declared range")
            }
            Self::FrameOffset => {
                formatter.write_str("wire removal frame offset is outside the declared range")
            }
        }
    }
}

impl std::error::Error for WireRemovalError {}

/// A validated wire removal grade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WireRemovalGrade {
    point_a: [f32; 2],
    point_b: [f32; 2],
    style: RemovalStyle,
    thickness: f32,
    slope: f32,
    mirror_blend: f32,
    frame_offset: i32,
}

/// Per-pixel geometry shared by every removal style.
struct WireRemovalGeometry {
    pixel: [f64; 2],
    signed: f64,
    mirrored: [f64; 2],
    direction: [f64; 2],
    displacement: f64,
}

impl WireRemovalGrade {
    /// Validates the authored line, style and numeric controls.
    pub fn new(
        point_a: [f32; 2],
        point_b: [f32; 2],
        style: RemovalStyle,
        thickness: f32,
        slope: f32,
        mirror_blend: f32,
        frame_offset: i32,
    ) -> Result<Self, WireRemovalError> {
        for point in [point_a, point_b] {
            for coordinate in point {
                if !coordinate.is_finite() || coordinate.abs() > WIRE_REMOVAL_MAX_COORDINATE {
                    return Err(WireRemovalError::Point);
                }
            }
        }
        if !thickness.is_finite() || !(0.0..=WIRE_REMOVAL_MAX_THICKNESS).contains(&thickness) {
            return Err(WireRemovalError::Thickness);
        }
        if !slope.is_finite() || !(0.0..=1.0).contains(&slope) {
            return Err(WireRemovalError::Slope);
        }
        if !mirror_blend.is_finite() || !(0.0..=1.0).contains(&mirror_blend) {
            return Err(WireRemovalError::MirrorBlend);
        }
        if !(-WIRE_REMOVAL_MAX_FRAME_OFFSET..=WIRE_REMOVAL_MAX_FRAME_OFFSET).contains(&frame_offset)
        {
            return Err(WireRemovalError::FrameOffset);
        }
        Ok(Self {
            point_a,
            point_b,
            style,
            thickness,
            slope,
            mirror_blend,
            frame_offset,
        })
    }

    /// First point of the authored line.
    #[must_use]
    pub fn point_a(&self) -> [f32; 2] {
        self.point_a
    }

    /// Second point of the authored line.
    #[must_use]
    pub fn point_b(&self) -> [f32; 2] {
        self.point_b
    }

    /// Which side of the line supplies the clone samples.
    #[must_use]
    pub fn style(&self) -> RemovalStyle {
        self.style
    }

    /// Band thickness in pixels.
    #[must_use]
    pub fn thickness(&self) -> f32 {
        self.thickness
    }

    /// Clone slope in `[0, 1]`, where `0.5` leaves the displacement along the normal.
    #[must_use]
    pub fn slope(&self) -> f32 {
        self.slope
    }

    /// Position blend between the clone and the mirrored position.
    #[must_use]
    pub fn mirror_blend(&self) -> f32 {
        self.mirror_blend
    }

    /// Authored frame offset.
    ///
    /// v1 never samples it; the getter exists so the author surface can round-trip
    /// the value and so the tests can pin that round trip.
    #[must_use]
    pub fn frame_offset(&self) -> i32 {
        self.frame_offset
    }

    /// Whether the grade leaves the frame untouched.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.thickness <= 0.0 || self.unit_frame().is_none()
    }

    /// Unit tangent and left normal of the authored line, or `None` for a
    /// degenerate line.
    #[must_use]
    fn unit_frame(&self) -> Option<([f64; 2], [f64; 2])> {
        let delta_x = f64::from(self.point_b[0]) - f64::from(self.point_a[0]);
        let delta_y = f64::from(self.point_b[1]) - f64::from(self.point_a[1]);
        let length = delta_x.hypot(delta_y);
        if !length.is_finite() || length <= GEOMETRY_EPSILON {
            return None;
        }
        let tangent = [delta_x / length, delta_y / length];
        let normal = [-delta_y / length, delta_x / length];
        Some((tangent, normal))
    }

    /// Shared geometry of one pixel, or `None` outside the band.
    fn geometry(
        &self,
        column: usize,
        row: usize,
        tangent: [f64; 2],
        normal: [f64; 2],
    ) -> Option<WireRemovalGeometry> {
        let pixel = [f64::from(column as f32), f64::from(row as f32)];
        let offset = [
            pixel[0] - f64::from(self.point_a[0]),
            pixel[1] - f64::from(self.point_a[1]),
        ];
        let signed = offset[0] * normal[0] + offset[1] * normal[1];
        let thickness = f64::from(self.thickness);
        if signed.abs() > thickness * 0.5 || signed.is_nan() {
            return None;
        }
        // `k` only rotates the displacement: `u` is normalised, so the offset is
        // always exactly `thickness` pixels long no matter what the slope is.
        let k = 2.0 * f64::from(self.slope) - 1.0;
        let direction = [normal[0] + tangent[0] * k, normal[1] + tangent[1] * k];
        let magnitude = direction[0].hypot(direction[1]);
        let direction = [direction[0] / magnitude, direction[1] / magnitude];
        // The displacement is the authored thickness in pixels. It must never be
        // scaled by the frame size; the assertion is the guard for future edits.
        let displacement = thickness;
        debug_assert!((displacement - thickness).abs() <= f64::from(f32::EPSILON));
        let mirrored = [
            pixel[0] - 2.0 * signed * normal[0],
            pixel[1] - 2.0 * signed * normal[1],
        ];
        Some(WireRemovalGeometry { pixel, signed, mirrored, direction, displacement })
    }

    /// Blends the clone position on one side with the mirrored position.
    fn blend_target(&self, geometry: &WireRemovalGeometry, side: f64) -> [f64; 2] {
        let clone = [
            geometry.pixel[0] + side * geometry.direction[0] * geometry.displacement,
            geometry.pixel[1] + side * geometry.direction[1] * geometry.displacement,
        ];
        let blend = f64::from(self.mirror_blend);
        let target = [
            clone[0] + (geometry.mirrored[0] - clone[0]) * blend,
            clone[1] + (geometry.mirrored[1] - clone[1]) * blend,
        ];
        debug_assert!(target[0].is_finite() && target[1].is_finite());
        target
    }

    /// Sample position of one pixel, or `[f64::NAN; 2]` outside the band.
    fn sample_position(
        &self,
        column: usize,
        row: usize,
        tangent: [f64; 2],
        normal: [f64; 2],
    ) -> [f64; 2] {
        let Some(geometry) = self.geometry(column, row, tangent, normal) else {
            return [f64::NAN; 2];
        };
        let side = match self.style {
            RemovalStyle::Left | RemovalStyle::Both => 1.0,
            RemovalStyle::Right => -1.0,
            // `s == 0` counts as the positive side, matching the split tie rule.
            RemovalStyle::Cross => {
                if geometry.signed >= 0.0 {
                    -1.0
                } else {
                    1.0
                }
            }
        };
        self.blend_target(&geometry, side)
    }

    /// Both sample positions of a `Both` pixel, or `None` outside the band.
    fn sample_both(
        &self,
        column: usize,
        row: usize,
        tangent: [f64; 2],
        normal: [f64; 2],
    ) -> Option<([f64; 2], [f64; 2])> {
        let geometry = self.geometry(column, row, tangent, normal)?;
        Some((
            self.blend_target(&geometry, 1.0),
            self.blend_target(&geometry, -1.0),
        ))
    }

    /// Applies the grade to every pixel of a pre-multiplied frame region.
    ///
    /// Rows are independent because the kernel reads a snapshot of the source, so
    /// the band helper is free to spread them over worker threads. Pixels outside
    /// the band are skipped instead of written, which keeps them bit for bit
    /// identical to the input.
    pub(crate) fn apply(&self, frame: &mut [[f32; 4]], width: usize, height: usize) {
        let Some((tangent, normal)) = self.unit_frame() else {
            return;
        };
        let source = frame.to_vec();
        let source_view: &[[f32; 4]] = &source;
        for_each_row_band(frame, width, 0..height, |first_row, band| {
            for (offset, row_pixels) in band.chunks_mut(width).enumerate() {
                let row = first_row + offset;
                for (column, destination) in row_pixels.iter_mut().enumerate() {
                    if self.style == RemovalStyle::Both {
                        let Some((first, second)) = self.sample_both(column, row, tangent, normal)
                        else {
                            continue;
                        };
                        let first =
                            sample_bilinear_clamped(source_view, width, height, first[0], first[1]);
                        let second = sample_bilinear_clamped(
                            source_view,
                            width,
                            height,
                            second[0],
                            second[1],
                        );
                        let mut sample = [0.0_f32; 4];
                        for (channel, value) in sample.iter_mut().enumerate() {
                            *value = 0.5 * (first[channel] + second[channel]);
                        }
                        *destination = sample;
                        continue;
                    }
                    let target = self.sample_position(column, row, tangent, normal);
                    if !target[0].is_finite() {
                        continue;
                    }
                    *destination =
                        sample_bilinear_clamped(source_view, width, height, target[0], target[1]);
                }
            }
        });
    }

    /// Line, controls and style packed for the GPU pass.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.point_a[0],
                self.point_a[1],
                self.point_b[0],
                self.point_b[1],
            ],
            [
                self.thickness,
                self.slope,
                self.mirror_blend,
                self.style.as_u32() as f32,
            ],
        )
    }
}

/// Applies a wire removal grade to a whole straight-alpha frame.
pub fn wire_removal_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: WireRemovalGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_wire_removal_f32_controlled(pixels, width, height, grade, &mut checkpoint)
        .unwrap_or(false)
}

/// Applies a wire removal grade to a frame slice with checkpointing.
pub(crate) fn apply_wire_removal_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: WireRemovalGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(expected) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || expected != pixels.len() || grade.is_identity() {
        return Ok(false);
    }
    premultiply_rgba_controlled(pixels, checkpoint)?;
    // The band pass runs on parallel row bands, so a `&mut` checkpoint cannot be
    // shared with the workers; the premultiply and unpremultiply passes already
    // report every `CHECKPOINT_STRIDE` pixels, and the two boundaries bracket the
    // pass itself.
    checkpoint()?;
    grade.apply(pixels, width, height);
    checkpoint()?;
    unpremultiply_rgba_controlled(pixels, checkpoint)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
    const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; width * height];
        for row in 0..height {
            for column in 0..width {
                let value = 0.1 * (column + row) as f32;
                pixels[row * width + column] = [value, 1.0 - value * 0.5, value * 0.25, 1.0];
            }
        }
        pixels
    }

    fn assert_pixel_close(actual: [f32; 4], expected: [f32; 4], tolerance: f32) {
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= tolerance,
                "channel {channel}: {actual:?} vs {expected:?}"
            );
        }
    }

    fn horizontal(style: RemovalStyle) -> WireRemovalGrade {
        WireRemovalGrade::new([0.0, 4.0], [10.0, 4.0], style, 2.0, 0.5, 0.0, 0).expect("grade")
    }

    #[test]
    fn only_the_band_rows_are_rewritten() {
        let grade = horizontal(RemovalStyle::Left);
        let mut pixels = frame(11, 9);
        let original = pixels.clone();
        assert!(wire_removal_frame_f32(&mut pixels, 11, 9, grade));
        for row in 0..9 {
            for column in 0..11 {
                let index = row * 11 + column;
                if (3..=5).contains(&row) {
                    assert_ne!(pixels[index], original[index], "band pixel {row},{column}");
                } else {
                    assert_eq!(
                        pixels[index], original[index],
                        "outside pixel {row},{column}"
                    );
                }
            }
        }
    }

    #[test]
    fn pixels_outside_the_band_are_bit_identical() {
        let mut pixels = vec![[0.0_f32; 4]; 7 * 7];
        for row in 0..7 {
            for column in 0..7 {
                // Opaque pixels keep the frame-wide premultiply and unpremultiply
                // exactly invertible. With alpha < 1 the round trip is only accurate
                // to one ulp, which would weaken this bit-identity assertion even
                // though the band pass itself never touches these pixels.
                pixels[row * 7 + column] = if row < 4 {
                    [0.0, 0.0, 0.0, 0.0]
                } else {
                    let value = 0.05 * (column + row) as f32;
                    [value, 1.0 - value, 0.5 * value, 1.0]
                };
            }
        }
        let grade = WireRemovalGrade::new(
            [0.0, 1.0],
            [6.0, 1.0],
            RemovalStyle::Both,
            2.0,
            0.5,
            0.25,
            0,
        )
        .expect("grade");
        let original = pixels.clone();
        assert!(wire_removal_frame_f32(&mut pixels, 7, 7, grade));
        for row in 3..7 {
            for column in 0..7 {
                assert_eq!(
                    pixels[row * 7 + column],
                    original[row * 7 + column],
                    "outside pixel {row},{column}"
                );
            }
        }
    }

    #[test]
    fn zero_thickness_is_identity() {
        let grade = WireRemovalGrade::new(
            [0.0, 0.0],
            [10.0, 0.0],
            RemovalStyle::Both,
            0.0,
            0.5,
            0.25,
            5,
        )
        .expect("grade");
        assert!(grade.is_identity());
        let mut pixels = frame(9, 9);
        let original = pixels.clone();
        assert!(!wire_removal_frame_f32(&mut pixels, 9, 9, grade));
        assert_eq!(pixels, original);
    }

    #[test]
    fn a_degenerate_line_is_identity() {
        let grade = WireRemovalGrade::new(
            [4.0, 4.0],
            [4.0, 4.0],
            RemovalStyle::Left,
            20.0,
            0.5,
            0.25,
            0,
        )
        .expect("grade");
        assert!(grade.is_identity());
        let mut pixels = frame(9, 9);
        let original = pixels.clone();
        assert!(!wire_removal_frame_f32(&mut pixels, 9, 9, grade));
        assert_eq!(pixels, original);
    }

    #[test]
    fn slope_half_displaces_along_the_normal_by_the_thickness() {
        let grade = WireRemovalGrade::new(
            [0.0, 0.0],
            [10.0, 0.0],
            RemovalStyle::Left,
            3.0,
            0.5,
            0.0,
            0,
        )
        .expect("grade");
        let (tangent, normal) = grade.unit_frame().expect("frame");
        assert!(normal[0].abs() <= 1.0e-12 && (normal[1] - 1.0).abs() <= 1.0e-12);
        let target = grade.sample_position(4, 0, tangent, normal);
        assert!((target[0] - 4.0).abs() <= 1.0e-12, "{target:?}");
        assert!((target[1] - 3.0).abs() <= 1.0e-12, "{target:?}");
    }

    #[test]
    fn the_slope_rotates_the_displacement_without_changing_its_length() {
        for (slope, expected_along) in [(0.0, -1.0), (1.0, 1.0)] {
            let grade = WireRemovalGrade::new(
                [0.0, 0.0],
                [10.0, 0.0],
                RemovalStyle::Left,
                4.0,
                slope,
                0.0,
                0,
            )
            .expect("grade");
            let (tangent, normal) = grade.unit_frame().expect("frame");
            let target = grade.sample_position(0, 0, tangent, normal);
            let along = target[0] * tangent[0] + target[1] * tangent[1];
            let across = target[0] * normal[0] + target[1] * normal[1];
            let length = target[0].hypot(target[1]);
            assert!((length - 4.0).abs() <= 1.0e-12, "slope {slope}: {target:?}");
            let diagonal = 4.0 / 2.0_f64.sqrt();
            assert!((along - expected_along * diagonal).abs() <= 1.0e-12);
            assert!((across - diagonal).abs() <= 1.0e-12);
        }
    }

    #[test]
    fn the_clone_displacement_is_the_thickness_in_pixels() {
        // The same two-pixel displacement on two different frame widths pins that
        // the geometry never multiplies the thickness by the frame size.
        for width in [20_usize, 200] {
            let grade = WireRemovalGrade::new(
                [0.0, 4.0],
                [10.0, 4.0],
                RemovalStyle::Left,
                2.0,
                0.5,
                0.0,
                0,
            )
            .expect("grade");
            let mut pixels = frame(width, 9);
            let original = pixels.clone();
            assert!(wire_removal_frame_f32(&mut pixels, width, 9, grade));
            for column in 0..width {
                assert_pixel_close(
                    pixels[4 * width + column],
                    original[6 * width + column],
                    1.0e-6,
                );
                assert_pixel_close(
                    pixels[3 * width + column],
                    original[5 * width + column],
                    1.0e-6,
                );
            }
        }
    }

    #[test]
    fn the_four_styles_pick_the_documented_sides() {
        let mut pixels = vec![RED; 9 * 9];
        for column in 0..9 {
            pixels[4 * 9 + column] = GREEN;
            for row in 5..9 {
                pixels[row * 9 + column] = BLUE;
            }
        }
        let original = pixels.clone();
        let mut left = pixels.clone();
        assert!(wire_removal_frame_f32(
            &mut left,
            9,
            9,
            horizontal(RemovalStyle::Left)
        ));
        let mut right = pixels.clone();
        assert!(wire_removal_frame_f32(
            &mut right,
            9,
            9,
            horizontal(RemovalStyle::Right)
        ));
        let mut both = pixels.clone();
        assert!(wire_removal_frame_f32(
            &mut both,
            9,
            9,
            horizontal(RemovalStyle::Both)
        ));
        let mut cross = pixels.clone();
        assert!(wire_removal_frame_f32(
            &mut cross,
            9,
            9,
            horizontal(RemovalStyle::Cross)
        ));
        // The normal of a left-to-right line points down, so `Left` samples two
        // rows below the band, `Right` two rows above it.
        for row in 3..=5 {
            for column in 0..9 {
                assert_pixel_close(left[row * 9 + column], BLUE, 1.0e-6);
                assert_pixel_close(right[row * 9 + column], RED, 1.0e-6);
                assert_pixel_close(both[row * 9 + column], [0.5, 0.0, 0.5, 1.0], 1.0e-6);
            }
        }
        // `Cross` takes the opposite side: above the line it clones from below,
        // below the line it clones from above, and the axis row (`s = 0`) counts as
        // the positive side, so it clones from above like the pixel below it.
        for column in 0..9 {
            assert_pixel_close(cross[3 * 9 + column], BLUE, 1.0e-6);
            assert_pixel_close(cross[4 * 9 + column], RED, 1.0e-6);
            assert_pixel_close(cross[5 * 9 + column], RED, 1.0e-6);
        }
        assert_eq!(pixels, original);
    }

    #[test]
    fn menu_values_round_trip_through_the_enum() {
        for (menu, style) in [
            (1_i64, RemovalStyle::Left),
            (2, RemovalStyle::Right),
            (3, RemovalStyle::Both),
            (4, RemovalStyle::Cross),
        ] {
            assert_eq!(RemovalStyle::from_menu(menu), Some(style));
            assert_eq!(style.as_menu(), menu);
            assert_eq!(RemovalStyle::from_key(style.as_key()), Some(style));
            assert_eq!(style.as_u32(), (menu - 1) as u32);
        }
        assert_eq!(RemovalStyle::from_menu(0), None);
        assert_eq!(RemovalStyle::from_menu(5), None);
        assert_eq!(RemovalStyle::from_key("neither"), None);
        // The effect list's numeric default is 3, which is the both-sides style.
        assert_eq!(RemovalStyle::from_menu(3), Some(RemovalStyle::Both));
    }

    #[test]
    fn mirror_blend_zero_clones_and_one_mirrors() {
        let target = |blend| {
            let grade = WireRemovalGrade::new(
                [0.0, 0.0],
                [10.0, 0.0],
                RemovalStyle::Left,
                6.0,
                0.5,
                blend,
                0,
            )
            .expect("grade");
            let (tangent, normal) = grade.unit_frame().expect("frame");
            grade.sample_position(2, 2, tangent, normal)
        };
        let clone = target(0.0);
        assert!((clone[0] - 2.0).abs() <= 1.0e-12 && (clone[1] - 8.0).abs() <= 1.0e-12);
        let mirrored = target(1.0);
        assert!((mirrored[0] - 2.0).abs() <= 1.0e-12 && (mirrored[1] + 2.0).abs() <= 1.0e-12);
    }

    #[test]
    fn the_centre_row_keeps_a_quarter_of_the_original_pixel() {
        // Thickness 1 makes the band exactly the axis row and the clone offset one
        // pixel, so the mirrored position (the pixel itself) keeps a quarter.
        let grade = WireRemovalGrade::new(
            [0.0, 4.0],
            [8.0, 4.0],
            RemovalStyle::Left,
            1.0,
            0.5,
            0.25,
            0,
        )
        .expect("grade");
        let mut pixels = frame(9, 9);
        let original = pixels.clone();
        assert!(wire_removal_frame_f32(&mut pixels, 9, 9, grade));
        for column in 0..9 {
            let mut expected = [0.0_f32; 4];
            for (channel, value) in expected.iter_mut().enumerate() {
                *value = 0.25 * original[4 * 9 + column][channel]
                    + 0.75 * original[5 * 9 + column][channel];
            }
            assert_pixel_close(pixels[4 * 9 + column], expected, 1.0e-6);
            // The neighbouring row is outside the band and must not move.
            assert_eq!(pixels[5 * 9 + column], original[5 * 9 + column]);
        }
    }

    #[test]
    fn sampling_clamps_to_the_frame_border() {
        // Every pixel is inside the hundred-pixel band and the clone target sits a
        // hundred rows above the frame, so every tap clamps to the top border.
        let grade = WireRemovalGrade::new(
            [0.0, 1.0],
            [8.0, 1.0],
            RemovalStyle::Right,
            100.0,
            0.5,
            0.0,
            0,
        )
        .expect("grade");
        let mut pixels = frame(9, 9);
        let original = pixels.clone();
        assert!(wire_removal_frame_f32(&mut pixels, 9, 9, grade));
        for row in 0..9 {
            for column in 0..9 {
                let pixel = pixels[row * 9 + column];
                assert!(pixel.iter().all(|value| value.is_finite()), "{pixel:?}");
                assert_pixel_close(pixel, original[column], 1.0e-6);
            }
        }
    }

    #[test]
    fn frame_offsets_round_trip_and_are_bounded() {
        let grade = WireRemovalGrade::new(
            [1.0, 2.0],
            [3.0, 4.0],
            RemovalStyle::Cross,
            7.5,
            0.25,
            0.75,
            5,
        )
        .expect("grade");
        assert_eq!(grade.frame_offset(), 5);
        assert_eq!(grade.point_a(), [1.0, 2.0]);
        assert_eq!(grade.point_b(), [3.0, 4.0]);
        assert_eq!(grade.style(), RemovalStyle::Cross);
        assert_eq!(grade.thickness(), 7.5);
        assert_eq!(grade.slope(), 0.25);
        assert_eq!(grade.mirror_blend(), 0.75);
        let (params, extra) = grade.gpu_controls();
        assert_eq!(params, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(extra, [7.5, 0.25, 0.75, 3.0]);
        assert_eq!(
            WireRemovalGrade::new(
                [0.0, 0.0],
                [1.0, 0.0],
                RemovalStyle::Left,
                2.0,
                0.5,
                0.25,
                121
            ),
            Err(WireRemovalError::FrameOffset)
        );
        assert_eq!(
            WireRemovalGrade::new(
                [0.0, 0.0],
                [1.0, 0.0],
                RemovalStyle::Left,
                2.0,
                0.5,
                0.25,
                -121
            ),
            Err(WireRemovalError::FrameOffset)
        );
        assert!(WireRemovalGrade::new(
            [0.0, 0.0],
            [1.0, 0.0],
            RemovalStyle::Left,
            2.0,
            0.5,
            0.25,
            120
        )
        .is_ok());
    }

    #[test]
    fn row_band_parallelism_matches_the_serial_reference() {
        let grade = WireRemovalGrade::new(
            [0.0, 0.0],
            [63.0, 40.0],
            RemovalStyle::Left,
            12.0,
            0.8,
            0.25,
            0,
        )
        .expect("grade");
        let original = frame(64, 64);
        let mut parallel = original.clone();
        grade.apply(&mut parallel, 64, 64);
        let (tangent, normal) = grade.unit_frame().expect("frame");
        let mut serial = original.clone();
        for row in 0..64 {
            for column in 0..64 {
                let target = grade.sample_position(column, row, tangent, normal);
                if !target[0].is_finite() {
                    continue;
                }
                serial[row * 64 + column] =
                    sample_bilinear_clamped(&original, 64, 64, target[0], target[1]);
            }
        }
        assert_eq!(parallel, serial);
    }

    #[test]
    fn premultiply_and_unpremultiply_are_applied_once() {
        let grade =
            WireRemovalGrade::new([0.0, 0.0], [3.0, 0.0], RemovalStyle::Both, 2.0, 0.5, 0.0, 0)
                .expect("grade");
        let mut pixels = vec![[0.5_f32, 0.25, 0.1, 0.5]; 9];
        assert!(wire_removal_frame_f32(&mut pixels, 3, 3, grade));
        for pixel in &pixels {
            assert_pixel_close(*pixel, [0.5, 0.25, 0.1, 0.5], 1.0e-6);
        }
    }
}
