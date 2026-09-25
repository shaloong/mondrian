//! Machine-local OpenFX Filter installation and startup restoration.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use mondrian_core::{MondrianError, Result};

use crate::openfx_adapter::inspect_openfx_binary;
use crate::openfx_effect::register_selected_openfx_filter;

use super::AppState;

#[derive(Clone, Copy)]
enum CatalogPurpose {
    Install,
    Restore,
}

struct CatalogRequest {
    path: PathBuf,
    purpose: CatalogPurpose,
}

struct CatalogResult {
    path: PathBuf,
    purpose: CatalogPurpose,
    result: std::result::Result<(usize, Vec<String>), String>,
}

pub(super) struct OpenFxCatalog {
    requests: SyncSender<CatalogRequest>,
    results: Receiver<CatalogResult>,
    worker: Option<JoinHandle<()>>,
    canceled: Arc<AtomicBool>,
    restores_remaining: usize,
    restored: usize,
    restore_failures: usize,
}

impl Drop for OpenFxCatalog {
    fn drop(&mut self) {
        self.canceled.store(true, Ordering::Release);
    }
}

impl AppState {
    /// Queue one selected Filter bundle without blocking the UI thread.
    pub(super) fn install_openfx_bundle(&mut self, path: PathBuf) -> Result<()> {
        self.start_openfx_catalog()?;
        self.openfx_catalog
            .as_ref()
            .ok_or_else(|| catalog_error("OpenFX catalog is unavailable"))?
            .requests
            .try_send(CatalogRequest { path, purpose: CatalogPurpose::Install })
            .map_err(|error| catalog_error(error.to_string()))?;
        self.set_status_hint("正在扫描 OpenFX Filter…", false);
        Ok(())
    }

    /// Retry saved machine-local bundles without delaying window startup.
    pub(crate) fn schedule_openfx_catalog_restore(&mut self, bundles: Vec<PathBuf>) {
        if bundles.is_empty() {
            return;
        }
        if let Err(error) = self.start_openfx_catalog() {
            self.set_status_hint(format!("OpenFX 恢复无法启动：{error}"), true);
            return;
        }
        let Some(catalog) = self.openfx_catalog.as_mut() else {
            return;
        };
        for path in bundles {
            if catalog
                .requests
                .try_send(CatalogRequest { path, purpose: CatalogPurpose::Restore })
                .is_ok()
            {
                catalog.restores_remaining += 1;
            } else {
                catalog.restore_failures += 1;
            }
        }
        if catalog.restores_remaining == 0 {
            self.set_status_hint("OpenFX 恢复无法排队", true);
        }
    }

