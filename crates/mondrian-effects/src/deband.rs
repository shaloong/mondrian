//! RES-04 去色带 · deband.
//!
//! An eight-bit frame only has 256 levels per channel, so a smooth gradient —
//! a sky, a lamp falloff, a dark vignette — turns into visible contour lines:
//! the colour banding. Deband smooths those plateaus while leaving real detail
//! alone by blending a blurred copy back **only where the blurred luma stays
//! close to the original luma**. A real edge keeps its contrast (the weight
//! collapses to `0`), while the one-code-value step between two bands is
//! averaged out. A triangular dither then breaks up whatever staircase survives,
//! which is the "post optimization" knob of the grade of the same name.
//!
//! # Stages
//!
//! 1. **Blur.** The frame is box blurred by
//!    [`crate::box_blur::box_blur_frame_f32`] with `radius`, one iteration, both
//!    axes and repeated edge pixels — exactly the two `fs_box_blur` passes the
//!    Deband spatial pass records on the GPU. The box blur premultiplies its own
//!    input and returns straight alpha, so a copy of the frame is premultiplied
//!    for the merge and the blurred result is premultiplied again after the
//!    blur. Premultiplying the frame *before* the blur instead would weight a
//!    soft matte by `alpha * alpha` and drift away from the shader. A box
//!    average is a weighted sum of whole pixels, so no resampling or
//!    interpolation is involved anywhere in this kernel.
//! 2. **Merge.** With `luma(x) = 0.2126 * x.r + 0.7152 * x.g + 0.0722 * x.b`,
//!    evaluated in premultiplied space,
//!
//!    ```text
//!    w     = clamp(1 - |luma(original) - luma(blurred)| / threshold, 0, 1)
//!    mixed = original + (blurred - original) * w
//!    ```
//!
//!    (`threshold <= 0` means `w = 1`). The blur returns straight alpha, so its
//!    result is premultiplied again before the merge: both luminances must come
//!    from the same premultiplied plane, otherwise a soft matte would bend the
//!    weight. Coverage is not a colour and the merge never touches it: `mixed`
//!    keeps the **original** alpha, so the matte cannot shrink or grow, and a
//!    pixel that arrives fully transparent still covers nothing after the merge.
//!
//!    The dither is added to the three colour channels — never to alpha — and
//!    only to pixels with `w > 0` and `mixed.a > 0`:
//!
//!    ```text
//!    d = post_optimization * (unit_noise(x, y, SALT_A) - unit_noise(x, y, SALT_B))
//!    unit_noise(x, y, salt) = (noise_hash(x, y, salt) >> 8) / 16777216
//!    ```
//!
//!    where `x` and `y` are the integer pixel indices and
//!    [`crate::noise::noise_hash`] is the crate hash, so `unit_noise` lands in
//!    `[0, 1)` and the triangular difference lands in `(-1, 1)`: one unit of
//!    `post_optimization` is one step of the eight-bit ladder
//!    (`1 / 255`). `DEBAND_DITHER_SALT_A` and `DEBAND_DITHER_SALT_B` decorrelate
//!    the two halves, which turns the flat uniform dither into a triangular
//!    noise that survives quantisation much better. The shader mirrors this hash
//!    bit for bit (wrapping `u32` arithmetic on the same two salts).
//! 3. **Unpremultiply.** The merged premultiplied frame is converted back to
//!    straight alpha, so a pixel whose alpha is `0` leaves as `[0, 0, 0, 0]`.
//!
//! # Identity
//!
//! The identity is `radius == 0 && post_optimization == 0`: nothing to smooth
//! and nothing to dither. The graph builder elides the node, and the kernel
//! still reports the frame as supported and returns without touching it.
//!
//! # Precision
//!
//! Everything is `f32` on both the CPU and the GPU. The box average accumulates
//! in `f32` and the shader does the same, which keeps the two paths inside the
//! `1e-4` parity budget; the dither hash is integer arithmetic, so it is
//! bit-exact on both sides.

