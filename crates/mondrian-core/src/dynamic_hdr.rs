//! Dynamic-HDR standard value objects shared by authoring and delivery.
//!
//! These types identify metadata semantics without claiming branded
//! certification. Machine-local licenses, executables, and entitlement grants
//! deliberately remain outside Core.

use serde::{Deserialize, Serialize};

use crate::{AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError};

/// Dynamic-metadata family requested for exact whole-source-file preservation.
///
/// Preservation admission intentionally identifies only the detectable family:
/// it does not invent a CM version, bitstream profile, or Application version
/// that the media probe cannot prove. Exact payload identity is preserved by
/// copying and hashing the complete source artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicHdrMetadataFamily {
    /// SMPTE ST 2094-40 Application #4 syntax; use of the HDR10+ brand remains
    /// subject to separate adopter and validation evidence.
    St2094_40Application4,
    /// Dolby Vision metadata; delivery remains subject to Dolby licensing and
    /// approved-tool qualification.
    DolbyVision,
}

impl DynamicHdrMetadataFamily {
    /// Stable product label that does not imply branded certification.
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::St2094_40Application4 => "ST 2094-40 Application #4",
            Self::DolbyVision => "Dolby Vision metadata",
        }
    }
}

impl AuthoringFootprint for DynamicHdrMetadataFamily {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> std::result::Result<(), AuthoringFootprintError> {
        collector.collect(&match self {
            Self::St2094_40Application4 => 0_u8,
            Self::DolbyVision => 1_u8,
        })
    }
}

impl From<&DynamicHdrStandard> for DynamicHdrMetadataFamily {
    fn from(value: &DynamicHdrStandard) -> Self {
        match value {
            DynamicHdrStandard::St2094_40Application4 { .. } => Self::St2094_40Application4,
            DynamicHdrStandard::DolbyVision { .. } => Self::DolbyVision,
        }
    }
}

/// Maximum opaque canonical metadata-level payload admitted per authored shot.
pub const MAX_DYNAMIC_HDR_LEVEL_PAYLOAD_BYTES: u32 = 256 * 1024;
/// Maximum target-specific trim summaries admitted per Dolby Vision shot.
pub const MAX_DOLBY_VISION_TRIMS_PER_SHOT: usize = 32;
/// Maximum percentile entries admitted in one ST 2094-40 analysis summary.
pub const MAX_ST2094_DISTRIBUTION_POINTS: usize = 15;

/// Closed dynamic-HDR semantic row.
///
/// A row is not a certification claim. `St2094_40Application4` means that the
/// public Application #4 syntax is authored or detected; use of the HDR10+
/// brand still requires the applicable adopter/test evidence. Dolby Vision
/// similarly requires a licensed, approved Adapter and validation toolchain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "standard", rename_all = "snake_case", deny_unknown_fields)]
pub enum DynamicHdrStandard {
    /// SMPTE ST 2094-40:2020 Application #4 metadata.
    St2094_40Application4 {
        /// Application version carried by the metadata, currently 0 or 1.
        application_version: u8,
    },
    /// Dolby Vision metadata with distinct CM and bitstream identities.
    DolbyVision {
        /// Dolby content-mapping metadata version, for example `4.0.2`.
        cm_version: String,
        /// Dolby Vision bitstream profile identity.
        bitstream_profile: u8,
        /// Backward-compatibility identifier when carried by the profile.
        compatibility_id: Option<u8>,
        /// Dolby Vision bitstream level identity when known.
        bitstream_level: Option<u8>,
    },
}

