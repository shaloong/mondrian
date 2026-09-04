//! Owning App UI Host construction and exact failed-start cleanup.
//!
//! The owner takes the unique App before preferences, theme, native workers or
//! Widget models are touched. Every returned execution owner is installed here
//! before the next construction step can fail.

use super::*;

use crate::app::preview_runtime::PreviewStartupOwner;
use crate::app::preview_shutdown_evidence::PreviewStartupShutdownEvidence;
#[cfg(any(test, feature = "validation"))]
use crate::app::waveform_service::AudioWaveformStartupStage;
use crate::app::waveform_service::{
    AudioWaveformStartupFailure, AudioWaveformStartupShutdownEvidence,
};

/// Last fully owned Host construction stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppUiHostStartupStage {
    /// The exact caller App is held before any other work.
    AppOwned,
    /// Preferences and their persistence path are held.
    PreferencesLoaded,
    /// App runtime intent and the process theme were applied transactionally.
    AppConfigured,
    /// Both Thumbnail transports and inert state are owned.
    ThumbnailPrepared,
    /// Thumbnail native startup returned to the installed Adapter.
    ThumbnailStarted,
    /// Thumbnail color context was synchronized.
    ThumbnailConfigured,
    /// A complete Waveform owner was installed.
    WaveformStarted,
    /// Waveform Project-library state was synchronized.
    WaveformConfigured,
    /// A complete Preview owner was installed.
    PreviewStarted,
    /// Preview transport and signal monitoring were synchronized.
    PreviewConfigured,
    /// Shared execution-resource policy was applied.
    ResourcePolicyApplied,
    /// The initial Preview projection was resolved.
    InitialPreviewResolved,
    /// The inert device-catalog Adapter is owned.
    CatalogPrepared,
    /// Required native catalog startup returned success or ordinary failure.
    CatalogAttempted,
    /// The complete Widget root was constructed.
    RootBuilt,
    /// Catalog state and Viewer playback feedback were installed in the root.
    RootConfigured,
    /// Recovery candidates and startup rows were fully built.
    RecoveryLoaded,
    /// No fallible work remains before the complete Host is published.
    Ready,
}

/// Owner category that produced the primary Host startup failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppUiHostStartupFailureKind {
    /// Host composition or a named Host checkpoint unwound.
    Host,
    /// Waveform's owning factory returned a partial owner.
    Waveform,
    /// Preview's owning factory returned a partial owner.
    Preview,
}

/// Owner-free primary startup diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, thiserror::Error)]
#[error("App UI Host {kind:?} startup failed: {detail}; opaque_payload_abandoned={opaque_payload_abandoned}")]
pub struct AppUiHostStartupDiagnostic {
    /// Module that retained the failed startup owner.
    pub kind: AppUiHostStartupFailureKind,
    /// Canonical diagnostic text captured before consuming any owner.
    pub detail: String,
    /// An unknown panic payload was retained without running its destructor.
    pub opaque_payload_abandoned: bool,
}

/// Exact Waveform owner shape consumed after failed Host startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppUiHostStartupWaveformShutdown {
    /// Host failed before Waveform construction.
    NotCreated,
    /// Waveform construction failed with an owned partial inventory.
    Partial(AudioWaveformStartupShutdownEvidence),
    /// Complete Waveform construction preceded the Host failure.
    Complete(AudioWaveformShutdownEvidence),
}

/// Exact Preview owner shape consumed after failed Host startup.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppUiHostStartupPreviewShutdown {
    /// Host failed before Preview construction.
    NotCreated,
    /// Preview construction failed with an owned partial inventory.
    Partial(PreviewStartupShutdownEvidence),
    /// Complete Preview construction preceded the Host failure.
    Complete(PreviewRuntimeShutdownEvidence),
}

/// Exact catalog inventory and its consuming shutdown receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct AppUiHostStartupCatalogShutdown {
    /// Fresh Adapter state captured before cleanup.
    pub startup: AudioDeviceCatalogStartupState,
    /// Unmodified catalog receipt.
    pub shutdown: AudioDeviceCatalogShutdownEvidence,
}

/// Owner-derived closure facts for an unpublished Host.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiHostStartupShutdownEvidence {
    /// Receipt schema.
    pub schema_version: u32,
    /// Last fully installed Host stage.
    pub stage: AppUiHostStartupStage,
    /// Module that produced the original failure.
    pub failure_kind: AppUiHostStartupFailureKind,
    /// Unknown panic ownership prevents a clean whole-startup claim.
    pub opaque_panic_payload_abandoned: bool,
    /// App audio/display intents were restored before returning the App.
    pub app_configuration_restored: bool,
    /// The exact pre-start process theme was restored.
    pub theme_restored: bool,
    /// Thumbnail was absent or consumed through its real lifecycle Module.
    pub thumbnail: Option<
        Result<
            crate::app::thumbnail_service::ThumbnailShutdownEvidence,
            crate::app::thumbnail_service::ThumbnailShutdownUnavailable,
        >,
    >,
    /// Mutually exclusive absent/partial/complete Waveform receipt.
    pub waveform: AppUiHostStartupWaveformShutdown,
    /// Mutually exclusive absent/partial/complete Preview receipt.
    pub preview: AppUiHostStartupPreviewShutdown,
    /// Catalog was absent or consumed with its exact startup state.
    pub catalog: Option<AppUiHostStartupCatalogShutdown>,
}

