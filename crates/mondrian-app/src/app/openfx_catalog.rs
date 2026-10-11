//! Machine-local OpenFX Filter installation and startup restoration.

use crate::app::AppMessage;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use mondrian_core::{MondrianError, Result};
use mondrian_effects::{register_effect_definition, EffectDefinition};

use crate::openfx_adapter::{inspect_openfx_binary_with_checkpoint, OpenFxDiscoveryError};
use crate::openfx_effect::prepare_selected_openfx_filter;
use crate::openfx_host::OpenFxHostError;

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
    result: std::result::Result<(Vec<EffectDefinition>, Vec<String>), String>,
}

pub(super) struct OpenFxCatalog {
    requests: Option<SyncSender<CatalogRequest>>,
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
        // Wake an idle recv before joining. Active children observe the same
        // cancellation authority and are killed/reaped by their worker owner.
        drop(self.requests.take());
        if self.worker.take().is_some_and(|worker| worker.join().is_err()) {
            tracing::warn!("OpenFX catalog worker panicked during settlement");
        }
    }
}

impl OpenFxCatalog {
    fn start(helper: PathBuf) -> Result<Self> {
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
                    let result = install_bundle(&helper, &request.path, &worker_canceled);
                    if worker_canceled.load(Ordering::Acquire) {
                        break;
                    }
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
        Ok(Self {
            requests: Some(request_sender),
            results: result_receiver,
            worker: Some(worker),
            canceled,
            restores_remaining: 0,
            restored: 0,
            restore_failures: 0,
        })
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
            .as_ref()
            .ok_or_else(|| catalog_error("OpenFX catalog is closed"))?
            .try_send(CatalogRequest { path, purpose: CatalogPurpose::Install })
            .map_err(|error| catalog_error(error.to_string()))?;
        self.set_status_hint(AppMessage::new("status-openfx-scanning"), false);
        Ok(())
    }

    /// Retry saved machine-local bundles without delaying window startup.
    pub(crate) fn schedule_openfx_catalog_restore(&mut self, bundles: Vec<PathBuf>) {
        if bundles.is_empty() {
            return;
        }
        if let Err(error) = self.start_openfx_catalog() {
            self.set_status_hint(
                AppMessage::new("status-openfx-restore-failed")
                    .with_text("error", error.to_string()),
                true,
            );
            return;
        }
        let Some(catalog) = self.openfx_catalog.as_mut() else {
            return;
        };
        for path in bundles {
            if catalog
                .requests
                .as_ref()
                .expect("live catalog request sender")
                .try_send(CatalogRequest { path, purpose: CatalogPurpose::Restore })
                .is_ok()
            {
                catalog.restores_remaining += 1;
            } else {
                catalog.restore_failures += 1;
            }
        }
        if catalog.restores_remaining == 0 {
            self.set_status_hint(AppMessage::new("status-openfx-restore-queue-failed"), true);
        }
    }

