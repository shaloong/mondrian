//! Kaleidoscope: folds the frame into radial wedges around an authored centre.
//!
//! The destination angle is reduced to one wedge of `2π / mirroring` radians
//! and alternate wedges are mirrored, which produces the classic symmetric
//! pattern. `size` magnifies the sampled radius (a value of 200 % shows twice
//! as much of the frame), `rotation` turns the whole fold and `floating
//! centre` reads the authored centre as a fraction of the frame instead of
//! absolute pixels.
//!
//! Sampling is nearest-neighbour with a clamped source coordinate: the fold
//! is a pure coordinate remap, so the result is deterministic and cheap, and
//! pixels outside the frame replicate the edge instead of inventing colour.

use thiserror::Error;

/// Smallest accepted kaleidoscope size as a percentage.
pub const KALEIDOSCOPE_MIN_SIZE: f32 = 1.0;
/// Largest accepted kaleidoscope size as a percentage.
pub const KALEIDOSCOPE_MAX_SIZE: f32 = 1_000.0;
/// Smallest accepted wedge count.
pub const KALEIDOSCOPE_MIN_MIRRORING: u32 = 1;
/// Largest accepted wedge count.
pub const KALEIDOSCOPE_MAX_MIRRORING: u32 = 9;

#[derive(Debug, Error, PartialEq)]
pub enum KaleidoscopeError {
    #[error("kaleidoscope centre must be finite")]
    Center,
    #[error("kaleidoscope size must be finite and within [{KALEIDOSCOPE_MIN_SIZE}, {KALEIDOSCOPE_MAX_SIZE}] percent")]
    Size,
    #[error("kaleidoscope mirroring must be within [{KALEIDOSCOPE_MIN_MIRRORING}, {KALEIDOSCOPE_MAX_MIRRORING}]")]
    Mirroring,
    #[error("kaleidoscope rotation must be finite")]
    Rotation,
    #[error("kaleidoscope floating centre must be finite and within [0, 1]")]
    FloatingCenter,
}

/// Authored kaleidoscope parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KaleidoscopeGrade {
    center: [f32; 2],
    size_percent: f32,
    mirroring: u32,
    rotation_degrees: f32,
    floating_center: bool,
}

impl KaleidoscopeGrade {
    pub fn new(
        center: [f32; 2],
        size_percent: f32,
        mirroring: u32,
        rotation_degrees: f32,
        floating_center: bool,
    ) -> Result<Self, KaleidoscopeError> {
        if !center[0].is_finite() || !center[1].is_finite() {
            return Err(KaleidoscopeError::Center);
        }
        if !size_percent.is_finite()
            || !(KALEIDOSCOPE_MIN_SIZE..=KALEIDOSCOPE_MAX_SIZE).contains(&size_percent)
        {
            return Err(KaleidoscopeError::Size);
        }
        if !(KALEIDOSCOPE_MIN_MIRRORING..=KALEIDOSCOPE_MAX_MIRRORING).contains(&mirroring) {
            return Err(KaleidoscopeError::Mirroring);
        }
        if !rotation_degrees.is_finite() {
            return Err(KaleidoscopeError::Rotation);
        }
        Ok(Self {
            center,
            size_percent,
            mirroring,
            rotation_degrees,
            floating_center,
        })
    }

    /// Centre in absolute pixels, or in fractions of the frame when the
    /// floating flag is set.
    pub fn center(&self) -> [f32; 2] {
        self.center
    }

    pub fn size_percent(&self) -> f32 {
        self.size_percent
    }

    pub fn mirroring(&self) -> u32 {
        self.mirroring
    }

    pub fn rotation_degrees(&self) -> f32 {
        self.rotation_degrees
    }

    pub fn floating_center(&self) -> bool {
        self.floating_center
    }

    /// One wedge spans `2π / mirroring`; a single wedge with no rotation and
    /// a full-size window is the only configuration that leaves the frame
    /// alone, and even then only around the centre.
    pub fn is_identity(&self) -> bool {
        self.mirroring == 1
            && self.rotation_degrees == 0.0
            && (self.size_percent - 100.0).abs() <= f32::EPSILON
            && self.center == [0.0, 0.0]
            && !self.floating_center
    }

    fn resolved_center(&self, width: u32, height: u32) -> [f32; 2] {
        if self.floating_center {
            [
                self.center[0] * width as f32,
                self.center[1] * height as f32,
            ]
        } else {
            self.center
        }
    }

    /// Resolves the source pixel for one destination pixel.
    pub fn source_pixel(&self, x: u32, y: u32, width: u32, height: u32) -> (usize, usize) {
        let center = self.resolved_center(width, height);
        let dx = x as f32 + 0.5 - center[0];
        let dy = y as f32 + 0.5 - center[1];
        let mut angle =
            dy.atan2(dx) - self.rotation_degrees.to_radians().rem_euclid(std::f32::consts::TAU);
        let wedge = std::f32::consts::TAU / self.mirroring.max(1) as f32;
        let index = (angle / wedge).floor();
        let mut local = angle - index * wedge;
        if (index as i64).rem_euclid(2) == 1 {
            local = wedge - local;
        }
        angle = local;
        let radius = dx.hypot(dy) * (self.size_percent / 100.0);
        let source_x = center[0] + radius * angle.cos();
        let source_y = center[1] + radius * angle.sin();
        (
            source_x.floor().max(0.0) as usize,
            source_y.floor().max(0.0) as usize,
        )
    }
}