impl AppUiHostStartupShutdownEvidence {
    /// Prove closure of exactly the resources created before the Host failed.
    pub fn all_created_resources_released(&self) -> bool {
        if self.schema_version != 1
            || self.opaque_panic_payload_abandoned
            || !self.app_configuration_restored
            || !self.theme_restored
            || !self.thumbnail_matches_stage()
            || !self.waveform_matches_stage()
            || !self.preview_matches_stage()
            || !self.catalog_matches_stage()
        {
            return false;
        }
        match self.failure_kind {
            AppUiHostStartupFailureKind::Host => true,
            AppUiHostStartupFailureKind::Waveform => {
                self.stage == AppUiHostStartupStage::ThumbnailConfigured
                    && matches!(self.waveform, AppUiHostStartupWaveformShutdown::Partial(_))
            }
            AppUiHostStartupFailureKind::Preview => {
                self.stage == AppUiHostStartupStage::WaveformConfigured
                    && matches!(self.preview, AppUiHostStartupPreviewShutdown::Partial(_))
            }
        }
    }

    fn thumbnail_matches_stage(&self) -> bool {
        let expected = self.stage >= AppUiHostStartupStage::ThumbnailPrepared;
        match self.thumbnail {
            Some(Ok(receipt)) => expected && receipt.all_created_resources_released(),
            Some(Err(_)) => false,
            None => !expected,
        }
    }

    fn waveform_matches_stage(&self) -> bool {
        match &self.waveform {
            AppUiHostStartupWaveformShutdown::NotCreated => {
                self.stage < AppUiHostStartupStage::WaveformStarted
                    && self.failure_kind != AppUiHostStartupFailureKind::Waveform
            }
            AppUiHostStartupWaveformShutdown::Partial(receipt) => {
                self.failure_kind == AppUiHostStartupFailureKind::Waveform
                    && receipt.all_created_resources_released()
            }
            AppUiHostStartupWaveformShutdown::Complete(receipt) => {
                self.stage >= AppUiHostStartupStage::WaveformStarted
                    && receipt.all_resources_released()
            }
        }
    }

    fn preview_matches_stage(&self) -> bool {
        match &self.preview {
            AppUiHostStartupPreviewShutdown::NotCreated => {
                self.stage < AppUiHostStartupStage::PreviewStarted
                    && self.failure_kind != AppUiHostStartupFailureKind::Preview
            }
            AppUiHostStartupPreviewShutdown::Partial(receipt) => {
                self.failure_kind == AppUiHostStartupFailureKind::Preview
                    && receipt.all_created_resources_released()
            }
            AppUiHostStartupPreviewShutdown::Complete(receipt) => {
                self.stage >= AppUiHostStartupStage::PreviewStarted
                    && receipt.all_workers_terminated()
            }
        }
    }

    fn catalog_matches_stage(&self) -> bool {
        let expected = self.stage >= AppUiHostStartupStage::CatalogPrepared;
        match self.catalog {
            Some(receipt) => {
                expected && receipt.shutdown.all_created_resources_released(receipt.startup)
            }
            None => !expected,
        }
    }
}

enum WaveformStartupOwner {
    Complete(Arc<AudioWaveformService>),
    Partial(AudioWaveformStartupFailure),
}

enum PreviewStartupHostOwner {
    Complete(Box<WindowPreviewAdapter>),
    Partial(PreviewStartupOwner<mondrian_ui_widgets::ViewerExternalTextureFrame>),
}

struct OriginalHostConfiguration {
    audio_output: mondrian_media::RealtimeAudioOutputDeviceSelection,
    display: mondrian_core::DisplayManagementPolicy,
    theme: mondrian_ui_theme::Theme,
}

struct AppUiHostStartupOwner {
    app_state: Option<AppState>,
    preferences: Option<AppUiPreferences>,
    preferences_path: Option<PathBuf>,
    original_configuration: Option<OriginalHostConfiguration>,
    thumbnail: Option<AssetThumbnailAdapter>,
    waveform: Option<WaveformStartupOwner>,
    preview: Option<PreviewStartupHostOwner>,
    catalog: Option<AudioOutputDeviceCatalogAdapter>,
    catalog_startup: Option<AudioDeviceCatalogStartupState>,
    stage: AppUiHostStartupStage,
    #[cfg(any(test, feature = "validation"))]
    fault: Option<AppUiHostStartupFault>,
}

/// Owning failed Host startup. Consume it to recover the exact App and receipts.
#[must_use = "consume the failed Host owner under the original deadline"]
pub struct AppUiHostStartupFailure {
    diagnostic: AppUiHostStartupDiagnostic,
    owner: AppUiHostStartupOwner,
}

impl std::fmt::Debug for AppUiHostStartupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppUiHostStartupFailure")
            .field("diagnostic", &self.diagnostic)
            .field("stage", &self.owner.stage)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Display for AppUiHostStartupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.diagnostic.fmt(formatter)
    }
}

