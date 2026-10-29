//! Scatter: replicates the frame with position-hashed jitter.
//!
//! Scatter draws a few offset copies of the source on top of the original.
//! The offsets come from an integer hash of the sample position and the copy
//! index, so a frame always scatters the same way and repeated processing is
//! stable; there is no time or frame-seed dependency.

use thiserror::Error;

/// Pixels of source neighbourhood the kernel reads on every side.
pub const SCATTER_MAX_AMOUNT_PIXELS: f32 = 1_000.0;
/// Largest number of offset copies.
pub const SCATTER_MAX_COPIES: u32 = 3;

const IDENTITY_AMOUNT: f32 = 1.0e-6;
const TAU: f32 = std::f32::consts::TAU;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ScatterError {
    #[error("scatter amount must be finite and within [0, 1000] pixels")]
    Amount,
    #[error("scatter copies must be within [1, 3]")]
    Copies,
    #[error("scatter randomness must be finite and within [0, 1]")]
    Randomness,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScatterGrade {
    amount_pixels: f32,
    copies: u32,
    randomness: f32,
}

impl ScatterGrade {
    pub fn new(amount_pixels: f32, copies: u32, randomness: f32) -> Result<Self, ScatterError> {
        if !amount_pixels.is_finite() || !(0.0..=SCATTER_MAX_AMOUNT_PIXELS).contains(&amount_pixels)
        {
            return Err(ScatterError::Amount);
        }
        if !(1..=SCATTER_MAX_COPIES).contains(&copies) {
            return Err(ScatterError::Copies);
        }
        if !randomness.is_finite() || !(0.0..=1.0).contains(&randomness) {
            return Err(ScatterError::Randomness);
        }
        Ok(Self { amount_pixels, copies, randomness })
    }

    pub fn amount_pixels(&self) -> f32 {
        self.amount_pixels
    }

    pub fn copies(&self) -> u32 {
        self.copies
    }

    pub fn randomness(&self) -> f32 {
        self.randomness
    }

    /// Without displacement the effect is a pure identity.
    pub fn is_identity(&self) -> bool {
        self.amount_pixels <= IDENTITY_AMOUNT
    }

    /// Source neighbourhood the kernel needs on every side.
    pub fn input_halo(&self) -> Option<(u32, u32)> {
        if self.is_identity() {
            return Some((0, 0));
        }
        let halo = self.amount_pixels.ceil() as u32;
        Some((halo, halo))
    }

    /// Offset for one copy, in pixels.
    ///
    /// `randomness` zero places every copy on a half-radius ring, one spreads
    /// them over the whole disc; the bearing always comes from the position
    /// hash so neighbouring pixels scatter independently.
    pub fn offset(&self, x: u32, y: u32, copy: u32) -> (f32, f32) {
        if self.is_identity() {
            return (0.0, 0.0);
        }
        let bearing = hash_unit(x, y, copy, 0x9e37_79b9) * TAU;
        let spread = hash_unit(x, y, copy, 0x85eb_ca6b);
        let fraction = (1.0 - self.randomness) * 0.5 + self.randomness * spread;
        let radius = self.amount_pixels * fraction;
        (bearing.cos() * radius, bearing.sin() * radius)
    }

    /// Composites one scattered sample over an existing pixel.
    pub fn over(source: [f32; 4], destination: [f32; 4]) -> [f32; 4] {
        let alpha = source[3].clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return destination;
        }
        if alpha >= 1.0 {
            return source;
        }
        let inverse = 1.0 - alpha;
        [
            source[0] * alpha + destination[0] * inverse,
            source[1] * alpha + destination[1] * inverse,
            source[2] * alpha + destination[2] * inverse,
            alpha + destination[3] * inverse,
        ]
    }
}

/// Deterministic 32-bit mix of the sample position and copy index.
fn hash_u32(x: u32, y: u32, copy: u32, salt: u32) -> u32 {
    let mut value = x
        .wrapping_mul(0x1b87_3593)
        .wrapping_add(y.wrapping_mul(0xcc9e_2d51))
        .wrapping_add(copy.wrapping_mul(0x7feb_352d))
        .wrapping_add(salt);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    value
}

/// Hash mapped to `[0, 1)`.
fn hash_unit(x: u32, y: u32, copy: u32, salt: u32) -> f32 {
    (hash_u32(x, y, copy, salt) >> 8) as f32 / (1u32 << 24) as f32
}

fn sample(source: &[[f32; 4]], width: usize, height: usize, x: f32, y: f32) -> [f32; 4] {
    let x = x.floor();
    let y = y.floor();
    if x < 0.0 || y < 0.0 || x >= width as f32 || y >= height as f32 {
        return [0.0; 4];
    }
    source[y as usize * width + x as usize]
}

