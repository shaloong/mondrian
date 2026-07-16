//! HDR metadata value objects shared by media probing, timeline policy, and export.

use serde::{Deserialize, Serialize};

/// Parsed HDR side-data payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoHdrMetadataPayload {
    /// SMPTE ST 2086 mastering-display metadata.
    MasteringDisplay(VideoMasteringDisplayMetadata),
    /// CTA-861.3 MaxCLL / MaxFALL content-light metadata.
    ContentLightLevel(VideoContentLightMetadata),
    /// Embedded ICC profile interpreted as input color-family metadata.
    IccProfile(VideoIccProfileMetadata),
}

/// Rational value as stored in FFmpeg HDR metadata payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoHdrRational {
    /// Numerator.
    pub numerator: i32,
    /// Denominator.
    pub denominator: i32,
}

/// Chromaticity coordinate stored as exact rationals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoHdrChromaticity {
    /// CIE x coordinate.
    pub x: VideoHdrRational,
    /// CIE y coordinate.
    pub y: VideoHdrRational,
}

/// RGB mastering-display primaries and white point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoMasteringDisplayPrimaries {
    /// Red primary chromaticity.
    pub red: VideoHdrChromaticity,
    /// Green primary chromaticity.
    pub green: VideoHdrChromaticity,
    /// Blue primary chromaticity.
    pub blue: VideoHdrChromaticity,
    /// White point chromaticity.
    pub white_point: VideoHdrChromaticity,
}

/// Mastering-display luminance range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoMasteringDisplayLuminance {
    /// Minimum mastering-display luminance in cd/m².
    pub min: VideoHdrRational,
    /// Maximum mastering-display luminance in cd/m².
    pub max: VideoHdrRational,
}

/// SMPTE ST 2086 mastering-display metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoMasteringDisplayMetadata {
    /// RGB primaries and white point when present in the source side data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primaries: Option<VideoMasteringDisplayPrimaries>,
    /// Luminance range when present in the source side data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub luminance: Option<VideoMasteringDisplayLuminance>,
}

/// CTA-861.3 content-light metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoContentLightMetadata {
    /// Maximum content light level in cd/m².
    pub max_content_light_level: u32,
    /// Maximum frame-average light level in cd/m².
    pub max_frame_average_light_level: u32,
}

/// Structural or numeric failure in authored HDR delivery metadata.
#[derive(Debug, thiserror::Error)]
pub enum HdrMetadataValidationError {
    /// Complete delivery metadata requires mastering primaries and white point.
    #[error("HDR mastering metadata is missing display primaries and white point")]
    MissingMasteringPrimaries,
    /// Complete delivery metadata requires mastering luminance bounds.
    #[error("HDR mastering metadata is missing display luminance bounds")]
    MissingMasteringLuminance,
    /// An authored rational is negative or has a non-positive denominator.
    #[error(
        "HDR metadata field '{field}' has invalid rational {numerator}/{denominator}; numerator must be non-negative and denominator positive"
    )]
    InvalidRational {
        /// Stable field path.
        field: &'static str,
        /// Authored numerator.
        numerator: i32,
        /// Authored denominator.
        denominator: i32,
    },
    /// A CIE xy coordinate is outside the realizable chromaticity triangle.
    #[error("HDR metadata field '{field}' has invalid CIE xy coordinate ({x}, {y})")]
    InvalidChromaticity {
        /// Stable field path.
        field: &'static str,
        /// Decoded x coordinate.
        x: f64,
        /// Decoded y coordinate.
        y: f64,
    },
    /// Mastering black/peak luminance bounds are not ordered and positive.
    #[error(
        "HDR mastering luminance must satisfy 0 <= min < max; got min={min_nits} nits, max={max_nits} nits"
    )]
    InvalidMasteringLuminance {
        /// Decoded minimum luminance.
        min_nits: f64,
        /// Decoded maximum luminance.
        max_nits: f64,
    },
    /// MaxCLL/MaxFALL are absent or mutually inconsistent.
    #[error(
        "HDR content-light metadata must satisfy 0 < MaxFALL <= MaxCLL; got MaxCLL={max_cll}, MaxFALL={max_fall}"
    )]
    InvalidContentLight {
        /// Authored maximum content light level.
        max_cll: u32,
        /// Authored maximum frame-average light level.
        max_fall: u32,
    },
}

/// Embedded ICC profile metadata relevant to automatic color interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoIccProfileMetadata {
    /// Human-readable ICC profile name.
    pub name: String,
    /// Explicit ICC-to-Mondrian mapping, or an unmapped diagnostic.
    pub mapping: crate::icc::IccColorSpaceMapping,
}

impl VideoHdrMetadataPayload {
    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        match self {
            Self::MasteringDisplay(metadata) => metadata.summary(),
            Self::ContentLightLevel(metadata) => metadata.summary(),
            Self::IccProfile(metadata) => metadata.summary(),
        }
    }
}

