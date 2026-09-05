//! Typed ownership evidence for the process-local winit event loop.
//!
//! Dropping the Rust owner proves only that Mondrian returned its in-process
//! authority. It deliberately does not claim that the OS compositor or native
//! display server has reached a physically terminal state.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const EVENT_LOOP_SHUTDOWN_RECEIPT_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_EVENT_LOOP_SHUTDOWN_RECEIPT_JSON_BYTES: usize = 1024;

/// Stable classification of a winit event-loop construction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppUiEventLoopConstructionFailureKind {
    /// The selected platform backend cannot provide the requested event loop.
    NotSupported,
    /// The operating system rejected event-loop construction.
    OperatingSystem,
    /// Winit rejected a second process-local event-loop owner.
    RecreationAttempt,
    /// Winit reported an application exit status while constructing/running.
    ExitFailure(i32),
}

/// Exact typed failure returned before an event-loop owner exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUiEventLoopConstructionFailure {
    kind: AppUiEventLoopConstructionFailureKind,
    diagnostic: String,
}

impl AppUiEventLoopConstructionFailure {
    /// Stable failure classification.
    pub const fn kind(&self) -> AppUiEventLoopConstructionFailureKind {
        self.kind
    }

    /// Original human-readable winit diagnostic.
    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }

    #[cfg(test)]
    pub(crate) fn synthetic(
        kind: AppUiEventLoopConstructionFailureKind,
        diagnostic: impl Into<String>,
    ) -> Self {
        Self { kind, diagnostic: diagnostic.into() }
    }
}

impl From<winit::error::EventLoopError> for AppUiEventLoopConstructionFailure {
    fn from(error: winit::error::EventLoopError) -> Self {
        let kind = match &error {
            winit::error::EventLoopError::NotSupported(_) => {
                AppUiEventLoopConstructionFailureKind::NotSupported
            }
            winit::error::EventLoopError::Os(_) => {
                AppUiEventLoopConstructionFailureKind::OperatingSystem
            }
            winit::error::EventLoopError::RecreationAttempt => {
                AppUiEventLoopConstructionFailureKind::RecreationAttempt
            }
            winit::error::EventLoopError::ExitFailure(status) => {
                AppUiEventLoopConstructionFailureKind::ExitFailure(*status)
            }
        };
        Self { kind, diagnostic: error.to_string() }
    }
}

impl std::fmt::Display for AppUiEventLoopConstructionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.diagnostic)
    }
}

impl std::error::Error for AppUiEventLoopConstructionFailure {}

/// Rust-owner handback evidence emitted only after the event-loop drop returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppUiEventLoopShutdownEvidence {
    rust_owner_released: bool,
}

impl AppUiEventLoopShutdownEvidence {
    pub(crate) const fn after_owner_drop() -> Self {
        Self { rust_owner_released: true }
    }

    /// Whether the process-local Rust event-loop owner was released.
    pub const fn rust_owner_released(self) -> bool {
        self.rust_owner_released
    }

    /// Native physical termination is outside this Rust ownership proof.
    pub const fn qualifies_physical_native_termination(self) -> bool {
        false
    }
}

/// Canonical bounded receipt for one returned process-local EventLoop owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUiEventLoopShutdownReceipt {
    evidence: AppUiEventLoopShutdownEvidence,
    canonical_json: String,
    sha256: String,
}

impl AppUiEventLoopShutdownReceipt {
    pub(crate) fn seal(
        evidence: AppUiEventLoopShutdownEvidence,
    ) -> Result<Self, AppUiEventLoopShutdownReceiptError> {
        let projection = CanonicalEventLoopShutdownEvidence::from(evidence);
        projection.validate()?;
        let canonical_json = serde_json::to_string(&projection).map_err(|error| {
            AppUiEventLoopShutdownReceiptError::Serialization(error.to_string())
        })?;
        if canonical_json.len() > MAXIMUM_EVENT_LOOP_SHUTDOWN_RECEIPT_JSON_BYTES {
            return Err(AppUiEventLoopShutdownReceiptError::TooLarge);
        }
        let sha256 = event_loop_lower_sha256(canonical_json.as_bytes());
        Ok(Self { evidence, canonical_json, sha256 })
    }

    /// Exact process-local Rust owner evidence represented by this receipt.
    pub const fn evidence(&self) -> AppUiEventLoopShutdownEvidence {
        self.evidence
    }

