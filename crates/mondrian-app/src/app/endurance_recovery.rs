//! Sealed, independently replayable receipts for controlled endurance recovery.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::endurance_export::ExportCancelRetryFacts;
use super::endurance_playback::{CachePressureRecoveryFacts, SeekRecoveryFacts};
use super::endurance_qualification::EnduranceRecoveryStep;
#[cfg(feature = "validation")]
use crate::app_ui::window::SurfaceDeviceReopenFacts;

const RECOVERY_RECEIPT_SCHEMA_VERSION: u32 = 3;
pub(crate) const MAXIMUM_RECOVERY_RECEIPT_JSON_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
enum EnduranceRecoveryOperationEvidence {
    Seek {
        schema_version: u32,
        cycle_index: u32,
        operation_id: String,
        sequence_binding_sha256: String,
        from_frame: i64,
        target_frame: i64,
        before_epoch: u64,
        after_epoch: u64,
        exact_picture_ready: bool,
    },
    SurfaceDeviceReopen {
        schema_version: u32,
        cycle_index: u32,
        operation_id: String,
        sequence_binding_sha256: String,
        surface_generation_before: u64,
        surface_generation_after: u64,
        device_generation_before: u64,
        device_generation_after: u64,
        shutdown_receipt_json: String,
        shutdown_receipt_sha256: String,
        reopened_contract_json: String,
        reopened_contract_sha256: String,
    },
    ExportCancelRetry {
        schema_version: u32,
        cycle_index: u32,
        operation_id: String,
        cancelled_job_id: String,
        retry_job_id: String,
        cancellation_count_before: u64,
        cancellation_count_after: u64,
        cancelled_terminal_sha256: String,
        retry_artifact_sha256: String,
        retry_validation_report_sha256: String,
    },
    CachePressure {
        schema_version: u32,
        cycle_index: u32,
        operation_id: String,
        decision_generation_before: u64,
        pressure_decision_generation: u64,
        recovered_decision_generation: u64,
        cache_bytes_before_pressure: u64,
        cache_bytes_after_pressure: u64,
        pressure_trimmed_bytes: u64,
        residual_owned_resources: u64,
        recovered_nominal: bool,
        exact_picture_ready: bool,
        gpu_device_losses_before: u64,
        gpu_device_losses_after: u64,
        fatal_errors_before: u64,
        fatal_errors_after: u64,
        export_failures_before: u64,
        export_failures_after: u64,
        pressure_decision_sha256: String,
        recovered_decision_sha256: String,
    },
}

impl EnduranceRecoveryOperationEvidence {
    fn step(&self) -> EnduranceRecoveryStep {
        match self {
            Self::Seek { .. } => EnduranceRecoveryStep::Seek,
            Self::SurfaceDeviceReopen { .. } => EnduranceRecoveryStep::SurfaceDeviceReopen,
            Self::ExportCancelRetry { .. } => EnduranceRecoveryStep::ExportCancelRetry,
            Self::CachePressure { .. } => EnduranceRecoveryStep::CachePressure,
        }
    }

    fn cycle_index(&self) -> u32 {
        match self {
            Self::Seek { cycle_index, .. }
            | Self::SurfaceDeviceReopen { cycle_index, .. }
            | Self::ExportCancelRetry { cycle_index, .. }
            | Self::CachePressure { cycle_index, .. } => *cycle_index,
        }
    }

    fn operation_id(&self) -> &str {
        match self {
            Self::Seek { operation_id, .. }
            | Self::SurfaceDeviceReopen { operation_id, .. }
            | Self::ExportCancelRetry { operation_id, .. }
            | Self::CachePressure { operation_id, .. } => operation_id,
        }
    }

