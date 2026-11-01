//! Ripple distortion (DST-02 in the effect list).
//!
//! The effect pushes the frame in and out along the radius from an authored
//! centre, so the picture appears to ripple like the surface of water. The list
//! declares a radius, a centre, a wave type, a wave speed, a wave width, a wave
//! height and a phase, but it does not say how they combine; this module fixes
//! that reading:
//!
//! - The radius is a percentage of the shorter frame side, like the twirl and
//!   spherize radii, and the displacement falls to nothing at the rim with the
//!   `(1 - d^2/r^2)^2` envelope that keeps the boundary `C^1`.
//! - The wave width is the wavelength in pixels and the wave height is the
//!   radial displacement in pixels at the crest.
//! - The phase is measured in turns, and the wave speed in turns per second
//!   advances it over time: the graph builder folds `phase + speed * seconds`
//!   into the grade, so the kernel itself stays a pure function of one frame.
//!   A positive speed moves the crests outward, away from the centre.
//! - The two wave types differ in how the crest and the trough are seated on
//!   the frame: `Symmetric` moves the sample both inward and outward by half
//!   the height on either side of the authored position, while `Asymmetric`
//!   only ever pushes outward, so the trough becomes a pause instead of a pull.
//!
//! The mapping is a pure inverse resampling — destination pixel in, source
//! pixel out — which keeps the kernel free of state and independent of the
//! processing order. Colour is premultiplied before the samples are blended and
//! unpremultiplied afterwards, so transparent neighbours cannot tint a visible
//! pixel, and coverage is capped at one by the shared unpremultiply helper.

use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest authored radius, as a percentage of the shorter frame side.
pub const RIPPLE_MAX_RADIUS_PERCENT: f32 = 100.0;

/// Smallest authored radius.
pub const RIPPLE_MIN_RADIUS_PERCENT: f32 = 0.0;

/// Largest magnitude accepted for the authored wave speed, in turns per second.
pub const RIPPLE_MAX_WAVE_SPEED: f32 = 15.0;

/// Shortest accepted wavelength, in pixels.
pub const RIPPLE_MIN_WAVE_WIDTH: f32 = 2.0;

/// Longest accepted wavelength, in pixels.
pub const RIPPLE_MAX_WAVE_WIDTH: f32 = 100.0;

/// Largest accepted crest displacement, in pixels.
pub const RIPPLE_MAX_WAVE_HEIGHT: f32 = 400.0;

/// Largest magnitude accepted for the authored phase, in turns.
///
/// The list leaves the phase unbounded. The cap keeps `phase * TAU` exact
/// enough to round-trip between the CPU and GPU paths in `f32`.
pub const RIPPLE_MAX_PHASE_TURNS: f32 = 1_000_000.0;

/// Largest magnitude accepted for a centre coordinate, in pixels.
pub const RIPPLE_MAX_CENTRE_PIXELS: f32 = 1_000_000.0;

/// Number of wave types the effect list declares.
pub const RIPPLE_TYPE_COUNT: usize = 2;

/// How the wave seats its crest and trough on the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RippleType {
    /// Only pushes outward; the trough becomes a pause instead of a pull.
    Asymmetric,
    /// Pushes outward and inward by half the height around the authored place.
    Symmetric,
}

impl RippleType {
    /// Every wave type, in the order the effect list declares them.
    pub const ALL: [Self; RIPPLE_TYPE_COUNT] = [Self::Asymmetric, Self::Symmetric];

    /// Returns the stable authoring key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Asymmetric => "asymmetric",
            Self::Symmetric => "symmetric",
        }
    }

    /// Returns the one-based menu value the effect list declares.
    pub const fn as_menu(self) -> i64 {
        match self {
            Self::Asymmetric => 1,
            Self::Symmetric => 2,
        }
    }

    /// Returns the value the GPU control block carries.
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Asymmetric => 1,
            Self::Symmetric => 2,
        }
    }

    /// Resolves a stable authoring key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|wave_type| wave_type.as_key() == key)
    }

    /// Resolves a one-based menu value.
    pub fn from_menu(menu: i64) -> Option<Self> {
        Self::ALL.into_iter().find(|wave_type| wave_type.as_menu() == menu)
    }
}

