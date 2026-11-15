//! Cineon converter: moves image data between 10-bit log code values and an
//! internal linear range.
//!
//! Cineon scans store their densities as 10-bit log code values, so a frame that
//! looks washed out on a linear timeline is usually a scan that has not been
//! decoded yet. The converter normalises the authored black and white code
//! values into the unit interval, applies the log curve and lands the result in
//! an internal linear range. The reverse direction encodes a linear frame back
//! into code values, and a third mode maps the two ranges without the log curve
//! so a scan can be checked for black and white alignment.
//!
//! The highlight control only acts on the decoding direction. Values above the
//! white point are pulled towards one with a soft shoulder instead of being
//! clipped at the white point, which keeps a bright window from turning into a
//! flat patch.
//!
//! Every channel is converted on its own and the alpha channel is passed
//! through, so the effect is pixel local and deterministic: two runs over the
//! same frame produce the same bytes.

use thiserror::Error;

/// Documented default of the black point, in 10-bit code values.
pub const CINEON_CONVERTER_DEFAULT_BLACK_POINT: f32 = 95.0;
/// Smallest 10-bit code value accepted for the black point.
pub const CINEON_CONVERTER_MIN_BLACK_POINT: f32 = 0.0;
/// Largest 10-bit code value accepted for the black point.
pub const CINEON_CONVERTER_MAX_BLACK_POINT: f32 = 1023.0;
/// Documented default of the internal black point.
pub const CINEON_CONVERTER_DEFAULT_INTERNAL_BLACK: f32 = 0.0;
/// Documented default of the white point, in 10-bit code values.
pub const CINEON_CONVERTER_DEFAULT_WHITE_POINT: f32 = 685.0;
/// Smallest 10-bit code value accepted for the white point.
pub const CINEON_CONVERTER_MIN_WHITE_POINT: f32 = 0.0;
/// Largest 10-bit code value accepted for the white point.
pub const CINEON_CONVERTER_MAX_WHITE_POINT: f32 = 1023.0;
/// Documented default of the internal white point.
pub const CINEON_CONVERTER_DEFAULT_INTERNAL_WHITE: f32 = 1.0;
/// Smallest accepted gamma.
pub const CINEON_CONVERTER_MIN_GAMMA: f32 = 0.009_995;
/// Largest accepted gamma.
pub const CINEON_CONVERTER_MAX_GAMMA: f32 = 5.0;
/// Documented default of the gamma.
pub const CINEON_CONVERTER_DEFAULT_GAMMA: f32 = 1.7;
/// Smallest accepted highlight filter amount.
pub const CINEON_CONVERTER_MIN_HIGHLIGHT: f32 = 0.0;
/// Largest accepted highlight filter amount.
pub const CINEON_CONVERTER_MAX_HIGHLIGHT: f32 = 150.0;
/// Documented default of the highlight filter amount.
pub const CINEON_CONVERTER_DEFAULT_HIGHLIGHT: f32 = 20.0;

/// The largest 10-bit code value.
const CINEON_CONVERTER_CODE_VALUES: f32 = 1023.0;
/// The smallest gamma that still yields a finite exponent.
const MINIMUM_GAMMA: f32 = 1.0e-3;
/// The highlight amount that maps to a fully engaged shoulder.
const HIGHLIGHT_RANGE: f32 = 150.0;

/// The direction the converter runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CineonConversion {
    /// Decode 10-bit log code values into the internal linear range.
    LogToLinear,
    /// Encode the internal linear range back into 10-bit log code values.
    LinearToLog,
    /// Map between the two ranges without the log curve.
    LinearToLinear,
}

impl CineonConversion {
    /// The documented default direction.
    pub const DEFAULT: Self = Self::LogToLinear;

    /// Every direction, in menu order.
    pub const ALL: [Self; 3] = [Self::LogToLinear, Self::LinearToLog, Self::LinearToLinear];

    /// The menu key the schema stores.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::LogToLinear => "log_to_linear",
            Self::LinearToLog => "linear_to_log",
            Self::LinearToLinear => "linear_to_linear",
        }
    }

    /// The compact tag the render op hashes and the shader reads.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::LogToLinear => 0,
            Self::LinearToLog => 1,
            Self::LinearToLinear => 2,
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|conversion| conversion.as_key() == key)
    }
}