    fn validate(&self) -> Result<(), EnduranceRecoveryReceiptError> {
        let (schema_version, operation_id) = match self {
            Self::Seek { schema_version, operation_id, .. }
            | Self::SurfaceDeviceReopen { schema_version, operation_id, .. }
            | Self::ExportCancelRetry { schema_version, operation_id, .. }
            | Self::CachePressure { schema_version, operation_id, .. } => {
                (*schema_version, operation_id)
            }
        };
        if schema_version != RECOVERY_RECEIPT_SCHEMA_VERSION || !valid_token(operation_id) {
            return Err(EnduranceRecoveryReceiptError::InvalidCommonEvidence);
        }
        match self {
            Self::Seek {
                sequence_binding_sha256,
                from_frame,
                target_frame,
                before_epoch,
                after_epoch,
                exact_picture_ready,
                ..
            } if valid_sha256(sequence_binding_sha256)
                && *from_frame >= 0
                && *target_frame >= 0
                && from_frame != target_frame
                && after_epoch > before_epoch
                && *exact_picture_ready =>
            {
                Ok(())
            }
            Self::SurfaceDeviceReopen {
                sequence_binding_sha256,
                surface_generation_before,
                surface_generation_after,
                device_generation_before,
                device_generation_after,
                shutdown_receipt_json,
                shutdown_receipt_sha256,
                reopened_contract_json,
                reopened_contract_sha256,
                ..
            } if valid_sha256(sequence_binding_sha256)
                && *surface_generation_before != 0
                && *surface_generation_after != 0
                && surface_generation_before != surface_generation_after
                && *device_generation_before != 0
                && *device_generation_after != 0
                && device_generation_before != device_generation_after
                && valid_embedded_json_sha256(shutdown_receipt_json, shutdown_receipt_sha256)
                && valid_embedded_json_sha256(reopened_contract_json, reopened_contract_sha256)
                && valid_surface_shutdown_receipt(shutdown_receipt_json)
                && valid_reopened_surface_contract(
                    reopened_contract_json,
                    *surface_generation_after,
                    *device_generation_after,
                ) =>
            {
                Ok(())
            }
            Self::ExportCancelRetry {
                cancelled_job_id,
                retry_job_id,
                cancellation_count_before,
                cancellation_count_after,
                cancelled_terminal_sha256,
                retry_artifact_sha256,
                retry_validation_report_sha256,
                ..
            } if valid_token(cancelled_job_id)
                && valid_token(retry_job_id)
                && cancelled_job_id != retry_job_id
                && cancellation_count_before.checked_add(1) == Some(*cancellation_count_after)
                && valid_sha256(cancelled_terminal_sha256)
                && valid_sha256(retry_artifact_sha256)
                && valid_sha256(retry_validation_report_sha256) =>
            {
                Ok(())
            }
            Self::CachePressure {
                decision_generation_before,
                pressure_decision_generation,
                recovered_decision_generation,
                cache_bytes_before_pressure,
                cache_bytes_after_pressure,
                pressure_trimmed_bytes,
                residual_owned_resources,
                recovered_nominal,
                exact_picture_ready,
                gpu_device_losses_before,
                gpu_device_losses_after,
                fatal_errors_before,
                fatal_errors_after,
                export_failures_before,
                export_failures_after,
                pressure_decision_sha256,
                recovered_decision_sha256,
                ..
            } if decision_generation_before < pressure_decision_generation
                && pressure_decision_generation < recovered_decision_generation
                && cache_bytes_after_pressure < cache_bytes_before_pressure
                && cache_bytes_before_pressure.checked_sub(*cache_bytes_after_pressure)
                    == Some(*pressure_trimmed_bytes)
                && *pressure_trimmed_bytes != 0
                && *residual_owned_resources == 0
                && *recovered_nominal
                && *exact_picture_ready
                && gpu_device_losses_before == gpu_device_losses_after
                && fatal_errors_before == fatal_errors_after
                && export_failures_before == export_failures_after
                && valid_sha256(pressure_decision_sha256)
                && valid_sha256(recovered_decision_sha256) =>
            {
                Ok(())
            }
            _ => Err(EnduranceRecoveryReceiptError::InvalidOperationEvidence { step: self.step() }),
        }
    }
}

