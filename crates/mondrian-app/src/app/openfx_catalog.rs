//! Machine-local OpenFX Filter installation and startup restoration.

use crate::app::AppMessage;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use mondrian_core::{MondrianError, Result};
use mondrian_effects::{register_effect_definition, EffectDefinition};

use crate::openfx_adapter::{inspect_openfx_binary_with_checkpoint, OpenFxDiscoveryError};
use crate::openfx_effect::{prepare_selected_openfx_filter, PreparedOpenFxFilter};
use crate::openfx_host::{OpenFxDescribeNotices, OpenFxHostError};
use mondrian_effects::EffectProcessorNoticeKind;

use super::notifications::AppNotificationSeverity;

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
    preparation: BundlePreparation,
}

const MAX_BUNDLE_NOTICES: usize = 256;
const MAX_BUNDLE_NOTICE_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct CatalogNotices {
    batches: Vec<OpenFxDescribeNotices>,
    count: usize,
    utf8_bytes: usize,
}

impl CatalogNotices {
    fn push(&mut self, batch: OpenFxDescribeNotices) -> std::result::Result<(), String> {
        let count = self.count.checked_add(batch.notices.len());
        let bytes = batch.notices.iter().try_fold(self.utf8_bytes, |sum, notice| {
            sum.checked_add(notice.text().len())?.checked_add(notice.id().len())
        });
        let Some((count, bytes)) = count.zip(bytes).filter(|(count, bytes)| {
            *count <= MAX_BUNDLE_NOTICES && *bytes <= MAX_BUNDLE_NOTICE_BYTES
        }) else {
            return Err("OpenFX bundle notice budget exceeded".to_owned());
        };
        if !batch.notices.is_empty() || !batch.complete || batch.fatal {
            self.batches
                .try_reserve(1)
                .map_err(|_| "OpenFX bundle notice allocation failed".to_owned())?;
            self.batches.push(batch);
        }
        self.count = count;
        self.utf8_bytes = bytes;
        Ok(())
    }
}

struct BundlePreparation {
    result: std::result::Result<(Vec<EffectDefinition>, Vec<String>), String>,
    notices: CatalogNotices,
}

impl BundlePreparation {
    fn record(&mut self, identifier: &str, prepared: PreparedOpenFxFilter) {
        if self.result.is_err() {
            return;
        }
        if let Some(batch) = prepared.notices
            && let Err(error) = self.notices.push(batch)
        {
            // Refuse this entire preparation, including earlier definitions;
            // no truncated notification batch escapes as successful work.
            self.result = Err(error);
            self.notices = CatalogNotices::default();
            return;
        }
        let Ok((definitions, rejected)) = &mut self.result else {
            unreachable!()
        };
        match prepared.definition {
            Ok(definition) => definitions.push(definition),
            Err(error) => rejected.push(format!("{identifier}: {error}")),
        }
    }
}

