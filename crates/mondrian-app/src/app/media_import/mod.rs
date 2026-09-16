//! Instance-owned bounded media-import Module.
//!
//! The public Interface is deliberately small: bounded batch control and
//! immutable diagnostics. Execution owns scheduling and terminal truth, the
//! physical-media Adapter owns probing and the serialized Asset Library
//! commit, and `AppState` owns presentation plus product follow-up actions.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mondrian_assets::AssetKind;
use mondrian_core::events::AppEvent;
use mondrian_core::types::AssetId;
use mondrian_core::{
    ExecutionTerminalDisposition, ExecutionTerminalEvidence, MondrianError, Result,
};

use super::proxy_generation::{
    resolve_app_state_proxy_color_contract, ProxyGenerationOrigin, ProxyGenerationRequestOutcome,
};
use super::AppState;

mod asset_adapter;
mod execution;

pub(super) use execution::MediaImportExecution;
use execution::{MediaImportPublication, MediaImportPublicationOutcome};

const MEDIA_IMPORT_MAX_RESULTS_PER_POLL: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaImportFailureReason {
    /// The bounded batch or file capacity rejected work before execution.
    AdmissionRejected,
    /// The packaged Probe Helper executable was unavailable.
    ProbeWorkerUnavailable,
    /// Physical source canonicalization, fingerprinting, probing, or protocol
    /// validation failed before Asset Library publication.
    ProbeFailed,
    /// The isolated Probe Helper exceeded its admitted monotonic deadline.
    ProbeDeadlineExceeded,
    /// Transactional Asset Library commit failed after successful preparation.
    ImportFailed,
    /// Cancellation was observed before the file import committed.
    Canceled,
}

/// Stable identity of one admitted media-import batch.
///
/// The identity is local to the running application instance. Callers may
/// obtain active identities from [`MediaImportDiagnostics`] and use them to
/// request explicit cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MediaImportBatchId(u64);

impl MediaImportBatchId {
    /// Return the instance-local numeric identity for diagnostics.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Bounded terminal evidence for one media-import file or rejected batch.
#[derive(Debug, Clone)]
pub struct MediaImportTerminalRecord {
    /// Shared generation, priority, and terminal disposition.
    pub evidence: ExecutionTerminalEvidence,
    /// Instance-local batch identity.
    pub batch_id: u64,
    /// Source path, absent only for a batch-level admission rejection.
    pub path: Option<PathBuf>,
    /// Worker execution duration; pre-dispatch terminals use zero.
    pub elapsed: Duration,
    /// Stable domain failure category, when applicable.
    pub failure: Option<MediaImportFailureReason>,
}

/// Immutable bounded Media Import execution and terminal evidence.
#[derive(Debug, Clone, Default)]
pub struct MediaImportDiagnostics {
    /// Wrapping dirty token for every execution-diagnostics change, including
    /// resource policy and worker phase. Only equality comparison is meaningful.
    pub diagnostics_revision: u64,
    /// Wrapping dirty token for product-visible batch, publication, counter,
    /// and terminal-record changes. Resource policy alone never advances it.
    pub model_revision: u64,
    /// Current Project execution generation.
    pub generation: u64,
    /// Whether queued files may enter a worker.
    pub dispatch_enabled: bool,
    /// Product-requested running-file limit.
    pub dispatch_parallelism: usize,
    /// Workers configured for this service instance.
    pub requested_workers: usize,
    /// Worker threads successfully created.
    pub started_workers: usize,
    /// Whether a worker returned before service shutdown was requested.
    pub worker_unexpectedly_exited: bool,
    /// Admitted current-generation batches.
    pub active_batches: usize,
    /// Stable identities of admitted current-generation batches.
    pub active_batch_ids: Vec<MediaImportBatchId>,
    /// Files waiting behind the worker dispatch Seam.
    pub queued_files: usize,
    /// Files currently inside the canonicalize/fingerprint/probe Adapter.
    pub running_files: usize,
    /// Current-generation admitted files without a drained terminal result.
    pub outstanding_files: usize,
    /// Total worker/result transport occupancy including superseded
    /// generations that can no longer publish into the current Project.
    pub transport_occupied_files: usize,
    /// Successfully admitted batches.
    pub admissions: u64,
    /// Rejected batch admissions.
    pub rejections: u64,
    /// Current-generation committed file imports.
    pub imported_files: u64,
    /// Current-generation file failures.
    pub failed_files: u64,
    /// Physical preparation failures, including results not yet polled by AppState.
    pub preparation_failures: u64,
    /// Asset Library publication failures after successful preparation.
    pub publication_failures: u64,
    /// Files canceled before a committed import.
    pub canceled_files: u64,
    /// Late results made ineligible by Project generation rotation.
    pub superseded_files: u64,
    /// Bounded terminal evidence in publication order.
    pub terminal_records: Vec<MediaImportTerminalRecord>,
}

pub(super) struct PendingMediaImportBatch {
    generation: u64,
    total: usize,
    completed: usize,
    imported: usize,
    proxy_started: usize,
    failures: Vec<String>,
}

impl PendingMediaImportBatch {
    fn new(generation: u64, total: usize) -> Self {
        Self {
            generation,
            total,
            completed: 0,
            imported: 0,
            proxy_started: 0,
            failures: Vec::new(),
        }
    }
}

impl AppState {
    /// Queue a bounded media-import batch without probing media on the caller thread.
    pub(crate) fn start_media_import_batch(
        &mut self,
        paths: Vec<PathBuf>,
        folder_id: Option<String>,
    ) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }

