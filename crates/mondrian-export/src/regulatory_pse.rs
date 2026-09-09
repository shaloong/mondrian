//! Frozen external regulatory PSE process Adapter.
use mondrian_broadcast::{
    BroadcastArtifactQcReport, BroadcastQcReport, RegulatoryPseApproval, RegulatoryPseResponse,
};
use mondrian_core::ExecutionCancellationToken;
use mondrian_media::{
    run_supervised_command, SupervisedProcessCleanupReceipt, SupervisedProcessError,
    SupervisedProcessPolicy, SupervisedStreamCapture,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Instant,
};

const MAX_CONTROL_FILE: u64 = 16 * 1024 * 1024;
const MAX_EXECUTABLE: u64 = 512 * 1024 * 1024;
const MAX_RESPONSE: usize = 2 * 1024 * 1024;

/// Explicit third-party installation and externally established approval trust anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegulatoryPseProviderConfig {
    /// Exact native protocol adapter executable; no shell or PATH lookup.
    pub executable: PathBuf,
    /// Original third-party approval document matching the caller's trusted digest.
    pub approval_document: PathBuf,
    /// Provider-native frozen approved profile, supplied unchanged to the process.
    pub approved_profile: PathBuf,
    /// Externally commissioned approval; Mondrian never generates a default approval.
    pub approval: RegulatoryPseApproval,
    /// Explicit complete approved non-system DLL closure for a qualified run.
    /// None is admission-time NotRun; an empty list explicitly declares system-only imports.
    #[serde(default)]
    pub runtime_files: Option<Vec<RegulatoryPseRuntimeFile>>,
}

/// One externally approved native runtime dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegulatoryPseRuntimeFile {
    /// Absolute direct DLL source path.
    pub path: PathBuf,
    /// Externally approved full-file digest.
    pub sha256: [u8; 32],
}

/// Admission failure that must remain NotRun rather than a compliance pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegulatoryPseNotRun {
    /// No external provider was configured.
    ProviderMissing,
    /// A required executable, approval document or native profile is absent.
    RequiredFileMissing {
        /// Missing file path.
        path: PathBuf,
    },
    /// No fixed external approval matching this delivery profile was supplied.
    ApprovalMissingOrMismatched,
    /// This platform lacks the required executable/profile identity lease Adapter.
    NativeIdentityLeaseUnavailable,
    /// Qualified execution lacks an explicit complete provider loader closure.
    RuntimeClosureMissing,
}

/// Native work is possible only after every external prerequisite was verified.
#[derive(Debug)]
pub enum RegulatoryPseAdmission {
    /// Frozen files are held through the complete consuming invocation.
    Available(Box<PreparedRegulatoryPseProvider>),
    /// No process has started; retain this explicit qualification result.
    NotRun(RegulatoryPseNotRun),
}

/// Owned installation identity leases; admission cannot be recreated from report text.
#[derive(Debug)]
pub struct PreparedRegulatoryPseProvider {
    config: RegulatoryPseProviderConfig,
    leases: Vec<File>,
    native: mondrian_media::PreparedRegulatoryPseRuntime,
}

/// Strict stdin protocol delivered once to `--mondrian-regulatory-pse-v1`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegulatoryPseRequest {
    /// Protocol schema, currently one.
    pub schema_version: u32,
    /// Random invocation identity, echoed exactly by stdout.
    pub request_nonce: String,
    /// Externally pinned approval and provider identities.
    pub approval: RegulatoryPseApproval,
    /// Immutable encoded final artifact snapshot; analyze this file, never a preview feed.
    pub artifact_path: PathBuf,
    /// Frozen approved provider-native profile path.
    pub approved_profile_path: PathBuf,
    /// Complete same-run final-artifact QC binding and exact frame inventory.
    pub artifact: BroadcastArtifactQcReport,
}

/// Strict bounded stdout envelope retaining the actual native provider report bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegulatoryPseOutput {
    /// Typed provider analysis and exact request binding.
    pub analysis: RegulatoryPseResponse,
    /// Original vendor report, whose digest must equal `analysis.native_report_sha256`.
    pub native_report: Vec<u8>,
}

