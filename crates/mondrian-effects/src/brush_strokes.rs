//! Brush strokes: smears the frame along an authored stroke direction.
//!
//! Each pixel averages a short line of samples running along the stroke
//! direction.  The line's phase and cross-stroke jitter come from a
//! position hash, so the strokes look hand placed while staying deterministic
//! across frames and runs.

use thiserror::Error;

/// Pixels of source neighbourhood the kernel reads on every side.
pub const BRUSH_STROKES_MAX_SIZE: f32 = 5.0;
pub const BRUSH_STROKES_MAX_LENGTH: f32 = 40.0;
pub const BRUSH_STROKES_MAX_DENSITY: f32 = 2.0;
pub const BRUSH_STROKES_MAX_RANDOMNESS: f32 = 2.0;
pub const BRUSH_STROKES_SURFACE_COUNT: u32 = 4;

const IDENTITY_LENGTH: f32 = 1.0e-6;
const IDENTITY_BLEND: f32 = 1.0e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BrushStrokesError {
    #[error("brush strokes angle must be finite degrees")]
    Angle,
    #[error("brush strokes size must be finite and within [0, 5]")]
    Size,
    #[error("brush strokes length must be finite and within [0, 40]")]
    Length,
    #[error("brush strokes density must be finite and within [0, 2]")]
    Density,
    #[error("brush strokes randomness must be finite and within [0, 2]")]
    Randomness,
    #[error("brush strokes surface must be within [1, 4]")]
    Surface,
    #[error("brush strokes blend must be finite and within [0, 1]")]
    Blend,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushStrokesGrade {
    angle_degrees: f32,
    size: f32,
    length: f32,
    density: f32,
    randomness: f32,
    surface: u32,
    blend_with_original: f32,
}

impl BrushStrokesGrade {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        angle_degrees: f32,
        size: f32,
        length: f32,
        density: f32,
        randomness: f32,
        surface: u32,
        blend_with_original: f32,
    ) -> Result<Self, BrushStrokesError> {
        if !angle_degrees.is_finite() {
            return Err(BrushStrokesError::Angle);
        }
        if !size.is_finite() || !(0.0..=BRUSH_STROKES_MAX_SIZE).contains(&size) {
            return Err(BrushStrokesError::Size);
        }
        if !length.is_finite() || !(0.0..=BRUSH_STROKES_MAX_LENGTH).contains(&length) {
            return Err(BrushStrokesError::Length);
        }
        if !density.is_finite() || !(0.0..=BRUSH_STROKES_MAX_DENSITY).contains(&density) {
            return Err(BrushStrokesError::Density);
        }
        if !randomness.is_finite() || !(0.0..=BRUSH_STROKES_MAX_RANDOMNESS).contains(&randomness) {
            return Err(BrushStrokesError::Randomness);
        }
        if !(1..=BRUSH_STROKES_SURFACE_COUNT).contains(&surface) {
            return Err(BrushStrokesError::Surface);
        }
        if !blend_with_original.is_finite() || !(0.0..=1.0).contains(&blend_with_original) {
            return Err(BrushStrokesError::Blend);
        }
        Ok(Self {
            angle_degrees,
            size,
            length,
            density,
            randomness,
            surface,
            blend_with_original,
        })
    }

    pub fn angle_degrees(&self) -> f32 {
        self.angle_degrees
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    pub fn length(&self) -> f32 {
        self.length
    }

    pub fn density(&self) -> f32 {
        self.density
    }

    pub fn randomness(&self) -> f32 {
        self.randomness
    }

    pub fn surface(&self) -> u32 {
        self.surface
    }

    pub fn blend_with_original(&self) -> f32 {
        self.blend_with_original
    }

    /// A zero length or a full original blend leaves the frame alone.
    pub fn is_identity(&self) -> bool {
        self.length <= IDENTITY_LENGTH || self.blend_with_original >= 1.0 - IDENTITY_BLEND
    }

    /// Number of taps along the stroke.
    pub fn taps(&self) -> u32 {
        let taps = (self.length * self.density.max(0.5)).round() as u32;
        taps.clamp(1, 64)
    }

    /// Source neighbourhood: half the stroke plus any jitter.
    pub fn input_halo(&self) -> Option<(u32, u32)> {
        if self.is_identity() {
            return Some((0, 0));
        }
        let reach = self.length * 0.5 + self.size + self.randomness * self.size * 0.5;
        let halo = reach.ceil().max(1.0) as u32;
        Some((halo, halo))
    }

    /// Unit stroke direction; 0 degrees runs left to right.
    pub fn direction(&self) -> [f32; 2] {
        let radians = self.angle_degrees.to_radians();
        [radians.cos(), radians.sin()]
    }

    /// Per-pixel jitter applied across the stroke, in pixels.
    pub fn jitter(&self, x: u32, y: u32) -> (f32, f32) {
        if self.randomness <= 0.0 {
            return (0.0, 0.0);
        }
        let scale = self.randomness * self.size;
        let along = (hash_unit(x, y, self.surface, 0x9e37_79b9) - 0.5) * 2.0 * self.length * 0.5;
        let across = (hash_unit(x, y, self.surface, 0x85eb_ca6b) - 0.5) * 2.0 * scale;
        let direction = self.direction();
        let normal = [-direction[1], direction[0]];
        (
            direction[0] * along + normal[0] * across,
            direction[1] * along + normal[1] * across,
        )
    }
}

fn hash_u32(x: u32, y: u32, salt: u32, seed: u32) -> u32 {
    let mut value = x
        .wrapping_mul(0x1b87_3593)
        .wrapping_add(y.wrapping_mul(0xcc9e_2d51))
        .wrapping_add(salt.wrapping_mul(0x7feb_352d))
        .wrapping_add(seed);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    value
}

fn hash_unit(x: u32, y: u32, salt: u32, seed: u32) -> f32 {
    (hash_u32(x, y, salt, seed) >> 8) as f32 / (1u32 << 24) as f32
}

/// Reads the nearest source pixel, repeating the frame edge for out of range
/// taps.
///
/// Nearest sampling keeps the kernel at one fetch per tap, and clamping keeps a
/// flat frame flat: without it the taps that fall outside the frame would count
/// as empty coverage and fade every border pixel.
fn sample_premultiplied(
    source: &[[f32; 4]],
    width: usize,
    height: usize,
    x: f32,
    y: f32,
) -> [f32; 4] {
    let max_x = width.saturating_sub(1) as f32;
    let max_y = height.saturating_sub(1) as f32;
    let x = x.round().clamp(0.0, max_x) as usize;
    let y = y.round().clamp(0.0, max_y) as usize;
    let pixel = source[y * width + x];
    let alpha = pixel[3].clamp(0.0, 1.0);
    [pixel[0] * alpha, pixel[1] * alpha, pixel[2] * alpha, alpha]
}

/// Applies the brush strokes in place from a snapshot of the source frame.
pub(crate) fn apply_brush_strokes_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &BrushStrokesGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || pixels.len() != width.saturating_mul(height) {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let source = pixels.to_vec();
    let direction = grade.direction();
    let taps = grade.taps();
    let step = if taps > 1 {
        grade.length() / (taps - 1) as f32
    } else {
        0.0
    };
    let half = (taps - 1) as f32 * 0.5;
    checkpoint()?;
    for y in 0..height as u32 {
        for x in 0..width as u32 {
            let index = y as usize * width + x as usize;
            let (jitter_x, jitter_y) = grade.jitter(x, y);
            let mut sum = [0.0_f32; 4];
            for tap in 0..taps {
                let offset = (tap as f32 - half) * step;
                let sample = sample_premultiplied(
                    &source,
                    width,
                    height,
                    x as f32 + jitter_x + direction[0] * offset,
                    y as f32 + jitter_y + direction[1] * offset,
                );
                for channel in 0..4 {
                    sum[channel] += sample[channel];
                }
            }
            let average = [
                sum[0] / taps as f32,
                sum[1] / taps as f32,
                sum[2] / taps as f32,
                sum[3] / taps as f32,
            ];
            let straight = crate::coverage::straight_rgba_from_premultiplied(average);
            let blend = grade.blend_with_original();
            pixels[index] = if blend <= IDENTITY_BLEND {
                straight
            } else {
                let original = source[index];
                [
                    straight[0] * (1.0 - blend) + original[0] * blend,
                    straight[1] * (1.0 - blend) + original[1] * blend,
                    straight[2] * (1.0 - blend) + original[2] * blend,
                    straight[3] * (1.0 - blend) + original[3] * blend,
                ]
            };
        }
        if y % 512 == 0 {
            checkpoint()?;
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Whole-frame helper shared by tests and the renderer parity check.
pub fn brush_strokes_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &BrushStrokesGrade,
) -> bool {
    match apply_brush_strokes_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    }) {
        Ok(applied) => applied,
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(length: f32, angle: f32) -> BrushStrokesGrade {
        BrushStrokesGrade::new(angle, 2.0, length, 1.0, 0.0, 1, 0.0).expect("valid brush strokes")
    }

    fn ramp(width: usize, height: usize) -> Vec<[f32; 4]> {
        (0..width * height)
            .map(|index| [(index % width) as f32, 0.0, 0.0, 1.0])
            .collect()
    }

    #[test]
    fn range_validation_rejects_impossible_parameters() {
        assert_eq!(
            BrushStrokesGrade::new(0.0, 6.0, 1.0, 1.0, 0.0, 1, 0.0).unwrap_err(),
            BrushStrokesError::Size
        );
        assert_eq!(
            BrushStrokesGrade::new(0.0, 1.0, 41.0, 1.0, 0.0, 1, 0.0).unwrap_err(),
            BrushStrokesError::Length
        );
        assert_eq!(
            BrushStrokesGrade::new(0.0, 1.0, 1.0, 1.0, 0.0, 5, 0.0).unwrap_err(),
            BrushStrokesError::Surface
        );
        assert_eq!(
            BrushStrokesGrade::new(0.0, 1.0, 1.0, 1.0, 0.0, 1, 1.5).unwrap_err(),
            BrushStrokesError::Blend
        );
    }

    #[test]
    fn zero_length_or_full_blend_is_identity() {
        assert!(grade(0.0, 45.0).is_identity());
        let full = BrushStrokesGrade::new(45.0, 2.0, 10.0, 1.0, 0.0, 1, 1.0).expect("identity");
        assert!(full.is_identity());
        let mut frame = ramp(4, 2);
        let original = frame.clone();
        assert!(brush_strokes_frame_f32(&mut frame, 4, 2, &full));
        assert_eq!(frame, original);
    }

    #[test]
    fn flat_frames_survive_the_smear() {
        let mut frame = vec![[0.25, 0.5, 0.75, 1.0]; 16];
        assert!(brush_strokes_frame_f32(&mut frame, 4, 4, &grade(5.0, 30.0)));
        for pixel in frame {
            assert!((pixel[0] - 0.25).abs() < 1.0e-6);
            assert!((pixel[1] - 0.5).abs() < 1.0e-6);
            assert!((pixel[2] - 0.75).abs() < 1.0e-6);
            assert_eq!(pixel[3], 1.0);
        }
    }

    #[test]
    fn horizontal_strokes_smear_the_impulse_along_the_row() {
        let mut frame = vec![[0.0, 0.0, 0.0, 1.0]; 9];
        frame[4] = [8.0, 0.0, 0.0, 1.0];
        assert!(brush_strokes_frame_f32(&mut frame, 9, 1, &grade(4.0, 0.0)));
        // The impulse spreads into its immediate neighbours, loses peak energy,
        // and never reaches the far end of the row.
        assert!(frame[3][0] > 0.0);
        assert!(frame[5][0] > 0.0);
        assert!(frame[4][0] < 8.0);
        assert_eq!(frame[0][0], 0.0);
        assert_eq!(frame[8][0], 0.0);
    }

    #[test]
    fn vertical_strokes_smear_the_impulse_along_the_column() {
        let mut frame = vec![[0.0, 0.0, 0.0, 1.0]; 4 * 9];
        frame[4 * 4 + 1] = [8.0, 0.0, 0.0, 1.0];
        assert!(brush_strokes_frame_f32(&mut frame, 4, 9, &grade(4.0, 90.0)));
        assert!(frame[3 * 4 + 1][0] > 0.0);
        assert!(frame[5 * 4 + 1][0] > 0.0);
        assert_eq!(frame[0][0], 0.0);
        assert_eq!(frame[8 * 4 + 1][0], 0.0);
        // A vertical stroke must not bleed sideways.
        assert_eq!(frame[4 * 4][0], 0.0);
    }

    #[test]
    fn jitter_is_deterministic_and_bounded() {
        let grade = BrushStrokesGrade::new(0.0, 2.0, 10.0, 1.0, 2.0, 3, 0.0).expect("valid");
        let first = grade.jitter(5, 7);
        assert_eq!(first, grade.jitter(5, 7));
        assert!(first.0.abs() <= 10.0 + 1.0e-4);
        let mut frame = ramp(6, 4);
        let mut second = frame.clone();
        assert!(brush_strokes_frame_f32(&mut frame, 6, 4, &grade));
        assert!(brush_strokes_frame_f32(&mut second, 6, 4, &grade));
        assert_eq!(frame, second);
    }

    #[test]
    fn blend_keeps_the_original_amount() {
        let mut frame = ramp(8, 1);
        let original = frame.clone();
        let grade = BrushStrokesGrade::new(0.0, 2.0, 6.0, 1.0, 0.0, 1, 0.5).expect("valid");
        assert!(brush_strokes_frame_f32(&mut frame, 8, 1, &grade));
        for (index, pixel) in frame.iter().enumerate() {
            let smeared = {
                let mut probe = original.clone();
                let full = BrushStrokesGrade::new(0.0, 2.0, 6.0, 1.0, 0.0, 1, 0.0).expect("valid");
                assert!(brush_strokes_frame_f32(&mut probe, 8, 1, &full));
                probe[index][0]
            };
            let expected = smeared * 0.5 + original[index][0] * 0.5;
            assert!((pixel[0] - expected).abs() < 1.0e-5);
        }
    }

    #[test]
    fn extended_range_colours_are_not_clamped() {
        let mut frame = vec![[2.0, 0.5, 0.0, 1.0]; 4];
        frame[2][0] = 4.0;
        assert!(brush_strokes_frame_f32(&mut frame, 4, 1, &grade(3.0, 0.0)));
        assert!(frame.iter().any(|pixel| pixel[0] > 2.0));
    }

    #[test]
    fn taps_follow_length_and_density() {
        let dense = BrushStrokesGrade::new(0.0, 1.0, 10.0, 2.0, 0.0, 1, 0.0).expect("valid");
        assert_eq!(dense.taps(), 20);
        assert_eq!(grade(0.5, 0.0).taps(), 1);
    }
}
