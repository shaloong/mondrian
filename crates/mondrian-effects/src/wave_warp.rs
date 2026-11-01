//! Wave Warp (DST-03): displaces the frame along one travelling regular wave.
//!
//! # Semantics
//!
//! Every output pixel samples the source at
//!
//! ```text
//! p' = p + axis * (wave_height * shape(t / wave_width - phase_turns)) * pin(p)
//! t  = dot(p, axis)
//! ```
//!
//! where `axis = (sin θ, -cos θ)` for `direction` degrees measured in the
//! author-facing convention: `0` moves the wave up the frame (bottom to top)
//! and `90` (the default) moves it left to right. Because the wave is a
//! function of `t = dot(p, axis)`, the wave *crests* are lines perpendicular to
//! `axis`: a 90 degree direction yields vertical crests that travel
//! horizontally, which is the behaviour the effect list documents.
//!
//! * `wave_height` is the crest displacement in pixels. Zero means identity.
//! * `wave_width` is the wavelength in pixels (one full period).
//! * `phase_turns` is the phase in turns, folded into `[0, 1)`.
//! * `wave_speed` authored in the UI is folded by the graph builder as
//!   `phase_turns + wave_speed * seconds`, so it never enters the kernel.
//! * `pin` damps the displacement near the chosen frame edges (or, for
//!   [`WavePin::Center`], near the frame centre) so those pixels stay put.
//! * `quality` supersamples the *source lookup* by an `n × n` grid; it changes
//!   how finely the distortion is resampled, not the wave itself.
//!
//! # Wave shapes
//!
//! The list declares nine wave types without labels, so the nine documented
//! After Effects shapes are implemented and each one is defined explicitly:
//! sine, square, triangle, sawtooth, circle (full arc), semicircle (half arc),
//! "non-circle" (inverted arc), noise (piecewise constant hash) and smooth
//! noise (cubic-interpolated hash). Noise uses an integer hash so the CPU and
//! the GPU compute bit-identical cells.

use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest accepted crest displacement in pixels.
pub const WAVE_WARP_MAX_HEIGHT: f32 = 32_000.0;
/// Smallest accepted crest displacement in pixels.
pub const WAVE_WARP_MIN_HEIGHT: f32 = -32_000.0;
/// Smallest accepted wavelength in pixels.
pub const WAVE_WARP_MIN_WIDTH: f32 = 1.0;
/// Largest accepted wavelength in pixels.
pub const WAVE_WARP_MAX_WIDTH: f32 = 32_000.0;
/// Largest accepted wave speed, in turns per second.
pub const WAVE_WARP_MAX_SPEED: f32 = 100.0;
/// Largest accepted authored phase, in degrees.
pub const WAVE_WARP_MAX_PHASE_DEGREES: f32 = 360_000_000.0;
/// Number of wave shapes.
pub const WAVE_WARP_TYPE_COUNT: usize = 9;
/// Number of edge pins.
pub const WAVE_WARP_PIN_COUNT: usize = 9;
/// Number of antialias qualities.
pub const WAVE_WARP_QUALITY_COUNT: usize = 3;
/// Fraction of the corresponding frame dimension over which a pin ramps in.
pub const WAVE_WARP_PIN_MARGIN: f64 = 0.2;
/// Radius of the centre pin, as a fraction of the shorter frame side.
pub const WAVE_WARP_CENTRE_PIN_FRACTION: f64 = 0.25;
const TAU: f64 = std::f64::consts::TAU;

/// The nine wave shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WaveType {
    Sine,
    Square,
    Triangle,
    Sawtooth,
    Circle,
    Semicircle,
    NonCircle,
    Noise,
    SmoothNoise,
}

impl WaveType {
    /// Every wave shape, in menu order.
    pub const ALL: [WaveType; WAVE_WARP_TYPE_COUNT] = [
        WaveType::Sine,
        WaveType::Square,
        WaveType::Triangle,
        WaveType::Sawtooth,
        WaveType::Circle,
        WaveType::Semicircle,
        WaveType::NonCircle,
        WaveType::Noise,
        WaveType::SmoothNoise,
    ];