    fn start_openfx_catalog(&mut self) -> Result<()> {
        if self.openfx_catalog.is_some() {
            return Ok(());
        }
        let helper = std::env::current_exe().map_err(|error| catalog_error(error.to_string()))?;
        let (request_sender, request_receiver) = mpsc::sync_channel::<CatalogRequest>(64);
        let (result_sender, result_receiver) = mpsc::channel::<CatalogResult>();
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = Arc::clone(&canceled);
        let worker = thread::Builder::new()
            .name("mondrian-openfx-catalog".to_owned())
            .spawn(move || {
                while let Ok(request) = request_receiver.recv() {
                    if worker_canceled.load(Ordering::Acquire) {
                        break;
                    }
                    let result = install_bundle(&helper, &request.path);
                    if result_sender
                        .send(CatalogResult {
                            path: request.path,
                            purpose: request.purpose,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|error| catalog_error(error.to_string()))?;
        self.openfx_catalog = Some(OpenFxCatalog {
            requests: request_sender,
            results: result_receiver,
            worker: Some(worker),
            canceled,
            restores_remaining: 0,
            restored: 0,
            restore_failures: 0,
        });
        Ok(())
    }

    /// Publish completed definitions and report machine-local install outcomes.
    pub(crate) fn poll_openfx_catalog(&mut self) -> bool {
        let Some(mut catalog) = self.openfx_catalog.take() else {
            return false;
        };
        let mut changed = false;
        let mut disconnected = false;
        loop {
            match catalog.results.try_recv() {
                Ok(result) => {
                    changed = true;
                    match result.purpose {
                        CatalogPurpose::Install => match result.result {
                            Ok((installed, rejected)) => {
                                self.completed_openfx_installs.push(result.path);
                                if rejected.is_empty() {
                                    self.set_status_hint(
                                        format!("已安装 {installed} 个 OpenFX Filter"),
                                        false,
                                    );
                                } else {
                                    self.set_status_hint(
                                        format!(
                                            "已安装 {installed} 个 OpenFX Filter；{} 个插件不受当前宿主支持",
                                            rejected.len()
                                        ),
                                        true,
                                    );
                                }
                            }
                            Err(reason) => {
                                self.set_status_hint(format!("OpenFX 安装失败：{reason}"), true)
                            }
                        },
                        CatalogPurpose::Restore => {
                            catalog.restores_remaining =
                                catalog.restores_remaining.saturating_sub(1);
                            match result.result {
                                Ok((installed, rejected)) => {
                                    catalog.restored += installed;
                                    catalog.restore_failures += rejected.len();
                                    for reason in rejected {
                                        tracing::warn!(path = %result.path.display(), %reason, "OpenFX Filter was not restored");
                                    }
                                }
                                Err(reason) => {
                                    catalog.restore_failures += 1;
                                    tracing::warn!(path = %result.path.display(), %reason, "OpenFX bundle was not restored");
                                }
                            }
                            if catalog.restores_remaining == 0 {
                                if catalog.restore_failures > 0 {
                                    self.set_status_hint(
                                        format!(
                                            "OpenFX 恢复完成：{} 个 Filter 可用，{} 个失败；可重新选择安装文件",
                                            catalog.restored, catalog.restore_failures
                                        ),
                                        true,
                                    );
                                } else if catalog.restored > 0
                                    && !self
                                        .status_hint
                                        .as_ref()
                                        .is_some_and(|(_, is_error)| *is_error)
                                {
                                    self.set_status_hint(
                                        format!("已恢复 {} 个 OpenFX Filter", catalog.restored),
                                        false,
                                    );
                                }
                                catalog.restored = 0;
                                catalog.restore_failures = 0;
                            }
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            if catalog.worker.take().is_some_and(|worker| worker.join().is_err())
                || catalog.restores_remaining > 0
            {
                self.set_status_hint("OpenFX 扫描任务异常结束；请重新选择插件", true);
                changed = true;
            }
        } else {
            self.openfx_catalog = Some(catalog);
        }
        changed
    }

    /// Completed user selections to persist after successful admission.
    pub(crate) fn take_completed_openfx_installs(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.completed_openfx_installs)
    }
}

fn install_bundle(helper: &Path, path: &Path) -> std::result::Result<(usize, Vec<String>), String> {
    let inspection = inspect_openfx_binary(helper, path).map_err(|error| error.to_string())?;
    let mut installed = 0;
    let mut rejected = Vec::new();
    for plugin in &inspection.plugins {
        match register_selected_openfx_filter(helper, &inspection, &plugin.identifier) {
            Ok(_) => installed += 1,
            Err(error) => rejected.push(format!("{}: {error}", plugin.identifier)),
        }
    }
    if installed == 0 {
        return Err(format!(
            "selected bundle contains no supported OpenFX Filter: {}",
            rejected.join("; ")
        ));
    }
    Ok((installed, rejected))
}

fn catalog_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "install_openfx_bundle".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn missing_install_and_restore_finish_without_authoring_or_persistence() {
        let mut state = AppState::new();
        let generation = state.project_author_generation();
        let missing = std::env::temp_dir().join(format!(
            "mondrian-missing-openfx-{}.ofx.bundle",
            mondrian_core::ProjectId::new()
        ));
        state.install_openfx_bundle(missing.clone()).expect("queue install");
        state.schedule_openfx_catalog_restore(vec![missing]);
        let deadline = Instant::now() + Duration::from_secs(5);
        while state
            .openfx_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.restores_remaining > 0)
            && Instant::now() < deadline
        {
            state.poll_openfx_catalog();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            state.openfx_catalog.as_ref().map(|catalog| catalog.restores_remaining),
            Some(0),
            "restore must reach a terminal result"
        );
        assert!(state.take_completed_openfx_installs().is_empty());
        assert_eq!(state.project_author_generation(), generation);
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }
}
