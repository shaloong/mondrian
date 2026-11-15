//! Color difference key: builds a matte from the colour distance between each
//! pixel and an authored key colour.
//!
//! The matte is assembled from two partial mattes, `A` and `B`, that share the
//! same colour-distance signal but carry independent windows. Each partial maps
//! the distance into the unit interval through its own black point, white point
//! and gamma, and takes an authored value outside that window: `black_outside`
//! below the black point and `white_outside` above the white point. The two
//! partials multiply, which lets one of them gate a cleanly lit screen while the
//! other trims the spill that survives uneven lighting.
//!
//! A final set of mask controls remaps the assembled matte: the black mask and
//! the white mask are the matte values that map to zero and one, and the mask
//! gamma bends the ramp between them. The keyed matte then multiplies the source
//! alpha; the colour channels stay straight so the compositor premultiplies the
//! result exactly once.
//!
//! Every value is read from the pixel itself, so the effect is pixel local and
//! deterministic: two runs over the same frame produce the same bytes.

use thiserror::Error;

/// Documented default of the key colour: a saturated blue screen.
pub const COLOR_DIFFERENCE_KEY_DEFAULT_KEY_COLOUR: [f32; 3] = [0.0, 0.0, 1.0];
/// Smallest colour-matching accuracy.
pub const COLOR_DIFFERENCE_KEY_MIN_ACCURACY: f32 = 1.0;
/// Largest colour-matching accuracy.
pub const COLOR_DIFFERENCE_KEY_MAX_ACCURACY: f32 = 2.0;
/// Documented default of the colour-matching accuracy.
pub const COLOR_DIFFERENCE_KEY_DEFAULT_ACCURACY: f32 = 1.0;
/// Documented default of a partial matte's black point.
pub const COLOR_DIFFERENCE_KEY_DEFAULT_BLACK: f32 = 0.0;
/// Documented default of a partial matte's white point.
pub const COLOR_DIFFERENCE_KEY_DEFAULT_WHITE: f32 = 1.0;
/// Smallest partial-matte gamma.
pub const COLOR_DIFFERENCE_KEY_MIN_GAMMA: f32 = 0.0;
/// Largest partial-matte gamma.
pub const COLOR_DIFFERENCE_KEY_MAX_GAMMA: f32 = 10.0;
/// Documented default of a partial matte's gamma.
pub const COLOR_DIFFERENCE_KEY_DEFAULT_GAMMA: f32 = 1.0;
/// Documented default of the black matte level.
pub const COLOR_DIFFERENCE_KEY_DEFAULT_BLACK_MASK: f32 = 0.0;
/// Documented default of the white matte level.
pub const COLOR_DIFFERENCE_KEY_DEFAULT_WHITE_MASK: f32 = 1.0;

/// Default black point of partial matte B, which leaves it fully open.
pub const COLOR_DIFFERENCE_KEY_DEFAULT_OPEN_BLACK_B: f32 = 1.0;
/// Default white point of partial matte B, which leaves it fully open.
pub const COLOR_DIFFERENCE_KEY_DEFAULT_OPEN_WHITE_B: f32 = 1.0;
/// Default outside value of partial matte B, which leaves it fully open.
pub const COLOR_DIFFERENCE_KEY_DEFAULT_OPEN_OUTSIDE_B: f32 = 1.0;

/// The smallest gamma that still yields a finite exponent.
const MINIMUM_GAMMA: f32 = 1.0e-3;