use crate::box_blur::{box_blur_frame_f32, BoxBlurDirection};
use crate::noise::noise_hash;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Smallest supported blur radius, in pixels.
pub const DEBAND_MIN_RADIUS: f32 = 0.0;

/// Largest supported blur radius, in pixels.
pub const DEBAND_MAX_RADIUS: f32 = 100.0;

/// Largest supported edge threshold, in linear luminance.
pub const DEBAND_MAX_EDGE_THRESHOLD: f32 = 1.0;

/// Largest supported dither amplitude, in eight-bit code values.
pub const DEBAND_MAX_POST_OPTIMIZATION: f32 = 1.0;

/// First salt of the two decorrelated dither hashes.
pub const DEBAND_DITHER_SALT_A: u32 = 0x9E37_79B9;

/// Second salt of the two decorrelated dither hashes.
pub const DEBAND_DITHER_SALT_B: u32 = 0x85EB_CA6B;

/// A rejected deband cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DebandError {
    /// The edge threshold is not a finite value inside `[0, 1]`.
    EdgeThreshold,
    /// The blur radius is not a finite value inside `[0, 100]`.
    Radius,
    /// The post optimization amplitude is not a finite value inside `[0, 1]`.
    PostOptimization,
}

impl std::fmt::Display for DebandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::EdgeThreshold => "deband edge threshold must be a finite value inside [0, 1]",
            Self::Radius => "deband radius must be a finite value inside [0, 100]",
            Self::PostOptimization => {
                "deband post optimization must be a finite value inside [0, 1]"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for DebandError {}

/// A validated deband configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DebandGrade {
    edge_threshold: f32,
    radius: f32,
    post_optimization: f32,
}

impl DebandGrade {
    /// Validate every authored cell.
    pub fn new(
        edge_threshold: f32,
        radius: f32,
        post_optimization: f32,
    ) -> Result<Self, DebandError> {
        if !edge_threshold.is_finite()
            || !(0.0..=DEBAND_MAX_EDGE_THRESHOLD).contains(&edge_threshold)
        {
            return Err(DebandError::EdgeThreshold);
        }
        if !radius.is_finite() || !(DEBAND_MIN_RADIUS..=DEBAND_MAX_RADIUS).contains(&radius) {
            return Err(DebandError::Radius);
        }
        if !post_optimization.is_finite()
            || !(0.0..=DEBAND_MAX_POST_OPTIMIZATION).contains(&post_optimization)
        {
            return Err(DebandError::PostOptimization);
        }
        Ok(Self { edge_threshold, radius, post_optimization })
    }

    /// The largest luminance difference still treated as banding.
    pub const fn edge_threshold(&self) -> f32 {
        self.edge_threshold
    }

    /// The blur radius, in pixels.
    pub const fn radius(&self) -> f32 {
        self.radius
    }

    /// The dither amplitude, in eight-bit code values.
    pub const fn post_optimization(&self) -> f32 {
        self.post_optimization
    }

    /// Whether the effect leaves the frame untouched.
    pub fn is_identity(&self) -> bool {
        self.radius == 0.0 && self.post_optimization == 0.0
    }

    /// Pack the two uniform cells the shader reads.
    ///
    /// `params` carries `[edge_threshold, radius, post_optimization, 0.0]` and
    /// `extra` is reserved for later revisions (`[0.0; 4]`).
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.edge_threshold,
                self.radius,
                self.post_optimization,
                0.0,
            ],
            [0.0; 4],
        )
    }
}

