//! Mondrian Standard display-rendering transform contracts and reference math.

use crate::ColorSpace;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

const TONE_SHOULDER_START_FRACTION: f64 = 0.75;
const GAMUT_COMPRESSION_ONSET: f64 = 0.9;

/// Versioned output target consumed by the Mondrian display transform.
///
/// Luminance values are integer cd/m² so the target remains deterministic,
/// hashable, and suitable for project/cache identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct MondrianOutputTarget {
    output_color_space: ColorSpace,
    peak_luminance_nits: u16,
    reference_white_nits: u16,
}

#[derive(Deserialize)]
struct MondrianOutputTargetWire {
    output_color_space: ColorSpace,
    peak_luminance_nits: u16,
    reference_white_nits: u16,
}

impl<'de> Deserialize<'de> for MondrianOutputTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = MondrianOutputTargetWire::deserialize(deserializer)?;
        Self::new(
            wire.output_color_space,
            wire.peak_luminance_nits,
            wire.reference_white_nits,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl MondrianOutputTarget {
    /// Build and validate an output target.
    pub fn new(
        output_color_space: ColorSpace,
        peak_luminance_nits: u16,
        reference_white_nits: u16,
    ) -> Result<Self, MondrianDisplayTransformError> {
        if matches!(
            output_color_space,
            ColorSpace::AppleLog | ColorSpace::SLog3 | ColorSpace::ArriLogC4
        ) {
            return Err(MondrianDisplayTransformError::UnsupportedOutputColorSpace {
                output_color_space,
            });
        }
        if reference_white_nits == 0 || peak_luminance_nits < reference_white_nits {
            return Err(MondrianDisplayTransformError::InvalidLuminanceRange {
                peak_luminance_nits,
                reference_white_nits,
            });
        }
        Ok(Self {
            output_color_space,
            peak_luminance_nits,
            reference_white_nits,
        })
    }

    /// Encoded output color space reached after the display-linear transform.
    pub const fn output_color_space(self) -> ColorSpace {
        self.output_color_space
    }

    /// Target peak display luminance in cd/m².
    pub const fn peak_luminance_nits(self) -> u16 {
        self.peak_luminance_nits
    }

    /// Target reference-white luminance in cd/m².
    pub const fn reference_white_nits(self) -> u16 {
        self.reference_white_nits
    }
}

/// Error returned when an MDRT contract or sample is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MondrianDisplayTransformError {
    /// Acquisition/log encodings are not display targets.
    #[error("{output_color_space:?} is not a supported Mondrian display output")]
    UnsupportedOutputColorSpace {
        /// Rejected encoded output identity.
        output_color_space: ColorSpace,
    },
    /// Peak luminance must be at least the positive reference-white luminance.
    #[error(
        "invalid Mondrian output luminance range: peak {peak_luminance_nits} nits, reference white {reference_white_nits} nits"
    )]
    InvalidLuminanceRange {
        /// Target display peak in cd/m².
        peak_luminance_nits: u16,
        /// Target display reference white in cd/m².
        reference_white_nits: u16,
    },
    /// Display-rendering math accepts only finite target-linear RGB samples.
    #[error("Mondrian display transform input contains a non-finite RGB channel")]
    NonFiniteRgb,
}

/// High-precision CPU reference for Mondrian Display Rendering Transform v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MondrianDisplayTransformV1 {
    target: MondrianOutputTarget,
}

impl MondrianDisplayTransformV1 {
    /// Build the reference transform for a validated output target.
    pub const fn new(target: MondrianOutputTarget) -> Self {
        Self { target }
    }

    /// Return the exact output target carried by this reference transform.
    pub const fn target(self) -> MondrianOutputTarget {
        self.target
    }

    /// Transform RGB already expressed in the target's linear primaries.
    ///
    /// The input and output are relative to target reference white (`1.0`).
    pub fn render_target_linear_rgb(
        self,
        rgb: [f64; 3],
    ) -> Result<[f64; 3], MondrianDisplayTransformError> {
        if !rgb.into_iter().all(f64::is_finite) {
            return Err(MondrianDisplayTransformError::NonFiniteRgb);
        }
        let luminance = dot(
            rgb,
            target_luminance_coefficients(self.target.output_color_space),
        );
        if !luminance.is_finite() {
            return Err(MondrianDisplayTransformError::NonFiniteRgb);
        }
        if luminance <= 0.0 {
            return Ok([0.0; 3]);
        }

        let headroom = f64::from(self.target.peak_luminance_nits)
            / f64::from(self.target.reference_white_nits);
        let mapped_luminance = tone_scale(luminance, headroom);
        let scale = mapped_luminance / luminance;
        Ok(compress_gamut(
            rgb.map(|channel| channel * scale),
            mapped_luminance,
            headroom,
        ))
    }
}

fn tone_scale(luminance: f64, headroom: f64) -> f64 {
    let knee = headroom * TONE_SHOULDER_START_FRACTION;
    if luminance <= knee {
        return luminance;
    }
    let shoulder_range = headroom - knee;
    let distance = luminance - knee;
    headroom - shoulder_range * shoulder_range / (distance + shoulder_range)
}