/// Authoring values of the Cineon converter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CineonConverterParameters {
    /// Direction of the conversion.
    pub conversion: CineonConversion,
    /// Black point in 10-bit code values, 0 to 1023.
    pub black_point: f32,
    /// Black point of the internal range, 0 to 1.
    pub internal_black: f32,
    /// White point in 10-bit code values, 0 to 1023.
    pub white_point: f32,
    /// White point of the internal range, 0 to 1.
    pub internal_white: f32,
    /// Gamma of the log curve, 0.009995 to 5.
    pub gamma: f32,
    /// Soft shoulder applied above the white point, 0 to 150.
    pub highlight: f32,
}

impl Default for CineonConverterParameters {
    fn default() -> Self {
        Self {
            conversion: CineonConversion::DEFAULT,
            black_point: CINEON_CONVERTER_DEFAULT_BLACK_POINT,
            internal_black: CINEON_CONVERTER_DEFAULT_INTERNAL_BLACK,
            white_point: CINEON_CONVERTER_DEFAULT_WHITE_POINT,
            internal_white: CINEON_CONVERTER_DEFAULT_INTERNAL_WHITE,
            gamma: CINEON_CONVERTER_DEFAULT_GAMMA,
            highlight: CINEON_CONVERTER_DEFAULT_HIGHLIGHT,
        }
    }
}

/// A parameter value that fell outside its documented range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("cineon_converter.{parameter} must be finite and within its documented range")]
pub struct CineonConverterError {
    parameter: &'static str,
}

impl CineonConverterError {
    /// Name of the offending parameter.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// Validated Cineon converter configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CineonConverterGrade {
    parameters: CineonConverterParameters,
}

impl CineonConverterGrade {
    /// Validates the documented ranges.
    pub fn new(parameters: CineonConverterParameters) -> Result<Self, CineonConverterError> {
        let finite = |parameter: &'static str, value: f32, min: f32, max: f32| {
            if value.is_finite() && value >= min && value <= max {
                Ok(())
            } else {
                Err(CineonConverterError { parameter })
            }
        };
        finite(
            "black_point",
            parameters.black_point,
            CINEON_CONVERTER_MIN_BLACK_POINT,
            CINEON_CONVERTER_MAX_BLACK_POINT,
        )?;
        finite(
            "white_point",
            parameters.white_point,
            CINEON_CONVERTER_MIN_WHITE_POINT,
            CINEON_CONVERTER_MAX_WHITE_POINT,
        )?;
        finite("internal_black", parameters.internal_black, 0.0, 1.0)?;
        finite("internal_white", parameters.internal_white, 0.0, 1.0)?;
        finite(
            "gamma",
            parameters.gamma,
            CINEON_CONVERTER_MIN_GAMMA,
            CINEON_CONVERTER_MAX_GAMMA,
        )?;
        finite(
            "highlight",
            parameters.highlight,
            CINEON_CONVERTER_MIN_HIGHLIGHT,
            CINEON_CONVERTER_MAX_HIGHLIGHT,
        )?;
        Ok(Self { parameters })
    }

    /// The documented defaults.
    pub fn documented_defaults() -> Self {
        Self { parameters: CineonConverterParameters::default() }
    }

    /// Borrows the validated parameters.
    pub const fn parameters(&self) -> &CineonConverterParameters {
        &self.parameters
    }

    /// The direction the converter runs in.
    pub const fn conversion(&self) -> CineonConversion {
        self.parameters.conversion
    }

    /// True when the configuration leaves every channel unchanged.
    ///
    /// The decoding direction is the only one that can be an identity: its
    /// defaults move the black and white points, so the documented defaults are
    /// never an identity and the builder always appends a node.
    pub fn is_identity(&self) -> bool {
        self.parameters.conversion == CineonConversion::LogToLinear
            && self.parameters.black_point == CINEON_CONVERTER_MIN_BLACK_POINT
            && self.parameters.white_point == CINEON_CONVERTER_MAX_WHITE_POINT
            && self.parameters.internal_black == 0.0
            && self.parameters.internal_white == 1.0
            && self.parameters.gamma == 1.0
            && self.parameters.highlight == 0.0
    }

    /// Authoring cost of the effect.
    pub const fn cost(&self) -> u8 {
        1
    }

    /// Packs the uniform cells for the fused point shader.
    ///
    /// `params` carries the black point, the white point and the internal range,
    /// all normalised; `extra0` carries the gamma, the highlight fraction and
    /// the direction tag.
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        let parameters = &self.parameters;
        (
            [
                parameters.black_point / CINEON_CONVERTER_CODE_VALUES,
                parameters.white_point / CINEON_CONVERTER_CODE_VALUES,
                parameters.internal_black,
                parameters.internal_white,
            ],
            [
                parameters.gamma,
                parameters.highlight / HIGHLIGHT_RANGE,
                f32::from(parameters.conversion.as_u8()),
                0.0,
            ],
        )
    }

    /// Hashes the full authored identity.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;
        self.parameters.conversion.as_u8().hash(state);
        for value in [
            self.parameters.black_point,
            self.parameters.white_point,
            self.parameters.internal_black,
            self.parameters.internal_white,
            self.parameters.gamma,
            self.parameters.highlight,
        ] {
            value.to_bits().hash(state);
        }
    }
}