impl VideoHdrRational {
    /// Creates a rational from an FFmpeg numerator/denominator pair.
    pub const fn new(numerator: i32, denominator: i32) -> Self {
        Self { numerator, denominator }
    }

    /// Decode a non-negative rational with a positive denominator.
    pub fn to_non_negative_f64(self) -> Option<f64> {
        if self.numerator < 0 || self.denominator <= 0 {
            return None;
        }
        Some(self.numerator as f64 / self.denominator as f64)
    }

    /// Converts the rational to an integer scale used by encoder metadata strings.
    pub fn scaled_i64(self, scale: i64) -> Option<i64> {
        if self.denominator == 0 {
            return None;
        }
        let numerator = self.numerator as i64 * scale;
        Some(div_round_nearest(numerator, self.denominator as i64))
    }
}

impl VideoMasteringDisplayMetadata {
    /// Rec.2100 PQ reference mastering metadata commonly used for 1000-nit HDR10 delivery.
    pub fn rec2100_pq_1000_nit_reference() -> Self {
        Self {
            primaries: Some(VideoMasteringDisplayPrimaries {
                red: chromaticity_50000(34_000, 16_000),
                green: chromaticity_50000(13_250, 34_500),
                blue: chromaticity_50000(7_500, 3_000),
                white_point: chromaticity_50000(15_635, 16_450),
            }),
            luminance: Some(VideoMasteringDisplayLuminance {
                min: VideoHdrRational::new(1, 10_000),
                max: VideoHdrRational::new(1000, 1),
            }),
        }
    }

    /// Validate complete SMPTE ST 2086 delivery metadata.
    pub fn validate(&self) -> Result<(), HdrMetadataValidationError> {
        let primaries = self
            .primaries
            .as_ref()
            .ok_or(HdrMetadataValidationError::MissingMasteringPrimaries)?;
        validate_chromaticity("mastering.red", primaries.red)?;
        validate_chromaticity("mastering.green", primaries.green)?;
        validate_chromaticity("mastering.blue", primaries.blue)?;
        validate_chromaticity("mastering.white_point", primaries.white_point)?;

        let luminance =
            self.luminance.ok_or(HdrMetadataValidationError::MissingMasteringLuminance)?;
        let min_nits = validate_rational("mastering.min_luminance", luminance.min)?;
        let max_nits = validate_rational("mastering.max_luminance", luminance.max)?;
        if !min_nits.is_finite() || !max_nits.is_finite() || max_nits <= 0.0 || min_nits >= max_nits
        {
            return Err(HdrMetadataValidationError::InvalidMasteringLuminance {
                min_nits,
                max_nits,
            });
        }
        Ok(())
    }

    /// Formats metadata as x265 `master-display` syntax when all required fields are present.
    pub fn to_x265_master_display(&self) -> Option<String> {
        self.validate().ok()?;
        let primaries = self.primaries.as_ref()?;
        let luminance = self.luminance?;
        Some(format!(
            "G({},{})B({},{})R({},{})WP({},{})L({},{})",
            primaries.green.x.scaled_i64(50_000)?,
            primaries.green.y.scaled_i64(50_000)?,
            primaries.blue.x.scaled_i64(50_000)?,
            primaries.blue.y.scaled_i64(50_000)?,
            primaries.red.x.scaled_i64(50_000)?,
            primaries.red.y.scaled_i64(50_000)?,
            primaries.white_point.x.scaled_i64(50_000)?,
            primaries.white_point.y.scaled_i64(50_000)?,
            luminance.max.scaled_i64(10_000)?,
            luminance.min.scaled_i64(10_000)?,
        ))
    }

    /// Compact diagnostic representation.
    pub fn summary(&self) -> String {
        match self.to_x265_master_display() {
            Some(value) => format!("master_display={value}"),
            None => format!(
                "master_display=partial(primaries={},luminance={})",
                self.primaries.is_some(),
                self.luminance.is_some()
            ),
        }
    }
}

impl VideoContentLightMetadata {
    /// HDR10 1000-nit reference MaxCLL / MaxFALL metadata.
    pub const fn hdr10_1000_nit_reference() -> Self {
        Self {
            max_content_light_level: 1000,
            max_frame_average_light_level: 400,
        }
    }

    /// Validate CTA-861.3 content-light metadata for delivery.
    pub fn validate(self) -> Result<(), HdrMetadataValidationError> {
        if self.max_content_light_level == 0
            || self.max_frame_average_light_level == 0
            || self.max_frame_average_light_level > self.max_content_light_level
        {
            return Err(HdrMetadataValidationError::InvalidContentLight {
                max_cll: self.max_content_light_level,
                max_fall: self.max_frame_average_light_level,
            });
        }
        Ok(())
    }

    /// Formats metadata as x265 `max-cll` syntax.
    pub fn to_x265_max_cll(self) -> String {
        format!(
            "{},{}",
            self.max_content_light_level, self.max_frame_average_light_level
        )
    }