    /// Stable authoring key.
    pub const fn as_key(self) -> &'static str {
        match self {
            WaveType::Sine => "sine",
            WaveType::Square => "square",
            WaveType::Triangle => "triangle",
            WaveType::Sawtooth => "sawtooth",
            WaveType::Circle => "circle",
            WaveType::Semicircle => "semicircle",
            WaveType::NonCircle => "non_circle",
            WaveType::Noise => "noise",
            WaveType::SmoothNoise => "smooth_noise",
        }
    }

    /// Resolves an authoring key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "sine" => Some(WaveType::Sine),
            "square" => Some(WaveType::Square),
            "triangle" => Some(WaveType::Triangle),
            "sawtooth" => Some(WaveType::Sawtooth),
            "circle" => Some(WaveType::Circle),
            "semicircle" => Some(WaveType::Semicircle),
            "non_circle" => Some(WaveType::NonCircle),
            "noise" => Some(WaveType::Noise),
            "smooth_noise" => Some(WaveType::SmoothNoise),
            _ => None,
        }
    }

    /// One-based menu value.
    pub fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Zero-based index, also used for the GPU uniform.
    pub const fn as_u32(self) -> u32 {
        match self {
            WaveType::Sine => 0,
            WaveType::Square => 1,
            WaveType::Triangle => 2,
            WaveType::Sawtooth => 3,
            WaveType::Circle => 4,
            WaveType::Semicircle => 5,
            WaveType::NonCircle => 6,
            WaveType::Noise => 7,
            WaveType::SmoothNoise => 8,
        }
    }

    /// Resolves a zero-based index.
    pub fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0 => WaveType::Sine,
            1 => WaveType::Square,
            2 => WaveType::Triangle,
            3 => WaveType::Sawtooth,
            4 => WaveType::Circle,
            5 => WaveType::Semicircle,
            6 => WaveType::NonCircle,
            7 => WaveType::Noise,
            8 => WaveType::SmoothNoise,
            _ => return None,
        })
    }
}

/// The frame edge a wave can be pinned to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WavePin {
    None,
    AllEdges,
    Center,
    LeftEdge,
    TopEdge,
    RightEdge,
    BottomEdge,
    HorizontalEdges,
    VerticalEdges,
}

impl WavePin {
    /// Every pin, in menu order.
    pub const ALL: [WavePin; WAVE_WARP_PIN_COUNT] = [
        WavePin::None,
        WavePin::AllEdges,
        WavePin::Center,
        WavePin::LeftEdge,
        WavePin::TopEdge,
        WavePin::RightEdge,
        WavePin::BottomEdge,
        WavePin::HorizontalEdges,
        WavePin::VerticalEdges,
    ];

    /// Stable authoring key.
    pub const fn as_key(self) -> &'static str {
        match self {
            WavePin::None => "none",
            WavePin::AllEdges => "all_edges",
            WavePin::Center => "center",
            WavePin::LeftEdge => "left_edge",
            WavePin::TopEdge => "top_edge",
            WavePin::RightEdge => "right_edge",
            WavePin::BottomEdge => "bottom_edge",
            WavePin::HorizontalEdges => "horizontal_edges",
            WavePin::VerticalEdges => "vertical_edges",
        }
    }

    /// Resolves an authoring key.
    pub fn from_key(key: &str) -> Option<Self> {
        WavePin::ALL.into_iter().find(|pin| pin.as_key() == key)
    }

    /// Zero-based index, also used for the GPU uniform.
    pub const fn as_u32(self) -> u32 {
        match self {
            WavePin::None => 0,
            WavePin::AllEdges => 1,
            WavePin::Center => 2,
            WavePin::LeftEdge => 3,
            WavePin::TopEdge => 4,
            WavePin::RightEdge => 5,
            WavePin::BottomEdge => 6,
            WavePin::HorizontalEdges => 7,
            WavePin::VerticalEdges => 8,
        }
    }
}

/// Antialias quality: the `n × n` source-lookup supersample grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WaveQuality {
    Low,
    Normal,
    High,
}

