//! Sealed ownership evidence for one complete Window run.
//!
//! This Module keeps the Runtime/Host/GPU/native state product behind one
//! mutually-exclusive Interface. Validation and report Adapters consume the
//! same canonical receipt instead of rebuilding shutdown meaning independently.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::app::endurance_recovery::EnduranceRecoveryOperationReceipt;
use crate::app_ui::background_runtime::AppUiBackgroundRuntimeShutdownEvidence;
use crate::app_ui::host::{AppUiHostStartupShutdownEvidence, AppUiServiceShutdownEvidence};
use crate::app_ui::window::{
    AppUiActiveWindowGpuShutdownEvidence, AppUiPreActiveWindowShutdownEvidence,
};

const WINDOW_OUTER_RECEIPT_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_WINDOW_OUTER_RECEIPT_JSON_BYTES: usize = 128 * 1024;
const WINDOW_CLOSED_RECEIPT_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_WINDOW_CLOSED_LEAF_JSON_BYTES: usize = 128 * 1024;
const MAXIMUM_WINDOW_CLOSED_RECEIPT_JSON_BYTES: usize = 2 * 1024 * 1024;
const ACTIVE_EXIT_OUTCOME: &str = "active_exited";

/// Owner-free, mutually-exclusive closure evidence for one Window execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(super) enum AppUiWindowOuterShutdownEvidence {
    /// Runtime construction failed before Host startup was attempted.
    RuntimeStartupFailed {
        runtime: AppUiBackgroundRuntimeShutdownEvidence,
    },
    /// Host construction failed and returned the original App owner.
    HostStartupFailed {
        runtime: AppUiBackgroundRuntimeShutdownEvidence,
        host: AppUiHostStartupShutdownEvidence,
    },
    /// Native/GPU candidate construction failed before Host publication.
    PreActiveFailed {
        runtime: AppUiBackgroundRuntimeShutdownEvidence,
        host: AppUiServiceShutdownEvidence,
        native: AppUiPreActiveWindowShutdownEvidence,
    },
    /// A published active Window returned through the consuming close path.
    ActiveExited {
        runtime: AppUiBackgroundRuntimeShutdownEvidence,
        host: AppUiServiceShutdownEvidence,
        gpu: AppUiActiveWindowGpuShutdownEvidence,
        native: AppUiWindowNativeReturnEvidence,
    },
    /// Activated owners closed after Host publication itself panicked.
    ActivePublicationFailed {
        runtime: AppUiBackgroundRuntimeShutdownEvidence,
        host: AppUiServiceShutdownEvidence,
        gpu: AppUiActiveWindowGpuShutdownEvidence,
        native: AppUiWindowNativeReturnEvidence,
    },
}

/// Opaque handback that keeps failure shutdown evidence typed across Modules.
#[derive(Debug, Clone)]
pub struct AppUiWindowClosedEvidence {
    evidence: AppUiWindowOuterShutdownEvidence,
}

impl AppUiWindowClosedEvidence {
    pub(super) fn new(evidence: AppUiWindowOuterShutdownEvidence) -> Self {
        Self { evidence }
    }

    /// Whether every Window-owned authority represented by this outcome returned.
    pub fn all_owned_authority_released(&self) -> bool {
        self.evidence.all_owned_authority_released()
    }

    /// Seal the exact mutually-exclusive Window closure evidence for persistence.
    pub fn seal_receipt(&self) -> Result<AppUiWindowClosedReceipt, AppUiWindowClosedReceiptError> {
        AppUiWindowClosedReceipt::seal(&self.evidence)
    }
}

/// Stable variant identity for one returned Window closure outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppUiWindowClosedOutcome {
    /// Runtime construction failed before Host startup.
    RuntimeStartupFailed,
    /// Host construction failed before native Window construction.
    HostStartupFailed,
    /// Native/GPU candidate construction failed before Host publication.
    PreActiveFailed,
    /// An active Window returned through the normal owner-close transaction.
    ActiveExited,
    /// Activated owners returned after Host publication panicked.
    ActivePublicationFailed,
}

/// Canonical bounded receipt for exact Window closure evidence on failure paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUiWindowClosedReceipt {
    outcome: AppUiWindowClosedOutcome,
    canonical_json: String,
    sha256: String,
    all_owned_authority_released: bool,
}

impl AppUiWindowClosedReceipt {
    fn seal(
        evidence: &AppUiWindowOuterShutdownEvidence,
    ) -> Result<Self, AppUiWindowClosedReceiptError> {
        let projection = CanonicalWindowClosedReceipt {
            schema_version: WINDOW_CLOSED_RECEIPT_SCHEMA_VERSION,
            shutdown: CanonicalWindowClosedEvidence::from_evidence(evidence)?,
        };
        let outcome = projection.validate()?;
        let canonical_json = serde_json::to_string(&projection)
            .map_err(|error| AppUiWindowClosedReceiptError::Serialization(error.to_string()))?;
        if canonical_json.len() > MAXIMUM_WINDOW_CLOSED_RECEIPT_JSON_BYTES {
            return Err(AppUiWindowClosedReceiptError::TooLarge);
        }
        let sha256 = lower_sha256(canonical_json.as_bytes());
        Ok(Self {
            outcome,
            canonical_json,
            sha256,
            all_owned_authority_released: evidence.all_owned_authority_released(),
        })
    }

