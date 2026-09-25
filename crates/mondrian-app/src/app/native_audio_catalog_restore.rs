//! Background restoration of machine-local native audio plugin selections.
//!
//! Discovery is bounded in a domain-owned thread; plugin code loads only in
//! its format-specific isolated discovery child.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use mondrian_audio::{
    InstalledClapAudioProcessorSpecResolver, InstalledVst3AudioProcessorSpecResolver,
};

use super::native_audio_plugin::{NativeAudioPluginFormat, NativeAudioPluginSelection};
use super::AppState;

struct RestoreResult {
    selection: NativeAudioPluginSelection,
    result: Result<usize, String>,
}

pub(super) struct NativeAudioCatalogRestore {
    receiver: Receiver<RestoreResult>,
    worker: Option<JoinHandle<()>>,
    canceled: Arc<AtomicBool>,
    succeeded: usize,
    failed: usize,
}

impl Drop for NativeAudioCatalogRestore {
    fn drop(&mut self) {
        self.canceled.store(true, Ordering::Release);
    }
}

impl AppState {
    /// Retry saved machine-local native binaries without stalling window startup.
    pub(crate) fn schedule_native_audio_catalog_restore(
        &mut self,
        selections: Vec<NativeAudioPluginSelection>,
    ) {
        if selections.is_empty() {
            return;
        }
        let (Some(clap), Some(vst3)) = (self.clap_catalog.clone(), self.vst3_catalog.clone())
        else {
            self.set_status_hint("原生音频插件宿主不可用；已保存的插件无法恢复", true);
            return;
        };
        if self.native_audio_restore.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = Arc::clone(&canceled);
        let worker = thread::Builder::new()
            .name("mondrian-native-audio-restore".to_owned())
            .spawn(move || restore_plugins(selections, clap, vst3, worker_canceled, sender));
        match worker {
            Ok(worker) => {
                self.native_audio_restore = Some(NativeAudioCatalogRestore {
                    receiver,
                    worker: Some(worker),
                    canceled,
                    succeeded: 0,
                    failed: 0,
                });
            }
            Err(error) => self.set_status_hint(format!("原生音频插件恢复无法启动：{error}"), true),
        }
    }

    /// Publish completed discovery results and refresh audio once new plugins exist.
    pub(crate) fn poll_native_audio_catalog_restore(&mut self) -> bool {
        let Some(mut restore) = self.native_audio_restore.take() else {
            return false;
        };
        let mut changed = false;
        let mut new_plugins = false;
        let mut finished = false;
        loop {
            match restore.receiver.try_recv() {
                Ok(RestoreResult { selection, result }) => {
                    changed = true;
                    match result {
                        Ok(count) => {
                            restore.succeeded += 1;
                            new_plugins |= count > 0;
                        }
                        Err(error) => {
                            restore.failed += 1;
                            tracing::warn!(path = %selection.path.display(), format = ?selection.format, %error, "native audio plugin restoration failed");
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
            self.reconcile_audio_after_committed_authoring_change("native_audio_catalog_restore");
        }
        if finished {
            let panicked = restore.worker.take().is_some_and(|worker| worker.join().is_err());
            if panicked {
                restore.failed += 1;
            }
            if restore.failed > 0 {
                self.set_status_hint(
                    format!(
                        "音频插件恢复完成：{} 个库可用，{} 个库失败；可重新选择失败的插件文件",
                        restore.succeeded, restore.failed
                    ),
                    true,
                );
            } else if restore.succeeded > 0
                && !self.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error)
            {
                self.set_status_hint(format!("已恢复 {} 个音频插件库", restore.succeeded), false);
            }
            changed = true;
        } else {
            self.native_audio_restore = Some(restore);
        }
        changed
    }
}

fn restore_plugins(
    selections: Vec<NativeAudioPluginSelection>,
    clap: Arc<InstalledClapAudioProcessorSpecResolver>,
    vst3: Arc<InstalledVst3AudioProcessorSpecResolver>,
    canceled: Arc<AtomicBool>,
    sender: mpsc::Sender<RestoreResult>,
) {
    for selection in selections {
        if canceled.load(Ordering::Acquire) {
            break;
        }
        let result = match selection.format {
            NativeAudioPluginFormat::Clap => clap
                .install_library(selection.path.clone())
                .map(|descriptors| descriptors.len()),
            NativeAudioPluginFormat::Vst3 => {
                vst3.install_plugin(selection.path.clone()).map(|descriptors| descriptors.len())
            }
        }
        .map_err(|error| error.to_string());
        if sender.send(RestoreResult { selection, result }).is_err() {
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
        state.schedule_native_audio_catalog_restore(vec![
            NativeAudioPluginSelection {
                format: NativeAudioPluginFormat::Clap,
                path: missing.clone(),
            },
            NativeAudioPluginSelection {
                format: NativeAudioPluginFormat::Vst3,
                path: missing,
            },
        ]);
        let deadline = Instant::now() + Duration::from_secs(3);
        while state.native_audio_restore.is_some() && Instant::now() < deadline {
            state.poll_native_audio_catalog_restore();
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            state.native_audio_restore.is_none(),
            "restore worker must finish"
        );
        assert_eq!(state.project_author_generation(), generation);
        assert!(state.installed_clap_processors().expect("catalog").is_empty());
        assert!(state.installed_vst3_processors().expect("catalog").is_empty());
        assert!(state.status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
    }
}