impl std::error::Error for AppUiHostStartupFailure {}

impl AppUiHostStartupFailure {
    /// Borrow the owner-free primary diagnostic before consuming cleanup.
    pub fn diagnostic(&self) -> &AppUiHostStartupDiagnostic {
        &self.diagnostic
    }

    /// Close every created UI owner and return the same live App.
    pub fn shutdown_until(mut self, deadline: Instant) -> AppUiHostStartupClosed {
        self.owner.begin_shutdown();
        let shutdown = self.owner.consume_shutdown(
            deadline,
            self.diagnostic.kind,
            self.diagnostic.opaque_payload_abandoned,
        );
        let app_state = self
            .owner
            .app_state
            .take()
            .expect("Host startup owner must retain its unique App");
        AppUiHostStartupClosed { app_state, diagnostic: self.diagnostic, shutdown }
    }
}

/// Consumed Host startup failure with the original App and raw UI receipts.
pub struct AppUiHostStartupClosed {
    /// Exact App that entered Host startup; it remains live and unconsumed.
    pub app_state: AppState,
    /// Original startup diagnostic, never replaced by cleanup failure.
    pub diagnostic: AppUiHostStartupDiagnostic,
    /// Raw partial/complete UI owner closure evidence.
    pub shutdown: AppUiHostStartupShutdownEvidence,
}

/// One actual failed-start case consumed by the production-linked qualifier.
#[cfg(feature = "validation")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiHostStartupQualificationCase {
    /// Stable injected fault label.
    pub fault: String,
    /// Owner-free primary diagnostic captured before cleanup.
    pub diagnostic: AppUiHostStartupDiagnostic,
    /// Raw owner-derived cleanup receipt.
    pub shutdown: AppUiHostStartupShutdownEvidence,
}

/// Production-linked Host owning-startup qualification result.
#[cfg(feature = "validation")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppUiHostStartupQualificationReport {
    /// Receipt schema.
    pub schema_version: u32,
    /// Normal product and explicit-preferences constructions closed cleanly.
    pub successful_routes: u32,
    /// Every injected partial-start owner and its consuming receipt.
    pub failed_start_cases: Vec<AppUiHostStartupQualificationCase>,
    /// An opaque panic owner was deliberately abandoned and rejected as clean.
    pub opaque_payload_fail_closed: bool,
}

/// Failure of the production-linked Host owning-startup qualifier.
#[cfg(feature = "validation")]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Host owning-startup qualification failed: {detail}")]
pub struct AppUiHostStartupQualificationError {
    /// Exact failed expectation and its available evidence.
    pub detail: String,
}

#[cfg(any(test, feature = "validation"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppUiHostStartupFault {
    /// Unwind after a complete Host stage is installed.
    Host(AppUiHostStartupStage),
    /// Unwind with an opaque owner whose destructor must never run here.
    HostOpaque(AppUiHostStartupStage),
    /// Unwind inside Waveform after its prepared owner exists.
    WaveformPrepared,
    /// Unwind inside Waveform after its Source Cache is installed.
    WaveformSourceCache,
    /// Unwind inside Waveform after its analysis worker is installed.
    WaveformAnalysisWorker,
    /// Unwind inside Preview after its cache startup step.
    PreviewCache,
    /// Unwind inside Preview after its visual worker startup step.
    PreviewVisual,
    /// Unwind inside Preview after its CPU fallback startup step.
    PreviewCpuFallback,
    /// Unwind inside Preview after one media-worker startup step.
    PreviewMedia(usize),
    /// Unwind inside Preview after its dependency observer startup step.
    PreviewObserver,
}

impl AppUiHostStartupOwner {
    fn new(app_state: AppState) -> Self {
        Self {
            app_state: Some(app_state),
            preferences: None,
            preferences_path: None,
            original_configuration: None,
            thumbnail: None,
            waveform: None,
            preview: None,
            catalog: None,
            catalog_startup: None,
            stage: AppUiHostStartupStage::AppOwned,
            #[cfg(any(test, feature = "validation"))]
            fault: None,
        }
    }

    fn checkpoint(&mut self, stage: AppUiHostStartupStage) {
        self.stage = stage;
        #[cfg(any(test, feature = "validation"))]
        match self.fault {
            Some(AppUiHostStartupFault::Host(target)) if target == stage => {
                panic!("injected Host startup failure after {stage:?}");
            }
            Some(AppUiHostStartupFault::HostOpaque(target)) if target == stage => {
                std::panic::panic_any(HostOpaqueQualificationPayload);
            }
            _ => {}
        }
    }

