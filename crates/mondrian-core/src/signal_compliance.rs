//! Shared signal-compliance math for Viewer warnings and delivery legalization.
//!
//! Monitoring classifies encoded RGB without changing Program Output. Export
//! legalization consumes the same classification contract but is an explicit,
//! destructive delivery operation applied before integer/YUV quantization.

use serde::{Deserialize, Serialize};

use crate::{ColorSpace, ProgramSignalColorimetry};

/// Exact nominal bounds used by signal warnings and delivery legalization.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SignalComplianceContract {
    /// Display-encoded RGB identity at the evaluation boundary.
    pub signal_color_space: ColorSpace,
    /// Inclusive nominal minimum in normalized encoded RGB.
    pub minimum: f32,
    /// Inclusive nominal maximum in normalized encoded RGB.
    pub maximum: f32,
}

impl SignalComplianceContract {
    /// Build the normalized RGB contract used before output range conversion.
    pub fn normalized_rgb(signal_color_space: ColorSpace) -> Result<Self, SignalComplianceError> {
        ProgramSignalColorimetry::for_color_space(signal_color_space).map_err(|_| {
            SignalComplianceError::UnsupportedSignalColorSpace { color_space: signal_color_space }
        })?;
        Ok(Self { signal_color_space, minimum: 0.0, maximum: 1.0 })
    }

    /// Validate a deserialized or externally supplied contract.
    pub fn validate(self) -> Result<Self, SignalComplianceError> {
        ProgramSignalColorimetry::for_color_space(self.signal_color_space).map_err(|_| {
            SignalComplianceError::UnsupportedSignalColorSpace {
                color_space: self.signal_color_space,
            }
        })?;
        if self.minimum != 0.0 || self.maximum != 1.0 {
            return Err(SignalComplianceError::InvalidBounds {
                minimum: self.minimum,
                maximum: self.maximum,
            });
        }
        Ok(self)
    }

    /// Classify one encoded RGB sample using the exact shared contract.
    pub fn classify(
        self,
        rgb: [f32; 3],
    ) -> Result<SignalSampleClassification, SignalComplianceError> {
        self.validate()?;
        for (channel, value) in rgb.into_iter().enumerate() {
            if !value.is_finite() {
                return Err(SignalComplianceError::NonFiniteSample { channel });
            }
        }
        let colorimetry = ProgramSignalColorimetry::for_color_space(self.signal_color_space)
            .map_err(|_| SignalComplianceError::UnsupportedSignalColorSpace {
                color_space: self.signal_color_space,
            })?;
        let luma = colorimetry.encoded_luma(rgb);
        Ok(SignalSampleClassification {
            encoded_luma: luma,
            rgb_below_nominal: rgb.map(|value| value < self.minimum),
            rgb_above_nominal: rgb.map(|value| value > self.maximum),
            luma_below_nominal: luma < self.minimum,
            luma_above_nominal: luma > self.maximum,
        })
    }

    /// Clamp encoded RGB to the nominal interval while preserving alpha.
    pub fn legalize_rgba(self, rgba: [f32; 4]) -> Result<[f32; 4], SignalComplianceError> {
        self.validate()?;
        for (channel, value) in rgba.into_iter().enumerate() {
            if !value.is_finite() {
                return Err(SignalComplianceError::NonFiniteSample { channel });
            }
        }
        Ok([
            rgba[0].clamp(self.minimum, self.maximum),
            rgba[1].clamp(self.minimum, self.maximum),
            rgba[2].clamp(self.minimum, self.maximum),
            rgba[3],
        ])
    }
}

/// Result of classifying one display-encoded RGB sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalSampleClassification {
    /// Non-constant-luminance encoded luma.
    pub encoded_luma: f32,
    /// Per-channel values below the nominal interval.
    pub rgb_below_nominal: [bool; 3],
    /// Per-channel values above the nominal interval.
    pub rgb_above_nominal: [bool; 3],
    /// Encoded luma is below the nominal interval.
    pub luma_below_nominal: bool,
    /// Encoded luma is above the nominal interval.
    pub luma_above_nominal: bool,
}

impl SignalSampleClassification {
    /// Whether any encoded RGB channel falls outside the target gamut cube.
    pub fn has_rgb_excursion(self) -> bool {
        self.rgb_below_nominal.into_iter().any(|value| value)
            || self.rgb_above_nominal.into_iter().any(|value| value)
    }
}

