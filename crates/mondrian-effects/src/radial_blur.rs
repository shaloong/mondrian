//! Radial blur (effect list BLR-04).
//!
//! Spins or zooms each pixel along its ray from an authored centre, producing
//! the streaked look of a rotating or pushing camera. Authoring units follow
//! the effect list: the amount is unitless (0–1000), the centre is an absolute
//! pixel position and the mode picks spin or zoom.
//!
//! Conventions:
//! - Spin rotates the source around the centre by a total sweep of
//!   `amount * 0.36°`, so the default amount of 10 sweeps 3.6° and the maximum
//!   of 1000 sweeps 360°. The radius of every sample is preserved, so spin can
//!   never drag colour across the centre.
//! - Zoom scales the radius geometrically over `1/sqrt(s) .. sqrt(s)` with
//!   `s = 1 + amount * 0.002`, which keeps the pixel's own radius in the middle
//!   of the sweep instead of biasing the blur outwards. `amount == 0` is the
//!   identity for both modes.
//! - A sample count derived from the amount (doubled when antialiasing is on)
//!   bounds the cost. Samples are also jittered by up to half a tap with a
//!   deterministic per-pixel hash, so a low sample count reads as a smear
//!   instead of discrete ghosts. The hash is a pure function of the seed and the
//!   pixel index, so repeated frames and repeated runs are identical.
//! - Colour is filtered with its coverage (premultiplied intermediate) and edge
//!   samples repeat the border pixel rather than treating the outside as empty.

/// Largest amount the authoring schema accepts.
pub const RADIAL_BLUR_AUTHOR_MAX_AMOUNT: f32 = 1_000.0;

/// Upper bound on the number of samples used for one blurred pixel.
pub const RADIAL_BLUR_MAX_TAPS: u32 = 128;

/// Rotation swept by one unit of amount, in degrees.
const SPIN_DEGREES_PER_AMOUNT: f32 = 0.36;

/// Zoom scale gained by one unit of amount.
const ZOOM_SCALE_PER_AMOUNT: f32 = 0.002;

const AMOUNT_EPSILON: f32 = 1.0e-6;

/// Blur direction around the authored centre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadialBlurMode {
    /// Rotate around the centre, preserving each sample's radius.
    Spin,
    /// Scale away from or towards the centre, preserving each sample's angle.
    Zoom,
}

impl RadialBlurMode {
    /// Maps the effect list's 1-based menu onto a mode.
    pub fn from_menu(value: i64) -> Option<Self> {
        match value {
            1 => Some(Self::Spin),
            2 => Some(Self::Zoom),
            _ => None,
        }
    }

    /// Maps an authored enumeration key onto a mode.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "spin" => Some(Self::Spin),
            "zoom" => Some(Self::Zoom),
            _ => None,
        }
    }

    /// Stable key used by the authoring schema and by cached identities.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Spin => "spin",
            Self::Zoom => "zoom",
        }
    }

    /// Stable tag used by the op hash and by the GPU plan.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Spin => 1,
            Self::Zoom => 2,
        }
    }
}

/// Reasons a radial blur cannot be built from its authored values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadialBlurError {
    /// The amount was not finite or fell outside the declared range.
    Amount,
    /// The centre was not finite.
    Centre,
}

impl std::fmt::Display for RadialBlurError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Amount => {
                formatter.write_str("radial blur amount must be finite and in 0..=1000")
            }
            Self::Centre => formatter.write_str("radial blur centre must be finite"),
        }
    }
}

impl std::error::Error for RadialBlurError {}

/// Validated radial blur parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadialBlurGrade {
    amount: f32,
    centre: [f32; 2],
    mode: RadialBlurMode,
    antialiasing: bool,
    seed: u32,
}

impl RadialBlurGrade {
    /// Builds a grade, rejecting values the authoring schema cannot produce.
    pub fn new(
        amount: f32,
        centre: [f32; 2],
        mode: RadialBlurMode,
        antialiasing: bool,
        seed: u32,
    ) -> Result<Self, RadialBlurError> {
        if !amount.is_finite() || !(0.0..=RADIAL_BLUR_AUTHOR_MAX_AMOUNT).contains(&amount) {
            return Err(RadialBlurError::Amount);
        }
        if !centre[0].is_finite() || !centre[1].is_finite() {
            return Err(RadialBlurError::Centre);
        }
        Ok(Self { amount, centre, mode, antialiasing, seed })
    }