    /// Exact mutually-exclusive Window closure variant.
    pub const fn outcome(&self) -> AppUiWindowClosedOutcome {
        self.outcome
    }

    /// Whether the original typed evidence proved all represented owners returned.
    pub const fn all_owned_authority_released(&self) -> bool {
        self.all_owned_authority_released
    }

    /// Canonical UTF-8 JSON for durable embedding.
    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }

    /// SHA-256 over the exact bytes returned by [`Self::canonical_json`].
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Revalidate canonical shape and embedded byte integrity.
    ///
    /// Runtime, Host, and GPU semantic replay remains the responsibility of
    /// their owning Modules; this method does not promote integrity to physical
    /// qualification.
    pub fn verify_integrity(
        canonical_json: &str,
        sha256: &str,
    ) -> Result<AppUiWindowClosedOutcome, AppUiWindowClosedReceiptError> {
        if canonical_json.len() > MAXIMUM_WINDOW_CLOSED_RECEIPT_JSON_BYTES {
            return Err(AppUiWindowClosedReceiptError::TooLarge);
        }
        if lower_sha256(canonical_json.as_bytes()) != sha256 {
            return Err(AppUiWindowClosedReceiptError::HashMismatch);
        }
        let projection: CanonicalWindowClosedReceipt = serde_json::from_str(canonical_json)
            .map_err(|error| AppUiWindowClosedReceiptError::Serialization(error.to_string()))?;
        let outcome = projection.validate()?;
        let normalized = serde_json::to_string(&projection)
            .map_err(|error| AppUiWindowClosedReceiptError::Serialization(error.to_string()))?;
        if normalized != canonical_json {
            return Err(AppUiWindowClosedReceiptError::NonCanonicalEvidence);
        }
        Ok(outcome)
    }

    /// Physical native termination remains unqualified without OS evidence.
    pub const fn qualifies_physical_native_termination(&self) -> bool {
        false
    }
}

/// Stable Window-closure receipt sealing or replay rejection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppUiWindowClosedReceiptError {
    /// The canonical projection violates its schema or variant invariants.
    #[error("Window closure receipt evidence is invalid")]
    InvalidEvidence,
    /// The outer canonical JSON does not match its supplied digest.
    #[error("Window closure receipt hash does not match canonical JSON")]
    HashMismatch,
    /// An embedded raw receipt does not match its embedded digest.
    #[error("Window closure embedded receipt hash does not match raw JSON")]
    EmbeddedHashMismatch,
    /// Valid JSON was not encoded in the one canonical compact form.
    #[error("Window closure receipt JSON is not canonical")]
    NonCanonicalEvidence,
    /// Canonical encoding or decoding failed.
    #[error("Window closure receipt serialization failed: {0}")]
    Serialization(String),
    /// The bounded receipt or one of its leaves exceeded its schema limit.
    #[error("Window closure receipt exceeds its maximum canonical size")]
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalWindowClosedReceipt {
    schema_version: u32,
    shutdown: CanonicalWindowClosedEvidence,
}