/// Original invocation evidence retained on both successful and failed outcomes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegulatoryPseExecutionEvidence {
    /// Explicit interruption or rejection cause, retained with the raw owner evidence.
    pub terminal_failure: Option<RegulatoryPseTerminalFailure>,
    /// Exact request; absent when failure preceded request preparation.
    pub request: Option<RegulatoryPseRequest>,
    /// Exact bounded stdout bytes, including malformed protocol output.
    pub stdout: Vec<u8>,
    /// Bounded stderr bytes, with explicit truncation status.
    pub stderr: Vec<u8>,
    /// Whether the retained stderr tail omitted earlier bytes.
    pub stderr_truncated: bool,
    /// Numeric exit code where supplied by the operating system.
    pub exit_code: Option<i32>,
    /// Native process and pipe ownership closure, never inferred from an exit code.
    pub cleanup: Option<SupervisedProcessCleanupReceipt>,
    /// Parsed provider output, retained even when its binding is rejected.
    pub output: Option<RegulatoryPseOutput>,
    /// Snapshot disposal failure, independent of the analysis outcome.
    pub snapshot_cleanup_error: Option<String>,
    /// Independent approved runtime namespace/file owner cleanup failure.
    pub runtime_cleanup_error: Option<String>,
}

/// Terminal classification independent of a provider's claimed regulatory verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum RegulatoryPseTerminalFailure {
    /// The caller canceled before the consuming operation settled.
    #[error("regulatory PSE canceled")]
    Canceled,
    /// The original monotonic deadline expired.
    #[error("regulatory PSE original deadline exceeded")]
    DeadlineExceeded,
    /// Process or protocol validation failed.
    #[error("regulatory PSE input, process or output was rejected")]
    Rejected,
    /// A panic was isolated while keeping the consuming snapshot owner outside the unwind.
    #[error("regulatory PSE operation panicked")]
    Panicked,
}

/// Complete consuming execution receipt; only this Adapter can construct a success.
#[derive(Debug, Clone, Serialize)]
pub struct RegulatoryPseExecutionReceipt {
    /// All original evidence; serialized for independent review.
    evidence: RegulatoryPseExecutionEvidence,
}

impl RegulatoryPseExecutionReceipt {
    /// Original invocation and consuming ownership evidence.
    pub fn evidence(&self) -> &RegulatoryPseExecutionEvidence {
        &self.evidence
    }
    /// Consume the execution authority while retaining every original observation.
    pub fn into_evidence(self) -> RegulatoryPseExecutionEvidence {
        self.evidence
    }

    /// Rebind only the same final scan after verifying native report identity and cleanup.
    pub fn resolved_qc(&self, artifact: &BroadcastArtifactQcReport) -> Option<BroadcastQcReport> {
        let request = self.evidence.request.as_ref()?;
        let output = self.evidence.output.as_ref()?;
        if request.schema_version != 1
            || serde_json::from_slice::<RegulatoryPseOutput>(&self.evidence.stdout)
                .ok()
                .as_ref()
                != Some(output)
        {
            return None;
        }
        if request.artifact != *artifact
            || self.evidence.snapshot_cleanup_error.is_some()
            || self.evidence.runtime_cleanup_error.is_some()
            || self.evidence.terminal_failure.is_some()
            || !self.evidence.cleanup.as_ref()?.all_resources_released()
            || self.evidence.exit_code != Some(0)
            || output.native_report.is_empty()
            || <[u8; 32]>::from(Sha256::digest(&output.native_report))
                != output.analysis.native_report_sha256
        {
            return None;
        }
        artifact.scan.with_regulatory_pse(
            artifact,
            &request.approval,
            &request.request_nonce,
            &output.analysis,
        )
    }
}

/// Typed failure preserving raw execution evidence rather than a success-shaped summary.
#[derive(Debug, thiserror::Error)]
#[error("regulatory PSE failed: {cause}")]
pub struct RegulatoryPseFailure {
    /// Primary operation error, including typed cancellation/process errors.
    #[source]
    pub cause: anyhow::Error,
    /// Original owned resources and provider output observed before failure.
    pub evidence: Box<RegulatoryPseExecutionEvidence>,
}