/// Sealed canonical recovery evidence accepted by the campaign event ledger.
///
/// Product operation owners construct this value inside the App crate after
/// validating their exact before/after facts. External runtime implementations
/// cannot construct arbitrary recovery events or provide a caller-authored hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnduranceRecoveryOperationReceipt {
    evidence: EnduranceRecoveryOperationEvidence,
    canonical_json: String,
    sha256: String,
}

impl EnduranceRecoveryOperationReceipt {
    fn seal(
        evidence: EnduranceRecoveryOperationEvidence,
    ) -> Result<Self, EnduranceRecoveryReceiptError> {
        evidence.validate()?;
        let canonical_json = serde_json::to_string(&evidence)
            .map_err(|error| EnduranceRecoveryReceiptError::Serialization(error.to_string()))?;
        if canonical_json.len() > MAXIMUM_RECOVERY_RECEIPT_JSON_BYTES {
            return Err(EnduranceRecoveryReceiptError::TooLarge);
        }
        let sha256 = lower_sha256(canonical_json.as_bytes());
        Ok(Self { evidence, canonical_json, sha256 })
    }

    /// Exact cycle index sealed by the operation owner.
    pub fn cycle_index(&self) -> u32 {
        self.evidence.cycle_index()
    }

    /// Exact ordered operation kind sealed by the owner.
    pub fn step(&self) -> EnduranceRecoveryStep {
        self.evidence.step()
    }

    /// Stable per-phase operation identity used to reject receipt replay.
    pub fn operation_id(&self) -> &str {
        self.evidence.operation_id()
    }