fn compress_gamut(rgb: [f64; 3], luminance: f64, headroom: f64) -> [f64; 3] {
    let chroma = rgb.map(|channel| channel - luminance);
    let mut normalized_chroma = 0.0_f64;
    for channel in chroma {
        let distance = if channel == 0.0 {
            0.0
        } else if channel > 0.0 {
            channel / (headroom - luminance)
        } else {
            -channel / luminance
        };
        normalized_chroma = normalized_chroma.max(distance);
    }
    if normalized_chroma <= GAMUT_COMPRESSION_ONSET {
        return rgb;
    }

    let compression_range = 1.0 - GAMUT_COMPRESSION_ONSET;
    let compressed_chroma = 1.0
        - compression_range * compression_range
            / (normalized_chroma + 1.0 - 2.0 * GAMUT_COMPRESSION_ONSET);
    let scale = compressed_chroma / normalized_chroma;
    chroma.map(|channel| luminance + channel * scale)
}

fn target_luminance_coefficients(output: ColorSpace) -> [f64; 3] {
    match output {
        ColorSpace::Rec709 | ColorSpace::Srgb => [0.2126, 0.7152, 0.0722],
        ColorSpace::Rec601Pal => [0.222_004, 0.706_655, 0.071_341],
        ColorSpace::Rec601Ntsc => [0.212_376, 0.701_06, 0.086_564],
        ColorSpace::Rec2020 | ColorSpace::Rec2100Hlg | ColorSpace::Rec2100Pq => {
            [0.2627, 0.6780, 0.0593]
        }
        ColorSpace::DciP3 => [0.228_974_56, 0.691_738_52, 0.079_286_91],
        ColorSpace::AppleLog | ColorSpace::SLog3 | ColorSpace::ArriLogC4 => {
            unreachable!("log acquisition spaces are rejected by MondrianOutputTarget")
        }
    }
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ColorSpace;

    #[test]
    fn mdrt_v1_preserves_sdr_values_below_the_shoulder() {
        let target =
            MondrianOutputTarget::new(ColorSpace::Rec709, 100, 100).expect("valid SDR target");
        let reference = MondrianDisplayTransformV1::new(target);
        let samples = [
            [0.0, 0.0, 0.0],
            [0.18, 0.18, 0.18],
            [0.4, 0.3, 0.2],
            [0.6, 0.6, 0.6],
        ];

        for sample in samples {
            let actual =
                reference.render_target_linear_rgb(sample).expect("finite target-linear sample");
            assert_eq!(actual, sample);
        }
    }

    #[test]
    fn mdrt_v1_neutral_axis_is_monotonic_finite_and_bounded() {
        let target =
            MondrianOutputTarget::new(ColorSpace::Rec709, 100, 100).expect("valid SDR target");
        let reference = MondrianDisplayTransformV1::new(target);
        let mut previous = 0.0;

        for step in 0..=10_000 {
            let neutral = -1.0 + f64::from(step) * 0.01;
            let actual = reference
                .render_target_linear_rgb([neutral; 3])
                .expect("finite target-linear sample");
            assert_eq!(actual[0], actual[1]);
            assert_eq!(actual[1], actual[2]);
            assert!(actual[0].is_finite());
            assert!((0.0..=1.0).contains(&actual[0]));
            assert!(actual[0] >= previous);
            previous = actual[0];
        }

        let diffuse_white =
            reference.render_target_linear_rgb([1.0; 3]).expect("finite diffuse white")[0];
        assert!((0.75..1.0).contains(&diffuse_white));
    }

    #[test]
    fn mdrt_v1_compresses_gamut_toward_neutral_without_hue_flip() {
        let target =
            MondrianOutputTarget::new(ColorSpace::Rec709, 100, 100).expect("valid SDR target");
        let reference = MondrianDisplayTransformV1::new(target);
        let coefficients = target_luminance_coefficients(ColorSpace::Rec709);
        let samples = [
            [4.0, -1.0, 0.2],
            [-2.0, 3.0, 0.5],
            [8.0, 0.1, 5.0],
            [0.2, 1.8, -0.4],
        ];

        for sample in samples {
            let actual =
                reference.render_target_linear_rgb(sample).expect("finite target-linear sample");
            assert!(actual.into_iter().all(|channel| (0.0..=1.0).contains(&channel)));

            let input_luminance = dot(sample, coefficients);
            let output_luminance = dot(actual, coefficients);
            let input_chroma = sample.map(|channel| channel - input_luminance);
            let output_chroma = actual.map(|channel| channel - output_luminance);
            let scale = output_chroma
                .iter()
                .zip(input_chroma)
                .find_map(|(output, input)| (input.abs() > 1.0e-9).then_some(*output / input))
                .expect("sample has chroma");
            assert!(scale > 0.0);
            for (output, input) in output_chroma.into_iter().zip(input_chroma) {
                assert!((output - input * scale).abs() < 1.0e-10);
            }
        }
    }

    #[test]
    fn mdrt_v1_preserves_reference_white_and_uses_hdr_headroom() {
        let target =
            MondrianOutputTarget::new(ColorSpace::Rec2100Pq, 1_000, 203).expect("valid HDR target");
        let reference = MondrianDisplayTransformV1::new(target);
        let headroom = 1_000.0 / 203.0;

        assert_eq!(
            reference.render_target_linear_rgb([1.0; 3]).expect("finite reference white"),
            [1.0; 3]
        );
        let peak =
            reference.render_target_linear_rgb([headroom; 3]).expect("finite target peak")[0];
        let super_white =
            reference.render_target_linear_rgb([1_000.0; 3]).expect("finite super-white")[0];
        assert!((headroom * 0.75..headroom).contains(&peak));
        assert!((peak..headroom).contains(&super_white));
    }

    #[test]
    fn mdrt_v1_extreme_rgb_grid_never_produces_nan_or_escapes_target_gamut() {
        let target =
            MondrianOutputTarget::new(ColorSpace::Rec709, 100, 100).expect("valid SDR target");
        let reference = MondrianDisplayTransformV1::new(target);
        let channels = [
            -f64::MAX,
            -1.0e6,
            -16.0,
            -1.0,
            0.0,
            0.18,
            0.75,
            1.0,
            4.0,
            16.0,
            1.0e6,
            f64::MAX,
        ];

        for red in channels {
            for green in channels {
                for blue in channels {
                    let actual = reference
                        .render_target_linear_rgb([red, green, blue])
                        .expect("finite target-linear sample");
                    assert!(
                        actual
                            .into_iter()
                            .all(|channel| channel.is_finite() && (0.0..=1.0).contains(&channel)),
                        "input [{red}, {green}, {blue}] produced {actual:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn mdrt_v1_tone_and_gamut_knees_are_value_and_slope_continuous() {
        let epsilon = 1.0e-6;
        let tone_knee = 0.75;
        let tone_at_knee = tone_scale(tone_knee, 1.0);
        let tone_left_slope = (tone_at_knee - tone_scale(tone_knee - epsilon, 1.0)) / epsilon;
        let tone_right_slope = (tone_scale(tone_knee + epsilon, 1.0) - tone_at_knee) / epsilon;
        assert!((tone_left_slope - tone_right_slope).abs() < 1.0e-5);

        let luminance = 0.5;
        let gamut_knee = 0.9;
        let red_at = |normalized_chroma: f64| {
            let red = luminance + normalized_chroma * (1.0 - luminance);
            compress_gamut([red, luminance, luminance], luminance, 1.0)[0]
        };
        let gamut_at_knee = red_at(gamut_knee);
        let gamut_left_slope = (gamut_at_knee - red_at(gamut_knee - epsilon)) / epsilon;
        let gamut_right_slope = (red_at(gamut_knee + epsilon) - gamut_at_knee) / epsilon;
        assert!((gamut_left_slope - gamut_right_slope).abs() < 1.0e-5);
    }

    #[test]
    fn mdrt_contract_rejects_invalid_targets_and_non_finite_samples() {
        assert_eq!(
            MondrianOutputTarget::new(ColorSpace::SLog3, 100, 100),
            Err(MondrianDisplayTransformError::UnsupportedOutputColorSpace {
                output_color_space: ColorSpace::SLog3,
            })
        );
        assert_eq!(
            MondrianOutputTarget::new(ColorSpace::Rec709, 100, 0),
            Err(MondrianDisplayTransformError::InvalidLuminanceRange {
                peak_luminance_nits: 100,
                reference_white_nits: 0,
            })
        );
        assert_eq!(
            MondrianOutputTarget::new(ColorSpace::Rec2100Pq, 100, 203),
            Err(MondrianDisplayTransformError::InvalidLuminanceRange {
                peak_luminance_nits: 100,
                reference_white_nits: 203,
            })
        );

        let target =
            MondrianOutputTarget::new(ColorSpace::Rec709, 100, 100).expect("valid SDR target");
        let reference = MondrianDisplayTransformV1::new(target);
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                reference.render_target_linear_rgb([invalid, 0.0, 0.0]),
                Err(MondrianDisplayTransformError::NonFiniteRgb)
            );
        }
    }

    #[test]
    fn mondrian_output_target_deserialization_preserves_constructor_invariants() {
        let target =
            MondrianOutputTarget::new(ColorSpace::Rec2100Pq, 1_000, 203).expect("valid HDR target");
        let json = serde_json::to_string(&target).expect("serialize output target");
        assert_eq!(
            serde_json::from_str::<MondrianOutputTarget>(&json)
                .expect("deserialize valid output target"),
            target
        );

        let zero_reference =
            r#"{"output_color_space":"Rec709","peak_luminance_nits":100,"reference_white_nits":0}"#;
        assert!(serde_json::from_str::<MondrianOutputTarget>(zero_reference).is_err());
        let acquisition_output = r#"{"output_color_space":"SLog3","peak_luminance_nits":100,"reference_white_nits":100}"#;
        assert!(serde_json::from_str::<MondrianOutputTarget>(acquisition_output).is_err());
    }
}
