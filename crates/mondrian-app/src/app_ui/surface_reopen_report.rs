//! Durable-file Adapter for standalone Surface/Device validation outcomes.
//!
//! The batch receipt owns semantics. This Module only publishes its exact bytes
//! into a create-new schema-3 envelope and synchronizes the opened file handle.
//! It does not claim parent-directory crash durability.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::app_ui::surface_reopen_batch_receipt::AppUiSurfaceDeviceReopenValidationReceipt;

const SURFACE_REOPEN_REPORT_SCHEMA_VERSION: u32 = 3;

#[derive(Serialize)]
struct SurfaceReopenReport<'a> {
    schema_version: u32,
    qualifying: bool,
    batch_receipt_json: &'a str,
    batch_receipt_sha256: &'a str,
}

/// Successful create-new publication facts for a standalone validation report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUiSurfaceReopenReportPublication {
    output_path: PathBuf,
    bytes_written: u64,
    file_handle_synchronized: bool,
}

impl AppUiSurfaceReopenReportPublication {
    /// Exact report path whose create-new file handle was synchronized.
    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    /// Exact schema-3 byte count written before synchronization.
    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Whether `sync_all` completed for the opened report file handle.
    pub const fn file_handle_synchronized(&self) -> bool {
        self.file_handle_synchronized
    }

    /// Parent-directory crash durability is not established by file sync alone.
    pub const fn parent_directory_crash_durable(&self) -> bool {
        false
    }
}

/// Stable standalone report publication failure.
#[derive(Debug, thiserror::Error)]
pub enum AppUiSurfaceReopenReportPublicationError {
    /// The schema-3 envelope could not be serialized.
    #[error("could not serialize Surface validation report: {0}")]
    Serialization(#[source] serde_json::Error),
    /// The destination could not be opened with create-new semantics.
    #[error("could not create new Surface validation report {path}: {source}")]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The complete schema-3 bytes could not be written.
    #[error("could not write Surface validation report {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The opened report file handle could not be synchronized.
    #[error("could not synchronize Surface validation report {path}: {source}")]
    Synchronize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The encoded byte count could not fit the durable counter schema.
    #[error("Surface validation report byte count overflow")]
    ByteCountOverflow,
}

/// Create, write, and synchronize one immutable schema-3 outcome report.
pub fn publish_surface_reopen_validation_report(
    output_path: &Path,
    receipt: &AppUiSurfaceDeviceReopenValidationReceipt,
) -> Result<AppUiSurfaceReopenReportPublication, AppUiSurfaceReopenReportPublicationError> {
    let report = serde_json::to_vec(&SurfaceReopenReport {
        schema_version: SURFACE_REOPEN_REPORT_SCHEMA_VERSION,
        qualifying: receipt.qualifying(),
        batch_receipt_json: receipt.canonical_json(),
        batch_receipt_sha256: receipt.sha256(),
    })
    .map_err(AppUiSurfaceReopenReportPublicationError::Serialization)?;
    let bytes_written = u64::try_from(report.len())
        .map_err(|_| AppUiSurfaceReopenReportPublicationError::ByteCountOverflow)?;
    let mut output =
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output_path)
            .map_err(|source| AppUiSurfaceReopenReportPublicationError::Create {
                path: output_path.to_path_buf(),
                source,
            })?;
    output.write_all(&report).map_err(|source| {
        AppUiSurfaceReopenReportPublicationError::Write { path: output_path.to_path_buf(), source }
    })?;
    output.sync_all().map_err(
        |source| AppUiSurfaceReopenReportPublicationError::Synchronize {
            path: output_path.to_path_buf(),
            source,
        },
    )?;
    Ok(AppUiSurfaceReopenReportPublication {
        output_path: output_path.to_path_buf(),
        bytes_written,
        file_handle_synchronized: true,
    })
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::app::AppState;
    use crate::app_ui::surface_reopen_batch_receipt::AppUiSurfaceDeviceReopenValidationReceipt;
    use crate::app_ui::window::run_app_ui_surface_device_reopen_validation_batch;

    use super::*;

    fn unique_test_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).expect("system time").as_nanos();
        std::env::temp_dir().join(format!(
            "mondrian-surface-report-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn request_failure_receipt() -> AppUiSurfaceDeviceReopenValidationReceipt {
        let error = run_app_ui_surface_device_reopen_validation_batch(AppState::new(), Vec::new())
            .expect_err("empty batch must fail");
        AppUiSurfaceDeviceReopenValidationReceipt::seal_failure(&error)
            .expect("request failure should seal")
    }

    #[test]
    fn schema_three_publishes_verifiable_failure_before_nonzero_exit_decision() {
        let directory = unique_test_directory("failure");
        std::fs::create_dir(&directory).expect("test directory");
        let output_path = directory.join("report.json");
        let receipt = request_failure_receipt();
        let publication = publish_surface_reopen_validation_report(&output_path, &receipt)
            .expect("failure report should publish");

        assert_eq!(publication.output_path(), output_path);
        assert!(publication.file_handle_synchronized());
        assert!(!publication.parent_directory_crash_durable());
        let bytes = std::fs::read(&output_path).expect("published report");
        assert_eq!(publication.bytes_written(), bytes.len() as u64);
        let report: serde_json::Value = serde_json::from_slice(&bytes).expect("schema-3 report");
        assert_eq!(report["schema_version"], 3);
        assert_eq!(report["qualifying"], false);
        let nested_json = report["batch_receipt_json"].as_str().expect("nested receipt JSON");
        let nested_sha256 = report["batch_receipt_sha256"].as_str().expect("nested receipt hash");
        assert_eq!(
            AppUiSurfaceDeviceReopenValidationReceipt::verify_integrity(nested_json, nested_sha256,),
            Ok(false)
        );

        std::fs::remove_file(&output_path).expect("remove report");
        std::fs::remove_dir(&directory).expect("remove test directory");
    }

    #[test]
    fn create_new_never_overwrites_an_existing_report() {
        let directory = unique_test_directory("existing");
        std::fs::create_dir(&directory).expect("test directory");
        let output_path = directory.join("report.json");
        std::fs::write(&output_path, b"existing evidence").expect("existing report");
        let receipt = request_failure_receipt();

        let error = publish_surface_reopen_validation_report(&output_path, &receipt)
            .expect_err("existing report must not be overwritten");
        assert!(matches!(
            error,
            AppUiSurfaceReopenReportPublicationError::Create { .. }
        ));
        assert_eq!(
            std::fs::read(&output_path).expect("existing report"),
            b"existing evidence"
        );

        std::fs::remove_file(&output_path).expect("remove report");
        std::fs::remove_dir(&directory).expect("remove test directory");
    }
}