    fn construct(
        &mut self,
        explicit: Option<(AppUiPreferences, PathBuf)>,
    ) -> Result<AppUiHost, AppUiHostStartupDiagnostic> {
        self.checkpoint(AppUiHostStartupStage::AppOwned);
        let (preferences, preferences_path) = explicit.unwrap_or_else(|| {
            let path = app_ui_preferences_path();
            (load_app_ui_preferences_from(&path), path)
        });
        self.preferences = Some(preferences);
        self.preferences_path = Some(preferences_path);
        self.checkpoint(AppUiHostStartupStage::PreferencesLoaded);

        let preferences_snapshot =
            self.preferences.as_ref().expect("preferences installed").clone();
        {
            let app_state = self.app_state.as_mut().expect("App installed");
            let theme = mondrian_ui_theme::current_theme().clone();
            self.original_configuration = Some(OriginalHostConfiguration {
                audio_output: app_state.audio_output_device_selection(),
                display: app_state.viewer_display_management().clone(),
                theme,
            });
            app_state.set_audio_output_device_selection(
                preferences_snapshot.audio_output_device.clone(),
            );
            app_state
                .set_viewer_display_management(preferences_snapshot.display_management.clone());
        }
        let system_theme_preset = ThemePreset::Dark;
        set_theme_preset(preferences_snapshot.theme_preference.resolve(system_theme_preset));
        self.checkpoint(AppUiHostStartupStage::AppConfigured);

        self.thumbnail = Some(AssetThumbnailAdapter::prepare());
        self.checkpoint(AppUiHostStartupStage::ThumbnailPrepared);
        self.thumbnail.as_ref().expect("Thumbnail installed").start_in_place();
        self.checkpoint(AppUiHostStartupStage::ThumbnailStarted);
        self.thumbnail.as_ref().expect("Thumbnail installed").set_color_context(
            self.app_state.as_ref().expect("App installed").thumbnail_color_context().ok(),
        );
        self.checkpoint(AppUiHostStartupStage::ThumbnailConfigured);

        let waveform = self.start_waveform()?;
        self.waveform = Some(WaveformStartupOwner::Complete(waveform));
        self.checkpoint(AppUiHostStartupStage::WaveformStarted);
        let library = self.app_state.as_ref().expect("App installed").asset_library_handle();
        self.complete_waveform().set_library(library);
        self.checkpoint(AppUiHostStartupStage::WaveformConfigured);

        let preview = self.start_preview()?;
        self.preview = Some(PreviewStartupHostOwner::Complete(Box::new(preview)));
        self.checkpoint(AppUiHostStartupStage::PreviewStarted);
        let preview_transport_intent =
            self.app_state.as_ref().expect("App installed").preview_transport_intent();
        self.complete_preview().synchronize_transport_intent(preview_transport_intent);
        self.complete_preview().set_viewer_signal_monitoring(
            preferences_snapshot.video_scopes.tap,
            preferences_snapshot.video_scopes.monitoring,
        );
        self.checkpoint(AppUiHostStartupStage::PreviewConfigured);
        {
            let app_state = self.app_state.as_ref().expect("App installed");
            apply_execution_resource_policy(
                app_state,
                self.thumbnail.as_ref().expect("Thumbnail installed"),
                self.complete_waveform(),
                self.complete_preview(),
            );
        }
        self.checkpoint(AppUiHostStartupStage::ResourcePolicyApplied);
        let window_preview_state = self
            .complete_preview()
            .viewer_preview_for_state(self.app_state.as_ref().expect("App installed"));
        self.checkpoint(AppUiHostStartupStage::InitialPreviewResolved);

        self.catalog = Some(AudioOutputDeviceCatalogAdapter::prepare());
        self.catalog_startup = Some(AudioDeviceCatalogStartupState::Prepared);
        self.checkpoint(AppUiHostStartupStage::CatalogPrepared);
        if !cfg!(test) {
            self.catalog_startup = Some(AudioDeviceCatalogStartupState::InProgress);
            let started = self.catalog.as_mut().expect("catalog installed").request_refresh();
            self.catalog_startup = Some(if started {
                AudioDeviceCatalogStartupState::Started
            } else {
                AudioDeviceCatalogStartupState::OrdinaryFailed
            });
        }
        self.checkpoint(AppUiHostStartupStage::CatalogAttempted);

        let mut root = {
            let window_preview_snapshot =
                WindowPreviewSnapshot::new(&window_preview_state, self.complete_preview());
            AppUiAppRoot::from_app_state_with_preferences_thumbnails_and_preview(
                self.app_state.as_ref().expect("App installed"),
                &preferences_snapshot,
                self.thumbnail
                    .as_ref()
                    .map(|thumbnail| thumbnail as &dyn crate::app_ui::panels::AssetThumbnailSource),
                Some(&window_preview_snapshot),
                Some(self.complete_waveform().source()),
            )
        };
        self.checkpoint(AppUiHostStartupStage::RootBuilt);
        root.set_audio_output_device_catalog(
            self.catalog.as_ref().expect("catalog installed").state().clone(),
        );
        let playback_feedback = root.viewer_playback_feedback();
        self.checkpoint(AppUiHostStartupStage::RootConfigured);
        let mode = if self.app_state.as_ref().expect("App installed").has_open_project() {
            AppUiMode::Workspace
        } else {
            AppUiMode::Startup
        };
        let recovery_candidates = discover_crash_recovery_candidates();
        let mut startup = AppUiStartupScreen::new();
        startup.set_recent_projects(startup_recent_projects_from_preferences(
            &preferences_snapshot,
        ));
        startup.set_recovery_projects(startup_recovery_projects_from_candidates(
            &recovery_candidates,
        ));
        self.checkpoint(AppUiHostStartupStage::RecoveryLoaded);
        self.checkpoint(AppUiHostStartupStage::Ready);

        let app_state = self.app_state.take().expect("App installed");
        let preferences = self.preferences.take().expect("preferences installed");
        let preferences_path = self.preferences_path.take().expect("preferences path installed");
        let asset_thumbnails = self.thumbnail.take().expect("Thumbnail installed");
        let waveform_service = match self.waveform.take().expect("Waveform installed") {
            WaveformStartupOwner::Complete(service) => service,
            WaveformStartupOwner::Partial(_) => unreachable!("partial Waveform cannot publish"),
        };
        let preview_service = match self.preview.take().expect("Preview installed") {
            PreviewStartupHostOwner::Complete(preview) => *preview,
            PreviewStartupHostOwner::Partial(_) => unreachable!("partial Preview cannot publish"),
        };
        let audio_device_catalog = self.catalog.take().expect("catalog installed");
        Ok(AppUiHost {
            startup,
            root,
            app_state: RefCell::new(app_state),
            preferences,
            preferences_path,
            recovery_candidates,
            asset_thumbnails,
            audio_device_catalog,
            waveform_service,
            preview_service,
            window_preview_state: RefCell::new(window_preview_state),
            playback_feedback,
            last_playback_frame_advance_at: Cell::new(None),
            mode,
            system_theme_preset,
            ui_dirty: Cell::new(false),
            preview_dirty: Cell::new(false),
            pending_close_action: None,
            quiescing_close_action: None,
            pending_gallery_capture_name: RefCell::new(None),
        })
    }