impl DynamicHdrStandard {
    /// Validate public structural identities without making a brand claim.
    pub fn validate(&self) -> crate::Result<()> {
        match self {
            Self::St2094_40Application4 { application_version } => {
                if !matches!(application_version, 0 | 1) {
                    return Err(dynamic_hdr_error(format!(
                        "ST 2094-40 Application #4 version must be 0 or 1; got {application_version}"
                    )));
                }
            }
            Self::DolbyVision { cm_version, bitstream_profile, .. } => {
                if cm_version.trim().is_empty() || cm_version.len() > 32 {
                    return Err(dynamic_hdr_error(
                        "Dolby Vision CM version must contain 1..=32 characters",
                    ));
                }
                if *bitstream_profile == 0 {
                    return Err(dynamic_hdr_error(
                        "Dolby Vision bitstream profile must be non-zero",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Stable product label that avoids implying certification.
    pub const fn diagnostic_label(&self) -> &'static str {
        match self {
            Self::St2094_40Application4 { .. } => "ST 2094-40 Application #4",
            Self::DolbyVision { .. } => "Dolby Vision metadata",
        }
    }
}

/// Ordered dynamic-HDR product-readiness state.
///
/// Runtime Adapters may advance only one proven stage at a time. Persisting a
/// later stage does not itself create entitlement or execution authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DynamicHdrReadinessStage {
    Detected,
    PreservableExactly,
    RequiresRegeneration,
    QualifiedToolAvailable,
    TechnicallyValidated,
    HumanQcApproved,
    Deliverable,
}

/// ST 2094-40 distribution point expressed on the standard integer grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct St2094DistributionPoint {
    /// Percentile in `0..=100`.
    pub percentile: u8,
    /// DistributionMaxRGB code value.
    pub value: u32,
}

/// Optional ST 2094-40 guided tone-mapping curve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct St2094ToneMappingCurve {
    /// 12-bit knee x coordinate.
    pub knee_x: u16,
    /// 12-bit knee y coordinate.
    pub knee_y: u16,
    /// Up to fifteen 10-bit Bezier anchors; Application version 1 narrows the
    /// maximum to nine.
    pub bezier_anchors: Vec<u16>,
}

/// One-window ST 2094-40 Application #4 analysis/transform summary.
///
/// This is intentionally format-specific; it is not a Dolby trim model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct St2094Application4ShotMetadata {
    /// Target display peak in whole cd/m².
    pub targeted_system_display_maximum_luminance: u32,
    /// Maximum linearized R/G/B values on the standard integer grid.
    pub max_scl: [u32; 3],
    /// AverageMaxRGB on the standard integer grid.
    pub average_max_rgb: u32,
    /// Strictly increasing percentile distribution.
    pub distribution: Vec<St2094DistributionPoint>,
    /// FractionBrightPixels in `0..=1000`.
    pub fraction_bright_pixels: u16,
    /// Optional guided tone-mapping curve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone_mapping: Option<St2094ToneMappingCurve>,
    /// Optional 6-bit color-saturation mapping weight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_saturation_weight: Option<u8>,
}

impl St2094Application4ShotMetadata {
    /// Validate bounded public Application #4 fields.
    pub fn validate(&self, application_version: u8) -> crate::Result<()> {
        if self.targeted_system_display_maximum_luminance == 0
            || self.targeted_system_display_maximum_luminance > 10_000
        {
            return Err(dynamic_hdr_error(
                "ST 2094-40 target-display peak must be in 1..=10000 cd/m²",
            ));
        }
        if self.max_scl.iter().any(|value| *value > 100_000) || self.average_max_rgb > 100_000 {
            return Err(dynamic_hdr_error(
                "ST 2094-40 MaxSCL and AverageMaxRGB must be in 0..=100000 code units",
            ));
        }
        if self.distribution.is_empty() || self.distribution.len() > MAX_ST2094_DISTRIBUTION_POINTS
        {
            return Err(dynamic_hdr_error(format!(
                "ST 2094-40 distribution must contain 1..={MAX_ST2094_DISTRIBUTION_POINTS} points"
            )));
        }
        if application_version == 1 && self.distribution.len() != 9 {
            return Err(dynamic_hdr_error(
                "ST 2094-40 Application #4 version 1 requires exactly nine distribution points",
            ));
        }
        let mut previous = None;
        for point in &self.distribution {
            if point.percentile > 100
                || point.value > 100_000
                || previous.is_some_and(|previous| point.percentile <= previous)
            {
                return Err(dynamic_hdr_error(
                    "ST 2094-40 distribution points must be strictly increasing in 0..=100 with values in 0..=100000",
                ));
            }
            previous = Some(point.percentile);
        }
        if application_version == 1
            && self.distribution[1].percentile == 5
            && self.distribution[2].percentile == 10
            && (self.distribution[1].value != 0 || self.distribution[2].value != 255)
        {
            return Err(dynamic_hdr_error(
                "ST 2094-40 version 1 reserves the 5%/10% distribution slots as 0 and 0.00255",
            ));
        }
        if self.fraction_bright_pixels > 1000 {
            return Err(dynamic_hdr_error(
                "ST 2094-40 fraction-bright-pixels must be in 0..=1000",
            ));
        }
        let maximum_anchors = if application_version == 1 { 9 } else { 15 };
        if let Some(curve) = &self.tone_mapping
            && (curve.knee_x > 4095
                || curve.knee_y > 4095
                || curve.bezier_anchors.len() > maximum_anchors
                || curve.bezier_anchors.iter().any(|anchor| *anchor > 1023)
                || curve.bezier_anchors.windows(2).any(|pair| pair[0] > pair[1]))
        {
            return Err(dynamic_hdr_error(
                "ST 2094-40 tone-mapping curve exceeds its bounded integer fields",
            ));
        }
        if self.color_saturation_weight.is_some_and(|weight| weight > 63) {
            return Err(dynamic_hdr_error(
                "ST 2094-40 saturation weight must be in 0..=63",
            ));
        }
        if application_version == 1 && self.color_saturation_weight.is_some() {
            return Err(dynamic_hdr_error(
                "ST 2094-40 Application #4 version 1 forbids ColorSaturationWeight in the qualified one-window row",
            ));
        }
        Ok(())
    }
}

/// Dolby Vision L1 min/mid/max analysis summary on the 12-bit PQ code grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DolbyVisionL1Analysis {
    pub min_pq: u16,
    pub mid_pq: u16,
    pub max_pq: u16,
}