/// Reasons a ripple grade can be rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RippleError {
    /// The radius was not finite or outside the declared percentage range.
    Radius,
    /// The centre was not a finite point or out of range.
    Centre,
    /// The wave speed was not finite or exceeded the accepted magnitude.
    WaveSpeed,
    /// The wavelength was not finite or outside the declared range.
    WaveWidth,
    /// The crest displacement was not finite or outside the declared range.
    WaveHeight,
    /// The phase was not finite or exceeded the accepted magnitude.
    Phase,
}

impl std::fmt::Display for RippleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Radius => write!(
                formatter,
                "ripple radius must be within {RIPPLE_MIN_RADIUS_PERCENT}..{RIPPLE_MAX_RADIUS_PERCENT} percent"
            ),
            Self::Centre => formatter.write_str("ripple centre must be a finite point"),
            Self::WaveSpeed => write!(
                formatter,
                "ripple wave speed must be within +/-{RIPPLE_MAX_WAVE_SPEED} turns per second"
            ),
            Self::WaveWidth => write!(
                formatter,
                "ripple wave width must be within {RIPPLE_MIN_WAVE_WIDTH}..{RIPPLE_MAX_WAVE_WIDTH} pixels"
            ),
            Self::WaveHeight => write!(
                formatter,
                "ripple wave height must be within 0..={RIPPLE_MAX_WAVE_HEIGHT} pixels"
            ),
            Self::Phase => write!(
                formatter,
                "ripple phase must be within +/-{RIPPLE_MAX_PHASE_TURNS} turns"
            ),
        }
    }
}

impl std::error::Error for RippleError {}

/// Resolves the phase the kernel sees from the authored phase, the wave speed
/// and the frame time.
///
/// The builder calls this per evaluated time, so the kernel never needs a clock.
/// The result is folded into `[0, 1)` turns: the wave is periodic, and keeping
/// the phase small keeps the `sin` argument precise in `f32` on the GPU, which
/// is what lets the per-pixel parity test compare the two paths directly.
pub fn ripple_phase_turns(authored_phase: f32, wave_speed: f32, seconds: f64) -> f64 {
    (f64::from(authored_phase) + f64::from(wave_speed) * seconds).rem_euclid(1.0)
}

/// Validates the authored wave speed.
///
/// # Errors
///
/// Returns [`RippleError::WaveSpeed`] when the speed is not finite or exceeds
/// the magnitude the effect list declares.
pub fn validate_wave_speed(wave_speed: f32) -> Result<f32, RippleError> {
    if !wave_speed.is_finite() || wave_speed.abs() > RIPPLE_MAX_WAVE_SPEED {
        return Err(RippleError::WaveSpeed);
    }
    Ok(wave_speed)
}

/// A validated ripple configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RippleGrade {
    radius_fraction: f32,
    centre: [f32; 2],
    wave_type: RippleType,
    wave_width: f32,
    wave_height: f32,
    phase: f32,
}

impl RippleGrade {
    /// Validates an authoring configuration.
    ///
    /// `phase_turns` already carries the wave speed folded in over time; the
    /// speed itself only matters while the graph is built.
    ///
    /// # Errors
    ///
    /// Returns the first [`RippleError`] the author state violates.
    pub fn new(
        radius_percent: f32,
        centre: [f32; 2],
        wave_type: RippleType,
        wave_width: f32,
        wave_height: f32,
        phase_turns: f32,
    ) -> Result<Self, RippleError> {
        if !radius_percent.is_finite()
            || !(RIPPLE_MIN_RADIUS_PERCENT..=RIPPLE_MAX_RADIUS_PERCENT).contains(&radius_percent)
        {
            return Err(RippleError::Radius);
        }
        if centre.iter().any(|coordinate| {
            !coordinate.is_finite() || coordinate.abs() > RIPPLE_MAX_CENTRE_PIXELS
        }) {
            return Err(RippleError::Centre);
        }
        if !wave_width.is_finite()
            || !(RIPPLE_MIN_WAVE_WIDTH..=RIPPLE_MAX_WAVE_WIDTH).contains(&wave_width)
        {
            return Err(RippleError::WaveWidth);
        }
        if !wave_height.is_finite() || !(0.0..=RIPPLE_MAX_WAVE_HEIGHT).contains(&wave_height) {
            return Err(RippleError::WaveHeight);
        }
        if !phase_turns.is_finite() || phase_turns.abs() > RIPPLE_MAX_PHASE_TURNS {
            return Err(RippleError::Phase);
        }
        Ok(Self {
            radius_fraction: radius_percent / RIPPLE_MAX_RADIUS_PERCENT,
            centre,
            wave_type,
            wave_width,
            wave_height,
            phase: phase_turns,
        })
    }