    /// Authored blur amount.
    pub fn amount(self) -> f32 {
        self.amount
    }

    /// Absolute centre in pixels.
    pub fn centre(self) -> [f32; 2] {
        self.centre
    }

    /// Selected blur mode.
    pub fn mode(self) -> RadialBlurMode {
        self.mode
    }

    /// Whether the best-quality menu doubles the sample count.
    pub fn antialiasing(self) -> bool {
        self.antialiasing
    }

    /// Deterministic jitter seed.
    pub fn seed(self) -> u32 {
        self.seed
    }

    /// Returns `true` when the grade leaves the frame untouched.
    pub fn is_identity(self) -> bool {
        self.amount <= AMOUNT_EPSILON
    }

    /// Packs the values the GPU pass needs.
    ///
    /// The first vector carries the centre, the tap count and the mode; the
    /// second carries the amount and the deterministic jitter seed.
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.centre[0],
                self.centre[1],
                self.taps() as f32,
                self.mode.as_u8() as f32,
            ],
            [self.amount, self.seed as f32, 0.0, 0.0],
        )
    }

    /// Number of samples averaged for one blurred pixel.
    pub fn taps(self) -> u32 {
        if self.is_identity() {
            return 1;
        }
        let base = (self.amount / 4.0).ceil() as u32 + 2;
        let scaled = if self.antialiasing { base * 2 } else { base };
        scaled.clamp(2, RADIAL_BLUR_MAX_TAPS)
    }

    /// Total rotation swept by the spin mode, in radians.
    pub fn spin_span_radians(self) -> f32 {
        (self.amount * SPIN_DEGREES_PER_AMOUNT).to_radians()
    }

    /// Radial scale reached by each end of the zoom sweep.
    pub fn zoom_span_scale(self) -> f32 {
        1.0 + self.amount * ZOOM_SCALE_PER_AMOUNT
    }
}

/// Deterministic per-pixel jitter in `[-0.5, 0.5]`.
///
/// The GPU kernel repeats this exact u32 arithmetic, so both paths place their
/// samples at the same positions.
pub fn radial_blur_jitter(seed: u32, x: usize, y: usize) -> f32 {
    let mut hash = seed
        .wrapping_add((x as u32).wrapping_mul(0x9E37_79B9))
        .wrapping_add((y as u32).wrapping_mul(0x85EB_CA6B));
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x7FEB_352D);
    hash ^= hash >> 15;
    ((hash >> 8) & 0xFFFF) as f32 / 65_535.0 - 0.5
}

/// Blurs one frame in place, returning `false` when the grades are unusable.
pub fn radial_blur_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: RadialBlurGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_radial_blur_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Blurs one frame in place with an explicit cancellation checkpoint.
///
/// Returns `Ok(false)` when the parameters cannot be executed so the caller can
/// fall back to an explicit unsupported-op error.
pub(crate) fn apply_radial_blur_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: RadialBlurGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }

    let frame = &mut working[..pixel_count];
    crate::straight_alpha::premultiply_rgba_controlled(frame, checkpoint)?;

    let source = frame.to_vec();
    let taps = grade.taps();
    let weight = 1.0 / taps as f32;
    let tap_step = 2.0 / taps as f64;
    let centre = [grade.centre()[0] as f64, grade.centre()[1] as f64];
    let mode = grade.mode();
    let seed = grade.seed();
    let half_span = grade.spin_span_radians() as f64 * 0.5;
    let log_span = (grade.zoom_span_scale() as f64).ln() * 0.5;

    checkpoint()?;
    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let dx = column as f64 - centre[0];
                let dy = row as f64 - centre[1];
                let jitter = radial_blur_jitter(seed, column, row) as f64;
                let mut sum = [0.0_f64; 4];
                for tap in 0..taps {
                    let base = (tap as f64 + 0.5) * tap_step - 1.0;
                    let position = (base + jitter * tap_step).clamp(-1.0, 1.0);
                    let (sample_x, sample_y) = match mode {
                        RadialBlurMode::Spin => {
                            let angle = position * half_span;
                            let (sin, cos) = angle.sin_cos();
                            (
                                centre[0] + dx * cos - dy * sin,
                                centre[1] + dx * sin + dy * cos,
                            )
                        }
                        RadialBlurMode::Zoom => {
                            let scale = (position * log_span).exp();
                            (centre[0] + dx * scale, centre[1] + dy * scale)
                        }
                    };
                    let sample =
                        sample_bilinear(&source, width, height, sample_x as f32, sample_y as f32);
                    sum[0] += sample[0] as f64;
                    sum[1] += sample[1] as f64;
                    sum[2] += sample[2] as f64;
                    sum[3] += sample[3] as f64;
                }
                *pixel = [
                    (sum[0] * weight as f64) as f32,
                    (sum[1] * weight as f64) as f32,
                    (sum[2] * weight as f64) as f32,
                    (sum[3] * weight as f64) as f32,
                ];
            }
        }
    });

    crate::straight_alpha::unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