/// Dolby target-specific trim payload retained as immutable canonical evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DolbyVisionTrimSummary {
    /// Metadata level, currently L2 or L8 for target-specific trims.
    pub level: u8,
    /// Exact target-display identity from the qualified Adapter.
    pub target_display_id: String,
    /// SHA-256 of the complete canonical level payload.
    pub canonical_payload_sha256: [u8; 32],
    /// Canonical payload byte length.
    pub canonical_payload_bytes: u32,
}

/// Immutable Dolby Vision shot summary.
///
/// Mondrian does not reinterpret opaque L2/L8 payloads. A licensed Adapter
/// owns their schema and must independently validate the canonical digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DolbyVisionShotMetadata {
    pub l1: DolbyVisionL1Analysis,
    #[serde(default)]
    pub trims: Vec<DolbyVisionTrimSummary>,
    /// Other retained metadata levels, sorted and unique.
    #[serde(default)]
    pub retained_levels: Vec<u8>,
}

impl DolbyVisionShotMetadata {
    /// Validate bounded immutable Dolby summaries.
    pub fn validate(&self, cm_version: &str) -> crate::Result<()> {
        if self.l1.max_pq > 4095
            || self.l1.mid_pq > 4095
            || self.l1.min_pq > self.l1.mid_pq
            || self.l1.mid_pq > self.l1.max_pq
        {
            return Err(dynamic_hdr_error(
                "Dolby Vision L1 analysis must satisfy 0 <= min <= mid <= max <= 4095",
            ));
        }
        if self.trims.len() > MAX_DOLBY_VISION_TRIMS_PER_SHOT {
            return Err(dynamic_hdr_error(format!(
                "Dolby Vision shot exceeds {MAX_DOLBY_VISION_TRIMS_PER_SHOT} trim summaries"
            )));
        }
        let is_cm4 =
            cm_version.trim_start().starts_with('4') || cm_version.trim_start().starts_with('5');
        let mut targets = std::collections::HashSet::new();
        for trim in &self.trims {
            if !matches!(trim.level, 2 | 8) || (trim.level == 8 && !is_cm4) {
                return Err(dynamic_hdr_error(
                    "Dolby Vision trim level is incompatible with the authored CM version",
                ));
            }
            if trim.target_display_id.trim().is_empty()
                || trim.target_display_id.len() > 128
                || trim.canonical_payload_bytes == 0
                || trim.canonical_payload_bytes > MAX_DYNAMIC_HDR_LEVEL_PAYLOAD_BYTES
                || !targets.insert((trim.level, trim.target_display_id.as_str()))
            {
                return Err(dynamic_hdr_error(
                    "Dolby Vision trim summary has an invalid or duplicate target payload",
                ));
            }
        }
        let mut previous = None;
        for level in &self.retained_levels {
            if *level == 0 || previous.is_some_and(|value| value >= *level) {
                return Err(dynamic_hdr_error(
                    "Dolby Vision retained metadata levels must be sorted and unique",
                ));
            }
            previous = Some(*level);
        }
        Ok(())
    }
}

/// Format-specific metadata owned by one final Program Output shot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DynamicHdrShotMetadata {
    St2094_40Application4(St2094Application4ShotMetadata),
    DolbyVision(DolbyVisionShotMetadata),
}

