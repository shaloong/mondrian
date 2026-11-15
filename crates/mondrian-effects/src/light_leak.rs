//! Light leak: a procedural, coordinate-driven bloom that fades in from the
//! authored position along the authored angle.
//!
//! The effect is a pure function of the pixel's own coordinate and the frame
//! seed, so it runs in the fused point pass. `强度` scales the added light,
//! `颜色` tints it, `角度` rotates the streak, `位置` moves its centre, `速度`
//! widens the spacing of the internal striations, `混合模式` selects how the
//! leak combines with the source, and `闪烁` jitters the exposure from frame to
//! frame using the frame seed.
//!
//! The kernel does not clamp the result: the working space is scene-linear and
//! highlights above one are legal. Pixels with no coverage are left untouched.

use thiserror::Error;

/// Upper bound of the documented intensity in percent.
pub const LIGHT_LEAK_MAX_INTENSITY: f32 = 100.0;
/// Lower bound of the documented intensity in percent.
pub const LIGHT_LEAK_MIN_INTENSITY: f32 = 0.0;
/// Documented default intensity in percent.
pub const LIGHT_LEAK_DEFAULT_INTENSITY: f32 = 50.0;
/// Default tint: a warm orange.
pub const LIGHT_LEAK_DEFAULT_COLOUR: [f32; 3] = [1.0, 0.55, 0.2];
/// Default leak centre, at the centre of the frame.
pub const LIGHT_LEAK_DEFAULT_POSITION: [f32; 2] = [0.5, 0.5];
/// Default angle in degrees.
pub const LIGHT_LEAK_DEFAULT_ANGLE_DEGREES: f32 = 0.0;
/// Lower bound of the documented speed.
pub const LIGHT_LEAK_MIN_SPEED: f32 = 0.0;
/// Upper bound of the documented speed.
pub const LIGHT_LEAK_MAX_SPEED: f32 = 10.0;
/// Documented default speed.
pub const LIGHT_LEAK_DEFAULT_SPEED: f32 = 1.0;
/// Lower bound of the documented flicker in percent.
pub const LIGHT_LEAK_MIN_FLICKER: f32 = 0.0;
/// Upper bound of the documented flicker in percent.
pub const LIGHT_LEAK_MAX_FLICKER: f32 = 100.0;
/// Documented default flicker in percent.
pub const LIGHT_LEAK_DEFAULT_FLICKER: f32 = 0.0;
/// Number of sampled bits of the frame seed used by the flicker.
pub const LIGHT_LEAK_SEED_MASK: u32 = 0xFFFF;
/// Divisor that maps the sampled seed bits into the unit interval.
pub const LIGHT_LEAK_SEED_SCALE: f32 = 65535.0;

const TAU: f32 = std::f32::consts::TAU;
/// Half-width of the leak envelope along the streak direction.
const ENVELOPE_ALONG: f32 = 0.9;
/// Half-width of the leak envelope across the streak direction.
const ENVELOPE_ACROSS: f32 = 0.35;
/// Weight of the constant term in the striation profile.
const STRIATION_FLOOR: f32 = 0.55;
/// Weight of the cosine term in the striation profile.
const STRIATION_GAIN: f32 = 0.45;

/// How the leak combines with the source pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightLeakBlend {
    /// Screen: `1 - (1 - base) * (1 - leak)`.
    Screen,
    /// Overlay: multiply the dark half, screen the light half.
    Overlay,
    /// Add: `base + leak`.
    Add,
    /// Normal: interpolate towards the leak tint.
    Normal,
}

impl LightLeakBlend {
    /// All variants in menu order.
    pub const ALL: [Self; 4] = [Self::Screen, Self::Overlay, Self::Add, Self::Normal];

    /// Stable authoring key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Overlay => "overlay",
            Self::Add => "add",
            Self::Normal => "normal",
        }
    }

    /// Parses a stable authoring key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|blend| blend.as_key() == key)
    }

    /// Compact tag used by the effect hash and the GPU uniform.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Screen => 0,
            Self::Overlay => 1,
            Self::Add => 2,
            Self::Normal => 3,
        }
    }

    /// Menu caption, in the documented order.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Screen => "滤色",
            Self::Overlay => "叠加",
            Self::Add => "相加",
            Self::Normal => "正常",
        }
    }
}