impl WaveQuality {
    /// Every quality, in menu order.
    pub const ALL: [WaveQuality; WAVE_WARP_QUALITY_COUNT] =
        [WaveQuality::Low, WaveQuality::Normal, WaveQuality::High];

    /// Stable authoring key.
    pub const fn as_key(self) -> &'static str {
        match self {
            WaveQuality::Low => "low",
            WaveQuality::Normal => "normal",
            WaveQuality::High => "high",
        }
    }

    /// Resolves an authoring key.
    pub fn from_key(key: &str) -> Option<Self> {
        WaveQuality::ALL.into_iter().find(|quality| quality.as_key() == key)
    }

    /// Zero-based index, also used for the GPU uniform.
    pub const fn as_u32(self) -> u32 {
        match self {
            WaveQuality::Low => 0,
            WaveQuality::Normal => 1,
            WaveQuality::High => 2,
        }
    }

    /// Samples taken per axis.
    pub const fn samples_per_axis(self) -> u32 {
        match self {
            WaveQuality::Low => 1,
            WaveQuality::Normal => 2,
            WaveQuality::High => 4,
        }
    }
}

/// Validation failures for a wave warp grade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaveWarpError {
    /// The crest displacement is not a finite value inside [-32000, 32000].
    Height,
    /// The wavelength is not a finite value inside [1, 32000].
    Width,
    /// The direction is not finite.
    Direction,
    /// The wave speed is not a finite value inside [-100, 100].
    Speed,
    /// The phase is not a finite value.
    Phase,
    /// The phase in turns is not finite.
    PhaseTurns,
}

impl std::fmt::Display for WaveWarpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            WaveWarpError::Height => "wave height must be a finite value inside [-32000, 32000]",
            WaveWarpError::Width => "wave width must be a finite value inside [1, 32000]",
            WaveWarpError::Direction => "wave direction must be a finite value",
            WaveWarpError::Speed => "wave speed must be a finite value inside [-100, 100]",
            WaveWarpError::Phase => "wave phase must be a finite value",
            WaveWarpError::PhaseTurns => "wave phase in turns must be finite",
        };
        formatter.write_str(message)
    }
}

/// Folds the authored phase, the wave speed and the frame time into one phase.
///
/// The result is in `[0, 1)` turns so the `sin` argument stays precise in f32
/// on the GPU.
pub fn wave_warp_phase_turns(authored_phase_degrees: f32, wave_speed: f32, seconds: f64) -> f64 {
    let authored = f64::from(authored_phase_degrees) / 360.0;
    (authored + f64::from(wave_speed) * seconds).rem_euclid(1.0)
}

/// Validates the authored wave speed.
pub fn validate_wave_speed(wave_speed: f32) -> Result<f32, WaveWarpError> {
    if !wave_speed.is_finite() || wave_speed.abs() > WAVE_WARP_MAX_SPEED {
        return Err(WaveWarpError::Speed);
    }
    Ok(wave_speed)
}

/// A validated wave-warp configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveWarpGrade {
    wave_type: WaveType,
    wave_height: f32,
    wave_width: f32,
    direction_degrees: f32,
    phase_turns: f32,
    pin: WavePin,
    quality: WaveQuality,
}

