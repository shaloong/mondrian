//! Stable interchange errors.

use crate::{InterchangeConformanceReport, InterchangeFormatProfile};

/// Fail-closed error returned before author state or an output artifact changes.
#[derive(Debug, thiserror::Error)]
pub enum InterchangeError {
    /// At least one configured resource limit is zero.
    #[error("interchange limits must all be non-zero")]
    InvalidLimits,
    /// Input or generated structure exceeded a closed resource bound.
    #[error("interchange input exceeds the configured {limit_name} limit ({actual} > {maximum})")]
    LimitExceeded {
        /// Stable name of the violated limit.
        limit_name: &'static str,
        /// Observed resource value.
        actual: usize,
        /// Admitted maximum.
        maximum: usize,
    },
    /// Native bytes violate the selected format grammar.
    #[error("{profile:?} artifact is malformed: {reason}")]
    MalformedArtifact {
        /// Selected native profile.
        profile: InterchangeFormatProfile,
        /// Bounded diagnostic reason.
        reason: String,
    },
    /// Valid native input requires semantics outside the declared subset.
    #[error("{profile:?} artifact contains unsupported required semantics: {reason}")]
    UnsupportedSemantics {
        /// Selected native profile.
        profile: InterchangeFormatProfile,
        /// Bounded diagnostic reason.
        reason: String,
    },
    /// A foreign media identity has no canonical Asset binding.
    #[error("interchange media key '{key}' has no product-owned Asset binding")]
    MissingMediaBinding { key: String },
    /// A foreign media identity was bound more than once.
    #[error("interchange media key '{key}' is bound more than once")]
    DuplicateMediaBinding { key: String },
    /// Repeated foreign media identity carries conflicting descriptions.
    #[error("interchange source uses duplicate media key '{key}' with conflicting metadata")]
    ConflictingMediaReference { key: String },
    /// Caller policy or a blocker rejected the conformance report.
    #[error("interchange conformance report rejected materialization or publication")]
    ConformanceRejected {
        /// Complete machine-readable evidence that caused rejection.
        report: Box<InterchangeConformanceReport>,
    },
    /// AAF operation has no helper with an accepted identity contract.
    #[error("AAF helper is required but unavailable or unqualified: {reason}")]
    AafHelperUnavailable { reason: String },
    /// Qualified AAF helper execution or binary validation failed closed.
    #[error("AAF helper failed closed: {reason}")]
    AafHelperFailed { reason: String },
    /// Canonical Timeline validation/materialization failure.
    #[error("timeline materialization failed: {0}")]
    Timeline(#[from] mondrian_core::MondrianError),
    /// Exact time arithmetic failure.
    #[error("exact timeline arithmetic failed: {0}")]
    TimelineTime(#[from] mondrian_core::TimelineTimeError),
    /// JSON or bridge serialization failure.
    #[error("interchange serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    /// Bounded helper/filesystem I/O failure.
    #[error("interchange I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

impl InterchangeError {
    /// Rejected conformance evidence, when this error was caused by loss policy
    /// or a blocker rather than malformed input or execution failure.
    pub fn conformance_report(&self) -> Option<&InterchangeConformanceReport> {
        match self {
            Self::ConformanceRejected { report } => Some(report),
            _ => None,
        }
    }
}
