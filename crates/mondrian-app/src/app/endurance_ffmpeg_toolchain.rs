//! Machine-plan-bound FFmpeg executable ownership for endurance qualification.

use std::sync::Arc;

use mondrian_media::{
    install_process_ffmpeg_toolchain, PreparedFfmpegToolchain,
    QualifiedFfmpegRuntimeFileExpectation, QualifiedFfmpegRuntimeFileReceipt,
    QualifiedFfmpegToolExpectation, QualifiedFfmpegToolReceipt, QualifiedFfmpegToolchainError,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::endurance_machine_plan::{
    EnduranceMachineToolPlan, PreparedCommercialEnduranceMachinePlan,
};

/// Exact machine-plan-bound FFmpeg/ffprobe receipt retained by phase factories.
#[derive(Debug)]
pub struct PreparedEnduranceFfmpegToolchain {
    machine_plan_sha256: String,
    toolchain_receipt_sha256: String,
    prepared: Arc<PreparedFfmpegToolchain>,
}

impl PreparedEnduranceFfmpegToolchain {
    /// Prepare, verify, and process-install the machine plan's exact tool pair.
    ///
    /// Installation is process-wide and immutable because Preview, audio,
    /// Export encoding, ffprobe validation, and independent full decode all
    /// enter through the shared media command constructors. Reinstalling the
    /// same receipt is idempotent; a different identity fails closed.
    pub fn prepare_and_install(
        machine_plan: &PreparedCommercialEnduranceMachinePlan,
    ) -> Result<Self, EnduranceFfmpegToolchainError> {
        let tools = &machine_plan.plan().verifier_tools;
        let runtime_files = tools
            .runtime_files
            .iter()
            .map(|binding| QualifiedFfmpegRuntimeFileExpectation {
                path: &binding.path,
                sha256: &binding.sha256,
            })
            .collect::<Vec<_>>();
        let prepared = PreparedFfmpegToolchain::prepare(
            expectation(&tools.ffmpeg),
            expectation(&tools.ffprobe),
            &runtime_files,
        )?;
        let prepared = install_process_ffmpeg_toolchain(prepared)?;
        let toolchain_receipt_sha256 = receipt_sha256(machine_plan.sha256(), &prepared)?;
        Ok(Self {
            machine_plan_sha256: machine_plan.sha256().to_owned(),
            toolchain_receipt_sha256,
            prepared,
        })
    }

    /// Exact machine-plan digest that authorized this pair.
    pub fn machine_plan_sha256(&self) -> &str {
        &self.machine_plan_sha256
    }

    /// Canonical digest of the plan binding and all observed executable/runtime receipts.
    pub fn toolchain_receipt_sha256(&self) -> &str {
        &self.toolchain_receipt_sha256
    }

    /// Exact retained FFmpeg identity.
    pub fn ffmpeg_receipt(&self) -> &QualifiedFfmpegToolReceipt {
        self.prepared.ffmpeg_receipt()
    }

    /// Exact retained ffprobe identity.
    pub fn ffprobe_receipt(&self) -> &QualifiedFfmpegToolReceipt {
        self.prepared.ffprobe_receipt()
    }

    /// Complete ordered packaged runtime-DLL closure used by the CLI capsule.
    pub fn runtime_file_receipts(
        &self,
    ) -> impl ExactSizeIterator<Item = &QualifiedFfmpegRuntimeFileReceipt> {
        self.prepared.runtime_file_receipts()
    }

    /// Revalidate that a later factory still borrows the authorizing plan.
    pub fn validate_machine_plan(
        &self,
        machine_plan: &PreparedCommercialEnduranceMachinePlan,
    ) -> Result<(), EnduranceFfmpegToolchainError> {
        self.prepared.validate_current()?;
        if self.machine_plan_sha256 != machine_plan.sha256()
            || !receipt_matches_plan(
                self.prepared.ffmpeg_receipt(),
                &machine_plan.plan().verifier_tools.ffmpeg,
                mondrian_media::QualifiedFfmpegToolKind::Ffmpeg,
            )
            || !receipt_matches_plan(
                self.prepared.ffprobe_receipt(),
                &machine_plan.plan().verifier_tools.ffprobe,
                mondrian_media::QualifiedFfmpegToolKind::Ffprobe,
            )
            || !self
                .prepared
                .runtime_file_receipts()
                .zip(&machine_plan.plan().verifier_tools.runtime_files)
                .all(|(receipt, binding)| {
                    receipt.source_path == binding.path && receipt.sha256 == binding.sha256
                })
            || self.prepared.runtime_file_receipts().len()
                != machine_plan.plan().verifier_tools.runtime_files.len()
        {
            return Err(EnduranceFfmpegToolchainError::MachinePlanChanged);
        }
        Ok(())
    }
}

fn receipt_sha256(
    machine_plan_sha256: &str,
    prepared: &PreparedFfmpegToolchain,
) -> Result<String, EnduranceFfmpegToolchainError> {
    receipt_sha256_from_parts(
        machine_plan_sha256,
        prepared.ffmpeg_receipt(),
        prepared.ffprobe_receipt(),
        prepared.runtime_file_receipts(),
    )
}

fn receipt_sha256_from_parts<'a>(
    machine_plan_sha256: &str,
    ffmpeg: &QualifiedFfmpegToolReceipt,
    ffprobe: &QualifiedFfmpegToolReceipt,
    runtime_files: impl ExactSizeIterator<Item = &'a QualifiedFfmpegRuntimeFileReceipt>,
) -> Result<String, EnduranceFfmpegToolchainError> {
    let mut digest = Sha256::new();
    digest.update(b"mondrian/endurance-ffmpeg-toolchain-receipt/v1\0");
    update_frame(&mut digest, machine_plan_sha256.as_bytes());
    for (role, receipt) in [
        (b"ffmpeg".as_slice(), ffmpeg),
        (b"ffprobe".as_slice(), ffprobe),
    ] {
        update_frame(&mut digest, role);
        update_path_frame(&mut digest, &receipt.source_path)?;
        update_frame(&mut digest, receipt.executable_sha256.as_bytes());
        update_frame(&mut digest, receipt.snapshot_sha256.as_bytes());
        update_frame(&mut digest, receipt.version_output_sha256.as_bytes());
        update_frame(&mut digest, receipt.capability_report_sha256.as_bytes());
    }
    digest.update(u64::try_from(runtime_files.len()).unwrap_or(u64::MAX).to_be_bytes());
    for receipt in runtime_files {
        update_frame(&mut digest, b"runtime-file");
        update_path_frame(&mut digest, &receipt.source_path)?;
        digest.update(receipt.byte_length.to_be_bytes());
        update_frame(&mut digest, receipt.sha256.as_bytes());
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn update_path_frame(
    digest: &mut Sha256,
    path: &std::path::Path,
) -> Result<(), EnduranceFfmpegToolchainError> {
    let text = path.to_str().ok_or(EnduranceFfmpegToolchainError::NonUtf8ReceiptPath)?;
    update_frame(digest, text.as_bytes());
    Ok(())
}

fn update_frame(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

fn expectation(tool: &EnduranceMachineToolPlan) -> QualifiedFfmpegToolExpectation<'_> {
    QualifiedFfmpegToolExpectation {
        executable_path: &tool.executable.path,
        executable_sha256: &tool.executable.sha256,
        version_output_sha256: &tool.version_output_sha256,
        capability_report_sha256: &tool.capability_report_sha256,
    }
}

fn receipt_matches_plan(
    receipt: &QualifiedFfmpegToolReceipt,
    plan: &EnduranceMachineToolPlan,
    expected_kind: mondrian_media::QualifiedFfmpegToolKind,
) -> bool {
    receipt.kind == expected_kind
        && receipt.source_path == plan.executable.path
        && receipt.executable_sha256 == plan.executable.sha256
        && receipt.snapshot_sha256 == plan.executable.sha256
        && receipt.version_output_sha256 == plan.version_output_sha256
        && receipt.capability_report_sha256 == plan.capability_report_sha256
}

/// Exact executable preparation or later machine-plan revalidation failure.
#[derive(Debug, Error)]
pub enum EnduranceFfmpegToolchainError {
    /// Media-owned executable preparation or process installation failed.
    #[error(transparent)]
    Toolchain(#[from] QualifiedFfmpegToolchainError),
    /// A factory attempted to reuse this receipt with a different machine plan.
    #[error("prepared FFmpeg toolchain no longer matches the exact machine plan")]
    MachinePlanChanged,
    /// A receipt path could not enter the stable UTF-8 evidence encoding.
    #[error("prepared FFmpeg toolchain receipt contains a non-UTF-8 path")]
    NonUtf8ReceiptPath,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_media::QualifiedFfmpegToolKind;
    use std::path::PathBuf;

    fn receipt(kind: QualifiedFfmpegToolKind) -> QualifiedFfmpegToolReceipt {
        QualifiedFfmpegToolReceipt {
            kind,
            source_path: PathBuf::from(r"C:\qualified\ffmpeg.exe"),
            executable_sha256: "a".repeat(64),
            snapshot_sha256: "a".repeat(64),
            version_output_sha256: "b".repeat(64),
            capability_report_sha256: "c".repeat(64),
        }
    }

    #[test]
    fn exact_receipt_requires_every_machine_plan_identity() {
        let plan = EnduranceMachineToolPlan {
            executable: super::super::endurance_machine_plan::EnduranceMachineFileBinding {
                path: PathBuf::from(r"C:\qualified\ffmpeg.exe"),
                sha256: "a".repeat(64),
            },
            version_output_sha256: "b".repeat(64),
            capability_report_sha256: "c".repeat(64),
        };
        let exact = receipt(QualifiedFfmpegToolKind::Ffmpeg);
        assert!(receipt_matches_plan(
            &exact,
            &plan,
            QualifiedFfmpegToolKind::Ffmpeg
        ));

        let mut changed = plan.clone();
        changed.capability_report_sha256 = "d".repeat(64);
        assert!(!receipt_matches_plan(
            &exact,
            &changed,
            QualifiedFfmpegToolKind::Ffmpeg
        ));
        assert!(!receipt_matches_plan(
            &exact,
            &plan,
            QualifiedFfmpegToolKind::Ffprobe
        ));
    }

    #[test]
    fn receipt_digest_has_a_stable_framed_external_encoding() {
        let ffmpeg = receipt(QualifiedFfmpegToolKind::Ffmpeg);
        let mut ffprobe = receipt(QualifiedFfmpegToolKind::Ffprobe);
        ffprobe.source_path = PathBuf::from(r"C:\qualified\ffprobe.exe");
        let runtime = [QualifiedFfmpegRuntimeFileReceipt {
            source_path: PathBuf::from(r"C:\qualified\avcodec-61.dll"),
            sha256: "d".repeat(64),
            byte_length: 123,
        }];
        let digest = receipt_sha256_from_parts(&"e".repeat(64), &ffmpeg, &ffprobe, runtime.iter())
            .expect("encode canonical receipt");
        assert_eq!(
            digest,
            "5e90a65d1e11cb4cab69f59d6a4c18bfbb9dbd1360ca690abe9733cd0a5a837a"
        );
    }
}
