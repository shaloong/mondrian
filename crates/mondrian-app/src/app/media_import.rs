use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use mondrian_assets::{AssetKind, AssetLibrary};
use mondrian_core::events::AppEvent;
use mondrian_core::types::AssetId;
use mondrian_core::{MondrianError, Result};

use super::proxy_generation::{request_proxy_generation, resolve_app_state_proxy_color_contract};
use super::AppState;

pub(super) struct MediaImportResult {
    batch_id: u64,
    path: PathBuf,
    result: Result<AssetId>,
}

pub(super) struct PendingMediaImportBatch {
    total: usize,
    completed: usize,
    imported: usize,
    proxy_started: usize,
    failures: Vec<String>,
}

impl PendingMediaImportBatch {
    fn new(total: usize) -> Self {
        Self {
            total,
            completed: 0,
            imported: 0,
            proxy_started: 0,
            failures: Vec::new(),
        }
    }
}

impl AppState {
    /// Queue a media-import batch without probing media on the caller thread.
    pub(crate) fn start_media_import_batch(
        &mut self,
        paths: Vec<PathBuf>,
        folder_id: Option<String>,
    ) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }

        let library = self.asset_library.clone().ok_or_else(|| {
            let reason = "素材库未连接".to_string();
            self.set_status_hint(format!("导入失败：{reason}"), true);
            MondrianError::WorkflowStepFailed { step_id: "import_media".to_string(), reason }
        })?;

        if let Some(folder_id) = folder_id.as_deref() {
            if !library.folder_exists(folder_id)? {
                let reason = format!("目标素材文件夹不存在：{folder_id}");
                self.set_status_hint(format!("导入失败：{reason}"), true);
                return Err(MondrianError::WorkflowStepFailed {
                    step_id: "import_media".to_string(),
                    reason,
                });
            }
        }

        let batch_id = self.next_media_import_batch_id;
        self.next_media_import_batch_id = self.next_media_import_batch_id.saturating_add(1).max(1);
        let total = paths.len();
        let result_tx = self.media_import_tx.clone();
        let worker_folder_id = folder_id.clone();
        let spawn_result =
            thread::Builder::new().name("mondrian-media-import".to_owned()).spawn(move || {
                media_import_worker(batch_id, library, paths, worker_folder_id, result_tx)
            });

        if let Err(err) = spawn_result {
            let reason = format!("无法启动媒体导入后台任务：{err}");
            self.set_status_hint(format!("导入失败：{reason}"), true);
            return Err(MondrianError::WorkflowStepFailed {
                step_id: "import_media".to_string(),
                reason,
            });
        }

        self.media_import_batches.insert(batch_id, PendingMediaImportBatch::new(total));
        self.set_status_hint(format!("正在导入 {total} 个媒体文件..."), false);
        Ok(())
    }

    /// Drain completed background media imports into app state.
    ///
    /// Returns true when status, asset-library presentation, or project state
    /// may have changed and UI models should refresh.
    pub fn poll_media_imports(&mut self) -> bool {
        let mut changed = false;
        while let Ok(result) = self.media_import_rx.try_recv() {
            changed = true;
            self.apply_media_import_result(result);
        }
        changed
    }

    #[cfg(test)]
    /// Number of import batches still waiting for background completion.
    pub(crate) fn pending_media_import_batches(&self) -> usize {
        self.media_import_batches.len()
    }

    fn apply_media_import_result(&mut self, result: MediaImportResult) {
        let Some(mut batch) = self.media_import_batches.remove(&result.batch_id) else {
            return;
        };

        batch.completed = batch.completed.saturating_add(1);
        match result.result {
            Ok(asset_id) => {
                batch.imported = batch.imported.saturating_add(1);
                if self.configure_imported_asset(asset_id) {
                    batch.proxy_started = batch.proxy_started.saturating_add(1);
                }
                self.event_bus.publish(AppEvent::AssetImported { asset_id });
            }
            Err(err) => {
                batch.failures.push(format!("{}: {err}", result.path.display()));
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
        let Some(library) = self.asset_library.as_ref() else {
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
        let proxy_color = match resolve_app_state_proxy_color_contract(self, &asset) {
            Ok(contract) => contract,
            Err(err) => {
                tracing::warn!(
                    target: "mondrian::proxy",
                    asset_id = %asset_id,
                    path = %asset.path.display(),
                    "automatic proxy generation rejected: {err}"
                );
                self.set_asset_proxy_mode(asset_id, false);
                return false;
            }
        };
        self.set_asset_proxy_mode(asset_id, true);
        request_proxy_generation(asset_id, asset.path, self.proxy_config(), proxy_color);
        true
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
            let _ = self.save_project_file();
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

fn media_import_worker(
    batch_id: u64,
    library: Arc<AssetLibrary>,
    paths: Vec<PathBuf>,
    folder_id: Option<String>,
    result_tx: std::sync::mpsc::Sender<MediaImportResult>,
) {
    for path in paths {
        let result = import_media_path(&library, &path, folder_id.as_deref());
        if result_tx.send(MediaImportResult { batch_id, path, result }).is_err() {
            return;
        }
    }
}

fn import_media_path(
    library: &AssetLibrary,
    path: &std::path::Path,
    folder_id: Option<&str>,
) -> Result<AssetId> {
    let asset_id = library.import_media_file(path)?;
    library.move_asset_to_folder(asset_id, folder_id)?;
    Ok(asset_id)
}