/// Bilinear sample that clamps coordinates to the frame so borders repeat.
fn sample_bilinear(source: &[[f32; 4]], width: usize, height: usize, x: f32, y: f32) -> [f32; 4] {
    let max_x = (width - 1) as f32;
    let max_y = (height - 1) as f32;
    let x = x.clamp(0.0, max_x);
    let y = y.clamp(0.0, max_y);
    let x0 = x.floor();
    let y0 = y.floor();
    let frac_x = x - x0;
    let frac_y = y - y0;
    let x0 = x0 as usize;
    let y0 = y0 as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let top_left = source[y0 * width + x0];
    let top_right = source[y0 * width + x1];
    let bottom_left = source[y1 * width + x0];
    let bottom_right = source[y1 * width + x1];
    let mut sample = [0.0_f32; 4];
    for channel in 0..4 {
        let top = top_left[channel] + (top_right[channel] - top_left[channel]) * frac_x;
        let bottom = bottom_left[channel] + (bottom_right[channel] - bottom_left[channel]) * frac_x;
        sample[channel] = top + (bottom - top) * frac_y;
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize, background: [f32; 4]) -> Vec<[f32; 4]> {
        vec![background; width * height]
    }

    fn grade(amount: f32, mode: RadialBlurMode) -> RadialBlurGrade {
        RadialBlurGrade::new(amount, [3.0, 3.0], mode, false, 0).expect("valid radial blur")
    }

    #[test]
    fn new_rejects_values_the_schema_cannot_author() {
        assert_eq!(
            RadialBlurGrade::new(-0.5, [0.0, 0.0], RadialBlurMode::Spin, false, 0).unwrap_err(),
            RadialBlurError::Amount
        );
        assert_eq!(
            RadialBlurGrade::new(1_000.5, [0.0, 0.0], RadialBlurMode::Spin, false, 0).unwrap_err(),
            RadialBlurError::Amount
        );
        assert_eq!(
            RadialBlurGrade::new(f32::NAN, [0.0, 0.0], RadialBlurMode::Spin, false, 0).unwrap_err(),
            RadialBlurError::Amount
        );
        assert_eq!(
            RadialBlurGrade::new(10.0, [0.0, f32::INFINITY], RadialBlurMode::Spin, false, 0)
                .unwrap_err(),
            RadialBlurError::Centre
        );
    }

    #[test]
    fn menu_values_map_onto_the_declared_modes() {
        assert_eq!(RadialBlurMode::from_menu(1), Some(RadialBlurMode::Spin));
        assert_eq!(RadialBlurMode::from_menu(2), Some(RadialBlurMode::Zoom));
        assert_eq!(RadialBlurMode::from_menu(0), None);
        assert_eq!(RadialBlurMode::from_menu(3), None);
    }

    #[test]
    fn taps_follow_the_amount_and_double_for_antialiasing() {
        assert_eq!(grade(0.0, RadialBlurMode::Spin).taps(), 1);
        assert_eq!(grade(1.0, RadialBlurMode::Spin).taps(), 3);
        assert_eq!(grade(12.0, RadialBlurMode::Spin).taps(), 5);
        let antialiased =
            RadialBlurGrade::new(12.0, [0.0, 0.0], RadialBlurMode::Spin, true, 0).expect("grade");
        assert_eq!(antialiased.taps(), 10);
        assert_eq!(
            grade(1_000.0, RadialBlurMode::Spin).taps(),
            RADIAL_BLUR_MAX_TAPS
        );
        let capped = RadialBlurGrade::new(1_000.0, [0.0, 0.0], RadialBlurMode::Spin, true, 0)
            .expect("grade");
        assert_eq!(capped.taps(), RADIAL_BLUR_MAX_TAPS);
    }

    #[test]
    fn zero_amount_is_the_identity() {
        let mut pixels = frame(5, 5, [0.25, 0.5, 0.75, 1.0]);
        pixels[7] = [2.0, -1.0, 0.5, 1.0];
        let before = pixels.clone();
        assert!(grade(0.0, RadialBlurMode::Spin).is_identity());
        assert!(radial_blur_frame_f32(
            &mut pixels,
            5,
            5,
            grade(0.0, RadialBlurMode::Spin)
        ));
        assert_eq!(pixels, before);
    }

    #[test]
    fn spin_keeps_the_centre_and_its_radius() {
        let mut pixels = frame(9, 7, [0.0, 0.0, 0.0, 1.0]);
        pixels[3 * 9 + 6] = [1.0, 1.0, 1.0, 1.0];
        let centre_only = {
            let mut probe = pixels.clone();
            assert!(radial_blur_frame_f32(
                &mut probe,
                9,
                7,
                grade(250.0, RadialBlurMode::Spin)
            ));
            probe
        };
        // The centre pixel never moves: every sample is the same distance zero.
        assert_eq!(centre_only[3 * 9 + 3], [0.0, 0.0, 0.0, 1.0]);
        // Energy travels along the arc: a rotated neighbour lights up.
        assert!(centre_only[4 * 9 + 6][0] > 0.05);
        // Spin preserves the radius, so the pixel further out on the same ray stays dark.
        assert_eq!(centre_only[3 * 9 + 8], [0.0, 0.0, 0.0, 1.0]);
        // The source pixel keeps part of its energy instead of disappearing.
        assert!(centre_only[3 * 9 + 6][0] < 1.0);
    }

    #[test]
    fn zoom_smears_along_the_ray_and_never_across_it() {
        let mut pixels = frame(9, 7, [0.0, 0.0, 0.0, 1.0]);
        pixels[3 * 9 + 6] = [1.0, 1.0, 1.0, 1.0];
        let zoomed = {
            let mut probe = pixels.clone();
            assert!(radial_blur_frame_f32(
                &mut probe,
                9,
                7,
                grade(250.0, RadialBlurMode::Zoom)
            ));
            probe
        };
        // Zoom changes the radius: the pixel further out on the ray now carries light.
        assert!(zoomed[3 * 9 + 7][0] > 0.05);
        // Zoom preserves the angle, so a neighbour off the ray stays dark. It has
        // to be two rows away: bilinear sampling does read the adjacent row.
        assert_eq!(zoomed[5 * 9 + 6], [0.0, 0.0, 0.0, 1.0]);
        // The centre itself has no radius to scale.
        assert_eq!(zoomed[3 * 9 + 3], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn jitter_is_deterministic_and_seed_dependent() {
        assert_eq!(radial_blur_jitter(7, 3, 4), radial_blur_jitter(7, 3, 4));
        assert_ne!(radial_blur_jitter(7, 3, 4), radial_blur_jitter(8, 3, 4));
        for x in 0..16 {
            for y in 0..16 {
                let value = radial_blur_jitter(1, x, y);
                assert!((-0.5..=0.5).contains(&value), "jitter {value} out of range");
            }
        }
        let mut first = frame(9, 7, [0.0, 0.0, 0.0, 1.0]);
        first[3 * 9 + 6] = [1.0, 1.0, 1.0, 1.0];
        let mut second = first.clone();
        let seeded =
            RadialBlurGrade::new(120.0, [3.0, 3.0], RadialBlurMode::Spin, true, 9).expect("grade");
        let other =
            RadialBlurGrade::new(120.0, [3.0, 3.0], RadialBlurMode::Spin, true, 10).expect("grade");
        assert!(radial_blur_frame_f32(&mut first, 9, 7, seeded));
        assert!(radial_blur_frame_f32(&mut second, 9, 7, seeded));
        assert_eq!(first, second, "the same seed must reproduce the frame");
        let mut third = second;
        assert!(radial_blur_frame_f32(&mut third, 9, 7, other));
        assert_ne!(first, third, "a different seed must move the samples");
    }

    #[test]
    fn transparent_neighbours_do_not_pull_the_visible_colour_down() {
        let mut pixels = frame(9, 7, [0.0, 0.0, 0.0, 0.0]);
        pixels[3 * 9 + 6] = [1.0, 0.5, 0.25, 1.0];
        assert!(radial_blur_frame_f32(
            &mut pixels,
            9,
            7,
            grade(250.0, RadialBlurMode::Spin)
        ));
        // Every output colour is a coverage-weighted average of the one authored
        // colour, so straight alpha can only fade towards that colour.
        for pixel in &pixels {
            assert!((0.0..=1.0).contains(&pixel[3]));
            assert!(pixel[0] <= 1.0 + 1.0e-6);
            assert!(pixel[1] <= 0.5 + 1.0e-6);
        }
        let kept = pixels[3 * 9 + 6];
        // Straight alpha survives: the kept colour is unchanged even though the
        // average coverage of the pixel dropped.
        assert!((kept[0] - 1.0).abs() < 1.0e-5);
        assert!((kept[1] - 0.5).abs() < 1.0e-5);
        assert!(kept[3] <= 1.0);
    }

    #[test]
    fn extended_range_values_are_not_clamped() {
        let mut pixels = frame(9, 7, [0.0, 0.0, 0.0, 1.0]);
        pixels[3 * 9 + 6] = [4.0, -1.0, 0.5, 1.0];
        assert!(radial_blur_frame_f32(
            &mut pixels,
            9,
            7,
            grade(250.0, RadialBlurMode::Spin)
        ));
        let source = pixels[3 * 9 + 6];
        // A quarter of the sweep lands on the source pixel, so it keeps a
        // proportionate share of both the bright and the negative channel
        // instead of being clamped into 0..1.
        assert!(source[0] > 0.5, "bright energy must survive: {source:?}");
        assert!(source[1] < 0.0, "negative energy must survive: {source:?}");
        assert!(pixels[4 * 9 + 6][0] > 0.0);
    }

    #[test]
    fn the_whole_frame_entry_matches_the_controlled_path() {
        let mut direct = frame(9, 7, [0.1, 0.2, 0.3, 1.0]);
        direct[3 * 9 + 6] = [1.0, 1.0, 1.0, 1.0];
        let mut controlled = direct.clone();
        let value = grade(180.0, RadialBlurMode::Zoom);
        assert!(radial_blur_frame_f32(&mut direct, 9, 7, value));
        let mut counter = 0;
        let mut checkpoint = || {
            counter += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_radial_blur_f32_controlled(&mut controlled, 9, 7, value, &mut checkpoint)
                .expect("checkpoint never fails")
        );
        assert_eq!(direct, controlled);
        assert!(counter > 0);
    }

    #[test]
    fn mismatched_dimensions_are_rejected() {
        let mut pixels = frame(3, 3, [0.0, 0.0, 0.0, 1.0]);
        assert!(!radial_blur_frame_f32(
            &mut pixels,
            4,
            3,
            grade(50.0, RadialBlurMode::Spin)
        ));
        let mut sized = frame(9, 7, [0.0, 0.0, 0.0, 1.0]);
        assert!(!radial_blur_frame_f32(
            &mut sized,
            9,
            0,
            grade(50.0, RadialBlurMode::Spin)
        ));
    }
}