/// Non-destructive Viewer warning controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SignalMonitoringSettings {
    /// Replace the image with bounded exposure-zone colors.
    pub false_color: bool,
    /// Overlay diagonal stripes inside the configured luma interval.
    pub zebra: bool,
    /// Mark encoded RGB gamut excursions.
    pub gamut_alarm: bool,
    /// Inclusive encoded-luma zebra floor in thousandths of nominal signal.
    pub zebra_lower_per_mille: u16,
    /// Inclusive encoded-luma zebra ceiling in thousandths of nominal signal.
    pub zebra_upper_per_mille: u16,
}

impl Default for SignalMonitoringSettings {
    fn default() -> Self {
        Self {
            false_color: false,
            zebra: false,
            gamut_alarm: false,
            zebra_lower_per_mille: 900,
            zebra_upper_per_mille: 1_000,
        }
    }
}

impl SignalMonitoringSettings {
    /// Whether the Viewer needs the fused signal-monitoring pass.
    pub const fn is_active(self) -> bool {
        self.false_color || self.zebra || self.gamut_alarm
    }

    /// Reject ambiguous thresholds rather than silently reordering them.
    pub fn validate(self) -> Result<Self, SignalComplianceError> {
        if self.zebra_upper_per_mille > 1_000
            || self.zebra_lower_per_mille > self.zebra_upper_per_mille
        {
            return Err(SignalComplianceError::InvalidZebraBounds {
                lower_per_mille: self.zebra_lower_per_mille,
                upper_per_mille: self.zebra_upper_per_mille,
            });
        }
        Ok(self)
    }
}

/// Explicit delivery-pixel legalization policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalLegalizer {
    /// Preserve output-transform values, including excursions.
    #[default]
    Off,
    /// Hard-limit encoded RGB to the nominal signal cube before quantization.
    ClampRgb,
}

impl SignalLegalizer {
    /// Whether this policy modifies delivery pixels.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::ClampRgb)
    }
}

/// Stable failures shared by CPU, GPU admission, Viewer, and Export.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum SignalComplianceError {
    /// The boundary must be a standardized display-encoded signal.
    #[error("{color_space:?} is not a supported signal-compliance color space")]
    UnsupportedSignalColorSpace { color_space: ColorSpace },
    /// The normalized signal interval must remain exactly zero through one.
    #[error("invalid signal-compliance bounds {minimum}..={maximum}")]
    InvalidBounds { minimum: f32, maximum: f32 },
    /// Zebra interval is non-finite, reversed, or outside normalized signal.
    #[error("invalid zebra bounds {lower_per_mille}..={upper_per_mille} per mille")]
    InvalidZebraBounds {
        lower_per_mille: u16,
        upper_per_mille: u16,
    },
    /// Signal processing must never manufacture output from NaN/Infinity.
    #[error("signal sample channel {channel} is not finite")]
    NonFiniteSample { channel: usize },
    /// Classification and presentation buffers did not form one shared raster.
    #[error("signal-monitoring buffers do not form one non-empty shared raster")]
    InvalidRaster,
}

/// Apply an explicit legalizer to an encoded-float RGBA frame in place.
pub fn legalize_encoded_rgba_f32(
    rgba: &mut [[f32; 4]],
    contract: SignalComplianceContract,
    legalizer: SignalLegalizer,
) -> Result<u64, SignalComplianceError> {
    contract.validate()?;
    if !legalizer.is_active() {
        return Ok(0);
    }
    let mut changed = 0_u64;
    for pixel in rgba {
        let legalized = contract.legalize_rgba(*pixel)?;
        if legalized != *pixel {
            changed = changed.saturating_add(1);
            *pixel = legalized;
        }
    }
    Ok(changed)
}

