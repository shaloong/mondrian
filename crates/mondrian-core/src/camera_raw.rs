//! Camera RAW authoring intent and probe contracts.
//!
//! Source parsing and debayer execution belong to `mondrian-media`. Core owns
//! only the stable persisted controls and the closed facts that cross module
//! boundaries.

use serde::{Deserialize, Serialize};

/// Camera RAW source Adapter selected by a media probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraRawAdapter {
    /// TIFF/DNG parsing plus FFmpeg Bayer decompression.
    Dng,
    /// CinemaDNG frame-file parsing plus FFmpeg Bayer decompression.
    CinemaDng,
}

/// Two-by-two color-filter-array arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraRawCfaPattern {
    /// Red, green / green, blue.
    Rggb,
    /// Blue, green / green, red.
    Bggr,
    /// Green, blue / red, green.
    Gbrg,
    /// Green, red / blue, green.
    Grbg,
}

/// Probe facts for one camera RAW picture stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CameraRawMetadata {
    /// Adapter that proved and can execute this source.
    pub adapter: CameraRawAdapter,
    /// Sensor color-filter-array layout.
    pub cfa_pattern: CameraRawCfaPattern,
    /// Full sensor raster width selected from the RAW image directory.
    pub width: u32,
    /// Full sensor raster height selected from the RAW image directory.
    pub height: u32,
    /// Stored sensor component precision.
    pub bit_depth: u8,
    /// TIFF/DNG compression code retained for diagnostics.
    pub compression: u16,
    /// Camera manufacturer, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_make: Option<String>,
    /// Camera model, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_model: Option<String>,
    /// Whether a source ColorMatrix was present and admitted.
    pub has_color_matrix: bool,
    /// Whether source AsShotNeutral metadata was present and admitted.
    pub has_as_shot_neutral: bool,
}

/// Camera RAW white-balance intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum CameraRawWhiteBalance {
    /// Use the DNG AsShotNeutral camera metadata.
    #[default]
    CameraMetadata,
    /// Resolve a user-authored correlated color temperature and tint.
    TemperatureTint {
        /// Correlated color temperature in Kelvin.
        temperature_kelvin: u16,
        /// Green/magenta adjustment in milli-units; positive values add green.
        tint_milli: i16,
    },
}

/// Debayer algorithm quality selected for RAW materialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CameraRawDebayerQuality {
    /// Bounded bilinear interpolation, useful for responsive drafts.
    Bilinear,
    /// Directional green reconstruction followed by color-difference interpolation.
    #[default]
    EdgeAware,
}

/// Persistent Camera RAW controls stored on an asset record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct CameraRawInterpretation {
    /// Exposure adjustment in one-thousandth stops.
    #[serde(default)]
    pub exposure_millistops: i16,
    /// Camera or authored white-balance selection.
    #[serde(default)]
    pub white_balance: CameraRawWhiteBalance,
    /// Debayer quality used by Preview and Export.
    #[serde(default)]
    pub debayer_quality: CameraRawDebayerQuality,
}

impl CameraRawInterpretation {
    /// Smallest admitted exposure correction.
    pub const MIN_EXPOSURE_MILLISTOPS: i16 = -5_000;
    /// Largest admitted exposure correction.
    pub const MAX_EXPOSURE_MILLISTOPS: i16 = 5_000;
    /// Smallest admitted authored white-balance temperature.
    pub const MIN_TEMPERATURE_KELVIN: u16 = 2_000;
    /// Largest admitted authored white-balance temperature.
    pub const MAX_TEMPERATURE_KELVIN: u16 = 50_000;
    /// Absolute tint bound in milli-units.
    pub const MAX_TINT_MILLI: i16 = 1_000;

    /// Validate bounded authoring state before it enters execution/cache identity.
    pub fn validate(self) -> Result<(), CameraRawInterpretationError> {
        if !(Self::MIN_EXPOSURE_MILLISTOPS..=Self::MAX_EXPOSURE_MILLISTOPS)
            .contains(&self.exposure_millistops)
        {
            return Err(CameraRawInterpretationError::ExposureOutOfRange {
                exposure_millistops: self.exposure_millistops,
            });
        }
        if let CameraRawWhiteBalance::TemperatureTint { temperature_kelvin, tint_milli } =
            self.white_balance
        {
            if !(Self::MIN_TEMPERATURE_KELVIN..=Self::MAX_TEMPERATURE_KELVIN)
                .contains(&temperature_kelvin)
            {
                return Err(CameraRawInterpretationError::TemperatureOutOfRange {
                    temperature_kelvin,
                });
            }
            if tint_milli.unsigned_abs() > Self::MAX_TINT_MILLI as u16 {
                return Err(CameraRawInterpretationError::TintOutOfRange { tint_milli });
            }
        }
        Ok(())
    }
}

/// Invalid persistent Camera RAW authoring state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CameraRawInterpretationError {
    /// Exposure exceeded the closed product range.
    #[error("camera RAW exposure {exposure_millistops} millistops is outside -5000..=5000")]
    ExposureOutOfRange {
        /// Invalid exposure value.
        exposure_millistops: i16,
    },
    /// Temperature exceeded the closed product range.
    #[error("camera RAW temperature {temperature_kelvin}K is outside 2000..=50000K")]
    TemperatureOutOfRange {
        /// Invalid temperature value.
        temperature_kelvin: u16,
    },
    /// Tint exceeded the closed product range.
    #[error("camera RAW tint {tint_milli} is outside -1000..=1000")]
    TintOutOfRange {
        /// Invalid tint value.
        tint_milli: i16,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_raw_interpretation_validates_closed_bounds() {
        CameraRawInterpretation::default().validate().expect("default RAW intent");
        assert!(CameraRawInterpretation {
            exposure_millistops: 5_001,
            ..CameraRawInterpretation::default()
        }
        .validate()
        .is_err());
        assert!(CameraRawInterpretation {
            white_balance: CameraRawWhiteBalance::TemperatureTint {
                temperature_kelvin: 1_999,
                tint_milli: 0,
            },
            ..CameraRawInterpretation::default()
        }
        .validate()
        .is_err());
    }
}