/// Authoring values of the colour difference key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorDifferenceKeyParameters {
    /// The colour the key removes.
    pub key_colour: [f32; 3],
    /// Colour-matching accuracy, 1 to 2. Higher values make the match stricter.
    pub accuracy: f32,
    /// Black point of partial matte `A`.
    pub black_a: f32,
    /// White point of partial matte `A`.
    pub white_a: f32,
    /// Gamma of partial matte `A`, 0 to 10.
    pub gamma_a: f32,
    /// Matte value of partial matte `A` below its black point.
    pub black_outside_a: f32,
    /// Matte value of partial matte `A` above its white point.
    pub white_outside_a: f32,
    /// Black point of partial matte `B`.
    pub black_b: f32,
    /// White point of partial matte `B`.
    pub white_b: f32,
    /// Gamma of partial matte `B`, 0 to 10.
    pub gamma_b: f32,
    /// Matte value of partial matte `B` below its black point.
    pub black_outside_b: f32,
    /// Matte value of partial matte `B` above its white point.
    pub white_outside_b: f32,
    /// Matte value that maps to fully transparent.
    pub black_mask: f32,
    /// Matte value that maps to fully opaque.
    pub white_mask: f32,
    /// Gamma of the final matte ramp, 0 to 10.
    pub mask_gamma: f32,
}

impl Default for ColorDifferenceKeyParameters {
    fn default() -> Self {
        Self {
            key_colour: COLOR_DIFFERENCE_KEY_DEFAULT_KEY_COLOUR,
            accuracy: COLOR_DIFFERENCE_KEY_DEFAULT_ACCURACY,
            black_a: COLOR_DIFFERENCE_KEY_DEFAULT_BLACK,
            white_a: COLOR_DIFFERENCE_KEY_DEFAULT_WHITE,
            gamma_a: COLOR_DIFFERENCE_KEY_DEFAULT_GAMMA,
            black_outside_a: COLOR_DIFFERENCE_KEY_DEFAULT_BLACK,
            white_outside_a: COLOR_DIFFERENCE_KEY_DEFAULT_WHITE,
            black_b: COLOR_DIFFERENCE_KEY_DEFAULT_OPEN_BLACK_B,
            white_b: COLOR_DIFFERENCE_KEY_DEFAULT_OPEN_WHITE_B,
            gamma_b: COLOR_DIFFERENCE_KEY_DEFAULT_GAMMA,
            // Partial matte B defaults to fully open, so the documented
            // defaults key the frame with partial A alone. Narrowing B's
            // window turns it into a trim on the assembled matte.
            black_outside_b: COLOR_DIFFERENCE_KEY_DEFAULT_OPEN_OUTSIDE_B,
            white_outside_b: COLOR_DIFFERENCE_KEY_DEFAULT_OPEN_OUTSIDE_B,
            black_mask: COLOR_DIFFERENCE_KEY_DEFAULT_BLACK_MASK,
            white_mask: COLOR_DIFFERENCE_KEY_DEFAULT_WHITE_MASK,
            mask_gamma: COLOR_DIFFERENCE_KEY_DEFAULT_GAMMA,
        }
    }
}

/// A parameter value that fell outside its documented range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("color_difference_key.{parameter} must be finite and within its documented range")]
pub struct ColorDifferenceKeyError {
    parameter: &'static str,
}

impl ColorDifferenceKeyError {
    /// Name of the offending parameter.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// The six uniform cells the fused point shader reads: partial matte `A`'s
/// window, the key colour, `A`'s outside values with `B`'s window, `B`'s gamma
/// and outside values with the black mask level, and the two remaining mask
/// levels.
pub type ColorDifferenceKeyUniforms = ([f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4], [f32; 4]);

/// Validated colour difference key configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorDifferenceKeyGrade {
    parameters: ColorDifferenceKeyParameters,
}

