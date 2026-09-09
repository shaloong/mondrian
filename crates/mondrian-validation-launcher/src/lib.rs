//! FFmpeg-free pre-loader ownership and native bootstrap attestation.
//!
//! The launcher seals a fresh application namespace before CreateProcess and
//! retains exact executable/DLL objects until native termination. The child
//! authenticates its pipe server against an externally approved image digest.
//! This filesystem-race contract excludes process injection/privileged handle theft.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Shared native namespace sealing policy for launcher and runtime capsules.
#[cfg(windows)]
pub mod namespace;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{
    attest, launch, prepare_process_authority, process_authority, PreparedAuthority,
};

/// Absolute direct file with an externally approved full-file SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileBinding {
    /// Exact source path.
    pub path: PathBuf,
    /// Lowercase SHA-256 of all bytes.
    pub sha256: String,
}

/// Externally approved input to the FFmpeg-free executable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchPlan {
    /// Exact version, currently one.
    pub schema_version: u32,
    /// Launcher image expected by the child machine plan.
    pub launcher: FileBinding,
    /// Endurance executable to stage and execute.
    pub application: FileBinding,
    /// Complete non-system DLL closure, with unique case-insensitive leaf names.
    pub runtime_files: Vec<FileBinding>,
    /// Exact strict endurance request.
    pub request: FileBinding,
    /// Original non-renewing campaign and shutdown bound.
    pub deadline_ms: u64,
    /// Create-only outer-owner receipt, written after child termination and cleanup.
    pub report_path: PathBuf,
}

/// Native filesystem identity retained before process creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    /// Native volume serial number.
    pub volume_serial: u32,
    /// Native 64-bit file index.
    pub file_index: u64,
    /// Full file length.
    pub length: u64,
}

/// A source identity and its independently hashed staged object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappedImageEvidence {
    /// Original externally approved source.
    pub source: FileBinding,
    /// Exact staged path, held in the sealed namespace.
    pub staged_path: PathBuf,
    /// Native staged file identity.
    pub object: FileIdentity,
}

/// Handshake derived from native pipe peer identity and full module enumeration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreloaderAttestation {
    /// Version of the native bootstrap protocol.
    pub schema_version: u32,
    /// Native launcher process identifier, read from the named pipe.
    pub launcher_pid: u32,
    /// Native child process identifier, independently checked by the launcher.
    pub child_pid: u32,
    /// Approved server executable digest.
    pub launcher_sha256: String,
    /// Exact strict request digest.
    pub request_sha256: String,
    /// Exact externally approved machine-plan digest from the strict request.
    pub machine_plan_sha256: String,
    /// Unique launch challenge reflected by both sides.
    pub challenge: String,
    /// All staged application and runtime objects, including delay-loaded objects.
    pub owned_images: Vec<MappedImageEvidence>,
    /// Actual non-system module mappings observed by the authenticated child.
    pub mapped_image_paths: Vec<PathBuf>,
}

/// Raw outer-owner outcome; a child cannot attest its own post-exit cleanup.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Completed handshake, absent if admission failed.
    pub attestation: Option<PreloaderAttestation>,
    /// Completed child manifest read after all native descendants exit.
    pub child_manifest: Option<FileBinding>,
    /// Native terminal exit code, absent when reap did not complete.
    pub exit_code: Option<i32>,
    /// Original deadline expired.
    pub deadline_exceeded: bool,
    /// Exact capsule removed after all held object handles were released.
    pub capsule_removed: bool,
    /// Kernel Job Object reports zero active descendant processes.
    pub descendants_reaped: bool,
    /// All original admission/termination/cleanup failures, in observation order.
    pub errors: Vec<String>,
}

/// Caller-owned expectation; no environment variable is an authority.
pub struct AttestationExpectation<'a> {
    /// Externally approved launcher path and full digest.
    pub launcher: &'a FileBinding,
    /// Exact application digest bound by this run's runtime-image identity.
    pub application_sha256: &'a str,
    /// Exact strict request digest.
    pub request_sha256: &'a str,
    /// Exact external machine plan bound by the strict run request.
    pub machine_plan_sha256: &'a str,
    /// Complete approved non-system runtime closure.
    pub runtime_files: &'a [FileBinding],
}

/// Native bootstrap failure, never converted to a qualifying capability.
#[derive(Debug, thiserror::Error)]
#[error("pre-loader admission failed: {0}")]
pub struct LaunchError(pub String);

impl From<std::io::Error> for LaunchError {
    fn from(value: std::io::Error) -> Self {
        Self(value.to_string())
    }
}
impl From<serde_json::Error> for LaunchError {
    fn from(value: serde_json::Error) -> Self {
        Self(value.to_string())
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    #[test]
    fn unknown_preloader_evidence_is_not_silently_accepted() {
        let value = serde_json::json!({ "schema_version": 1, "attestation": null, "child_manifest": null,
            "exit_code": null, "deadline_exceeded": true, "capsule_removed": false,
            "descendants_reaped": false, "errors": ["deadline"], "claimed_success": true });
        assert!(serde_json::from_value::<LaunchReport>(value).is_err());
    }
}
