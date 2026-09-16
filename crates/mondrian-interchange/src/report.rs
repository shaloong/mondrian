//! Versioned, machine-readable preservation and loss evidence.

use crate::{InterchangeDirection, InterchangeFormatProfile};
use mondrian_core::{ClipId, SequenceId, TimelineTime, TrackId, VideoTransitionId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Report schema version.
pub const INTERCHANGE_REPORT_VERSION: u32 = 1;

/// Finding severity. Blockers always prevent materialization/publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterchangeFindingSeverity {
    /// Exact representation or informational validation evidence.
    Info,
    /// Non-blocking loss or normalization that requires attention.
    Warning,
    /// Condition that always prevents materialization or publication.
    Blocker,
}

impl InterchangeFindingSeverity {
    /// Stable machine-readable spelling used in report aggregates.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Blocker => "blocker",
        }
    }
}

/// Exact disposition applied to one source semantic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterchangeDisposition {
    /// Native representation retained the semantic exactly.
    Preserved,
    /// Equivalent semantic retained with normalized spelling or identity.
    Normalized,
    /// Required value was deterministically created under the profile.
    Synthesized,
    /// Exact semantic retained through a declared namespaced extension.
    RepresentedByExtension,
    /// Native profile contains only a non-exact approximation.
    Approximated,
    /// Structure was reduced into one equivalent or near-equivalent layer.
    Flattened,
    /// Dynamic processing was rendered into static media.
    Baked,
    /// Semantic is absent from the native artifact.
    Omitted,
    /// Required semantic is outside the declared profile.
    Unsupported,
    /// External media must be rebound before canonical use.
    RelinkRequired,
}

impl InterchangeDisposition {
    /// Stable machine-readable spelling used in report aggregates.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preserved => "preserved",
            Self::Normalized => "normalized",
            Self::Synthesized => "synthesized",
            Self::RepresentedByExtension => "represented_by_extension",
            Self::Approximated => "approximated",
            Self::Flattened => "flattened",
            Self::Baked => "baked",
            Self::Omitted => "omitted",
            Self::Unsupported => "unsupported",
            Self::RelinkRequired => "relink_required",
        }
    }

    /// Whether this disposition changes, loses, or requires external recovery
    /// of source semantics.
    pub const fn is_unpreserved(self) -> bool {
        matches!(
            self,
            Self::Approximated
                | Self::Flattened
                | Self::Baked
                | Self::Omitted
                | Self::Unsupported
                | Self::RelinkRequired
        )
    }
}

/// Stable owner address for one finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct InterchangeOwnerAddress {
    /// Canonical Sequence owner, when a materialized/export owner exists.
    pub sequence_id: Option<SequenceId>,
    /// Canonical Track owner.
    pub track_id: Option<TrackId>,
    /// Canonical Clip owner.
    pub clip_id: Option<ClipId>,
    /// Canonical Transition owner.
    pub transition_id: Option<VideoTransitionId>,
    /// Stable foreign-format address when no canonical identity exists.
    pub external_address: Option<String>,
    /// Exact Sequence-local time associated with the finding.
    pub sequence_time: Option<TimelineTime>,
    /// Exact media-source-local time associated with the finding.
    pub source_time: Option<TimelineTime>,
}

/// One stable, actionable preservation finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterchangeFinding {
    /// Stable programmatic finding code.
    pub code: String,
    /// Operational severity.
    pub severity: InterchangeFindingSeverity,
    /// Exact preservation disposition.
    pub disposition: InterchangeDisposition,
    /// Semantic domain such as `timeline.retime` or `color.grade`.
    pub domain: String,
    /// Stable owner address.
    pub owner: InterchangeOwnerAddress,
    /// Optional bounded description of the source semantic.
    pub original: Option<String>,
    /// Optional bounded description of the projected semantic.
    pub result: Option<String>,
    /// Whether product approval is required before admitting this loss.
    pub approval_required: bool,
    /// Whether a future import/relink can recover the original semantic.
    pub reversible: bool,
}

/// Overall conformance outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterchangeOutcome {
    /// Every semantic in the declared subset was preserved or extended exactly.
    LosslessDeclaredSubset,
    /// Non-blocking losses require explicit product acknowledgement.
    LossyAcknowledgementRequired,
    /// At least one blocker prevents materialization or publication.
    Rejected,
}

/// Complete report returned for every successful inspection or preparation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterchangeConformanceReport {
    /// Version of this report JSON contract.
    pub report_version: u32,
    /// Exact qualified native profile.
    pub profile: InterchangeFormatProfile,
    /// Import or export evidence direction.
    pub direction: InterchangeDirection,
    /// SHA-256 of the inspected or prepared native artifact.
    pub source_digest_sha256: String,
    /// SHA-256 of the exact private semantic projection.
    pub semantic_fingerprint_sha256: String,
    /// SHA-256 of the profile and report capability contract.
    pub capability_fingerprint_sha256: String,
    /// Stable owner-addressed findings.
    pub findings: Vec<InterchangeFinding>,
    /// Finding counts keyed by stable severity spelling.
    pub counts_by_severity: BTreeMap<String, u64>,
    /// Finding counts keyed by stable snake-case disposition spelling.
    pub counts_by_disposition: BTreeMap<String, u64>,
    /// Overall conformance result.
    pub outcome: InterchangeOutcome,
}

impl InterchangeConformanceReport {
    pub(crate) fn new(
        profile: InterchangeFormatProfile,
        direction: InterchangeDirection,
        source: &[u8],
        semantic_bytes: &[u8],
        findings: Vec<InterchangeFinding>,
    ) -> Self {
        let has_blocker = findings
            .iter()
            .any(|finding| finding.severity == InterchangeFindingSeverity::Blocker);
        let has_loss = findings.iter().any(|finding| {
            finding.disposition.is_unpreserved()
                || finding.severity == InterchangeFindingSeverity::Warning
        });
        let outcome = if has_blocker {
            InterchangeOutcome::Rejected
        } else if has_loss {
            InterchangeOutcome::LossyAcknowledgementRequired
        } else {
            InterchangeOutcome::LosslessDeclaredSubset
        };
        let mut counts_by_severity = BTreeMap::new();
        let mut counts_by_disposition = BTreeMap::new();
        for finding in &findings {
            *counts_by_severity.entry(finding.severity.as_str().to_owned()).or_insert(0) += 1;
            *counts_by_disposition
                .entry(finding.disposition.as_str().to_owned())
                .or_insert(0) += 1;
        }
        let capability = format!("{}:contract-v1:report-v1", profile.as_str());
        Self {
            report_version: INTERCHANGE_REPORT_VERSION,
            profile,
            direction,
            source_digest_sha256: sha256_hex(source),
            semantic_fingerprint_sha256: sha256_hex(semantic_bytes),
            capability_fingerprint_sha256: sha256_hex(capability.as_bytes()),
            findings,
            counts_by_severity,
            counts_by_disposition,
            outcome,
        }
    }

    /// Whether this report contains a publication blocker.
    pub fn has_blockers(&self) -> bool {
        self.outcome == InterchangeOutcome::Rejected
    }

    /// Whether explicit user approval is required by the selected loss policy.
    pub fn requires_loss_approval(&self) -> bool {
        self.outcome == InterchangeOutcome::LossyAcknowledgementRequired
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