impl WaveWarpGrade {
    /// Builds a grade, rejecting values outside the documented ranges.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        wave_type: WaveType,
        wave_height: f32,
        wave_width: f32,
        direction_degrees: f32,
        phase_turns: f32,
        pin: WavePin,
        quality: WaveQuality,
    ) -> Result<Self, WaveWarpError> {
        if !wave_height.is_finite()
            || !(WAVE_WARP_MIN_HEIGHT..=WAVE_WARP_MAX_HEIGHT).contains(&wave_height)
        {
            return Err(WaveWarpError::Height);
        }
        if !wave_width.is_finite()
            || !(WAVE_WARP_MIN_WIDTH..=WAVE_WARP_MAX_WIDTH).contains(&wave_width)
        {
            return Err(WaveWarpError::Width);
        }
        if !direction_degrees.is_finite() {
            return Err(WaveWarpError::Direction);
        }
        if !phase_turns.is_finite() {
            return Err(WaveWarpError::PhaseTurns);
        }
        Ok(Self {
            wave_type,
            wave_height,
            wave_width,
            direction_degrees,
            phase_turns,
            pin,
            quality,
        })
    }

    /// The wave shape.
    pub const fn wave_type(&self) -> WaveType {
        self.wave_type
    }

    /// Crest displacement in pixels.
    pub const fn wave_height(&self) -> f32 {
        self.wave_height
    }

    /// Wavelength in pixels.
    pub const fn wave_width(&self) -> f32 {
        self.wave_width
    }

    /// Direction in degrees.
    pub const fn direction_degrees(&self) -> f32 {
        self.direction_degrees
    }

    /// Phase in turns, in `[0, 1)`.
    pub const fn phase_turns(&self) -> f32 {
        self.phase_turns
    }

    /// The pinned edge.
    pub const fn pin(&self) -> WavePin {
        self.pin
    }

    /// The antialias quality.
    pub const fn quality(&self) -> WaveQuality {
        self.quality
    }

    /// A zero crest displacement leaves the frame untouched.
    pub fn is_identity(&self) -> bool {
        self.wave_height == 0.0
    }

    /// The wave travel direction as a unit vector in frame coordinates.
    pub fn axis(&self) -> (f64, f64) {
        let radians = f64::from(self.direction_degrees).to_radians();
        (radians.sin(), -radians.cos())
    }

    /// Uniform packing for the GPU pass:
    /// `([wave_height, wave_width, direction_radians, phase_turns],
    ///   [wave_type, pin, samples_per_axis, 0])`.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.wave_height,
                self.wave_width,
                f64::to_radians(f64::from(self.direction_degrees)) as f32,
                self.phase_turns,
            ],
            [
                self.wave_type.as_u32() as f32,
                self.pin.as_u32() as f32,
                self.quality.samples_per_axis() as f32,
                0.0,
            ],
        )
    }
}

/// One deterministic hash in `[0, 1)` for a wave cell index.
///
/// Integer-only so the CPU and the GPU agree bit for bit.
fn noise_unit(cell: i64) -> f64 {
    let mut mixed = (cell as i32 as u32).wrapping_mul(2_654_435_761);
    mixed = mixed.rotate_left(13) ^ 0x9e37_79b9;
    f64::from(mixed >> 8) / f64::from(0x00ff_ffffu32)
}