impl CanonicalWindowClosedReceipt {
    fn validate(&self) -> Result<AppUiWindowClosedOutcome, AppUiWindowClosedReceiptError> {
        if self.schema_version != WINDOW_CLOSED_RECEIPT_SCHEMA_VERSION {
            return Err(AppUiWindowClosedReceiptError::InvalidEvidence);
        }
        self.shutdown.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum CanonicalWindowClosedEvidence {
    RuntimeStartupFailed {
        runtime: CanonicalWindowClosedLeaf,
    },
    HostStartupFailed {
        runtime: CanonicalWindowClosedLeaf,
        host: CanonicalWindowClosedLeaf,
    },
    PreActiveFailed {
        runtime: CanonicalWindowClosedLeaf,
        host: CanonicalWindowClosedLeaf,
        native: CanonicalWindowClosedLeaf,
    },
    ActiveExited {
        runtime: CanonicalWindowClosedLeaf,
        host: CanonicalWindowClosedLeaf,
        gpu: CanonicalWindowClosedLeaf,
        native: CanonicalWindowClosedLeaf,
    },
    ActivePublicationFailed {
        runtime: CanonicalWindowClosedLeaf,
        host: CanonicalWindowClosedLeaf,
        gpu: CanonicalWindowClosedLeaf,
        native: CanonicalWindowClosedLeaf,
    },
}

impl CanonicalWindowClosedEvidence {
    fn from_evidence(
        evidence: &AppUiWindowOuterShutdownEvidence,
    ) -> Result<Self, AppUiWindowClosedReceiptError> {
        Ok(match evidence {
            AppUiWindowOuterShutdownEvidence::RuntimeStartupFailed { runtime } => {
                Self::RuntimeStartupFailed { runtime: CanonicalWindowClosedLeaf::seal(runtime)? }
            }
            AppUiWindowOuterShutdownEvidence::HostStartupFailed { runtime, host } => {
                Self::HostStartupFailed {
                    runtime: CanonicalWindowClosedLeaf::seal(runtime)?,
                    host: CanonicalWindowClosedLeaf::seal(host)?,
                }
            }
            AppUiWindowOuterShutdownEvidence::PreActiveFailed { runtime, host, native } => {
                Self::PreActiveFailed {
                    runtime: CanonicalWindowClosedLeaf::seal(runtime)?,
                    host: CanonicalWindowClosedLeaf::seal(host)?,
                    native: CanonicalWindowClosedLeaf::seal(native)?,
                }
            }
            AppUiWindowOuterShutdownEvidence::ActiveExited { runtime, host, gpu, native } => {
                Self::ActiveExited {
                    runtime: CanonicalWindowClosedLeaf::seal(runtime)?,
                    host: CanonicalWindowClosedLeaf::seal(host)?,
                    gpu: CanonicalWindowClosedLeaf::seal(gpu)?,
                    native: CanonicalWindowClosedLeaf::seal(native)?,
                }
            }
            AppUiWindowOuterShutdownEvidence::ActivePublicationFailed {
                runtime,
                host,
                gpu,
                native,
            } => Self::ActivePublicationFailed {
                runtime: CanonicalWindowClosedLeaf::seal(runtime)?,
                host: CanonicalWindowClosedLeaf::seal(host)?,
                gpu: CanonicalWindowClosedLeaf::seal(gpu)?,
                native: CanonicalWindowClosedLeaf::seal(native)?,
            },
        })
    }

    fn validate(&self) -> Result<AppUiWindowClosedOutcome, AppUiWindowClosedReceiptError> {
        let (outcome, leaves, active_native) = match self {
            Self::RuntimeStartupFailed { runtime } => (
                AppUiWindowClosedOutcome::RuntimeStartupFailed,
                vec![runtime],
                None,
            ),
            Self::HostStartupFailed { runtime, host } => (
                AppUiWindowClosedOutcome::HostStartupFailed,
                vec![runtime, host],
                None,
            ),
            Self::PreActiveFailed { runtime, host, native } => (
                AppUiWindowClosedOutcome::PreActiveFailed,
                vec![runtime, host, native],
                None,
            ),
            Self::ActiveExited { runtime, host, gpu, native } => (
                AppUiWindowClosedOutcome::ActiveExited,
                vec![runtime, host, gpu, native],
                Some(native),
            ),
            Self::ActivePublicationFailed { runtime, host, gpu, native } => (
                AppUiWindowClosedOutcome::ActivePublicationFailed,
                vec![runtime, host, gpu, native],
                Some(native),
            ),
        };
        for leaf in leaves {
            leaf.validate()?;
        }
        if let Some(native) = active_native {
            native.validate_active_native_return()?;
        }
        Ok(outcome)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalWindowClosedLeaf {
    json: String,
    sha256: String,
}

impl CanonicalWindowClosedLeaf {
    fn seal(value: &impl Serialize) -> Result<Self, AppUiWindowClosedReceiptError> {
        let json = canonical_closed_leaf_json(value)?;
        if json.len() > MAXIMUM_WINDOW_CLOSED_LEAF_JSON_BYTES {
            return Err(AppUiWindowClosedReceiptError::TooLarge);
        }
        let sha256 = lower_sha256(json.as_bytes());
        Ok(Self { json, sha256 })
    }

    fn validate(&self) -> Result<(), AppUiWindowClosedReceiptError> {
        if self.json.len() > MAXIMUM_WINDOW_CLOSED_LEAF_JSON_BYTES {
            return Err(AppUiWindowClosedReceiptError::TooLarge);
        }
        if lower_sha256(self.json.as_bytes()) != self.sha256 {
            return Err(AppUiWindowClosedReceiptError::EmbeddedHashMismatch);
        }
        let value: serde_json::Value = serde_json::from_str(&self.json)
            .map_err(|error| AppUiWindowClosedReceiptError::Serialization(error.to_string()))?;
        let normalized = serde_json::to_string(&value)
            .map_err(|error| AppUiWindowClosedReceiptError::Serialization(error.to_string()))?;
        if normalized != self.json {
            return Err(AppUiWindowClosedReceiptError::NonCanonicalEvidence);
        }
        Ok(())
    }

    fn validate_active_native_return(&self) -> Result<(), AppUiWindowClosedReceiptError> {
        let native: CanonicalActiveNativeReturnEvidence = serde_json::from_str(&self.json)
            .map_err(|error| AppUiWindowClosedReceiptError::Serialization(error.to_string()))?;
        if !native.event_loop_borrow_returned
            || !native.window_owner_scope_exited
            || native.physical_native_termination != PhysicalNativeTermination::Unverified
        {
            return Err(AppUiWindowClosedReceiptError::InvalidEvidence);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalActiveNativeReturnEvidence {
    event_loop_borrow_returned: bool,
    window_owner_scope_exited: bool,
    physical_native_termination: PhysicalNativeTermination,
}

fn canonical_closed_leaf_json(
    value: &impl Serialize,
) -> Result<String, AppUiWindowClosedReceiptError> {
    let value = serde_json::to_value(value)
        .map_err(|error| AppUiWindowClosedReceiptError::Serialization(error.to_string()))?;
    serde_json::to_string(&value)
        .map_err(|error| AppUiWindowClosedReceiptError::Serialization(error.to_string()))
}

/// Evidence that can only be minted after the Window function returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct AppUiWindowNativeReturnEvidence {
    event_loop_borrow_returned: bool,
    window_owner_scope_exited: bool,
    physical_native_termination: PhysicalNativeTermination,
}

impl AppUiWindowNativeReturnEvidence {
    pub(super) const fn after_window_function_return() -> Self {
        Self {
            event_loop_borrow_returned: true,
            window_owner_scope_exited: true,
            physical_native_termination: PhysicalNativeTermination::Unverified,
        }
    }

    const fn qualifies_rust_authority_release(&self) -> bool {
        self.event_loop_borrow_returned && self.window_owner_scope_exited
    }
}

impl AppUiWindowOuterShutdownEvidence {
    pub(super) fn runtime_startup_failed(runtime: AppUiBackgroundRuntimeShutdownEvidence) -> Self {
        Self::RuntimeStartupFailed { runtime }
    }

    pub(super) fn host_startup_failed(
        runtime: AppUiBackgroundRuntimeShutdownEvidence,
        host: AppUiHostStartupShutdownEvidence,
    ) -> Self {
        Self::HostStartupFailed { runtime, host }
    }

    pub(super) fn pre_active_failed(
        runtime: AppUiBackgroundRuntimeShutdownEvidence,
        host: AppUiServiceShutdownEvidence,
        native: AppUiPreActiveWindowShutdownEvidence,
    ) -> Self {
        Self::PreActiveFailed { runtime, host, native }
    }

    pub(super) fn active_exited(
        runtime: AppUiBackgroundRuntimeShutdownEvidence,
        host: AppUiServiceShutdownEvidence,
        gpu: AppUiActiveWindowGpuShutdownEvidence,
        native: AppUiWindowNativeReturnEvidence,
    ) -> Self {
        Self::ActiveExited { runtime, host, gpu, native }
    }

    pub(super) fn active_publication_failed(
        runtime: AppUiBackgroundRuntimeShutdownEvidence,
        host: AppUiServiceShutdownEvidence,
        gpu: AppUiActiveWindowGpuShutdownEvidence,
        native: AppUiWindowNativeReturnEvidence,
    ) -> Self {
        Self::ActivePublicationFailed { runtime, host, gpu, native }
    }

    /// Whether every owner represented by this exact outcome returned cleanly.
    pub(super) fn all_owned_authority_released(&self) -> bool {
        match self {
            Self::RuntimeStartupFailed { runtime } => runtime.all_created_resources_released(),
            Self::HostStartupFailed { runtime, host } => {
                runtime.all_created_resources_released() && host.all_created_resources_released()
            }
            Self::PreActiveFailed { runtime, host, native } => {
                runtime.all_created_resources_released()
                    && host.all_resources_released()
                    && native.all_created_resources_released()
            }
            Self::ActiveExited { runtime, host, gpu, native }
            | Self::ActivePublicationFailed { runtime, host, gpu, native } => {
                runtime.all_created_resources_released()
                    && host.all_resources_released()
                    && gpu.qualifies_normal_runtime()
                    && native.qualifies_rust_authority_release()
            }
        }
    }

    fn active_parts(
        &self,
    ) -> Option<(
        &AppUiBackgroundRuntimeShutdownEvidence,
        &AppUiServiceShutdownEvidence,
        &AppUiActiveWindowGpuShutdownEvidence,
        &AppUiWindowNativeReturnEvidence,
    )> {
        match self {
            Self::ActiveExited { runtime, host, gpu, native } => Some((runtime, host, gpu, native)),
            Self::RuntimeStartupFailed { .. }
            | Self::HostStartupFailed { .. }
            | Self::PreActiveFailed { .. }
            | Self::ActivePublicationFailed { .. } => None,
        }
    }
}

/// Canonical durable evidence for one successful Window recovery run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUiWindowRunReceipt {
    recovery: EnduranceRecoveryOperationReceipt,
    shutdown: AppUiWindowOuterShutdownEvidence,
    canonical_json: String,
    sha256: String,
}

impl AppUiWindowRunReceipt {
    pub(super) fn seal_active(
        recovery: EnduranceRecoveryOperationReceipt,
        shutdown: AppUiWindowOuterShutdownEvidence,
    ) -> Result<Self, AppUiWindowRunReceiptError> {
        if !shutdown.all_owned_authority_released() {
            return Err(AppUiWindowRunReceiptError::IncompleteOwnedAuthority);
        }
        let Some((runtime, host, gpu, native)) = shutdown.active_parts() else {
            return Err(AppUiWindowRunReceiptError::NotActiveExit);
        };
        let runtime_json = canonical_leaf_json(runtime)?;
        let host_json = canonical_leaf_json(host)?;
        let gpu_json = canonical_leaf_json(gpu)?;
        let native_json = canonical_leaf_json(native)?;
        let projection = CanonicalActiveWindowRunEvidence {
            schema_version: WINDOW_OUTER_RECEIPT_SCHEMA_VERSION,
            outcome: ACTIVE_EXIT_OUTCOME.to_owned(),
            recovery_receipt_json: recovery.canonical_json().to_owned(),
            recovery_receipt_sha256: recovery.sha256().to_owned(),
            runtime_shutdown_sha256: lower_sha256(runtime_json.as_bytes()),
            runtime_shutdown_json: runtime_json,
            host_shutdown_sha256: lower_sha256(host_json.as_bytes()),
            host_shutdown_json: host_json,
            gpu_shutdown_sha256: lower_sha256(gpu_json.as_bytes()),
            gpu_shutdown_json: gpu_json,
            native_return_sha256: lower_sha256(native_json.as_bytes()),
            native_return_json: native_json,
        };
        projection.validate()?;
        let canonical_json = serde_json::to_string(&projection)
            .map_err(|error| AppUiWindowRunReceiptError::Serialization(error.to_string()))?;
        if canonical_json.len() > MAXIMUM_WINDOW_OUTER_RECEIPT_JSON_BYTES {
            return Err(AppUiWindowRunReceiptError::TooLarge);
        }
        let sha256 = lower_sha256(canonical_json.as_bytes());
        Ok(Self { recovery, shutdown, canonical_json, sha256 })
    }

    /// Existing sealed recovery operation nested by this Window-run receipt.
    pub fn recovery_receipt(&self) -> &EnduranceRecoveryOperationReceipt {
        &self.recovery
    }

    /// Consume the Window receipt and return its compatibility recovery receipt.
    pub fn into_recovery_receipt(self) -> EnduranceRecoveryOperationReceipt {
        self.recovery
    }

    /// Whether every Rust-owned authority represented by this run closed cleanly.
    pub(crate) fn all_owned_authority_released(&self) -> bool {
        self.shutdown.all_owned_authority_released()
    }

    /// Canonical UTF-8 JSON for durable publication and independent replay.
    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }

    /// SHA-256 over the exact bytes returned by [`Self::canonical_json`].
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Revalidate schema and byte integrity without trusting the enclosing report.
    pub fn verify_integrity(
        canonical_json: &str,
        sha256: &str,
    ) -> Result<(), AppUiWindowRunReceiptError> {
        if canonical_json.len() > MAXIMUM_WINDOW_OUTER_RECEIPT_JSON_BYTES {
            return Err(AppUiWindowRunReceiptError::TooLarge);
        }
        if lower_sha256(canonical_json.as_bytes()) != sha256 {
            return Err(AppUiWindowRunReceiptError::HashMismatch);
        }
        let projection: CanonicalActiveWindowRunEvidence = serde_json::from_str(canonical_json)
            .map_err(|error| AppUiWindowRunReceiptError::Serialization(error.to_string()))?;
        projection.validate()?;
        let normalized = serde_json::to_string(&projection)
            .map_err(|error| AppUiWindowRunReceiptError::Serialization(error.to_string()))?;
        if normalized != canonical_json {
            return Err(AppUiWindowRunReceiptError::NonCanonicalEvidence);
        }
        Ok(())
    }

    pub(crate) fn verify_recovery_binding(
        canonical_json: &str,
        sha256: &str,
        recovery: &EnduranceRecoveryOperationReceipt,
    ) -> Result<(), AppUiWindowRunReceiptError> {
        Self::verify_integrity(canonical_json, sha256)?;
        let projection: CanonicalActiveWindowRunEvidence = serde_json::from_str(canonical_json)
            .map_err(|error| AppUiWindowRunReceiptError::Serialization(error.to_string()))?;
        if projection.recovery_receipt_json != recovery.canonical_json()
            || projection.recovery_receipt_sha256 != recovery.sha256()
        {
            return Err(AppUiWindowRunReceiptError::RecoveryBindingMismatch);
        }
        Ok(())
    }

    /// Physical native termination remains unqualified without an OS/driver receipt.
    pub const fn qualifies_physical_native_termination(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalActiveWindowRunEvidence {
    schema_version: u32,
    outcome: String,
    recovery_receipt_json: String,
    recovery_receipt_sha256: String,
    runtime_shutdown_json: String,
    runtime_shutdown_sha256: String,
    host_shutdown_json: String,
    host_shutdown_sha256: String,
    gpu_shutdown_json: String,
    gpu_shutdown_sha256: String,
    native_return_json: String,
    native_return_sha256: String,
}

impl CanonicalActiveWindowRunEvidence {
    fn validate(&self) -> Result<(), AppUiWindowRunReceiptError> {
        if self.schema_version != WINDOW_OUTER_RECEIPT_SCHEMA_VERSION
            || self.outcome != ACTIVE_EXIT_OUTCOME
        {
            return Err(AppUiWindowRunReceiptError::InvalidEvidence);
        }
        EnduranceRecoveryOperationReceipt::parse_and_validate(
            &self.recovery_receipt_json,
            &self.recovery_receipt_sha256,
        )
        .map_err(|error| AppUiWindowRunReceiptError::Serialization(error.to_string()))?;
        for (json, expected_hash) in [
            (&self.runtime_shutdown_json, &self.runtime_shutdown_sha256),
            (&self.host_shutdown_json, &self.host_shutdown_sha256),
            (&self.gpu_shutdown_json, &self.gpu_shutdown_sha256),
            (&self.native_return_json, &self.native_return_sha256),
        ] {
            let value = serde_json::from_str::<serde_json::Value>(json)
                .map_err(|error| AppUiWindowRunReceiptError::Serialization(error.to_string()))?;
            if lower_sha256(json.as_bytes()) != *expected_hash {
                return Err(AppUiWindowRunReceiptError::EmbeddedHashMismatch);
            }
            let normalized = serde_json::to_string(&value)
                .map_err(|error| AppUiWindowRunReceiptError::Serialization(error.to_string()))?;
            if normalized != *json {
                return Err(AppUiWindowRunReceiptError::NonCanonicalEvidence);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PhysicalNativeTermination {
    Unverified,
}

/// Stable Window-run sealing or replay rejection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppUiWindowRunReceiptError {
    /// A successful receipt may only be sealed from an active exit outcome.
    #[error("Window run receipt did not contain an active exit")]
    NotActiveExit,
    /// A represented owner failed to return cleanly.
    #[error("Window run receipt contains incomplete owned authority")]
    IncompleteOwnedAuthority,
    /// The canonical projection violates its schema invariants.
    #[error("Window run receipt evidence is invalid")]
    InvalidEvidence,
    /// The outer canonical JSON does not match its supplied digest.
    #[error("Window run receipt hash does not match canonical JSON")]
    HashMismatch,
    /// An embedded raw receipt does not match its embedded digest.
    #[error("Window run embedded receipt hash does not match its raw JSON")]
    EmbeddedHashMismatch,
    /// The nested recovery receipt is not the operation being recorded.
    #[error("Window run receipt is bound to a different recovery operation")]
    RecoveryBindingMismatch,
    /// Outer JSON is valid but not its canonical compact encoding.
    #[error("Window run receipt JSON is not canonical")]
    NonCanonicalEvidence,
    /// Canonical encoding or decoding failed.
    #[error("Window run receipt serialization failed: {0}")]
    Serialization(String),
    /// The bounded receipt exceeds its schema limit.
    #[error("Window run receipt exceeds its maximum canonical size")]
    TooLarge,
}

fn canonical_leaf_json(value: &impl Serialize) -> Result<String, AppUiWindowRunReceiptError> {
    let value = serde_json::to_value(value)
        .map_err(|error| AppUiWindowRunReceiptError::Serialization(error.to_string()))?;
    serde_json::to_string(&value)
        .map_err(|error| AppUiWindowRunReceiptError::Serialization(error.to_string()))
}

fn lower_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
pub(crate) fn test_integrity_receipt_for_recovery(
    recovery: &EnduranceRecoveryOperationReceipt,
) -> (String, String) {
    let runtime = r#"{"supervisor":"terminated"}"#.to_owned();
    let host = r#"{"services":"returned"}"#.to_owned();
    let gpu = r#"{"retirement":"returned"}"#.to_owned();
    let native = r#"{"event_loop_borrow_returned":true,"window_owner_scope_exited":true,"physical_native_termination":"unverified"}"#.to_owned();
    let projection = CanonicalActiveWindowRunEvidence {
        schema_version: WINDOW_OUTER_RECEIPT_SCHEMA_VERSION,
        outcome: ACTIVE_EXIT_OUTCOME.to_owned(),
        recovery_receipt_json: recovery.canonical_json().to_owned(),
        recovery_receipt_sha256: recovery.sha256().to_owned(),
        runtime_shutdown_sha256: lower_sha256(runtime.as_bytes()),
        runtime_shutdown_json: runtime,
        host_shutdown_sha256: lower_sha256(host.as_bytes()),
        host_shutdown_json: host,
        gpu_shutdown_sha256: lower_sha256(gpu.as_bytes()),
        gpu_shutdown_json: gpu,
        native_return_sha256: lower_sha256(native.as_bytes()),
        native_return_json: native,
    };
    let json = serde_json::to_string(&projection).expect("test projection should serialize");
    let sha256 = lower_sha256(json.as_bytes());
    (json, sha256)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn fixture_projection() -> CanonicalActiveWindowRunEvidence {
        let recovery = EnduranceRecoveryOperationReceipt::surface_device_reopen(
            1,
            "surface.c1".to_owned(),
            SHA.to_owned(),
            1,
            2,
            3,
            4,
        )
        .expect("fixture recovery receipt");
        let runtime = r#"{"supervisor":"terminated"}"#.to_owned();
        let host = r#"{"preview":{"worker":"returned"}}"#.to_owned();
        let gpu = r#"{"retirement":"returned"}"#.to_owned();
        let native = r#"{"physical_native_termination":"unverified"}"#.to_owned();
        CanonicalActiveWindowRunEvidence {
            schema_version: WINDOW_OUTER_RECEIPT_SCHEMA_VERSION,
            outcome: ACTIVE_EXIT_OUTCOME.to_owned(),
            recovery_receipt_sha256: recovery.sha256().to_owned(),
            recovery_receipt_json: recovery.canonical_json().to_owned(),
            runtime_shutdown_sha256: lower_sha256(runtime.as_bytes()),
            runtime_shutdown_json: runtime,
            host_shutdown_sha256: lower_sha256(host.as_bytes()),
            host_shutdown_json: host,
            gpu_shutdown_sha256: lower_sha256(gpu.as_bytes()),
            gpu_shutdown_json: gpu,
            native_return_sha256: lower_sha256(native.as_bytes()),
            native_return_json: native,
        }
    }

    fn closed_leaf(json: &str) -> CanonicalWindowClosedLeaf {
        let value: serde_json::Value =
            serde_json::from_str(json).expect("closed fixture leaf should parse");
        let json = serde_json::to_string(&value).expect("closed fixture leaf should normalize");
        CanonicalWindowClosedLeaf {
            json: json.clone(),
            sha256: lower_sha256(json.as_bytes()),
        }
    }

    fn closed_projection(shutdown: CanonicalWindowClosedEvidence) -> CanonicalWindowClosedReceipt {
        CanonicalWindowClosedReceipt {
            schema_version: WINDOW_CLOSED_RECEIPT_SCHEMA_VERSION,
            shutdown,
        }
    }

    fn verify_closed_projection(
        projection: &CanonicalWindowClosedReceipt,
    ) -> Result<AppUiWindowClosedOutcome, AppUiWindowClosedReceiptError> {
        let json = serde_json::to_string(projection).expect("closed fixture should serialize");
        let hash = lower_sha256(json.as_bytes());
        AppUiWindowClosedReceipt::verify_integrity(&json, &hash)
    }

    #[test]
    fn closed_receipt_preserves_all_mutually_exclusive_outcomes() {
        let runtime = || closed_leaf(r#"{"supervisor":"terminated"}"#);
        let host = || closed_leaf(r#"{"preview":"returned"}"#);
        let gpu = || closed_leaf(r#"{"retirement":"returned"}"#);
        let pre_active = || closed_leaf(r#"{"last_stage":"surface_created"}"#);
        let active_native = || {
            closed_leaf(
                r#"{"event_loop_borrow_returned":true,"window_owner_scope_exited":true,"physical_native_termination":"unverified"}"#,
            )
        };
        let cases = [
            (
                closed_projection(CanonicalWindowClosedEvidence::RuntimeStartupFailed {
                    runtime: runtime(),
                }),
                AppUiWindowClosedOutcome::RuntimeStartupFailed,
            ),
            (
                closed_projection(CanonicalWindowClosedEvidence::HostStartupFailed {
                    runtime: runtime(),
                    host: host(),
                }),
                AppUiWindowClosedOutcome::HostStartupFailed,
            ),
            (
                closed_projection(CanonicalWindowClosedEvidence::PreActiveFailed {
                    runtime: runtime(),
                    host: host(),
                    native: pre_active(),
                }),
                AppUiWindowClosedOutcome::PreActiveFailed,
            ),
            (
                closed_projection(CanonicalWindowClosedEvidence::ActiveExited {
                    runtime: runtime(),
                    host: host(),
                    gpu: gpu(),
                    native: active_native(),
                }),
                AppUiWindowClosedOutcome::ActiveExited,
            ),
            (
                closed_projection(CanonicalWindowClosedEvidence::ActivePublicationFailed {
                    runtime: runtime(),
                    host: host(),
                    gpu: gpu(),
                    native: active_native(),
                }),
                AppUiWindowClosedOutcome::ActivePublicationFailed,
            ),
        ];

        for (projection, expected) in cases {
            assert_eq!(verify_closed_projection(&projection), Ok(expected));
        }
    }

    #[test]
    fn closed_receipt_rejects_nested_tamper_and_noncanonical_leaf() {
        let mut projection =
            closed_projection(CanonicalWindowClosedEvidence::RuntimeStartupFailed {
                runtime: closed_leaf(r#"{"supervisor":"terminated"}"#),
            });
        if let CanonicalWindowClosedEvidence::RuntimeStartupFailed { runtime } =
            &mut projection.shutdown
        {
            runtime.json = r#"{"supervisor":"timed_out"}"#.to_owned();
        } else {
            unreachable!("fixture outcome")
        }
        assert_eq!(
            verify_closed_projection(&projection),
            Err(AppUiWindowClosedReceiptError::EmbeddedHashMismatch)
        );

        if let CanonicalWindowClosedEvidence::RuntimeStartupFailed { runtime } =
            &mut projection.shutdown
        {
            runtime.json = r#" {"supervisor":"terminated"}"#.to_owned();
            runtime.sha256 = lower_sha256(runtime.json.as_bytes());
        } else {
            unreachable!("fixture outcome")
        }
        assert_eq!(
            verify_closed_projection(&projection),
            Err(AppUiWindowClosedReceiptError::NonCanonicalEvidence)
        );
    }

    #[test]
    fn closed_receipt_rejects_native_physical_promotion() {
        let native_json = r#"{"event_loop_borrow_returned":true,"window_owner_scope_exited":true,"physical_native_termination":"qualified"}"#;
        let projection = closed_projection(CanonicalWindowClosedEvidence::ActiveExited {
            runtime: closed_leaf(r#"{"supervisor":"terminated"}"#),
            host: closed_leaf(r#"{"preview":"returned"}"#),
            gpu: closed_leaf(r#"{"retirement":"returned"}"#),
            native: closed_leaf(native_json),
        });
        assert!(matches!(
            verify_closed_projection(&projection),
            Err(AppUiWindowClosedReceiptError::Serialization(_))
        ));
    }

    #[test]
    fn canonical_projection_round_trips_and_rejects_outer_tamper() {
        let projection = fixture_projection();
        let json = serde_json::to_string(&projection).expect("fixture should serialize");
        let hash = lower_sha256(json.as_bytes());
        assert_eq!(
            AppUiWindowRunReceipt::verify_integrity(&json, &hash),
            Ok(())
        );

        let tampered = json.replace("active_exited", "active_failed");
        assert_eq!(
            AppUiWindowRunReceipt::verify_integrity(&tampered, &hash),
            Err(AppUiWindowRunReceiptError::HashMismatch)
        );
    }

    #[test]
    fn recomputed_outer_hash_cannot_hide_embedded_receipt_tamper() {
        let mut projection = fixture_projection();
        projection.runtime_shutdown_json = r#"{"supervisor":"timed_out"}"#.to_owned();
        let json = serde_json::to_string(&projection).expect("fixture should serialize");
        let hash = lower_sha256(json.as_bytes());

        assert_eq!(
            AppUiWindowRunReceipt::verify_integrity(&json, &hash),
            Err(AppUiWindowRunReceiptError::EmbeddedHashMismatch)
        );
    }

    #[test]
    fn valid_but_noncanonical_outer_json_is_rejected() {
        let projection = fixture_projection();
        let compact = serde_json::to_string(&projection).expect("fixture should serialize");
        let json = format!(" {compact}");
        let hash = lower_sha256(json.as_bytes());

        assert_eq!(
            AppUiWindowRunReceipt::verify_integrity(&json, &hash),
            Err(AppUiWindowRunReceiptError::NonCanonicalEvidence)
        );
    }

    #[test]
    fn native_leaf_tamper_without_embedded_hash_update_is_rejected() {
        let mut projection = fixture_projection();
        projection.native_return_json = r#"{"physical_native_termination":"qualified"}"#.to_owned();
        let json = serde_json::to_string(&projection).expect("fixture should serialize");
        let hash = lower_sha256(json.as_bytes());

        assert_eq!(
            AppUiWindowRunReceipt::verify_integrity(&json, &hash),
            Err(AppUiWindowRunReceiptError::EmbeddedHashMismatch)
        );
    }

    #[test]
    fn outer_receipt_is_bound_to_the_recorded_recovery_operation() {
        let recovery = EnduranceRecoveryOperationReceipt::surface_device_reopen(
            1,
            "surface.c1".to_owned(),
            SHA.to_owned(),
            1,
            2,
            3,
            4,
        )
        .expect("fixture recovery receipt");
        let other = EnduranceRecoveryOperationReceipt::surface_device_reopen(
            2,
            "surface.c2".to_owned(),
            SHA.to_owned(),
            5,
            6,
            7,
            8,
        )
        .expect("other recovery receipt");
        let mut projection = fixture_projection();
        projection.recovery_receipt_json = recovery.canonical_json().to_owned();
        projection.recovery_receipt_sha256 = recovery.sha256().to_owned();
        let json = serde_json::to_string(&projection).expect("fixture should serialize");
        let hash = lower_sha256(json.as_bytes());

        assert_eq!(
            AppUiWindowRunReceipt::verify_recovery_binding(&json, &hash, &recovery),
            Ok(())
        );
        assert_eq!(
            AppUiWindowRunReceipt::verify_recovery_binding(&json, &hash, &other),
            Err(AppUiWindowRunReceiptError::RecoveryBindingMismatch)
        );
    }
}