/// Normalises `value` into the unit interval between `low` and `high`.
///
/// A collapsed range degenerates into a hard threshold so the result stays
/// finite instead of dividing by zero.
fn normalize_range(value: f32, low: f32, high: f32) -> f32 {
    if high <= low {
        if value < low {
            0.0
        } else {
            1.0
        }
    } else {
        (value - low) / (high - low)
    }
}

/// Pulls values above one towards one with a soft shoulder.
fn shoulder(value: f32, highlight: f32) -> f32 {
    if highlight <= 0.0 || value <= 1.0 {
        value
    } else {
        value / (1.0 + highlight * (value - 1.0))
    }
}

/// Converts one channel through the validated grade.
fn converted(value: f32, grade: &CineonConverterGrade) -> f32 {
    let parameters = &grade.parameters;
    let black = parameters.black_point / CINEON_CONVERTER_CODE_VALUES;
    let white = parameters.white_point / CINEON_CONVERTER_CODE_VALUES;
    let internal_black = parameters.internal_black;
    let internal_white = parameters.internal_white;
    let gamma = parameters.gamma.max(MINIMUM_GAMMA);
    match parameters.conversion {
        CineonConversion::LogToLinear => {
            let normalised = normalize_range(value, black, white);
            let normalised = shoulder(normalised, parameters.highlight / HIGHLIGHT_RANGE);
            let normalised = normalised.clamp(0.0, 1.0);
            let linear = normalised.powf(1.0 / gamma);
            internal_black + linear * (internal_white - internal_black)
        }
        CineonConversion::LinearToLog => {
            let normalised = normalize_range(value, internal_black, internal_white).clamp(0.0, 1.0);
            let encoded = normalised.powf(gamma);
            black + encoded * (white - black)
        }
        CineonConversion::LinearToLinear => {
            let normalised = normalize_range(value, internal_black, internal_white).clamp(0.0, 1.0);
            black + normalised * (white - black)
        }
    }
}

