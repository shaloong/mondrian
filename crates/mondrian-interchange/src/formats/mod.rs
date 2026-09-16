//! Private format Adapter Seam.

pub(crate) mod aaf;
pub(crate) mod cmx3600;
pub(crate) mod fcp7_xml;
pub(crate) mod otio_json;

use crate::{InterchangeError, InterchangeFormatProfile, InterchangeLimits};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct InterchangeTimeline {
    pub name: String,
    pub rate_num: i64,
    pub rate_den: i64,
    pub start_timecode: Option<mondrian_core::SmpteTimecodeReference>,
    pub media: Vec<crate::InterchangeMediaReference>,
    pub tracks: Vec<InterchangeTrack>,
    pub transitions: Vec<InterchangeTransition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct InterchangeTrack {
    pub name: String,
    pub kind: crate::InterchangeTrackKind,
    pub enabled: bool,
    pub locked: bool,
    pub clips: Vec<InterchangeClip>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct InterchangeClip {
    pub key: String,
    pub media_key: crate::InterchangeMediaKey,
    pub name: Option<String>,
    pub record_start: i64,
    pub duration: i64,
    pub source_start: i64,
    pub enabled: bool,
    pub editorial_source: Option<mondrian_core::timeline_data::EditorialSourceIdentity>,
    pub color_space: Option<mondrian_core::ColorSpace>,
    pub static_grade: Option<InterchangeStaticGrade>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct InterchangeTransition {
    pub key: String,
    pub left_clip_key: String,
    pub right_clip_key: String,
    pub start: i64,
    pub duration: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct InterchangeStaticGrade {
    pub cdl: Option<InterchangeAscCdl>,
    pub lut: Option<InterchangeLutReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct InterchangeAscCdl {
    pub slope: [f32; 3],
    pub offset: [f32; 3],
    pub power: [f32; 3],
    pub saturation: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct InterchangeLutReference {
    pub uri: String,
    pub digest_sha256: String,
    pub processing_space: String,
    pub intensity: f32,
}

pub(crate) struct ParsedFormat {
    pub timeline: InterchangeTimeline,
    pub findings: Vec<crate::InterchangeFinding>,
}

pub(crate) struct EncodedFormat {
    pub bytes: Vec<u8>,
    pub findings: Vec<crate::InterchangeFinding>,
}

pub(crate) fn parse(
    profile: InterchangeFormatProfile,
    bytes: &[u8],
    limits: InterchangeLimits,
    explicit_rate: Option<mondrian_core::Rational>,
) -> Result<ParsedFormat, InterchangeError> {
    match profile {
        InterchangeFormatProfile::OtioJsonV1 => otio_json::parse(bytes, limits),
        InterchangeFormatProfile::Cmx3600 => cmx3600::parse(bytes, limits, explicit_rate),
        InterchangeFormatProfile::Fcp7XmlV5 => fcp7_xml::parse(bytes, limits),
        InterchangeFormatProfile::AafEditProtocolV1 => {
            Err(InterchangeError::AafHelperUnavailable {
                reason: "binary AAF import must use inspect_aaf_with_toolchain".to_owned(),
            })
        }
    }
}

pub(crate) fn encode(
    profile: InterchangeFormatProfile,
    timeline: &InterchangeTimeline,
    limits: InterchangeLimits,
) -> Result<EncodedFormat, InterchangeError> {
    match profile {
        InterchangeFormatProfile::OtioJsonV1 => otio_json::encode(timeline, limits),
        InterchangeFormatProfile::Cmx3600 => cmx3600::encode(timeline, limits),
        InterchangeFormatProfile::Fcp7XmlV5 => fcp7_xml::encode(timeline, limits),
        InterchangeFormatProfile::AafEditProtocolV1 => {
            Err(InterchangeError::AafHelperUnavailable {
                reason: "binary AAF export must use prepare_aaf_export_with_toolchain".to_owned(),
            })
        }
    }
}

pub(crate) fn validate_timeline(
    profile: InterchangeFormatProfile,
    timeline: &InterchangeTimeline,
    limits: InterchangeLimits,
) -> Result<(), InterchangeError> {
    if timeline.rate_num <= 0 || timeline.rate_den <= 0 {
        return Err(InterchangeError::MalformedArtifact {
            profile,
            reason: "frame rate must be positive".to_owned(),
        });
    }
    if timeline.tracks.len() > limits.max_tracks {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "tracks",
            actual: timeline.tracks.len(),
            maximum: limits.max_tracks,
        });
    }
    let item_count = timeline.transitions.len()
        + timeline.tracks.iter().map(|track| track.clips.len()).sum::<usize>();
    if item_count > limits.max_items {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "items",
            actual: item_count,
            maximum: limits.max_items,
        });
    }
    for text in std::iter::once(&timeline.name)
        .chain(timeline.tracks.iter().map(|track| &track.name))
        .chain(
            timeline
                .tracks
                .iter()
                .flat_map(|track| track.clips.iter().filter_map(|clip| clip.name.as_ref())),
        )
    {
        if text.len() > limits.max_string_bytes {
            return Err(InterchangeError::LimitExceeded {
                limit_name: "string bytes",
                actual: text.len(),
                maximum: limits.max_string_bytes,
            });
        }
    }
    let mut clip_keys = std::collections::HashSet::new();
    for media in &timeline.media {
        if let Some(identity) = &media.editorial_source {
            identity.validate().map_err(|error| InterchangeError::MalformedArtifact {
                profile,
                reason: format!(
                    "media '{}' has invalid editorial source identity: {error}",
                    media.key.0
                ),
            })?;
        }
    }
    for clip in timeline.tracks.iter().flat_map(|track| &track.clips) {
        if clip.duration <= 0 || clip.record_start < 0 || clip.source_start < 0 {
            return Err(InterchangeError::MalformedArtifact {
                profile,
                reason: format!("clip '{}' has invalid frame geometry", clip.key),
            });
        }
        if !clip_keys.insert(clip.key.as_str()) {
            return Err(InterchangeError::MalformedArtifact {
                profile,
                reason: format!("duplicate clip key '{}'", clip.key),
            });
        }
        if let Some(identity) = &clip.editorial_source {
            identity.validate().map_err(|error| InterchangeError::MalformedArtifact {
                profile,
                reason: format!(
                    "clip '{}' has invalid editorial source identity: {error}",
                    clip.key
                ),
            })?;
        }
        if let Some(grade) = &clip.static_grade {
            if let Some(cdl) = &grade.cdl
                && cdl
                    .slope
                    .iter()
                    .chain(&cdl.offset)
                    .chain(&cdl.power)
                    .chain(std::iter::once(&cdl.saturation))
                    .any(|value| !value.is_finite())
            {
                return Err(InterchangeError::MalformedArtifact {
                    profile,
                    reason: format!("clip '{}' has non-finite ASC CDL values", clip.key),
                });
            }
            if let Some(lut) = &grade.lut
                && (lut.uri.is_empty()
                    || lut.processing_space.is_empty()
                    || !lut.intensity.is_finite()
                    || lut.digest_sha256.len() != 64
                    || !lut.digest_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                return Err(InterchangeError::MalformedArtifact {
                    profile,
                    reason: format!("clip '{}' has an invalid LUT reference", clip.key),
                });
            }
        }
    }
    for transition in &timeline.transitions {
        if transition.duration <= 0
            || !clip_keys.contains(transition.left_clip_key.as_str())
            || !clip_keys.contains(transition.right_clip_key.as_str())
        {
            return Err(InterchangeError::MalformedArtifact {
                profile,
                reason: format!(
                    "transition '{}' has invalid endpoints or duration",
                    transition.key
                ),
            });
        }
    }
    Ok(())
}

pub(crate) fn finding(
    code: &str,
    severity: crate::InterchangeFindingSeverity,
    disposition: crate::InterchangeDisposition,
    domain: &str,
    address: Option<String>,
) -> crate::InterchangeFinding {
    crate::InterchangeFinding {
        code: code.to_owned(),
        severity,
        disposition,
        domain: domain.to_owned(),
        owner: crate::InterchangeOwnerAddress { external_address: address, ..Default::default() },
        original: None,
        result: None,
        approval_required: disposition.is_unpreserved(),
        reversible: !matches!(disposition, crate::InterchangeDisposition::Omitted),
    }
}