fn smoothstep01(value: f64) -> f64 {
    let t = value.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The wave shape evaluated at `u` (in wavelengths).
fn wave_value(wave_type: WaveType, u: f64) -> f64 {
    let cell = u.floor();
    let s = u - cell;
    match wave_type {
        WaveType::Sine => (TAU * s).sin(),
        WaveType::Square => {
            if s < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        WaveType::Triangle => 1.0 - 4.0 * (s - 0.5).abs(),
        WaveType::Sawtooth => 2.0 * s - 1.0,
        WaveType::Circle => {
            let offset = 2.0 * s - 1.0;
            2.0 * (1.0 - offset * offset).max(0.0).sqrt() - 1.0
        }
        WaveType::Semicircle => {
            let offset = 2.0 * s - 1.0;
            (1.0 - offset * offset).max(0.0).sqrt()
        }
        WaveType::NonCircle => {
            let offset = 2.0 * s - 1.0;
            1.0 - 2.0 * (1.0 - offset * offset).max(0.0).sqrt()
        }
        WaveType::Noise => 2.0 * noise_unit(cell as i64) - 1.0,
        WaveType::SmoothNoise => {
            let left = noise_unit(cell as i64);
            let right = noise_unit(cell as i64 + 1);
            2.0 * (left + (right - left) * smoothstep01(s)) - 1.0
        }
    }
}

/// Damping applied to the displacement at one frame position.
fn pin_damping(pin: WavePin, column: f64, row: f64, width: usize, height: usize) -> f64 {
    let width_f = width as f64;
    let height_f = height as f64;
    let x = (column + 0.5) / width_f;
    let y = (row + 0.5) / height_f;
    let horizontal = WAVE_WARP_PIN_MARGIN;
    let vertical = WAVE_WARP_PIN_MARGIN;
    let left = smoothstep01(x / horizontal);
    let right = smoothstep01((1.0 - x) / horizontal);
    let top = smoothstep01(y / vertical);
    let bottom = smoothstep01((1.0 - y) / vertical);
    match pin {
        WavePin::None => 1.0,
        WavePin::AllEdges => left.min(right).min(top).min(bottom),
        WavePin::Center => {
            // Pixel coordinates are texel indices, so the frame centre sits at
            // half of the *index* extent rather than half of the pixel count.
            let centre_x = 0.5 * (width_f - 1.0);
            let centre_y = 0.5 * (height_f - 1.0);
            let radius = WAVE_WARP_CENTRE_PIN_FRACTION * width_f.min(height_f);
            let distance = ((column - centre_x).powi(2) + (row - centre_y).powi(2)).sqrt();
            smoothstep01(distance / radius)
        }
        WavePin::LeftEdge => left,
        WavePin::TopEdge => top,
        WavePin::RightEdge => right,
        WavePin::BottomEdge => bottom,
        WavePin::HorizontalEdges => top.min(bottom),
        WavePin::VerticalEdges => left.min(right),
    }
}

/// Applies the wave warp to a whole frame, returning `false` when the frame
/// cannot be processed.
pub fn wave_warp_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: WaveWarpGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_wave_warp_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the wave warp in place with an explicit cancellation checkpoint.
pub(crate) fn apply_wave_warp_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: WaveWarpGrade,
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
    let (axis_x, axis_y) = grade.axis();
    let height_px = f64::from(grade.wave_height);
    let wavelength = f64::from(grade.wave_width);
    let phase = f64::from(grade.phase_turns);
    let samples = grade.quality.samples_per_axis().max(1);
    let step = 1.0 / f64::from(samples);

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
                        let position_x = column as f64 + offset_x;
                        let position_y = row as f64 + offset_y;
                        let along = position_x * axis_x + position_y * axis_y;
                        let damping = pin_damping(grade.pin, position_x, position_y, width, height);
                        let displacement = height_px
                            * wave_value(grade.wave_type, along / wavelength - phase)
                            * damping;
                        let sample_x = position_x + axis_x * displacement;
                        let sample_y = position_y + axis_y * displacement;
                        let texel =
                            sample_bilinear_clamped(&source, width, height, sample_x, sample_y);
                        for channel in 0..4 {
                            accumulated[channel] += f64::from(texel[channel]);
                        }
                    }
                }
                let scale = 1.0 / f64::from(samples * samples);
                *pixel = [
                    (accumulated[0] * scale) as f32,
                    (accumulated[1] * scale) as f32,
                    (accumulated[2] * scale) as f32,
                    (accumulated[3] * scale) as f32,
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
    use super::*;

    fn grade(
        wave_type: WaveType,
        wave_height: f32,
        wave_width: f32,
        direction_degrees: f32,
        phase_turns: f32,
        pin: WavePin,
        quality: WaveQuality,
    ) -> WaveWarpGrade {
        WaveWarpGrade::new(
            wave_type,
            wave_height,
            wave_width,
            direction_degrees,
            phase_turns,
            pin,
            quality,
        )
        .expect("the grade is inside the documented ranges")
    }

    fn gradient(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut frame = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                let value = 0.01 * (column as f32 + 3.0 * row as f32);
                frame.push([value, 0.5 - 0.3 * value, 0.2 + 0.7 * value, 1.0]);
            }
        }
        frame
    }

    #[test]
    fn a_zero_height_is_identity() {
        let value = grade(
            WaveType::Sine,
            0.0,
            40.0,
            90.0,
            0.0,
            WavePin::None,
            WaveQuality::Low,
        );
        assert!(value.is_identity());
        let mut frame = gradient(9, 7);
        let original = frame.clone();
        assert!(wave_warp_frame_f32(&mut frame, 9, 7, value));
        assert_eq!(frame, original);
    }

    #[test]
    fn the_ranges_are_enforced() {
        assert_eq!(
            WaveWarpGrade::new(
                WaveType::Sine,
                32_001.0,
                40.0,
                90.0,
                0.0,
                WavePin::None,
                WaveQuality::Low
            ),
            Err(WaveWarpError::Height)
        );
        assert_eq!(
            WaveWarpGrade::new(
                WaveType::Sine,
                10.0,
                0.5,
                90.0,
                0.0,
                WavePin::None,
                WaveQuality::Low
            ),
            Err(WaveWarpError::Width)
        );
        assert_eq!(
            WaveWarpGrade::new(
                WaveType::Sine,
                10.0,
                40.0,
                f32::NAN,
                0.0,
                WavePin::None,
                WaveQuality::Low
            ),
            Err(WaveWarpError::Direction)
        );
        assert_eq!(
            WaveWarpGrade::new(
                WaveType::Sine,
                10.0,
                40.0,
                90.0,
                f32::INFINITY,
                WavePin::None,
                WaveQuality::Low
            ),
            Err(WaveWarpError::PhaseTurns)
        );
        assert_eq!(validate_wave_speed(100.0), Ok(100.0));
        assert_eq!(validate_wave_speed(-100.5), Err(WaveWarpError::Speed));
        assert_eq!(validate_wave_speed(f32::NAN), Err(WaveWarpError::Speed));
    }

    #[test]
    fn the_shapes_hit_their_documented_landmarks() {
        assert!((wave_value(WaveType::Sine, 0.25) - 1.0).abs() <= 1e-12);
        assert!((wave_value(WaveType::Sine, 0.75) + 1.0).abs() <= 1e-12);
        assert_eq!(wave_value(WaveType::Square, 0.25), 1.0);
        assert_eq!(wave_value(WaveType::Square, 0.75), -1.0);
        assert!((wave_value(WaveType::Triangle, 0.5) - 1.0).abs() <= 1e-12);
        assert!((wave_value(WaveType::Triangle, 0.0) + 1.0).abs() <= 1e-12);
        assert!(wave_value(WaveType::Sawtooth, 0.5).abs() <= 1e-12);
        assert!((wave_value(WaveType::Sawtooth, 0.0) + 1.0).abs() <= 1e-12);
        assert!((wave_value(WaveType::Circle, 0.5) - 1.0).abs() <= 1e-12);
        assert!((wave_value(WaveType::Circle, 0.0) + 1.0).abs() <= 1e-12);
        assert!((wave_value(WaveType::Semicircle, 0.5) - 1.0).abs() <= 1e-12);
        assert!(wave_value(WaveType::Semicircle, 0.0).abs() <= 1e-12);
        assert!((wave_value(WaveType::NonCircle, 0.5) + 1.0).abs() <= 1e-12);
        assert!((wave_value(WaveType::NonCircle, 0.0) - 1.0).abs() <= 1e-12);
    }

    #[test]
    fn the_noise_shapes_are_deterministic_and_bounded() {
        let first = wave_value(WaveType::Noise, 3.4);
        assert_eq!(first, wave_value(WaveType::Noise, 3.4));
        assert_eq!(first, wave_value(WaveType::Noise, 3.9));
        assert!((-1.0..=1.0).contains(&first));
        assert_eq!(first, wave_value(WaveType::Noise, 3.4));
        let smooth = wave_value(WaveType::SmoothNoise, 3.5);
        assert!((-1.0..=1.0).contains(&smooth));
        assert_eq!(smooth, wave_value(WaveType::SmoothNoise, 3.5));
        let left = wave_value(WaveType::SmoothNoise, 3.0);
        let right = wave_value(WaveType::SmoothNoise, 4.0);
        assert!((smooth - 0.5 * (left + right)).abs() <= 1e-12);
    }

    #[test]
    fn the_phase_folds_into_the_unit_interval() {
        assert!((wave_warp_phase_turns(0.0, 1.0, 0.25) - 0.25).abs() <= 1e-12);
        assert!((wave_warp_phase_turns(90.0, 0.0, 12.0) - 0.25).abs() <= 1e-12);
        assert!((wave_warp_phase_turns(0.0, -1.0, 0.25) - 0.75).abs() <= 1e-12);
        assert!((wave_warp_phase_turns(0.0, 2.0, 1.2) - 0.4).abs() <= 1e-9);
    }

    #[test]
    fn the_four_cardinal_directions_select_the_documented_axis() {
        let axis = grade(
            WaveType::Sine,
            1.0,
            40.0,
            0.0,
            0.0,
            WavePin::None,
            WaveQuality::Low,
        )
        .axis();
        assert!(axis.0.abs() <= 1e-12);
        assert!((axis.1 + 1.0).abs() <= 1e-12);
        let axis = grade(
            WaveType::Sine,
            1.0,
            40.0,
            90.0,
            0.0,
            WavePin::None,
            WaveQuality::Low,
        )
        .axis();
        assert!((axis.0 - 1.0).abs() <= 1e-12);
        assert!(axis.1.abs() <= 1e-12);
    }

    #[test]
    fn a_horizontal_wave_samples_the_source_along_the_axis() {
        let width = 17;
        let height = 13;
        let value = grade(
            WaveType::Sine,
            6.0,
            40.0,
            90.0,
            0.125,
            WavePin::None,
            WaveQuality::Low,
        );
        let mut frame = gradient(width, height);
        assert!(wave_warp_frame_f32(&mut frame, width, height, value));
        let source = gradient(width, height);
        let (axis_x, axis_y) = value.axis();
        for (column, row) in [(3usize, 2usize), (9, 5), (14, 11)] {
            let along = column as f64 * axis_x + row as f64 * axis_y;
            let displacement = f64::from(value.wave_height())
                * wave_value(
                    value.wave_type(),
                    along / f64::from(value.wave_width()) - 0.125,
                );
            let sample_x = column as f64 + axis_x * displacement;
            let sample_y = row as f64 + axis_y * displacement;
            let expected = sample_bilinear_clamped(&source, width, height, sample_x, sample_y);
            let actual = frame[row * width + column];
            for channel in 0..4 {
                assert!(
                    (actual[channel] - expected[channel]).abs() <= 1e-5,
                    "pixel ({column}, {row}) channel {channel}: {} vs {}",
                    actual[channel],
                    expected[channel]
                );
            }
        }
    }

    #[test]
    fn the_quality_averages_sixteen_source_lookups() {
        let width = 21;
        let height = 19;
        let base = grade(
            WaveType::Sine,
            9.0,
            50.0,
            90.0,
            0.2,
            WavePin::None,
            WaveQuality::High,
        );
        let mut frame = gradient(width, height);
        assert!(wave_warp_frame_f32(&mut frame, width, height, base));
        let mut single = gradient(width, height);
        let low = WaveWarpGrade::new(
            WaveType::Sine,
            9.0,
            50.0,
            90.0,
            0.2,
            WavePin::None,
            WaveQuality::Low,
        )
        .expect("the low-quality grade is valid");
        assert!(wave_warp_frame_f32(&mut single, width, height, low));
        assert_ne!(frame, single, "supersampling changes the sampled result");
    }

    #[test]
    fn a_pinned_edge_keeps_its_pixels_in_place() {
        let width = 40;
        let height = 30;
        let value = grade(
            WaveType::Sine,
            12.0,
            40.0,
            90.0,
            0.25,
            WavePin::AllEdges,
            WaveQuality::Low,
        );
        let mut frame = gradient(width, height);
        let original = frame.clone();
        assert!(wave_warp_frame_f32(&mut frame, width, height, value));
        let row = 15;
        let left = original[row * width];
        let actual = frame[row * width];
        for channel in 0..4 {
            assert!(
                (actual[channel] - left[channel]).abs() <= 1e-6,
                "the pinned left edge moved"
            );
        }
        let centre_column = width / 2;
        let before = original[row * width + centre_column];
        let after = frame[row * width + centre_column];
        let difference: f32 = (0..4).map(|c| (after[c] - before[c]).abs()).sum();
        assert!(difference > 1e-4, "the free centre must still warp");
    }

    #[test]
    fn the_centre_pin_holds_the_frame_centre() {
        let width = 41;
        let height = 41;
        let value = grade(
            WaveType::Sine,
            14.0,
            38.0,
            90.0,
            0.3,
            WavePin::Center,
            WaveQuality::Low,
        );
        let mut frame = gradient(width, height);
        let original = frame.clone();
        assert!(wave_warp_frame_f32(&mut frame, width, height, value));
        let centre = 20 * width + 20;
        for channel in 0..4 {
            assert!((frame[centre][channel] - original[centre][channel]).abs() <= 1e-6);
        }
    }

    #[test]
    fn the_kernel_mixes_premultiplied_colour() {
        let width = 16;
        let height = 16;
        let mut frame = vec![[0.0f32, 0.0, 0.0, 1.0]; width * height];
        // Two texels that a half-pixel sample lands between: translucent red
        // above an opaque blue.
        frame[2 * width + 2] = [1.0, 0.0, 0.0, 0.25];
        frame[3 * width + 2] = [0.0, 0.0, 1.0, 1.0];
        // Direction 0 travels up the frame, so a positive displacement lifts the
        // sample by `-height * sin(-2π * 3 / 12) = height`. With `-0.5` pixels of
        // crest height the pixel at row 3 samples row 2.5, halfway between the
        // two texels above.
        let value = grade(
            WaveType::Sine,
            -0.5,
            12.0,
            0.0,
            0.0,
            WavePin::None,
            WaveQuality::Low,
        );
        assert!(wave_warp_frame_f32(&mut frame, width, height, value));
        let pixel = frame[3 * width + 2];
        // Premultiplied mixing yields (0.2, 0, 0.8) at alpha 0.625; straight
        // mixing would yield (0.5, 0, 0.5) at the same alpha.
        assert!((pixel[0] - 0.2).abs() <= 1e-6, "red: {}", pixel[0]);
        assert!(pixel[1].abs() <= 1e-6);
        assert!((pixel[2] - 0.8).abs() <= 1e-6, "blue: {}", pixel[2]);
        assert!((pixel[3] - 0.625).abs() <= 1e-6, "alpha: {}", pixel[3]);
    }

    #[test]
    fn the_frame_entry_point_rejects_mismatched_buffers() {
        let value = grade(
            WaveType::Sine,
            5.0,
            40.0,
            90.0,
            0.0,
            WavePin::None,
            WaveQuality::Low,
        );
        let mut frame = vec![[0.0f32; 4]; 10];
        assert!(!wave_warp_frame_f32(&mut frame, 4, 4, value));
    }

    #[test]
    fn the_gpu_controls_pack_the_documented_fields() {
        let value = grade(
            WaveType::Triangle,
            7.5,
            32.0,
            45.0,
            0.25,
            WavePin::BottomEdge,
            WaveQuality::Normal,
        );
        let (params, extra) = value.gpu_controls();
        assert!((params[0] - 7.5).abs() <= 1e-6);
        assert!((params[1] - 32.0).abs() <= 1e-6);
        assert!((params[2] - std::f32::consts::FRAC_PI_4).abs() <= 1e-6);
        assert!((params[3] - 0.25).abs() <= 1e-6);
        assert_eq!(extra[0], WaveType::Triangle.as_u32() as f32);
        assert_eq!(extra[1], WavePin::BottomEdge.as_u32() as f32);
        assert_eq!(extra[2], 2.0);
        assert_eq!(extra[3], 0.0);
    }

    #[test]
    fn the_keys_round_trip() {
        for wave_type in WaveType::ALL {
            assert_eq!(WaveType::from_key(wave_type.as_key()), Some(wave_type));
            assert_eq!(WaveType::from_u32(wave_type.as_u32()), Some(wave_type));
            assert_eq!(wave_type.as_menu(), wave_type.as_u32() + 1);
        }
        for pin in WavePin::ALL {
            assert_eq!(WavePin::from_key(pin.as_key()), Some(pin));
        }
        for quality in WaveQuality::ALL {
            assert_eq!(WaveQuality::from_key(quality.as_key()), Some(quality));
        }
        assert_eq!(WaveType::from_key("spiral"), None);
        assert_eq!(WavePin::from_key("diagonal"), None);
        assert_eq!(WaveQuality::from_key("ultra"), None);
    }
}