impl AuthoringFootprint for DynamicHdrStandard {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        if let Self::DolbyVision { cm_version, .. } = self {
            collector.collect(cm_version)?;
        }
        Ok(())
    }
}

impl AuthoringFootprint for St2094Application4ShotMetadata {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.collect(&self.distribution)?;
        if let Some(curve) = &self.tone_mapping {
            collector.collect(&curve.bezier_anchors)?;
        }
        Ok(())
    }
}

impl AuthoringFootprint for St2094DistributionPoint {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        Ok(())
    }
}

impl AuthoringFootprint for DolbyVisionShotMetadata {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.collect(&self.trims)
    }
}

impl AuthoringFootprint for DolbyVisionTrimSummary {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.collect(&self.target_display_id)
    }
}

impl AuthoringFootprint for DynamicHdrShotMetadata {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        match self {
            Self::St2094_40Application4(metadata) => collector.collect(metadata),
            Self::DolbyVision(metadata) => collector.collect(metadata),
        }
    }
}

fn dynamic_hdr_error(reason: impl Into<String>) -> crate::MondrianError {
    crate::MondrianError::WorkflowStepFailed {
        step_id: "dynamic_hdr_validate".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st2094() -> St2094Application4ShotMetadata {
        St2094Application4ShotMetadata {
            targeted_system_display_maximum_luminance: 1000,
            max_scl: [10_000, 9_000, 8_000],
            average_max_rgb: 1_500,
            distribution: vec![
                St2094DistributionPoint { percentile: 50, value: 1_000 },
                St2094DistributionPoint { percentile: 99, value: 9_500 },
            ],
            fraction_bright_pixels: 100,
            tone_mapping: None,
            color_saturation_weight: None,
        }
    }

    #[test]
    fn standard_rows_do_not_accept_unknown_public_versions() {
        assert!(
            DynamicHdrStandard::St2094_40Application4 { application_version: 2 }
                .validate()
                .is_err()
        );
        assert!(DynamicHdrStandard::DolbyVision {
            cm_version: "4.0.2".to_owned(),
            bitstream_profile: 8,
            compatibility_id: Some(1),
            bitstream_level: Some(6),
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn st2094_distribution_and_version_specific_fields_fail_closed() {
        let mut metadata = st2094();
        metadata.distribution[1].percentile = 50;
        assert!(metadata.validate(0).is_err());

        let mut version_one = st2094();
        version_one.distribution = [1_u8, 5, 10, 25, 50, 75, 90, 95, 99]
            .into_iter()
            .enumerate()
            .map(|(index, percentile)| St2094DistributionPoint {
                percentile,
                value: match index {
                    1 => 0,
                    2 => 255,
                    _ => (index as u32 + 1) * 1_000,
                },
            })
            .collect();
        version_one.tone_mapping = Some(St2094ToneMappingCurve {
            knee_x: 512,
            knee_y: 256,
            bezier_anchors: vec![64, 256, 768],
        });
        version_one.validate(1).expect("valid version 1 tone curve");

        version_one.color_saturation_weight = Some(1);
        assert!(version_one.validate(1).is_err());

        let mut reserved = version_one;
        reserved.color_saturation_weight = None;
        reserved.distribution[2].value = 256;
        assert!(reserved.validate(1).is_err());

        let mut version_zero = st2094();
        version_zero.distribution[0].percentile = 0;
        version_zero.tone_mapping = Some(St2094ToneMappingCurve {
            knee_x: 512,
            knee_y: 256,
            bezier_anchors: (0_u16..15).map(|value| value * 64).collect(),
        });
        version_zero.validate(0).expect("valid version 0 wider curve");
    }

    #[test]
    fn dolby_analysis_and_target_trims_are_bounded() {
        let mut metadata = DolbyVisionShotMetadata {
            l1: DolbyVisionL1Analysis { min_pq: 64, mid_pq: 512, max_pq: 3072 },
            trims: vec![DolbyVisionTrimSummary {
                level: 8,
                target_display_id: "600-nit".to_owned(),
                canonical_payload_sha256: [7; 32],
                canonical_payload_bytes: 128,
            }],
            retained_levels: vec![1, 5, 8, 9, 254],
        };
        metadata.validate("4.0.2").expect("valid CM4 summary");
        metadata.l1.mid_pq = 4096;
        assert!(metadata.validate("4.0.2").is_err());
    }
}
