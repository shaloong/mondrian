//! Machine-local OpenFX context selection, installation and startup restoration.

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
use crate::openfx_effect::{prepare_observed_openfx_effect, PreparedOpenFxEffect};
use crate::openfx_host::{
    observe_openfx_catalog_with_checkpoint, OpenFxContext, OpenFxDescribeNotices, OpenFxHostError,
};
use mondrian_effects::EffectProcessorNoticeKind;
use serde::{Deserialize, Serialize};

use super::notifications::AppNotificationSeverity;

use super::AppState;

/// One machine-local selection, independent of Project authoring identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OpenFxBundleSelection {
    pub path: PathBuf,
    pub context: OpenFxContext,
}

impl From<PathBuf> for OpenFxBundleSelection {
    fn from(path: PathBuf) -> Self {
        Self { path, context: OpenFxContext::Filter }
    }
}

impl<'de> Deserialize<'de> for OpenFxBundleSelection {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Current {
            path: PathBuf,
            context: OpenFxContext,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Current(Current),
            // Previous preferences recorded paths for Filter installation only.
            LegacyFilter(PathBuf),
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Current(value) => Self { path: value.path, context: value.context },
            Wire::LegacyFilter(path) => path.into(),
        })
    }
}

#[derive(Clone, Copy)]
enum CatalogPurpose {
    Install,
    Restore,
}

struct CatalogRequest {
    selection: OpenFxBundleSelection,
    purpose: CatalogPurpose,
}

