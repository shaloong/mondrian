//! Canonical terminal evidence for one Surface/Device validation batch.
//!
//! This Module is the sole composition seam for successful and failed batch
//! outcomes. It retains each owning Module's sealed receipt rather than
//! rebuilding App, EventLoop, Window, or recovery shutdown meaning.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::app::{AppEnduranceShutdownReceipt, AppEnduranceShutdownReceiptError};
use crate::app_ui::event_loop_owner::{
    AppUiEventLoopConstructionFailureKind, AppUiEventLoopShutdownReceipt,
    AppUiEventLoopShutdownReceiptError,
};
use crate::app_ui::window::{
    AppUiSurfaceDeviceReopenValidationBatch, AppUiSurfaceDeviceReopenValidationError,
    AppUiSurfaceDeviceReopenValidationFailureKind,
};
use crate::app_ui::window_outer_receipt::{
    AppUiWindowClosedReceipt, AppUiWindowClosedReceiptError, AppUiWindowRunReceipt,
    AppUiWindowRunReceiptError,
};

const SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_SURFACE_REOPEN_BATCH_OPERATIONS: usize = 24;
const MAXIMUM_SURFACE_REOPEN_DIAGNOSTIC_BYTES: usize = 64 * 1024;
const MAXIMUM_SURFACE_REOPEN_BATCH_RECEIPT_JSON_BYTES: usize = 24 * 1024 * 1024;

/// Canonical bounded receipt for one complete batch success or failure outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUiSurfaceDeviceReopenValidationReceipt {
    canonical_json: String,
    sha256: String,
    qualifying: bool,
}

impl AppUiSurfaceDeviceReopenValidationReceipt {
    /// Seal a successful batch without projecting away any owner receipt.
    pub fn seal_success(
        batch: &AppUiSurfaceDeviceReopenValidationBatch,
    ) -> Result<Self, AppUiSurfaceDeviceReopenValidationReceiptError> {
        if !batch.all_returned_authority_released()
            || batch.receipts().iter().any(|receipt| !receipt.all_owned_authority_released())
        {
            return Err(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence);
        }
        let event_loop = batch.event_loop_shutdown_receipt().map_err(embedded_event_loop_error)?;
        let app =
            AppEnduranceShutdownReceipt::seal(batch.app_shutdown()).map_err(embedded_app_error)?;
        let projection = CanonicalSurfaceReopenBatchReceipt {
            schema_version: SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION,
            submitted_request_count: count_to_u64(batch.submitted_request_count())?,
            completed_window_receipts: batch
                .receipts()
                .iter()
                .map(CanonicalEmbeddedReceipt::from_window)
                .collect(),
            app_shutdown: CanonicalEmbeddedReceipt::from_app(&app),
            cleanup_diagnostic: None,
            outcome: CanonicalSurfaceReopenBatchOutcome::Success {
                event_loop_shutdown: CanonicalEmbeddedReceipt::from_event_loop(&event_loop),
            },
        };
        Self::seal_projection(projection)
    }