/// Apply deband to one straight-alpha rgba f32 frame.
///
/// Returns `false` only when the frame is empty or cannot be addressed; the
/// grade is assumed to be validated already, so an unsupported frame is the
/// only failure.
pub fn deband_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &DebandGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_deband_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// The checkpointed kernel behind [`deband_frame_f32`].
pub(crate) fn apply_deband_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &DebandGrade,
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
    let frame = &mut working[..pixel_count];
    // The box blur premultiplies its own input, averages the premultiplied
    // pixels and returns straight alpha — exactly like the two `fs_box_blur`
    // passes the GPU records. The original copy is premultiplied here so both
    // luminances come from the same plane, and the blurred frame is folded back
    // into premultiplied space *after* the blur instead of before it:
    // premultiplying first would apply coverage twice (`alpha * alpha`) and
    // drift away from the shader for a soft matte.
    let mut original = frame.to_vec();
    premultiply_rgba_controlled(&mut original, checkpoint)?;
    checkpoint()?;
    box_blur_frame_f32(
        frame,
        width,
        height,
        grade.radius,
        1,
        BoxBlurDirection::HorizontalAndVertical,
        true,
    );
    // The box blur hands back straight alpha; the merge compares premultiplied
    // luminances, so fold coverage back in before mixing.
    premultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    let threshold = grade.edge_threshold;
    let amplitude = grade.post_optimization;
    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (row_offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + row_offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let index = row * width + column;
                let source = original[index];
                let blurred = *pixel;
                let weight = deband_weight(deband_luma(source), deband_luma(blurred), threshold);
                let mut mixed = [
                    source[0] + (blurred[0] - source[0]) * weight,
                    source[1] + (blurred[1] - source[1]) * weight,
                    source[2] + (blurred[2] - source[2]) * weight,
                    // Coverage is the original matte: the merge never changes it,
                    // so a transparent pixel cannot pick up the blur's alpha.
                    source[3],
                ];
                if weight > 0.0 && mixed[3] > 0.0 {
                    let dither = amplitude * deband_triangular_noise(column as u32, row as u32);
                    mixed[0] += dither;
                    mixed[1] += dither;
                    mixed[2] += dither;
                }
                *pixel = mixed;
            }
        }
    });
    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

/// The blend weight for one pixel, from the two premultiplied luminances.
fn deband_weight(original_luma: f32, blurred_luma: f32, threshold: f32) -> f32 {
    if threshold <= 0.0 {
        return 1.0;
    }
    (1.0 - (original_luma - blurred_luma).abs() / threshold).clamp(0.0, 1.0)
}

/// The premultiplied Rec. 709 luminance of one pixel.
fn deband_luma(pixel: [f32; 4]) -> f32 {
    0.2126 * pixel[0] + 0.7152 * pixel[1] + 0.0722 * pixel[2]
}

/// One decorrelated dither tap, mapped to `[0, 1)`.
fn deband_unit_noise(x: u32, y: u32, salt: u32) -> f32 {
    (noise_hash(x, y, salt) >> 8) as f32 / 16_777_216.0
}

/// The triangular dither of one pixel, inside `(-1, 1)`.
fn deband_triangular_noise(x: u32, y: u32) -> f32 {
    deband_unit_noise(x, y, DEBAND_DITHER_SALT_A) - deband_unit_noise(x, y, DEBAND_DITHER_SALT_B)
}

#[cfg(test)]
mod tests {
    use super::{
        deband_frame_f32, DebandError, DebandGrade, DEBAND_DITHER_SALT_A, DEBAND_DITHER_SALT_B,
        DEBAND_MAX_EDGE_THRESHOLD, DEBAND_MAX_POST_OPTIMIZATION, DEBAND_MAX_RADIUS,
        DEBAND_MIN_RADIUS,
    };