    fn start_openfx_catalog(&mut self) -> Result<()> {
        if self.openfx_catalog.is_some() {
            return Ok(());
        }
        let helper = super::packaged_worker::discover_openfx_worker()
            .map_err(|error| catalog_error(error.to_string()))?;
        self.openfx_catalog = Some(OpenFxCatalog::start(helper)?);
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
                    // Registry mutation occurs only on this live App owner.
                    // A dropped catalog consumes/discards prepared definitions.
                    let outcome = publish_bundle(result.result);
                    changed = true;
                    match result.purpose {
                        CatalogPurpose::Install => match outcome {
                            Ok((installed, rejected)) => {
                                self.completed_openfx_installs.push(result.path);
                                if rejected.is_empty() {
                                    self.set_status_hint(
                                        AppMessage::new("status-openfx-installed")
                                            .with_count("installed", installed),
                                        false,
                                    );
                                } else {
                                    self.set_status_hint(
                                        AppMessage::new("status-openfx-installed-partial")
                                            .with_count("installed", installed)
                                            .with_count("unsupported", rejected.len()),
                                        true,
                                    );
                                }
                            }
                            Err(reason) => self.set_status_hint(
                                AppMessage::new("status-openfx-install-failed")
                                    .with_text("reason", reason.clone()),
                                true,
                            ),
                        },
                        CatalogPurpose::Restore => {
                            catalog.restores_remaining =
                                catalog.restores_remaining.saturating_sub(1);
                            match outcome {
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
                                        AppMessage::new("status-openfx-restored-partial")
                                            .with_count("available", catalog.restored)
                                            .with_count("failed", catalog.restore_failures),
                                        true,
                                    );
                                } else if catalog.restored > 0
                                    && !self
                                        .status_hint
                                        .as_ref()
                                        .is_some_and(|(_, is_error)| *is_error)
                                {
                                    self.set_status_hint(
                                        AppMessage::new("status-openfx-restored")
                                            .with_count("count", catalog.restored),
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
                self.set_status_hint(AppMessage::new("status-openfx-scan-failed"), true);
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

fn install_bundle(
    helper: &Path,
    path: &Path,
    canceled: &AtomicBool,
) -> std::result::Result<(Vec<EffectDefinition>, Vec<String>), String> {
    let inspection = inspect_openfx_binary_with_checkpoint(helper, path, &mut || {
        if canceled.load(Ordering::Acquire) {
            Err(OpenFxDiscoveryError::Canceled)
        } else {
            Ok(())
        }
    })
    .map_err(|error| error.to_string())?;
    let mut definitions = Vec::new();
    let mut rejected = Vec::new();
    for plugin in &inspection.plugins {
        let mut checkpoint = || {
            if canceled.load(Ordering::Acquire) {
                Err(OpenFxHostError::Canceled)
            } else {
                Ok(())
            }
        };
        checkpoint().map_err(|error| error.to_string())?;
        match prepare_selected_openfx_filter(
            helper,
            &inspection,
            &plugin.identifier,
            &mut checkpoint,
        ) {
            Ok(definition) => definitions.push(definition),
            Err(error) => rejected.push(format!("{}: {error}", plugin.identifier)),
        }
        checkpoint().map_err(|error| error.to_string())?;
    }
    Ok((definitions, rejected))
}

fn publish_bundle(
    result: std::result::Result<(Vec<EffectDefinition>, Vec<String>), String>,
) -> std::result::Result<(usize, Vec<String>), String> {
    let (definitions, mut rejected) = result?;
    let mut installed = 0;
    for definition in definitions {
        match register_effect_definition(definition) {
            Ok(()) => installed += 1,
            Err(error) => rejected.push(error.to_string()),
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

#[cfg(all(test, windows))]
impl AppState {
    /// Retain an independent native observation handle; does not alter ownership.
    pub(crate) fn openfx_catalog_worker_handle_for_qualification(
        &self,
    ) -> std::io::Result<std::os::windows::io::OwnedHandle> {
        use std::os::windows::io::AsHandle;
        let worker = self
            .openfx_catalog
            .as_ref()
            .and_then(|catalog| catalog.worker.as_ref())
            .ok_or_else(|| std::io::Error::other("catalog worker is unavailable"))?;
        worker.as_handle().try_clone_to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn missing_install_and_restore_finish_without_authoring_or_persistence() {
        let mut state = AppState::new();
        // Missing selections are rejected before spawning a helper. This test
        // deliberately does not require a built product executable sibling.
        state.openfx_catalog = Some(
            OpenFxCatalog::start(std::env::current_exe().expect("test executable"))
                .expect("catalog"),
        );
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

    #[test]
    fn catalog_drop_wakes_and_joins_an_idle_worker() {
        let catalog = OpenFxCatalog::start(std::env::current_exe().expect("test executable"))
            .expect("catalog");
        let canceled = Arc::clone(&catalog.canceled);
        let started = Instant::now();
        drop(catalog);
        assert!(canceled.load(Ordering::Acquire));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "idle recv must be woken before join"
        );
    }

    fn prepared_catalog() -> (OpenFxCatalog, mondrian_effects::EffectType) {
        let key = format!("test.openfx.catalog.{}", mondrian_core::ProjectId::new());
        let effect_type = mondrian_effects::EffectType::Plugin(key.clone());
        let definition = EffectDefinition::new(
            key,
            "Prepared catalog control",
            mondrian_core::automation::PropertyBag::default(),
            mondrian_effects::EffectColorDomainContract::SCENE_LINEAR,
        );
        let (requests, _) = mpsc::sync_channel(1);
        let (sender, results) = mpsc::channel();
        sender
            .send(CatalogResult {
                path: PathBuf::from("prepared-catalog-control.ofx"),
                purpose: CatalogPurpose::Install,
                result: Ok((vec![definition], Vec::new())),
            })
            .expect("prepared result");
        (
            OpenFxCatalog {
                requests: Some(requests),
                results,
                worker: None,
                canceled: Arc::new(AtomicBool::new(false)),
                restores_remaining: 0,
                restored: 0,
                restore_failures: 0,
            },
            effect_type,
        )
    }

    #[test]
    fn dropping_unpolled_catalog_never_publishes_prepared_definition() {
        let (catalog, effect_type) = prepared_catalog();
        assert!(mondrian_effects::effect_definition(&effect_type).is_none());
        drop(catalog);
        assert!(mondrian_effects::effect_definition(&effect_type).is_none());
    }

    #[test]
    fn live_owner_poll_commits_prepared_definition_without_authoring_mutation() {
        let (catalog, effect_type) = prepared_catalog();
        let mut state = AppState::new();
        let generation = state.project_author_generation();
        state.openfx_catalog = Some(catalog);
        assert!(mondrian_effects::effect_definition(&effect_type).is_none());
        assert!(state.poll_openfx_catalog());
        assert!(mondrian_effects::effect_definition(&effect_type).is_some());
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(
            state.take_completed_openfx_installs(),
            vec![PathBuf::from("prepared-catalog-control.ofx")]
        );
    }
}