    fn start_waveform(&mut self) -> Result<Arc<AudioWaveformService>, AppUiHostStartupDiagnostic> {
        #[cfg(any(test, feature = "validation"))]
        let result = match self.fault {
            Some(AppUiHostStartupFault::WaveformPrepared) => {
                AudioWaveformService::try_start_with_checkpoint_for_host(|stage| {
                    if stage == AudioWaveformStartupStage::Prepared {
                        panic!("injected Waveform prepared failure");
                    }
                })
            }
            Some(AppUiHostStartupFault::WaveformSourceCache) => {
                AudioWaveformService::try_start_with_checkpoint_for_host(|stage| {
                    if stage == AudioWaveformStartupStage::SourceCache {
                        panic!("injected Waveform Source Cache failure");
                    }
                })
            }
            Some(AppUiHostStartupFault::WaveformAnalysisWorker) => {
                AudioWaveformService::try_start_with_checkpoint_for_host(|stage| {
                    if stage == AudioWaveformStartupStage::AnalysisWorker {
                        panic!("injected Waveform worker failure");
                    }
                })
            }
            _ => AudioWaveformService::try_start(),
        };
        #[cfg(not(any(test, feature = "validation")))]
        let result = AudioWaveformService::try_start();
        match result {
            Ok(service) => Ok(service),
            Err(failure) => {
                let diagnostic = AppUiHostStartupDiagnostic {
                    kind: AppUiHostStartupFailureKind::Waveform,
                    detail: failure.diagnostic().to_string(),
                    opaque_payload_abandoned: failure.diagnostic().opaque_payload_abandoned,
                };
                self.waveform = Some(WaveformStartupOwner::Partial(failure));
                Err(diagnostic)
            }
        }
    }

    fn start_preview(&mut self) -> Result<WindowPreviewAdapter, AppUiHostStartupDiagnostic> {
        #[cfg(any(test, feature = "validation"))]
        let result = self.fault.and_then(preview_fault_checkpoint).map_or_else(
            WindowPreviewAdapter::try_new,
            |target| {
                WindowPreviewAdapter::try_start_with_checkpoint_for_host(move |actual| {
                    if actual == target {
                        panic!("injected Preview {actual:?} failure");
                    }
                })
            },
        );
        #[cfg(not(any(test, feature = "validation")))]
        let result = WindowPreviewAdapter::try_new();
        match result {
            Ok(preview) => Ok(preview),
            Err(failure) => {
                let (error, owner): (anyhow::Error, PreviewStartupOwner<_>) = failure.into_parts();
                let diagnostic = AppUiHostStartupDiagnostic {
                    kind: AppUiHostStartupFailureKind::Preview,
                    detail: error.to_string(),
                    opaque_payload_abandoned:
                        crate::app::execution_panic_diagnostic::opaque_panic_payload_abandoned(
                            &error,
                        ),
                };
                self.preview = Some(PreviewStartupHostOwner::Partial(owner));
                Err(diagnostic)
            }
        }
    }

    fn complete_waveform(&self) -> &Arc<AudioWaveformService> {
        match self.waveform.as_ref().expect("Waveform installed") {
            WaveformStartupOwner::Complete(service) => service,
            WaveformStartupOwner::Partial(_) => unreachable!("partial Waveform is not usable"),
        }
    }