    /// Build a frame from a per-pixel recipe, row by row.
    fn frame<F: Fn(usize, usize) -> [f32; 4]>(
        width: usize,
        height: usize,
        fill: F,
    ) -> Vec<[f32; 4]> {
        let mut pixels = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                pixels.push(fill(column, row));
            }
        }
        pixels
    }

    /// The bit patterns of every channel, for exact comparisons.
    fn bits(pixels: &[[f32; 4]]) -> Vec<[u32; 4]> {
        pixels.iter().map(|pixel| pixel.map(f32::to_bits)).collect()
    }

    /// The bit pattern of one pixel, for exact comparisons.
    fn pixel_bits(pixel: &[f32; 4]) -> [u32; 4] {
        pixel.map(f32::to_bits)
    }

    /// The largest difference between neighbouring columns, per channel zero.
    fn largest_column_step(pixels: &[[f32; 4]]) -> f32 {
        pixels
            .windows(2)
            .map(|pair| (pair[1][0] - pair[0][0]).abs())
            .fold(0.0_f32, f32::max)
    }

    fn grade(edge_threshold: f32, radius: f32, post_optimization: f32) -> DebandGrade {
        DebandGrade::new(edge_threshold, radius, post_optimization).expect("a valid grade")
    }

    #[test]
    fn identity_grade_leaves_the_frame_bit_identical() {
        let grade = grade(0.025, 0.0, 0.0);
        assert!(grade.is_identity());
        let before = frame(9, 5, |column, row| {
            [column as f32 * 0.05, row as f32 * 0.1, 0.3, 1.0]
        });
        let mut after = before.clone();
        assert!(deband_frame_f32(&mut after, 9, 5, &grade));
        assert_eq!(bits(&after), bits(&before));
    }

    #[test]
    fn identity_is_radius_zero_with_no_post_optimization() {
        assert!(grade(0.025, 0.0, 0.0).is_identity());
        assert!(grade(0.0, 0.0, 0.0).is_identity());
        assert!(grade(1.0, DEBAND_MIN_RADIUS, 0.0).is_identity());
        assert!(!grade(0.025, 0.0, 0.001).is_identity());
        assert!(!grade(0.025, 0.5, 0.0).is_identity());
        assert!(!grade(0.025, 3.0, 0.05).is_identity());
    }

    #[test]
    fn hard_step_edge_keeps_both_sides() {
        let width = 9;
        let before = frame(width, 1, |column, _| {
            let value = if column < 4 { 0.1 } else { 0.9 };
            [value, value, value, 1.0]
        });
        let mut after = before.clone();
        assert!(deband_frame_f32(
            &mut after,
            width,
            1,
            &grade(0.025, 2.0, 0.0)
        ));
        // The two pixels that straddle the edge have a luminance difference far
        // above the threshold, so the weight is exactly zero and they keep their
        // own colour: the step survives, it is not averaged away.
        assert_eq!(bits(&after[3..5]), bits(&before[3..5]));
        assert!((after[3][0] - 0.1).abs() <= 1.0e-6);
        assert!((after[4][0] - 0.9).abs() <= 1.0e-6);
        for index in [0, 1, 2, 5, 6, 7, 8] {
            assert!((after[index][0] - before[index][0]).abs() <= 1.0e-6);
        }
        assert!((after[8][0] - after[0][0] - 0.8).abs() <= 1.0e-6);
    }

    #[test]
    fn smooth_ramp_flattens() {
        // Six-pixel plateaus one eighth-bit step apart: the banding staircase a
        // real gradient produces.
        let width = 48;
        let step = 2.0 / 255.0;
        let before = frame(width, 1, |column, _| {
            let value = 0.2 + (column / 6) as f32 * step;
            [value, value, value, 1.0]
        });
        let mut after = before.clone();
        assert!(deband_frame_f32(
            &mut after,
            width,
            1,
            &grade(0.025, 3.0, 0.0)
        ));
        let before_step = largest_column_step(&before);
        let after_step = largest_column_step(&after);
        assert!(
            after_step < before_step * 0.5,
            "the staircase must smooth: before {before_step}, after {after_step}"
        );
        // The plateaus stay in place: the effect smooths edges, it does not
        // shift the level of a gradient.
        assert!((after[0][0] - before[0][0]).abs() <= 0.01);
        assert!((after[width - 1][0] - before[width - 1][0]).abs() <= 0.01);
    }

    #[test]
    fn transparent_pixels_keep_zero_coverage() {
        let before = frame(5, 3, |column, row| {
            if column == 2 && row == 1 {
                [0.0, 0.0, 0.0, 0.0]
            } else {
                [0.6, 0.5, 0.4, 1.0]
            }
        });
        let mut after = before.clone();
        assert!(deband_frame_f32(&mut after, 5, 3, &grade(1.0, 2.0, 0.5)));
        assert_eq!(after[7], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(pixel_bits(&after[7]), pixel_bits(&before[7]));
    }

    #[test]
    fn premultiplied_mixing_does_not_bleed_hidden_colour() {
        // A fully transparent pixel still carries wild hidden rgb; premultiplied
        // mixing must not let it tint its visible neighbours, even with the
        // threshold wide open so the blur is fully blended in.
        let width = 3;
        let hidden = [8.0, -4.0, 2.0, 0.0];
        let visible = [0.2, 0.4, 0.6, 1.0];
        let before = vec![visible, hidden, visible];
        let mut after = before.clone();
        assert!(deband_frame_f32(
            &mut after,
            width,
            1,
            &grade(1.0, 1.0, 0.0)
        ));
        // The hidden rgb is multiplied away by the premultiplied pass, so the
        // transparent pixel keeps zero coverage and cannot survive the merge.
        assert_eq!(after[1], [0.0, 0.0, 0.0, 0.0]);
        for channel in 0..3 {
            assert!(
                after[0][channel] <= before[0][channel] + 1.0e-6,
                "channel {channel} picked up hidden colour: {}",
                after[0][channel]
            );
            assert!(
                after[2][channel] <= before[2][channel] + 1.0e-6,
                "channel {channel} picked up hidden colour: {}",
                after[2][channel]
            );
        }
        assert!(after[0][0] < 0.2);
        assert!((after[0][3] - 1.0).abs() <= 1.0e-6);
    }

    #[test]
    fn dither_is_deterministic() {
        let grade = grade(0.05, 2.0, 0.2);
        let before = frame(11, 7, |column, row| {
            [
                (column as f32 * 3.0 + row as f32) / 64.0,
                (column as f32 + row as f32 * 2.0) / 64.0,
                0.25,
                1.0,
            ]
        });
        let mut first = before.clone();
        let mut second = before.clone();
        assert!(deband_frame_f32(&mut first, 11, 7, &grade));
        assert!(deband_frame_f32(&mut second, 11, 7, &grade));
        assert_eq!(bits(&first), bits(&second));
        assert_ne!(bits(&first), bits(&before));
    }

    #[test]
    fn dither_amplitude_scales_with_post_optimization() {
        // A flat frame with radius zero is the pure dither: the merge finds no
        // difference at all, so every change is the dither itself.
        let before = frame(13, 6, |column, _| {
            [0.5 + column as f32 * 0.01, 0.4, 0.3, 1.0]
        });
        let mut quiet = before.clone();
        let mut loud = before.clone();
        assert!(deband_frame_f32(
            &mut quiet,
            13,
            6,
            &grade(0.025, 0.0, 0.05)
        ));
        assert!(deband_frame_f32(&mut loud, 13, 6, &grade(0.025, 0.0, 0.1)));
        for (index, (quiet_pixel, loud_pixel)) in quiet.iter().zip(loud.iter()).enumerate() {
            let quiet_offset = quiet_pixel[0] - before[index][0];
            let loud_offset = loud_pixel[0] - before[index][0];
            assert!(
                (loud_offset - 2.0 * quiet_offset).abs() <= 1.0e-6,
                "the dither must scale: {quiet_offset} vs {loud_offset}"
            );
            assert!(quiet_offset.abs() <= 0.05 + 1.0e-6);
            assert!(loud_offset.abs() <= 0.1 + 1.0e-6);
        }
    }

    #[test]
    fn zero_post_optimization_adds_no_dither() {
        // A flat field has no luminance difference anywhere, so the merge keeps
        // every pixel and no dither is added when the amplitude is zero.
        let before = frame(8, 4, |_, _| [0.35, 0.45, 0.55, 1.0]);
        let mut after = before.clone();
        assert!(deband_frame_f32(&mut after, 8, 4, &grade(0.025, 3.0, 0.0)));
        for (quiet, source) in after.iter().zip(before.iter()) {
            for channel in 0..4 {
                assert!((quiet[channel] - source[channel]).abs() <= 1.0e-6);
            }
        }
        let mut dithered = before.clone();
        assert!(deband_frame_f32(
            &mut dithered,
            8,
            4,
            &grade(0.025, 3.0, 0.05)
        ));
        let largest_dither = dithered
            .iter()
            .zip(before.iter())
            .map(|(dithered, source)| (dithered[0] - source[0]).abs())
            .fold(0.0_f32, f32::max);
        assert!(
            largest_dither > 1.0e-3,
            "the dither must be visible when it is enabled: {largest_dither}"
        );
    }

    #[test]
    fn dither_never_changes_alpha() {
        let before = frame(7, 5, |column, row| {
            let alpha = match (column + row) % 4 {
                0 => 0.0,
                1 => 0.25,
                2 => 0.5,
                _ => 1.0,
            };
            [column as f32 * 0.1, row as f32 * 0.1, 0.5, alpha]
        });
        let mut after = before.clone();
        assert!(deband_frame_f32(&mut after, 7, 5, &grade(0.2, 2.0, 1.0)));
        let alphas_before: Vec<u32> = before.iter().map(|pixel| pixel[3].to_bits()).collect();
        let alphas_after: Vec<u32> = after.iter().map(|pixel| pixel[3].to_bits()).collect();
        assert_eq!(alphas_after, alphas_before);
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        assert_eq!(
            DebandGrade::new(-0.1, 3.0, 0.05),
            Err(DebandError::EdgeThreshold)
        );
        assert_eq!(
            DebandGrade::new(DEBAND_MAX_EDGE_THRESHOLD + 0.5, 3.0, 0.05),
            Err(DebandError::EdgeThreshold)
        );
        assert_eq!(
            DebandGrade::new(f32::NAN, 3.0, 0.05),
            Err(DebandError::EdgeThreshold)
        );
        assert_eq!(
            DebandGrade::new(f32::INFINITY, 3.0, 0.05),
            Err(DebandError::EdgeThreshold)
        );
        assert_eq!(
            DebandGrade::new(0.025, -1.0, 0.05),
            Err(DebandError::Radius)
        );
        assert_eq!(
            DebandGrade::new(0.025, DEBAND_MAX_RADIUS + 0.5, 0.05),
            Err(DebandError::Radius)
        );
        assert_eq!(
            DebandGrade::new(0.025, f32::NAN, 0.05),
            Err(DebandError::Radius)
        );
        assert_eq!(
            DebandGrade::new(0.025, f32::INFINITY, 0.05),
            Err(DebandError::Radius)
        );
        assert_eq!(
            DebandGrade::new(0.025, 3.0, -0.1),
            Err(DebandError::PostOptimization)
        );
        assert_eq!(
            DebandGrade::new(0.025, 3.0, DEBAND_MAX_POST_OPTIMIZATION + 0.5),
            Err(DebandError::PostOptimization)
        );
        assert_eq!(
            DebandGrade::new(0.025, 3.0, f32::NAN),
            Err(DebandError::PostOptimization)
        );
        assert_eq!(
            DebandGrade::new(0.025, 3.0, f32::INFINITY),
            Err(DebandError::PostOptimization)
        );
        // The declared limits themselves are accepted.
        assert!(DebandGrade::new(0.0, DEBAND_MIN_RADIUS, 0.0).is_ok());
        assert!(DebandGrade::new(
            DEBAND_MAX_EDGE_THRESHOLD,
            DEBAND_MAX_RADIUS,
            DEBAND_MAX_POST_OPTIMIZATION
        )
        .is_ok());
        assert_eq!(
            DebandError::EdgeThreshold.to_string(),
            "deband edge threshold must be a finite value inside [0, 1]"
        );
        assert_eq!(
            DebandError::Radius.to_string(),
            "deband radius must be a finite value inside [0, 100]"
        );
        assert_eq!(
            DebandError::PostOptimization.to_string(),
            "deband post optimization must be a finite value inside [0, 1]"
        );
    }

    #[test]
    fn gpu_controls_pack_the_documented_cells() {
        let grade = grade(0.05, 2.5, 0.2);
        let (params, extra) = grade.gpu_controls();
        assert_eq!(params, [0.05, 2.5, 0.2, 0.0]);
        assert_eq!(extra, [0.0; 4]);
        assert_eq!(grade.edge_threshold().to_bits(), 0.05_f32.to_bits());
        assert_eq!(grade.radius().to_bits(), 2.5_f32.to_bits());
        assert_eq!(grade.post_optimization().to_bits(), 0.2_f32.to_bits());
    }

    #[test]
    fn one_pixel_frame_passes_through() {
        let before = vec![[0.3, 0.5, 0.7, 1.0]];
        let mut after = before.clone();
        assert!(deband_frame_f32(&mut after, 1, 1, &grade(0.025, 3.0, 0.05)));
        assert_eq!(after[0][3], 1.0);
        for channel in 0..3 {
            assert!((after[0][channel] - before[0][channel]).abs() <= 0.05 + 1.0e-6);
        }
        let mut empty_post = before.clone();
        assert!(deband_frame_f32(
            &mut empty_post,
            1,
            1,
            &grade(0.025, 3.0, 0.0)
        ));
        for channel in 0..4 {
            assert!((empty_post[0][channel] - before[0][channel]).abs() <= 1.0e-6);
        }
    }

    #[test]
    fn kernel_reports_false_only_for_an_empty_frame() {
        let default_grade = grade(0.025, 2.0, 0.05);
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!deband_frame_f32(&mut empty, 0, 0, &default_grade));
        assert!(!deband_frame_f32(&mut empty, 0, 4, &default_grade));
        let mut short = vec![[0.5, 0.5, 0.5, 1.0]; 3];
        assert!(!deband_frame_f32(&mut short, 4, 1, &default_grade));
        let mut overflow = vec![[0.5, 0.5, 0.5, 1.0]; 4];
        assert!(!deband_frame_f32(
            &mut overflow,
            usize::MAX,
            2,
            &default_grade
        ));
        let mut one = vec![[0.5, 0.5, 0.5, 1.0]];
        assert!(deband_frame_f32(&mut one, 1, 1, &default_grade));
        let mut full = frame(6, 4, |column, row| {
            [column as f32 * 0.1, row as f32 * 0.1, 0.4, 1.0]
        });
        assert!(deband_frame_f32(&mut full, 6, 4, &default_grade));
        assert!(deband_frame_f32(&mut full, 6, 4, &grade(0.025, 0.0, 0.0)));
    }

    #[test]
    fn zero_threshold_blends_every_pixel() {
        let width = 9;
        let before = frame(width, 1, |column, _| {
            let value = if column < 4 { 0.1 } else { 0.9 };
            [value, value, value, 1.0]
        });
        let mut after = before.clone();
        assert!(deband_frame_f32(
            &mut after,
            width,
            1,
            &grade(0.0, 2.0, 0.0)
        ));
        // A zero threshold means "no edge", so the weight is one everywhere and
        // the pixel next to the step becomes the average of its own window.
        assert!((after[3][0] - 0.42).abs() <= 1.0e-6);
        assert!((after[4][0] - 0.58).abs() <= 1.0e-6);
    }

    #[test]
    fn dither_salts_are_the_documented_constants() {
        assert_eq!(DEBAND_DITHER_SALT_A, 0x9E37_79B9);
        assert_eq!(DEBAND_DITHER_SALT_B, 0x85EB_CA6B);
        assert_ne!(DEBAND_DITHER_SALT_A, DEBAND_DITHER_SALT_B);
    }
}