impl ColorDifferenceKeyGrade {
    /// Validates the documented ranges.
    pub fn new(parameters: ColorDifferenceKeyParameters) -> Result<Self, ColorDifferenceKeyError> {
        let finite = |parameter: &'static str, value: f32, min: f32, max: f32| {
            if value.is_finite() && value >= min && value <= max {
                Ok(())
            } else {
                Err(ColorDifferenceKeyError { parameter })
            }
        };
        for (channel, value) in ["red", "green", "blue"].into_iter().zip(parameters.key_colour) {
            finite(channel, value, 0.0, 1.0)?;
        }
        finite(
            "accuracy",
            parameters.accuracy,
            COLOR_DIFFERENCE_KEY_MIN_ACCURACY,
            COLOR_DIFFERENCE_KEY_MAX_ACCURACY,
        )?;
        for (parameter, value) in [
            ("black_a", parameters.black_a),
            ("white_a", parameters.white_a),
            ("black_outside_a", parameters.black_outside_a),
            ("white_outside_a", parameters.white_outside_a),
            ("black_b", parameters.black_b),
            ("white_b", parameters.white_b),
            ("black_outside_b", parameters.black_outside_b),
            ("white_outside_b", parameters.white_outside_b),
            ("black_mask", parameters.black_mask),
            ("white_mask", parameters.white_mask),
        ] {
            finite(parameter, value, 0.0, 1.0)?;
        }
        for (parameter, value) in [
            ("gamma_a", parameters.gamma_a),
            ("gamma_b", parameters.gamma_b),
            ("mask_gamma", parameters.mask_gamma),
        ] {
            finite(
                parameter,
                value,
                COLOR_DIFFERENCE_KEY_MIN_GAMMA,
                COLOR_DIFFERENCE_KEY_MAX_GAMMA,
            )?;
        }
        Ok(Self { parameters })
    }

    /// The documented defaults.
    pub fn documented_defaults() -> Self {
        Self {
            parameters: ColorDifferenceKeyParameters::default(),
        }
    }

    /// Borrows the validated parameters.
    pub const fn parameters(&self) -> &ColorDifferenceKeyParameters {
        &self.parameters
    }

    /// Authoring cost of the effect.
    pub const fn cost(&self) -> u8 {
        1
    }

    /// Packs the uniform cells for the fused point shader.
    ///
    /// `params` carries the accuracy, the black and white points and the gamma
    /// of partial matte `A`. `colour` carries the key colour. The remaining
    /// cells carry matte `A`'s outside values together with matte `B`'s window,
    /// and the three mask controls.
    pub fn gpu_controls(&self) -> ColorDifferenceKeyUniforms {
        (
            [
                self.parameters.accuracy,
                self.parameters.black_a,
                self.parameters.white_a,
                self.parameters.gamma_a,
            ],
            [
                self.parameters.key_colour[0],
                self.parameters.key_colour[1],
                self.parameters.key_colour[2],
                0.0,
            ],
            [
                self.parameters.black_outside_a,
                self.parameters.white_outside_a,
                self.parameters.black_b,
                self.parameters.white_b,
            ],
            [
                self.parameters.gamma_b,
                self.parameters.black_outside_b,
                self.parameters.white_outside_b,
                self.parameters.black_mask,
            ],
            [
                self.parameters.white_mask,
                self.parameters.mask_gamma,
                0.0,
                0.0,
            ],
            [0.0; 4],
        )
    }

    /// Hashes the full authored identity.
    pub(crate) fn hash_state<H: std::hash::Hasher>(&self, state: &mut H) {
        use std::hash::Hash;
        for channel in self.parameters.key_colour {
            channel.to_bits().hash(state);
        }
        for value in [
            self.parameters.accuracy,
            self.parameters.black_a,
            self.parameters.white_a,
            self.parameters.gamma_a,
            self.parameters.black_outside_a,
            self.parameters.white_outside_a,
            self.parameters.black_b,
            self.parameters.white_b,
            self.parameters.gamma_b,
            self.parameters.black_outside_b,
            self.parameters.white_outside_b,
            self.parameters.black_mask,
            self.parameters.white_mask,
            self.parameters.mask_gamma,
        ] {
            value.to_bits().hash(state);
        }
    }
}