    /// Seal a typed failure while preserving its primary and cleanup evidence.
    pub fn seal_failure(
        error: &AppUiSurfaceDeviceReopenValidationError,
    ) -> Result<Self, AppUiSurfaceDeviceReopenValidationReceiptError> {
        let outcome = match error.kind() {
            AppUiSurfaceDeviceReopenValidationFailureKind::EmptyBatch => {
                CanonicalSurfaceReopenBatchOutcome::RequestRejected {
                    kind: CanonicalRequestRejectionKind::EmptyBatch,
                    primary_diagnostic: error.primary_diagnostic().to_owned(),
                }
            }
            AppUiSurfaceDeviceReopenValidationFailureKind::TooManyOperations => {
                CanonicalSurfaceReopenBatchOutcome::RequestRejected {
                    kind: CanonicalRequestRejectionKind::TooManyOperations,
                    primary_diagnostic: error.primary_diagnostic().to_owned(),
                }
            }
            AppUiSurfaceDeviceReopenValidationFailureKind::InvalidOrReplayedIdentity => {
                CanonicalSurfaceReopenBatchOutcome::RequestRejected {
                    kind: CanonicalRequestRejectionKind::InvalidOrReplayedIdentity,
                    primary_diagnostic: error.primary_diagnostic().to_owned(),
                }
            }
            AppUiSurfaceDeviceReopenValidationFailureKind::ZeroTimeout => {
                CanonicalSurfaceReopenBatchOutcome::RequestRejected {
                    kind: CanonicalRequestRejectionKind::ZeroTimeout,
                    primary_diagnostic: error.primary_diagnostic().to_owned(),
                }
            }
            AppUiSurfaceDeviceReopenValidationFailureKind::DeadlineOverflow => {
                CanonicalSurfaceReopenBatchOutcome::RequestRejected {
                    kind: CanonicalRequestRejectionKind::DeadlineOverflow,
                    primary_diagnostic: error.primary_diagnostic().to_owned(),
                }
            }
            AppUiSurfaceDeviceReopenValidationFailureKind::EventLoopConstruction(kind) => {
                CanonicalSurfaceReopenBatchOutcome::EventLoopConstructionFailed {
                    kind: CanonicalEventLoopConstructionFailureKind::from(kind),
                    primary_diagnostic: error.primary_diagnostic().to_owned(),
                }
            }
            AppUiSurfaceDeviceReopenValidationFailureKind::OperationDeadlineOverflow => {
                CanonicalSurfaceReopenBatchOutcome::OperationAdmissionFailed {
                    cycle_index: required_failed_cycle(error)?,
                    operation_id: required_failed_operation(error)?.to_owned(),
                    primary_diagnostic: error.primary_diagnostic().to_owned(),
                    event_loop_shutdown: required_event_loop_receipt(error)?,
                }
            }
            AppUiSurfaceDeviceReopenValidationFailureKind::WindowOperation => {
                CanonicalSurfaceReopenBatchOutcome::WindowOperationFailed {
                    cycle_index: required_failed_cycle(error)?,
                    operation_id: required_failed_operation(error)?.to_owned(),
                    primary_diagnostic: error.primary_diagnostic().to_owned(),
                    event_loop_shutdown: required_event_loop_receipt(error)?,
                    window_shutdown: match error
                        .window_shutdown_receipt()
                        .map_err(embedded_window_closed_error)?
                    {
                        Some(receipt) => CanonicalReturnedWindowShutdown::Returned {
                            receipt: CanonicalEmbeddedReceipt::from_window_closed(&receipt),
                        },
                        None => CanonicalReturnedWindowShutdown::Missing,
                    },
                }
            }
            AppUiSurfaceDeviceReopenValidationFailureKind::AppShutdownIncomplete => {
                CanonicalSurfaceReopenBatchOutcome::AppShutdownIncomplete {
                    primary_diagnostic: error.primary_diagnostic().to_owned(),
                    event_loop_shutdown: required_event_loop_receipt(error)?,
                }
            }
        };
        let app =
            AppEnduranceShutdownReceipt::seal(error.app_shutdown()).map_err(embedded_app_error)?;
        let projection = CanonicalSurfaceReopenBatchReceipt {
            schema_version: SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION,
            submitted_request_count: count_to_u64(error.submitted_request_count())?,
            completed_window_receipts: error
                .completed_receipts()
                .iter()
                .map(CanonicalEmbeddedReceipt::from_window)
                .collect(),
            app_shutdown: CanonicalEmbeddedReceipt::from_app(&app),
            cleanup_diagnostic: error.cleanup_diagnostic().map(str::to_owned),
            outcome,
        };
        Self::seal_projection(projection)
    }

    fn seal_projection(
        projection: CanonicalSurfaceReopenBatchReceipt,
    ) -> Result<Self, AppUiSurfaceDeviceReopenValidationReceiptError> {
        let qualifying = projection.validate()?;
        let canonical_json = serde_json::to_string(&projection).map_err(|error| {
            AppUiSurfaceDeviceReopenValidationReceiptError::Serialization(error.to_string())
        })?;
        if canonical_json.len() > MAXIMUM_SURFACE_REOPEN_BATCH_RECEIPT_JSON_BYTES {
            return Err(AppUiSurfaceDeviceReopenValidationReceiptError::TooLarge);
        }
        let sha256 = batch_lower_sha256(canonical_json.as_bytes());
        Ok(Self { canonical_json, sha256, qualifying })
    }