/// Authoring values of the light leak effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightLeakParameters {
    /// Intensity in percent, 0 to 100.
    pub intensity: f32,
    /// Leak tint.
    pub colour: [f32; 3],
    /// Streak angle in degrees, -180 to 180.
    pub angle_degrees: f32,
    /// Leak centre in normalized frame coordinates.
    pub position: [f32; 2],
    /// Striation spacing control, 0 to 10.
    pub speed: f32,
    /// How the leak combines with the source.
    pub blend: LightLeakBlend,
    /// Per-frame exposure jitter in percent, 0 to 100.
    pub flicker: f32,
}

impl Default for LightLeakParameters {
    fn default() -> Self {
        Self {
            intensity: LIGHT_LEAK_DEFAULT_INTENSITY,
            colour: LIGHT_LEAK_DEFAULT_COLOUR,
            angle_degrees: LIGHT_LEAK_DEFAULT_ANGLE_DEGREES,
            position: LIGHT_LEAK_DEFAULT_POSITION,
            speed: LIGHT_LEAK_DEFAULT_SPEED,
            blend: LightLeakBlend::Screen,
            flicker: LIGHT_LEAK_DEFAULT_FLICKER,
        }
    }
}

/// A parameter value that fell outside its documented range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("light_leak.{parameter} must be finite and within its documented range")]
pub struct LightLeakError {
    parameter: &'static str,
}

impl LightLeakError {
    /// Name of the offending parameter.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// Validated light leak configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightLeakGrade {
    parameters: LightLeakParameters,
}

impl LightLeakGrade {
    /// Validates the documented ranges.
    pub fn new(parameters: LightLeakParameters) -> Result<Self, LightLeakError> {
        let finite = |parameter: &'static str, value: f32, min: f32, max: f32| {
            if value.is_finite() && value >= min && value <= max {
                Ok(())
            } else {
                Err(LightLeakError { parameter })
            }
        };
        finite(
            "intensity",
            parameters.intensity,
            LIGHT_LEAK_MIN_INTENSITY,
            LIGHT_LEAK_MAX_INTENSITY,
        )?;
        for (channel, value) in ["red", "green", "blue"].into_iter().zip(parameters.colour) {
            finite(channel, value, 0.0, 1.0)?;
        }
        finite("angle", parameters.angle_degrees, -180.0, 180.0)?;
        finite("position_x", parameters.position[0], 0.0, 1.0)?;
        finite("position_y", parameters.position[1], 0.0, 1.0)?;
        finite(
            "speed",
            parameters.speed,
            LIGHT_LEAK_MIN_SPEED,
            LIGHT_LEAK_MAX_SPEED,
        )?;
        finite(
            "flicker",
            parameters.flicker,
            LIGHT_LEAK_MIN_FLICKER,
            LIGHT_LEAK_MAX_FLICKER,
        )?;
        Ok(Self { parameters })
    }

    /// The documented defaults.
    pub fn documented_defaults() -> Self {
        Self { parameters: LightLeakParameters::default() }
    }

    /// Borrows the validated parameters.
    pub const fn parameters(&self) -> &LightLeakParameters {
        &self.parameters
    }

    /// Intensity as a unit fraction.
    pub const fn intensity(&self) -> f32 {
        self.parameters.intensity / LIGHT_LEAK_MAX_INTENSITY
    }

    /// Flicker as a unit fraction.
    pub const fn flicker(&self) -> f32 {
        self.parameters.flicker / LIGHT_LEAK_MAX_FLICKER
    }

    /// Striations per unit of normalized distance along the streak.
    pub const fn waves(&self) -> f32 {
        1.0 + self.parameters.speed
    }

    /// Whether the effect leaves the frame unchanged.
    pub const fn is_identity(&self) -> bool {
        self.parameters.intensity <= 0.0
    }

    /// Authoring cost of the effect.
    pub const fn cost(&self) -> u8 {
        1
    }

    /// Packs the uniform cells for the fused point shader.
    ///
    /// `params` carries intensity, the cosine and sine of the angle, and the
    /// striation rate. `colour` carries the tint and the blend tag. `extras`
    /// carries the leak centre and the flicker amount. The frame seed is read
    /// from the shared point-pass uniform, so it never enters this payload.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4], [f32; 4]) {
        let angle = self.parameters.angle_degrees.to_radians();
        (
            [self.intensity(), angle.cos(), angle.sin(), self.waves()],
            [
                self.parameters.colour[0],
                self.parameters.colour[1],
                self.parameters.colour[2],
                f32::from(self.parameters.blend.as_u8()),
            ],
            [
                self.parameters.position[0],
                self.parameters.position[1],
                self.flicker(),
                0.0,
            ],
        )
    }

    /// Hashes the full authored identity.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;
        self.parameters.intensity.to_bits().hash(state);
        for channel in self.parameters.colour {
            channel.to_bits().hash(state);
        }
        self.parameters.angle_degrees.to_bits().hash(state);
        self.parameters.position[0].to_bits().hash(state);
        self.parameters.position[1].to_bits().hash(state);
        self.parameters.speed.to_bits().hash(state);
        self.parameters.blend.as_u8().hash(state);
        self.parameters.flicker.to_bits().hash(state);
    }
}