/// Applies the kaleidoscope in place from a snapshot of the source frame.
pub(crate) fn apply_kaleidoscope_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &KaleidoscopeGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if width == 0 || height == 0 || pixels.len() != width.saturating_mul(height) {
        return Ok(false);
    }
    let width = width as u32;
    let height = height as u32;
    let source = pixels.to_vec();
    checkpoint()?;
    for y in 0..height {
        for x in 0..width {
            let (source_x, source_y) = grade.source_pixel(x, y, width, height);
            let index = (source_y.min(height as usize - 1)) * width as usize
                + source_x.min(width as usize - 1);
            pixels[y as usize * width as usize + x as usize] = source[index];
        }
        if y % 512 == 0 {
            checkpoint()?;
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Whole-frame helper shared by tests and the renderer parity check.
pub fn kaleidoscope_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &KaleidoscopeGrade,
) -> bool {
    match apply_kaleidoscope_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    }) {
        Ok(applied) => applied,
        Err(never) => match never {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(mirroring: u32, size: f32, rotation: f32) -> KaleidoscopeGrade {
        KaleidoscopeGrade::new([2.0, 2.0], size, mirroring, rotation, false).expect("valid grade")
    }

    fn ramp(width: usize, height: usize) -> Vec<[f32; 4]> {
        (0..width * height).map(|index| [index as f32, 0.0, 0.0, 1.0]).collect()
    }

    #[test]
    fn range_validation_rejects_impossible_parameters() {
        assert_eq!(
            KaleidoscopeGrade::new([0.0, 0.0], 0.0, 3, 0.0, false).unwrap_err(),
            KaleidoscopeError::Size
        );
        assert_eq!(
            KaleidoscopeGrade::new([0.0, 0.0], 100.0, 10, 0.0, false).unwrap_err(),
            KaleidoscopeError::Mirroring
        );
        assert_eq!(
            KaleidoscopeGrade::new([f32::NAN, 0.0], 100.0, 3, 0.0, false).unwrap_err(),
            KaleidoscopeError::Center
        );
    }

    #[test]
    fn single_wedge_at_the_origin_maps_the_frame_onto_itself() {
        let grade = KaleidoscopeGrade::new([0.0, 0.0], 100.0, 1, 0.0, false).expect("identity");
        let mut frame = ramp(2, 2);
        let original = frame.clone();
        assert!(kaleidoscope_frame_f32(&mut frame, 2, 2, &grade));
        assert_eq!(frame, original);
        assert!(grade.is_identity());
    }

    #[test]
    fn mirrors_produce_symmetric_samples() {
        let grade = grade(2, 100.0, 0.0);
        // With two wedges the fold line runs along the horizontal axis through
        // the centre, so pixel rows that mirror across that line (pixel centres
        // at -0.5 and +0.5 around centre y = 2.0) share one source pixel.
        assert_eq!(
            grade.source_pixel(0, 1, 4, 4),
            grade.source_pixel(0, 2, 4, 4)
        );
        assert_eq!(
            grade.source_pixel(3, 1, 4, 4),
            grade.source_pixel(3, 2, 4, 4)
        );
    }

    #[test]
    fn size_scales_the_sampled_radius() {
        let small = grade(1, 50.0, 0.0);
        let large = grade(1, 200.0, 0.0);
        let (near_x, _) = small.source_pixel(3, 2, 4, 4);
        let (far_x, _) = large.source_pixel(3, 2, 4, 4);
        assert!(far_x >= near_x);
    }

    #[test]
    fn floats_the_centre_when_asked() {
        let absolute = KaleidoscopeGrade::new([8.0, 8.0], 100.0, 1, 0.0, false).unwrap();
        let floating = KaleidoscopeGrade::new([0.5, 0.5], 100.0, 1, 0.0, true).unwrap();
        // A floating centre of 0.5 sits at the middle of an 16x16 frame.
        assert_eq!(
            floating.source_pixel(8, 8, 16, 16),
            absolute.source_pixel(8, 8, 16, 16)
        );
    }

    #[test]
    fn transparent_pixels_and_extended_ranges_survive() {
        let grade = KaleidoscopeGrade::new([0.0, 0.0], 100.0, 1, 0.0, false).unwrap();
        let mut frame: Vec<[f32; 4]> = vec![[0.0, 0.0, 0.0, 0.0], [2.5, -0.5, 1.0, 1.0]];
        assert!(kaleidoscope_frame_f32(&mut frame, 2, 1, &grade));
        assert_eq!(frame, vec![[0.0, 0.0, 0.0, 0.0], [2.5, -0.5, 1.0, 1.0]]);
    }

    #[test]
    fn mismatched_frame_size_is_rejected() {
        let grade = grade(4, 100.0, 0.0);
        let mut frame = ramp(2, 2);
        assert!(!kaleidoscope_frame_f32(&mut frame, 3, 2, &grade));
    }

    #[test]
    fn rotation_turns_the_fold() {
        let base = grade(2, 100.0, 0.0);
        let turned = grade(2, 100.0, 45.0);
        assert_ne!(
            base.source_pixel(3, 0, 4, 4),
            turned.source_pixel(3, 0, 4, 4)
        );
    }
}