    /// Canonical UTF-8 JSON for durable publication.
    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }

    /// SHA-256 over the exact bytes returned by [`Self::canonical_json`].
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Whether replay proved a complete successful in-process owner handback.
    pub const fn qualifying(&self) -> bool {
        self.qualifying
    }

    /// Replay the full receipt tree and recompute success qualification.
    pub fn verify_integrity(
        canonical_json: &str,
        sha256: &str,
    ) -> Result<bool, AppUiSurfaceDeviceReopenValidationReceiptError> {
        if canonical_json.len() > MAXIMUM_SURFACE_REOPEN_BATCH_RECEIPT_JSON_BYTES {
            return Err(AppUiSurfaceDeviceReopenValidationReceiptError::TooLarge);
        }
        if batch_lower_sha256(canonical_json.as_bytes()) != sha256 {
            return Err(AppUiSurfaceDeviceReopenValidationReceiptError::HashMismatch);
        }
        let projection: CanonicalSurfaceReopenBatchReceipt = serde_json::from_str(canonical_json)
            .map_err(|error| {
            AppUiSurfaceDeviceReopenValidationReceiptError::Serialization(error.to_string())
        })?;
        let qualifying = projection.validate()?;
        let normalized = serde_json::to_string(&projection).map_err(|error| {
            AppUiSurfaceDeviceReopenValidationReceiptError::Serialization(error.to_string())
        })?;
        if normalized != canonical_json {
            return Err(AppUiSurfaceDeviceReopenValidationReceiptError::NonCanonicalEvidence);
        }
        Ok(qualifying)
    }
}

/// Stable batch receipt sealing or replay rejection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppUiSurfaceDeviceReopenValidationReceiptError {
    /// The outcome violates request, ordering, or mutually-exclusive shape rules.
    #[error("Surface validation batch receipt evidence is invalid")]
    InvalidEvidence,
    /// The outer canonical JSON does not match its supplied digest.
    #[error("Surface validation batch receipt hash does not match canonical JSON")]
    HashMismatch,
    /// A nested owning-Module receipt was rejected.
    #[error("Surface validation batch embedded receipt is invalid: {0}")]
    EmbeddedEvidence(String),
    /// Valid JSON was not encoded in the one canonical compact form.
    #[error("Surface validation batch receipt JSON is not canonical")]
    NonCanonicalEvidence,
    /// Canonical encoding or decoding failed.
    #[error("Surface validation batch receipt serialization failed: {0}")]
    Serialization(String),
    /// The bounded receipt exceeded its schema limit.
    #[error("Surface validation batch receipt exceeds its maximum canonical size")]
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalSurfaceReopenBatchReceipt {
    schema_version: u32,
    submitted_request_count: u64,
    completed_window_receipts: Vec<CanonicalEmbeddedReceipt>,
    app_shutdown: CanonicalEmbeddedReceipt,
    cleanup_diagnostic: Option<String>,
    outcome: CanonicalSurfaceReopenBatchOutcome,
}