/// Apply fused Viewer warnings to an encoded-float presentation frame.
///
/// `signal` is the selected Program/Monitor classification tap; `presentation`
/// is the monitor-adapted image that receives warning colors. Alpha is never
/// changed. Gamut alarm has priority over zebra, which has priority over false
/// color, matching the GPU implementation.
pub fn apply_signal_monitoring_rgba_f32(
    signal: &[[f32; 4]],
    presentation: &mut [[f32; 4]],
    width: u32,
    contract: SignalComplianceContract,
    settings: SignalMonitoringSettings,
) -> Result<u64, SignalComplianceError> {
    contract.validate()?;
    settings.validate()?;
    let width = usize::try_from(width).map_err(|_| SignalComplianceError::InvalidRaster)?;
    if width == 0 || signal.len() != presentation.len() || !signal.len().is_multiple_of(width) {
        return Err(SignalComplianceError::InvalidRaster);
    }
    if !settings.is_active() {
        return Ok(0);
    }
    let lower = f32::from(settings.zebra_lower_per_mille) / 1_000.0;
    let upper = f32::from(settings.zebra_upper_per_mille) / 1_000.0;
    let mut changed = 0_u64;
    for (index, (signal, output)) in signal.iter().zip(presentation.iter_mut()).enumerate() {
        let classification = contract.classify([signal[0], signal[1], signal[2]])?;
        let original = *output;
        let mut rgb = [output[0], output[1], output[2]];
        if settings.false_color {
            rgb = false_color_palette(classification.encoded_luma);
        }
        if settings.zebra
            && classification.encoded_luma >= lower
            && classification.encoded_luma <= upper
            && (((index % width) + (index / width)) / 4).is_multiple_of(2)
        {
            rgb = rgb.map(|value| value * 0.2 + 0.8);
        }
        if settings.gamut_alarm && classification.has_rgb_excursion() {
            rgb = [1.0, 0.0, 1.0];
        }
        output[..3].copy_from_slice(&rgb);
        if *output != original {
            changed = changed.saturating_add(1);
        }
    }
    Ok(changed)
}

fn false_color_palette(luma: f32) -> [f32; 3] {
    if luma < 0.02 {
        [0.45, 0.0, 0.65]
    } else if luma < 0.10 {
        [0.0, 0.15, 0.8]
    } else if luma < 0.40 {
        [0.05, 0.55, 0.75]
    } else if luma < 0.55 {
        [0.18, 0.72, 0.28]
    } else if luma < 0.70 {
        [0.72, 0.68, 0.28]
    } else if luma < 0.90 {
        [0.95, 0.42, 0.08]
    } else {
        [0.9, 0.05, 0.05]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_and_legalization_share_exact_nominal_bounds() {
        let contract = SignalComplianceContract::normalized_rgb(ColorSpace::Rec709)
            .expect("Rec.709 compliance");
        let sample = [-0.1, 0.5, 1.2, 0.25];
        let classification =
            contract.classify([sample[0], sample[1], sample[2]]).expect("finite sample");
        assert!(classification.has_rgb_excursion());
        assert_eq!(
            contract.legalize_rgba(sample).expect("legalize"),
            [0.0, 0.5, 1.0, 0.25]
        );
    }

    #[test]
    fn legalizer_preserves_alpha_and_counts_changed_pixels() {
        let mut pixels = [[-0.1, 0.2, 1.1, 0.4], [0.1, 0.2, 0.3, 0.5]];
        let changed = legalize_encoded_rgba_f32(
            &mut pixels,
            SignalComplianceContract::normalized_rgb(ColorSpace::Rec709).expect("contract"),
            SignalLegalizer::ClampRgb,
        )
        .expect("legalizer");
        assert_eq!(changed, 1);
        assert_eq!(pixels[0], [0.0, 0.2, 1.0, 0.4]);
    }

    #[test]
    fn invalid_monitoring_and_non_finite_samples_fail_closed() {
        assert!(SignalMonitoringSettings {
            zebra_lower_per_mille: 900,
            zebra_upper_per_mille: 800,
            ..Default::default()
        }
        .validate()
        .is_err());
        let contract =
            SignalComplianceContract::normalized_rgb(ColorSpace::Rec709).expect("contract");
        assert!(contract.classify([f32::NAN, 0.0, 0.0]).is_err());
    }

    #[test]
    fn fused_monitoring_uses_stable_priority_and_preserves_alpha() {
        let signal = [[1.2, 0.95, 0.95, 0.25]];
        let mut presentation = [[0.2, 0.3, 0.4, 0.25]];
        let changed = apply_signal_monitoring_rgba_f32(
            &signal,
            &mut presentation,
            1,
            SignalComplianceContract::normalized_rgb(ColorSpace::Rec709).expect("contract"),
            SignalMonitoringSettings {
                false_color: true,
                zebra: true,
                gamut_alarm: true,
                ..Default::default()
            },
        )
        .expect("monitoring");
        assert_eq!(changed, 1);
        assert_eq!(presentation[0], [1.0, 0.0, 1.0, 0.25]);
    }
}