/// Applies the key to a straight-alpha scene-linear frame.
pub fn color_difference_key_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ColorDifferenceKeyGrade,
) -> bool {
    apply_color_difference_key_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Applies the key, reporting cancellation through `checkpoint`.
pub(crate) fn apply_color_difference_key_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &ColorDifferenceKeyGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    checkpoint()?;

    let parameters = grade.parameters;
    let key = parameters.key_colour;
    for chunk in working[..pixel_count].chunks_mut(4_096) {
        checkpoint()?;
        for pixel in chunk {
            let alpha = pixel[3];
            if alpha <= 0.0 {
                continue;
            }
            let distance = colour_distance(pixel, key, parameters.accuracy);
            let partial_a = partial_mat(
                distance,
                parameters.black_a,
                parameters.white_a,
                parameters.gamma_a,
                parameters.black_outside_a,
                parameters.white_outside_a,
            );
            let partial_b = partial_mat(
                distance,
                parameters.black_b,
                parameters.white_b,
                parameters.gamma_b,
                parameters.black_outside_b,
                parameters.white_outside_b,
            );
            let matte = partial_a * partial_b;
            let remapped = remap_matte(
                matte,
                parameters.black_mask,
                parameters.white_mask,
                parameters.mask_gamma,
            );
            pixel[3] = alpha * remapped;
        }
    }
    checkpoint()?;
    Ok(true)
}

/// Maximum per-channel distance between the pixel and the key colour.
///
/// The accuracy multiplies the distance, so a higher accuracy treats more
/// colours as different from the key and therefore keys fewer pixels.
fn colour_distance(pixel: &[f32; 4], key: [f32; 3], accuracy: f32) -> f32 {
    let mut distance = 0.0_f32;
    for channel in 0..3 {
        let difference = (pixel[channel] - key[channel]).abs();
        if difference > distance {
            distance = difference;
        }
    }
    (distance * accuracy).clamp(0.0, 1.0)
}

/// Maps a colour distance through one partial matte window.
fn partial_mat(
    distance: f32,
    black: f32,
    white: f32,
    gamma: f32,
    black_outside: f32,
    white_outside: f32,
) -> f32 {
    if white <= black {
        return if distance < black {
            black_outside
        } else {
            white_outside
        };
    }
    if distance < black {
        return black_outside;
    }
    if distance > white {
        return white_outside;
    }
    let ramp = (distance - black) / (white - black);
    ramp.clamp(0.0, 1.0).powf(1.0 / gamma.max(MINIMUM_GAMMA))
}