    /// Returns the radius of influence as a percentage of the shorter side.
    pub fn radius_percent(&self) -> f32 {
        self.radius_fraction * RIPPLE_MAX_RADIUS_PERCENT
    }

    /// Returns the radius of influence as a fraction of the shorter side.
    pub fn radius_fraction(&self) -> f32 {
        self.radius_fraction
    }

    /// Returns the centre of the ripple in frame pixels.
    pub fn centre(&self) -> [f32; 2] {
        self.centre
    }

    /// Returns the authored wave type.
    pub fn wave_type(&self) -> RippleType {
        self.wave_type
    }

    /// Returns the wavelength in pixels.
    pub fn wave_width(&self) -> f32 {
        self.wave_width
    }

    /// Returns the crest displacement in pixels.
    pub fn wave_height(&self) -> f32 {
        self.wave_height
    }

    /// Returns the phase in turns.
    pub fn phase_turns(&self) -> f32 {
        self.phase
    }

    /// `true` when the effect cannot change any pixel.
    pub fn is_identity(&self) -> bool {
        self.radius_fraction <= 0.0 || self.wave_height <= 1.0e-6
    }

    /// Radius of influence in pixels for a frame of `width` x `height`.
    pub fn radius_pixels(&self, width: usize, height: usize) -> f32 {
        self.radius_fraction * width.min(height) as f32
    }

    /// Returns the radial displacement, in pixels, of a destination pixel that
    /// sits `distance` pixels from the centre of a ripple whose influence
    /// reaches `radius` pixels.
    ///
    /// The sign is the direction the source sample is taken from: a positive
    /// value reads outward, beyond the destination radius.
    pub fn radial_displacement(&self, distance: f64, radius: f64) -> f64 {
        if radius <= 0.0 || distance >= radius || self.wave_height <= 0.0 {
            return 0.0;
        }
        let normalized = distance / radius;
        let envelope = (1.0 - normalized * normalized).powi(2);
        let angle =
            std::f64::consts::TAU * (distance / f64::from(self.wave_width) - f64::from(self.phase));
        let wave = match self.wave_type {
            RippleType::Asymmetric => 0.5 + 0.5 * angle.sin(),
            RippleType::Symmetric => angle.sin(),
        };
        f64::from(self.wave_height) * envelope * wave
    }

    /// Uniform packing for the GPU pass.
    ///
    /// The first block carries the centre, the radius fraction and the wave
    /// type menu value; the second carries the wavelength, the crest
    /// displacement and the phase in turns.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.centre[0],
                self.centre[1],
                self.radius_fraction,
                self.wave_type.as_u32() as f32,
            ],
            [self.wave_width, self.wave_height, self.phase, 0.0],
        )
    }
}