    fn complete_preview(&self) -> &WindowPreviewAdapter {
        match self.preview.as_ref().expect("Preview installed") {
            PreviewStartupHostOwner::Complete(preview) => preview,
            PreviewStartupHostOwner::Partial(_) => unreachable!("partial Preview is not usable"),
        }
    }

    fn begin_shutdown(&mut self) {
        if let Some(thumbnail) = &self.thumbnail {
            thumbnail.begin_shutdown();
        }
        if let Some(waveform) = &self.waveform {
            match waveform {
                WaveformStartupOwner::Complete(service) => service.begin_shutdown(),
                WaveformStartupOwner::Partial(failure) => failure.begin_shutdown(),
            }
        }
        if let Some(preview) = &mut self.preview {
            match preview {
                PreviewStartupHostOwner::Complete(runtime) => runtime.begin_endurance_shutdown(),
                PreviewStartupHostOwner::Partial(owner) => owner.begin_shutdown(),
            }
        }
        if let Some(catalog) = &mut self.catalog {
            catalog.begin_shutdown();
        }
    }

    fn consume_shutdown(
        &mut self,
        deadline: Instant,
        failure_kind: AppUiHostStartupFailureKind,
        opaque_panic_payload_abandoned: bool,
    ) -> AppUiHostStartupShutdownEvidence {
        let preview =
            self.preview
                .take()
                .map_or(
                    AppUiHostStartupPreviewShutdown::NotCreated,
                    |owner| match owner {
                        PreviewStartupHostOwner::Complete(runtime) => {
                            AppUiHostStartupPreviewShutdown::Complete(
                                (*runtime).shutdown_until(deadline),
                            )
                        }
                        PreviewStartupHostOwner::Partial(owner) => {
                            AppUiHostStartupPreviewShutdown::Partial(owner.shutdown_until(deadline))
                        }
                    },
                );
        let waveform =
            self.waveform
                .take()
                .map_or(
                    AppUiHostStartupWaveformShutdown::NotCreated,
                    |owner| match owner {
                        WaveformStartupOwner::Complete(service) => {
                            AppUiHostStartupWaveformShutdown::Complete(
                                service.shutdown_until(deadline),
                            )
                        }
                        WaveformStartupOwner::Partial(failure) => {
                            AppUiHostStartupWaveformShutdown::Partial(
                                failure.shutdown_until(deadline),
                            )
                        }
                    },
                );
        let thumbnail = self.thumbnail.as_ref().map(|owner| owner.shutdown_until(deadline));
        self.thumbnail = None;
        let catalog = self.catalog.as_mut().map(|owner| AppUiHostStartupCatalogShutdown {
            startup: self.catalog_startup.unwrap_or(AudioDeviceCatalogStartupState::InProgress),
            shutdown: owner.shutdown_until(deadline),
        });
        self.catalog = None;
        let (app_configuration_restored, theme_restored) = self.restore_configuration();
        AppUiHostStartupShutdownEvidence {
            schema_version: 1,
            stage: self.stage,
            failure_kind,
            opaque_panic_payload_abandoned,
            app_configuration_restored,
            theme_restored,
            thumbnail,
            waveform,
            preview,
            catalog,
        }
    }

    fn restore_configuration(&mut self) -> (bool, bool) {
        let Some(original) = self.original_configuration.take() else {
            return (true, true);
        };
        let Some(app_state) = self.app_state.as_mut() else {
            return (false, false);
        };
        app_state.set_audio_output_device_selection(original.audio_output.clone());
        app_state.set_viewer_display_management(original.display.clone());
        mondrian_ui_theme::set_theme(original.theme.clone());
        let app_configuration_restored = app_state.audio_output_device_selection()
            == original.audio_output
            && app_state.viewer_display_management() == &original.display;
        let theme_restored = *mondrian_ui_theme::current_theme() == original.theme;
        (app_configuration_restored, theme_restored)
    }
}

impl Drop for AppUiHostStartupOwner {
    fn drop(&mut self) {
        // This is deliberately only a fallback, not qualification evidence.
        // Signal every execution owner before service-first field destruction.
        self.begin_shutdown();
        self.preview.take();
        self.waveform.take();
        self.thumbnail.take();
        self.catalog.take();
        self.app_state.take();
    }
}

#[cfg(any(test, feature = "validation"))]
fn preview_fault_checkpoint(
    fault: AppUiHostStartupFault,
) -> Option<crate::app::preview_runtime::PreviewStartupCheckpoint> {
    use crate::app::preview_runtime::PreviewStartupCheckpoint as Checkpoint;
    match fault {
        AppUiHostStartupFault::PreviewCache => Some(Checkpoint::Cache),
        AppUiHostStartupFault::PreviewVisual => Some(Checkpoint::Visual),
        AppUiHostStartupFault::PreviewCpuFallback => Some(Checkpoint::CpuFallback),
        AppUiHostStartupFault::PreviewMedia(index) => Some(Checkpoint::Media(index)),
        AppUiHostStartupFault::PreviewObserver => Some(Checkpoint::Observer),
        AppUiHostStartupFault::HostOpaque(_) => None,
        _ => None,
    }
}

