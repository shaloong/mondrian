//! Physical-media preparation and Asset Library commit adapter.

use std::path::{Path, PathBuf};

use mondrian_assets::{AssetLibrary, AssetMediaProbeCandidate};
use mondrian_core::types::AssetId;
use mondrian_core::{
    ExecutionCancellationToken, MediaFileFingerprint, MediaProbeSnapshot, MondrianError, Result,
};

use super::execution::{
    MediaImportCommitBackend, MediaImportPreparationBackend, MediaImportPublicationOutcome,
    MediaImportWorkerOutcome,
};

#[derive(Debug)]
pub(super) struct MediaImportPreparedCandidate {
    pub(super) canonical_path: PathBuf,
    pub(super) source_fingerprint: MediaFileFingerprint,
    pub(super) info: MediaProbeSnapshot,
    pub(super) folder_id: Option<String>,
}

pub(super) struct AssetLibraryMediaImportBackend;

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
        match prepare_media_import_candidate(path, folder_id) {
            Ok(_) if cancellation.is_canceled() => MediaImportWorkerOutcome::Canceled,
            Ok(candidate) => MediaImportWorkerOutcome::Prepared(Box::new(candidate)),
            Err(error) => MediaImportWorkerOutcome::Failed(error.to_string()),
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
    path: &Path,
    folder_id: Option<&str>,
) -> Result<MediaImportPreparedCandidate> {
    let canonical_path = path.canonicalize().map_err(|error| MondrianError::MediaOpen {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let source_fingerprint = MediaFileFingerprint::capture(&canonical_path);
    let info = mondrian_media::probe_media_info(&canonical_path)?;
    let verified_fingerprint = MediaFileFingerprint::capture(&canonical_path);
    if !source_fingerprint.authorizes_reuse() || source_fingerprint != verified_fingerprint {
        return Err(MondrianError::MediaOpen {
            path: canonical_path.display().to_string(),
            reason: "媒体文件在导入分析期间发生变化，未提交过期元数据".to_owned(),
        });
    }
    Ok(MediaImportPreparedCandidate {
        canonical_path,
        source_fingerprint,
        info,
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