struct CatalogResult {
    selection: OpenFxBundleSelection,
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
    fn record(&mut self, identifier: &str, prepared: PreparedOpenFxEffect) {
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
                    let preparation = install_bundle(&helper, &request.selection, &worker_canceled);
                    if worker_canceled.load(Ordering::Acquire) {
                        break;
                    }
                    if !send_catalog_result(
                        &result_sender,
                        CatalogResult {
                            selection: request.selection,
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
    /// Queue exactly the selected context without blocking the UI thread.
    pub(super) fn install_openfx_bundle(&mut self, selection: OpenFxBundleSelection) -> Result<()> {
        self.start_openfx_catalog()?;
        self.openfx_catalog
            .as_ref()
            .ok_or_else(|| catalog_error("OpenFX catalog is unavailable"))?
            .requests
            .as_ref()
            .ok_or_else(|| catalog_error("OpenFX catalog is closed"))?
            .try_send(CatalogRequest { selection, purpose: CatalogPurpose::Install })
            .map_err(|error| catalog_error(error.to_string()))?;
        self.set_status_hint(AppMessage::new("status-openfx-scanning"), false);
        Ok(())
    }

    /// Retry saved machine-local bundles without delaying window startup.
    pub(crate) fn schedule_openfx_catalog_restore(&mut self, bundles: Vec<OpenFxBundleSelection>) {
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
        for selection in bundles {
            if catalog
                .requests
                .as_ref()
                .expect("live catalog request sender")
                .try_send(CatalogRequest { selection, purpose: CatalogPurpose::Restore })
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
                    self.publish_openfx_metadata_notices(
                        result.preparation.notices,
                        result.selection.context,
                    );
                    // Registry mutation occurs only on this live App owner.
                    // A dropped catalog consumes/discards prepared definitions.
                    let outcome = publish_bundle(result.preparation.result);
                    changed = true;
                    match result.purpose {
                        CatalogPurpose::Install => match outcome {
                            Ok((installed, rejected)) => {
                                self.completed_openfx_installs.push(result.selection);
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
                                        tracing::warn!(path = %result.selection.path.display(), context = ?result.selection.context, %reason, "OpenFX effect was not restored");
                                    }
                                }
                                Err(reason) => {
                                    catalog.restore_failures += 1;
                                    tracing::warn!(path = %result.selection.path.display(), context = ?result.selection.context, %reason, "OpenFX bundle was not restored");
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

    fn publish_openfx_metadata_notices(&mut self, notices: CatalogNotices, context: OpenFxContext) {
        for batch in notices.batches {
            // This is the verified request context, not an observed emitter
            // class/action, native instance, Render invocation, or generation.
            if !batch.complete || batch.fatal {
                tracing::warn!(binary = %batch.binary_path.display(), sha256 = %batch.binary_sha256,
                    requested_class = %batch.requested_plugin.identifier,
                    requested_major = batch.requested_plugin.version_major, requested_minor = batch.requested_plugin.version_minor,
                    phase = "whole_metadata_request", complete = batch.complete, fatal = batch.fatal,
                    "OpenFX metadata notices accompany a refused definition");
            }
            for notice in batch.notices {
                if notice.kind() == EffectProcessorNoticeKind::Log {
                    tracing::info!(binary = %batch.binary_path.display(), sha256 = %batch.binary_sha256,
                        requested_class = %batch.requested_plugin.identifier,
                        requested_major = batch.requested_plugin.version_major, requested_minor = batch.requested_plugin.version_minor,
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
                        "openfx-metadata:{}:{}:{}.{}:{context:?}:{:?}:{}",
                        batch.binary_sha256,
                        batch.requested_plugin.identifier,
                        batch.requested_plugin.version_major,
                        batch.requested_plugin.version_minor,
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
    pub(crate) fn take_completed_openfx_installs(&mut self) -> Vec<OpenFxBundleSelection> {
        std::mem::take(&mut self.completed_openfx_installs)
    }
}

fn install_bundle(
    helper: &Path,
    selection: &OpenFxBundleSelection,
    canceled: &AtomicBool,
) -> BundlePreparation {
    let mut preparation = BundlePreparation {
        result: Ok((Vec::new(), Vec::new())),
        notices: CatalogNotices::default(),
    };
    let result = (|| {
        let inspection =
            inspect_openfx_binary_with_checkpoint(helper, &selection.path, &mut || {
                if canceled.load(Ordering::Acquire) {
                    Err(OpenFxDiscoveryError::Canceled)
                } else {
                    Ok(())
                }
            })
            .map_err(|error| error.to_string())?;
        let mut checkpoint = || {
            if canceled.load(Ordering::Acquire) {
                Err(OpenFxHostError::Canceled)
            } else {
                Ok(())
            }
        };
        observe_openfx_catalog_with_checkpoint(
            helper,
            &inspection,
            selection.context,
            &mut checkpoint,
            &mut |plugin, observed| {
                let prepared = prepare_observed_openfx_effect(
                    helper,
                    &inspection,
                    &plugin.identifier,
                    selection.context,
                    observed,
                );
                preparation.record(
                    &format!(
                        "{} {}.{} ({:?})",
                        plugin.identifier,
                        plugin.version_major,
                        plugin.version_minor,
                        selection.context
                    ),
                    prepared,
                );
                if let Err(error) = &preparation.result {
                    return Err(OpenFxHostError::Invalid(error.clone()));
                }
                Ok(())
            },
        )
        .map_err(|error| error.to_string())?;
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
            "selected bundle contains no supported OpenFX effect: {}",
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

    fn native_catalog_probe() -> (PathBuf, OpenFxBundleSelection, PathBuf) {
        let helper = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_PRODUCT_BINARY").unwrap());
        let binary =
            PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_CATALOG_PROBE_BINARY").unwrap());
        let log = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_CATALOG_PROBE_LOG").unwrap());
        (
            helper,
            OpenFxBundleSelection { path: binary, context: OpenFxContext::Generator },
            log,
        )
    }

    #[test]
    #[ignore = "owned out-of-order same-ID multi-version Catalog and actual Graph pixel gate"]
    fn native_catalog_versions_bind_metadata_and_graph_pixels_to_exact_version() {
        use mondrian_core::{
            Rational, SampleAspectRatio, TimelineTime, TimelineTimeRange, WorkingColorSpace,
        };
        use mondrian_effects::*;
        let (helper, selection, _) = native_catalog_probe();
        assert_eq!(
            std::env::var("MONDRIAN_OPENFX_CATALOG_PROBE_MODE").unwrap(),
            "versions"
        );
        let mut state = AppState::new();
        state.openfx_catalog = Some(OpenFxCatalog::start(helper).unwrap());
        state.install_openfx_bundle(selection.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut completed = Vec::new();
        while completed.is_empty() && Instant::now() < deadline {
            let changed = state.poll_openfx_catalog();
            completed.extend(state.take_completed_openfx_installs());
            assert!(
                !(changed
                    && completed.is_empty()
                    && state.status_hint.as_ref().is_some_and(|(_, failed)| *failed)),
                "{:?}",
                state.status_hint
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(completed, vec![selection]);
        let mut keys = std::collections::BTreeSet::new();
        for (major, minor, pixel_code) in [(1, 0, 1), (2, 0, 2), (2, 1, 3), (3, 0, 4)] {
            let label = format!("Catalog Probe v{major}.{minor} (Generator)");
            let kind = effect_library_types()
                .into_iter()
                .find(|kind| effect_definition(kind).is_some_and(|d| d.display_name() == label))
                .unwrap_or_else(|| panic!("exact metadata for version {major}.{minor} must exist"));
            assert!(
                keys.insert(kind.key().to_owned()),
                "versions must retain distinct existing Effect keys"
            );
            let effect = instantiate_effect_node(kind).unwrap();
            let geometry = EffectProjectGeometry::new(4, 2, SampleAspectRatio::SQUARE).unwrap();
            let context = EffectFrameContext::new(
                Rational::FPS_24,
                geometry,
                TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(13, 24).unwrap())
                    .unwrap(),
            )
            .unwrap();
            let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
                &[effect],
                &[],
                &[],
                &[],
                WorkingColorSpace::LinearRec709,
                context,
                &LutPreparationCache::uncached(),
            )
            .unwrap();
            let format = EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            );
            let sampling = mondrian_core::ImageSampling::new(
                SampleAspectRatio::SQUARE,
                [Rational::new(1, 1); 2],
            )
            .unwrap();
            let mut session = EffectExecutionSession::new(EffectExecutionSessionConfig {
                max_processor_instances: 1,
                max_processor_bytes: 128 * 1024 * 1024,
                ..EffectExecutionSessionConfig::uncached(128 * 1024 * 1024)
            });
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                for frame in [0, 6, 3, 6] {
                    let graph = program.evaluate(TimelineTime::new(frame, 24).unwrap()).unwrap();
                    let output = session
                        .generate_compiled_rgba_f32(
                            EffectFrameExtent::new(4, 2),
                            format,
                            sampling,
                            &graph,
                            -999,
                            &mut EffectProcessorControl::uncancelled(),
                        )
                        .unwrap();
                    let expected = [
                        pixel_code as f32 / 16.0,
                        pixel_code as f32 / 32.0,
                        0.25,
                        1.0,
                    ];
                    assert!(
                        output.pixels().iter().all(|pixel| *pixel == expected),
                        "actual pixels for version {major}.{minor}"
                    );
                }
            }));
            session.retire_processor_resources();
            assert_eq!(session.diagnostics().processor_instances, 0);
            drop(session);
            if let Err(panic) = outcome {
                std::panic::resume_unwind(panic);
            }
        }
        drop(state);
    }

    #[test]
    #[ignore = "owned changed-export version guard before any returned SetHost pointer"]
    fn native_catalog_changed_export_identity_refuses_before_set_host() {
        let (helper, selection, log) = native_catalog_probe();
        assert_eq!(
            std::env::var("MONDRIAN_OPENFX_CATALOG_PROBE_MODE").unwrap(),
            "swap_version"
        );
        let preparation = install_bundle(&helper, &selection, &AtomicBool::new(false));
        let (definitions, rejected) = preparation.result.unwrap();
        let labels: Vec<_> = definitions.iter().map(|d| d.display_name()).collect();
        assert_eq!(
            labels,
            vec![
                "Catalog Probe v1.0 (Generator)",
                "Catalog Probe v2.0 (Generator)",
                "Catalog Probe v3.0 (Generator)"
            ]
        );
        assert_eq!(rejected.len(), 1);
        assert!(rejected[0].contains("catalog-version 2.1"), "{rejected:?}");
        let text = std::fs::read_to_string(log).unwrap();
        assert!(
            !text.contains("unexpected_set_host"),
            "changed export must be refused before its function pointers are called"
        );
        assert!(
            !text.lines().any(|l| l.contains("|2|")),
            "the changed version cannot enter any native action"
        );
    }

    #[test]
    #[ignore = "owned foreign SetHost exception lifetime and cooperative worker retirement"]
    fn native_catalog_foreign_constructor_exception_retires_before_later_class() {
        let (helper, selection, log) = native_catalog_probe();
        assert_eq!(
            std::env::var("MONDRIAN_OPENFX_CATALOG_PROBE_MODE").unwrap(),
            "throw_set_host"
        );
        let preparation = install_bundle(&helper, &selection, &AtomicBool::new(false));
        let (definitions, rejected) = preparation.result.unwrap();
        assert_eq!(definitions.len(), 3);
        assert_eq!(rejected.len(), 1);
        assert!(
            rejected[0].contains("catalog-version 2.1") && rejected[0].contains("native status 6"),
            "{rejected:?}"
        );
        let text = std::fs::read_to_string(log).unwrap();
        assert!(
            text.contains("throwing_set_host") && text.contains("foreign_exception_destroyed"),
            "foreign exception must be destroyed while its module is alive"
        );
        assert!(
            !text.lines().any(|l| l.contains("|2|")),
            "failed SetHost cannot enter Load"
        );
        let load_pids: Vec<_> = text
            .lines()
            .filter(|l| l.ends_with("|OfxActionLoad"))
            .map(|l| l.split('|').next().unwrap())
            .collect();
        assert_eq!(load_pids.len(), 3);
        assert_eq!(load_pids[0], load_pids[1]);
        assert_ne!(
            load_pids[1], load_pids[2],
            "native constructor fault retires the process before the remaining class"
        );
    }

    #[test]
    #[ignore = "owned three-class Catalog probe; healthy and crash_middle are separate native gates"]
    fn native_catalog_reuses_process_and_preserves_class_fault_isolation() {
        let (helper, selection, log) = native_catalog_probe();
        let mode = std::env::var("MONDRIAN_OPENFX_CATALOG_PROBE_MODE").unwrap();
        assert!(matches!(mode.as_str(), "healthy" | "crash_middle"));
        let crashed = mode == "crash_middle";
        let preparation = install_bundle(&helper, &selection, &AtomicBool::new(false));
        let (definitions, rejected) = preparation.result.unwrap();
        assert_eq!(definitions.len(), if crashed { 2 } else { 3 });
        assert_eq!(rejected.len(), usize::from(crashed));
        if crashed {
            assert!(
                rejected[0].contains("catalog-b") && rejected[0].contains("23"),
                "{rejected:?}"
            );
        }
        let text = std::fs::read_to_string(log).unwrap();
        let rows: Vec<Vec<&str>> = text.lines().map(|line| line.split('|').collect()).collect();
        assert!(rows.iter().all(|row| row.len() == 3));
        let load_pids: Vec<_> =
            rows.iter().filter(|r| r[2] == "OfxActionLoad").map(|r| r[0]).collect();
        assert_eq!(load_pids.len(), 3);
        assert_eq!(
            load_pids[0], load_pids[1],
            "healthy classes share only their cold process"
        );
        assert_eq!(
            load_pids[1] == load_pids[2],
            !crashed,
            "a fault retires the worker before the remaining class"
        );
        for index in 0..3 {
            let index_text = index.to_string();
            let actions: Vec<_> =
                rows.iter().filter(|r| r[1] == index_text).map(|r| r[2]).collect();
            let expected = if crashed && index == 1 {
                vec!["OfxActionLoad"]
            } else {
                vec![
                    "OfxActionLoad",
                    "OfxActionDescribe",
                    "OfxImageEffectActionDescribeInContext",
                    "OfxActionUnload",
                ]
            };
            assert_eq!(actions, expected, "exact lifecycle for class {index}");
        }
        assert!(
            !mondrian_effects::effect_library_types().iter().any(|kind| {
                mondrian_effects::effect_definition(kind)
                    .is_some_and(|d| d.display_name() == "Catalog Probe (Generator)")
            }),
            "preparation cannot register definitions before live App publication"
        );
    }

    #[test]
    #[ignore = "owned block_middle Catalog probe under process supervision"]
    fn native_catalog_cancel_discards_prefix_and_reaps_blocked_class() {
        let (helper, selection, log) = native_catalog_probe();
        assert_eq!(
            std::env::var("MONDRIAN_OPENFX_CATALOG_PROBE_MODE").unwrap(),
            "block_middle"
        );
        let catalog = OpenFxCatalog::start(helper).unwrap();
        catalog
            .requests
            .as_ref()
            .unwrap()
            .try_send(CatalogRequest { selection, purpose: CatalogPurpose::Install })
            .unwrap_or_else(|_| panic!("idle catalog queue"));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .any(|line| line.ends_with("|1|OfxActionLoad"))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "middle class must enter before cancellation"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            catalog.results.try_recv(),
            Err(TryRecvError::Empty)
        ));
        let started = Instant::now();
        drop(catalog);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancel must settle the original child before returning"
        );
        let text = std::fs::read_to_string(log).unwrap();
        assert!(text.lines().any(|l| l.ends_with("|0|OfxActionUnload")));
        assert!(
            !text.lines().any(|l| l.contains("|2|")),
            "later class must not start after cancellation"
        );
        assert!(
            !mondrian_effects::effect_library_types().iter().any(|kind| {
                mondrian_effects::effect_definition(kind)
                    .is_some_and(|d| d.display_name() == "Catalog Probe (Generator)")
            }),
            "a completed prefix cannot escape a dropped Catalog"
        );
    }

    #[test]
    fn public_generator_install_dispatch_reaches_the_original_catalog_queue() {
        use crate::app::product_action::{
            ProductAction, VisualEffectInstallOpenFxBundlePayload, VisualEffectProductAction,
        };
        let (requests, receiver) = mpsc::sync_channel(1);
        let (_sender, results) = mpsc::sync_channel(1);
        let mut state = AppState::new();
        state.openfx_catalog = Some(OpenFxCatalog {
            requests: Some(requests),
            results,
            worker: None,
            canceled: Arc::new(AtomicBool::new(false)),
            restores_remaining: 0,
            restored: 0,
            restore_failures: 0,
        });
        let path = std::env::temp_dir().join("selected-generator.ofx.bundle");
        let generation = state.project_author_generation();
        state
            .dispatch_action(
                ProductAction::VisualEffect(VisualEffectProductAction::InstallOpenFxBundle(
                    VisualEffectInstallOpenFxBundlePayload {
                        path: path.clone(),
                        context: OpenFxContext::Generator,
                    },
                ))
                .into_external_action(),
            )
            .unwrap();
        let queued = receiver.try_recv().expect("public dispatch must enqueue actual Catalog work");
        assert_eq!(
            queued.selection,
            OpenFxBundleSelection { path, context: OpenFxContext::Generator }
        );
        assert_eq!(state.project_author_generation(), generation);
        assert!(state.take_completed_openfx_installs().is_empty());
    }

    #[test]
    #[ignore = "requires current Product and GeneratorProbe under native supervision"]
    fn native_wrong_context_never_falls_back_and_catalog_accepts_next_selection() {
        let helper = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_PRODUCT_BINARY").unwrap());
        let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_GENERATOR_BINARY").unwrap());
        let mut state = AppState::new();
        state.openfx_catalog = Some(OpenFxCatalog::start(helper).unwrap());
        let generation = state.project_author_generation();
        state.install_openfx_bundle(binary.clone().into()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !state.status_hint.as_ref().is_some_and(|(_, failed)| *failed)
            && Instant::now() < deadline
        {
            state.poll_openfx_catalog();
            thread::sleep(Duration::from_millis(5));
        }
        assert!(state.status_hint.as_ref().is_some_and(|(_, failed)| *failed));
        assert!(state.take_completed_openfx_installs().is_empty());
        assert!(
            !mondrian_effects::effect_library_types()
                .iter()
                .any(|kind| kind.key().starts_with("openfx.generator.")),
            "a failed Filter request cannot publish a Generator"
        );
        let selection = OpenFxBundleSelection { path: binary, context: OpenFxContext::Generator };
        state.install_openfx_bundle(selection.clone()).unwrap();
        let mut completed = Vec::new();
        while completed.is_empty() && Instant::now() < deadline {
            let changed = state.poll_openfx_catalog();
            completed.extend(state.take_completed_openfx_installs());
            assert!(
                !(changed
                    && completed.is_empty()
                    && state.status_hint.as_ref().is_some_and(|(_, failed)| *failed)),
                "Catalog reached failure before successful installation: {:?}",
                state.status_hint
            );
            if completed.is_empty() {
                thread::sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(completed, vec![selection]);
        assert!(mondrian_effects::effect_library_types()
            .iter()
            .any(|kind| kind.key().starts_with("openfx.generator.")));
        assert_eq!(state.project_author_generation(), generation);
        drop(state);
    }

    #[test]
    #[ignore = "requires current Product and real Misc bundle under native supervision"]
    fn native_generator_catalog_install_restore_and_public_timeline_roundtrip() {
        use crate::app::product_action::{
            ProductAction, VisualEffectInstallOpenFxBundlePayload, VisualEffectProductAction,
        };
        use crate::app_ui::preferences_store::{
            load_app_ui_preferences_from, persist_app_ui_preferences_to, AppUiPreferences,
        };
        use sha2::{Digest, Sha256};
        let helper = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_PRODUCT_BINARY").unwrap());
        let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_INVERT_BINARY").unwrap());
        let pinned = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary).unwrap();
        assert_eq!(
            pinned
                .plugins
                .iter()
                .filter(|p| p.identifier == "net.sf.openfx.ConstantPlugin"
                    && p.version_major == 1
                    && p.version_minor == 0)
                .count(),
            1
        );
        // Frozen persistent key contract: locate the exact installed class,
        // never a display label that differs between vendor builds/locales.
        // This does not register or replace any Catalog-produced definition.
        let identity = format!(
            "generator\0{}\0{}.{}\0{}",
            "net.sf.openfx.ConstantPlugin", 1, 0, pinned.binary_sha256
        );
        let kind = mondrian_effects::EffectType::External(format!(
            "openfx.generator.{:x}",
            Sha256::digest(identity.as_bytes())
        ));
        let bundle = binary.parent().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
        assert!(bundle.file_name().unwrap().to_string_lossy().ends_with(".ofx.bundle"));
        let selection = OpenFxBundleSelection {
            path: bundle.clone(),
            context: OpenFxContext::Generator,
        };
        let mut state = AppState::new();
        state.openfx_catalog = Some(OpenFxCatalog::start(helper.clone()).unwrap());
        let generation = state.project_author_generation();
        let deadline = Instant::now() + Duration::from_secs(120);
        state
            .dispatch_action(
                ProductAction::VisualEffect(VisualEffectProductAction::InstallOpenFxBundle(
                    VisualEffectInstallOpenFxBundlePayload {
                        path: bundle,
                        context: OpenFxContext::Generator,
                    },
                ))
                .into_external_action(),
            )
            .unwrap();
        let mut completed = Vec::new();
        while completed.is_empty() && Instant::now() < deadline {
            let changed = state.poll_openfx_catalog();
            completed.extend(state.take_completed_openfx_installs());
            assert!(
                !(changed
                    && completed.is_empty()
                    && state.status_hint.as_ref().is_some_and(|(_, failed)| *failed)),
                "Catalog reached failure before successful installation: {:?}",
                state.status_hint
            );
            if completed.is_empty() {
                thread::sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(
            completed,
            vec![selection.clone()],
            "selected-context installation must complete"
        );
        assert_eq!(state.project_author_generation(), generation);
        assert!(
            mondrian_effects::effect_library_types().contains(&kind),
            "real Catalog publication must expose the exact Constant1.0 Generator key; status={:?}",
            state.status_hint
        );
        let installed = mondrian_effects::effect_definition(&kind).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let preferences_path = temp.path().join("preferences.json");
        let mut preferences = AppUiPreferences::default();
        for selected in completed {
            preferences.record_openfx_bundle(selected);
        }
        persist_app_ui_preferences_to(&preferences_path, &preferences).unwrap();
        let loaded = load_app_ui_preferences_from(&preferences_path);
        assert_eq!(loaded.installed_openfx_bundles, vec![selection]);
        drop(state);

        let mut restored = AppState::new();
        restored.openfx_catalog = Some(OpenFxCatalog::start(helper).unwrap());
        let generation = restored.project_author_generation();
        restored.schedule_openfx_catalog_restore(loaded.installed_openfx_bundles);
        while restored.openfx_catalog.as_ref().unwrap().restores_remaining > 0
            && Instant::now() < deadline
        {
            restored.poll_openfx_catalog();
            if restored.openfx_catalog.as_ref().unwrap().restores_remaining > 0 {
                thread::sleep(Duration::from_millis(5));
            }
        }
        assert_eq!(
            restored.openfx_catalog.as_ref().unwrap().restores_remaining,
            0
        );
        assert!(
            restored.take_completed_openfx_installs().is_empty(),
            "restore does not manufacture a new user installation"
        );
        assert_eq!(restored.project_author_generation(), generation);
        let current = mondrian_effects::effect_definition(&kind).unwrap();
        assert!(
            !Arc::ptr_eq(&installed, &current),
            "startup restore must publish a fresh definition, not reuse the prior registry entry"
        );
        assert_eq!(current.key(), installed.key());
        drop(restored);
        // Reuse the actual public author/Preview/Export/history/durable Project
        // gate with this Catalog-produced definition. No direct registration.
        super::super::contextual_effect_authoring_tests::qualify_native_generator_timeline(Some(
            kind,
        ));
    }

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
        state.install_openfx_bundle(missing.clone().into()).expect("queue install");
        state.schedule_openfx_catalog_restore(vec![missing.into()]);
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
                selection: PathBuf::from("prepared-catalog-control.ofx").into(),
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
            vec![PathBuf::from("prepared-catalog-control.ofx").into()]
        );
    }
    fn notice_batch(count: usize, text: &str, id: &str) -> OpenFxDescribeNotices {
        OpenFxDescribeNotices {
            binary_path: PathBuf::from("verified-request.ofx"),
            binary_sha256: "a".repeat(64),
            requested_plugin: crate::openfx_adapter::OpenFxPluginDescriptor {
                identifier: "requested.class".into(),
                api_version: 1,
                version_major: 1,
                version_minor: 0,
            },
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
    fn metadata_notices_from_distinct_versions_do_not_overwrite_each_other() {
        let mut state = AppState::new();
        let generation = state.project_author_generation();
        let mut notices = CatalogNotices::default();
        for (major, minor) in [(1, 0), (2, 0), (1, 1)] {
            let mut batch = notice_batch(1, "same notice id, distinct version", "shared");
            batch.requested_plugin.version_major = major;
            batch.requested_plugin.version_minor = minor;
            notices.push(batch).unwrap();
        }
        state.publish_openfx_metadata_notices(notices, OpenFxContext::Generator);
        assert_eq!(state.notifications.iter().count(), 3);
        assert_eq!(state.project_author_generation(), generation);
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
            PreparedOpenFxEffect {
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
            PreparedOpenFxEffect {
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
                PreparedOpenFxEffect {
                    definition: Err(crate::openfx_effect::OpenFxEffectRegistrationError::Schema(
                        "owned refusal".into(),
                    )),
                    notices: Some(notice_batch(32, "x", "")),
                },
            );
        }
        preparation.record(
            "overflow",
            PreparedOpenFxEffect {
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
            selection: PathBuf::from(path).into(),
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
        let mut retained = failed_result("second-result-must-not-be-cloned");
        retained.selection.context = OpenFxContext::Generator;
        let allocation = retained.selection.path.as_os_str().as_encoded_bytes().as_ptr() as usize;
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
        assert_eq!(
            receiver.recv().unwrap().selection.path,
            PathBuf::from("first")
        );
        assert!(completion.recv_timeout(Duration::from_secs(2)).unwrap());
        let second = receiver.recv().unwrap();
        assert_eq!(
            second.selection.path,
            PathBuf::from("second-result-must-not-be-cloned")
        );
        assert_eq!(
            second.selection.path.as_os_str().as_encoded_bytes().as_ptr() as usize,
            allocation
        );
        assert_eq!(second.selection.context, OpenFxContext::Generator);
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
        state
            .install_openfx_bundle(binary.clone().into())
            .expect("queue actual Describe");
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
        assert_eq!(completed, vec![binary.into()]);
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
