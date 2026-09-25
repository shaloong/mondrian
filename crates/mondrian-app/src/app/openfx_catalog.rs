//! Machine-local OpenFX Filter installation and background startup restoration.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use mondrian_core::{MondrianError, Result};

use crate::openfx_adapter::inspect_openfx_binary;
use crate::openfx_effect::register_selected_openfx_filter;

use super::AppState;

struct RestoreResult {
    path: PathBuf,
    result: std::result::Result<(usize, Vec<String>), String>,
}

pub(super) struct OpenFxCatalogRestore {
    receiver: Receiver<RestoreResult>,
    worker: Option<JoinHandle<()>>,
    canceled: Arc<AtomicBool>,
    installed: usize,
    failed: usize,
}

impl Drop for OpenFxCatalogRestore {
    fn drop(&mut self) {
        self.canceled.store(true, Ordering::Release);
    }
}

impl AppState {
    /// Admit installed Filter definitions without changing the Project.
    pub(super) fn install_openfx_bundle(&mut self, path: PathBuf) -> Result<()> {
        let helper = std::env::current_exe().map_err(|error| install_error(error.to_string()))?;
        let (installed, rejected) = install_bundle(&helper, &path).map_err(install_error)?;
        if rejected.is_empty() {
            self.set_status_hint(format!("已安装 {installed} 个 OpenFX Filter"), false);
        } else {
            self.set_status_hint(
                format!(
                    "已安装 {installed} 个 OpenFX Filter；{} 个插件不受当前宿主支持",
                    rejected.len()
                ),
                true,
            );
        }
        Ok(())
    }

    /// Retry saved machine-local bundles without delaying window startup.
    pub(crate) fn schedule_openfx_catalog_restore(&mut self, bundles: Vec<PathBuf>) {
        if bundles.is_empty() || self.openfx_restore.is_some() {
            return;
        }
        let helper = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                self.set_status_hint(format!("OpenFX 恢复无法启动：{error}"), true);
                return;
            }
        };
        let (sender, receiver) = mpsc::channel();
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = Arc::clone(&canceled);
        let worker =
            thread::Builder::new()
                .name("mondrian-openfx-restore".to_owned())
                .spawn(move || {
                    for path in bundles {
                        if worker_canceled.load(Ordering::Acquire) {
                            break;
                        }
                        let result = install_bundle(&helper, &path);
                        if sender.send(RestoreResult { path, result }).is_err() {
                            break;
                        }
                    }
                });
        match worker {
            Ok(worker) => {
                self.openfx_restore = Some(OpenFxCatalogRestore {
                    receiver,
                    worker: Some(worker),
                    canceled,
                    installed: 0,
                    failed: 0,
                });
            }
            Err(error) => self.set_status_hint(format!("OpenFX 恢复线程无法启动：{error}"), true),
        }
    }

    /// Publish newly registered Filters to the retained Effect Browser.
    pub(crate) fn poll_openfx_catalog_restore(&mut self) -> bool {
        let Some(mut restore) = self.openfx_restore.take() else {
            return false;
        };
        let mut changed = false;
        let mut finished = false;
        loop {
            match restore.receiver.try_recv() {
                Ok(RestoreResult { path, result }) => {
                    changed = true;
                    match result {
                        Ok((installed, rejected)) => {
                            restore.installed += installed;
                            restore.failed += rejected.len();
                            for reason in rejected {
                                tracing::warn!(path = %path.display(), %reason, "OpenFX Filter was not restored");
                            }
                        }
                        Err(reason) => {
                            restore.failed += 1;
                            tracing::warn!(path = %path.display(), %reason, "OpenFX bundle was not restored");
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            if restore.worker.take().is_some_and(|worker| worker.join().is_err()) {
                restore.failed += 1;
            }
            if restore.failed > 0 {
                self.set_status_hint(
                    format!(
                        "OpenFX 恢复完成：{} 个 Filter 可用，{} 个失败；可重新选择安装文件",
                        restore.installed, restore.failed
                    ),
                    true,
                );
            } else if restore.installed > 0
                && !self.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error)
            {
                self.set_status_hint(
                    format!("已恢复 {} 个 OpenFX Filter", restore.installed),
                    false,
                );
            }
            changed = true;
        } else {
            self.openfx_restore = Some(restore);
        }
        changed
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

fn install_error(reason: impl Into<String>) -> MondrianError {
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
    fn missing_bundle_fails_without_authoring_and_restore_reports_failure() {
        let mut state = AppState::new();
        let generation = state.project_author_generation();
        let missing = std::env::temp_dir().join(format!(
            "mondrian-missing-openfx-{}.ofx.bundle",
            mondrian_core::ProjectId::new()
        ));
        assert!(state.install_openfx_bundle(missing.clone()).is_err());
        assert_eq!(state.project_author_generation(), generation);

        state.schedule_openfx_catalog_restore(vec![missing]);
        let deadline = Instant::now() + Duration::from_secs(5);
        while state.openfx_restore.is_some() && Instant::now() < deadline {
            state.poll_openfx_catalog_restore();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            state.openfx_restore.is_none(),
            "restore must reach a terminal result"
        );
        assert_eq!(state.project_author_generation(), generation);
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }
}
