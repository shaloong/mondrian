//! Persisted Gallery stills and deterministic Shot Match evidence.
//!
//! Gallery stills are Project author data, not Preview caches. Their frozen
//! display raster supports wipe/split comparison while working-linear
//! statistics support reproducible, auditable matching.

use crate::{
    AuthoringFootprint, AuthoringFootprintCollector, AuthoringFootprintError, AuthoringList,
    GalleryStillId, GradeDefinitionId, GradeVersionId, SequenceId, TimelineTime,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashSet;

/// Maximum number of authored stills in one Project Gallery.
pub const MAX_GALLERY_STILLS: usize = 512;
/// Maximum decoded PNG payload retained by one Gallery still.
pub const MAX_GALLERY_STILL_ENCODED_BYTES: usize = 64 * 1024 * 1024;
/// Maximum decoded allocation admitted for one Gallery still.
pub const MAX_GALLERY_STILL_DECODED_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum width or height admitted for one Gallery still.
pub const MAX_GALLERY_STILL_DIMENSION: u32 = 16_384;
/// Current deterministic Shot Match algorithm contract.
pub const SHOT_MATCH_ALGORITHM_VERSION: u32 = 1;

/// Display encoding of a frozen Gallery comparison raster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GalleryRasterColorSpace {
    /// IEC 61966-2-1 sRGB encoded RGBA.
    Srgb,
}

/// Immutable PNG raster stored compactly inside canonical Project JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GalleryStillRaster {
    pub width: u32,
    pub height: u32,
    pub color_space: GalleryRasterColorSpace,
    #[serde(with = "base64_bytes")]
    pub png: Vec<u8>,
}

impl GalleryStillRaster {
    /// Validate bounded raster metadata and payload admission.
    pub fn validate(&self) -> crate::Result<()> {
        if self.width == 0 || self.height == 0 {
            return Err(gallery_error("Gallery raster extent must be non-zero"));
        }
        if self.width > MAX_GALLERY_STILL_DIMENSION || self.height > MAX_GALLERY_STILL_DIMENSION {
            return Err(gallery_error(format!(
                "Gallery raster extent exceeds {MAX_GALLERY_STILL_DIMENSION} pixels"
            )));
        }
        let decoded_bytes = u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| gallery_error("Gallery raster decoded size overflows"))?;
        if decoded_bytes > MAX_GALLERY_STILL_DECODED_BYTES {
            return Err(gallery_error(format!(
                "Gallery raster decoded RGBA8 payload exceeds {MAX_GALLERY_STILL_DECODED_BYTES} bytes"
            )));
        }
        if self.png.is_empty() || self.png.len() > MAX_GALLERY_STILL_ENCODED_BYTES {
            return Err(gallery_error(format!(
                "Gallery PNG payload must contain 1..={MAX_GALLERY_STILL_ENCODED_BYTES} bytes"
            )));
        }
        if !self.png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(gallery_error("Gallery raster payload is not a PNG stream"));
        }
        Ok(())
    }
}

impl AuthoringFootprint for GalleryStillRaster {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.collect(&self.png)
    }
}

/// Bounded working-linear distribution used by deterministic Shot Match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GalleryColorStatistics {
    /// Number of non-transparent pixels sampled.
    pub sample_count: u64,
    /// Per-channel 5th percentile from a straight-alpha working frame.
    pub low_rgb: [f32; 3],
    /// Per-channel 50th percentile from a straight-alpha working frame.
    pub median_rgb: [f32; 3],
    /// Per-channel 95th percentile from a straight-alpha working frame.
    pub high_rgb: [f32; 3],
}

impl GalleryColorStatistics {
    /// Validate finite, ordered, non-empty statistics.
    pub fn validate(&self) -> crate::Result<()> {
        if self.sample_count == 0 {
            return Err(gallery_error(
                "Gallery statistics require at least one sample",
            ));
        }
        for channel in 0..3 {
            let values = [
                self.low_rgb[channel],
                self.median_rgb[channel],
                self.high_rgb[channel],
            ];
            if values.iter().any(|value| !value.is_finite()) {
                return Err(gallery_error(
                    "Gallery statistics contain a non-finite value",
                ));
            }
            if values[0] > values[1] || values[1] > values[2] {
                return Err(gallery_error(
                    "Gallery statistics percentiles are not ordered",
                ));
            }
        }
        Ok(())
    }
}

impl AuthoringFootprint for GalleryColorStatistics {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        Ok(())
    }
}

/// Grade Version identities active when a still was frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GalleryGradeVersionBinding {
    pub definition_id: GradeDefinitionId,
    pub version_id: GradeVersionId,
}

impl AuthoringFootprint for GalleryGradeVersionBinding {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        Ok(())
    }
}