    /// Canonical embedded JSON that an external verifier can parse and replay.
    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }

    /// SHA-256 over the exact UTF-8 bytes returned by [`Self::canonical_json`].
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(super) fn from_seek_facts(
        facts: SeekRecoveryFacts,
    ) -> Result<Self, EnduranceRecoveryReceiptError> {
        Self::seal(EnduranceRecoveryOperationEvidence::Seek {
            schema_version: RECOVERY_RECEIPT_SCHEMA_VERSION,
            cycle_index: facts.cycle_index(),
            operation_id: facts.operation_id().to_owned(),
            sequence_binding_sha256: facts.sequence_binding_sha256().to_owned(),
            from_frame: facts.source_frame(),
            target_frame: facts.target_frame(),
            before_epoch: facts.before_epoch(),
            after_epoch: facts.after_epoch(),
            exact_picture_ready: true,
        })
    }

    pub(super) fn from_cache_pressure_facts(
        facts: CachePressureRecoveryFacts,
    ) -> Result<Self, EnduranceRecoveryReceiptError> {
        Self::seal(EnduranceRecoveryOperationEvidence::CachePressure {
            schema_version: RECOVERY_RECEIPT_SCHEMA_VERSION,
            cycle_index: facts.cycle_index(),
            operation_id: facts.operation_id().to_owned(),
            decision_generation_before: facts.decision_generation_before(),
            pressure_decision_generation: facts.pressure_decision_generation(),
            recovered_decision_generation: facts.recovered_decision_generation(),
            cache_bytes_before_pressure: facts.cache_bytes_before_pressure(),
            cache_bytes_after_pressure: facts.cache_bytes_after_pressure(),
            pressure_trimmed_bytes: facts.pressure_trimmed_bytes(),
            residual_owned_resources: facts.residual_owned_resources(),
            recovered_nominal: true,
            exact_picture_ready: facts.exact_picture_ready(),
            gpu_device_losses_before: facts.gpu_device_losses_before(),
            gpu_device_losses_after: facts.gpu_device_losses_after(),
            fatal_errors_before: facts.fatal_errors_before(),
            fatal_errors_after: facts.fatal_errors_after(),
            export_failures_before: facts.export_failures_before(),
            export_failures_after: facts.export_failures_after(),
            pressure_decision_sha256: facts.pressure_decision_sha256().to_owned(),
            recovered_decision_sha256: facts.recovered_decision_sha256().to_owned(),
        })
    }

    pub(super) fn from_export_cancel_retry_facts(
        facts: ExportCancelRetryFacts,
    ) -> Result<Self, EnduranceRecoveryReceiptError> {
        Self::seal(EnduranceRecoveryOperationEvidence::ExportCancelRetry {
            schema_version: RECOVERY_RECEIPT_SCHEMA_VERSION,
            cycle_index: facts.cycle_index(),
            operation_id: facts.operation_id().to_owned(),
            cancelled_job_id: facts.cancelled_job_id().to_owned(),
            retry_job_id: facts.retry_job_id().to_owned(),
            cancellation_count_before: facts.cancellation_count_before(),
            cancellation_count_after: facts.cancellation_count_after(),
            cancelled_terminal_sha256: facts.cancelled_terminal_sha256().to_owned(),
            retry_artifact_sha256: facts.retry_artifact_sha256().to_owned(),
            retry_validation_report_sha256: facts.retry_validation_report_sha256().to_owned(),
        })
    }

    #[cfg(feature = "validation")]
    pub(crate) fn from_surface_device_reopen_facts(
        facts: SurfaceDeviceReopenFacts,
    ) -> Result<Self, EnduranceRecoveryReceiptError> {
        Self::seal(EnduranceRecoveryOperationEvidence::SurfaceDeviceReopen {
            schema_version: RECOVERY_RECEIPT_SCHEMA_VERSION,
            cycle_index: facts.cycle_index(),
            operation_id: facts.operation_id().to_owned(),
            sequence_binding_sha256: facts.sequence_binding_sha256().to_owned(),
            surface_generation_before: facts.surface_generation_before(),
            surface_generation_after: facts.surface_generation_after(),
            device_generation_before: facts.device_generation_before(),
            device_generation_after: facts.device_generation_after(),
            shutdown_receipt_json: facts.shutdown_receipt_json().to_owned(),
            shutdown_receipt_sha256: facts.shutdown_receipt_sha256().to_owned(),
            reopened_contract_json: facts.reopened_contract_json().to_owned(),
            reopened_contract_sha256: facts.reopened_contract_sha256().to_owned(),
        })
    }

    #[cfg(test)]
    pub(crate) fn seek(
        cycle_index: u32,
        operation_id: String,
        sequence_binding_sha256: String,
        from_frame: i64,
        target_frame: i64,
        before_epoch: u64,
        after_epoch: u64,
        exact_picture_ready: bool,
    ) -> Result<Self, EnduranceRecoveryReceiptError> {
        Self::seal(EnduranceRecoveryOperationEvidence::Seek {
            schema_version: RECOVERY_RECEIPT_SCHEMA_VERSION,
            cycle_index,
            operation_id,
            sequence_binding_sha256,
            from_frame,
            target_frame,
            before_epoch,
            after_epoch,
            exact_picture_ready,
        })
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn surface_device_reopen(
        cycle_index: u32,
        operation_id: String,
        sequence_binding_sha256: String,
        surface_generation_before: u64,
        surface_generation_after: u64,
        device_generation_before: u64,
        device_generation_after: u64,
    ) -> Result<Self, EnduranceRecoveryReceiptError> {
        let shutdown_receipt_json = r#"{"schema_version":1,"worker_started":true,"worker_terminated":true,"worker_panicked":false,"timed_out":false,"retirement_requested":true,"retirement_handoff_accepted":true,"retirement_completed":true,"generation_terminal_kind":null}"#.to_owned();
        let display_contract_sha256 =
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let reopened_picture_json = format!(
            concat!(
                r#"{{"sequence_id":"test-sequence","frame":0,"width":1920,"height":1080,"output_target":"Display","output_color_space":"Srgb","monitor_color_space":"Srgb","tone_map":false,"display_view":null,"frame_residency":{{"execution_observed":true,"working_residency":"GpuWorkingCompositeExecuted"}},"display_contract_sha256":"{display_contract_sha256}"}}"#,
            ),
            display_contract_sha256 = display_contract_sha256,
        );
        let reopened_picture_sha256 = lower_sha256(reopened_picture_json.as_bytes());
        let reopened_picture_json_literal =
            serde_json::to_string(&reopened_picture_json).expect("test picture JSON string");
        let reopened_contract_json = format!(
            concat!(
                r#"{{"schema_version":2,"surface_generation":{surface_generation_after},"device_generation":{device_generation_after},"actual_surface_presented":true,"original_picture_sha256":"{reopened_picture_sha256}","reopened_picture_json":{reopened_picture_json_literal},"reopened_picture_sha256":"{reopened_picture_sha256}"}}"#,
            ),
            surface_generation_after = surface_generation_after,
            device_generation_after = device_generation_after,
            reopened_picture_sha256 = reopened_picture_sha256,
            reopened_picture_json_literal = reopened_picture_json_literal,
        );
        let shutdown_receipt_sha256 = lower_sha256(shutdown_receipt_json.as_bytes());
        let reopened_contract_sha256 = lower_sha256(reopened_contract_json.as_bytes());
        Self::seal(EnduranceRecoveryOperationEvidence::SurfaceDeviceReopen {
            schema_version: RECOVERY_RECEIPT_SCHEMA_VERSION,
            cycle_index,
            operation_id,
            sequence_binding_sha256,
            surface_generation_before,
            surface_generation_after,
            device_generation_before,
            device_generation_after,
            shutdown_receipt_json,
            shutdown_receipt_sha256,
            reopened_contract_json,
            reopened_contract_sha256,
        })
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn export_cancel_retry(
        cycle_index: u32,
        operation_id: String,
        cancelled_job_id: String,
        retry_job_id: String,
        cancellation_count_before: u64,
        cancellation_count_after: u64,
        cancelled_terminal_sha256: String,
        retry_artifact_sha256: String,
        retry_validation_report_sha256: String,
    ) -> Result<Self, EnduranceRecoveryReceiptError> {
        Self::seal(EnduranceRecoveryOperationEvidence::ExportCancelRetry {
            schema_version: RECOVERY_RECEIPT_SCHEMA_VERSION,
            cycle_index,
            operation_id,
            cancelled_job_id,
            retry_job_id,
            cancellation_count_before,
            cancellation_count_after,
            cancelled_terminal_sha256,
            retry_artifact_sha256,
            retry_validation_report_sha256,
        })
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn cache_pressure(
        cycle_index: u32,
        operation_id: String,
        decision_generation_before: u64,
        pressure_decision_generation: u64,
        recovered_decision_generation: u64,
        cache_bytes_before_pressure: u64,
        cache_bytes_after_pressure: u64,
        pressure_trimmed_bytes: u64,
        residual_owned_resources: u64,
        recovered_nominal: bool,
        exact_picture_ready: bool,
        gpu_device_losses_before: u64,
        gpu_device_losses_after: u64,
        fatal_errors_before: u64,
        fatal_errors_after: u64,
        export_failures_before: u64,
        export_failures_after: u64,
        pressure_decision_sha256: String,
        recovered_decision_sha256: String,
    ) -> Result<Self, EnduranceRecoveryReceiptError> {
        Self::seal(EnduranceRecoveryOperationEvidence::CachePressure {
            schema_version: RECOVERY_RECEIPT_SCHEMA_VERSION,
            cycle_index,
            operation_id,
            decision_generation_before,
            pressure_decision_generation,
            recovered_decision_generation,
            cache_bytes_before_pressure,
            cache_bytes_after_pressure,
            pressure_trimmed_bytes,
            residual_owned_resources,
            recovered_nominal,
            exact_picture_ready,
            gpu_device_losses_before,
            gpu_device_losses_after,
            fatal_errors_before,
            fatal_errors_after,
            export_failures_before,
            export_failures_after,
            pressure_decision_sha256,
            recovered_decision_sha256,
        })
    }

    pub(crate) fn parse_and_validate(
        canonical_json: &str,
        expected_sha256: &str,
    ) -> Result<Self, EnduranceRecoveryReceiptError> {
        if canonical_json.len() > MAXIMUM_RECOVERY_RECEIPT_JSON_BYTES
            || !valid_sha256(expected_sha256)
            || lower_sha256(canonical_json.as_bytes()) != expected_sha256
        {
            return Err(EnduranceRecoveryReceiptError::DigestMismatch);
        }
        let evidence: EnduranceRecoveryOperationEvidence = serde_json::from_str(canonical_json)
            .map_err(|error| EnduranceRecoveryReceiptError::Serialization(error.to_string()))?;
        let receipt = Self::seal(evidence)?;
        if receipt.canonical_json != canonical_json || receipt.sha256 != expected_sha256 {
            return Err(EnduranceRecoveryReceiptError::NonCanonical);
        }
        Ok(receipt)
    }
}

fn lower_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_embedded_json_sha256(canonical_json: &str, expected_sha256: &str) -> bool {
    canonical_json.len() <= MAXIMUM_RECOVERY_RECEIPT_JSON_BYTES
        && valid_sha256(expected_sha256)
        && lower_sha256(canonical_json.as_bytes()) == expected_sha256
        && serde_json::from_str::<serde_json::Value>(canonical_json).is_ok()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceShutdownReceiptEvidence {
    schema_version: u32,
    worker_started: bool,
    worker_terminated: bool,
    worker_panicked: bool,
    timed_out: bool,
    retirement_requested: bool,
    retirement_handoff_accepted: bool,
    retirement_completed: bool,
    generation_terminal_kind: Option<String>,
}

fn valid_surface_shutdown_receipt(canonical_json: &str) -> bool {
    serde_json::from_str::<SurfaceShutdownReceiptEvidence>(canonical_json)
        .ok()
        .is_some_and(|receipt| {
            receipt.schema_version == 1
                && receipt.worker_started
                && receipt.worker_terminated
                && !receipt.worker_panicked
                && !receipt.timed_out
                && receipt.retirement_requested
                && receipt.retirement_handoff_accepted
                && receipt.retirement_completed
                && receipt.generation_terminal_kind.is_none()
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReopenedSurfaceContractEvidence {
    schema_version: u32,
    surface_generation: u64,
    device_generation: u64,
    actual_surface_presented: bool,
    original_picture_sha256: String,
    reopened_picture_json: String,
    reopened_picture_sha256: String,
}

fn valid_reopened_surface_contract(
    canonical_json: &str,
    expected_surface_generation: u64,
    expected_device_generation: u64,
) -> bool {
    let Ok(contract) = serde_json::from_str::<ReopenedSurfaceContractEvidence>(canonical_json)
    else {
        return false;
    };
    if contract.schema_version != 2
        || contract.surface_generation != expected_surface_generation
        || contract.device_generation != expected_device_generation
        || !contract.actual_surface_presented
        || !valid_embedded_json_sha256(
            &contract.reopened_picture_json,
            &contract.reopened_picture_sha256,
        )
        || contract.original_picture_sha256 != contract.reopened_picture_sha256
    {
        return false;
    }
    let Ok(picture) = serde_json::from_str::<serde_json::Value>(&contract.reopened_picture_json)
    else {
        return false;
    };
    let Some(picture) = picture.as_object() else {
        return false;
    };
    let expected_picture_keys = [
        "sequence_id",
        "frame",
        "width",
        "height",
        "output_target",
        "output_color_space",
        "monitor_color_space",
        "tone_map",
        "display_view",
        "frame_residency",
        "display_contract_sha256",
    ];
    let exact_picture_shape = picture.len() == expected_picture_keys.len()
        && expected_picture_keys.iter().all(|key| picture.contains_key(*key));
    let frame_residency = picture.get("frame_residency").and_then(serde_json::Value::as_object);
    valid_sha256(&contract.original_picture_sha256)
        && exact_picture_shape
        && picture
            .get("sequence_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| !id.is_empty())
        && picture
            .get("frame")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|frame| frame >= 0)
        && picture
            .get("width")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|width| width > 0)
        && picture
            .get("height")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|height| height > 0)
        && picture.get("output_target").and_then(serde_json::Value::as_str) == Some("Display")
        && picture
            .get("display_contract_sha256")
            .and_then(serde_json::Value::as_str)
            .is_some_and(valid_sha256)
        && frame_residency
            .and_then(|residency| residency.get("execution_observed"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && frame_residency
            .and_then(|residency| residency.get("working_residency"))
            .and_then(serde_json::Value::as_str)
            == Some("GpuWorkingCompositeExecuted")
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Stable recovery receipt sealing or replay failure.
#[derive(Debug, Error)]
pub enum EnduranceRecoveryReceiptError {
    /// Schema or operation identity is invalid.
    #[error("recovery receipt common evidence is invalid")]
    InvalidCommonEvidence,
    /// Step-specific before/after evidence is not a successful recovery.
    #[error("recovery receipt evidence is invalid for {step:?}")]
    InvalidOperationEvidence { step: EnduranceRecoveryStep },
    /// Canonical receipt JSON exceeded the fixed event bound.
    #[error("recovery receipt exceeds its fixed JSON byte bound")]
    TooLarge,
    /// JSON serialization or parsing failed.
    #[error("recovery receipt JSON failed: {0}")]
    Serialization(String),
    /// Event digest does not bind the exact embedded JSON bytes.
    #[error("recovery receipt digest does not match its embedded JSON")]
    DigestMismatch,
    /// Embedded JSON is semantically valid but not the sole canonical encoding.
    #[error("recovery receipt JSON is not canonical")]
    NonCanonical,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn all_four_receipts_round_trip_canonical_json_and_digest() {
        let receipts = [
            EnduranceRecoveryOperationReceipt::seek(
                0,
                "seek-0".to_owned(),
                SHA.to_owned(),
                1,
                60,
                3,
                4,
                true,
            )
            .expect("seek receipt"),
            EnduranceRecoveryOperationReceipt::surface_device_reopen(
                0,
                "reopen-0".to_owned(),
                SHA.to_owned(),
                1,
                2,
                4,
                5,
            )
            .expect("reopen receipt"),
            EnduranceRecoveryOperationReceipt::export_cancel_retry(
                0,
                "export-0".to_owned(),
                "cancelled-job".to_owned(),
                "retry-job".to_owned(),
                7,
                8,
                SHA.to_owned(),
                SHA.to_owned(),
                SHA.to_owned(),
            )
            .expect("Export receipt"),
            EnduranceRecoveryOperationReceipt::cache_pressure(
                0,
                "cache-0".to_owned(),
                10,
                11,
                12,
                4096,
                1024,
                3072,
                0,
                true,
                true,
                0,
                0,
                0,
                0,
                0,
                0,
                SHA.to_owned(),
                SHA.to_owned(),
            )
            .expect("cache receipt"),
        ];

        for receipt in receipts {
            let replayed = EnduranceRecoveryOperationReceipt::parse_and_validate(
                receipt.canonical_json(),
                receipt.sha256(),
            )
            .expect("replay canonical receipt");
            assert_eq!(replayed, receipt);
        }
    }

    #[test]
    fn invalid_transition_and_noncanonical_or_tampered_json_are_rejected() {
        assert!(EnduranceRecoveryOperationReceipt::seek(
            0,
            "seek-0".to_owned(),
            SHA.to_owned(),
            1,
            1,
            3,
            3,
            false,
        )
        .is_err());
        let receipt = EnduranceRecoveryOperationReceipt::seek(
            0,
            "seek-0".to_owned(),
            SHA.to_owned(),
            1,
            2,
            3,
            4,
            true,
        )
        .expect("valid seek");
        let spaced = format!(" {}", receipt.canonical_json());
        let spaced_sha = lower_sha256(spaced.as_bytes());
        assert!(matches!(
            EnduranceRecoveryOperationReceipt::parse_and_validate(&spaced, &spaced_sha),
            Err(EnduranceRecoveryReceiptError::NonCanonical)
        ));
        assert!(matches!(
            EnduranceRecoveryOperationReceipt::parse_and_validate(receipt.canonical_json(), SHA,),
            Err(EnduranceRecoveryReceiptError::DigestMismatch)
        ));
        assert!(EnduranceRecoveryOperationReceipt::cache_pressure(
            0,
            "cache-0".to_owned(),
            1,
            2,
            3,
            4096,
            0,
            4096,
            0,
            true,
            true,
            0,
            0,
            0,
            1,
            0,
            0,
            SHA.to_owned(),
            SHA.to_owned(),
        )
        .is_err());
    }

    #[test]
    fn surface_receipt_rejects_rehashed_dirty_nested_shutdown_evidence() {
        let receipt = EnduranceRecoveryOperationReceipt::surface_device_reopen(
            0,
            "reopen-0".to_owned(),
            SHA.to_owned(),
            1,
            2,
            3,
            4,
        )
        .expect("valid Surface/device receipt");
        let mut evidence: EnduranceRecoveryOperationEvidence =
            serde_json::from_str(receipt.canonical_json()).expect("receipt evidence");
        let EnduranceRecoveryOperationEvidence::SurfaceDeviceReopen {
            shutdown_receipt_json,
            shutdown_receipt_sha256,
            ..
        } = &mut evidence
        else {
            panic!("expected Surface/device receipt");
        };
        let mut shutdown: serde_json::Value =
            serde_json::from_str(shutdown_receipt_json).expect("shutdown evidence");
        shutdown["worker_terminated"] = serde_json::Value::Bool(false);
        *shutdown_receipt_json = serde_json::to_string(&shutdown).expect("tampered shutdown JSON");
        *shutdown_receipt_sha256 = lower_sha256(shutdown_receipt_json.as_bytes());
        let tampered = serde_json::to_string(&evidence).expect("tampered receipt JSON");
        let tampered_sha = lower_sha256(tampered.as_bytes());

        assert!(matches!(
            EnduranceRecoveryOperationReceipt::parse_and_validate(&tampered, &tampered_sha),
            Err(EnduranceRecoveryReceiptError::InvalidOperationEvidence {
                step: EnduranceRecoveryStep::SurfaceDeviceReopen
            })
        ));
    }

    #[test]
    fn surface_receipt_rejects_rehashed_mismatched_original_picture_digest() {
        let receipt = EnduranceRecoveryOperationReceipt::surface_device_reopen(
            0,
            "reopen-0".to_owned(),
            SHA.to_owned(),
            1,
            2,
            3,
            4,
        )
        .expect("valid Surface/device receipt");
        let mut evidence: EnduranceRecoveryOperationEvidence =
            serde_json::from_str(receipt.canonical_json()).expect("receipt evidence");
        let EnduranceRecoveryOperationEvidence::SurfaceDeviceReopen {
            reopened_contract_json,
            reopened_contract_sha256,
            ..
        } = &mut evidence
        else {
            panic!("expected Surface/device receipt");
        };
        let mut contract: serde_json::Value =
            serde_json::from_str(reopened_contract_json).expect("reopened contract");
        contract["original_picture_sha256"] = serde_json::Value::String(SHA.to_owned());
        *reopened_contract_json =
            serde_json::to_string(&contract).expect("tampered reopened contract JSON");
        *reopened_contract_sha256 = lower_sha256(reopened_contract_json.as_bytes());
        let tampered = serde_json::to_string(&evidence).expect("tampered receipt JSON");
        let tampered_sha = lower_sha256(tampered.as_bytes());

        assert!(matches!(
            EnduranceRecoveryOperationReceipt::parse_and_validate(&tampered, &tampered_sha),
            Err(EnduranceRecoveryReceiptError::InvalidOperationEvidence {
                step: EnduranceRecoveryStep::SurfaceDeviceReopen
            })
        ));
    }
}
