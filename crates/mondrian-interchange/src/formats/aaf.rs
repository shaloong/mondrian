//! AAF helper interchange contract.
//!
//! Native AAF CFB/KLV parsing stays in a separately qualified process. The
//! helper exchanges only this crate's versioned exact JSON model, keeping the
//! Timeline and App independent from pyaaf2 and platform ABI concerns.

use super::{validate_timeline, EncodedFormat, InterchangeTimeline, ParsedFormat};
use crate::{InterchangeError, InterchangeFormatProfile, InterchangeLimits};
use serde::{Deserialize, Serialize};

const PROFILE: InterchangeFormatProfile = InterchangeFormatProfile::AafEditProtocolV1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AafBridgeDocument {
    contract_version: u32,
    profile: String,
    timeline: InterchangeTimeline,
    findings: Vec<crate::InterchangeFinding>,
}

pub(crate) fn parse_helper_output(
    bytes: &[u8],
    limits: InterchangeLimits,
) -> Result<ParsedFormat, InterchangeError> {
    let document: AafBridgeDocument = serde_json::from_slice(bytes).map_err(|error| {
        InterchangeError::AafHelperFailed { reason: format!("invalid bridge JSON: {error}") }
    })?;
    if document.contract_version != crate::AAF_BRIDGE_CONTRACT_VERSION
        || document.profile != PROFILE.as_str()
    {
        return Err(InterchangeError::AafHelperFailed {
            reason: "helper bridge identity/version mismatch".to_owned(),
        });
    }
    let carries_undeclared_color =
        document.timeline.media.iter().any(|media| media.color_space.is_some())
            || document
                .timeline
                .tracks
                .iter()
                .flat_map(|track| &track.clips)
                .any(|clip| clip.color_space.is_some() || clip.static_grade.is_some());
    if carries_undeclared_color {
        return Err(InterchangeError::AafHelperFailed {
            reason: "helper bridge carried color semantics outside the declared AAF profile"
                .to_owned(),
        });
    }
    validate_timeline(PROFILE, &document.timeline, limits)?;
    Ok(ParsedFormat {
        timeline: document.timeline,
        findings: document.findings,
    })
}

pub(crate) fn encode_helper_input(
    timeline: &InterchangeTimeline,
    limits: InterchangeLimits,
) -> Result<EncodedFormat, InterchangeError> {
    validate_timeline(PROFILE, timeline, limits)?;
    let bytes = serde_json::to_vec(&AafBridgeDocument {
        contract_version: crate::AAF_BRIDGE_CONTRACT_VERSION,
        profile: PROFILE.as_str().to_owned(),
        timeline: timeline.clone(),
        findings: Vec::new(),
    })?;
    if bytes.len() > limits.max_bytes {
        return Err(InterchangeError::LimitExceeded {
            limit_name: "AAF bridge bytes",
            actual: bytes.len(),
            maximum: limits.max_bytes,
        });
    }
    Ok(EncodedFormat { bytes, findings: Vec::new() })
}