impl AppUiHost {
    /// Construct the product Host while retaining every partial execution owner.
    pub fn try_new(app_state: AppState) -> Result<Self, Box<AppUiHostStartupFailure>> {
        Self::try_new_owned(app_state, None, None)
    }

    #[cfg(any(test, feature = "validation"))]
    pub(super) fn try_new_with_preferences_path(
        app_state: AppState,
        preferences: AppUiPreferences,
        preferences_path: PathBuf,
    ) -> Result<Self, Box<AppUiHostStartupFailure>> {
        Self::try_new_owned(app_state, Some((preferences, preferences_path)), None)
    }

    fn try_new_owned(
        app_state: AppState,
        explicit: Option<(AppUiPreferences, PathBuf)>,
        #[cfg(any(test, feature = "validation"))] fault: Option<AppUiHostStartupFault>,
        #[cfg(not(any(test, feature = "validation")))] _fault: Option<()>,
    ) -> Result<Self, Box<AppUiHostStartupFailure>> {
        let mut owner = AppUiHostStartupOwner::new(app_state);
        #[cfg(any(test, feature = "validation"))]
        {
            owner.fault = fault;
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.construct(explicit))) {
            Ok(Ok(host)) => Ok(host),
            Ok(Err(diagnostic)) => Err(Box::new(AppUiHostStartupFailure { diagnostic, owner })),
            Err(payload) => {
                let error = crate::app::execution_panic_diagnostic::execution_panic_diagnostic(
                    payload,
                    "App UI Host startup",
                );
                let diagnostic = AppUiHostStartupDiagnostic {
                    kind: AppUiHostStartupFailureKind::Host,
                    detail: error.to_string(),
                    opaque_payload_abandoned:
                        crate::app::execution_panic_diagnostic::opaque_panic_payload_abandoned(
                            &error,
                        ),
                };
                Err(Box::new(AppUiHostStartupFailure { diagnostic, owner }))
            }
        }
    }

    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn try_new_with_startup_fault(
        app_state: AppState,
        preferences: AppUiPreferences,
        preferences_path: PathBuf,
        fault: AppUiHostStartupFault,
    ) -> Result<Self, Box<AppUiHostStartupFailure>> {
        Self::try_new_owned(
            app_state,
            Some((preferences, preferences_path)),
            Some(fault),
        )
    }
}