    /// Compact diagnostic representation.
    pub fn summary(&self) -> String {
        format!("max_cll={}", self.to_x265_max_cll())
    }
}

impl VideoIccProfileMetadata {
    /// Compact diagnostic representation.
    pub fn summary(&self) -> String {
        match &self.mapping {
            crate::icc::IccColorSpaceMapping::Mapped { color_space, method } => {
                format!("icc_profile={}->{color_space:?} ({method:?})", self.name)
            }
            crate::icc::IccColorSpaceMapping::Unmapped { profile_color_space, reason } => format!(
                "icc_profile={} unmapped({profile_color_space}): {reason}",
                self.name
            ),
        }
    }
}

fn div_round_nearest(numerator: i64, denominator: i64) -> i64 {
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder.abs() * 2 >= denominator.abs() {
        quotient + numerator.signum() * denominator.signum()
    } else {
        quotient
    }
}

fn validate_rational(
    field: &'static str,
    value: VideoHdrRational,
) -> Result<f64, HdrMetadataValidationError> {
    value.to_non_negative_f64().ok_or(HdrMetadataValidationError::InvalidRational {
        field,
        numerator: value.numerator,
        denominator: value.denominator,
    })
}

fn validate_chromaticity(
    field: &'static str,
    value: VideoHdrChromaticity,
) -> Result<(), HdrMetadataValidationError> {
    let x = validate_rational(field, value.x)?;
    let y = validate_rational(field, value.y)?;
    if !x.is_finite()
        || !y.is_finite()
        || x > 1.0
        || y <= 0.0
        || y > 1.0
        || x + y > 1.0 + f64::EPSILON
    {
        return Err(HdrMetadataValidationError::InvalidChromaticity { field, x, y });
    }
    Ok(())
}

fn chromaticity_50000(x: i32, y: i32) -> VideoHdrChromaticity {
    VideoHdrChromaticity {
        x: VideoHdrRational::new(x, 50_000),
        y: VideoHdrRational::new(y, 50_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mastering_display_formats_x265_metadata() {
        let metadata = VideoMasteringDisplayMetadata::rec2100_pq_1000_nit_reference();

        assert_eq!(
            metadata.to_x265_master_display().as_deref(),
            Some("G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1)")
        );
    }

    #[test]
    fn content_light_formats_x265_metadata() {
        let metadata = VideoContentLightMetadata::hdr10_1000_nit_reference();

        assert_eq!(metadata.to_x265_max_cll(), "1000,400");
    }

    #[test]
    fn rational_scaling_rejects_zero_denominator() {
        assert_eq!(VideoHdrRational::new(1, 0).scaled_i64(50_000), None);
    }

    #[test]
    fn mastering_metadata_rejects_invalid_rationals_and_luminance_order() {
        let mut invalid_denominator =
            VideoMasteringDisplayMetadata::rec2100_pq_1000_nit_reference();
        invalid_denominator.luminance.as_mut().expect("reference luminance").max =
            VideoHdrRational::new(1000, 0);
        assert!(matches!(
            invalid_denominator.validate(),
            Err(HdrMetadataValidationError::InvalidRational {
                field: "mastering.max_luminance",
                ..
            })
        ));
        assert_eq!(invalid_denominator.to_x265_master_display(), None);

        let mut reversed = VideoMasteringDisplayMetadata::rec2100_pq_1000_nit_reference();
        let luminance = reversed.luminance.as_mut().expect("reference luminance");
        luminance.min = VideoHdrRational::new(1000, 1);
        luminance.max = VideoHdrRational::new(100, 1);
        assert!(matches!(
            reversed.validate(),
            Err(HdrMetadataValidationError::InvalidMasteringLuminance { .. })
        ));
    }

    #[test]
    fn mastering_metadata_rejects_impossible_chromaticity() {
        let mut invalid = VideoMasteringDisplayMetadata::rec2100_pq_1000_nit_reference();
        invalid.primaries.as_mut().expect("reference primaries").red = VideoHdrChromaticity {
            x: VideoHdrRational::new(4, 5),
            y: VideoHdrRational::new(3, 5),
        };

        assert!(matches!(
            invalid.validate(),
            Err(HdrMetadataValidationError::InvalidChromaticity { field: "mastering.red", .. })
        ));
    }

    #[test]
    fn content_light_requires_positive_ordered_levels() {
        VideoContentLightMetadata::hdr10_1000_nit_reference()
            .validate()
            .expect("valid reference metadata");
        assert!(matches!(
            VideoContentLightMetadata {
                max_content_light_level: 400,
                max_frame_average_light_level: 401,
            }
            .validate(),
            Err(HdrMetadataValidationError::InvalidContentLight { .. })
        ));
        assert!(matches!(
            VideoContentLightMetadata {
                max_content_light_level: 0,
                max_frame_average_light_level: 0,
            }
            .validate(),
            Err(HdrMetadataValidationError::InvalidContentLight { .. })
        ));
    }
}