/// One frozen, portable Project Gallery still.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GalleryStill {
    pub id: GalleryStillId,
    pub name: String,
    pub source_sequence_id: SequenceId,
    pub source_time: TimelineTime,
    /// Fingerprint of the author/output/display contract used for capture.
    pub presentation_fingerprint: [u8; 32],
    pub active_grade_versions: AuthoringList<GalleryGradeVersionBinding>,
    pub raster: GalleryStillRaster,
    pub statistics: GalleryColorStatistics,
}

impl GalleryStill {
    /// Validate identity-independent Gallery still state.
    pub fn validate(&self) -> crate::Result<()> {
        if self.name.trim().is_empty() {
            return Err(gallery_error("Gallery still name cannot be empty"));
        }
        self.raster.validate()?;
        self.statistics.validate()?;
        let mut definitions = HashSet::with_capacity(self.active_grade_versions.len());
        if self
            .active_grade_versions
            .iter()
            .any(|binding| !definitions.insert(binding.definition_id))
        {
            return Err(gallery_error(
                "Gallery still contains duplicate grade-definition bindings",
            ));
        }
        Ok(())
    }
}

impl AuthoringFootprint for GalleryStill {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.collect(&self.name)?;
        collector.collect(&self.active_grade_versions)?;
        collector.collect(&self.raster)?;
        collector.collect(&self.statistics)
    }
}

/// Canonical Project-owned Gallery.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectGallery {
    pub stills: AuthoringList<GalleryStill>,
}

impl ProjectGallery {
    /// Validate Gallery bounds, unique identities, and each frozen still.
    pub fn validate(&self) -> crate::Result<()> {
        if self.stills.len() > MAX_GALLERY_STILLS {
            return Err(gallery_error(format!(
                "Project Gallery exceeds {MAX_GALLERY_STILLS} stills"
            )));
        }
        let mut ids = HashSet::with_capacity(self.stills.len());
        for still in &self.stills {
            if !ids.insert(still.id) {
                return Err(gallery_error(format!(
                    "duplicate Gallery still identity {}",
                    still.id
                )));
            }
            still.validate()?;
        }
        Ok(())
    }
}

impl AuthoringFootprint for ProjectGallery {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.collect(&self.stills)
    }
}

/// Exact evidence carried by a Grade Version generated through Shot Match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShotMatchEvidence {
    pub algorithm_version: u32,
    pub reference_still_id: GalleryStillId,
    pub reference_statistics: GalleryColorStatistics,
    pub target_statistics: GalleryColorStatistics,
    /// Authored ColorWheel gain parameter.
    pub gain_rgb: [f32; 3],
    /// Authored ColorWheel offset parameter.
    pub offset_rgb: [f32; 3],
}

impl ShotMatchEvidence {
    /// Validate a supported and fully finite matching recipe.
    pub fn validate(&self) -> crate::Result<()> {
        if self.algorithm_version != SHOT_MATCH_ALGORITHM_VERSION {
            return Err(gallery_error(format!(
                "unsupported Shot Match algorithm version {}",
                self.algorithm_version
            )));
        }
        self.reference_statistics.validate()?;
        self.target_statistics.validate()?;
        if self
            .gain_rgb
            .iter()
            .chain(self.offset_rgb.iter())
            .any(|value| !value.is_finite())
        {
            return Err(gallery_error(
                "Shot Match result contains a non-finite value",
            ));
        }
        if self.gain_rgb.iter().any(|value| !(0.0..=4.0).contains(value))
            || self.offset_rgb.iter().any(|value| !(-2.0..=2.0).contains(value))
        {
            return Err(gallery_error(
                "Shot Match result exceeds ColorWheel author bounds",
            ));
        }
        Ok(())
    }
}

impl AuthoringFootprint for ShotMatchEvidence {
    fn collect_authoring_footprint(
        &self,
        collector: &mut AuthoringFootprintCollector,
    ) -> Result<(), AuthoringFootprintError> {
        collector.collect(&self.reference_statistics)?;
        collector.collect(&self.target_statistics)
    }
}

fn gallery_error(reason: impl Into<String>) -> crate::MondrianError {
    crate::MondrianError::WorkflowStepFailed {
        step_id: "gallery_author_state".to_owned(),
        reason: reason.into(),
    }
}

mod base64_bytes {
    use super::*;

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > MAX_GALLERY_STILL_ENCODED_BYTES.saturating_mul(2) {
            return Err(serde::de::Error::custom(
                "Gallery PNG base64 exceeds admission bound",
            ));
        }
        let decoded = STANDARD.decode(encoded).map_err(serde::de::Error::custom)?;
        if decoded.len() > MAX_GALLERY_STILL_ENCODED_BYTES {
            return Err(serde::de::Error::custom(
                "Gallery PNG payload exceeds admission bound",
            ));
        }
        Ok(decoded)
    }
}