/// Applies the light leak to a straight-alpha scene-linear frame.
pub fn light_leak_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &LightLeakGrade,
    frame_seed: i64,
) -> bool {
    apply_light_leak_f32_controlled(pixels, width, height, grade, frame_seed, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Applies the light leak, reporting cancellation through `checkpoint`.
pub(crate) fn apply_light_leak_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &LightLeakGrade,
    frame_seed: i64,
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
    checkpoint()?;

    let parameters = grade.parameters;
    let intensity = grade.intensity();
    let flicker = grade.flicker();
    let angle = parameters.angle_degrees.to_radians();
    let (sin_angle, cos_angle) = angle.sin_cos();
    let waves = grade.waves();
    let seed_unit = seed_unit(frame_seed);
    let jitter = 1.0 + (seed_unit - 0.5) * flicker;
    let tinted = [
        parameters.colour[0] * jitter,
        parameters.colour[1] * jitter,
        parameters.colour[2] * jitter,
    ];
    let inverse_width = 1.0 / width as f32;
    let inverse_height = 1.0 / height as f32;

    for row in 0..height {
        let v = (row as f32 + 0.5) * inverse_height;
        for column in 0..width {
            let index = row * width + column;
            if working[index][3] <= 0.0 {
                continue;
            }
            let u = (column as f32 + 0.5) * inverse_width;
            let dx = u - parameters.position[0];
            let dy = v - parameters.position[1];
            let along = dx * cos_angle + dy * sin_angle;
            let across = -dx * sin_angle + dy * cos_angle;
            let envelope =
                (-(square(along / ENVELOPE_ALONG) + square(across / ENVELOPE_ACROSS)) * 2.0).exp();
            let striation = STRIATION_FLOOR + STRIATION_GAIN * (TAU * along * waves).cos();
            let opacity = (envelope * striation).clamp(0.0, 1.0) * intensity;
            if opacity <= 0.0 {
                continue;
            }
            let base = working[index];
            for channel in 0..3 {
                let leak = tinted[channel];
                let combined = match parameters.blend {
                    LightLeakBlend::Screen => 1.0 - (1.0 - base[channel]) * (1.0 - leak),
                    LightLeakBlend::Overlay => {
                        if base[channel] < 0.5 {
                            2.0 * base[channel] * leak
                        } else {
                            1.0 - 2.0 * (1.0 - base[channel]) * (1.0 - leak)
                        }
                    }
                    LightLeakBlend::Add => base[channel] + leak,
                    LightLeakBlend::Normal => leak,
                };
                working[index][channel] = base[channel] + (combined - base[channel]) * opacity;
            }
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Maps the low bits of the frame seed into the unit interval.
///
/// Both the CPU kernel and the point shader sample the same sixteen bits of the
/// seed, so the flicker agrees channel by channel.
pub const fn seed_unit(frame_seed: i64) -> f32 {
    (frame_seed as u32 & LIGHT_LEAK_SEED_MASK) as f32 / LIGHT_LEAK_SEED_SCALE
}

fn square(value: f32) -> f32 {
    value * value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_grade(parameters: LightLeakParameters) -> LightLeakGrade {
        match LightLeakGrade::new(parameters) {
            Ok(grade) => grade,
            Err(error) => panic!("unexpected parameter rejection: {error}"),
        }
    }

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![[0.4, 0.4, 0.4, 1.0]; width * height]
    }

    #[test]
    fn rejects_values_outside_the_documented_ranges() {
        let cases = [
            LightLeakParameters { intensity: 100.5, ..Default::default() },
            LightLeakParameters { intensity: -0.001, ..Default::default() },
            LightLeakParameters { intensity: f32::NAN, ..Default::default() },
            LightLeakParameters { colour: [1.2, 0.5, 0.5], ..Default::default() },
            LightLeakParameters { angle_degrees: -181.0, ..Default::default() },
            LightLeakParameters { position: [1.1, 0.5], ..Default::default() },
            LightLeakParameters { speed: 10.001, ..Default::default() },
            LightLeakParameters { flicker: 100.1, ..Default::default() },
        ];
        for case in cases {
            assert!(
                LightLeakGrade::new(case).is_err(),
                "{case:?} should be rejected"
            );
        }
        assert!(LightLeakGrade::new(LightLeakParameters {
            intensity: 0.0,
            speed: 10.0,
            flicker: 100.0,
            angle_degrees: -180.0,
            position: [1.0, 1.0],
            colour: [1.0, 1.0, 1.0],
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn documented_defaults_stay_stable() {
        let grade = LightLeakGrade::documented_defaults();
        let parameters = grade.parameters();
        assert_eq!(parameters.intensity, 50.0);
        assert_eq!(parameters.colour, [1.0, 0.55, 0.2]);
        assert_eq!(parameters.angle_degrees, 0.0);
        assert_eq!(parameters.position, [0.5, 0.5]);
        assert_eq!(parameters.speed, 1.0);
        assert_eq!(parameters.blend, LightLeakBlend::Screen);
        assert_eq!(parameters.flicker, 0.0);
        assert!(!grade.is_identity());
        assert_eq!(grade.cost(), 1);
        assert_eq!(grade.waves(), 2.0);
    }

    #[test]
    fn menu_order_and_keys_are_stable() {
        assert_eq!(
            LightLeakBlend::ALL.map(LightLeakBlend::as_key),
            ["screen", "overlay", "add", "normal"]
        );
        assert_eq!(
            LightLeakBlend::ALL.map(LightLeakBlend::label),
            ["滤色", "叠加", "相加", "正常"]
        );
        assert_eq!(LightLeakBlend::Add.as_u8(), 2);
        assert_eq!(
            LightLeakBlend::from_key("normal"),
            Some(LightLeakBlend::Normal)
        );
        assert_eq!(LightLeakBlend::from_key("unknown"), None);
    }

    #[test]
    fn packs_the_uniform_cells_for_the_point_shader() {
        let grade = valid_grade(LightLeakParameters {
            angle_degrees: 90.0,
            speed: 3.0,
            ..Default::default()
        });
        let (params, colour, extras) = grade.gpu_controls();
        assert_eq!(params[0], 0.5);
        assert!(params[1].abs() < 1e-6);
        assert_eq!(params[2], 1.0);
        assert_eq!(params[3], 4.0);
        assert_eq!(&colour[..3], &[1.0, 0.55, 0.2]);
        assert_eq!(colour[3], 0.0);
        assert_eq!(&extras[..3], &[0.5, 0.5, 0.0]);
    }

    #[test]
    fn a_zero_intensity_is_an_exact_identity() {
        let grade = valid_grade(LightLeakParameters { intensity: 0.0, ..Default::default() });
        let mut pixels = frame(4, 3);
        let before = pixels.clone();
        assert!(grade.is_identity());
        assert!(light_leak_frame_f32(&mut pixels, 4, 3, &grade, 7));
        assert_eq!(pixels, before);
    }

    #[test]
    fn the_leak_brightens_the_authored_position_most() {
        let grade = valid_grade(LightLeakParameters { intensity: 100.0, ..Default::default() });
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; 9 * 9];
        assert!(light_leak_frame_f32(&mut pixels, 9, 9, &grade, 0));
        let centre = pixels[4 * 9 + 4];
        let corner = pixels[0];
        assert!(
            centre[0] > corner[0],
            "centre {} corner {}",
            centre[0],
            corner[0]
        );
        assert!(centre[0] > 0.0);
    }

    #[test]
    fn the_tint_scales_each_channel() {
        let grade = valid_grade(LightLeakParameters {
            intensity: 100.0,
            colour: [1.0, 0.5, 0.0],
            ..Default::default()
        });
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; 5 * 5];
        assert!(light_leak_frame_f32(&mut pixels, 5, 5, &grade, 0));
        let centre = pixels[12];
        assert!(centre[0] > centre[1]);
        assert_eq!(centre[2], 0.0);
    }

    #[test]
    fn the_angle_rotates_the_streak() {
        let horizontal = valid_grade(LightLeakParameters {
            intensity: 100.0,
            angle_degrees: 0.0,
            ..Default::default()
        });
        let vertical = valid_grade(LightLeakParameters {
            intensity: 100.0,
            angle_degrees: 90.0,
            ..Default::default()
        });
        let mut flat = vec![[0.0, 0.0, 0.0, 1.0]; 9 * 9];
        let mut rotated = flat.clone();
        assert!(light_leak_frame_f32(&mut flat, 9, 9, &horizontal, 0));
        assert!(light_leak_frame_f32(&mut rotated, 9, 9, &vertical, 0));
        assert_ne!(flat, rotated);
        assert!(flat[4 * 9] > rotated[4 * 9]);
        assert!(rotated[4] > flat[4]);
    }

    #[test]
    fn moving_the_position_moves_the_hotspot() {
        let left = valid_grade(LightLeakParameters {
            intensity: 100.0,
            position: [0.2, 0.5],
            ..Default::default()
        });
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; 9 * 9];
        assert!(light_leak_frame_f32(&mut pixels, 9, 9, &left, 0));
        assert!(pixels[4 * 9 + 2][0] > pixels[4 * 9 + 8][0]);
    }

    #[test]
    fn speed_spreads_the_striations() {
        let slow =
            valid_grade(LightLeakParameters { intensity: 100.0, speed: 0.0, ..Default::default() });
        let fast =
            valid_grade(LightLeakParameters { intensity: 100.0, speed: 8.0, ..Default::default() });
        let mut slow_pixels = vec![[0.0, 0.0, 0.0, 1.0]; 17 * 3];
        let mut fast_pixels = slow_pixels.clone();
        assert!(light_leak_frame_f32(&mut slow_pixels, 17, 3, &slow, 0));
        assert!(light_leak_frame_f32(&mut fast_pixels, 17, 3, &fast, 0));
        assert_ne!(slow_pixels, fast_pixels);
        let sign_changes = fast_pixels
            .as_slice()
            .windows(2)
            .filter(|pair| (pair[0][0] - pair[1][0]).abs() > 0.0)
            .count();
        assert!(sign_changes > 0);
    }

    #[test]
    fn the_blend_modes_combine_differently() {
        let base = 0.4;
        for (blend, expected) in [
            (LightLeakBlend::Add, base + 1.0),
            (LightLeakBlend::Normal, 1.0),
        ] {
            let grade = valid_grade(LightLeakParameters {
                intensity: 100.0,
                colour: [1.0, 1.0, 1.0],
                blend,
                position: [0.5, 0.5],
                angle_degrees: 0.0,
                ..Default::default()
            });
            let mut pixels = vec![[base, base, base, 1.0]; 3 * 3];
            assert!(light_leak_frame_f32(&mut pixels, 3, 3, &grade, 0));
            let centre = pixels[4][0];
            assert!(centre > base);
            assert!(centre <= expected + 1e-5, "{blend:?} centre {centre}");
        }
        let screen = valid_grade(LightLeakParameters {
            intensity: 100.0,
            colour: [1.0, 1.0, 1.0],
            blend: LightLeakBlend::Screen,
            ..Default::default()
        });
        let mut pixels = vec![[base, base, base, 1.0]; 3 * 3];
        assert!(light_leak_frame_f32(&mut pixels, 3, 3, &screen, 0));
        assert!((pixels[4][0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn flicker_changes_with_the_frame_seed_and_freezes_at_zero() {
        let still = valid_grade(LightLeakParameters {
            intensity: 100.0,
            flicker: 0.0,
            ..Default::default()
        });
        let mut first = vec![[0.0, 0.0, 0.0, 1.0]; 5 * 5];
        let mut second = first.clone();
        assert!(light_leak_frame_f32(&mut first, 5, 5, &still, 1));
        assert!(light_leak_frame_f32(&mut second, 5, 5, &still, 99));
        assert_eq!(first, second);

        let flickering = valid_grade(LightLeakParameters {
            intensity: 100.0,
            flicker: 100.0,
            ..Default::default()
        });
        let mut third = vec![[0.0, 0.0, 0.0, 1.0]; 5 * 5];
        let mut fourth = third.clone();
        assert!(light_leak_frame_f32(&mut third, 5, 5, &flickering, 1));
        assert!(light_leak_frame_f32(&mut fourth, 5, 5, &flickering, 99));
        assert_ne!(third, fourth);
        assert_ne!(seed_unit(1), seed_unit(99));
    }

    #[test]
    fn transparent_pixels_are_left_alone() {
        let grade = valid_grade(LightLeakParameters::default());
        let mut pixels = vec![[0.0, 0.0, 0.0, 0.0]; 5 * 5];
        assert!(light_leak_frame_f32(&mut pixels, 5, 5, &grade, 3));
        assert!(pixels.iter().all(|pixel| *pixel == [0.0, 0.0, 0.0, 0.0]));
    }

    #[test]
    fn coverage_is_preserved() {
        let grade = valid_grade(LightLeakParameters::default());
        let mut pixels = frame(5, 5);
        pixels[12][3] = 0.5;
        assert!(light_leak_frame_f32(&mut pixels, 5, 5, &grade, 0));
        assert!(pixels.iter().all(|pixel| pixel[3] == 1.0 || pixel[3] == 0.5));
    }

    #[test]
    fn short_and_empty_frames_are_refused() {
        let grade = valid_grade(LightLeakParameters::default());
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!light_leak_frame_f32(&mut empty, 0, 0, &grade, 0));
        let mut short = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        assert!(!light_leak_frame_f32(&mut short, 4, 4, &grade, 0));
        let mut zero_width = vec![[0.0, 0.0, 0.0, 1.0]; 4];
        assert!(!light_leak_frame_f32(&mut zero_width, 0, 4, &grade, 0));
    }

    #[test]
    fn the_kernel_is_deterministic() {
        let grade = valid_grade(LightLeakParameters {
            angle_degrees: 33.0,
            speed: 2.5,
            flicker: 40.0,
            ..Default::default()
        });
        let mut first = frame(7, 5);
        let mut second = first.clone();
        assert!(light_leak_frame_f32(&mut first, 7, 5, &grade, 5));
        assert!(light_leak_frame_f32(&mut second, 7, 5, &grade, 5));
        assert_eq!(first, second);
    }

    #[test]
    fn cancellation_is_reported() {
        let grade = valid_grade(LightLeakParameters::default());
        let mut pixels = frame(4, 4);
        let mut calls = 0;
        let result = apply_light_leak_f32_controlled(&mut pixels, 4, 4, &grade, 0, &mut || {
            calls += 1;
            if calls == 2 {
                Err("cancelled")
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err("cancelled"));
    }

    #[test]
    fn the_hash_separates_distinct_configurations() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;

        let hash = |parameters: LightLeakParameters| {
            let mut hasher = DefaultHasher::new();
            valid_grade(parameters).hash_state(&mut hasher);
            hasher.finish()
        };
        let base = hash(LightLeakParameters::default());
        assert_ne!(
            base,
            hash(LightLeakParameters { blend: LightLeakBlend::Add, ..Default::default() })
        );
        assert_ne!(
            base,
            hash(LightLeakParameters { flicker: 10.0, ..Default::default() })
        );
        assert_ne!(
            base,
            hash(LightLeakParameters { position: [0.25, 0.5], ..Default::default() })
        );
    }
}