fn check_control(
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> anyhow::Result<()> {
    if cancellation.is_canceled() {
        return Err(RegulatoryPseTerminalFailure::Canceled.into());
    }
    if Instant::now() >= deadline {
        return Err(RegulatoryPseTerminalFailure::DeadlineExceeded.into());
    }
    Ok(())
}

fn open_identity_lease(path: &Path) -> anyhow::Result<File> {
    anyhow::ensure!(
        path.is_absolute() && std::fs::symlink_metadata(path)?.file_type().is_file(),
        "PSE requires an absolute direct regular file"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ: prohibit replacement and writes through invocation.
    }
    Ok(options.open(path)?)
}

fn hash_file(
    file: &mut File,
    limit: u64,
    deadline: Instant,
    cancel: &ExecutionCancellationToken,
) -> anyhow::Result<(u64, [u8; 32])> {
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check_control(deadline, cancel)?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| anyhow::anyhow!("PSE input size overflow"))?;
        anyhow::ensure!(bytes <= limit, "PSE input exceeds admitted bound");
        hash.update(&buffer[..read]);
    }
    check_control(deadline, cancel)?;
    anyhow::ensure!(bytes != 0, "PSE input is empty");
    Ok((bytes, hash.finalize().into()))
}

/// Admit a real external provider without spawning; missing hardware/software stays NotRun.
pub fn admit_regulatory_pse_provider(
    config: Option<&RegulatoryPseProviderConfig>,
    qc_profile_fingerprint: [u8; 32],
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> Result<RegulatoryPseAdmission, RegulatoryPseFailure> {
    let Some(config) = config else {
        return Ok(RegulatoryPseAdmission::NotRun(
            RegulatoryPseNotRun::ProviderMissing,
        ));
    };
    if !config.approval.validate()
        || config.approval.qc_profile_fingerprint != qc_profile_fingerprint
    {
        return Ok(RegulatoryPseAdmission::NotRun(
            RegulatoryPseNotRun::ApprovalMissingOrMismatched,
        ));
    }
    if !cfg!(windows) {
        return Ok(RegulatoryPseAdmission::NotRun(
            RegulatoryPseNotRun::NativeIdentityLeaseUnavailable,
        ));
    }
    let specifications = [
        (
            &config.executable,
            config.approval.executable_sha256,
            MAX_EXECUTABLE,
        ),
        (
            &config.approval_document,
            config.approval.approval_document_sha256,
            MAX_CONTROL_FILE,
        ),
        (
            &config.approved_profile,
            config.approval.approved_profile_sha256,
            MAX_CONTROL_FILE,
        ),
    ];
    let mut leases = Vec::new();
    for (path, expected, bound) in specifications {
        if !path.exists() {
            return Ok(RegulatoryPseAdmission::NotRun(
                RegulatoryPseNotRun::RequiredFileMissing { path: path.clone() },
            ));
        }
        let result = (|| -> anyhow::Result<File> {
            check_control(deadline, cancellation)?;
            let mut file = open_identity_lease(path)?;
            anyhow::ensure!(
                hash_file(&mut file, bound, deadline, cancellation)?.1 == expected,
                "PSE approved installation identity mismatch: {}",
                path.display()
            );
            Ok(file)
        })();
        leases.push(
            result.map_err(|cause| RegulatoryPseFailure { cause, evidence: Box::default() })?,
        );
    }
    let prepare_native = (|| -> anyhow::Result<_> {
        let mut runtime = config.runtime_files.as_ref().map(|_| Vec::new());
        if let Some(files) = &config.runtime_files {
            for file in files {
                check_control(deadline, cancellation)?;
                if !file.path.exists() {
                    return Ok(None);
                }
                let lease = open_identity_lease(&file.path)?;
                if let Some(runtime) = &mut runtime {
                    runtime.push(mondrian_media::ApprovedProviderFile::from_retained(
                        file.path.clone(),
                        file.sha256,
                        lease,
                    ));
                }
            }
        }
        let approved = [
            mondrian_media::ApprovedProviderFile::from_retained(
                config.executable.clone(),
                config.approval.executable_sha256,
                leases[0].try_clone()?,
            ),
            mondrian_media::ApprovedProviderFile::from_retained(
                config.approval_document.clone(),
                config.approval.approval_document_sha256,
                leases[1].try_clone()?,
            ),
            mondrian_media::ApprovedProviderFile::from_retained(
                config.approved_profile.clone(),
                config.approval.approved_profile_sha256,
                leases[2].try_clone()?,
            ),
        ];
        Ok(Some(mondrian_media::prepare_regulatory_pse_runtime(
            approved,
            runtime,
            deadline,
            cancellation,
        )?))
    })()
    .map_err(|cause| RegulatoryPseFailure { cause, evidence: Box::default() })?;
    let Some(mondrian_media::RegulatoryPseRuntimeAdmission::Available(native)) = prepare_native
    else {
        return Ok(RegulatoryPseAdmission::NotRun(
            RegulatoryPseNotRun::RuntimeClosureMissing,
        ));
    };
    Ok(RegulatoryPseAdmission::Available(Box::new(
        PreparedRegulatoryPseProvider { config: config.clone(), leases, native },
    )))
}

impl PreparedRegulatoryPseProvider {
    /// Analyze immutable final artifact bytes with one deadline covering copy, native execution and close.
    pub fn verify_final_artifact(
        self,
        path: &Path,
        artifact: &BroadcastArtifactQcReport,
        maximum_artifact_bytes: u64,
        deadline: Instant,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<RegulatoryPseExecutionReceipt, RegulatoryPseFailure> {
        self.verify_final_artifact_from_source(
            path,
            None,
            artifact,
            maximum_artifact_bytes,
            deadline,
            cancellation,
        )
    }

    /// Analyze the exact publication object while borrowing its retained deny-write
    /// authority. Cloning that handle avoids a conflicting second writer-denying open.
    pub fn verify_owned_final_artifact(
        self,
        owner: &mondrian_storage::OwnedPublicationFile,
        artifact: &BroadcastArtifactQcReport,
        maximum_artifact_bytes: u64,
        deadline: Instant,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<RegulatoryPseExecutionReceipt, RegulatoryPseFailure> {
        self.verify_final_artifact_from_source(
            owner.path(),
            Some(owner),
            artifact,
            maximum_artifact_bytes,
            deadline,
            cancellation,
        )
    }

    fn verify_final_artifact_from_source(
        self,
        path: &Path,
        owner: Option<&mondrian_storage::OwnedPublicationFile>,
        artifact: &BroadcastArtifactQcReport,
        maximum_artifact_bytes: u64,
        deadline: Instant,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<RegulatoryPseExecutionReceipt, RegulatoryPseFailure> {
        let mut evidence = RegulatoryPseExecutionEvidence::default();
        let mut snapshot = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> anyhow::Result<()> {
                check_control(deadline, cancellation)?;
                anyhow::ensure!(
                    artifact.verify_evidence()
                        && artifact.scan.complete
                        && maximum_artifact_bytes >= artifact.artifact_bytes,
                    "PSE requires a complete bounded final-artifact scan"
                );
                let mut source = match owner {
                    Some(owner) => owner.file()?.try_clone()?,
                    None => open_identity_lease(path)?,
                };
                anyhow::ensure!(
                    hash_file(&mut source, maximum_artifact_bytes, deadline, cancellation)?
                        == (artifact.artifact_bytes, artifact.artifact_sha256),
                    "PSE final artifact differs from this scan"
                );
                let prepared = crate::artifact_verifier::snapshot_artifact_until(
                    path, maximum_artifact_bytes, cancellation, deadline,
                ).map_err(|cause| {
                    if let crate::artifact_verifier::IndependentExportArtifactVerificationError::SnapshotCleanup { cleanup, .. } = &cause {
                        evidence.snapshot_cleanup_error = Some(cleanup.to_string());
                    }
                    anyhow::Error::new(cause)
                })?;
                let expected_identity = (
                    artifact.artifact_bytes,
                    artifact
                        .artifact_sha256
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>(),
                );
                let prepared_identity = (prepared.bytes, prepared.sha256.clone());
                snapshot = Some(prepared.file.into_temp_path());
                anyhow::ensure!(
                    prepared_identity == expected_identity,
                    "PSE copied snapshot identity mismatch"
                );
                let snapshot_path = snapshot
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("PSE snapshot owner absent"))?;
                let _snapshot_lease = open_identity_lease(snapshot_path)?;
                anyhow::ensure!(
                    crate::artifact_verifier::hash_published_artifact_until(
                        snapshot_path,
                        maximum_artifact_bytes,
                        cancellation,
                        deadline,
                    )? == expected_identity,
                    "PSE snapshot identity mismatch"
                );
                let request = RegulatoryPseRequest {
                    schema_version: 1,
                    request_nonce: uuid::Uuid::new_v4().to_string(),
                    approval: self.config.approval.clone(),
                    artifact_path: snapshot_path.to_path_buf(),
                    approved_profile_path: self.native.approved_profile_path().to_path_buf(),
                    artifact: artifact.clone(),
                };
                let stdin = serde_json::to_vec(&request)?;
                anyhow::ensure!(stdin.len() <= MAX_RESPONSE, "PSE request exceeds bound");
                evidence.request = Some(request.clone());
                let mut command = self.native.command(deadline, cancellation)?;
                let output = run_supervised_command(
                    &mut command,
                    Some(stdin),
                    SupervisedProcessPolicy {
                        deadline: Some(deadline),
                        stdout: SupervisedStreamCapture::Head {
                            limit_bytes: MAX_RESPONSE,
                            reject_excess: true,
                        },
                        stderr: SupervisedStreamCapture::Tail { limit_bytes: 64 * 1024 },
                        ..Default::default()
                    },
                    cancellation,
                )
                .map_err(|error| {
                    if let SupervisedProcessError::Cleanup { cleanup, .. } = &error {
                        evidence.cleanup = Some(cleanup.as_ref().clone());
                    }
                    anyhow::Error::new(error)
                })?;
                evidence.exit_code = output.status.code();
                evidence.cleanup = Some(output.cleanup);
                evidence.stdout = output.stdout;
                evidence.stderr = output.stderr;
                evidence.stderr_truncated = output.stderr_truncated;
                anyhow::ensure!(
                    output.status.success() && !output.stdout_truncated,
                    "PSE provider failed or truncated stdout"
                );
                let parsed: RegulatoryPseOutput = serde_json::from_slice(&evidence.stdout)?;
                evidence.output = Some(parsed);
                let parsed =
                    evidence.output.as_ref().ok_or_else(|| anyhow::anyhow!("PSE output absent"))?;
                anyhow::ensure!(
                    parsed.analysis.verifies(
                        &self.config.approval,
                        &request.request_nonce,
                        artifact
                    ),
                    "PSE approval, invocation, profile or exact artifact coverage mismatch"
                );
                anyhow::ensure!(
                    !parsed.native_report.is_empty()
                        && <[u8; 32]>::from(Sha256::digest(&parsed.native_report))
                            == parsed.analysis.native_report_sha256,
                    "PSE native report identity mismatch"
                );
                anyhow::ensure!(
                    crate::artifact_verifier::hash_published_artifact_until(
                        snapshot_path,
                        maximum_artifact_bytes,
                        cancellation,
                        deadline,
                    )? == expected_identity
                        && hash_file(&mut source, maximum_artifact_bytes, deadline, cancellation)?
                            == (artifact.artifact_bytes, artifact.artifact_sha256),
                    "PSE final artifact changed during analysis"
                );
                anyhow::ensure!(
                    evidence
                        .cleanup
                        .as_ref()
                        .is_some_and(SupervisedProcessCleanupReceipt::all_resources_released),
                    "PSE native ownership did not close cleanly"
                );
                Ok(())
            },
        ));
        let result = match result {
            Ok(result) => result,
            Err(payload) => {
                if payload.is::<String>() || payload.is::<&'static str>() {
                    drop(payload);
                } else {
                    std::mem::forget(payload);
                }
                Err(RegulatoryPseTerminalFailure::Panicked.into())
            }
        };
        if let Some(snapshot) = snapshot
            && let Err(error) = snapshot.close()
        {
            evidence.snapshot_cleanup_error = Some(error.to_string());
        }
        if let Err(error) = self.native.close(deadline) {
            evidence.runtime_cleanup_error = Some(error.to_string());
        }
        drop(self.leases);
        let result = result.and_then(|()| {
            check_control(deadline, cancellation)?;
            anyhow::ensure!(
                evidence.snapshot_cleanup_error.is_none(),
                "PSE snapshot cleanup failed"
            );
            anyhow::ensure!(
                evidence.runtime_cleanup_error.is_none(),
                "PSE runtime owner cleanup failed"
            );
            Ok(())
        });
        result.map_err(|cause| {
            evidence.terminal_failure = Some(
                if let Some(control) = cause.downcast_ref::<RegulatoryPseTerminalFailure>() {
                    *control
                } else if let Some(control) = cause.chain().find_map(|error| {
                    match error.downcast_ref::<crate::artifact_verifier::IndependentExportArtifactVerificationError>() {
                        Some(crate::artifact_verifier::IndependentExportArtifactVerificationError::DeadlineExceeded) => Some(RegulatoryPseTerminalFailure::DeadlineExceeded),
                        Some(crate::artifact_verifier::IndependentExportArtifactVerificationError::Cancelled) => Some(RegulatoryPseTerminalFailure::Canceled),
                        _ => None,
                    }
                }) {
                    control
                } else if let Some(process) = cause.downcast_ref::<SupervisedProcessError>() {
                    if process.is_canceled() {
                        RegulatoryPseTerminalFailure::Canceled
                    } else if process.is_deadline_exceeded() {
                        RegulatoryPseTerminalFailure::DeadlineExceeded
                    } else {
                        RegulatoryPseTerminalFailure::Rejected
                    }
                } else {
                    RegulatoryPseTerminalFailure::Rejected
                },
            );
            RegulatoryPseFailure { cause, evidence: Box::new(evidence.clone()) }
        })?;
        Ok(RegulatoryPseExecutionReceipt { evidence })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;
    fn approval() -> RegulatoryPseApproval {
        RegulatoryPseApproval {
            schema_version: 1,
            approval_authority: "SYNTHETIC TEST ONLY".to_owned(),
            approval_id: "protocol-only".to_owned(),
            approval_document_sha256: [1; 32],
            standard_edition: "ITU-R BT.1702-3 (11/2023)".to_owned(),
            provider_id: "fixture".to_owned(),
            provider_version: "1".to_owned(),
            executable_sha256: [2; 32],
            approved_profile_sha256: [3; 32],
            qc_profile_fingerprint: [4; 32],
        }
    }
    #[test]
    fn missing_or_unapproved_provider_never_spawns_and_control_is_typed() {
        let token = ExecutionCancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(matches!(
            admit_regulatory_pse_provider(None, [4; 32], deadline, &token),
            Ok(RegulatoryPseAdmission::NotRun(
                RegulatoryPseNotRun::ProviderMissing
            ))
        ));
        let work = tempfile::tempdir().expect("work");
        let config = RegulatoryPseProviderConfig {
            runtime_files: None,
            executable: work.path().join("absent.exe"),
            approval_document: work.path().join("absent-approval.pdf"),
            approved_profile: work.path().join("absent-profile"),
            approval: approval(),
        };
        assert!(matches!(
            admit_regulatory_pse_provider(Some(&config), [5; 32], deadline, &token),
            Ok(RegulatoryPseAdmission::NotRun(
                RegulatoryPseNotRun::ApprovalMissingOrMismatched
            ))
        ));
        assert!(matches!(
            admit_regulatory_pse_provider(Some(&config), [4; 32], deadline, &token),
            Ok(RegulatoryPseAdmission::NotRun(_))
        ));
        let mut input = tempfile::tempfile().expect("file");
        input.write_all(b"data").expect("write");
        let error = hash_file(&mut input, 4, Instant::now(), &token).expect_err("expired");
        assert_eq!(
            error.downcast_ref::<RegulatoryPseTerminalFailure>(),
            Some(&RegulatoryPseTerminalFailure::DeadlineExceeded)
        );
        token.cancel();
        let error = hash_file(&mut input, 4, deadline, &token).expect_err("canceled");
        assert_eq!(
            error.downcast_ref::<RegulatoryPseTerminalFailure>(),
            Some(&RegulatoryPseTerminalFailure::Canceled)
        );
    }
    #[test]
    fn bounded_hash_refuses_empty_and_one_byte_overflow() {
        let token = ExecutionCancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut file = tempfile::tempfile().expect("file");
        assert!(hash_file(&mut file, 4, deadline, &token).is_err());
        file.write_all(b"12345").expect("write");
        assert!(hash_file(&mut file, 4, deadline, &token).is_err());
        assert_eq!(
            hash_file(&mut file, 5, deadline, &token).expect("exact bound").0,
            5
        );
    }
    #[cfg(windows)]
    #[test]
    fn admitted_installation_locks_actual_files_and_rejects_hash_substitution() {
        let work = tempfile::tempdir().expect("work");
        let paths = [
            work.path().join("provider.exe"),
            work.path().join("approval.pdf"),
            work.path().join("profile.dat"),
        ];
        for path in &paths {
            std::fs::write(path, b"SYNTHETIC IDENTITY TEST ONLY").expect("fixture bytes");
        }
        let hash: [u8; 32] = Sha256::digest(b"SYNTHETIC IDENTITY TEST ONLY").into();
        let mut approval = approval();
        approval.executable_sha256 = hash;
        approval.approval_document_sha256 = hash;
        approval.approved_profile_sha256 = hash;
        let config = RegulatoryPseProviderConfig {
            runtime_files: None,
            executable: paths[0].clone(),
            approval_document: paths[1].clone(),
            approved_profile: paths[2].clone(),
            approval,
        };
        let token = ExecutionCancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        let prepared = admit_regulatory_pse_provider(Some(&config), [4; 32], deadline, &token)
            .expect("identity fixture");
        assert!(matches!(prepared, RegulatoryPseAdmission::Available(_)));
        for path in &paths {
            assert!(std::fs::write(path, b"replace").is_err());
            assert!(std::fs::remove_file(path).is_err());
        }
        drop(prepared);
        std::fs::write(&paths[0], b"replaced").expect("leases released");
        assert!(admit_regulatory_pse_provider(Some(&config), [4; 32], deadline, &token).is_err());
    }
}
#[cfg(all(test, windows))]
mod native_protocol_tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;
    #[test]
    fn real_native_invalid_protocol_exit_retains_request_output_and_consumed_cleanup() {
        run_native_invalid_protocol(false);
    }

    #[test]
    fn real_owned_publication_provider_retains_writer_authority_through_snapshot_and_native_work() {
        run_native_invalid_protocol(true);
    }

    fn run_native_invalid_protocol(use_owned: bool) {
        let work = tempfile::tempdir().expect("work");
        let encoded = work.path().join("synthetic-artifact.bin");
        let control = work.path().join("synthetic-approval-profile.dat");
        let owner = if use_owned {
            let mut owner = mondrian_storage::OwnedPublicationFile::create_sibling(
                &encoded,
                "pse-authority-fixture",
            )
            .expect("real publication writer owner");
            owner
                .file_mut()
                .expect("writer")
                .write_all(b"synthetic final bytes")
                .expect("artifact");
            owner.file_mut().expect("writer").flush().expect("artifact flush");
            assert!(
                OpenOptions::new().write(true).open(owner.path()).is_err(),
                "external writer must be denied"
            );
            assert!(
                open_identity_lease(owner.path()).is_err(),
                "strict path reopen conflicts with retained legitimate writer"
            );
            Some(owner)
        } else {
            std::fs::write(&encoded, b"synthetic final bytes").expect("artifact");
            None
        };
        let encoded = owner.as_ref().map(|owner| owner.path().to_path_buf()).unwrap_or(encoded);
        std::fs::write(&control, b"SYNTHETIC PROTOCOL TEST ONLY").expect("control");
        let token = ExecutionCancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        let executable = std::env::current_exe().expect("fixture executable");
        let (_, executable_sha256) = hash_file(
            &mut File::open(&executable).expect("exe"),
            MAX_EXECUTABLE,
            deadline,
            &token,
        )
        .expect("executable identity");
        let (artifact_bytes, artifact_sha256) = hash_file(
            &mut File::open(&encoded).expect("artifact"),
            1024,
            deadline,
            &token,
        )
        .expect("artifact hash");
        let profile = mondrian_broadcast::BroadcastQcProfile {
            id: "synthetic-native-protocol".to_owned(),
            edition: "1".to_owned(),
            source_sha256: [1; 32],
            signal_color_space: mondrian_core::ColorSpace::Rec709,
            observation_tap:
                mondrian_broadcast::BroadcastQcObservationTap::DeliveryPictureAfterLegalizer,
            active_picture: mondrian_broadcast::QcActivePicture::full(1, 1),
            rules: vec![mondrian_broadcast::BroadcastQcRule::LumaFlashCandidate {
                rule_id: "triage".to_owned(),
                minimum_mean_luma_delta: 0.5,
                severity: mondrian_broadcast::BroadcastQcSeverity::Info,
            }],
            maximum_retained_findings: 1,
            require_regulatory_flash_analysis: true,
            require_encoded_artifact_revalidation: true,
        };
        let mut scan =
            mondrian_broadcast::BroadcastArtifactQcSession::new(profile, 1, 12).expect("scan");
        scan.push(&[0; 12]).expect("frame");
        let artifact = scan.finish(artifact_sha256, artifact_bytes, true).expect("receipt");
        let hash: [u8; 32] = Sha256::digest(b"SYNTHETIC PROTOCOL TEST ONLY").into();
        let approval = RegulatoryPseApproval {
            schema_version: 1,
            approval_authority: "SYNTHETIC TEST ONLY".to_owned(),
            approval_id: "not-certified".to_owned(),
            approval_document_sha256: hash,
            standard_edition: "ITU-R BT.1702-3 (11/2023)".to_owned(),
            provider_id: "test-harness-rejecting-unknown-flag".to_owned(),
            provider_version: "1".to_owned(),
            executable_sha256,
            approved_profile_sha256: hash,
            qc_profile_fingerprint: artifact.scan.profile_fingerprint,
        };
        let config = RegulatoryPseProviderConfig {
            runtime_files: None,
            executable,
            approval_document: control.clone(),
            approved_profile: control,
            approval,
        };
        let RegulatoryPseAdmission::Available(prepared) = admit_regulatory_pse_provider(
            Some(&config),
            artifact.scan.profile_fingerprint,
            deadline,
            &token,
        )
        .expect("admission") else {
            panic!("Windows identity fixture must be admitted")
        };
        let result = match owner.as_ref() {
            Some(owner) => {
                prepared.verify_owned_final_artifact(owner, &artifact, 1024, deadline, &token)
            }
            None => prepared.verify_final_artifact(&encoded, &artifact, 1024, deadline, &token),
        };
        let failure = result.expect_err("test harness is not a PSE provider");
        if let Some(owner) = owner.as_ref() {
            assert!(
                OpenOptions::new().write(true).open(owner.path()).is_err(),
                "consuming provider must not release publication authority"
            );
        }
        assert!(failure.evidence.request.is_some());
        assert!(failure
            .evidence
            .cleanup
            .as_ref()
            .is_some_and(SupervisedProcessCleanupReceipt::all_resources_released));
        assert_eq!(
            failure.evidence.terminal_failure,
            Some(RegulatoryPseTerminalFailure::Rejected)
        );
        assert!(failure.evidence.snapshot_cleanup_error.is_none());
        assert!(!failure.evidence.request.as_ref().expect("request").artifact_path.exists());
        assert!(failure.evidence.output.is_none());
    }
}