// One retained result slot, plus the worker's current owned result. In
// particular, Drop must not join a worker blocked in SyncSender::send.
fn send_catalog_result(
    sender: &SyncSender<CatalogResult>,
    mut result: CatalogResult,
    canceled: &AtomicBool,
) -> bool {
    while !canceled.load(Ordering::Acquire) {
        match sender.try_send(result) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(retained)) => {
                result = retained;
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
    false
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
        let (result_sender, result_receiver) = mpsc::sync_channel::<CatalogResult>(1);
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = Arc::clone(&canceled);
        let worker = thread::Builder::new()
            .name("mondrian-openfx-catalog".to_owned())
            .spawn(move || {
                while let Ok(request) = request_receiver.recv() {
                    if worker_canceled.load(Ordering::Acquire) {
                        break;
                    }
                    let preparation = install_bundle(&helper, &request.path, &worker_canceled);
                    if worker_canceled.load(Ordering::Acquire) {
                        break;
                    }
                    if !send_catalog_result(
                        &result_sender,
                        CatalogResult {
                            path: request.path,
                            purpose: request.purpose,
                            preparation,
                        },
                        &worker_canceled,
                    ) {
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
                    if catalog.canceled.load(Ordering::Acquire) {
                        break;
                    }
                    self.publish_openfx_metadata_notices(result.preparation.notices);
                    // Registry mutation occurs only on this live App owner.
                    // A dropped catalog consumes/discards prepared definitions.
                    let outcome = publish_bundle(result.preparation.result);
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

    fn publish_openfx_metadata_notices(&mut self, notices: CatalogNotices) {
        for batch in notices.batches {
            // This is the verified request context, not an observed emitter
            // class/action, native instance, Render invocation, or generation.
            if !batch.complete || batch.fatal {
                tracing::warn!(binary = %batch.binary_path.display(), sha256 = %batch.binary_sha256,
                    requested_class = %batch.requested_plugin_identifier,
                    phase = "whole_metadata_request", complete = batch.complete, fatal = batch.fatal,
                    "OpenFX metadata notices accompany a refused definition");
            }
            for notice in batch.notices {
                if notice.kind() == EffectProcessorNoticeKind::Log {
                    tracing::info!(binary = %batch.binary_path.display(), sha256 = %batch.binary_sha256,
                        requested_class = %batch.requested_plugin_identifier,
                        phase = "whole_metadata_request", id = notice.id(), message = notice.text(),
                        "OpenFX metadata log");
                    continue;
                }
                let severity = match notice.kind() {
                    EffectProcessorNoticeKind::Information => AppNotificationSeverity::Information,
                    EffectProcessorNoticeKind::Warning => AppNotificationSeverity::Warning,
                    EffectProcessorNoticeKind::Error | EffectProcessorNoticeKind::Fatal => {
                        AppNotificationSeverity::Error
                    }
                    EffectProcessorNoticeKind::Log => unreachable!(),
                };
                self.notifications.publish(
                    format!(
                        "openfx-metadata:{}:{}:{:?}:{}",
                        batch.binary_sha256,
                        batch.requested_plugin_identifier,
                        notice.kind(),
                        notice.id()
                    ),
                    severity,
                    AppMessage::new("notification-native-processor")
                        .with_text("message", notice.text()),
                );
            }
        }
    }

    /// Completed user selections to persist after successful admission.
    pub(crate) fn take_completed_openfx_installs(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.completed_openfx_installs)
    }
}

fn install_bundle(helper: &Path, path: &Path, canceled: &AtomicBool) -> BundlePreparation {
    let mut preparation = BundlePreparation {
        result: Ok((Vec::new(), Vec::new())),
        notices: CatalogNotices::default(),
    };
    let result = (|| {
        let inspection = inspect_openfx_binary_with_checkpoint(helper, path, &mut || {
            if canceled.load(Ordering::Acquire) {
                Err(OpenFxDiscoveryError::Canceled)
            } else {
                Ok(())
            }
        })
        .map_err(|error| error.to_string())?;
        for plugin in &inspection.plugins {
            let mut checkpoint = || {
                if canceled.load(Ordering::Acquire) {
                    Err(OpenFxHostError::Canceled)
                } else {
                    Ok(())
                }
            };
            checkpoint().map_err(|error| error.to_string())?;
            let prepared = prepare_selected_openfx_filter(
                helper,
                &inspection,
                &plugin.identifier,
                &mut checkpoint,
            );
            preparation.record(&plugin.identifier, prepared);
            if preparation.result.is_err() {
                return Ok(());
            }
            checkpoint().map_err(|error| error.to_string())?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        preparation.result = Err(error);
    }
    preparation
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
        let effect_type = mondrian_effects::EffectType::External(key.clone());
        let definition = EffectDefinition::new(
            key,
            "Prepared catalog control",
            mondrian_core::automation::PropertyBag::default(),
            mondrian_effects::EffectColorDomainContract::SCENE_LINEAR,
        );
        let (requests, _) = mpsc::sync_channel(1);
        let (sender, results) = mpsc::sync_channel(1);
        sender
            .send(CatalogResult {
                path: PathBuf::from("prepared-catalog-control.ofx"),
                purpose: CatalogPurpose::Install,
                preparation: BundlePreparation {
                    result: Ok((vec![definition], Vec::new())),
                    notices: CatalogNotices::default(),
                },
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
    fn notice_batch(count: usize, text: &str, id: &str) -> OpenFxDescribeNotices {
        OpenFxDescribeNotices {
            binary_path: PathBuf::from("verified-request.ofx"),
            binary_sha256: "a".repeat(64),
            requested_plugin_identifier: "requested.class".into(),
            complete: true,
            fatal: false,
            notices: (0..count)
                .map(|_| {
                    mondrian_effects::EffectProcessorNotice::new(
                        EffectProcessorNoticeKind::Information,
                        text.to_owned(),
                        id.to_owned(),
                    )
                    .unwrap()
                })
                .collect(),
        }
    }

    #[test]
    fn bundle_notice_count_and_utf8_bytes_accept_exact_limits_and_refuse_one_more() {
        let mut notices = CatalogNotices::default();
        for _ in 0..8 {
            notices.push(notice_batch(32, "x", "id")).unwrap();
        }
        assert_eq!(notices.count, 256);
        assert_eq!(notices.utf8_bytes, 768);
        assert!(notices.push(notice_batch(1, "x", "id")).is_err());
        assert_eq!(
            (notices.count, notices.utf8_bytes, notices.batches.len()),
            (256, 768, 8)
        );
        for (last_id, accepted) in [("x", true), ("xx", false)] {
            let mut notices = CatalogNotices::default();
            for _ in 0..7 {
                notices.push(notice_batch(32, &"x".repeat(4096), "")).unwrap();
            }
            notices.push(notice_batch(31, &"x".repeat(4096), "")).unwrap();
            let before = (notices.count, notices.utf8_bytes, notices.batches.len());
            assert_eq!(
                notices.push(notice_batch(1, &"x".repeat(4095), last_id)).is_ok(),
                accepted
            );
            if accepted {
                assert_eq!((notices.count, notices.utf8_bytes), (256, 1024 * 1024));
            } else {
                assert_eq!(
                    (notices.count, notices.utf8_bytes, notices.batches.len()),
                    before
                );
            }
        }
    }

    #[test]
    fn bundle_notice_overflow_discards_all_prepared_definitions_but_class_refusal_is_partial() {
        let key = format!(
            "test.openfx.notice-budget.{}",
            mondrian_core::ProjectId::new()
        );
        let effect_type = mondrian_effects::EffectType::External(key.clone());
        let mut preparation = BundlePreparation {
            result: Ok((Vec::new(), Vec::new())),
            notices: CatalogNotices::default(),
        };
        preparation.record(
            "supported",
            PreparedOpenFxFilter {
                definition: Ok(EffectDefinition::new(
                    key,
                    "Budget control",
                    mondrian_core::automation::PropertyBag::default(),
                    mondrian_effects::EffectColorDomainContract::SCENE_LINEAR,
                )),
                notices: Some(notice_batch(32, "x", "")),
            },
        );
        // An ordinary unsupported class retains the prior supported definition.
        preparation.record(
            "unsupported",
            PreparedOpenFxFilter {
                definition: Err(crate::openfx_effect::OpenFxEffectRegistrationError::Schema(
                    "owned refusal".into(),
                )),
                notices: Some(notice_batch(0, "", "")),
            },
        );
        let Ok((definitions, rejected)) = &preparation.result else {
            panic!("partial preparation")
        };
        assert_eq!((definitions.len(), rejected.len()), (1, 1));
        for _ in 0..7 {
            preparation.record(
                "unsupported",
                PreparedOpenFxFilter {
                    definition: Err(crate::openfx_effect::OpenFxEffectRegistrationError::Schema(
                        "owned refusal".into(),
                    )),
                    notices: Some(notice_batch(32, "x", "")),
                },
            );
        }
        preparation.record(
            "overflow",
            PreparedOpenFxFilter {
                definition: Err(crate::openfx_effect::OpenFxEffectRegistrationError::Schema(
                    "owned refusal".into(),
                )),
                notices: Some(notice_batch(1, "x", "")),
            },
        );
        assert!(matches!(&preparation.result, Err(reason) if reason.contains("notice budget")));
        assert_eq!(
            (preparation.notices.count, preparation.notices.batches.len()),
            (0, 0)
        );
        assert!(publish_bundle(preparation.result).is_err());
        assert!(mondrian_effects::effect_definition(&effect_type).is_none());
    }

    fn failed_result(path: &str) -> CatalogResult {
        CatalogResult {
            path: PathBuf::from(path),
            purpose: CatalogPurpose::Install,
            preparation: BundlePreparation {
                result: Err(path.into()),
                notices: CatalogNotices::default(),
            },
        }
    }

    #[test]
    fn full_result_channel_retains_the_owned_result_until_space_is_available() {
        let (sender, receiver) = mpsc::sync_channel(1);
        assert!(sender.try_send(failed_result("first")).is_ok());
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = Arc::clone(&canceled);
        let retained = failed_result("second-result-must-not-be-cloned");
        let allocation = retained.path.as_os_str().as_encoded_bytes().as_ptr() as usize;
        let (started, running) = mpsc::sync_channel(1);
        let (completed, completion) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            completed
                .send(send_catalog_result(&sender, retained, &worker_canceled))
                .unwrap();
        });
        running.recv().unwrap();
        assert!(matches!(
            completion.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(receiver.recv().unwrap().path, PathBuf::from("first"));
        assert!(completion.recv_timeout(Duration::from_secs(2)).unwrap());
        let second = receiver.recv().unwrap();
        assert_eq!(
            second.path,
            PathBuf::from("second-result-must-not-be-cloned")
        );
        assert_eq!(
            second.path.as_os_str().as_encoded_bytes().as_ptr() as usize,
            allocation
        );
        worker.join().unwrap();
    }

    #[test]
    fn full_result_channel_cancel_drop_and_disconnect_settle_without_blocking_send() {
        let (sender, results) = mpsc::sync_channel(1);
        assert!(sender.try_send(failed_result("queued")).is_ok());
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = Arc::clone(&canceled);
        let (started, running) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            started.send(()).unwrap();
            assert!(!send_catalog_result(
                &sender,
                failed_result("retained"),
                &worker_canceled
            ));
        });
        running.recv().unwrap();
        let catalog = OpenFxCatalog {
            requests: None,
            results,
            worker: Some(worker),
            canceled: Arc::clone(&canceled),
            restores_remaining: 0,
            restored: 0,
            restore_failures: 0,
        };
        let started = Instant::now();
        drop(catalog);
        assert!(canceled.load(Ordering::Acquire));
        assert!(started.elapsed() < Duration::from_secs(2));
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        assert!(!send_catalog_result(
            &sender,
            failed_result("disconnected"),
            &AtomicBool::new(false)
        ));
    }

    #[test]
    fn live_catalog_publishes_notices_once_and_canceled_catalog_publishes_nothing() {
        let (mut catalog, effect_type) = prepared_catalog();
        let result = catalog.results.try_recv().unwrap();
        let (sender, results) = mpsc::sync_channel(1);
        let mut result = result;
        result
            .preparation
            .notices
            .push(notice_batch(1, "metadata notice", "request-only"))
            .unwrap();
        assert!(sender.try_send(result).is_ok());
        drop(sender);
        catalog.results = results;
        let mut state = AppState::new();
        state.openfx_catalog = Some(catalog);
        let generation = state.project_author_generation();
        assert!(state.poll_openfx_catalog());
        assert!(mondrian_effects::effect_definition(&effect_type).is_some());
        assert_eq!(state.notifications.iter().count(), 1);
        let message = state.notifications.iter().next().unwrap();
        assert_eq!(
            message.message,
            AppMessage::new("notification-native-processor")
                .with_text("message", "metadata notice")
        );
        assert_eq!(message.severity, AppNotificationSeverity::Information);
        assert!(!state.poll_openfx_catalog());
        assert_eq!(state.notifications.iter().count(), 1);
        assert_eq!(state.project_author_generation(), generation);
        let (catalog, effect_type) = prepared_catalog();
        catalog.canceled.store(true, Ordering::Release);
        state.openfx_catalog = Some(catalog);
        assert!(!state.poll_openfx_catalog());
        assert!(mondrian_effects::effect_definition(&effect_type).is_none());
        assert_eq!(state.notifications.iter().count(), 1);
    }
    #[cfg(windows)]
    #[test]
    #[ignore = "requires current Product and SessionProbe aux_transient_null or aux_transient_descriptor"]
    fn native_describe_catalog_delivers_transient_notices_once_without_a_session() {
        let mode = std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("owned mode");
        assert!(matches!(
            mode.as_str(),
            "aux_transient_null" | "aux_transient_descriptor"
        ));
        let helper = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").expect("current product"),
        );
        let binary = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("owned fixture"),
        );
        let mut state = AppState::new();
        state.openfx_catalog = Some(OpenFxCatalog::start(helper).expect("catalog"));
        let generation = state.project_author_generation();
        state.install_openfx_bundle(binary.clone()).expect("queue actual Describe");
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut completed = Vec::new();
        while Instant::now() < deadline {
            state.poll_openfx_catalog();
            completed.extend(state.take_completed_openfx_installs());
            if !completed.is_empty() {
                break;
            }
            assert!(
                !state.status_hint.as_ref().is_some_and(|(_, failed)| *failed),
                "actual catalog metadata was refused"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(completed, vec![binary]);
        let notices = state.notifications.iter().collect::<Vec<_>>();
        assert_eq!(
            notices.len(),
            4,
            "two Info and two Warning; both Log records use tracing"
        );
        for (index, notice) in notices.iter().enumerate() {
            assert_eq!(
                notice.severity,
                if index < 2 {
                    AppNotificationSeverity::Information
                } else {
                    AppNotificationSeverity::Warning
                }
            );
            assert_eq!(
                notice.message,
                AppMessage::new("notification-native-processor").with_text(
                    "message",
                    if index % 2 == 0 {
                        "说明🙂 7 1.250"
                    } else {
                        "第二条通知✓"
                    }
                )
            );
        }
        state.poll_openfx_catalog();
        assert_eq!(state.notifications.iter().count(), 4);
        assert!(state.take_completed_openfx_installs().is_empty());
        assert_eq!(state.project_author_generation(), generation);
        // No Session or Render is created anywhere in this catalog-only path.
        drop(state);
    }
}