/// Applies the scatter in place from a snapshot of the source frame.
pub(crate) fn apply_scatter_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ScatterGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || pixels.len() != width.saturating_mul(height) {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let source = pixels.to_vec();
    checkpoint()?;
    for y in 0..height as u32 {
        for x in 0..width as u32 {
            let index = y as usize * width + x as usize;
            let mut pixel = source[index];
            for copy in 1..=grade.copies() {
                let (dx, dy) = grade.offset(x, y, copy);
                let sample = sample(&source, width, height, x as f32 + dx, y as f32 + dy);
                pixel = ScatterGrade::over(sample, pixel);
            }
            pixels[index] = pixel;
        }
        if y % 512 == 0 {
            checkpoint()?;
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Whole-frame helper shared by tests and the renderer parity check.
pub fn scatter_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ScatterGrade,
) -> bool {
    match apply_scatter_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    }) {
        Ok(applied) => applied,
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(amount: f32, copies: u32, randomness: f32) -> ScatterGrade {
        ScatterGrade::new(amount, copies, randomness).expect("valid scatter")
    }

    #[test]
    fn range_validation_rejects_impossible_parameters() {
        assert_eq!(
            ScatterGrade::new(-1.0, 1, 0.0).unwrap_err(),
            ScatterError::Amount
        );
        assert_eq!(
            ScatterGrade::new(2.0, 0, 0.0).unwrap_err(),
            ScatterError::Copies
        );
        assert_eq!(
            ScatterGrade::new(2.0, 2, 1.5).unwrap_err(),
            ScatterError::Randomness
        );
    }

    #[test]
    fn zero_amount_is_identity() {
        let grade = grade(0.0, 3, 0.5);
        assert!(grade.is_identity());
        let mut frame = vec![[0.1, 0.2, 0.3, 1.0]; 16];
        let original = frame.clone();
        assert!(scatter_frame_f32(&mut frame, 4, 4, &grade));
        assert_eq!(frame, original);
    }

    #[test]
    fn halo_tracks_the_amount() {
        assert_eq!(grade(0.0, 1, 0.0).input_halo(), Some((0, 0)));
        assert_eq!(grade(7.2, 2, 0.5).input_halo(), Some((8, 8)));
    }

    #[test]
    fn offsets_are_deterministic_and_bounded() {
        let grade = grade(10.0, 3, 1.0);
        let first = grade.offset(4, 9, 2);
        assert_eq!(first, grade.offset(4, 9, 2));
        assert!(first.0.hypot(first.1) <= 10.0 + 1.0e-4);
        assert_ne!(first, grade.offset(5, 9, 2));
    }

    #[test]
    fn zero_randomness_keeps_copies_on_the_half_ring() {
        let grade = grade(10.0, 2, 0.0);
        for x in 0..8 {
            let (dx, dy) = grade.offset(x, 3, 1);
            let radius = dx.hypot(dy);
            assert!((radius - 5.0).abs() < 1.0e-4, "radius {radius}");
        }
    }

    #[test]
    fn copies_land_on_distinct_offsets() {
        let grade = grade(20.0, 3, 0.75);
        let first = grade.offset(2, 2, 1);
        let second = grade.offset(2, 2, 2);
        let third = grade.offset(2, 2, 3);
        assert_ne!(first, second);
        assert_ne!(second, third);
        assert_ne!(first, third);
    }

    #[test]
    fn opaque_copies_replace_the_pixel() {
        let mut frame = vec![[0.0, 0.0, 0.0, 1.0]; 9];
        frame[4] = [1.0, 1.0, 1.0, 1.0];
        let grade = grade(2.0, 3, 0.0);
        assert!(scatter_frame_f32(&mut frame, 3, 3, &grade));
        assert!(
            frame.iter().enumerate().any(|(index, pixel)| index != 4 && pixel[0] > 0.0),
            "expected a scattered copy to leave the source pixel"
        );
    }

    #[test]
    fn out_of_frame_copies_leave_the_pixel_alone() {
        let mut frame = vec![[0.25, 0.5, 0.75, 1.0]; 9];
        let original = frame.clone();
        let grade = grade(50.0, 3, 1.0);
        assert!(scatter_frame_f32(&mut frame, 3, 3, &grade));
        assert_eq!(frame, original);
    }

    #[test]
    fn over_respects_coverage() {
        assert_eq!(
            ScatterGrade::over([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 1.0]),
            [0.0, 1.0, 0.0, 1.0]
        );
        let blended = ScatterGrade::over([1.0, 0.0, 0.0, 0.5], [0.0, 1.0, 0.0, 1.0]);
        assert!((blended[0] - 0.5).abs() < 1.0e-6);
        assert!((blended[1] - 0.5).abs() < 1.0e-6);
        assert!((blended[3] - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn identical_inputs_scatter_identically() {
        let grade = grade(3.0, 2, 0.5);
        let source: Vec<[f32; 4]> =
            (0..25).map(|index| [index as f32 / 25.0, 0.0, 0.0, 1.0]).collect();
        let mut first = source.clone();
        let mut second = source;
        assert!(scatter_frame_f32(&mut first, 5, 5, &grade));
        assert!(scatter_frame_f32(&mut second, 5, 5, &grade));
        assert_eq!(first, second);
        assert!(first.iter().any(|pixel| pixel[0] != 0.0));
    }
}
