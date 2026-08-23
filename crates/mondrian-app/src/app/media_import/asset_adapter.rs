//! Physical-media preparation and Asset Library commit adapter.

use std::path::{Path, PathBuf};
use std::time::Instant;

use mondrian_assets::{AssetLibrary, AssetMediaProbeCandidate};
use mondrian_core::types::AssetId;
use mondrian_core::{
    ExecutionCancellationToken, MediaFileFingerprint, MediaProbeSnapshot, MondrianError, Result,
};

use super::execution::{
    MediaImportCommitBackend, MediaImportPreparationBackend, MediaImportPublicationOutcome,
    MediaImportWorkerOutcome,
};
use super::MediaImportFailureReason;

#[derive(Debug)]
pub(super) struct MediaImportPreparedCandidate {
    pub(super) canonical_path: PathBuf,
    pub(super) source_fingerprint: MediaFileFingerprint,
    pub(super) info: MediaProbeSnapshot,
    pub(super) folder_id: Option<String>,
}

pub(super) struct AssetLibraryMediaImportBackend {
    helper_executable: Option<PathBuf>,
}

impl AssetLibraryMediaImportBackend {
    pub(super) fn new() -> Self {
        Self {
            helper_executable: super::super::packaged_worker::discover_media_probe_worker(),
        }
    }
}

impl MediaImportPreparationBackend for AssetLibraryMediaImportBackend {
    fn prepare(
        &self,
        path: &Path,
        folder_id: Option<&str>,
        cancellation: &ExecutionCancellationToken,
    ) -> MediaImportWorkerOutcome {
        if cancellation.is_canceled() {
            return MediaImportWorkerOutcome::Canceled;
        }
        let Some(helper_executable) = self.helper_executable.as_deref() else {
            return MediaImportWorkerOutcome::Failed {
                detail: "未找到与当前产品运行时匹配的媒体 Probe Helper".to_owned(),
                failure: MediaImportFailureReason::ProbeWorkerUnavailable,
            };
        };
        match prepare_media_import_candidate(helper_executable, path, folder_id, cancellation) {
            Ok(_) if cancellation.is_canceled() => MediaImportWorkerOutcome::Canceled,
            Ok(candidate) => MediaImportWorkerOutcome::Prepared(Box::new(candidate)),
            Err(error) if error.is_canceled() => MediaImportWorkerOutcome::Canceled,
            Err(error) => MediaImportWorkerOutcome::Failed {
                failure: if error.is_deadline_exceeded() {
                    MediaImportFailureReason::ProbeDeadlineExceeded
                } else {
                    MediaImportFailureReason::ProbeFailed
                },
                detail: error.to_string(),
            },
        }
    }
}

impl MediaImportCommitBackend for AssetLibraryMediaImportBackend {
    fn commit(
        &self,
        library: &AssetLibrary,
        candidate: MediaImportPreparedCandidate,
    ) -> MediaImportPublicationOutcome {
        match commit_media_import_candidate(library, candidate) {
            Ok(asset_id) => MediaImportPublicationOutcome::Imported(asset_id),
            Err(error) => MediaImportPublicationOutcome::Failed(error.to_string()),
        }
    }
}

fn prepare_media_import_candidate(
    helper_executable: &Path,
    path: &Path,
    folder_id: Option<&str>,
    cancellation: &ExecutionCancellationToken,
) -> std::result::Result<MediaImportPreparedCandidate, mondrian_media::IsolatedMediaProbeError> {
    let prepared = mondrian_media::prepare_media_probe_isolated(
        helper_executable,
        path,
        cancellation,
        Instant::now() + super::super::packaged_worker::MEDIA_PROBE_TIMEOUT,
    )?;
    Ok(MediaImportPreparedCandidate {
        canonical_path: prepared.canonical_path,
        source_fingerprint: prepared.source_fingerprint,
        info: prepared.probe,
        folder_id: folder_id.map(str::to_owned),
    })
}

fn commit_media_import_candidate(
    library: &AssetLibrary,
    candidate: MediaImportPreparedCandidate,
) -> Result<AssetId> {
    let current_fingerprint = MediaFileFingerprint::capture(&candidate.canonical_path);
    if !current_fingerprint.authorizes_reuse()
        || current_fingerprint != candidate.source_fingerprint
    {
        return Err(MondrianError::MediaOpen {
            path: candidate.canonical_path.display().to_string(),
            reason: "媒体文件在导入提交前发生变化，拒绝写入过期元数据".to_owned(),
        });
    }
    let folder_id = candidate.folder_id;
    let candidate = AssetMediaProbeCandidate::new(
        candidate.canonical_path,
        candidate.source_fingerprint,
        candidate.info,
    )?;
    library.commit_media_probe(candidate, folder_id.as_deref())
}
