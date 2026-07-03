//! HDR metadata value objects shared by media probing, timeline policy, and export.

use serde::{Deserialize, Serialize};

/// Parsed HDR side-data payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoHdrMetadataPayload {
    /// SMPTE ST 2086 mastering-display metadata.
    MasteringDisplay(VideoMasteringDisplayMetadata),
    /// CTA-861.3 MaxCLL / MaxFALL content-light metadata.
    ContentLightLevel(VideoContentLightMetadata),
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

impl VideoHdrMetadataPayload {
    /// Compact diagnostic representation for logs and export errors.
    pub fn summary(&self) -> String {
        match self {
            Self::MasteringDisplay(metadata) => metadata.summary(),
            Self::ContentLightLevel(metadata) => metadata.summary(),
        }
    }
}

impl VideoHdrRational {
    /// Creates a rational from an FFmpeg numerator/denominator pair.
    pub const fn new(numerator: i32, denominator: i32) -> Self {
        Self { numerator, denominator }
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

    /// Formats metadata as x265 `master-display` syntax when all required fields are present.
    pub fn to_x265_master_display(&self) -> Option<String> {
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

fn div_round_nearest(numerator: i64, denominator: i64) -> i64 {
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    if remainder.abs() * 2 >= denominator.abs() {
        quotient + numerator.signum() * denominator.signum()
    } else {
        quotient
    }
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
}