/// Applies the ripple to a whole frame, returning `false` when the frame cannot
/// be processed.
pub fn ripple_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: RippleGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_ripple_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the ripple in place with an explicit cancellation checkpoint.
pub(crate) fn apply_ripple_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: RippleGrade,
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
    premultiply_rgba_controlled(frame, checkpoint)?;

    let source = frame.to_vec();
    let centre_x = f64::from(grade.centre[0]);
    let centre_y = f64::from(grade.centre[1]);
    let radius = f64::from(grade.radius_pixels(width, height));

    checkpoint()?;
    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let delta_x = column as f64 - centre_x;
                let delta_y = row as f64 - centre_y;
                let distance = delta_x.hypot(delta_y);
                // The exact centre has no direction to displace along, and the
                // envelope already silences everything at or beyond the rim.
                if distance <= 0.0 || distance >= radius {
                    continue;
                }
                let displacement = grade.radial_displacement(distance, radius);
                let scale = (distance + displacement) / distance;
                let sample_x = centre_x + delta_x * scale;
                let sample_y = centre_y + delta_y * scale;
                *pixel = sample_bilinear_clamped(&source, width, height, sample_x, sample_y);
            }
        }
    });

    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for (index, pixel) in pixels.iter_mut().enumerate() {
            let phase = (index % 23) as f32 * 0.037;
            *pixel = [phase, 0.3 + phase * 0.5, 0.85 - phase, 1.0];
        }
        pixels
    }

    fn grade(radius_percent: f32, centre: [f32; 2]) -> RippleGrade {
        RippleGrade::new(
            radius_percent,
            centre,
            RippleType::Symmetric,
            20.0,
            20.0,
            0.0,
        )
        .expect("valid ripple grade")
    }

    #[test]
    fn rejects_author_state_outside_the_declared_ranges() {
        let centre = [4.0, 4.0];
        let build = |radius, centre, wave_type, width, height, phase| {
            RippleGrade::new(radius, centre, wave_type, width, height, phase)
        };
        let symmetric = RippleType::Symmetric;
        assert_eq!(
            build(-1.0, centre, symmetric, 20.0, 20.0, 0.0),
            Err(RippleError::Radius)
        );
        assert_eq!(
            build(101.0, centre, symmetric, 20.0, 20.0, 0.0),
            Err(RippleError::Radius)
        );
        assert_eq!(
            build(50.0, [f32::NAN, 0.0], symmetric, 20.0, 20.0, 0.0),
            Err(RippleError::Centre)
        );
        assert_eq!(
            build(
                50.0,
                [RIPPLE_MAX_CENTRE_PIXELS * 2.0, 0.0],
                symmetric,
                20.0,
                20.0,
                0.0
            ),
            Err(RippleError::Centre)
        );
        assert_eq!(
            build(50.0, centre, symmetric, 1.0, 20.0, 0.0),
            Err(RippleError::WaveWidth)
        );
        assert_eq!(
            build(50.0, centre, symmetric, 101.0, 20.0, 0.0),
            Err(RippleError::WaveWidth)
        );
        assert_eq!(
            build(50.0, centre, symmetric, 20.0, -1.0, 0.0),
            Err(RippleError::WaveHeight)
        );
        assert_eq!(
            build(50.0, centre, symmetric, 20.0, 401.0, 0.0),
            Err(RippleError::WaveHeight)
        );
        assert_eq!(
            build(50.0, centre, symmetric, 20.0, 20.0, f32::NAN),
            Err(RippleError::Phase)
        );
        assert_eq!(
            build(
                50.0,
                centre,
                symmetric,
                20.0,
                20.0,
                RIPPLE_MAX_PHASE_TURNS * 2.0
            ),
            Err(RippleError::Phase)
        );
        assert!(build(50.0, centre, symmetric, 20.0, 20.0, 0.0).is_ok());
    }

    #[test]
    fn the_wave_type_menus_round_trip() {
        for wave_type in RippleType::ALL {
            assert_eq!(RippleType::from_key(wave_type.as_key()), Some(wave_type));
            assert_eq!(RippleType::from_menu(wave_type.as_menu()), Some(wave_type));
            assert_eq!(wave_type.as_u32() as i64, wave_type.as_menu());
        }
        assert_eq!(RippleType::ALL.len(), RIPPLE_TYPE_COUNT);
        assert_eq!(RippleType::from_menu(3), None);
        assert_eq!(RippleType::from_key("triangle"), None);
    }

    #[test]
    fn zero_radius_and_zero_height_are_identity() {
        assert!(grade(0.0, [4.0, 4.0]).is_identity());
        let flat = RippleGrade::new(50.0, [4.0, 4.0], RippleType::Symmetric, 20.0, 0.0, 0.0)
            .expect("valid grade");
        assert!(flat.is_identity());

        let original = frame(11, 9);
        let mut pixels = original.clone();
        assert!(ripple_frame_f32(&mut pixels, 11, 9, grade(0.0, [5.0, 4.0])));
        assert_eq!(pixels, original);
    }

    #[test]
    fn radius_percent_scales_with_the_shorter_frame_side() {
        let grade = grade(40.0, [0.0, 0.0]);
        assert!((grade.radius_pixels(100, 50) - 20.0).abs() < 1.0e-5);
        assert!((grade.radius_pixels(50, 100) - 20.0).abs() < 1.0e-5);
        assert!((grade.radius_pixels(50, 50) - 20.0).abs() < 1.0e-5);
    }

    #[test]
    fn pixels_beyond_the_radius_are_untouched() {
        let width = 11;
        let height = 11;
        let mut pixels = frame(width, height);
        let original = pixels.clone();
        // Radius 30% of the shorter side (11 px) is 3.3 px around the centre.
        assert!(ripple_frame_f32(
            &mut pixels,
            width,
            height,
            grade(30.0, [5.0, 5.0])
        ));
        for (index, (before, after)) in original.iter().zip(pixels.iter()).enumerate() {
            let column = index % width;
            let row = index / width;
            let distance = ((column as f64 - 5.0).powi(2) + (row as f64 - 5.0).powi(2)).sqrt();
            if distance >= 3.3 {
                assert_eq!(before, after, "pixel {index} outside the radius moved");
            }
        }
    }

    #[test]
    fn the_exact_centre_keeps_its_own_sample() {
        let mut pixels = frame(11, 11);
        let original = pixels.clone();
        assert!(ripple_frame_f32(
            &mut pixels,
            11,
            11,
            grade(100.0, [5.0, 5.0])
        ));
        assert_eq!(pixels[5 * 11 + 5], original[5 * 11 + 5]);
    }

    #[test]
    fn a_symmetric_wave_displaces_inward_and_outward() {
        let grade = RippleGrade::new(100.0, [0.0, 0.0], RippleType::Symmetric, 20.0, 20.0, 0.0)
            .expect("valid grade");
        let radius = 100.0;
        // A quarter wavelength out is the crest, three quarters out the trough.
        assert!(grade.radial_displacement(5.0, radius) > 1.0);
        assert!(grade.radial_displacement(15.0, radius) < -1.0);
        assert!(grade.radial_displacement(10.0, radius).abs() < 1.0e-9);
    }

    #[test]
    fn an_asymmetric_wave_only_pushes_outward() {
        let grade = RippleGrade::new(100.0, [0.0, 0.0], RippleType::Asymmetric, 20.0, 20.0, 0.0)
            .expect("valid grade");
        let mut minimum = f64::INFINITY;
        let mut maximum = f64::NEG_INFINITY;
        for step in 1..1000 {
            let distance = f64::from(step) * 0.1;
            let displacement = grade.radial_displacement(distance, 100.0);
            minimum = minimum.min(displacement);
            maximum = maximum.max(displacement);
        }
        assert!(minimum >= 0.0, "an asymmetric ripple pulled inward");
        assert!(maximum > 1.0, "an asymmetric ripple never pushed outward");
    }

    #[test]
    fn the_pattern_repeats_every_authored_wavelength() {
        let grade = RippleGrade::new(100.0, [0.0, 0.0], RippleType::Symmetric, 16.0, 30.0, 0.35)
            .expect("valid grade");
        // A radius this large keeps the envelope at one, so only the wave remains.
        let radius = 1.0e6;
        for distance in [3.0_f64, 11.0, 27.5, 40.25] {
            let here = grade.radial_displacement(distance, radius);
            let next = grade.radial_displacement(distance + 16.0, radius);
            assert!(
                (here - next).abs() <= 1.0e-6,
                "displacement at {distance} was {here} but one wavelength out was {next}"
            );
        }
    }

    #[test]
    fn the_phase_advances_with_the_wave_speed_over_time() {
        assert!((ripple_phase_turns(0.0, 1.0, 0.25) - 0.25).abs() <= 1.0e-9);
        assert!((ripple_phase_turns(0.5, -2.0, 0.25) - 0.0).abs() <= 1.0e-9);
        assert!((ripple_phase_turns(0.25, 0.0, 99.0) - 0.25).abs() <= 1.0e-9);
        // Whole turns fold away, so a long timeline stays in `[0, 1)`. The
        // authored phase is an `f32`, so the folded result carries its rounding.
        assert!((ripple_phase_turns(0.9, 1.0, 0.5) - 0.4).abs() <= 1.0e-6);
        assert!((ripple_phase_turns(0.1, -1.0, 0.25) - 0.85).abs() <= 1.0e-6);
        assert!(validate_wave_speed(15.0).is_ok());
        assert_eq!(validate_wave_speed(15.5), Err(RippleError::WaveSpeed));
        assert_eq!(validate_wave_speed(f32::NAN), Err(RippleError::WaveSpeed));
    }

    #[test]
    fn sampling_outside_the_frame_clamps_to_the_edge() {
        let mut pixels = vec![[0.2, 0.4, 0.6, 1.0]; 9 * 9];
        let grade = RippleGrade::new(100.0, [0.0, 0.0], RippleType::Symmetric, 2.0, 400.0, 0.0)
            .expect("valid grade");
        assert!(ripple_frame_f32(&mut pixels, 9, 9, grade));
        for pixel in &pixels {
            for value in pixel {
                assert!(value.is_finite(), "the kernel produced a non-finite value");
            }
            assert!(pixel[3] >= 0.0 && pixel[3] <= 1.0);
        }
    }

    #[test]
    fn the_kernel_mixes_premultiplied_colour() {
        // Four texels around the sample point: a partly transparent red, an
        // opaque blue and two empty neighbours. Sampling their exact midpoint
        // gives every texel a quarter weight.
        let mut pixels = vec![[0.0_f32; 4]; 400 * 400];
        pixels[2 * 400 + 2] = [1.0, 0.0, 0.0, 0.25];
        pixels[3 * 400 + 3] = [0.0, 0.0, 1.0, 1.0];

        // Sample (2.5, 2.5) is 1.5 * sqrt(2) pixels further out than the
        // destination pixel (1, 1), whose distance is sqrt(2).
        let radius = 400.0_f64;
        let destination = 2.0_f64.sqrt();
        let displacement = 1.5 * destination;
        let envelope = (1.0 - (destination / radius).powi(2)).powi(2);
        let height = (displacement / envelope) as f32;
        // r / wavelength - phase = 0.25 puts the crest exactly on the sample.
        let phase = (destination / 4.0 - 0.25) as f32;
        let grade = RippleGrade::new(100.0, [0.0, 0.0], RippleType::Symmetric, 4.0, height, phase)
            .expect("valid grade");
        assert!(
            (grade.radial_displacement(destination, radius) - displacement).abs() <= 1.0e-3,
            "the test must land the sample on the midpoint of four texels"
        );

        assert!(ripple_frame_f32(&mut pixels, 400, 400, grade));
        let mixed = pixels[400 + 1];
        // Premultiplied: 0.25 * [0.25, 0, 0] + 0.25 * [0, 0, 1] = [0.0625, 0, 0.25]
        // over coverage 0.25 * 0.25 + 0.25 * 1 = 0.3125.
        assert!((mixed[0] - 0.2).abs() <= 1.0e-3, "red was {}", mixed[0]);
        assert!((mixed[2] - 0.8).abs() <= 1.0e-3, "blue was {}", mixed[2]);
        assert!(
            (mixed[3] - 0.3125).abs() <= 1.0e-3,
            "coverage was {}",
            mixed[3]
        );
    }

    #[test]
    fn extended_range_values_survive_a_ripple() {
        let mut pixels = vec![[2.5, -0.75, 1.25, 1.0]; 9 * 9];
        assert!(ripple_frame_f32(
            &mut pixels,
            9,
            9,
            grade(100.0, [4.0, 4.0])
        ));
        for pixel in &pixels {
            assert!((pixel[0] - 2.5).abs() <= 1.0e-5);
            assert!((pixel[1] + 0.75).abs() <= 1.0e-5);
            assert!((pixel[2] - 1.25).abs() <= 1.0e-5);
        }
    }

    #[test]
    fn the_whole_frame_entry_agrees_with_the_controlled_entry() {
        let mut through_entry = frame(11, 9);
        let mut through_control = frame(11, 9);
        let grade = RippleGrade::new(70.0, [5.5, 4.25], RippleType::Asymmetric, 7.0, 12.5, 0.3)
            .expect("valid grade");
        let entry_changed = ripple_frame_f32(&mut through_entry, 11, 9, grade);
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        let control_changed =
            apply_ripple_f32_controlled(&mut through_control, 11, 9, grade, &mut checkpoint)
                .expect("the checkpoint never fails");
        assert_eq!(entry_changed, control_changed);
        assert_eq!(through_entry, through_control);
    }

    #[test]
    fn empty_geometry_is_rejected() {
        let mut pixels: Vec<[f32; 4]> = Vec::new();
        assert!(!ripple_frame_f32(
            &mut pixels,
            0,
            0,
            grade(50.0, [0.0, 0.0])
        ));
        let mut short = vec![[0.0_f32; 4]; 3];
        assert!(!ripple_frame_f32(&mut short, 3, 3, grade(50.0, [1.0, 1.0])));
    }

    #[test]
    fn the_gpu_controls_carry_the_centre_radius_and_wave() {
        let grade = RippleGrade::new(60.0, [12.5, 7.25], RippleType::Asymmetric, 18.0, 9.5, 0.4)
            .expect("valid grade");
        let (params, extra) = grade.gpu_controls();
        assert_eq!(params[0], 12.5);
        assert_eq!(params[1], 7.25);
        assert!((params[2] - 0.6).abs() <= 1.0e-6);
        assert_eq!(params[3], RippleType::Asymmetric.as_u32() as f32);
        assert_eq!(extra[0], 18.0);
        assert_eq!(extra[1], 9.5);
        assert!((extra[2] - 0.4).abs() <= 1.0e-6);
        assert_eq!(extra[3], 0.0);
    }
}