impl CanonicalSurfaceReopenBatchReceipt {
    fn validate(&self) -> Result<bool, AppUiSurfaceDeviceReopenValidationReceiptError> {
        if self.schema_version != SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION
            || self.completed_window_receipts.len() > MAXIMUM_SURFACE_REOPEN_BATCH_OPERATIONS
            || self.cleanup_diagnostic.as_deref().is_some_and(|value| !valid_diagnostic(value))
        {
            return Err(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence);
        }
        let mut completed_cycles = BTreeSet::new();
        let mut completed_operations = BTreeSet::new();
        for receipt in &self.completed_window_receipts {
            let (cycle_index, operation_id) = receipt.verify_window()?;
            if !completed_cycles.insert(cycle_index) || !completed_operations.insert(operation_id) {
                return Err(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence);
            }
        }
        let app_clean = self.app_shutdown.verify_app()?;
        let completed_count = u64::try_from(self.completed_window_receipts.len())
            .map_err(|_| AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence)?;
        match &self.outcome {
            CanonicalSurfaceReopenBatchOutcome::Success { event_loop_shutdown } => {
                event_loop_shutdown.verify_event_loop()?;
                if !bounded_nonzero_request_count(self.submitted_request_count)
                    || completed_count != self.submitted_request_count
                    || self.cleanup_diagnostic.is_some()
                    || !app_clean
                {
                    return Err(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence);
                }
                Ok(true)
            }
            CanonicalSurfaceReopenBatchOutcome::RequestRejected { kind, primary_diagnostic } => {
                if !valid_diagnostic(primary_diagnostic)
                    || completed_count != 0
                    || !kind.accepts_count(self.submitted_request_count)
                {
                    return Err(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence);
                }
                validate_failure_app_cleanup(app_clean, self.cleanup_diagnostic.as_deref())?;
                Ok(false)
            }
            CanonicalSurfaceReopenBatchOutcome::EventLoopConstructionFailed {
                primary_diagnostic,
                ..
            } => {
                if !valid_diagnostic(primary_diagnostic)
                    || completed_count != 0
                    || !bounded_nonzero_request_count(self.submitted_request_count)
                {
                    return Err(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence);
                }
                validate_failure_app_cleanup(app_clean, self.cleanup_diagnostic.as_deref())?;
                Ok(false)
            }
            CanonicalSurfaceReopenBatchOutcome::OperationAdmissionFailed {
                cycle_index,
                operation_id,
                primary_diagnostic,
                event_loop_shutdown,
            } => {
                event_loop_shutdown.verify_event_loop()?;
                validate_started_failure(
                    self.submitted_request_count,
                    completed_count,
                    *cycle_index,
                    operation_id,
                    primary_diagnostic,
                    &completed_cycles,
                    &completed_operations,
                )?;
                validate_failure_app_cleanup(app_clean, self.cleanup_diagnostic.as_deref())?;
                Ok(false)
            }
            CanonicalSurfaceReopenBatchOutcome::WindowOperationFailed {
                cycle_index,
                operation_id,
                primary_diagnostic,
                event_loop_shutdown,
                window_shutdown,
            } => {
                event_loop_shutdown.verify_event_loop()?;
                window_shutdown.validate()?;
                validate_started_failure(
                    self.submitted_request_count,
                    completed_count,
                    *cycle_index,
                    operation_id,
                    primary_diagnostic,
                    &completed_cycles,
                    &completed_operations,
                )?;
                validate_failure_app_cleanup(app_clean, self.cleanup_diagnostic.as_deref())?;
                Ok(false)
            }
            CanonicalSurfaceReopenBatchOutcome::AppShutdownIncomplete {
                primary_diagnostic,
                event_loop_shutdown,
            } => {
                event_loop_shutdown.verify_event_loop()?;
                if !valid_diagnostic(primary_diagnostic)
                    || !bounded_nonzero_request_count(self.submitted_request_count)
                    || completed_count != self.submitted_request_count
                    || self.cleanup_diagnostic.is_some()
                {
                    return Err(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence);
                }
                Ok(false)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum CanonicalSurfaceReopenBatchOutcome {
    Success {
        event_loop_shutdown: CanonicalEmbeddedReceipt,
    },
    RequestRejected {
        kind: CanonicalRequestRejectionKind,
        primary_diagnostic: String,
    },
    EventLoopConstructionFailed {
        kind: CanonicalEventLoopConstructionFailureKind,
        primary_diagnostic: String,
    },
    OperationAdmissionFailed {
        cycle_index: u32,
        operation_id: String,
        primary_diagnostic: String,
        event_loop_shutdown: CanonicalEmbeddedReceipt,
    },
    WindowOperationFailed {
        cycle_index: u32,
        operation_id: String,
        primary_diagnostic: String,
        event_loop_shutdown: CanonicalEmbeddedReceipt,
        window_shutdown: CanonicalReturnedWindowShutdown,
    },
    AppShutdownIncomplete {
        primary_diagnostic: String,
        event_loop_shutdown: CanonicalEmbeddedReceipt,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CanonicalRequestRejectionKind {
    EmptyBatch,
    TooManyOperations,
    InvalidOrReplayedIdentity,
    ZeroTimeout,
    DeadlineOverflow,
}

impl CanonicalRequestRejectionKind {
    fn accepts_count(self, count: u64) -> bool {
        match self {
            Self::EmptyBatch => count == 0,
            Self::TooManyOperations => count > MAXIMUM_SURFACE_REOPEN_BATCH_OPERATIONS as u64,
            Self::InvalidOrReplayedIdentity | Self::ZeroTimeout | Self::DeadlineOverflow => {
                bounded_nonzero_request_count(count)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CanonicalEventLoopConstructionFailureKind {
    NotSupported,
    OperatingSystem,
    RecreationAttempt,
    ExitFailure { status: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum CanonicalReturnedWindowShutdown {
    Missing,
    Returned { receipt: CanonicalEmbeddedReceipt },
}

impl CanonicalReturnedWindowShutdown {
    fn validate(&self) -> Result<(), AppUiSurfaceDeviceReopenValidationReceiptError> {
        match self {
            Self::Missing => Ok(()),
            Self::Returned { receipt } => receipt.verify_window_closed(),
        }
    }
}

impl From<AppUiEventLoopConstructionFailureKind> for CanonicalEventLoopConstructionFailureKind {
    fn from(kind: AppUiEventLoopConstructionFailureKind) -> Self {
        match kind {
            AppUiEventLoopConstructionFailureKind::NotSupported => Self::NotSupported,
            AppUiEventLoopConstructionFailureKind::OperatingSystem => Self::OperatingSystem,
            AppUiEventLoopConstructionFailureKind::RecreationAttempt => Self::RecreationAttempt,
            AppUiEventLoopConstructionFailureKind::ExitFailure(status) => {
                Self::ExitFailure { status }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalEmbeddedReceipt {
    json: String,
    sha256: String,
}

impl CanonicalEmbeddedReceipt {
    fn new(json: &str, sha256: &str) -> Self {
        Self { json: json.to_owned(), sha256: sha256.to_owned() }
    }

    fn from_window(receipt: &AppUiWindowRunReceipt) -> Self {
        Self::new(receipt.canonical_json(), receipt.sha256())
    }

    fn from_window_closed(receipt: &AppUiWindowClosedReceipt) -> Self {
        Self::new(receipt.canonical_json(), receipt.sha256())
    }

    fn from_event_loop(receipt: &AppUiEventLoopShutdownReceipt) -> Self {
        Self::new(receipt.canonical_json(), receipt.sha256())
    }

    fn from_app(receipt: &AppEnduranceShutdownReceipt) -> Self {
        Self::new(receipt.canonical_json(), receipt.sha256())
    }

    fn verify_window(
        &self,
    ) -> Result<(u32, String), AppUiSurfaceDeviceReopenValidationReceiptError> {
        AppUiWindowRunReceipt::verify_surface_identity(&self.json, &self.sha256)
            .map_err(embedded_window_error)
    }

    fn verify_window_closed(&self) -> Result<(), AppUiSurfaceDeviceReopenValidationReceiptError> {
        AppUiWindowClosedReceipt::verify_integrity(&self.json, &self.sha256)
            .map(|_| ())
            .map_err(embedded_window_closed_error)
    }

    fn verify_event_loop(&self) -> Result<(), AppUiSurfaceDeviceReopenValidationReceiptError> {
        AppUiEventLoopShutdownReceipt::verify_integrity(&self.json, &self.sha256)
            .map_err(embedded_event_loop_error)
    }

    fn verify_app(&self) -> Result<bool, AppUiSurfaceDeviceReopenValidationReceiptError> {
        AppEnduranceShutdownReceipt::verify_integrity(&self.json, &self.sha256)
            .map_err(embedded_app_error)
    }
}

fn required_failed_cycle(
    error: &AppUiSurfaceDeviceReopenValidationError,
) -> Result<u32, AppUiSurfaceDeviceReopenValidationReceiptError> {
    error
        .failed_cycle_index()
        .ok_or(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence)
}

fn required_failed_operation(
    error: &AppUiSurfaceDeviceReopenValidationError,
) -> Result<&str, AppUiSurfaceDeviceReopenValidationReceiptError> {
    error
        .failed_operation_id()
        .ok_or(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence)
}

fn required_event_loop_receipt(
    error: &AppUiSurfaceDeviceReopenValidationError,
) -> Result<CanonicalEmbeddedReceipt, AppUiSurfaceDeviceReopenValidationReceiptError> {
    let receipt = error
        .event_loop_shutdown_receipt()
        .map_err(embedded_event_loop_error)?
        .ok_or(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence)?;
    Ok(CanonicalEmbeddedReceipt::from_event_loop(&receipt))
}

fn validate_started_failure(
    submitted_count: u64,
    completed_count: u64,
    cycle_index: u32,
    operation_id: &str,
    primary_diagnostic: &str,
    completed_cycles: &BTreeSet<u32>,
    completed_operations: &BTreeSet<String>,
) -> Result<(), AppUiSurfaceDeviceReopenValidationReceiptError> {
    if !bounded_nonzero_request_count(submitted_count)
        || completed_count >= submitted_count
        || !valid_operation_id(operation_id)
        || !valid_diagnostic(primary_diagnostic)
        || completed_cycles.contains(&cycle_index)
        || completed_operations.contains(operation_id)
    {
        return Err(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence);
    }
    Ok(())
}

fn validate_failure_app_cleanup(
    app_clean: bool,
    cleanup_diagnostic: Option<&str>,
) -> Result<(), AppUiSurfaceDeviceReopenValidationReceiptError> {
    if !app_clean && cleanup_diagnostic.is_none() {
        return Err(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence);
    }
    Ok(())
}

fn valid_operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_diagnostic(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAXIMUM_SURFACE_REOPEN_DIAGNOSTIC_BYTES
}

fn bounded_nonzero_request_count(value: u64) -> bool {
    (1..=MAXIMUM_SURFACE_REOPEN_BATCH_OPERATIONS as u64).contains(&value)
}

fn count_to_u64(value: usize) -> Result<u64, AppUiSurfaceDeviceReopenValidationReceiptError> {
    u64::try_from(value)
        .map_err(|_| AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence)
}

fn embedded_window_error(
    error: AppUiWindowRunReceiptError,
) -> AppUiSurfaceDeviceReopenValidationReceiptError {
    AppUiSurfaceDeviceReopenValidationReceiptError::EmbeddedEvidence(error.to_string())
}

fn embedded_window_closed_error(
    error: AppUiWindowClosedReceiptError,
) -> AppUiSurfaceDeviceReopenValidationReceiptError {
    AppUiSurfaceDeviceReopenValidationReceiptError::EmbeddedEvidence(error.to_string())
}

fn embedded_event_loop_error(
    error: AppUiEventLoopShutdownReceiptError,
) -> AppUiSurfaceDeviceReopenValidationReceiptError {
    AppUiSurfaceDeviceReopenValidationReceiptError::EmbeddedEvidence(error.to_string())
}

fn embedded_app_error(
    error: AppEnduranceShutdownReceiptError,
) -> AppUiSurfaceDeviceReopenValidationReceiptError {
    AppUiSurfaceDeviceReopenValidationReceiptError::EmbeddedEvidence(error.to_string())
}

fn batch_lower_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::app::endurance_recovery::EnduranceRecoveryOperationReceipt;
    use crate::app::AppState;
    use crate::app_ui::event_loop_owner::AppUiEventLoopShutdownEvidence;
    use crate::app_ui::window::run_app_ui_surface_device_reopen_validation_batch;
    use crate::app_ui::window_outer_receipt::test_integrity_receipt_for_recovery;

    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn app_leaf() -> CanonicalEmbeddedReceipt {
        let evidence = AppState::new().shutdown_for_endurance(
            Instant::now()
                .checked_add(Duration::from_secs(10))
                .expect("App shutdown deadline"),
        );
        let receipt =
            AppEnduranceShutdownReceipt::seal(&evidence).expect("App receipt should seal");
        CanonicalEmbeddedReceipt::from_app(&receipt)
    }

    fn event_loop_leaf() -> CanonicalEmbeddedReceipt {
        let receipt =
            AppUiEventLoopShutdownReceipt::seal(AppUiEventLoopShutdownEvidence::after_owner_drop())
                .expect("EventLoop receipt should seal");
        CanonicalEmbeddedReceipt::from_event_loop(&receipt)
    }

    fn window_leaf(cycle_index: u32, operation_id: &str) -> CanonicalEmbeddedReceipt {
        let recovery = EnduranceRecoveryOperationReceipt::surface_device_reopen(
            cycle_index,
            operation_id.to_owned(),
            SHA.to_owned(),
            1,
            2,
            3,
            4,
        )
        .expect("recovery receipt");
        let (json, sha256) = test_integrity_receipt_for_recovery(&recovery);
        CanonicalEmbeddedReceipt { json, sha256 }
    }

    fn verify_projection(
        projection: &CanonicalSurfaceReopenBatchReceipt,
    ) -> Result<bool, AppUiSurfaceDeviceReopenValidationReceiptError> {
        let json = serde_json::to_string(projection).expect("batch projection should serialize");
        let sha256 = batch_lower_sha256(json.as_bytes());
        AppUiSurfaceDeviceReopenValidationReceipt::verify_integrity(&json, &sha256)
    }

    #[test]
    fn successful_batch_replays_every_owner_receipt_and_identity() {
        let projection = CanonicalSurfaceReopenBatchReceipt {
            schema_version: SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION,
            submitted_request_count: 2,
            completed_window_receipts: vec![
                window_leaf(1, "surface.c1"),
                window_leaf(2, "surface.c2"),
            ],
            app_shutdown: app_leaf(),
            cleanup_diagnostic: None,
            outcome: CanonicalSurfaceReopenBatchOutcome::Success {
                event_loop_shutdown: event_loop_leaf(),
            },
        };

        assert_eq!(verify_projection(&projection), Ok(true));
    }

    #[test]
    fn request_failure_seals_before_event_loop_and_replays_nonqualifying() {
        let error = run_app_ui_surface_device_reopen_validation_batch(AppState::new(), Vec::new())
            .expect_err("empty batch must fail");
        let receipt = AppUiSurfaceDeviceReopenValidationReceipt::seal_failure(&error)
            .expect("request failure should seal");

        assert!(!receipt.qualifying());
        assert_eq!(
            AppUiSurfaceDeviceReopenValidationReceipt::verify_integrity(
                receipt.canonical_json(),
                receipt.sha256(),
            ),
            Ok(false)
        );
        assert!(receipt.canonical_json().contains("request_rejected"));
        assert!(!receipt.canonical_json().contains("event_loop_shutdown"));
    }

    #[test]
    fn every_failure_outcome_has_one_valid_exclusive_shape() {
        let app = app_leaf();
        let event_loop = event_loop_leaf();
        let cases = vec![
            CanonicalSurfaceReopenBatchReceipt {
                schema_version: SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION,
                submitted_request_count: 0,
                completed_window_receipts: Vec::new(),
                app_shutdown: app.clone(),
                cleanup_diagnostic: None,
                outcome: CanonicalSurfaceReopenBatchOutcome::RequestRejected {
                    kind: CanonicalRequestRejectionKind::EmptyBatch,
                    primary_diagnostic: "empty batch".to_owned(),
                },
            },
            CanonicalSurfaceReopenBatchReceipt {
                schema_version: SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION,
                submitted_request_count: 1,
                completed_window_receipts: Vec::new(),
                app_shutdown: app.clone(),
                cleanup_diagnostic: None,
                outcome: CanonicalSurfaceReopenBatchOutcome::EventLoopConstructionFailed {
                    kind: CanonicalEventLoopConstructionFailureKind::OperatingSystem,
                    primary_diagnostic: "EventLoop construction failed".to_owned(),
                },
            },
            CanonicalSurfaceReopenBatchReceipt {
                schema_version: SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION,
                submitted_request_count: 2,
                completed_window_receipts: vec![window_leaf(1, "surface.c1")],
                app_shutdown: app.clone(),
                cleanup_diagnostic: None,
                outcome: CanonicalSurfaceReopenBatchOutcome::OperationAdmissionFailed {
                    cycle_index: 2,
                    operation_id: "surface.c2".to_owned(),
                    primary_diagnostic: "deadline overflow".to_owned(),
                    event_loop_shutdown: event_loop.clone(),
                },
            },
            CanonicalSurfaceReopenBatchReceipt {
                schema_version: SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION,
                submitted_request_count: 2,
                completed_window_receipts: vec![window_leaf(1, "surface.c1")],
                app_shutdown: app.clone(),
                cleanup_diagnostic: None,
                outcome: CanonicalSurfaceReopenBatchOutcome::WindowOperationFailed {
                    cycle_index: 2,
                    operation_id: "surface.c2".to_owned(),
                    primary_diagnostic: "Window operation failed".to_owned(),
                    event_loop_shutdown: event_loop.clone(),
                    window_shutdown: CanonicalReturnedWindowShutdown::Missing,
                },
            },
            CanonicalSurfaceReopenBatchReceipt {
                schema_version: SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION,
                submitted_request_count: 1,
                completed_window_receipts: vec![window_leaf(1, "surface.c1")],
                app_shutdown: app,
                cleanup_diagnostic: None,
                outcome: CanonicalSurfaceReopenBatchOutcome::AppShutdownIncomplete {
                    primary_diagnostic: "App shutdown deadline overflow".to_owned(),
                    event_loop_shutdown: event_loop,
                },
            },
        ];

        for projection in cases {
            assert_eq!(verify_projection(&projection), Ok(false));
        }
    }

    #[test]
    fn batch_receipt_rejects_noncanonical_outer_bytes() {
        let projection = CanonicalSurfaceReopenBatchReceipt {
            schema_version: SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION,
            submitted_request_count: 0,
            completed_window_receipts: Vec::new(),
            app_shutdown: app_leaf(),
            cleanup_diagnostic: None,
            outcome: CanonicalSurfaceReopenBatchOutcome::RequestRejected {
                kind: CanonicalRequestRejectionKind::EmptyBatch,
                primary_diagnostic: "empty batch".to_owned(),
            },
        };
        let compact = serde_json::to_string(&projection).expect("batch projection");
        let noncanonical = format!(" {compact}");
        let sha256 = batch_lower_sha256(noncanonical.as_bytes());

        assert_eq!(
            AppUiSurfaceDeviceReopenValidationReceipt::verify_integrity(&noncanonical, &sha256,),
            Err(AppUiSurfaceDeviceReopenValidationReceiptError::NonCanonicalEvidence)
        );
    }

    #[test]
    fn success_rejects_missing_or_replayed_completed_identity() {
        let mut projection = CanonicalSurfaceReopenBatchReceipt {
            schema_version: SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION,
            submitted_request_count: 2,
            completed_window_receipts: vec![window_leaf(1, "surface.c1")],
            app_shutdown: app_leaf(),
            cleanup_diagnostic: None,
            outcome: CanonicalSurfaceReopenBatchOutcome::Success {
                event_loop_shutdown: event_loop_leaf(),
            },
        };
        assert_eq!(
            verify_projection(&projection),
            Err(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence)
        );

        projection.completed_window_receipts.push(window_leaf(1, "surface.c2"));
        assert_eq!(
            verify_projection(&projection),
            Err(AppUiSurfaceDeviceReopenValidationReceiptError::InvalidEvidence)
        );
    }

    #[test]
    fn nested_window_tamper_survives_outer_rehash_but_is_rejected() {
        let mut projection = CanonicalSurfaceReopenBatchReceipt {
            schema_version: SURFACE_REOPEN_BATCH_RECEIPT_SCHEMA_VERSION,
            submitted_request_count: 1,
            completed_window_receipts: vec![window_leaf(1, "surface.c1")],
            app_shutdown: app_leaf(),
            cleanup_diagnostic: None,
            outcome: CanonicalSurfaceReopenBatchOutcome::Success {
                event_loop_shutdown: event_loop_leaf(),
            },
        };
        projection.completed_window_receipts[0].sha256 = "0".repeat(64);

        assert!(matches!(
            verify_projection(&projection),
            Err(AppUiSurfaceDeviceReopenValidationReceiptError::EmbeddedEvidence(_))
        ));
    }
}