    /// Canonical UTF-8 JSON for durable embedding.
    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }

    /// SHA-256 over the exact bytes returned by [`Self::canonical_json`].
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Revalidate the schema, canonical bytes, and Rust-owner return predicate.
    pub fn verify_integrity(
        canonical_json: &str,
        sha256: &str,
    ) -> Result<(), AppUiEventLoopShutdownReceiptError> {
        if canonical_json.len() > MAXIMUM_EVENT_LOOP_SHUTDOWN_RECEIPT_JSON_BYTES {
            return Err(AppUiEventLoopShutdownReceiptError::TooLarge);
        }
        if event_loop_lower_sha256(canonical_json.as_bytes()) != sha256 {
            return Err(AppUiEventLoopShutdownReceiptError::HashMismatch);
        }
        let projection: CanonicalEventLoopShutdownEvidence = serde_json::from_str(canonical_json)
            .map_err(|error| {
            AppUiEventLoopShutdownReceiptError::Serialization(error.to_string())
        })?;
        projection.validate()?;
        let normalized = serde_json::to_string(&projection).map_err(|error| {
            AppUiEventLoopShutdownReceiptError::Serialization(error.to_string())
        })?;
        if normalized != canonical_json {
            return Err(AppUiEventLoopShutdownReceiptError::NonCanonicalEvidence);
        }
        Ok(())
    }

    /// Physical native termination remains unqualified without OS evidence.
    pub const fn qualifies_physical_native_termination(&self) -> bool {
        false
    }
}

/// Stable EventLoop shutdown receipt sealing or replay rejection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppUiEventLoopShutdownReceiptError {
    /// The canonical projection violates its schema or owner-return invariant.
    #[error("EventLoop shutdown receipt evidence is invalid")]
    InvalidEvidence,
    /// The canonical JSON does not match its supplied digest.
    #[error("EventLoop shutdown receipt hash does not match canonical JSON")]
    HashMismatch,
    /// Valid JSON was not encoded in the one canonical compact form.
    #[error("EventLoop shutdown receipt JSON is not canonical")]
    NonCanonicalEvidence,
    /// Canonical encoding or decoding failed.
    #[error("EventLoop shutdown receipt serialization failed: {0}")]
    Serialization(String),
    /// The bounded receipt exceeded its schema limit.
    #[error("EventLoop shutdown receipt exceeds its maximum canonical size")]
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalEventLoopShutdownEvidence {
    schema_version: u32,
    rust_owner_released: bool,
    physical_native_termination: EventLoopPhysicalNativeTermination,
}

impl From<AppUiEventLoopShutdownEvidence> for CanonicalEventLoopShutdownEvidence {
    fn from(evidence: AppUiEventLoopShutdownEvidence) -> Self {
        Self {
            schema_version: EVENT_LOOP_SHUTDOWN_RECEIPT_SCHEMA_VERSION,
            rust_owner_released: evidence.rust_owner_released(),
            physical_native_termination: EventLoopPhysicalNativeTermination::Unverified,
        }
    }
}

impl CanonicalEventLoopShutdownEvidence {
    fn validate(self) -> Result<(), AppUiEventLoopShutdownReceiptError> {
        if self.schema_version != EVENT_LOOP_SHUTDOWN_RECEIPT_SCHEMA_VERSION
            || !self.rust_owner_released
            || self.physical_native_termination != EventLoopPhysicalNativeTermination::Unverified
        {
            return Err(AppUiEventLoopShutdownReceiptError::InvalidEvidence);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EventLoopPhysicalNativeTermination {
    Unverified,
}

fn event_loop_lower_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_receipt_round_trips_and_never_claims_physical_native_termination() {
        let receipt =
            AppUiEventLoopShutdownReceipt::seal(AppUiEventLoopShutdownEvidence::after_owner_drop())
                .expect("returned EventLoop owner should seal");

        assert!(receipt.evidence().rust_owner_released());
        assert!(!receipt.qualifies_physical_native_termination());
        assert_eq!(
            AppUiEventLoopShutdownReceipt::verify_integrity(
                receipt.canonical_json(),
                receipt.sha256(),
            ),
            Ok(())
        );
    }

    #[test]
    fn shutdown_receipt_rejects_tamper_noncanonical_and_native_promotion() {
        let receipt =
            AppUiEventLoopShutdownReceipt::seal(AppUiEventLoopShutdownEvidence::after_owner_drop())
                .expect("returned EventLoop owner should seal");
        let tampered = receipt.canonical_json().replace("true", "false");
        assert_eq!(
            AppUiEventLoopShutdownReceipt::verify_integrity(&tampered, receipt.sha256()),
            Err(AppUiEventLoopShutdownReceiptError::HashMismatch)
        );

        let noncanonical = format!(" {}", receipt.canonical_json());
        let noncanonical_hash = event_loop_lower_sha256(noncanonical.as_bytes());
        assert_eq!(
            AppUiEventLoopShutdownReceipt::verify_integrity(&noncanonical, &noncanonical_hash),
            Err(AppUiEventLoopShutdownReceiptError::NonCanonicalEvidence)
        );

        let promoted = receipt.canonical_json().replace("unverified", "qualified");
        let promoted_hash = event_loop_lower_sha256(promoted.as_bytes());
        assert!(matches!(
            AppUiEventLoopShutdownReceipt::verify_integrity(&promoted, &promoted_hash),
            Err(AppUiEventLoopShutdownReceiptError::Serialization(_))
        ));
    }
}