        let library = self.asset_library_handle().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("导入失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "import_media".to_string(), reason }
        })?;

        if let Some(folder_id) = folder_id.as_deref()
            && !library.folder_exists(folder_id)?
        {
            let reason = format!("目标素材文件夹不存在：{folder_id}");
            self.set_status_hint(format!("导入失败：{reason}"), true);
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "import_media".to_string(),
                reason,
            });
        }

        let admission = self.media_import.admit_batch(paths, folder_id).map_err(|error| {
            let reason = error.to_string();
            self.set_status_hint(format!("导入失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "import_media".to_string(), reason }
        })?;
        self.media_import_batches.insert(
            admission.batch_id,
            PendingMediaImportBatch::new(admission.generation, admission.total),
        );
        let _ = self.refresh_internal_execution_resource_decision();
        self.set_status_hint(format!("正在导入 {} 个媒体文件...", admission.total), false);
        Ok(())
    }

    /// Drain bounded current-generation import results into author presentation.
    pub fn poll_media_imports(&mut self) -> bool {
        let library = self.asset_library_handle();
        let results = self
            .media_import
            .poll_results(library.as_deref(), MEDIA_IMPORT_MAX_RESULTS_PER_POLL);
        let mut changed = self.media_import.poll_model_changed();
        for result in results {
            changed = true;
            self.apply_media_import_result(result);
        }
        if changed {
            let _ = self.refresh_internal_execution_resource_decision();
        }
        changed
    }

    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn pending_media_import_batches(&self) -> usize {
        self.media_import_batches.len()
    }

    /// Snapshot bounded Media Import execution and terminal evidence.
    pub fn media_import_diagnostics(&self) -> MediaImportDiagnostics {
        self.media_import.diagnostics()
    }

    /// Cancel one admitted media-import batch.
    ///
    /// Queued probes terminate immediately. A probe already inside FFmpeg may
    /// finish preparing its immutable candidate, but the serialized commit
    /// Seam rejects that candidate before any Asset Library write.
    pub fn cancel_media_import_batch(&mut self, batch_id: MediaImportBatchId) -> bool {
        let canceled = self.media_import.cancel_batch(batch_id);
        if canceled {
            self.media_import_batches.remove(&batch_id.get());
            let _ = self.refresh_internal_execution_resource_decision();
            self.set_status_hint("媒体导入已取消".to_owned(), false);
        }
        canceled
    }

    fn apply_media_import_result(&mut self, result: MediaImportPublication) {
        let Some(mut batch) = self.media_import_batches.remove(&result.batch_id) else {
            return;
        };
        if batch.generation != result.generation
            || result.evidence.disposition == ExecutionTerminalDisposition::Superseded
        {
            return;
        }

        batch.completed = batch.completed.saturating_add(1);
        match result.outcome {
            MediaImportPublicationOutcome::Imported(asset_id) => {
                batch.imported = batch.imported.saturating_add(1);
                if self.configure_imported_asset(asset_id) {
                    batch.proxy_started = batch.proxy_started.saturating_add(1);
                }
                self.event_bus.publish(AppEvent::AssetImported { asset_id });
            }
            MediaImportPublicationOutcome::Failed(error) => {
                batch.failures.push(format!("{}: {error}", result.path.display()));
            }
            MediaImportPublicationOutcome::Canceled => {
                batch.failures.push(format!("{}: 导入已取消", result.path.display()));
            }
        }

        if batch.completed < batch.total {
            self.set_status_hint(
                format!("正在导入媒体文件 {}/{}...", batch.completed, batch.total),
                false,
            );
            self.media_import_batches.insert(result.batch_id, batch);
            return;
        }
        self.finish_media_import_batch(batch);
    }

    fn configure_imported_asset(&mut self, asset_id: AssetId) -> bool {
        let Some(library) = self.asset_library() else {
            return false;
        };
        if !self.should_auto_generate_proxy_for_import() {
            self.set_asset_proxy_mode(asset_id, false);
            return false;
        }
        let Ok(Some(asset)) = library.get_asset(asset_id) else {
            self.set_asset_proxy_mode(asset_id, false);
            return false;
        };
        if !matches!(asset.kind, AssetKind::Video) {
            self.set_asset_proxy_mode(asset_id, false);
            return false;
        }
        let Some(source_path) = asset.file_path().map(Path::to_path_buf) else {
            self.set_asset_proxy_mode(asset_id, false);
            return false;
        };
        let proxy_color = match resolve_app_state_proxy_color_contract(self, &asset) {
            Ok(contract) => contract,
            Err(err) => {
                tracing::warn!(
                    target: "mondrian::proxy",
                    asset_id = %asset_id,
                    path = %source_path.display(),
                    "automatic proxy generation rejected: {err}"
                );
                self.set_asset_proxy_mode(asset_id, false);
                return false;
            }
        };
        self.set_asset_proxy_mode(asset_id, true);
        match self.request_proxy_generation(
            asset_id,
            source_path,
            self.proxy_config(),
            proxy_color,
            ProxyGenerationOrigin::Import,
        ) {
            ProxyGenerationRequestOutcome::AlreadyFresh => false,
            ProxyGenerationRequestOutcome::Admitted { .. }
            | ProxyGenerationRequestOutcome::Deduplicated { .. } => true,
            ProxyGenerationRequestOutcome::RetainedFailure(failure)
            | ProxyGenerationRequestOutcome::Failed(failure) => {
                tracing::warn!(
                    target: "mondrian::proxy",
                    asset_id = %asset_id,
                    reason = failure.reason.code(),
                    detail = %failure.detail,
                    "automatic proxy generation was not admitted"
                );
                self.set_asset_proxy_mode(asset_id, false);
                false
            }
        }
    }

    fn finish_media_import_batch(&mut self, batch: PendingMediaImportBatch) {
        if batch.imported > 0 {
            let mut message = format!("已导入 {} 个媒体文件", batch.imported);
            if batch.proxy_started > 0 {
                message.push_str(&format!("，{} 个后台生成代理", batch.proxy_started));
            }
            if !batch.failures.is_empty() {
                message.push_str(&format!("，{} 个失败", batch.failures.len()));
                tracing::warn!(
                    target: "mondrian::action",
                    "media import completed with failures: {}",
                    batch.failures.join("; ")
                );
            }
            self.set_status_hint(message, !batch.failures.is_empty());
            return;
        }

        let reason = batch
            .failures
            .first()
            .cloned()
            .unwrap_or_else(|| "未导入任何媒体文件".to_string());
        self.set_status_hint(format!("导入失败：{reason}"), true);
    }
}