/// Remaps the assembled matte through the black and white mask levels.
fn remap_matte(matte: f32, black_mask: f32, white_mask: f32, mask_gamma: f32) -> f32 {
    if white_mask <= black_mask {
        return if matte < black_mask { 0.0 } else { 1.0 };
    }
    let ramp = (matte - black_mask) / (white_mask - black_mask);
    ramp.clamp(0.0, 1.0).powf(1.0 / mask_gamma.max(MINIMUM_GAMMA))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_grade(parameters: ColorDifferenceKeyParameters) -> ColorDifferenceKeyGrade {
        match ColorDifferenceKeyGrade::new(parameters) {
            Ok(grade) => grade,
            Err(error) => panic!("the authored values are in range: {error}"),
        }
    }

    fn flat(width: usize, height: usize, colour: [f32; 4]) -> Vec<[f32; 4]> {
        vec![colour; width * height]
    }

    #[test]
    fn rejects_values_outside_the_documented_ranges() {
        let cases = [
            ColorDifferenceKeyParameters { accuracy: 0.5, ..Default::default() },
            ColorDifferenceKeyParameters { accuracy: 2.5, ..Default::default() },
            ColorDifferenceKeyParameters { black_a: -0.01, ..Default::default() },
            ColorDifferenceKeyParameters { white_outside_b: 1.5, ..Default::default() },
            ColorDifferenceKeyParameters { gamma_b: 10.5, ..Default::default() },
            ColorDifferenceKeyParameters { mask_gamma: -1.0, ..Default::default() },
            ColorDifferenceKeyParameters { key_colour: [0.0, 0.0, 1.2], ..Default::default() },
            ColorDifferenceKeyParameters { accuracy: f32::NAN, ..Default::default() },
        ];
        for case in cases {
            assert!(
                ColorDifferenceKeyGrade::new(case).is_err(),
                "out-of-range values must be rejected"
            );
        }
    }

    #[test]
    fn documented_defaults_are_stable() {
        let grade = ColorDifferenceKeyGrade::documented_defaults();
        let parameters = grade.parameters();
        assert_eq!(parameters.key_colour, [0.0, 0.0, 1.0]);
        assert_eq!(parameters.accuracy, 1.0);
        assert_eq!(parameters.black_a, 0.0);
        assert_eq!(parameters.white_a, 1.0);
        assert_eq!(parameters.gamma_a, 1.0);
        assert_eq!(parameters.black_b, 1.0);
        assert_eq!(parameters.white_b, 1.0);
        assert_eq!(parameters.black_outside_b, 1.0);
        assert_eq!(parameters.white_outside_b, 1.0);
        assert_eq!(parameters.black_mask, 0.0);
        assert_eq!(parameters.white_mask, 1.0);
        assert_eq!(parameters.mask_gamma, 1.0);
        assert_eq!(grade.cost(), 1);
    }

    #[test]
    fn uniforms_pack_every_authored_value() {
        let parameters = ColorDifferenceKeyParameters {
            key_colour: [0.25, 0.5, 0.75],
            accuracy: 1.5,
            black_a: 0.1,
            white_a: 0.6,
            gamma_a: 2.0,
            black_outside_a: 0.2,
            white_outside_a: 0.9,
            black_b: 0.15,
            white_b: 0.55,
            gamma_b: 0.5,
            black_outside_b: 0.3,
            white_outside_b: 0.8,
            black_mask: 0.05,
            white_mask: 0.95,
            mask_gamma: 4.0,
        };
        let grade = valid_grade(parameters);
        let (params, colour, extra0, extra1, extra2, extra3) = grade.gpu_controls();
        assert_eq!(params, [1.5, 0.1, 0.6, 2.0]);
        assert_eq!(colour, [0.25, 0.5, 0.75, 0.0]);
        assert_eq!(extra0, [0.2, 0.9, 0.15, 0.55]);
        assert_eq!(extra1, [0.5, 0.3, 0.8, 0.05]);
        assert_eq!(extra2, [0.95, 4.0, 0.0, 0.0]);
        assert_eq!(extra3, [0.0; 4]);
    }

    #[test]
    fn the_key_colour_becomes_transparent() {
        let grade = ColorDifferenceKeyGrade::documented_defaults();
        let mut frame = flat(3, 1, [0.0, 0.0, 1.0, 1.0]);
        assert!(color_difference_key_frame_f32(&mut frame, 3, 1, &grade));
        for pixel in frame {
            assert_eq!(pixel, [0.0, 0.0, 1.0, 0.0]);
        }
    }

    #[test]
    fn a_distant_colour_stays_opaque() {
        let grade = ColorDifferenceKeyGrade::documented_defaults();
        let mut frame = flat(2, 1, [1.0, 0.0, 0.0, 1.0]);
        assert!(color_difference_key_frame_f32(&mut frame, 2, 1, &grade));
        for pixel in frame {
            assert_eq!(pixel[3], 1.0);
        }
    }

    #[test]
    fn the_accuracy_makes_the_match_stricter() {
        let pixel = [0.0, 0.3, 1.0, 1.0];
        let mut loose = vec![pixel];
        let mut strict = vec![pixel];
        let loose_grade = ColorDifferenceKeyGrade::documented_defaults();
        let strict_grade =
            valid_grade(ColorDifferenceKeyParameters { accuracy: 2.0, ..Default::default() });
        assert!(color_difference_key_frame_f32(
            &mut loose,
            1,
            1,
            &loose_grade
        ));
        assert!(color_difference_key_frame_f32(
            &mut strict,
            1,
            1,
            &strict_grade
        ));
        assert!(strict[0][3] > loose[0][3], "accuracy 2 keys fewer pixels");
    }

    #[test]
    fn the_white_point_ends_the_ramp() {
        let pixel = [0.0, 0.4, 1.0, 1.0];
        let mut narrow = vec![pixel];
        let grade = valid_grade(ColorDifferenceKeyParameters {
            white_a: 0.2,
            white_outside_a: 1.0,
            ..Default::default()
        });
        assert!(color_difference_key_frame_f32(&mut narrow, 1, 1, &grade));
        assert_eq!(narrow[0][3], 1.0);
    }

    #[test]
    fn outside_values_fill_both_sides_of_the_window() {
        let grade = valid_grade(ColorDifferenceKeyParameters {
            black_a: 0.4,
            white_a: 0.6,
            black_outside_a: 1.0,
            white_outside_a: 0.25,
            ..Default::default()
        });
        let keyed = [0.0, 0.1, 1.0, 1.0];
        let ramp = [0.0, 0.5, 1.0, 1.0];
        let distant = [1.0, 1.0, 0.0, 1.0];
        let mut frame = vec![keyed, ramp, distant];
        assert!(color_difference_key_frame_f32(&mut frame, 3, 1, &grade));
        assert_eq!(frame[0][3], 1.0);
        assert!((frame[1][3] - 0.5).abs() < 1.0e-6);
        assert_eq!(frame[2][3], 0.25);
    }

    #[test]
    fn gamma_brightens_the_ramp() {
        let pixel = [0.0, 0.25, 1.0, 1.0];
        let linear = [pixel];
        let bright = [pixel];
        let mut linear = linear.to_vec();
        let mut bright = bright.to_vec();
        let linear_grade = ColorDifferenceKeyGrade::documented_defaults();
        let bright_grade =
            valid_grade(ColorDifferenceKeyParameters { gamma_a: 2.0, ..Default::default() });
        assert!(color_difference_key_frame_f32(
            &mut linear,
            1,
            1,
            &linear_grade
        ));
        assert!(color_difference_key_frame_f32(
            &mut bright,
            1,
            1,
            &bright_grade
        ));
        assert!(bright[0][3] > linear[0][3]);
    }

    #[test]
    fn the_two_partials_multiply() {
        let pixel = [0.0, 0.5, 1.0, 1.0];
        let mut solo = vec![pixel];
        let mut both = vec![pixel];
        let solo_grade = valid_grade(ColorDifferenceKeyParameters {
            black_b: 0.0,
            white_b: 0.0,
            white_outside_b: 1.0,
            ..Default::default()
        });
        let both_grade = valid_grade(ColorDifferenceKeyParameters {
            black_b: 1.0,
            white_b: 1.0,
            black_outside_b: 0.0,
            white_outside_b: 0.0,
            ..Default::default()
        });
        assert!(color_difference_key_frame_f32(&mut solo, 1, 1, &solo_grade));
        assert!(color_difference_key_frame_f32(&mut both, 1, 1, &both_grade));
        assert!((solo[0][3] - 0.5).abs() < 1.0e-6);
        assert_eq!(both[0][3], 0.0);
    }

    #[test]
    fn the_masks_remap_the_assembled_matte() {
        let pixel = [0.0, 0.5, 1.0, 1.0];
        let mut frame = vec![pixel];
        let grade = valid_grade(ColorDifferenceKeyParameters {
            black_mask: 0.25,
            white_mask: 0.75,
            ..Default::default()
        });
        assert!(color_difference_key_frame_f32(&mut frame, 1, 1, &grade));
        assert!((frame[0][3] - 0.5).abs() < 1.0e-6);
        let mut clipped = vec![pixel];
        let clipped_grade = valid_grade(ColorDifferenceKeyParameters {
            black_mask: 0.75,
            white_mask: 0.9,
            ..Default::default()
        });
        assert!(color_difference_key_frame_f32(
            &mut clipped,
            1,
            1,
            &clipped_grade
        ));
        assert_eq!(clipped[0][3], 0.0);
    }

    #[test]
    fn the_mask_gamma_bends_the_final_ramp() {
        let pixel = [0.0, 0.25, 1.0, 1.0];
        let mut linear = vec![pixel];
        let mut bright = vec![pixel];
        let linear_grade = ColorDifferenceKeyGrade::documented_defaults();
        let bright_grade =
            valid_grade(ColorDifferenceKeyParameters { mask_gamma: 2.0, ..Default::default() });
        assert!(color_difference_key_frame_f32(
            &mut linear,
            1,
            1,
            &linear_grade
        ));
        assert!(color_difference_key_frame_f32(
            &mut bright,
            1,
            1,
            &bright_grade
        ));
        assert!(bright[0][3] > linear[0][3]);
    }

    #[test]
    fn the_source_alpha_multiplies_the_matte() {
        let mut frame = vec![[0.0, 0.5, 1.0, 0.5]];
        let grade = ColorDifferenceKeyGrade::documented_defaults();
        assert!(color_difference_key_frame_f32(&mut frame, 1, 1, &grade));
        assert!((frame[0][3] - 0.25).abs() < 1.0e-6);
    }

    #[test]
    fn transparent_pixels_are_left_alone() {
        let mut frame = vec![[0.25, 0.5, 0.75, 0.0]];
        let grade = ColorDifferenceKeyGrade::documented_defaults();
        assert!(color_difference_key_frame_f32(&mut frame, 1, 1, &grade));
        assert_eq!(frame[0], [0.25, 0.5, 0.75, 0.0]);
    }

    #[test]
    fn rejects_frames_that_are_too_short() {
        let grade = ColorDifferenceKeyGrade::documented_defaults();
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!color_difference_key_frame_f32(&mut empty, 4, 4, &grade));
        let mut short = flat(2, 2, [0.0, 0.0, 1.0, 1.0]);
        short.truncate(3);
        assert!(!color_difference_key_frame_f32(&mut short, 2, 2, &grade));
    }

    #[test]
    fn the_result_is_deterministic() {
        let grade = ColorDifferenceKeyGrade::documented_defaults();
        let source: Vec<[f32; 4]> = (0..64)
            .map(|index| {
                let value = index as f32 / 64.0;
                [value, 1.0 - value, value * 0.5, 1.0]
            })
            .collect();
        let mut first = source.clone();
        let mut second = source;
        assert!(color_difference_key_frame_f32(&mut first, 8, 8, &grade));
        assert!(color_difference_key_frame_f32(&mut second, 8, 8, &grade));
        assert_eq!(first, second);
    }

    #[test]
    fn cancellation_stops_the_frame() {
        let grade = ColorDifferenceKeyGrade::documented_defaults();
        let mut frame = flat(8, 8, [0.0, 0.5, 1.0, 1.0]);
        let mut calls = 0;
        let result =
            apply_color_difference_key_f32_controlled(&mut frame, 8, 8, &grade, &mut || {
                calls += 1;
                if calls > 1 {
                    Err("cancelled")
                } else {
                    Ok(())
                }
            });
        assert_eq!(result, Err("cancelled"));
    }

    #[test]
    fn the_hash_distinguishes_each_control() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;
        let hash = |parameters: ColorDifferenceKeyParameters| {
            let mut hasher = DefaultHasher::new();
            valid_grade(parameters).hash_state(&mut hasher);
            hasher.finish()
        };
        let base = hash(ColorDifferenceKeyParameters::default());
        assert_ne!(
            base,
            hash(ColorDifferenceKeyParameters { accuracy: 1.5, ..Default::default() })
        );
        assert_ne!(
            base,
            hash(ColorDifferenceKeyParameters { gamma_b: 2.0, ..Default::default() })
        );
        assert_ne!(
            base,
            hash(ColorDifferenceKeyParameters { white_mask: 0.5, ..Default::default() })
        );
        assert_ne!(
            base,
            hash(ColorDifferenceKeyParameters {
                key_colour: [0.0, 1.0, 0.0],
                ..Default::default()
            })
        );
    }
}