/// Applies the converter to a straight-alpha scene-linear frame.
pub fn cineon_converter_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &CineonConverterGrade,
) -> bool {
    apply_cineon_converter_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Applies the converter, reporting cancellation through `checkpoint`.
pub(crate) fn apply_cineon_converter_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &CineonConverterGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    checkpoint()?;

    for chunk in working[..pixel_count].chunks_mut(4_096) {
        checkpoint()?;
        for pixel in chunk {
            // Alpha does not enter the conversion, and a fully transparent
            // pixel still carries a code value worth decoding.
            for channel in pixel.iter_mut().take(3) {
                *channel = converted(*channel, grade);
            }
        }
    }
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(parameters: CineonConverterParameters) -> CineonConverterGrade {
        CineonConverterGrade::new(parameters).expect("valid grade")
    }

    fn flat(width: usize, height: usize, colour: [f32; 4]) -> Vec<[f32; 4]> {
        vec![colour; width * height]
    }

    #[test]
    fn rejects_values_outside_the_documented_ranges() {
        let base = CineonConverterParameters::default();
        for (name, parameters) in [
            (
                "black_point",
                CineonConverterParameters { black_point: 1100.0, ..base },
            ),
            (
                "white_point",
                CineonConverterParameters { white_point: -1.0, ..base },
            ),
            (
                "internal_black",
                CineonConverterParameters { internal_black: 1.5, ..base },
            ),
            (
                "internal_white",
                CineonConverterParameters { internal_white: -0.2, ..base },
            ),
            ("gamma", CineonConverterParameters { gamma: 0.0, ..base }),
            ("gamma", CineonConverterParameters { gamma: 6.0, ..base }),
            (
                "highlight",
                CineonConverterParameters { highlight: 151.0, ..base },
            ),
            (
                "black_point",
                CineonConverterParameters { black_point: f32::NAN, ..base },
            ),
        ] {
            let error = CineonConverterGrade::new(parameters).expect_err(name);
            assert_eq!(error.parameter(), name);
        }
    }

    #[test]
    fn documented_defaults_are_stable() {
        let grade = CineonConverterGrade::documented_defaults();
        let parameters = grade.parameters();
        assert_eq!(parameters.conversion, CineonConversion::LogToLinear);
        assert_eq!(parameters.black_point, 95.0);
        assert_eq!(parameters.white_point, 685.0);
        assert_eq!(parameters.internal_black, 0.0);
        assert_eq!(parameters.internal_white, 1.0);
        assert_eq!(parameters.gamma, 1.7);
        assert_eq!(parameters.highlight, 20.0);
        assert_eq!(grade.cost(), 1);
        assert!(!grade.is_identity());
    }

    #[test]
    fn menu_keys_round_trip() {
        for conversion in CineonConversion::ALL {
            assert_eq!(
                CineonConversion::from_key(conversion.as_key()),
                Some(conversion)
            );
            assert!(conversion.as_u8() <= 2);
        }
        assert_eq!(CineonConversion::from_key("log"), None);
        assert_eq!(
            CineonConversion::from_key("linear_to_linear"),
            Some(CineonConversion::LinearToLinear)
        );
    }

    #[test]
    fn uniform_cells_carry_the_normalised_range() {
        let grade = grade(CineonConverterParameters {
            conversion: CineonConversion::LinearToLog,
            black_point: 0.0,
            white_point: 1023.0,
            internal_black: 0.25,
            internal_white: 0.75,
            gamma: 2.2,
            highlight: 150.0,
        });
        let (params, extra) = grade.gpu_controls();
        assert_eq!(params, [0.0, 1.0, 0.25, 0.75]);
        assert_eq!(extra[0], 2.2);
        assert_eq!(extra[1], 1.0);
        assert_eq!(extra[2], 1.0);
        assert_eq!(extra[3], 0.0);
    }

    #[test]
    fn decoding_maps_the_documented_points_to_the_internal_range() {
        let grade = CineonConverterGrade::documented_defaults();
        assert!((converted(95.0 / 1023.0, &grade) - 0.0).abs() <= 1.0e-6);
        let white = converted(685.0 / 1023.0, &grade);
        assert!((white - 1.0).abs() <= 1.0e-6, "{white}");
        let mid = converted(400.0 / 1023.0, &grade);
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
    }

    #[test]
    fn decoding_is_monotonic() {
        let grade = CineonConverterGrade::documented_defaults();
        let mut previous = f32::NEG_INFINITY;
        for step in 0..=100 {
            let value = step as f32 / 100.0;
            let decoded = converted(value, &grade);
            assert!(decoded >= previous - 1.0e-6, "{value} -> {decoded}");
            previous = decoded;
        }
    }

    #[test]
    fn highlight_keeps_values_above_the_white_point_below_one() {
        let parameters = CineonConverterParameters {
            highlight: 150.0,
            ..CineonConverterParameters::default()
        };
        let grade = grade(parameters);
        let clipped = converted(1.0, &grade);
        assert!(clipped <= 1.0);
        assert!(clipped > 0.9, "{clipped}");
    }

    #[test]
    fn encode_inverts_decode_when_the_shoulder_is_off() {
        let decoding = grade(CineonConverterParameters {
            highlight: 0.0,
            ..CineonConverterParameters::default()
        });
        let encoding = grade(CineonConverterParameters {
            conversion: CineonConversion::LinearToLog,
            highlight: 0.0,
            ..CineonConverterParameters::default()
        });
        for step in 0..=20 {
            let value = 95.0 / 1023.0 + step as f32 * (590.0 / 1023.0) / 20.0;
            let linear = converted(value, &decoding);
            let back = converted(linear, &encoding);
            assert!(
                (back - value).abs() <= 2.0e-5,
                "{value} -> {linear} -> {back}"
            );
        }
    }

    #[test]
    fn the_linear_mode_skips_the_log_curve() {
        let linear = grade(CineonConverterParameters {
            conversion: CineonConversion::LinearToLinear,
            black_point: 0.0,
            white_point: 1023.0,
            gamma: 2.2,
            highlight: 0.0,
            ..CineonConverterParameters::default()
        });
        let log = grade(CineonConverterParameters {
            conversion: CineonConversion::LinearToLog,
            black_point: 0.0,
            white_point: 1023.0,
            gamma: 2.2,
            highlight: 0.0,
            ..CineonConverterParameters::default()
        });
        assert!((converted(0.25, &linear) - 0.25).abs() <= 1.0e-6);
        assert!((converted(0.25, &log) - 0.25_f32.powf(2.2)).abs() <= 1.0e-6);
    }

    #[test]
    fn collapsed_points_degenerate_to_a_threshold() {
        let grade = grade(CineonConverterParameters {
            black_point: 500.0,
            white_point: 500.0,
            internal_black: 0.0,
            internal_white: 1.0,
            gamma: 1.0,
            highlight: 0.0,
            ..CineonConverterParameters::default()
        });
        assert_eq!(converted(0.4, &grade), 0.0);
        assert_eq!(converted(0.6, &grade), 1.0);
    }

    #[test]
    fn transparent_pixels_still_decode_their_colour() {
        let grade = CineonConverterGrade::documented_defaults();
        let mut pixels = flat(2, 1, [0.6, 0.6, 0.6, 0.0]);
        assert!(cineon_converter_frame_f32(&mut pixels, 2, 1, &grade));
        assert_eq!(pixels[0][3], 0.0);
        assert!(pixels[0][0] > 0.0);
    }

    #[test]
    fn alpha_is_passed_through() {
        let grade = CineonConverterGrade::documented_defaults();
        let mut pixels = flat(1, 1, [0.4, 0.5, 0.6, 0.25]);
        assert!(!grade.is_identity());
        assert!(cineon_converter_frame_f32(&mut pixels, 1, 1, &grade));
        assert_eq!(pixels[0][3], 0.25);
        assert!(pixels[0][0] > 0.0);
    }

    #[test]
    fn identity_configuration_leaves_the_frame_alone() {
        let grade = grade(CineonConverterParameters {
            black_point: 0.0,
            white_point: 1023.0,
            internal_black: 0.0,
            internal_white: 1.0,
            gamma: 1.0,
            highlight: 0.0,
            ..CineonConverterParameters::default()
        });
        assert!(grade.is_identity());
        let mut pixels = flat(3, 2, [0.1, 0.2, 0.3, 1.0]);
        let expected = pixels.clone();
        assert!(cineon_converter_frame_f32(&mut pixels, 3, 2, &grade));
        for (actual, expected) in pixels.iter().zip(&expected) {
            for channel in 0..4 {
                assert!((actual[channel] - expected[channel]).abs() <= 1.0e-6);
            }
        }
    }

    #[test]
    fn short_and_empty_frames_are_rejected() {
        let grade = CineonConverterGrade::documented_defaults();
        let mut pixels = flat(2, 2, [0.5; 4]);
        assert!(!cineon_converter_frame_f32(&mut pixels, 3, 2, &grade));
        assert!(!cineon_converter_frame_f32(&mut pixels, 0, 2, &grade));
        assert!(!cineon_converter_frame_f32(&mut [], 0, 0, &grade));
    }

    #[test]
    fn the_frame_conversion_is_deterministic() {
        let grade = CineonConverterGrade::documented_defaults();
        let mut first = vec![[0.2, 0.5, 0.9, 1.0], [0.0, 0.3, 0.7, 1.0]];
        let mut second = first.clone();
        assert!(cineon_converter_frame_f32(&mut first, 2, 1, &grade));
        assert!(cineon_converter_frame_f32(&mut second, 2, 1, &grade));
        assert_eq!(first, second);
    }

    #[test]
    fn cancellation_stops_the_conversion() {
        let grade = CineonConverterGrade::documented_defaults();
        let mut pixels = flat(5000, 1, [0.5; 4]);
        let mut calls = 0;
        let outcome =
            apply_cineon_converter_f32_controlled(&mut pixels, 5000, 1, &grade, &mut || {
                calls += 1;
                if calls == 2 {
                    Err("cancelled")
                } else {
                    Ok(())
                }
            });
        assert_eq!(outcome, Err("cancelled"));
    }

    #[test]
    fn the_hash_separates_directions_and_points() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;
        let hash = |parameters: CineonConverterParameters| {
            let mut hasher = DefaultHasher::new();
            grade(parameters).hash_state(&mut hasher);
            hasher.finish()
        };
        let base = hash(CineonConverterParameters::default());
        let direction = hash(CineonConverterParameters {
            conversion: CineonConversion::LinearToLog,
            ..CineonConverterParameters::default()
        });
        let black = hash(CineonConverterParameters {
            black_point: 100.0,
            ..CineonConverterParameters::default()
        });
        let highlight = hash(CineonConverterParameters {
            highlight: 21.0,
            ..CineonConverterParameters::default()
        });
        assert_ne!(base, direction);
        assert_ne!(base, black);
        assert_ne!(base, highlight);
    }
}