/// Exercise normal construction plus every owned Host/Waveform/Preview failure boundary.
///
/// This is a deliberately narrow validation seam over the actual product Host. It does
/// not substitute fake service implementations or source-include Host internals.
#[cfg(feature = "validation")]
pub fn qualify_app_ui_host_startup_ownership(
) -> Result<AppUiHostStartupQualificationReport, AppUiHostStartupQualificationError> {
    const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
    const HOST_STAGES: [AppUiHostStartupStage; 18] = [
        AppUiHostStartupStage::AppOwned,
        AppUiHostStartupStage::PreferencesLoaded,
        AppUiHostStartupStage::AppConfigured,
        AppUiHostStartupStage::ThumbnailPrepared,
        AppUiHostStartupStage::ThumbnailStarted,
        AppUiHostStartupStage::ThumbnailConfigured,
        AppUiHostStartupStage::WaveformStarted,
        AppUiHostStartupStage::WaveformConfigured,
        AppUiHostStartupStage::PreviewStarted,
        AppUiHostStartupStage::PreviewConfigured,
        AppUiHostStartupStage::ResourcePolicyApplied,
        AppUiHostStartupStage::InitialPreviewResolved,
        AppUiHostStartupStage::CatalogPrepared,
        AppUiHostStartupStage::CatalogAttempted,
        AppUiHostStartupStage::RootBuilt,
        AppUiHostStartupStage::RootConfigured,
        AppUiHostStartupStage::RecoveryLoaded,
        AppUiHostStartupStage::Ready,
    ];
    const SERVICE_FAULTS: [AppUiHostStartupFault; 8] = [
        AppUiHostStartupFault::WaveformPrepared,
        AppUiHostStartupFault::WaveformSourceCache,
        AppUiHostStartupFault::WaveformAnalysisWorker,
        AppUiHostStartupFault::PreviewCache,
        AppUiHostStartupFault::PreviewVisual,
        AppUiHostStartupFault::PreviewCpuFallback,
        AppUiHostStartupFault::PreviewMedia(0),
        AppUiHostStartupFault::PreviewObserver,
    ];

    let mut failed_start_cases = Vec::with_capacity(HOST_STAGES.len() + SERVICE_FAULTS.len());
    for fault in HOST_STAGES.into_iter().map(AppUiHostStartupFault::Host).chain(SERVICE_FAULTS) {
        let fault_label = format!("{fault:?}");
        let marker = format!("Host owning-startup identity: {fault_label}");
        let mut app_state = AppState::new();
        app_state.set_status_hint(marker.clone(), false);
        let original_event_bus = Arc::clone(&app_state.event_bus);
        let path = std::env::temp_dir().join(format!(
            "mondrian-host-startup-{}.json",
            uuid::Uuid::new_v4()
        ));
        let failure = match AppUiHost::try_new_with_startup_fault(
            app_state,
            AppUiPreferences::default(),
            path,
            fault,
        ) {
            Ok(host) => {
                let (_app_state, receipt) =
                    host.into_validation_app_state_until(Instant::now() + SHUTDOWN_TIMEOUT);
                return Err(AppUiHostStartupQualificationError {
                    detail: format!(
                        "{fault_label} unexpectedly published a Host; cleanup_clean={}",
                        receipt.all_resources_released()
                    ),
                });
            }
            Err(failure) => failure,
        };
        let closed = failure.shutdown_until(Instant::now() + SHUTDOWN_TIMEOUT);
        let identity_preserved = Arc::ptr_eq(&original_event_bus, &closed.app_state.event_bus)
            && closed.app_state.status_hint.as_ref() == Some(&(marker, false));
        if !identity_preserved || !closed.shutdown.all_created_resources_released() {
            return Err(AppUiHostStartupQualificationError {
                detail: format!(
                    "{fault_label} did not preserve identity or close its exact inventory; identity_preserved={identity_preserved}; shutdown={:?}",
                    closed.shutdown
                ),
            });
        }
        failed_start_cases.push(AppUiHostStartupQualificationCase {
            fault: fault_label,
            diagnostic: closed.diagnostic,
            shutdown: closed.shutdown,
        });
    }

    let opaque_drops_before =
        HOST_OPAQUE_QUALIFICATION_DROPS.load(std::sync::atomic::Ordering::SeqCst);
    let mut opaque_app = AppState::new();
    opaque_app.set_status_hint("Host opaque startup identity", false);
    let opaque_event_bus = Arc::clone(&opaque_app.event_bus);
    let opaque_failure = match AppUiHost::try_new_with_startup_fault(
        opaque_app,
        AppUiPreferences::default(),
        std::env::temp_dir().join(format!(
            "mondrian-host-startup-opaque-{}.json",
            uuid::Uuid::new_v4()
        )),
        AppUiHostStartupFault::HostOpaque(AppUiHostStartupStage::AppConfigured),
    ) {
        Ok(host) => {
            let (_app_state, receipt) =
                host.into_validation_app_state_until(Instant::now() + SHUTDOWN_TIMEOUT);
            return Err(AppUiHostStartupQualificationError {
                detail: format!(
                    "opaque Host fault unexpectedly published a Host; cleanup_clean={}",
                    receipt.all_resources_released()
                ),
            });
        }
        Err(failure) => failure,
    };
    let opaque_closed = opaque_failure.shutdown_until(Instant::now() + SHUTDOWN_TIMEOUT);
    let opaque_payload_fail_closed = opaque_closed.diagnostic.opaque_payload_abandoned
        && opaque_closed.shutdown.opaque_panic_payload_abandoned
        && !opaque_closed.shutdown.all_created_resources_released()
        && Arc::ptr_eq(&opaque_event_bus, &opaque_closed.app_state.event_bus)
        && HOST_OPAQUE_QUALIFICATION_DROPS.load(std::sync::atomic::Ordering::SeqCst)
            == opaque_drops_before;
    if !opaque_payload_fail_closed {
        return Err(AppUiHostStartupQualificationError {
            detail: format!(
                "opaque Host payload was destroyed, lost, or admitted as clean; shutdown={:?}",
                opaque_closed.shutdown
            ),
        });
    }

    let mut successful_routes = 0_u32;
    for explicit in [false, true] {
        let result = if explicit {
            AppUiHost::try_new_with_preferences_path(
                AppState::new(),
                AppUiPreferences::default(),
                std::env::temp_dir().join(format!(
                    "mondrian-host-startup-success-{}.json",
                    uuid::Uuid::new_v4()
                )),
            )
        } else {
            AppUiHost::try_new(AppState::new())
        };
        let host = match result {
            Ok(host) => host,
            Err(failure) => {
                let diagnostic = failure.diagnostic().clone();
                let closed = failure.shutdown_until(Instant::now() + SHUTDOWN_TIMEOUT);
                return Err(AppUiHostStartupQualificationError {
                    detail: format!(
                        "normal route explicit={explicit} failed: {diagnostic}; shutdown={:?}",
                        closed.shutdown
                    ),
                });
            }
        };
        let (_app_state, receipt) =
            host.into_validation_app_state_until(Instant::now() + SHUTDOWN_TIMEOUT);
        if !receipt.all_resources_released() {
            return Err(AppUiHostStartupQualificationError {
                detail: format!(
                    "normal route explicit={explicit} did not close cleanly: {receipt:?}"
                ),
            });
        }
        successful_routes = successful_routes.saturating_add(1);
    }

    Ok(AppUiHostStartupQualificationReport {
        schema_version: 1,
        successful_routes,
        failed_start_cases,
        opaque_payload_fail_closed,
    })
}

#[cfg(feature = "validation")]
static HOST_OPAQUE_QUALIFICATION_DROPS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "validation")]
struct HostOpaqueQualificationPayload;

#[cfg(feature = "validation")]
impl Drop for HostOpaqueQualificationPayload {
    fn drop(&mut self) {
        HOST_OPAQUE_QUALIFICATION_DROPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
