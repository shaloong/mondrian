//! Background restoration of machine-local CLAP selections.
//!
//! Discovery is bounded in a domain-owned thread and each native library is
//! still loaded only by the isolated CLAP discovery child.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use mondrian_audio::InstalledClapAudioProcessorSpecResolver;

use super::AppState;

struct RestoreResult {
    path: PathBuf,
    result: Result<usize, String>,
}

pub(super) struct ClapCatalogRestore {
    receiver: Receiver<RestoreResult>,
    worker: Option<JoinHandle<()>>,
    canceled: Arc<AtomicBool>,
    succeeded: usize,
    failed: usize,
}

impl Drop for ClapCatalogRestore {
    fn drop(&mut self) {
        self.canceled.store(true, Ordering::Release);
    }
}

impl AppState {
    /// Retry saved machine-local CLAP binaries without stalling window startup.
    pub(crate) fn schedule_clap_catalog_restore(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let Some(catalog) = self.clap_catalog.clone() else {
            self.set_status_hint(
                "CLAP helper unavailable; saved plugins could not be restored",
                true,
            );
            return;
        };
        if self.clap_restore.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = Arc::clone(&canceled);
        let worker = thread::Builder::new()
            .name("mondrian-clap-restore".to_owned())
            .spawn(move || restore_libraries(paths, catalog, worker_canceled, sender));
        match worker {
            Ok(worker) => {
                self.clap_restore = Some(ClapCatalogRestore {
                    receiver,
                    worker: Some(worker),
                    canceled,
                    succeeded: 0,
                    failed: 0,
                });
            }
            Err(error) => self.set_status_hint(
                format!("CLAP plugin restoration could not start: {error}"),
                true,
            ),
        }
    }

    /// Publish completed discovery results and refresh audio once new plugins exist.
    pub(crate) fn poll_clap_catalog_restore(&mut self) -> bool {
        let Some(mut restore) = self.clap_restore.take() else {
            return false;
        };
        let mut changed = false;
        let mut new_plugins = false;
        let mut finished = false;
        loop {
            match restore.receiver.try_recv() {
                Ok(RestoreResult { path, result }) => {
                    changed = true;
                    match result {
                        Ok(count) => {
                            restore.succeeded += 1;
                            new_plugins |= count > 0;
                        }
                        Err(error) => {
                            restore.failed += 1;
                            tracing::warn!(path = %path.display(), %error, "CLAP restoration failed");
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
        if new_plugins && self.active_sequence().is_some() {
            self.reconcile_audio_after_committed_authoring_change("clap_catalog_restore");
        }
        if finished {
            let panicked = restore.worker.take().is_some_and(|worker| worker.join().is_err());
            if panicked {
                restore.failed += 1;
            }
            if restore.failed > 0 {
                self.set_status_hint(
                    format!(
                        "CLAP 恢复完成：{} 个库可用，{} 个库失败；可重新选择失败的插件文件",
                        restore.succeeded, restore.failed
                    ),
                    true,
                );
            } else if restore.succeeded > 0 {
                self.set_status_hint(
                    format!("已恢复 {} 个 CLAP 插件库", restore.succeeded),
                    false,
                );
            }
            changed = true;
        } else {
            self.clap_restore = Some(restore);
        }
        changed
    }
}

fn restore_libraries(
    paths: Vec<PathBuf>,
    catalog: Arc<InstalledClapAudioProcessorSpecResolver>,
    canceled: Arc<AtomicBool>,
    sender: mpsc::Sender<RestoreResult>,
) {
    for path in paths {
        if canceled.load(Ordering::Acquire) {
            break;
        }
        let result = catalog
            .install_library(path.clone())
            .map(|descriptors| descriptors.len())
            .map_err(|error| error.to_string());
        if sender.send(RestoreResult { path, result }).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn missing_saved_library_finishes_without_project_mutation() {
        let mut state = AppState::new();
        let generation = state.project_author_generation();
        let missing = std::env::temp_dir().join(format!(
            "mondrian-clap-restore-missing-{}.dll",
            mondrian_core::ProjectId::new()
        ));
        state.schedule_clap_catalog_restore(vec![missing]);
        let deadline = Instant::now() + Duration::from_secs(3);
        while state.clap_restore.is_some() && Instant::now() < deadline {
            state.poll_clap_catalog_restore();
            thread::sleep(Duration::from_millis(5));
        }
        assert!(state.clap_restore.is_none(), "restore worker must finish");
        assert_eq!(state.project_author_generation(), generation);
        assert!(state.installed_clap_processors().expect("catalog").is_empty());
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }
}
