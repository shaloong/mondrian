//! App UI host state.
//!
//! Window entrypoints own native event loops and rendering surfaces. This host
//! owns the reusable application/UI state bridge: root widget, `AppState`, and
//! refresh policy after widget-dispatched actions.

use std::cell::{Cell, Ref, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

#[cfg(test)]
use mondrian_core::ProjectId;
use mondrian_editor_state::state::{PanelKind, WorkspacePreset};
use mondrian_platform::PlatformService;
use mondrian_renderer::GpuNativeDecodedFrameImportSupport;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::{TreeWalker, Widget};
use mondrian_ui_theme::{set_theme_preset, ThemePreset};

use crate::app::execution_resource_coordination::{
    apply_preview_viewer_gpu_resource_decision, ExecutionDomainDemand,
    ExternalExecutionResourceDemand, PreviewViewerGpuResourceOwner,
};
use crate::app::native_video_import::resolve_playback_hardware_decode_admission;
use crate::app::playback_preview::{
    observe_playback_video_preroll as observe_preview_preroll, pump_playback_preview,
    PlaybackPreviewPumpOutcome,
};
use crate::app::preview_execution::{
    PreviewGpuFrame, PreviewGpuFrameState, PreviewGpuHeterogeneousExecution, PreviewOutputKey,
};
use crate::app::preview_runtime::{
    PreviewColorRejection, PreviewPresentationCandidate, PreviewPresentationState,
    PreviewVisualGpuCompletionDisposition,
};
use crate::app::preview_work_notification::PreviewWorkWatch;
use crate::app::ui_actions::{
    AssetsOpenFolderPayload, PreferencesAudioOutputDevicePayload, PreferencesShortcutPayload,
    PreferencesShortcutReboundPayload, PreferencesThemePayload, PreferencesViewerBackgroundPayload,
    PreferencesWaveformDisplayPayload, APP_SHELL_ASSET_BROWSER_OPEN_FOLDER,
    APP_SHELL_CANCEL_NEW_PROJECT_DIALOG, APP_SHELL_CLOSE_MODAL,
    APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG, APP_SHELL_CONFIRM_RECOVERY_DIALOG, APP_SHELL_NAMESPACE,
    APP_SHELL_NEW_PROJECT_DIALOG, APP_SHELL_NEW_PROJECT_DRAFT_CHANGED,
    APP_SHELL_OPEN_PROJECT_DIALOG, APP_SHELL_OPEN_RECENT_PROJECT, APP_SHELL_PENDING_CLOSE_CANCEL,
    APP_SHELL_PENDING_CLOSE_DISCARD, APP_SHELL_PENDING_CLOSE_SAVE_CONTINUE,
    APP_SHELL_PREFERENCES_AUDIO_OUTPUT_DEVICE_CHANGED,
    APP_SHELL_PREFERENCES_REFRESH_AUDIO_OUTPUT_DEVICES, APP_SHELL_PREFERENCES_SHORTCUT_DISABLED,
    APP_SHELL_PREFERENCES_SHORTCUT_REBOUND, APP_SHELL_PREFERENCES_SHORTCUT_RESET,
    APP_SHELL_PREFERENCES_THEME_CHANGED, APP_SHELL_PREFERENCES_VIEWER_BACKGROUND_CHANGED,
    APP_SHELL_PREFERENCES_WAVEFORM_DISPLAY_CHANGED, APP_SHELL_QUIT, APP_SHELL_RECOVERY_DIALOG,
    APP_SHELL_RECOVER_PROJECT, APP_SHELL_WINDOW_DRAG, APP_SHELL_WINDOW_MINIMIZE,
    APP_SHELL_WINDOW_TOGGLE_MAXIMIZE,
};
use crate::app::waveform_service::AudioWaveformService;
use crate::app::{
    discover_crash_recovery_candidates, AppState, CrashRecoveryCandidate,
    FramePresentationDisposition, FramePresentationPreflight, FramePresentationPublication,
    ProjectClosePoll,
};
use crate::app_ui::action_availability::app_state_action_enabled;
use crate::app_ui::action_queue::PendingUiActions;
use crate::app_ui::asset_thumbnails::AssetThumbnailAdapter;
use crate::app_ui::audio_device_catalog::AudioOutputDeviceCatalogAdapter;
use crate::app_ui::panels::{ViewerPreviewSource, ViewerPreviewState};
use crate::app_ui::pending_close_dialog::PendingCloseDialogAction;
use crate::app_ui::playback_feedback::ViewerPlaybackFeedback;
use crate::app_ui::preferences_store::{
    app_ui_preferences_path, load_app_ui_preferences, persist_app_ui_preferences_to,
    AppUiPreferences,
};
use crate::app_ui::preview::{viewer_frame_content, WindowPreviewAdapter, WindowPreviewSnapshot};
use crate::app_ui::recovery_dialog::recovery_age_label;
use crate::app_ui::shell::{try_resolve_app_shell_action, AppUiAppRoot};
use crate::app_ui::shortcuts::{
    default_shortcuts, is_known_shortcut_id, AppUiShortcutBinding, AppUiShortcutKey,
    AppUiShortcutOverride,
};
use crate::app_ui::startup::{AppUiStartupScreen, StartupRecentProject, StartupRecoveryProject};
use mondrian_editor_state::Action;

/// Window-host commands produced while draining app UI actions.
///
/// These are native shell side effects, not editor-state mutations. Entrypoints
/// apply them after the widget tree and `AppState` borrows have ended.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AppUiShellCommands {
    /// The native window should request application exit.
    pub quit: bool,
    /// The native window should toggle fullscreen mode.
    pub toggle_fullscreen: bool,
    /// The native window should minimize.
    pub minimize: bool,
    /// The native window should toggle maximized state.
    pub toggle_maximize: bool,
    /// The native window should begin an OS-level drag move.
    pub begin_window_drag: bool,
}

/// Product shell mode owned by the app UI host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppUiMode {
    /// Startup surface shown before a project is opened.
    Startup,
    /// Main editing workspace.
    Workspace,
}

/// Typed result of one bounded background-task pump.
///
/// Repaint authority is intentionally independent from the scheduling hint
/// that another bounded drain turn is required.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppUiBackgroundTaskPollOutcome {
    /// Visible, transport, or full model state changed.
    pub(crate) repaint_required: bool,
    /// A bounded completion source still has immediately drainable work.
    pub(crate) needs_follow_up_poll: bool,
    /// The guarded asynchronous Project close completed a pending app quit.
    pub(crate) quit_requested: bool,
}

impl AppUiBackgroundTaskPollOutcome {
    const fn from_changes(full_model_changed: bool, preview: PlaybackPreviewPumpOutcome) -> Self {
        Self {
            repaint_required: full_model_changed
                || preview.visible_change
                || preview.transport_change
                || preview.candidate_retry_required,
            needs_follow_up_poll: preview.needs_follow_up_poll,
            quit_requested: false,
        }
    }
}

/// Product-facing app UI session state.
pub struct AppUiHost {
    startup: AppUiStartupScreen,
    root: AppUiAppRoot,
    app_state: RefCell<AppState>,
    preferences: AppUiPreferences,
    preferences_path: PathBuf,
    recovery_candidates: Vec<CrashRecoveryCandidate>,
    asset_thumbnails: AssetThumbnailAdapter,
    audio_device_catalog: AudioOutputDeviceCatalogAdapter,
    waveform_service: Arc<AudioWaveformService>,
    preview_service: WindowPreviewAdapter,
    window_preview_state: RefCell<ViewerPreviewState>,
    playback_feedback: ViewerPlaybackFeedback,
    /// Exact observation that most recently crossed a running frame boundary.
    last_playback_frame_advance_at: Cell<Option<Instant>>,
    mode: AppUiMode,
    system_theme_preset: ThemePreset,
    ui_dirty: Cell<bool>,
    preview_dirty: Cell<bool>,
    pending_close_action: Option<PendingCloseAction>,
    /// Guarded close/quit intent whose Project persistence barrier is still
    /// progressing outside the UI thread.
    quiescing_close_action: Option<PendingCloseAction>,
}

fn window_preview_state_retains_external_gpu(state: &ViewerPreviewState) -> bool {
    matches!(
        state,
        ViewerPreviewState::Ready(mondrian_ui_widgets::ViewerFrameContent::ExternalTexture(_))
            | ViewerPreviewState::Stale(mondrian_ui_widgets::ViewerFrameContent::ExternalTexture(
                _
            ))
    )
}

fn window_preview_state_external_texture_key(state: &ViewerPreviewState) -> Option<&str> {
    match state {
        ViewerPreviewState::Ready(mondrian_ui_widgets::ViewerFrameContent::ExternalTexture(
            frame,
        ))
        | ViewerPreviewState::Stale(mondrian_ui_widgets::ViewerFrameContent::ExternalTexture(
            frame,
        )) => Some(frame.key.as_str()),
        ViewerPreviewState::Ready(_)
        | ViewerPreviewState::Stale(_)
        | ViewerPreviewState::Transparent
        | ViewerPreviewState::StaleTransparent
        | ViewerPreviewState::Loading
        | ViewerPreviewState::Unavailable(_) => None,
    }
}

impl AppUiHost {
    /// Create a host from an initial application state snapshot.
    pub fn new(app_state: AppState) -> Self {
        Self::new_with_preferences_path(
            app_state,
            load_app_ui_preferences(),
            app_ui_preferences_path(),
        )
    }

    /// Create a host from explicit preferences and path.
    pub(crate) fn new_with_preferences_path(
        app_state: AppState,
        preferences: AppUiPreferences,
        preferences_path: PathBuf,
    ) -> Self {
        app_state.set_audio_output_device_selection(preferences.audio_output_device.clone());
        let system_theme_preset = ThemePreset::Dark;
        set_theme_preset(preferences.theme_preference.resolve(system_theme_preset));
        let asset_thumbnails = AssetThumbnailAdapter::new();
        asset_thumbnails.set_color_context(Some(app_state.thumbnail_color_context()));
        let waveform_service = AudioWaveformService::new();
        waveform_service.set_library(app_state.asset_library_handle());
        let preview_service = WindowPreviewAdapter::new();
        preview_service.synchronize_transport_intent(app_state.preview_transport_intent());
        apply_execution_resource_policy(
            &app_state,
            &asset_thumbnails,
            &waveform_service,
            &preview_service,
        );
        let window_preview_state = preview_service.viewer_preview_for_state(&app_state);
        let window_preview_snapshot =
            WindowPreviewSnapshot::new(&window_preview_state, &preview_service);
        let audio_device_catalog = AudioOutputDeviceCatalogAdapter::new();
        let mut root = AppUiAppRoot::from_app_state_with_preferences_thumbnails_and_preview(
            &app_state,
            &preferences,
            Some(&asset_thumbnails),
            Some(&window_preview_snapshot),
            Some(waveform_service.source()),
        );
        root.set_audio_output_device_catalog(audio_device_catalog.state().clone());
        let playback_feedback = root.viewer_playback_feedback();
        let mode = if app_state.has_open_project() {
            AppUiMode::Workspace
        } else {
            AppUiMode::Startup
        };
        let recovery_candidates = discover_crash_recovery_candidates();
        let mut startup = AppUiStartupScreen::new();
        startup.set_recent_projects(startup_recent_projects_from_preferences(&preferences));
        startup.set_recovery_projects(startup_recovery_projects_from_candidates(
            &recovery_candidates,
        ));
        Self {
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
        }
    }

    /// Immutable access to the root widget.
    pub fn root(&self) -> &AppUiAppRoot {
        &self.root
    }

    /// Mutable access to the root widget for event routing and layout.
    pub fn root_mut(&mut self) -> &mut AppUiAppRoot {
        &mut self.root
    }

    /// Current visible product mode.
    pub fn mode(&self) -> AppUiMode {
        self.mode
    }

    /// Immutable access to the widget currently visible in the native window.
    pub fn active_root(&self) -> &dyn Widget {
        match self.mode {
            AppUiMode::Startup => &self.startup,
            AppUiMode::Workspace => &self.root,
        }
    }

    /// Mutable access to the widget currently visible in the native window.
    pub fn active_root_mut(&mut self) -> &mut dyn Widget {
        match self.mode {
            AppUiMode::Startup => &mut self.startup,
            AppUiMode::Workspace => &mut self.root,
        }
    }

    /// Resolve the currently laid-out Viewer spatial presentation contract.
    pub(crate) fn viewer_presentation_geometry(
        &self,
    ) -> Option<mondrian_ui_widgets::ViewerPresentationGeometry> {
        crate::app_ui::shell::viewer_presentation_geometry(self.active_root())
    }

    /// Whether a demand-driven panel is the active visible workspace tab.
    pub(crate) fn is_panel_active(&self, panel: PanelKind) -> bool {
        self.mode == AppUiMode::Workspace && self.root.is_panel_active(panel)
    }

    /// Read-only access to the current app state.
    pub fn app_state(&self) -> Ref<'_, AppState> {
        self.app_state.borrow()
    }

    /// Get the Project engine and machine-local Viewer display policy.
    pub(crate) fn resolved_display_color_management(
        &self,
    ) -> (
        mondrian_core::ColorEngine,
        mondrian_core::color_models::DisplayManagementPolicy,
    ) {
        let state = self.app_state.borrow();
        let engine = state.project_color_environment().engine().clone();
        (engine, state.viewer_display_management().clone())
    }

    /// Build a GPU-output preview candidate for the current app state.
    pub(crate) fn gpu_preview_frame_for_current_state(&self) -> PreviewGpuFrameState {
        let state = self.app_state.borrow();
        self.preview_service
            .gpu_preview_frame(state.preview_frame_execution_request(std::time::Instant::now()))
    }

    /// Build ticketless immediate-successor work from the same Preview Runtime.
    pub(crate) fn gpu_preview_successor_for_current_state(&self) -> PreviewGpuFrameState {
        let state = self.app_state.borrow();
        state
            .preview_successor_execution_request(std::time::Instant::now())
            .map_or(PreviewGpuFrameState::Loading, |request| {
                self.preview_service.gpu_preview_frame(request)
            })
    }

    /// Retain a completed successor without publishing it to the Viewer widget.
    pub(crate) fn register_prepared_viewer_gpu_successor(
        &self,
        frame: &PreviewGpuFrame,
        output: mondrian_ui_widgets::ViewerExternalTextureFrame,
    ) {
        debug_assert!(frame.is_successor_preparation());
        self.preview_service.register_prepared_gpu_successor(
            frame.playback_intent(),
            frame.output_key.clone(),
            output,
        );
    }

    /// Complete output identity currently proved by Preview semantics.
    pub(crate) fn exact_current_viewer_gpu_output_key(
        &self,
    ) -> Option<crate::app::preview_execution::PreviewOutputKey> {
        self.preview_service.registered_exact_current_gpu_output_key()
    }

    /// Prepared GPU output identity still relevant to the current or exact
    /// immediate-successor transport intent.
    pub(crate) fn exact_prepared_viewer_gpu_output_key(
        &self,
    ) -> Option<crate::app::preview_execution::PreviewOutputKey> {
        let state = self.app_state.borrow();
        let sampled_at = Instant::now();
        let intent = state.preview_execution_snapshot(sampled_at).transport().playback_intent();
        let immediate_successor_intent = state
            .preview_successor_execution_request(sampled_at)
            .map(|request| request.snapshot().transport().playback_intent());
        self.preview_service
            .registered_relevant_prepared_gpu_output_key(intent, immediate_successor_intent)
    }

    /// Clone the UI-independent watch for pollable Preview worker results.
    pub(crate) fn preview_work_watch(&self) -> PreviewWorkWatch {
        self.preview_service.work_watch()
    }

    /// Synchronize the current display output snapshot into preview scheduling.
    pub(crate) fn set_display_output_snapshot(
        &self,
        snapshot: Option<&mondrian_core::display_contract::DisplayOutputSnapshot>,
    ) {
        self.preview_service.set_display_output_snapshot(snapshot);
    }

    /// Synchronize renderer native video import readiness into preview decode admission.
    pub(crate) fn set_native_decoded_frame_import_support(
        &self,
        support: GpuNativeDecodedFrameImportSupport,
    ) {
        let admission = resolve_playback_hardware_decode_admission(&support);
        self.preview_service.set_playback_hardware_decode_admission(admission);
    }

    /// Request the UI-independent bounded CPU Viewer execution path after a
    /// concrete Window GPU failure.
    pub(crate) fn request_viewer_cpu_fallback(&self, reason: impl Into<String>) {
        self.preview_service.request_viewer_cpu_fallback(reason);
        self.mark_window_preview_pending();
        self.preview_dirty.set(true);
    }

    /// Return to GPU Viewer execution after a replacement device generation
    /// has published its native-import capabilities.
    pub(crate) fn clear_viewer_cpu_fallback(&self) {
        self.preview_service.clear_viewer_cpu_fallback();
    }

    /// Apply the latest immutable Preview Viewer projection to its GPU owner.
    ///
    /// The low-frequency Host resource-policy Seam already applies the complete
    /// Preview decision to `preview_service`. This per-frame Seam must not
    /// reconfigure Preview scheduling or caches.
    pub(crate) fn apply_preview_execution_resource_decision(
        &self,
        runtime: &mut impl PreviewViewerGpuResourceOwner,
    ) {
        let decision = self.app_state.borrow().execution_resource_decision();
        apply_preview_viewer_gpu_resource_decision(runtime, &decision.preview.viewer_gpu);
    }

    /// Consume an already-expired current demand before the Window builds a
    /// Viewer candidate.
    pub(crate) fn preflight_pending_viewer_gpu_presentation(&self) -> bool {
        let preflight =
            self.app_state.borrow_mut().preflight_pending_frame_presentation(Instant::now());
        self.finish_window_presentation_preflight(preflight)
    }

    /// Recheck the exact ticket carried by a built candidate immediately
    /// before the Window records GPU commands.
    pub(crate) fn preflight_viewer_gpu_presentation(
        &self,
        ticket: Option<mondrian_playback::FramePresentationTicket>,
    ) -> bool {
        let preflight =
            self.app_state.borrow_mut().preflight_frame_presentation(ticket, Instant::now());
        self.finish_window_presentation_preflight(preflight)
    }

    fn finish_window_presentation_preflight(&self, preflight: FramePresentationPreflight) -> bool {
        match preflight {
            FramePresentationPreflight::MaySubmit => true,
            FramePresentationPreflight::DroppedLate(completion) => {
                self.finish_window_presentation_disposition(
                    FramePresentationDisposition::DroppedLate(completion),
                    false,
                );
                false
            }
            FramePresentationPreflight::LostAuthority => {
                self.finish_window_presentation_disposition(
                    FramePresentationDisposition::LostAuthority,
                    false,
                );
                false
            }
        }
    }

    /// Advertise a registered GPU preview texture as the viewer frame for its resolved plan.
    pub(crate) fn set_external_viewer_frame(
        &self,
        frame: &PreviewGpuFrame,
        texture_key: impl Into<String>,
        presentation: mondrian_ui_widgets::ViewerExternalTexturePresentation,
    ) -> FramePresentationDisposition {
        let texture_key = texture_key.into();
        let visible_output = mondrian_ui_widgets::ViewerExternalTextureFrame::new_spatial(
            texture_key.clone(),
            presentation,
        );
        let visible_changed = Cell::new(false);
        let disposition = if let Some(visible_output) = visible_output {
            // Clone the pointer-only payload during preparation. The
            // authoritative commit below may only move prepared values into
            // their current-output slots.
            let registered_output = visible_output.clone();
            let output_key = frame.output_key.clone();
            self.app_state.borrow_mut().finalize_frame_presentation(
                frame.presentation_ticket(),
                FramePresentationPublication::prepared(|| {
                    self.preview_service.register_gpu_output(output_key, registered_output);
                    self.window_preview_state.replace(ViewerPreviewState::Ready(
                        mondrian_ui_widgets::ViewerFrameContent::ExternalTexture(visible_output),
                    ));
                    visible_changed.set(true);
                }),
            )
        } else {
            self.preview_service.reject_gpu_output_registration();
            self.app_state.borrow_mut().finalize_frame_presentation(
                frame.presentation_ticket(),
                FramePresentationPublication::rejected(),
            )
        };
        self.finish_window_presentation_disposition(disposition, visible_changed.get());
        disposition
    }

    /// Complete a candidate that reuses the already published Window output.
    ///
    /// The candidate carries the ticket captured when Preview proved that
    /// retained output exact for the new intent. A stale or missing ticket can
    /// never be replaced with the demand current at this later seam.
    pub(crate) fn present_current_viewer_output(
        &self,
        candidate: PreviewPresentationCandidate<()>,
    ) -> FramePresentationDisposition {
        let visible_changed = Cell::new(false);
        let disposition = if let Some((next, changed)) = self.prepared_retained_window_preview() {
            let clear_external_gpu = !window_preview_state_retains_external_gpu(&next);
            let publication = FramePresentationPublication::prepared(|| {
                if clear_external_gpu {
                    self.preview_service.clear_external_viewer_frame();
                }
                self.window_preview_state.replace(next);
                visible_changed.set(changed);
            });
            if candidate.was_already_visible()
                && let Some(already_visible_at) = self.last_playback_frame_advance_at.get()
            {
                self.app_state.borrow_mut().finalize_already_visible_frame_presentation(
                    candidate.presentation_ticket(),
                    already_visible_at,
                    publication,
                )
            } else {
                self.app_state
                    .borrow_mut()
                    .finalize_frame_presentation(candidate.presentation_ticket(), publication)
            }
        } else {
            self.app_state.borrow_mut().finalize_frame_presentation(
                candidate.presentation_ticket(),
                FramePresentationPublication::rejected(),
            )
        };
        self.finish_window_presentation_disposition(disposition, visible_changed.get());
        disposition
    }

    /// Publish a semantic transparent canvas through the same ticket seam as a
    /// registered texture or CPU raster.
    pub(crate) fn present_transparent_viewer_output(
        &self,
        candidate: PreviewPresentationCandidate<()>,
    ) -> FramePresentationDisposition {
        let visible_changed = Cell::new(false);
        let changed = !matches!(
            &*self.window_preview_state.borrow(),
            ViewerPreviewState::Transparent
        );
        let disposition = self.app_state.borrow_mut().finalize_frame_presentation(
            candidate.presentation_ticket(),
            FramePresentationPublication::prepared(|| {
                self.preview_service.clear_external_viewer_frame();
                self.window_preview_state.replace(ViewerPreviewState::Transparent);
                visible_changed.set(changed);
            }),
        );
        self.finish_window_presentation_disposition(disposition, visible_changed.get());
        disposition
    }

    fn prepared_retained_window_preview(&self) -> Option<(ViewerPreviewState, bool)> {
        let (next, changed) = match &*self.window_preview_state.borrow() {
            ViewerPreviewState::Ready(frame) => {
                (Some(ViewerPreviewState::Ready(frame.clone())), false)
            }
            ViewerPreviewState::Stale(frame) => {
                (Some(ViewerPreviewState::Ready(frame.clone())), true)
            }
            ViewerPreviewState::Transparent => (Some(ViewerPreviewState::Transparent), false),
            ViewerPreviewState::StaleTransparent => (Some(ViewerPreviewState::Transparent), true),
            ViewerPreviewState::Loading | ViewerPreviewState::Unavailable(_) => (None, false),
        };
        next.map(|next| (next, changed))
    }

    fn finish_window_presentation_disposition(
        &self,
        disposition: FramePresentationDisposition,
        visible_changed_without_demand: bool,
    ) {
        match disposition {
            FramePresentationDisposition::Presented(_) => {
                self.preview_service.try_release_settled_transport_media_residency();
                let _ = self.observe_playback_video_preroll();
                self.preview_dirty.set(true);
            }
            FramePresentationDisposition::NoDemand => {
                self.preview_service.try_release_settled_transport_media_residency();
                if visible_changed_without_demand {
                    self.preview_dirty.set(true);
                }
            }
            FramePresentationDisposition::DroppedLate(_)
            | FramePresentationDisposition::OutputRejected
            | FramePresentationDisposition::LostAuthority => {
                self.mark_window_preview_pending();
                self.preview_dirty.set(true);
            }
        }
    }

    fn mark_window_preview_pending(&self) {
        let pending = match &*self.window_preview_state.borrow() {
            ViewerPreviewState::Ready(frame) | ViewerPreviewState::Stale(frame) => {
                ViewerPreviewState::Stale(frame.clone())
            }
            ViewerPreviewState::Transparent | ViewerPreviewState::StaleTransparent => {
                ViewerPreviewState::StaleTransparent
            }
            ViewerPreviewState::Loading | ViewerPreviewState::Unavailable(_) => {
                ViewerPreviewState::Loading
            }
        };
        self.window_preview_state.replace(pending);
    }

    fn admit_window_preview_state(
        &self,
        ticket: Option<mondrian_playback::FramePresentationTicket>,
        next: ViewerPreviewState,
    ) -> FramePresentationDisposition {
        let clear_external_gpu = !window_preview_state_retains_external_gpu(&next);
        let disposition = self.app_state.borrow_mut().finalize_frame_presentation(
            ticket,
            FramePresentationPublication::prepared(|| {
                if clear_external_gpu {
                    self.preview_service.clear_external_viewer_frame();
                }
                self.window_preview_state.replace(next);
            }),
        );
        // This method is called while a Host model refresh is already
        // projecting `window_preview_state`, so a demand-free replacement is
        // immediately visible and must not enqueue a second identical refresh.
        self.finish_window_presentation_disposition(disposition, false);
        disposition
    }

    #[cfg(test)]
    fn admit_window_preview_state_at(
        &self,
        ticket: Option<mondrian_playback::FramePresentationTicket>,
        next: ViewerPreviewState,
        committed_at: Instant,
    ) -> FramePresentationDisposition {
        let clear_external_gpu = !window_preview_state_retains_external_gpu(&next);
        let disposition = self.app_state.borrow_mut().finalize_frame_presentation_at_for_test(
            ticket,
            committed_at,
            FramePresentationPublication::prepared(|| {
                if clear_external_gpu {
                    self.preview_service.clear_external_viewer_frame();
                }
                self.window_preview_state.replace(next);
            }),
        );
        self.finish_window_presentation_disposition(disposition, false);
        disposition
    }

    /// Evaluate and arbitrate the Window CPU/current-output projection before
    /// Widget models can observe it.
    fn refresh_window_preview_state(&self) {
        {
            let state = self.app_state.borrow();
            if state.is_playing() && state.pending_playback_frame_demand_identity().is_none() {
                return;
            }
        }
        let presentation = {
            let state = self.app_state.borrow();
            self.preview_service
                .presentation(state.preview_frame_execution_request(Instant::now()))
        };
        match presentation {
            PreviewPresentationState::Ready(candidate) => {
                let ticket = candidate.presentation_ticket();
                let next = match viewer_frame_content(candidate.into_value()) {
                    Ok(frame) => ViewerPreviewState::Ready(frame),
                    Err(reason) => {
                        let disposition = self.app_state.borrow_mut().finalize_frame_presentation(
                            ticket,
                            FramePresentationPublication::rejected(),
                        );
                        self.finish_window_presentation_disposition(disposition, false);
                        self.window_preview_state.replace(ViewerPreviewState::Unavailable(reason));
                        return;
                    }
                };
                let _ = self.admit_window_preview_state(ticket, next);
            }
            PreviewPresentationState::Transparent(candidate) => {
                let _ = self.admit_window_preview_state(
                    candidate.presentation_ticket(),
                    ViewerPreviewState::Transparent,
                );
            }
            PreviewPresentationState::Loading | PreviewPresentationState::Stale(_) => {
                self.mark_window_preview_pending();
            }
            PreviewPresentationState::Unavailable(reason) => {
                self.window_preview_state.replace(ViewerPreviewState::Unavailable(reason));
            }
        }
    }

    /// Resolve one Broker-owned heterogeneous Viewer candidate after wgpu
    /// reports actual completion. Exact Late delivery is applied only if its
    /// demand still owns terminal authority.
    pub(crate) fn finalize_heterogeneous_viewer_gpu(
        &self,
        execution: PreviewGpuHeterogeneousExecution,
        completed: &mondrian_renderer::ViewerHeterogeneousGpuCompletedBatch,
    ) -> Result<PreviewVisualGpuCompletionDisposition, String> {
        let disposition = match self
            .preview_service
            .finalize_heterogeneous_gpu_completion(execution, completed)
        {
            Ok(disposition) => disposition,
            Err(error) => {
                // Evidence rejection queues an exact Failed delivery inside
                // the runtime; ensure the normal Playback pump is scheduled.
                self.preview_dirty.set(true);
                return Err(error.to_string());
            }
        };
        self.observe_visual_gpu_disposition(disposition);
        Ok(disposition)
    }

    /// Fail a heterogeneous Viewer candidate that cannot reach actual GPU
    /// completion. The exact current Playback demand, if any, is consumed once
    /// at this Window composition seam.
    pub(crate) fn fail_heterogeneous_viewer_gpu(
        &self,
        execution: PreviewGpuHeterogeneousExecution,
    ) -> PreviewVisualGpuCompletionDisposition {
        let disposition = self.preview_service.fail_heterogeneous_gpu_execution(execution);
        self.observe_visual_gpu_disposition(disposition);
        disposition
    }

    fn observe_visual_gpu_disposition(&self, disposition: PreviewVisualGpuCompletionDisposition) {
        if let PreviewVisualGpuCompletionDisposition::TerminalCandidate(candidate) = &disposition {
            let mut state = self.app_state.borrow_mut();
            if state.pending_playback_frame_demand_identity() == Some(candidate.identity()) {
                state.observe_frame_delivery_candidate(*candidate, Instant::now());
            }
            self.preview_dirty.set(true);
        }
    }

    fn observe_playback_video_preroll(&self) -> bool {
        observe_preview_preroll(&mut self.app_state.borrow_mut(), &self.preview_service)
    }

    /// Clear any advertised GPU viewer frame.
    pub(crate) fn clear_external_viewer_frame(&self) {
        self.preview_service.clear_external_viewer_frame();
        self.preview_dirty.set(true);
    }

    /// Whether Preview retains the exact semantic Window texture artifact.
    pub(crate) fn has_external_viewer_frame_artifact(
        &self,
        key: &PreviewOutputKey,
        texture_key: &str,
    ) -> bool {
        self.preview_service
            .has_gpu_output_artifact(key, |output| output.key == texture_key)
    }

    /// Clear only the exact semantic and visible Window texture artifact.
    ///
    /// The renderer registration and the Widget projection are independent
    /// owners. Once an exact physical texture is revoked, leaving the Widget
    /// pointed at its key would make the next redraw sample an unregistered
    /// resource. A same-semantic replacement has a distinct texture key and
    /// therefore remains untouched.
    pub(crate) fn clear_external_viewer_frame_for_artifact(
        &self,
        key: &PreviewOutputKey,
        texture_key: &str,
    ) -> bool {
        let semantic_cleared = self
            .preview_service
            .clear_external_viewer_frame_for_artifact(key, |output| output.key == texture_key);
        let visible_cleared = {
            let mut visible = self.window_preview_state.borrow_mut();
            if window_preview_state_external_texture_key(&visible) == Some(texture_key) {
                *visible = ViewerPreviewState::Loading;
                true
            } else {
                false
            }
        };
        if semantic_cleared || visible_cleared {
            self.preview_dirty.set(true);
        }
        semantic_cleared || visible_cleared
    }

    /// Record a structured GPU output blocker from the window/GPU path.
    pub(crate) fn record_preview_gpu_output_blocker(
        &self,
        blocker: &crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlocker,
    ) {
        self.preview_service.record_preview_gpu_output_blocker(blocker);
    }

    /// Record a structured GPU output blocker breakdown from the window/GPU path.
    pub(crate) fn record_preview_gpu_output_blocker_breakdown(
        &self,
        breakdown: crate::app::preview_gpu_output_blocker::PreviewGpuOutputBlockerBreakdown,
    ) {
        self.preview_service.record_preview_gpu_output_blocker_breakdown(breakdown);
    }

    /// Record a CPU output fallback caused by failed native GPU preview output.
    pub(crate) fn record_preview_cpu_output_fallback(&self, width: u32, height: u32) {
        self.preview_service.record_cpu_output_fallback(width, height);
    }

    pub(crate) fn record_preview_gpu_compositing(
        &self,
        diagnostics: mondrian_renderer::GpuCompositingDiagnostics,
    ) {
        self.preview_service.record_gpu_compositing(diagnostics);
    }

    /// Latest structured preview color rejection, if the current viewer request was rejected.
    pub(crate) fn current_viewer_color_rejection(&self) -> Option<PreviewColorRejection> {
        self.preview_service.last_color_rejection()
    }

    /// Current persisted app UI preferences snapshot.
    pub fn preferences(&self) -> &AppUiPreferences {
        &self.preferences
    }

    /// Mark the root as needing a model refresh from `AppState`.
    pub fn mark_dirty(&self) {
        self.ui_dirty.set(true);
    }

    /// Update the desktop system theme used by `ThemePreference::System`.
    ///
    /// Returns true when the effective concrete theme changed.
    pub fn set_system_theme_preset(&mut self, preset: ThemePreset) -> bool {
        if self.system_theme_preset == preset {
            return false;
        }
        let previous = self.preferences.theme_preference.resolve(self.system_theme_preset);
        self.system_theme_preset = preset;
        let next = self.preferences.theme_preference.resolve(self.system_theme_preset);
        if previous == next {
            return false;
        }
        set_theme_preset(next);
        self.mark_dirty();
        true
    }

    /// Refresh the root widget models when editor state changed.
    pub fn refresh_if_dirty(&mut self, bounds: Rect) {
        let full_refresh = self.ui_dirty.replace(false);
        let preview_refresh = self.preview_dirty.replace(false);
        if !full_refresh {
            if preview_refresh && self.mode == AppUiMode::Workspace {
                self.refresh_preview_state_without_rebuild();
                self.sync_playback_feedback_from_viewer();
            }
            self.sync_mode_from_app_state(bounds);
            return;
        }
        let next_mode = mode_for_app_state(&self.app_state.borrow());
        if next_mode == self.mode && widget_tree_has_transient_interaction(self.active_root()) {
            self.ui_dirty.set(true);
            if preview_refresh && self.mode == AppUiMode::Workspace {
                self.refresh_preview_state_without_rebuild();
                self.sync_playback_feedback_from_viewer();
            }
            return;
        }
        self.normalize_asset_folder_selection();
        self.asset_thumbnails
            .set_color_context(Some(self.app_state.borrow().thumbnail_color_context()));
        self.refresh_window_preview_state();
        let window_preview_state = self.window_preview_state.borrow().clone();
        let window_preview_snapshot =
            WindowPreviewSnapshot::new(&window_preview_state, &self.preview_service);
        self.root.refresh_from_app_state_with_preferences_thumbnails_and_preview(
            &self.app_state.borrow(),
            &self.preferences,
            Some(&self.asset_thumbnails),
            Some(&window_preview_snapshot),
            Some(self.waveform_service.source()),
        );
        self.sync_mode_from_app_state(bounds);
        TreeWalker::layout(self.active_root_mut(), bounds);
    }

    fn refresh_preview_state_without_rebuild(&mut self) {
        self.refresh_window_preview_state();
        let window_preview_state = self.window_preview_state.borrow().clone();
        let window_preview_snapshot =
            WindowPreviewSnapshot::new(&window_preview_state, &self.preview_service);
        let state = self.app_state.borrow();
        self.root
            .refresh_playback_frame_from_app_state(&state, Some(&window_preview_snapshot));
    }

    /// Poll background host tasks without conflating repaint and drain policy.
    pub(crate) fn poll_background_tasks(&mut self, bounds: Rect) -> AppUiBackgroundTaskPollOutcome {
        // Keep the waveform service's library reference in sync with the
        // current app state (e.g. when a new project opens).
        if self.quiescing_close_action.is_some() {
            self.waveform_service.set_library(None);
        } else {
            self.waveform_service
                .set_library(self.app_state.borrow().asset_library_handle());
        }
        apply_execution_resource_policy(
            &self.app_state.borrow(),
            &self.asset_thumbnails,
            &self.waveform_service,
            &self.preview_service,
        );
        let project_path_before_persistence =
            self.app_state.borrow().current_project_path().map(std::path::Path::to_path_buf);
        let persistence_changed = self.app_state.borrow_mut().poll_project_persistence();
        if persistence_changed {
            let project_path_after_persistence =
                self.app_state.borrow().current_project_path().map(std::path::Path::to_path_buf);
            if project_path_after_persistence.is_some()
                && project_path_after_persistence != project_path_before_persistence
            {
                if let Some(path) = project_path_after_persistence {
                    self.record_recent_project(path);
                }
                self.refresh_recovery_candidates();
            }
        }
        let (project_close_changed, quit_requested) = self.poll_quiescing_project_close();
        // A quiescing Project remains readable for UI projection, but no
        // Project-scoped worker result may commit while its Session admission
        // is frozen. Final close invalidates their generations in one place.
        let project_execution_frozen = self.app_state.borrow().project_close_blocks_actions();
        let (media_imports_changed, media_asset_mutations_changed, proxy_generation_changed) =
            if project_execution_frozen {
                (false, false, false)
            } else {
                let mut state = self.app_state.borrow_mut();
                (
                    state.poll_media_imports(),
                    state.poll_media_asset_mutations(),
                    state.poll_proxy_generation(),
                )
            };
        let export_queue_changed = self.app_state.borrow_mut().poll_export_queue();
        let thumbnails_changed = self.asset_thumbnails.poll_finished();
        let audio_devices_changed = self.audio_device_catalog.poll_finished();
        if audio_devices_changed {
            self.root
                .set_audio_output_device_catalog(self.audio_device_catalog.state().clone());
        }
        let preview_outcome =
            pump_playback_preview(&mut self.app_state.borrow_mut(), &self.preview_service);
        let waveform_changed = self.waveform_service.poll_finished();
        let transport_model_changed = preview_outcome.transport_change;
        if transport_model_changed {
            self.refresh_transport_state_without_preview();
        }
        if preview_outcome.visible_change {
            self.preview_dirty.set(true);
        }
        let full_model_changed = persistence_changed
            || project_close_changed
            || media_imports_changed
            || media_asset_mutations_changed
            || proxy_generation_changed
            || export_queue_changed
            || thumbnails_changed
            || waveform_changed;
        let mut outcome =
            AppUiBackgroundTaskPollOutcome::from_changes(full_model_changed, preview_outcome);
        outcome.repaint_required |= audio_devices_changed;
        outcome.quit_requested = quit_requested;
        if !full_model_changed {
            if preview_outcome.visible_change {
                self.refresh_if_dirty(bounds);
            }
            return outcome;
        }
        self.mark_dirty();
        self.refresh_if_dirty(bounds);
        self.sync_playback_feedback_from_viewer();
        outcome
    }

    fn poll_quiescing_project_close(&mut self) -> (bool, bool) {
        if self.quiescing_close_action.is_none() {
            return (false, false);
        }
        let close_poll = self.app_state.borrow_mut().poll_project_close();
        match close_poll {
            ProjectClosePoll::Inactive | ProjectClosePoll::Pending => (false, false),
            ProjectClosePoll::Closed => {
                let Some(action) = self.quiescing_close_action.take() else {
                    return (true, false);
                };
                self.refresh_recovery_candidates();
                self.mark_dirty();
                match action {
                    PendingCloseAction::CloseProject => (true, false),
                    PendingCloseAction::QuitApp => {
                        self.preview_service.shutdown();
                        #[cfg(not(test))]
                        super::window::arm_process_exit_watchdog();
                        (true, true)
                    }
                }
            }
            ProjectClosePoll::SaveRejected(reason) => {
                tracing::warn!(%reason, "save-before-close was rejected; Project remains open");
                self.quiescing_close_action = None;
                self.waveform_service
                    .set_library(self.app_state.borrow().asset_library_handle());
                self.mark_dirty();
                (true, false)
            }
            ProjectClosePoll::Faulted(reason) => {
                tracing::error!(%reason, "asynchronous Project close failed closed");
                if let Some(action) = self.quiescing_close_action.take() {
                    self.pending_close_action = Some(action);
                    self.root.show_pending_close_dialog(action.dialog_action());
                }
                self.waveform_service
                    .set_library(self.app_state.borrow().asset_library_handle());
                self.mark_dirty();
                (true, false)
            }
        }
    }

    /// Earliest monotonic deadline for the next native resource observation.
    ///
    /// Window scheduling merges this with UI and playback timers so a fully
    /// idle editor cannot leave pressure evidence stale indefinitely.
    pub(crate) fn next_execution_resource_observation_deadline(&self) -> Instant {
        let resource_deadline =
            self.app_state.borrow().next_execution_resource_observation_deadline();
        if self.quiescing_close_action.is_some() {
            resource_deadline.min(Instant::now() + Duration::from_millis(16))
        } else {
            resource_deadline
        }
    }

    /// Advance active playback and refresh UI models when the visible frame changes.
    pub fn advance_playback_clock(&mut self, observed_at: Instant, bounds: Rect) -> bool {
        let observed_at = Instant::now().max(observed_at);
        let (playback_changed, crossed_frame) = {
            let mut state = self.app_state.borrow_mut();
            // Audio Device Clock is the authority while available. Apply its
            // latest coherent callback fact before asking the Engine to derive
            // a new frame target/deadline; otherwise callback age can mint an
            // unnecessarily expired demand that a fresh observation is not
            // allowed to extend.
            let audio_result = state.pump_audio_output();
            let advance = state.advance_playback_clock_at(observed_at);
            let changed = advance.requires_refresh();
            if let Err(error) = audio_result {
                tracing::error!(%error, "audio output pump failed closed");
            }
            (changed, advance.frames_advanced > 0)
        };
        if crossed_frame {
            self.last_playback_frame_advance_at.set(Some(observed_at));
        }
        if !playback_changed {
            return false;
        }
        {
            self.refresh_window_preview_state();
            let window_preview_state = self.window_preview_state.borrow().clone();
            let window_preview_snapshot =
                WindowPreviewSnapshot::new(&window_preview_state, &self.preview_service);
            let state = self.app_state.borrow();
            self.root
                .refresh_playback_frame_from_app_state(&state, Some(&window_preview_snapshot));
        }
        self.sync_playback_feedback_from_viewer();
        TreeWalker::layout(self.active_root_mut(), bounds);
        true
    }

    /// Delay until Playback next requires a Clock or bounded-priming wake.
    pub fn playback_next_wake_delay(&self) -> Option<Duration> {
        self.app_state.borrow().playback_next_wake_delay()
    }

    /// Whether transport currently requires the Window playback coordinator.
    pub(crate) fn is_playback_active(&self) -> bool {
        self.app_state.borrow().is_playing()
    }

    /// Whether current-frame work is pending in the Viewer Adapter.
    pub(crate) fn is_playback_frame_pending(&self) -> bool {
        self.app_state.borrow().is_playing()
            && self.playback_feedback == ViewerPlaybackFeedback::Loading
    }

    /// Whether GPU preview preparation should yield to interactive shell input.
    ///
    /// Pending Viewer work does not hold the Clock Master, but another redraw
    /// must not synchronously reconstruct the same GPU candidate. This gate is
    /// presentation feedback only and has no transport authority.
    pub(crate) fn should_defer_gpu_preview_prepare_for_interaction(&self) -> bool {
        self.app_state.borrow().is_playing() && self.playback_feedback.should_defer_gpu_prepare()
    }

    fn sync_playback_feedback_from_viewer(&mut self) -> bool {
        let raw_feedback = self.root.viewer_playback_feedback();
        // Ready and Blocked are projections of work already evaluated by
        // Preview. Payload-free Widget feedback has no demand identity and
        // must never mint or infer terminal authority for whichever demand is
        // current now.
        let feedback_changed = raw_feedback != self.playback_feedback;
        self.playback_feedback = raw_feedback;
        let transport_changed = self.observe_playback_video_preroll();
        if transport_changed {
            self.refresh_transport_state_without_preview();
        }
        feedback_changed || transport_changed
    }

    /// Drain queued widget actions through shell-local handling and `AppState`.
    pub fn drain_pending_actions(
        &mut self,
        pending_actions: &PendingUiActions,
        bounds: Rect,
        platform: &dyn PlatformService,
    ) -> AppUiShellCommands {
        let mut commands = AppUiShellCommands::default();
        let actions = pending_actions.take_all();
        if actions.is_empty() {
            self.refresh_if_dirty(bounds);
            return commands;
        }

        let mut needs_layout = false;
        for action in actions {
            if self.take_pending_close_response(&mut commands, &action) {
                if commands.quit {
                    return commands;
                }
                needs_layout = true;
                continue;
            }
            if self.take_guarded_close_or_quit(&mut commands, &action) {
                if commands.quit {
                    return commands;
                }
                needs_layout = true;
                continue;
            }
            if take_shell_window_command(&mut commands, &action) {
                continue;
            }
            if self.take_startup_action(&action, bounds, platform) {
                needs_layout = true;
                continue;
            }
            if self.take_preferences_update(&action, bounds) {
                needs_layout = true;
                continue;
            }
            if self.take_asset_browser_navigation(&action, bounds) {
                needs_layout = true;
                continue;
            }
            if !self.is_action_enabled(&action) {
                tracing::debug!(?action, "disabled custom UI action ignored");
                continue;
            }

            let workspace_before = self.root.workspace_preset();
            let current_project_path =
                self.app_state.borrow().current_project_path().map(std::path::Path::to_path_buf);
            let action = match self.root.try_handle_shell_action(
                action,
                platform,
                current_project_path.as_deref(),
            ) {
                Ok(Some(action)) => {
                    self.persist_root_workspace_change(workspace_before);
                    action
                }
                Ok(None) => {
                    self.persist_root_workspace_change(workspace_before);
                    needs_layout = true;
                    continue;
                }
                Err(err) => {
                    tracing::warn!("custom UI shell action failed: {err}");
                    self.app_state
                        .borrow_mut()
                        .set_status_hint(format!("UI shell action failed: {err}"), true);
                    self.mark_dirty();
                    needs_layout = true;
                    continue;
                }
            };

            if !self.is_action_enabled(&action) {
                tracing::debug!(?action, "disabled resolved custom UI action ignored");
                continue;
            }
            tracing::debug!(?action, "custom UI action");
            let lightweight_transport_refresh =
                action_prefers_transport_refresh_without_preview(&action);
            if let Err(err) = self.dispatch_editor_action(action) {
                tracing::warn!("custom UI action failed: {err}");
            }
            let transport_intent = self.app_state.borrow().preview_transport_intent();
            self.preview_service.synchronize_transport_intent(transport_intent);
            if lightweight_transport_refresh {
                self.refresh_transport_intent_without_preview();
            } else {
                self.mark_dirty();
            }
            needs_layout = true;
        }

        self.refresh_if_dirty(bounds);
        if needs_layout {
            TreeWalker::layout(self.active_root_mut(), bounds);
        }
        commands
    }

    fn sync_mode_from_app_state(&mut self, bounds: Rect) {
        let next = mode_for_app_state(&self.app_state.borrow());
        if self.mode != next {
            self.mode = next;
            self.refresh_window_preview_state();
            let window_preview_state = self.window_preview_state.borrow().clone();
            let window_preview_snapshot =
                WindowPreviewSnapshot::new(&window_preview_state, &self.preview_service);
            self.root.refresh_from_app_state_with_preferences_thumbnails_and_preview(
                &self.app_state.borrow(),
                &self.preferences,
                Some(&self.asset_thumbnails),
                Some(&window_preview_snapshot),
                Some(self.waveform_service.source()),
            );
            TreeWalker::layout(self.active_root_mut(), bounds);
        }
    }

    fn refresh_transport_state_without_preview(&mut self) {
        let state = self.app_state.borrow();
        self.root.refresh_playback_frame_from_app_state(&state, None);
    }

    fn refresh_transport_intent_without_preview(&mut self) {
        self.mark_window_preview_pending();
        let state = self.app_state.borrow();
        self.root.refresh_transport_intent_from_app_state(&state);
    }

    fn is_action_enabled(&self, action: &Action) -> bool {
        app_state_action_enabled(action, &self.app_state.borrow())
    }

    pub fn sync_workspace_layout_from_root(&mut self) {
        if self.mode != AppUiMode::Workspace {
            return;
        }
        if self.root.sync_custom_workspace_layout_from_dock() {
            self.persist_workspace_preferences_from_root();
        }
    }

    fn persist_root_workspace_change(&mut self, previous: WorkspacePreset) {
        let current = self.root.workspace_preset();
        if current != previous
            || self.preferences.custom_workspace_layout
                != self.root.custom_workspace_layout().cloned()
        {
            self.persist_workspace_preferences_from_root();
        }
    }

    fn persist_workspace_preferences_from_root(&mut self) {
        let preset = self.root.workspace_preset();
        let custom_workspace_layout = self.root.custom_workspace_layout().cloned();
        if self.preferences.workspace_preset == preset
            && self.preferences.custom_workspace_layout == custom_workspace_layout
        {
            return;
        }
        self.preferences.workspace_preset = preset;
        self.preferences.custom_workspace_layout = custom_workspace_layout;
        if let Err(err) = persist_app_ui_preferences_to(&self.preferences_path, &self.preferences) {
            tracing::warn!("failed to persist app UI workspace preference: {err}");
            self.app_state.borrow_mut().set_status_hint(
                format!("Workspace preference could not be saved: {err}"),
                true,
            );
            self.mark_dirty();
        }
    }

    fn take_startup_action(
        &mut self,
        action: &Action,
        bounds: Rect,
        platform: &dyn PlatformService,
    ) -> bool {
        if self.mode != AppUiMode::Startup {
            return false;
        }

        if is_startup_local_shell_action(action) {
            match self.startup.try_handle_shell_action(action.clone(), platform) {
                Ok(Some(resolved)) => {
                    if let Err(err) = self.dispatch_editor_action(resolved) {
                        tracing::warn!("startup local action failed: {err}");
                    }
                    self.mark_dirty();
                    self.refresh_if_dirty(bounds);
                }
                Ok(None) => {
                    TreeWalker::layout(self.active_root_mut(), bounds);
                }
                Err(err) => {
                    tracing::warn!("startup local shell action failed: {err}");
                    self.app_state
                        .borrow_mut()
                        .set_status_hint(format!("Startup action failed: {err}"), true);
                    self.mark_dirty();
                }
            }
            return true;
        }

        if !is_startup_project_action(action) {
            return false;
        }

        match try_resolve_app_shell_action(action.clone(), platform, None) {
            Ok(Some(resolved)) => {
                if let Err(err) = self.dispatch_editor_action(resolved) {
                    tracing::warn!("startup action failed: {err}");
                }
                self.mark_dirty();
                self.refresh_if_dirty(bounds);
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("startup shell action failed: {err}");
                self.app_state
                    .borrow_mut()
                    .set_status_hint(format!("Startup action failed: {err}"), true);
                self.mark_dirty();
            }
        }
        true
    }

    fn dispatch_editor_action(&mut self, action: Action) -> mondrian_core::Result<()> {
        let refresh_recovery_after_failure = is_recovery_project_action(&action);
        let previous_project_path =
            self.app_state.borrow().current_project_path().map(std::path::Path::to_path_buf);
        let previous_status_hint = self.app_state.borrow().status_hint.clone();
        let result = self.app_state.borrow_mut().dispatch_action(action);
        if result.is_ok() {
            let current_project_path =
                self.app_state.borrow().current_project_path().map(std::path::Path::to_path_buf);
            if current_project_path.is_some() && current_project_path != previous_project_path {
                if let Some(path) = current_project_path {
                    self.record_recent_project(path);
                }
                self.refresh_recovery_candidates();
            }
        } else if let Err(err) = &result {
            self.set_unreported_action_error_status(previous_status_hint, err);
            if refresh_recovery_after_failure {
                self.refresh_recovery_candidates();
            }
        }
        result
    }

    fn set_unreported_action_error_status(
        &self,
        previous_status_hint: Option<(String, bool)>,
        err: &mondrian_core::MondrianError,
    ) {
        let mut state = self.app_state.borrow_mut();
        let current_status_hint = state.status_hint.clone();
        let has_new_error = current_status_hint.as_ref().is_some_and(|(_, is_error)| *is_error)
            && current_status_hint != previous_status_hint;
        if !has_new_error {
            state.set_status_hint(format!("操作失败：{err}"), true);
        }
    }

    fn record_recent_project(&mut self, project_file: PathBuf) {
        self.preferences.record_recent_project(project_file);
        self.sync_startup_recent_projects();
        if let Err(err) = persist_app_ui_preferences_to(&self.preferences_path, &self.preferences) {
            tracing::warn!("failed to persist app UI recent projects: {err}");
            self.app_state
                .borrow_mut()
                .set_status_hint(format!("Recent projects could not be saved: {err}"), true);
            self.mark_dirty();
        }
    }

    fn sync_startup_recent_projects(&mut self) {
        self.startup
            .set_recent_projects(startup_recent_projects_from_preferences(&self.preferences));
    }

    fn refresh_recovery_candidates(&mut self) {
        self.recovery_candidates = discover_crash_recovery_candidates();
        self.startup.set_recovery_projects(startup_recovery_projects_from_candidates(
            &self.recovery_candidates,
        ));
    }

    fn take_preferences_update(&mut self, action: &Action, bounds: Rect) -> bool {
        let Some(result) = parse_preferences_update(action) else {
            return false;
        };
        match result {
            Ok(update) => {
                match update {
                    PreferencesUpdate::Theme(payload) => {
                        self.preferences.theme_preference = payload.preference;
                        set_theme_preset(
                            self.preferences.theme_preference.resolve(self.system_theme_preset),
                        );
                    }
                    PreferencesUpdate::WaveformDisplay(payload) => {
                        self.preferences.waveform_display = payload.mode;
                    }
                    PreferencesUpdate::ViewerBackground(payload) => {
                        self.preferences.viewer_canvas_background = payload.background;
                    }
                    PreferencesUpdate::AudioOutputDevice(payload) => {
                        self.preferences.audio_output_device = payload.selection.clone();
                        self.app_state
                            .borrow()
                            .set_audio_output_device_selection(payload.selection);
                    }
                    PreferencesUpdate::RefreshAudioOutputDevices => {
                        self.audio_device_catalog.request_refresh();
                        self.root.set_audio_output_device_catalog(
                            self.audio_device_catalog.state().clone(),
                        );
                        TreeWalker::layout(&mut self.root, bounds);
                        return true;
                    }
                    PreferencesUpdate::ShortcutDisabled(payload) => {
                        if !is_known_shortcut_id(&payload.id) {
                            self.app_state
                                .borrow_mut()
                                .set_status_hint("Shortcut preference is no longer valid", true);
                            self.mark_dirty();
                            return true;
                        }
                        self.preferences.shortcut_overrides.retain(|entry| entry.id != payload.id);
                        self.preferences
                            .shortcut_overrides
                            .push(AppUiShortcutOverride { id: payload.id, binding: None });
                    }
                    PreferencesUpdate::ShortcutReset(payload) => {
                        self.preferences.shortcut_overrides.retain(|entry| entry.id != payload.id);
                    }
                    PreferencesUpdate::ShortcutRebound(payload) => {
                        if !is_known_shortcut_id(&payload.id) {
                            self.app_state
                                .borrow_mut()
                                .set_status_hint("Shortcut preference is no longer valid", true);
                            self.mark_dirty();
                            return true;
                        }
                        let Some(key) = AppUiShortcutKey::from_preference_name(&payload.key) else {
                            self.app_state
                                .borrow_mut()
                                .set_status_hint("Shortcut key is no longer valid", true);
                            self.mark_dirty();
                            return true;
                        };
                        apply_shortcut_rebind(
                            &mut self.preferences.shortcut_overrides,
                            payload.id,
                            AppUiShortcutBinding {
                                key,
                                ctrl: payload.ctrl,
                                alt: payload.alt,
                                shift: payload.shift,
                                meta: payload.meta,
                            },
                        );
                    }
                }
                if let Err(err) =
                    persist_app_ui_preferences_to(&self.preferences_path, &self.preferences)
                {
                    tracing::warn!("failed to persist app UI preferences: {err}");
                    self.app_state
                        .borrow_mut()
                        .set_status_hint(format!("Preferences could not be saved: {err}"), true);
                }
                self.refresh_window_preview_state();
                let window_preview_state = self.window_preview_state.borrow().clone();
                let window_preview_snapshot =
                    WindowPreviewSnapshot::new(&window_preview_state, &self.preview_service);
                self.root.refresh_from_app_state_with_preferences_thumbnails_and_preview(
                    &self.app_state.borrow(),
                    &self.preferences,
                    Some(&self.asset_thumbnails),
                    Some(&window_preview_snapshot),
                    Some(self.waveform_service.source()),
                );
                TreeWalker::layout(&mut self.root, bounds);
                true
            }
            Err(err) => {
                tracing::warn!("invalid app UI preferences action: {err}");
                self.app_state
                    .borrow_mut()
                    .set_status_hint(format!("Preferences action failed: {err}"), true);
                self.mark_dirty();
                true
            }
        }
    }

    fn take_asset_browser_navigation(&mut self, action: &Action, bounds: Rect) -> bool {
        let Some(result) = parse_asset_browser_navigation(action) else {
            return false;
        };
        match result {
            Ok(payload) => {
                let folder_id = self.valid_asset_folder_id(payload.folder_id);
                self.root.set_asset_folder_id(folder_id);
                self.refresh_window_preview_state();
                let window_preview_state = self.window_preview_state.borrow().clone();
                let window_preview_snapshot =
                    WindowPreviewSnapshot::new(&window_preview_state, &self.preview_service);
                self.root.refresh_from_app_state_with_preferences_thumbnails_and_preview(
                    &self.app_state.borrow(),
                    &self.preferences,
                    Some(&self.asset_thumbnails),
                    Some(&window_preview_snapshot),
                    Some(self.waveform_service.source()),
                );
                TreeWalker::layout(&mut self.root, bounds);
                true
            }
            Err(err) => {
                tracing::warn!("invalid app UI asset browser action: {err}");
                self.app_state
                    .borrow_mut()
                    .set_status_hint(format!("Asset browser action failed: {err}"), true);
                self.mark_dirty();
                true
            }
        }
    }

    fn valid_asset_folder_id(&self, folder_id: Option<String>) -> Option<String> {
        let folder_id = folder_id?;
        let exists = {
            let app_state = self.app_state.borrow();
            app_state
                .asset_library()
                .and_then(|library| match library.list_folders() {
                    Ok(folders) => Some(folders.iter().any(|folder| folder.id == folder_id)),
                    Err(err) => {
                        tracing::warn!("failed to list asset folders for navigation: {err}");
                        None
                    }
                })
                .unwrap_or(false)
        };
        exists.then_some(folder_id)
    }

    fn normalize_asset_folder_selection(&mut self) {
        let current = self.root.asset_folder_id().map(str::to_owned);
        let valid = self.valid_asset_folder_id(current.clone());
        if valid != current {
            self.root.set_asset_folder_id(valid);
        }
    }

    fn take_guarded_close_or_quit(
        &mut self,
        commands: &mut AppUiShellCommands,
        action: &Action,
    ) -> bool {
        let Some(pending) = close_request_from_action(action) else {
            return false;
        };
        if self.quiescing_close_action.is_some() {
            return true;
        }
        self.preview_service.cancel_all_work_for_lifecycle();

        let has_unsaved_changes = self.app_state.borrow().has_unsaved_project_changes();
        if has_unsaved_changes {
            self.pending_close_action = Some(pending);
            self.root.show_pending_close_dialog(pending.dialog_action());
            return true;
        }

        self.execute_pending_close_action(commands, pending, None);
        true
    }

    fn take_pending_close_response(
        &mut self,
        commands: &mut AppUiShellCommands,
        action: &Action,
    ) -> bool {
        let Action::Custom { namespace, name, .. } = action else {
            return false;
        };
        if namespace != APP_SHELL_NAMESPACE {
            return false;
        }

        match name.as_str() {
            APP_SHELL_PENDING_CLOSE_SAVE_CONTINUE => {
                let Some(pending) = self.pending_close_action else {
                    self.root.close_pending_close_dialog();
                    return true;
                };
                // Queue the save and immediately place a FIFO close barrier
                // behind it. The barrier, not a UI-thread wait loop, proves
                // the save has published and released its heavy ownership.
                let save_request = match self.app_state.borrow_mut().request_project_save() {
                    Ok(request_id) => request_id,
                    Err(err) => {
                        tracing::warn!("closing project after save failed: {err}");
                        self.app_state
                            .borrow_mut()
                            .set_status_hint(format!("保存项目失败：{err}"), true);
                        self.mark_dirty();
                        return true;
                    }
                };
                self.pending_close_action = None;
                self.root.close_pending_close_dialog();
                self.execute_pending_close_action(commands, pending, Some(save_request));
                true
            }
            APP_SHELL_PENDING_CLOSE_DISCARD => {
                let Some(pending) = self.pending_close_action.take() else {
                    self.root.close_pending_close_dialog();
                    return true;
                };
                self.root.close_pending_close_dialog();
                self.execute_pending_close_action(commands, pending, None);
                true
            }
            APP_SHELL_PENDING_CLOSE_CANCEL => {
                self.pending_close_action = None;
                self.root.close_pending_close_dialog();
                true
            }
            _ => false,
        }
    }

    fn execute_pending_close_action(
        &mut self,
        commands: &mut AppUiShellCommands,
        pending: PendingCloseAction,
        required_save: Option<crate::app::ProjectPersistenceRequestId>,
    ) {
        if self.quiescing_close_action.is_some() {
            return;
        }
        self.preview_service.cancel_all_work_for_lifecycle();
        self.waveform_service.set_library(None);
        if self.app_state.borrow().has_project_close_fault() {
            let forced = self.app_state.borrow_mut().force_close_project_after_fault();
            match forced {
                Ok(()) => {
                    self.pending_close_action = None;
                    self.root.close_pending_close_dialog();
                    if pending == PendingCloseAction::QuitApp {
                        self.preview_service.shutdown();
                        #[cfg(not(test))]
                        super::window::arm_process_exit_watchdog();
                        commands.quit = true;
                    } else {
                        self.refresh_recovery_candidates();
                        self.mark_dirty();
                    }
                }
                Err(err) => {
                    self.waveform_service
                        .set_library(self.app_state.borrow().asset_library_handle());
                    self.app_state
                        .borrow_mut()
                        .set_status_hint(format!("无法放弃失败的项目关闭操作：{err}"), true);
                    self.mark_dirty();
                }
            }
            return;
        }
        let close_start = match required_save {
            Some(request_id) => {
                self.app_state.borrow_mut().begin_project_close_after_save(request_id)
            }
            None => self.app_state.borrow_mut().begin_project_close(),
        };
        match close_start {
            Ok(true) => {
                self.quiescing_close_action = Some(pending);
                self.mark_dirty();
            }
            Ok(false) => {
                if pending == PendingCloseAction::QuitApp {
                    self.preview_service.shutdown();
                    #[cfg(not(test))]
                    super::window::arm_process_exit_watchdog();
                    commands.quit = true;
                } else {
                    self.refresh_recovery_candidates();
                    self.mark_dirty();
                }
            }
            Err(err) => {
                tracing::warn!("guarded Project close failed to start: {err}");
                self.waveform_service
                    .set_library(self.app_state.borrow().asset_library_handle());
                self.app_state
                    .borrow_mut()
                    .set_status_hint(format!("无法开始安全关闭项目：{err}"), true);
                self.mark_dirty();
            }
        }
    }
}

fn apply_execution_resource_policy(
    state: &AppState,
    thumbnails: &AssetThumbnailAdapter,
    waveforms: &AudioWaveformService,
    preview: &WindowPreviewAdapter,
) {
    let thumbnail = thumbnails.diagnostics();
    let waveform = waveforms.diagnostics();
    let decision = state.coordinate_execution_resource_decision(ExternalExecutionResourceDemand {
        thumbnail: ExecutionDomainDemand {
            queued: thumbnail.queued_requests,
            running: thumbnail.running_requests,
            user_initiated: 0,
            terminal_generation: thumbnail
                .completions
                .saturating_add(thumbnail.failures)
                .saturating_add(thumbnail.cancellations)
                .saturating_add(thumbnail.superseded),
        },
        waveform: ExecutionDomainDemand {
            queued: waveform.queued_sources,
            running: waveform.running_sources,
            user_initiated: 0,
            terminal_generation: waveform
                .completions
                .saturating_add(waveform.failures)
                .saturating_add(waveform.cancellations)
                .saturating_add(waveform.superseded_completions),
        },
    });
    let closing = decision.heavy_slots.domains_to_close();
    if closing
        .contains(crate::app::execution_resource_slots::ExecutionResourceSlotDomain::Thumbnail)
    {
        let mut closed = decision.thumbnail;
        closed.dispatch_enabled = false;
        thumbnails.apply_resource_decision(&closed);
        let acknowledged = state.acknowledge_external_execution_resource_domain_closed(
            &decision,
            crate::app::execution_resource_slots::ExecutionResourceSlotDomain::Thumbnail,
        );
        debug_assert!(
            acknowledged,
            "Thumbnail close acknowledgement lost its transition"
        );
    }
    if closing.contains(crate::app::execution_resource_slots::ExecutionResourceSlotDomain::Waveform)
    {
        waveforms.set_resource_policy(
            decision.waveform.automatic_admission_enabled,
            false,
            decision.waveform.cache_budget_bytes,
        );
        let acknowledged = state.acknowledge_external_execution_resource_domain_closed(
            &decision,
            crate::app::execution_resource_slots::ExecutionResourceSlotDomain::Waveform,
        );
        debug_assert!(
            acknowledged,
            "Waveform close acknowledgement lost its transition"
        );
    }
    state.apply_internal_execution_resource_projection(&decision);
    thumbnails.apply_resource_decision(&decision.thumbnail);
    waveforms.set_resource_policy(
        decision.waveform.automatic_admission_enabled,
        decision.waveform.dispatch_enabled,
        decision.waveform.cache_budget_bytes,
    );
    preview.apply_resource_decision(&decision.preview);
    if let Err(error) = mondrian_ui_widgets::vector_icon::configure_vector_icon_raster_cache(
        decision.ui.vector_icon_cache_entries,
        decision.ui.vector_icon_cache_bytes,
    ) {
        tracing::warn!(%error, "failed to apply bounded vector-icon raster policy");
    }
}

fn action_prefers_transport_refresh_without_preview(action: &Action) -> bool {
    matches!(
        action,
        Action::Play
            | Action::Pause
            | Action::TogglePlay
            | Action::Seek(_)
            | Action::StepForward
            | Action::StepBack
            | Action::GoToStart
            | Action::GoToEnd
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingCloseAction {
    CloseProject,
    QuitApp,
}

impl PendingCloseAction {
    fn dialog_action(self) -> PendingCloseDialogAction {
        match self {
            Self::CloseProject => PendingCloseDialogAction::CloseProject,
            Self::QuitApp => PendingCloseDialogAction::QuitApp,
        }
    }
}

fn close_request_from_action(action: &Action) -> Option<PendingCloseAction> {
    match action {
        Action::CloseProject => Some(PendingCloseAction::CloseProject),
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_QUIT =>
        {
            Some(PendingCloseAction::QuitApp)
        }
        _ => None,
    }
}

fn mode_for_app_state(state: &AppState) -> AppUiMode {
    if state.has_open_project() {
        AppUiMode::Workspace
    } else {
        AppUiMode::Startup
    }
}

fn widget_tree_has_transient_interaction(widget: &dyn Widget) -> bool {
    widget.accepts_text_input()
        || widget.overlay_hit_test(Point::new(-1_000_000.0, -1_000_000.0))
        || (0..widget.child_count())
            .any(|index| widget.child(index).is_some_and(widget_tree_has_transient_interaction))
}

fn is_startup_project_action(action: &Action) -> bool {
    matches!(
        action,
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE
                && (name == APP_SHELL_OPEN_PROJECT_DIALOG
                    || name == APP_SHELL_OPEN_RECENT_PROJECT
                    || name == APP_SHELL_RECOVER_PROJECT)
    )
}

fn is_recovery_project_action(action: &Action) -> bool {
    matches!(
        action,
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_RECOVER_PROJECT
    )
}

fn is_startup_local_shell_action(action: &Action) -> bool {
    matches!(
        action,
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE
                && (name == APP_SHELL_NEW_PROJECT_DIALOG
                    || name == APP_SHELL_NEW_PROJECT_DRAFT_CHANGED
                    || name == APP_SHELL_CANCEL_NEW_PROJECT_DIALOG
                    || name == APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG
                    || name == APP_SHELL_RECOVERY_DIALOG
                    || name == APP_SHELL_CONFIRM_RECOVERY_DIALOG
                    || name == APP_SHELL_CLOSE_MODAL)
    )
}

fn startup_recent_projects_from_preferences(
    preferences: &AppUiPreferences,
) -> Vec<StartupRecentProject> {
    preferences
        .recent_projects
        .iter()
        .map(|project_file| StartupRecentProject {
            project_file: project_file.clone(),
            title: recent_project_title(project_file),
            subtitle: recent_project_subtitle(project_file),
        })
        .collect()
}

fn startup_recovery_projects_from_candidates(
    candidates: &[CrashRecoveryCandidate],
) -> Vec<StartupRecoveryProject> {
    candidates
        .iter()
        .map(|candidate| {
            let snapshots = if candidate.total_snapshots > 1 {
                format!("，共 {} 个恢复点", candidate.total_snapshots)
            } else {
                String::new()
            };
            StartupRecoveryProject {
                candidate: candidate.clone(),
                title: recent_project_title(&candidate.project_file),
                detail: format!(
                    "{}{} · {}",
                    recovery_age_label(candidate.saved_at_unix_ms),
                    snapshots,
                    recent_project_subtitle(&candidate.project_file)
                ),
            }
        })
        .collect()
}

fn recent_project_title(project_file: &Path) -> String {
    project_file
        .file_stem()
        .or_else(|| project_file.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| project_file.display().to_string())
}

fn recent_project_subtitle(project_file: &Path) -> String {
    let metadata = std::fs::metadata(project_file).ok();
    let modified = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
        .map(recent_project_modified_label)
        .unwrap_or_else(|| "未知时间".to_owned());
    let size = metadata
        .as_ref()
        .map(|metadata| format_file_size(metadata.len()))
        .unwrap_or_else(|| "--".to_owned());
    format!("{modified} • {size}")
}

fn recent_project_modified_label(modified: SystemTime) -> String {
    let age_secs = SystemTime::now()
        .duration_since(modified)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    if age_secs < 60 {
        "刚刚".to_owned()
    } else if age_secs < 3600 {
        format!("{} 分钟前", age_secs / 60)
    } else if age_secs < 86_400 {
        format!("{} 小时前", age_secs / 3600)
    } else if age_secs < 172_800 {
        "昨天".to_owned()
    } else if age_secs < 604_800 {
        format!("{} 天前", age_secs / 86_400)
    } else {
        format!("{} 周前", age_secs / 604_800)
    }
}

fn format_file_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else if size >= 10.0 {
        format!("{size:.0} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn take_shell_window_command(commands: &mut AppUiShellCommands, action: &Action) -> bool {
    match action {
        Action::ToggleFullscreen => {
            commands.toggle_fullscreen = true;
            true
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_QUIT =>
        {
            commands.quit = true;
            true
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_WINDOW_MINIMIZE =>
        {
            commands.minimize = true;
            true
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_WINDOW_TOGGLE_MAXIMIZE =>
        {
            commands.toggle_maximize = true;
            true
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_WINDOW_DRAG =>
        {
            commands.begin_window_drag = true;
            true
        }
        _ => false,
    }
}

fn apply_shortcut_rebind(
    overrides: &mut Vec<AppUiShortcutOverride>,
    id: String,
    binding: AppUiShortcutBinding,
) {
    let core_binding = binding.to_core();
    overrides.retain(|entry| {
        entry.id != id
            && entry.binding.map(|existing| existing.to_core() != core_binding).unwrap_or(true)
    });

    let defaults = default_shortcuts();
    for shortcut in &defaults {
        if shortcut.id == id || shortcut.binding != core_binding {
            continue;
        }
        if !overrides.iter().any(|entry| entry.id == shortcut.id) {
            overrides.push(AppUiShortcutOverride { id: shortcut.id.to_owned(), binding: None });
        }
    }

    if defaults
        .iter()
        .find(|shortcut| shortcut.id == id)
        .is_some_and(|shortcut| shortcut.binding == core_binding)
    {
        return;
    }

    overrides.push(AppUiShortcutOverride { id, binding: Some(binding) });
}

enum PreferencesUpdate {
    Theme(PreferencesThemePayload),
    WaveformDisplay(PreferencesWaveformDisplayPayload),
    ViewerBackground(PreferencesViewerBackgroundPayload),
    AudioOutputDevice(PreferencesAudioOutputDevicePayload),
    RefreshAudioOutputDevices,
    ShortcutDisabled(PreferencesShortcutPayload),
    ShortcutReset(PreferencesShortcutPayload),
    ShortcutRebound(PreferencesShortcutReboundPayload),
}

fn parse_preferences_update(
    action: &Action,
) -> Option<Result<PreferencesUpdate, serde_json::Error>> {
    match action {
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_PREFERENCES_THEME_CHANGED =>
        {
            Some(serde_json::from_value(payload.clone()).map(PreferencesUpdate::Theme))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE
                && name == APP_SHELL_PREFERENCES_WAVEFORM_DISPLAY_CHANGED =>
        {
            Some(serde_json::from_value(payload.clone()).map(PreferencesUpdate::WaveformDisplay))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE
                && name == APP_SHELL_PREFERENCES_VIEWER_BACKGROUND_CHANGED =>
        {
            Some(serde_json::from_value(payload.clone()).map(PreferencesUpdate::ViewerBackground))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE
                && name == APP_SHELL_PREFERENCES_AUDIO_OUTPUT_DEVICE_CHANGED =>
        {
            Some(serde_json::from_value(payload.clone()).map(PreferencesUpdate::AudioOutputDevice))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE
                && name == APP_SHELL_PREFERENCES_REFRESH_AUDIO_OUTPUT_DEVICES =>
        {
            Some(Ok(PreferencesUpdate::RefreshAudioOutputDevices))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE
                && name == APP_SHELL_PREFERENCES_SHORTCUT_DISABLED =>
        {
            Some(serde_json::from_value(payload.clone()).map(PreferencesUpdate::ShortcutDisabled))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_PREFERENCES_SHORTCUT_RESET =>
        {
            Some(serde_json::from_value(payload.clone()).map(PreferencesUpdate::ShortcutReset))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE
                && name == APP_SHELL_PREFERENCES_SHORTCUT_REBOUND =>
        {
            Some(serde_json::from_value(payload.clone()).map(PreferencesUpdate::ShortcutRebound))
        }
        _ => None,
    }
}

fn parse_asset_browser_navigation(
    action: &Action,
) -> Option<Result<AssetsOpenFolderPayload, serde_json::Error>> {
    match action {
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_ASSET_BROWSER_OPEN_FOLDER =>
        {
            Some(serde_json::from_value(payload.clone()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_assets::AssetLibrary;
    use mondrian_core::types::{AssetId, ClipId, Color, TrackId};
    use mondrian_core::{FramePosition, TimelineTime};
    use mondrian_editor_state::state::PanelKind;
    use mondrian_editor_state::Action;
    use mondrian_media::PreviewHardwareDecodeRequest;
    use mondrian_platform::{
        ClipboardError, FileDialogError, FileDialogOutcome, FileFilter, FileRevealError,
        NoopPlatformService,
    };
    use mondrian_timeline::{Clip, Sequence};
    use mondrian_ui_core::tree::TreeWalker;
    use mondrian_ui_core::types::{Modifiers, MouseButton, Point, Rect, SplitDirection};
    use mondrian_ui_core::widget::EventContext;
    use mondrian_ui_core::{EventRequests, EventResult, RasterImageColorSpace, UiEvent, Widget};
    use mondrian_ui_theme::{current_theme, ThemePreference, ThemePreset};
    use mondrian_ui_widgets::{
        ViewerCanvasBackground, ViewerExternalTextureFrame, ViewerExternalTexturePresentation,
        ViewerFrameContent, ViewerFrameImage, WaveformDisplay,
    };
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::app_ui::preferences_store::{load_app_ui_preferences_from, AppUiPreferences};
    use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use crate::app_ui::workspace_layout::AppUiWorkspaceLayout;

    #[test]
    fn bounded_background_remainder_requests_poll_without_repaint() {
        let outcome = AppUiBackgroundTaskPollOutcome::from_changes(
            false,
            PlaybackPreviewPumpOutcome {
                needs_follow_up_poll: true,
                ..PlaybackPreviewPumpOutcome::default()
            },
        );

        assert!(!outcome.repaint_required);
        assert!(outcome.needs_follow_up_poll);
    }

    #[test]
    fn visible_transport_and_full_model_changes_each_require_repaint() {
        for (full_model_changed, preview) in [
            (
                true,
                PlaybackPreviewPumpOutcome {
                    needs_follow_up_poll: true,
                    ..PlaybackPreviewPumpOutcome::default()
                },
            ),
            (
                false,
                PlaybackPreviewPumpOutcome {
                    visible_change: true,
                    ..PlaybackPreviewPumpOutcome::default()
                },
            ),
            (
                false,
                PlaybackPreviewPumpOutcome {
                    transport_change: true,
                    ..PlaybackPreviewPumpOutcome::default()
                },
            ),
            (
                false,
                PlaybackPreviewPumpOutcome {
                    candidate_retry_required: true,
                    ..PlaybackPreviewPumpOutcome::default()
                },
            ),
        ] {
            assert!(
                AppUiBackgroundTaskPollOutcome::from_changes(full_model_changed, preview)
                    .repaint_required
            );
        }
    }

    #[test]
    fn host_reports_renderer_native_import_admission_to_preview() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let host = AppUiHost::new(AppState::new());

        host.set_native_decoded_frame_import_support(
            GpuNativeDecodedFrameImportSupport::unavailable(),
        );
        assert_eq!(
            host.preview_service.playback_hardware_decode_request_for_test(),
            PreviewHardwareDecodeRequest::PreferHardwareDecode
        );
        assert!(
            host.preview_service
                .diagnostics()
                .hardware_decode_admission
                .renderer_native_import_support_known
        );
        assert!(
            !host
                .preview_service
                .diagnostics()
                .hardware_decode_admission
                .renderer_native_import_ready
        );
        assert!(
            !host
                .preview_service
                .diagnostics()
                .hardware_decode_admission
                .native_import_admission_ready
        );
    }

    #[derive(Default)]
    struct CountingPlatform {
        open_file_dialog_calls: AtomicUsize,
    }

    struct StartupProjectPlatform {
        project_file: PathBuf,
    }

    #[derive(Default)]
    struct ProjectDialogPlatform {
        open_paths: Option<Vec<PathBuf>>,
        save_path: Option<PathBuf>,
    }

    #[derive(Default)]
    struct RecordingPreviewViewerGpuResourceOwner {
        grant: Option<mondrian_renderer::ViewerGpuExecutionResourceGrant>,
        clear_idle_calls: Cell<usize>,
    }

    #[derive(Clone, Default)]
    struct ManualPreviewSchedulerClock {
        now_nanos: Arc<AtomicU64>,
    }

    impl ManualPreviewSchedulerClock {
        fn advance(&self, duration: Duration) {
            let delta = duration.as_nanos().min(u64::MAX as u128) as u64;
            let current = self.now_nanos.load(Ordering::Acquire);
            self.now_nanos.store(current.saturating_add(delta), Ordering::Release);
        }
    }

    impl mondrian_playback::MonotonicRuntimeClock for ManualPreviewSchedulerClock {
        fn now(&self) -> mondrian_playback::MonotonicTimestamp {
            mondrian_playback::MonotonicTimestamp::from_duration(Duration::from_nanos(
                self.now_nanos.load(Ordering::Acquire),
            ))
        }
    }

    impl PreviewViewerGpuResourceOwner for RecordingPreviewViewerGpuResourceOwner {
        fn reconfigure_resource_grant(
            &mut self,
            grant: mondrian_renderer::ViewerGpuExecutionResourceGrant,
        ) {
            self.grant = Some(grant);
        }

        fn clear_idle_resources(&self) {
            self.clear_idle_calls.set(self.clear_idle_calls.get().saturating_add(1));
        }
    }

    impl PlatformService for CountingPlatform {
        fn clipboard_copy(&self, _text: &str) -> Result<(), ClipboardError> {
            Err(ClipboardError::Unavailable)
        }

        fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
            Err(ClipboardError::Unavailable)
        }

        fn open_file_dialog(
            &self,
            _title: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<Vec<PathBuf>>, FileDialogError> {
            self.open_file_dialog_calls.fetch_add(1, Ordering::Relaxed);
            Ok(FileDialogOutcome::Selected(vec![PathBuf::from(
                "E:/media/a.mov",
            )]))
        }

        fn save_file_dialog(
            &self,
            _title: &str,
            _default_name: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<PathBuf>, FileDialogError> {
            Ok(FileDialogOutcome::Cancelled)
        }

        fn reveal_in_file_manager(&self, _path: &Path) -> Result<(), FileRevealError> {
            Ok(())
        }
    }

    impl PlatformService for StartupProjectPlatform {
        fn clipboard_copy(&self, _text: &str) -> Result<(), ClipboardError> {
            Err(ClipboardError::Unavailable)
        }

        fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
            Err(ClipboardError::Unavailable)
        }

        fn open_file_dialog(
            &self,
            _title: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<Vec<PathBuf>>, FileDialogError> {
            Ok(FileDialogOutcome::Cancelled)
        }

        fn save_file_dialog(
            &self,
            _title: &str,
            _default_name: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<PathBuf>, FileDialogError> {
            Ok(FileDialogOutcome::Selected(self.project_file.clone()))
        }

        fn reveal_in_file_manager(&self, _path: &Path) -> Result<(), FileRevealError> {
            Ok(())
        }
    }

    impl PlatformService for ProjectDialogPlatform {
        fn clipboard_copy(&self, _text: &str) -> Result<(), ClipboardError> {
            Err(ClipboardError::Unavailable)
        }

        fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
            Err(ClipboardError::Unavailable)
        }

        fn open_file_dialog(
            &self,
            _title: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<Vec<PathBuf>>, FileDialogError> {
            Ok(match self.open_paths.clone() {
                Some(paths) => FileDialogOutcome::Selected(paths),
                None => FileDialogOutcome::Cancelled,
            })
        }

        fn save_file_dialog(
            &self,
            _title: &str,
            _default_name: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<PathBuf>, FileDialogError> {
            Ok(match self.save_path.clone() {
                Some(path) => FileDialogOutcome::Selected(path),
                None => FileDialogOutcome::Cancelled,
            })
        }

        fn reveal_in_file_manager(&self, _path: &Path) -> Result<(), FileRevealError> {
            Ok(())
        }
    }

    fn temp_preferences_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("mondrian-host-{name}-{nanos}.json"))
    }

    fn temp_asset_library_dir(name: &str) -> PathBuf {
        temp_preferences_path(name).with_extension("assets")
    }

    fn workspace_app_state() -> AppState {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("Edit")));
        state.test_set_project_path(PathBuf::from("E:/projects/edit.mdp"));
        state.test_advance_project_generation();
        state
    }

    fn workspace_app_state_with_timed_solid() -> AppState {
        let mut state = workspace_app_state();
        let sequence = state.active_sequence_mut_uncommitted().expect("active Sequence");
        let time_base = sequence.time_base();
        let duration = TimelineTime::from_frame_position(FramePosition::new(100, time_base))
            .expect("fixture duration");
        let mut clip = Clip::new_solid_color(
            AssetId::new(),
            Color::from_rgba8(24, 80, 160, 255),
            TimelineTime::ZERO,
            duration,
        )
        .expect("timed solid fixture");
        // Disabled author content still gives Playback a real Sequence range
        // while Viewer evaluation remains the deterministic transparent canvas.
        clip.is_disabled = true;
        sequence.video_tracks[0].add_clip(clip).expect("add timed solid fixture");
        state
    }

    fn workspace_host_without_preview_workers(name: &str) -> AppUiHost {
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state(),
            AppUiPreferences::default(),
            temp_preferences_path(name),
        );
        host.preview_service.shutdown();
        host.preview_service = WindowPreviewAdapter::new_without_workers_for_test();
        let transport_intent = host.app_state.borrow().preview_transport_intent();
        host.preview_service.synchronize_transport_intent(transport_intent);
        host
    }

    fn test_window_raster(key: &str) -> ViewerPreviewState {
        ViewerPreviewState::Ready(ViewerFrameContent::Raster(
            ViewerFrameImage::new(key, 1, 1, RasterImageColorSpace::Srgb, vec![0, 0, 0, 255])
                .expect("valid test raster"),
        ))
    }

    fn seed_window_gpu_output(host: &AppUiHost) {
        let key = crate::app::preview_execution::PreviewOutputKey::new(
            mondrian_core::types::SequenceId::new(),
            1,
            1,
            crate::app::preview_execution::PreviewSemanticIdentity::from_test_fingerprint([7; 32]),
        );
        let output = ViewerExternalTextureFrame::new_spatial(
            "seeded-window-gpu",
            ViewerExternalTexturePresentation::full_frame(1, 1).expect("valid seeded presentation"),
        )
        .expect("valid seeded external frame");
        host.preview_service.register_gpu_output(key, output);
        assert!(host.preview_service.has_retained_gpu_output());
    }

    #[test]
    fn exact_gpu_artifact_revocation_clears_its_visible_widget_projection() {
        let host = workspace_host_without_preview_workers("exact-gpu-artifact-revocation");
        let key = crate::app::preview_execution::PreviewOutputKey::new(
            mondrian_core::types::SequenceId::new(),
            1,
            1,
            crate::app::preview_execution::PreviewSemanticIdentity::from_test_fingerprint([8; 32]),
        );
        let output = ViewerExternalTextureFrame::new_spatial(
            "window-gpu:submission:7",
            ViewerExternalTexturePresentation::full_frame(1, 1)
                .expect("valid external presentation"),
        )
        .expect("valid external frame");
        host.preview_service.register_gpu_output(key.clone(), output.clone());
        host.window_preview_state.replace(ViewerPreviewState::Ready(
            ViewerFrameContent::ExternalTexture(output),
        ));

        assert!(
            !host.clear_external_viewer_frame_for_artifact(&key, "window-gpu:submission:older"),
            "a different physical submission must not revoke the current artifact"
        );
        assert!(matches!(
            &*host.window_preview_state.borrow(),
            ViewerPreviewState::Ready(ViewerFrameContent::ExternalTexture(frame))
                if frame.key == "window-gpu:submission:7"
        ));

        assert!(host.clear_external_viewer_frame_for_artifact(&key, "window-gpu:submission:7"));
        assert!(!host.preview_service.has_retained_gpu_output());
        assert!(matches!(
            &*host.window_preview_state.borrow(),
            ViewerPreviewState::Loading
        ));
    }

    #[test]
    fn paused_untimed_current_output_completes_through_window_presentation_authority() {
        let host = workspace_host_without_preview_workers("paused-current-authority");
        let ticket = {
            let mut state = host.app_state.borrow_mut();
            state.set_playback_frame_running(4);
            state.pause().expect("pause");
            state
                .playback_frame_presentation_ticket(
                    mondrian_playback::FramePresentationQuality::Ready,
                )
                .expect("paused current demand")
        };
        assert_eq!(ticket.deadline(), None);
        seed_window_gpu_output(&host);
        host.window_preview_state.replace(test_window_raster("retained-current"));

        let disposition =
            host.present_current_viewer_output(PreviewPresentationCandidate::new((), Some(ticket)));

        assert!(matches!(
            disposition,
            FramePresentationDisposition::Presented(completion)
                if completion.delivery().kind()
                    == mondrian_playback::FrameDeliveryKind::Ready
        ));
        assert!(
            host.app_state.borrow().pending_playback_frame_demand_identity().is_none(),
            "the paused untimed demand must not remain permanently Loading"
        );
        assert!(matches!(
            &*host.window_preview_state.borrow(),
            ViewerPreviewState::Ready(_)
        ));
        assert!(!host.preview_service.has_retained_gpu_output());
    }

    #[test]
    fn transparent_canvas_completes_its_bound_window_demand() {
        let host = workspace_host_without_preview_workers("transparent-authority");
        let ticket = {
            let mut state = host.app_state.borrow_mut();
            state.set_playback_frame_running(4);
            state.pause().expect("pause");
            state
                .playback_frame_presentation_ticket(
                    mondrian_playback::FramePresentationQuality::Ready,
                )
                .expect("transparent demand")
        };
        seed_window_gpu_output(&host);

        let disposition = host
            .present_transparent_viewer_output(PreviewPresentationCandidate::new((), Some(ticket)));

        assert!(matches!(
            disposition,
            FramePresentationDisposition::Presented(completion)
                if completion.delivery().kind()
                    == mondrian_playback::FrameDeliveryKind::Ready
        ));
        assert!(matches!(
            &*host.window_preview_state.borrow(),
            ViewerPreviewState::Transparent
        ));
        assert!(host.app_state.borrow().pending_playback_frame_demand_identity().is_none());
        assert!(!host.preview_service.has_retained_gpu_output());
    }

    #[test]
    fn superseded_window_candidate_cannot_complete_the_replacement_demand() {
        let host = workspace_host_without_preview_workers("superseded-candidate");
        let ticket_a = {
            let mut state = host.app_state.borrow_mut();
            state.set_playback_frame_running(4);
            state
                .playback_frame_presentation_ticket(
                    mondrian_playback::FramePresentationQuality::Ready,
                )
                .expect("candidate A demand")
        };
        let replacement_identity = {
            let mut state = host.app_state.borrow_mut();
            state.set_playback_frame_running(7);
            state.pending_playback_frame_demand_identity().expect("replacement demand B")
        };
        host.window_preview_state.replace(test_window_raster("candidate-a"));

        let disposition = host
            .present_current_viewer_output(PreviewPresentationCandidate::new((), Some(ticket_a)));

        assert_eq!(disposition, FramePresentationDisposition::LostAuthority);
        assert_eq!(
            host.app_state.borrow().pending_playback_frame_demand_identity(),
            Some(replacement_identity)
        );
        assert!(matches!(
            &*host.window_preview_state.borrow(),
            ViewerPreviewState::Stale(_)
        ));
        assert_eq!(
            host.app_state.borrow().playback_evidence_report().deliveries.ready,
            0
        );
    }

    #[test]
    fn late_window_raster_never_replaces_the_authorized_output() {
        let host = workspace_host_without_preview_workers("late-raster");
        let ticket = {
            let mut state = host.app_state.borrow_mut();
            state.set_playback_frame_running(4);
            state
                .playback_frame_presentation_ticket(
                    mondrian_playback::FramePresentationQuality::Ready,
                )
                .expect("timed raster demand")
        };
        host.window_preview_state.replace(test_window_raster("authorized-old"));
        let late = test_window_raster("late-new");

        let disposition = host.admit_window_preview_state_at(
            Some(ticket),
            late,
            Instant::now() + Duration::from_secs(10),
        );

        assert!(matches!(
            disposition,
            FramePresentationDisposition::DroppedLate(completion)
                if completion.delivery().kind()
                    == mondrian_playback::FrameDeliveryKind::Late
        ));
        let ViewerPreviewState::Stale(ViewerFrameContent::Raster(frame)) =
            &*host.window_preview_state.borrow()
        else {
            panic!("the previously authorized raster must remain stale-visible");
        };
        assert_eq!(frame.key, "authorized-old");
        assert_eq!(
            host.app_state.borrow().playback_evidence_report().deliveries.late,
            1
        );
    }

    #[test]
    fn repeated_demand_free_current_output_does_not_self_schedule_preview_refresh() {
        let host = workspace_host_without_preview_workers("demand-free-current-idempotence");
        {
            let mut state = host.app_state.borrow_mut();
            if let Some(ticket) = state.playback_frame_presentation_ticket(
                mondrian_playback::FramePresentationQuality::Ready,
            ) {
                assert!(
                    state.complete_frame_presentation(ticket, Instant::now()).is_some(),
                    "fixture demand must be settled before testing demand-free publication"
                );
            }
        }
        host.window_preview_state.replace(test_window_raster("already-current"));
        host.preview_dirty.set(false);

        for _ in 0..2 {
            let disposition =
                host.present_current_viewer_output(PreviewPresentationCandidate::new((), None));
            assert_eq!(disposition, FramePresentationDisposition::NoDemand);
            assert!(
                !host.preview_dirty.get(),
                "an idempotent Current projection must not create a repaint loop"
            );
        }
    }

    #[test]
    fn per_frame_viewer_projection_does_not_reapply_preview_runtime_policy() {
        let host = workspace_host_without_preview_workers("viewer-resource-owner-boundary");
        let before = host.preview_service.diagnostics().resource_decision_applications;
        let expected = host.app_state.borrow().execution_resource_decision().preview.viewer_gpu;
        let mut owner = RecordingPreviewViewerGpuResourceOwner::default();

        host.apply_preview_execution_resource_decision(&mut owner);

        assert_eq!(owner.grant, Some(expected.grant));
        assert_eq!(
            owner.clear_idle_calls.get(),
            usize::from(expected.clear_idle)
        );
        assert_eq!(
            host.preview_service.diagnostics().resource_decision_applications,
            before,
            "per-frame Viewer projection must not reconfigure Preview scheduling or caches"
        );
    }

    fn poll_host_background_tasks_until_imports_idle(host: &mut AppUiHost) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while host.app_state().pending_media_import_batches() > 0 {
            host.poll_background_tasks(Rect::new(0.0, 0.0, 1280.0, 720.0));
            if host.app_state().pending_media_import_batches() == 0 {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for host background media import"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn poll_host_background_tasks_until(
        host: &mut AppUiHost,
        description: &str,
        condition: impl Fn(&AppState) -> bool,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if condition(&host.app_state()) {
                return;
            }
            host.poll_background_tasks(Rect::new(0.0, 0.0, 1280.0, 720.0));
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {description}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn poll_host_until_project_close_finishes(host: &mut AppUiHost) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut quit_requested = false;
        while host.quiescing_close_action.is_some() {
            let outcome = host.poll_background_tasks(Rect::new(0.0, 0.0, 1280.0, 720.0));
            quit_requested |= outcome.quit_requested;
            assert!(
                Instant::now() < deadline,
                "timed out waiting for Project close"
            );
            std::thread::yield_now();
        }
        quit_requested
    }

    #[test]
    fn workspace_layout_exposes_viewer_gpu_presentation_geometry() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state(),
            AppUiPreferences::default(),
            temp_preferences_path("viewer-presentation-geometry"),
        );
        let bounds = Rect::new(0.0, 0.0, 1408.0, 736.0);
        host.refresh_if_dirty(bounds);
        TreeWalker::layout(host.active_root_mut(), bounds);

        let geometry = host
            .viewer_presentation_geometry()
            .expect("laid out workspace Viewer should expose GPU presentation geometry");

        assert!(geometry.presentation.output_width > 0);
        assert!(geometry.presentation.output_height > 0);
        assert!(geometry.visible_rect.width > 0.0);
        assert!(geometry.visible_rect.height > 0.0);
    }

    #[test]
    fn editor_action_error_without_status_hint_surfaces_in_status_bar_state() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            AppUiPreferences::default(),
            temp_preferences_path("action-error-status"),
        );

        let err = host
            .dispatch_editor_action(Action::Custom {
                namespace: crate::app::ui_actions::TIMELINE_NAMESPACE.to_owned(),
                name: "unknown".to_owned(),
                payload: serde_json::Value::Null,
            })
            .expect_err("unknown registered UI action should fail");

        let state = host.app_state();
        let (message, is_error) = state.status_hint.as_ref().expect("status hint");
        assert!(*is_error);
        assert!(message.contains("操作失败"));
        assert!(message.contains("unknown app UI action"));
        assert!(err.to_string().contains("unknown app UI action"));
    }

    #[test]
    fn editor_action_error_keeps_specific_status_hint_from_app_state() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut state = AppState::new();
        state.set_status_hint("Previous failure", true);
        let mut host = AppUiHost::new_with_preferences_path(
            state,
            AppUiPreferences::default(),
            temp_preferences_path("action-specific-error-status"),
        );

        let _ = host
            .dispatch_editor_action(Action::ImportMedia(vec![PathBuf::from("")]))
            .expect_err("empty import should fail");

        let state = host.app_state();
        let (message, is_error) = state.status_hint.as_ref().expect("status hint");
        assert!(*is_error);
        assert_ne!(message, "Previous failure");
        assert!(message.contains("导入失败"));
    }

    fn saved_workspace_app_state(name: &str) -> AppState {
        let mut state = AppState::new();
        let project_file = temp_preferences_path(name).with_extension("mdp");
        state
            .create_new_project_at(
                project_file,
                "Edit",
                1920,
                1080,
                mondrian_core::Rational::FPS_2997,
            )
            .expect("project should be created");
        let sequence_id = state.active_sequence_id().expect("active Sequence");
        state
            .rename_sequence(sequence_id, "Changed")
            .expect("rename through author transaction");
        state
    }

    fn create_project_file(name: &str) -> PathBuf {
        let project_file = temp_preferences_path(name).with_extension("mdp");
        let mut state = AppState::new();
        state
            .create_new_project_at(
                project_file.clone(),
                name,
                1920,
                1080,
                mondrian_core::Rational::FPS_2997,
            )
            .expect("project should be created");
        project_file
    }

    fn cleanup_project_file(project_file: &Path) {
        let _ = std::fs::remove_file(project_file);
        let runtime_roots =
            crate::app::project_runtime::project_runtime_roots_for_path_for_test(project_file)
                .unwrap_or_default();
        for runtime_root in runtime_roots {
            let _ = std::fs::remove_dir_all(runtime_root);
        }
    }

    fn write_minimal_wav(path: &Path) {
        let sample_rate = 8_000u32;
        let channels = 1u16;
        let bits_per_sample = 16u16;
        let samples = [0i16; 16];
        let data_size = (samples.len() * std::mem::size_of::<i16>()) as u32;
        let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
        let block_align = channels * bits_per_sample / 8;
        let mut bytes = Vec::with_capacity(44 + data_size as usize);

        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }

        std::fs::write(path, bytes).expect("write wav fixture");
    }

    fn dispatch_ctx<'a>(
        focus: &'a mut DummyFocus,
        shortcut: &'a mut DummyShortcut,
        tooltip: &'a mut DummyTooltip,
        requests: &'a mut EventRequests,
    ) -> EventContext<'a> {
        event_ctx(focus, shortcut, tooltip, requests, &|_| {})
    }

    fn drag_first_splitter_to(root: &mut AppUiAppRoot, ratio: f32) {
        let (zone, direction) = root.dock().collect_grab_zones()[0];
        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);
        let target = match direction {
            SplitDirection::Horizontal => Point::new(bounds.width * ratio, zone.center().y),
            SplitDirection::Vertical => Point::new(zone.center().x, bounds.height * ratio),
        };
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatch_ctx(&mut focus, &mut shortcut, &mut tooltip, &mut requests);

        assert_eq!(
            root.dock_mut().event(
                &UiEvent::MouseDown {
                    position: zone.center(),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            root.dock_mut().event(
                &UiEvent::MouseMove { position: target, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            root.dock_mut().event(
                &UiEvent::MouseUp {
                    position: target,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
    }

    fn click_root(root: &mut AppUiAppRoot, position: Point) {
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatch_ctx(&mut focus, &mut shortcut, &mut tooltip, &mut requests);
        assert_eq!(
            root.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            root.event(
                &UiEvent::MouseUp {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
    }

    #[test]
    fn host_builds_root_from_initial_app_state() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(AppState::new());

        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(host.mode(), AppUiMode::Startup);
        assert!(!host.root().dock().collect_grab_zones().is_empty());
        assert!(!host.app_state().has_open_project());
    }

    #[test]
    fn startup_new_project_action_switches_to_workspace_mode() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let project_file = temp_preferences_path("startup-project").with_extension("mdp");
        let preferences_path = temp_preferences_path("startup-project-preferences");
        let platform = StartupProjectPlatform { project_file: project_file.clone() };
        let mut host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            AppUiPreferences::default(),
            preferences_path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_new_project_dialog_action());
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.mode(), AppUiMode::Startup);
        assert!(host.startup.has_modal());
        assert!(!host.app_state().has_open_project());

        pending.push(crate::app::ui_actions::app_shell_confirm_new_project_dialog_action());
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.mode(), AppUiMode::Workspace);
        assert!(!host.startup.has_modal());
        assert!(host.app_state().has_open_project());
        assert_eq!(
            host.app_state().current_project_path(),
            Some(project_file.as_path())
        );
        assert_eq!(
            host.preferences().recent_projects,
            vec![project_file.clone()]
        );
        assert_eq!(host.startup.recent_project_count(), 1);
        assert_eq!(
            load_app_ui_preferences_from(&preferences_path).recent_projects,
            vec![project_file.clone()]
        );
        let runtime_root = host
            .app_state()
            .authoring
            .as_ref()
            .expect("created Project Session")
            .runtime_root()
            .to_path_buf();
        host.app_state.borrow_mut().close_project().expect("close created Project");
        drop(host);

        let _ = std::fs::remove_file(project_file);
        let _ = std::fs::remove_file(preferences_path);
        let _ = std::fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn transport_action_drain_does_not_request_preview_refresh() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state(),
            AppUiPreferences::default(),
            temp_preferences_path("transport-preview-free-refresh"),
        );
        let pending = PendingUiActions::default();
        let before_render_requests = host.preview_service.diagnostics().render_requests;

        pending.push(Action::TogglePlay);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().is_playing());
        assert_eq!(
            host.preview_service.diagnostics().render_requests,
            before_render_requests,
            "transport actions must update controls without synchronously requesting preview"
        );
        assert!(
            !host.ui_dirty.get(),
            "transport actions should not leave a full preview-backed refresh queued"
        );
    }

    #[test]
    fn preview_presentation_refresh_keeps_shell_widget_identity_and_global_refresh_clean() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state(),
            AppUiPreferences::default(),
            temp_preferences_path("preview-presentation-refresh-domain"),
        );
        TreeWalker::layout(host.root_mut(), bounds);
        let titlebar_id = host.root().title_bar_id_for_test();

        host.clear_external_viewer_frame();

        assert!(host.preview_dirty.get());
        assert!(
            !host.ui_dirty.get(),
            "Preview presentation changes must not request a global model rebuild"
        );
        host.refresh_if_dirty(bounds);

        assert!(!host.preview_dirty.get());
        assert!(!host.ui_dirty.get());
        assert_eq!(
            host.root().title_bar_id_for_test(),
            titlebar_id,
            "Preview refresh must retain the persistent shell chrome tree"
        );
    }

    #[test]
    fn transport_action_while_preview_pending_cancels_obsolete_work() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = workspace_host_without_preview_workers("transport-cancel-preview-work");
        host.preview_service.seed_pending_preview_work_for_test();
        let before_render_requests = host.preview_service.diagnostics().render_requests;
        assert_eq!(
            host.preview_service.diagnostics().scheduler.pending_requests,
            1
        );
        assert_eq!(
            host.preview_service.diagnostics().worker_queue.queued_jobs,
            1
        );

        let pending = PendingUiActions::default();
        pending.push(Action::Play);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().is_playing());
        let diagnostics = host.preview_service.diagnostics();
        assert_eq!(diagnostics.interactive_cancel_requests, 1);
        assert_eq!(diagnostics.interactive_cancel_scheduler_requests, 1);
        assert_eq!(diagnostics.interactive_cancel_queued_jobs, 1);
        assert_eq!(diagnostics.queue_canceled_jobs, 1);
        assert_eq!(diagnostics.scheduler.pending_requests, 0);
        assert_eq!(diagnostics.worker_queue.queued_jobs, 0);
        assert_eq!(
            diagnostics.render_requests, before_render_requests,
            "transport synchronization must not synchronously request Preview while retiring stale work"
        );
    }

    #[test]
    fn ordinary_action_does_not_request_transport_family_cancellation() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = workspace_host_without_preview_workers("ordinary-action-preview-work");
        host.preview_service.seed_pending_preview_work_for_test();

        let pending = PendingUiActions::default();
        pending.push(Action::DeselectAll);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(
            host.preview_service.diagnostics().interactive_cancel_requests,
            0,
            "Window action routing must not infer Preview cancellation from an ordinary Action"
        );
    }

    #[test]
    fn loading_feedback_does_not_stop_transport_or_request_preview_refresh() {
        struct LoadingPreview;

        impl crate::app_ui::panels::ViewerPreviewSource for LoadingPreview {
            fn viewer_preview_for_state(
                &self,
                _state: &AppState,
            ) -> crate::app_ui::panels::ViewerPreviewState {
                crate::app_ui::panels::ViewerPreviewState::Loading
            }
        }

        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state(),
            AppUiPreferences::default(),
            temp_preferences_path("buffering-preview-free-refresh"),
        );
        host.app_state.borrow_mut().play().expect("play");
        {
            let state = host.app_state.borrow();
            host.root.refresh_playback_frame_from_app_state(&state, Some(&LoadingPreview));
        }
        let before_render_requests = host.preview_service.diagnostics().render_requests;

        let _ = host.sync_playback_feedback_from_viewer();

        assert!(host.app_state.borrow().is_playing());
        assert_eq!(host.playback_feedback, ViewerPlaybackFeedback::Loading);
        assert_eq!(
            host.preview_service.diagnostics().render_requests,
            before_render_requests,
            "buffering control sync must not synchronously request preview"
        );
        assert!(
            !host.ui_dirty.get(),
            "buffering control sync should not leave a full preview-backed refresh queued"
        );
    }

    #[test]
    fn loading_feedback_defers_duplicate_gpu_prepare_without_holding_clock() {
        struct LoadingPreview;

        impl crate::app_ui::panels::ViewerPreviewSource for LoadingPreview {
            fn viewer_preview_for_state(
                &self,
                _state: &AppState,
            ) -> crate::app_ui::panels::ViewerPreviewState {
                crate::app_ui::panels::ViewerPreviewState::Loading
            }
        }

        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state(),
            AppUiPreferences::default(),
            temp_preferences_path("buffering-defer-gpu-prepare"),
        );
        host.app_state.borrow_mut().play().expect("play");
        {
            let state = host.app_state.borrow();
            assert!(host.root.refresh_playback_frame_from_app_state(&state, Some(&LoadingPreview)));
        }
        assert!(host.sync_playback_feedback_from_viewer());

        assert!(host.app_state.borrow().is_playing());
        assert!(
            host.should_defer_gpu_preview_prepare_for_interaction(),
            "a redraw while already waiting must not synchronously re-enter GPU preview preparation"
        );

        let pending = PendingUiActions::default();
        pending.push(Action::TogglePlay);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(!host.should_defer_gpu_preview_prepare_for_interaction());
    }

    #[test]
    fn payload_free_ready_feedback_cannot_mint_presentation_authority() {
        struct TransparentPreview;

        impl crate::app_ui::panels::ViewerPreviewSource for TransparentPreview {
            fn viewer_preview_for_state(
                &self,
                _state: &AppState,
            ) -> crate::app_ui::panels::ViewerPreviewState {
                crate::app_ui::panels::ViewerPreviewState::Transparent
            }
        }

        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state(),
            AppUiPreferences::default(),
            temp_preferences_path("late-ready-feedback"),
        );
        host.app_state.borrow_mut().set_playback_frame_running(0);
        let pending_identity = host
            .app_state
            .borrow()
            .pending_playback_frame_demand_identity()
            .expect("pending presentation demand");
        {
            let state = host.app_state.borrow();
            host.root
                .refresh_playback_frame_from_app_state(&state, Some(&TransparentPreview));
        }
        assert_eq!(
            host.root.viewer_playback_feedback(),
            ViewerPlaybackFeedback::Ready
        );

        let _ = host.sync_playback_feedback_from_viewer();

        assert_eq!(host.playback_feedback, ViewerPlaybackFeedback::Ready);
        let state = host.app_state.borrow();
        assert_eq!(
            state.pending_playback_frame_demand_identity(),
            Some(pending_identity),
            "Widget Ready feedback must not attach itself to the current demand"
        );
        let evidence = state.playback_evidence_report();
        assert_eq!(evidence.deliveries.ready, 0);
        assert_eq!(evidence.deliveries.late, 0);
        assert_eq!(evidence.deliveries.rejected, 0);
    }

    #[test]
    fn poll_background_tasks_expires_stalled_delivery_without_holding_transport() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let scheduler_clock = ManualPreviewSchedulerClock::default();
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state_with_timed_solid(),
            AppUiPreferences::default(),
            temp_preferences_path("buffering-stall-release"),
        );
        host.preview_service.shutdown();
        host.preview_service =
            WindowPreviewAdapter::new_without_workers_with_scheduler_clock_for_test(
                scheduler_clock.clone(),
            );
        host.preview_service
            .synchronize_transport_intent(host.app_state.borrow().preview_transport_intent());
        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);
        // Construction may leave unrelated startup services with one visible
        // completion. Drain that work before measuring the stall-release path.
        let _ = host.poll_background_tasks(bounds);
        {
            let mut state = host.app_state.borrow_mut();
            state.play().expect("play");
            // Establish a running demand rather than injecting against the
            // priming demand. A correct transparent/solid presentation may
            // consume priming before stalled media expiry, and the superseded
            // identity must not retain terminal-delivery authority.
            assert!(
                !state.observe_viewer_frame_delivery(mondrian_playback::FrameDeliveryKind::Ready)
            );
            assert!(state.observe_video_preroll(0, 0));
            assert!(
                state.advance_playback_clock(Duration::from_millis(100)).requires_refresh(),
                "timed fixture must advance to a fresh running demand"
            );
        }
        // The normal Play path synchronizes Preview to the new Playback Epoch
        // before current-frame work is admitted. This test injects Scheduler
        // work directly, so establish that same boundary through the typed
        // transport-intent seam.
        host.preview_service
            .synchronize_transport_intent(host.app_state.borrow().preview_transport_intent());
        let demand_identity = host
            .app_state
            .borrow()
            .pending_playback_frame_demand_identity()
            .expect("playing state has pending demand");
        host.preview_service
            .seed_pending_playback_current_preview_work_for_test(demand_identity);
        let before_render_requests = host.preview_service.diagnostics().render_requests;
        assert_eq!(
            host.app_state.borrow().pending_playback_frame_demand_identity(),
            Some(demand_identity),
            "fixture must preserve terminal authority after Scheduler admission"
        );
        scheduler_clock
            .advance(crate::app::preview_runtime::playback_buffering_stall_timeout_for_test());

        assert!(host.poll_background_tasks(bounds).repaint_required);
        assert!(host.app_state.borrow().is_playing());
        assert!(!host.should_defer_gpu_preview_prepare_for_interaction());
        let diagnostics = host.preview_service.diagnostics();
        assert_eq!(diagnostics.playback_current_stalled_expirations, 1);
        assert_eq!(diagnostics.scheduler.pending_requests, 0);
        assert_eq!(diagnostics.worker_queue.queued_jobs, 0);
        assert_eq!(
            diagnostics.render_requests, before_render_requests,
            "stalled buffering release must not synchronously request preview"
        );
    }

    #[test]
    fn window_visual_terminal_consumes_the_exact_playback_demand_once() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let host = workspace_host_without_preview_workers("visual-terminal-exact-once");
        host.app_state.borrow_mut().play().expect("play");
        let identity = host
            .app_state
            .borrow()
            .pending_playback_frame_demand_identity()
            .expect("playing state has pending demand");
        let before = host.app_state.borrow().playback_evidence_report().deliveries.failed;
        let disposition = PreviewVisualGpuCompletionDisposition::TerminalCandidate(
            mondrian_playback::FrameDeliveryCandidate::for_demand(
                identity,
                mondrian_playback::FrameDeliveryKind::Failed,
            ),
        );

        host.observe_visual_gpu_disposition(disposition);
        assert_eq!(
            host.app_state.borrow().playback_evidence_report().deliveries.failed,
            before + 1
        );

        // A duplicate callback or queued copy no longer owns the pending
        // demand and therefore cannot increment terminal evidence twice.
        host.observe_visual_gpu_disposition(disposition);
        assert_eq!(
            host.app_state.borrow().playback_evidence_report().deliveries.failed,
            before + 1
        );
    }

    #[test]
    fn host_open_project_dialog_switches_to_workspace_and_records_recent_project() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let project_file = create_project_file("open-dialog");
        let preferences_path = temp_preferences_path("open-dialog-preferences");
        let platform = ProjectDialogPlatform {
            open_paths: Some(vec![project_file.clone()]),
            save_path: None,
        };
        let mut host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            AppUiPreferences::default(),
            preferences_path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_open_project_dialog_action());
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.mode(), AppUiMode::Workspace);
        assert_eq!(
            host.app_state().current_project_path(),
            Some(project_file.as_path())
        );
        assert_eq!(
            host.preferences().recent_projects,
            vec![project_file.clone()]
        );
        assert_eq!(
            load_app_ui_preferences_from(&preferences_path).recent_projects,
            vec![project_file.clone()]
        );

        cleanup_project_file(&project_file);
        let _ = std::fs::remove_file(preferences_path);
    }

    #[test]
    fn host_open_recent_project_uses_startup_shell_action_boundary() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let project_file = create_project_file("open-recent");
        let mut preferences = AppUiPreferences::default();
        preferences.record_recent_project(project_file.clone());
        let preferences_path = temp_preferences_path("open-recent-preferences");
        let mut host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            preferences,
            preferences_path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(
            crate::app::ui_actions::app_shell_open_recent_project_action(
                crate::app::ui_actions::AppShellOpenRecentProjectPayload {
                    project_file: project_file.clone(),
                },
            ),
        );
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.mode(), AppUiMode::Workspace);
        assert_eq!(
            host.app_state().current_project_path(),
            Some(project_file.as_path())
        );
        assert_eq!(
            host.preferences().recent_projects,
            vec![project_file.clone()]
        );

        cleanup_project_file(&project_file);
        let _ = std::fs::remove_file(preferences_path);
    }

    #[test]
    fn host_save_project_action_clears_unsaved_fingerprint_delta() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(saved_workspace_app_state("save-project-action"));
        let project_file = host
            .app_state()
            .current_project_path()
            .map(std::path::Path::to_path_buf)
            .expect("project path");
        assert!(host.app_state().has_unsaved_project_changes());
        let pending = PendingUiActions::default();

        pending.push(Action::SaveProject);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().has_open_project());
        poll_host_background_tasks_until(&mut host, "manual project save", |state| {
            !state.has_unsaved_project_changes()
        });
        assert!(!host.app_state().has_unsaved_project_changes());

        cleanup_project_file(&project_file);
    }

    #[test]
    fn host_save_as_dialog_updates_project_path_and_recent_project() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let preferences_path = temp_preferences_path("save-as-preferences");
        let mut host = AppUiHost::new_with_preferences_path(
            saved_workspace_app_state("save-as-source"),
            AppUiPreferences::default(),
            preferences_path.clone(),
        );
        let source_file = host
            .app_state()
            .current_project_path()
            .map(std::path::Path::to_path_buf)
            .expect("source project path");
        let target_file = temp_preferences_path("save-as-target").with_extension("mdp");
        let platform = ProjectDialogPlatform {
            open_paths: None,
            save_path: Some(target_file.clone()),
        };
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_save_project_as_dialog_action());
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, AppUiShellCommands::default());
        poll_host_background_tasks_until(&mut host, "Save As publication", |state| {
            state.current_project_path() == Some(target_file.as_path())
        });
        assert_eq!(
            host.app_state().current_project_path(),
            Some(target_file.as_path())
        );
        assert!(target_file.exists());
        assert_eq!(
            host.preferences().recent_projects,
            vec![target_file.clone()]
        );

        cleanup_project_file(&source_file);
        cleanup_project_file(&target_file);
        let _ = std::fs::remove_file(preferences_path);
    }

    #[test]
    fn host_recovers_autosave_candidate_and_records_recent_project() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let project_file = create_project_file("recover-host");
        let mut autosave_state = AppState::new();
        autosave_state
            .open_project_file(project_file.clone())
            .expect("project should open for autosave");
        let sequence_id = autosave_state.active_sequence().expect("active sequence").id;
        autosave_state
            .rename_sequence(sequence_id, "Recovered Edit")
            .expect("rename through author transaction");
        let autosave_file = autosave_state
            .write_autosave_snapshot(2, 7)
            .expect("autosave snapshot should write");
        let candidate = discover_crash_recovery_candidates()
            .into_iter()
            .find(|candidate| candidate.autosave_file == autosave_file)
            .expect("exact recovery candidate");
        drop(autosave_state);
        let preferences_path = temp_preferences_path("recover-host-preferences");
        let mut host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            AppUiPreferences::default(),
            preferences_path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_recover_project_action(
            crate::app::ui_actions::ProjectRecoverFromAutosavePayload { candidate },
        ));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(
            host.mode(),
            AppUiMode::Workspace,
            "recovery status: {:?}",
            host.app_state().status_hint
        );
        assert_eq!(
            host.app_state().current_project_path(),
            Some(project_file.as_path())
        );
        assert_eq!(
            host.app_state().active_sequence().map(|sequence| sequence.name.as_str()),
            Some("Recovered Edit")
        );
        assert_eq!(
            host.preferences().recent_projects,
            vec![project_file.clone()]
        );

        cleanup_project_file(&project_file);
        let _ = std::fs::remove_file(preferences_path);
    }

    #[test]
    fn host_loads_startup_recent_projects_from_preferences() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let project_file = temp_preferences_path("startup-recent").with_extension("mdp");
        let mut preferences = AppUiPreferences::default();
        preferences.record_recent_project(project_file.clone());

        let host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            preferences,
            temp_preferences_path("startup-recent-preferences"),
        );

        assert_eq!(host.mode(), AppUiMode::Startup);
        assert_eq!(host.startup.recent_project_count(), 1);
    }

    #[test]
    fn recovery_candidates_map_to_startup_rows() {
        let project_file = PathBuf::from("E:/projects/recover.mdp");
        let autosave_file = PathBuf::from("E:/runtime/autosave/project.autosave.mdp");
        let candidates = vec![CrashRecoveryCandidate {
            project_id: ProjectId::new(),
            runtime_root: PathBuf::from("E:/runtime"),
            project_file: project_file.clone(),
            canonical_target: crate::app::RecoveryCanonicalTargetEvidence::Missing,
            autosave_file: autosave_file.clone(),
            author_generation: 7,
            asset_library_revision: 3,
            document_revision: 11,
            archive_sha256: "0".repeat(64),
            saved_at_unix_ms: 0,
            total_snapshots: 2,
        }];

        let rows = startup_recovery_projects_from_candidates(&candidates);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].candidate.project_file, project_file);
        assert_eq!(rows[0].candidate.autosave_file, autosave_file);
        assert_eq!(rows[0].title, "recover");
        assert!(rows[0].detail.contains("2 个恢复点"));
    }

    #[test]
    fn host_builds_root_from_persisted_workspace_preference() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            AppUiPreferences {
                version: 1,
                theme_preference: ThemePreference::Dark,
                workspace_preset: WorkspacePreset::Compositing,
                recent_projects: Vec::new(),
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: None,
                waveform_display: WaveformDisplay::BottomAligned,
                viewer_canvas_background: ViewerCanvasBackground::Checkerboard,
                audio_output_device: Default::default(),
            },
            temp_preferences_path("initial-workspace"),
        );

        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Compositing);
        assert!((host.root().dock().ratio() - 0.42).abs() < f32::EPSILON);
    }

    #[test]
    fn host_persists_and_applies_viewer_canvas_background_preference() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let path = temp_preferences_path("viewer-canvas-background");
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state(),
            AppUiPreferences::default(),
            path.clone(),
        );
        let pending = PendingUiActions::default();
        pending.push(
            crate::app::ui_actions::app_shell_preferences_viewer_background_changed_action(
                ViewerCanvasBackground::Black,
            ),
        );

        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(
            host.preferences().viewer_canvas_background,
            ViewerCanvasBackground::Black
        );
        assert_eq!(
            load_app_ui_preferences_from(&path).viewer_canvas_background,
            ViewerCanvasBackground::Black
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn host_builds_root_from_persisted_custom_workspace_layout() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let layout = AppUiWorkspaceLayout::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.37,
            first: Box::new(AppUiWorkspaceLayout::Panel {
                kind: PanelKind::Assets,
                active_index: 1,
                hidden_tabs: Vec::new(),
                tabs: vec![PanelKind::Assets, PanelKind::Effects],
            }),
            second: Box::new(AppUiWorkspaceLayout::Panel {
                kind: PanelKind::Viewer,
                active_index: 0,
                hidden_tabs: Vec::new(),
                tabs: vec![PanelKind::Viewer],
            }),
        };
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state(),
            AppUiPreferences {
                version: 1,
                theme_preference: ThemePreference::Dark,
                workspace_preset: WorkspacePreset::Custom,
                recent_projects: Vec::new(),
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: Some(layout.clone()),
                waveform_display: WaveformDisplay::BottomAligned,
                viewer_canvas_background: ViewerCanvasBackground::Checkerboard,
                audio_output_device: Default::default(),
            },
            temp_preferences_path("initial-custom-workspace"),
        );

        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(host.mode(), AppUiMode::Workspace);
        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Custom);
        assert!((host.root().dock().ratio() - 0.37).abs() < f32::EPSILON);
        assert_eq!(host.root().workspace_layout(), Some(layout));
    }

    #[test]
    fn host_drains_actions_and_refreshes_root() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(Action::DeselectAll);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.root().dock().ratio() > 0.0);
    }

    #[test]
    fn host_defers_dirty_refresh_while_shell_overlay_is_open() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);
        let mut host = AppUiHost::new(workspace_app_state());
        TreeWalker::layout(host.root_mut(), bounds);

        let menu_bounds = host.root().menu_bar_bounds_for_test();
        let menu_point = Point::new(menu_bounds.x + 16.0, menu_bounds.center().y);
        click_root(host.root_mut(), menu_point);
        assert!(
            widget_tree_has_transient_interaction(host.active_root()),
            "test setup should leave the File menu overlay open"
        );

        host.mark_dirty();
        host.refresh_if_dirty(bounds);

        assert!(
            widget_tree_has_transient_interaction(host.active_root()),
            "dirty model refresh should not rebuild away an active menu/search overlay"
        );
    }

    #[test]
    fn host_returns_window_commands_without_dispatching_to_app_state() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(Action::ToggleFullscreen);
        pending.push(crate::app::ui_actions::app_shell_quit_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(
            commands,
            AppUiShellCommands {
                quit: true,
                toggle_fullscreen: true,
                ..AppUiShellCommands::default()
            }
        );
        assert!(!host.app_state().has_open_project());
    }

    #[test]
    fn host_guards_unsaved_close_project_with_pending_modal() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(workspace_app_state());
        let pending = PendingUiActions::default();

        pending.push(Action::CloseProject);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().has_open_project());
        assert!(host.root.has_pending_close_dialog());
        assert_eq!(
            host.pending_close_action,
            Some(PendingCloseAction::CloseProject)
        );
    }

    #[test]
    fn host_guards_unsaved_quit_after_canceling_preview_work() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = workspace_host_without_preview_workers("quit-cancel-preview-work");
        host.preview_service.seed_pending_preview_work_for_test();
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_quit_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().has_open_project());
        assert!(host.root.has_pending_close_dialog());
        let diagnostics = host.preview_service.diagnostics();
        assert_eq!(diagnostics.interactive_cancel_requests, 1);
        assert_eq!(diagnostics.interactive_cancel_scheduler_requests, 1);
        assert_eq!(diagnostics.interactive_cancel_queued_jobs, 1);
        assert_eq!(diagnostics.queue_canceled_jobs, 1);
        assert_eq!(diagnostics.scheduler.pending_requests, 0);
        assert_eq!(diagnostics.worker_queue.queued_jobs, 0);
    }

    #[test]
    fn host_can_cancel_pending_close_project() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(workspace_app_state());
        let pending = PendingUiActions::default();

        pending.push(Action::CloseProject);
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        pending.push(crate::app::ui_actions::app_shell_pending_close_cancel_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().has_open_project());
        assert!(!host.root.has_pending_close_dialog());
        assert_eq!(host.pending_close_action, None);
    }

    #[test]
    fn host_can_discard_pending_close_project() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(workspace_app_state());
        let pending = PendingUiActions::default();

        pending.push(Action::CloseProject);
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        pending.push(crate::app::ui_actions::app_shell_pending_close_discard_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().has_open_project());
        assert_eq!(
            host.quiescing_close_action,
            Some(PendingCloseAction::CloseProject)
        );
        assert!(!poll_host_until_project_close_finishes(&mut host));
        assert!(!host.app_state().has_open_project());
        assert!(!host.root.has_pending_close_dialog());
    }

    #[test]
    fn host_can_save_and_continue_pending_close_project() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(saved_workspace_app_state("save-close"));
        let pending = PendingUiActions::default();

        pending.push(Action::CloseProject);
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        assert!(host.root.has_pending_close_dialog());
        pending.push(crate::app::ui_actions::app_shell_pending_close_save_continue_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().has_open_project());
        assert_eq!(
            host.quiescing_close_action,
            Some(PendingCloseAction::CloseProject)
        );
        assert!(!poll_host_until_project_close_finishes(&mut host));
        assert!(!host.app_state().has_open_project());
        assert!(!host.root.has_pending_close_dialog());
    }

    #[test]
    fn host_does_not_quit_when_project_persistence_cannot_quiesce() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut state = saved_workspace_app_state("quit-quiescence-failure");
        state.save_project_file().expect("establish clean baseline");
        let project_file =
            state.authoring.as_ref().expect("open project").project_file().to_path_buf();
        state.test_poison_project_persistence_admission();
        let mut host = AppUiHost::new(state);
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_quit_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert!(!commands.quit);
        assert!(host.app_state().has_open_project());
        assert!(host.app_state().status_hint.as_ref().is_some_and(|(_, is_error)| *is_error));
        drop(host);
        cleanup_project_file(&project_file);
    }

    #[test]
    fn host_requires_explicit_discard_before_quitting_a_faulted_handoff() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut state = saved_workspace_app_state("quit-fault-force-discard");
        state.save_project_file().expect("establish clean baseline");
        let project_file =
            state.authoring.as_ref().expect("open project").project_file().to_path_buf();
        let mut host = AppUiHost::new(state);
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_quit_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(
            host.quiescing_close_action,
            Some(PendingCloseAction::QuitApp)
        );
        host.app_state.borrow_mut().test_poison_project_persistence_admission();
        assert!(!poll_host_until_project_close_finishes(&mut host));
        assert!(host.app_state().has_open_project());
        assert!(host.root.has_pending_close_dialog());
        assert_eq!(host.pending_close_action, Some(PendingCloseAction::QuitApp));

        pending.push(crate::app::ui_actions::app_shell_pending_close_discard_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        assert_eq!(
            commands,
            AppUiShellCommands { quit: true, ..AppUiShellCommands::default() }
        );
        assert!(!host.app_state().has_open_project());
        cleanup_project_file(&project_file);
    }

    #[test]
    fn host_guards_unsaved_quit_until_discarded() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(workspace_app_state());
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_quit_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().has_open_project());
        assert!(host.root.has_pending_close_dialog());

        pending.push(crate::app::ui_actions::app_shell_pending_close_discard_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().has_open_project());
        assert_eq!(
            host.quiescing_close_action,
            Some(PendingCloseAction::QuitApp)
        );
        assert!(poll_host_until_project_close_finishes(&mut host));
        assert!(!host.app_state().has_open_project());
    }

    #[test]
    fn host_returns_custom_chrome_window_commands() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_window_minimize_action());
        pending.push(crate::app::ui_actions::app_shell_window_toggle_maximize_action());
        pending.push(crate::app::ui_actions::app_shell_window_drag_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(
            commands,
            AppUiShellCommands {
                minimize: true,
                toggle_maximize: true,
                begin_window_drag: true,
                ..AppUiShellCommands::default()
            }
        );
        assert!(!host.app_state().has_open_project());
    }

    #[test]
    fn host_applies_and_persists_theme_preference_updates() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let path = temp_preferences_path("theme-preferences");
        let mut host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            AppUiPreferences::default(),
            path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(
            crate::app::ui_actions::app_shell_preferences_theme_changed_action(
                ThemePreference::Light,
            ),
        );
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.preferences().theme_preference, ThemePreference::Light);
        assert_eq!(
            load_app_ui_preferences_from(&path).theme_preference,
            ThemePreference::Light
        );

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn host_persists_specific_audio_device_intent_without_authoring_state() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let path = temp_preferences_path("audio-output-device-preferences");
        let mut host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            AppUiPreferences::default(),
            path.clone(),
        );
        let device_id =
            mondrian_media::RealtimeAudioOutputDeviceId::new("wasapi:host-preference-device")
                .expect("fixture device identity");
        let selection = mondrian_media::RealtimeAudioOutputDeviceSelection::Specific { device_id };
        let pending = PendingUiActions::default();
        pending.push(
            crate::app::ui_actions::app_shell_preferences_audio_output_device_changed_action(
                selection.clone(),
            ),
        );

        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.preferences().audio_output_device, selection);
        assert_eq!(
            load_app_ui_preferences_from(&path).audio_output_device,
            selection
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn host_resolves_system_theme_preference_from_desktop_theme() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            AppUiPreferences::default(),
            temp_preferences_path("system-theme-preferences"),
        );

        assert_eq!(host.preferences().theme_preference, ThemePreference::System);
        assert_eq!(current_theme().name, "Dark");

        assert!(host.set_system_theme_preset(ThemePreset::Light));
        assert_eq!(current_theme().name, "Light");
        assert!(host.set_system_theme_preset(ThemePreset::Dark));
        assert_eq!(current_theme().name, "Dark");

        let explicit = AppUiPreferences {
            theme_preference: ThemePreference::Dark,
            ..Default::default()
        };
        let mut explicit_host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            explicit,
            temp_preferences_path("explicit-theme-preferences"),
        );

        assert!(!explicit_host.set_system_theme_preset(ThemePreset::Light));
        assert_eq!(current_theme().name, "Dark");
    }

    #[test]
    fn host_applies_and_persists_shortcut_preference_updates() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let path = temp_preferences_path("shortcut-preferences");
        let mut preferences = AppUiPreferences::default();
        preferences
            .shortcut_overrides
            .push(AppUiShortcutOverride { id: "panel.inspector".to_owned(), binding: None });
        let mut host =
            AppUiHost::new_with_preferences_path(AppState::new(), preferences, path.clone());
        let pending = PendingUiActions::default();

        pending.push(
            crate::app::ui_actions::app_shell_preferences_shortcut_disabled_action(
                "file.save_project",
            ),
        );
        pending.push(
            crate::app::ui_actions::app_shell_preferences_shortcut_reset_action("panel.inspector"),
        );
        pending.push(
            crate::app::ui_actions::app_shell_preferences_shortcut_rebound_action(
                crate::app::ui_actions::PreferencesShortcutReboundPayload {
                    id: "file.save_project".to_owned(),
                    key: "I".to_owned(),
                    ctrl: true,
                    alt: true,
                    shift: false,
                    meta: false,
                },
            ),
        );
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(
            host.preferences().shortcut_overrides,
            vec![
                AppUiShortcutOverride { id: "panel.inspector".to_owned(), binding: None },
                AppUiShortcutOverride {
                    id: "file.save_project".to_owned(),
                    binding: Some(AppUiShortcutBinding {
                        key: AppUiShortcutKey::I,
                        ctrl: true,
                        alt: true,
                        shift: false,
                        meta: false,
                    }),
                },
            ]
        );
        assert_eq!(
            load_app_ui_preferences_from(&path).shortcut_overrides,
            host.preferences().shortcut_overrides
        );

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn host_persists_workspace_preference_updates() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let path = temp_preferences_path("workspace-preferences");
        let mut host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            AppUiPreferences {
                version: 1,
                theme_preference: ThemePreference::Dark,
                workspace_preset: WorkspacePreset::Editing,
                recent_projects: Vec::new(),
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: None,
                waveform_display: WaveformDisplay::BottomAligned,
                viewer_canvas_background: ViewerCanvasBackground::Checkerboard,
                audio_output_device: Default::default(),
            },
            path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(Action::SwitchWorkspace(WorkspacePreset::Export));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Export);
        assert_eq!(host.preferences().workspace_preset, WorkspacePreset::Export);
        let loaded = load_app_ui_preferences_from(&path);
        assert_eq!(loaded.workspace_preset, WorkspacePreset::Export);
        assert_eq!(loaded.theme_preference, ThemePreference::Dark);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn host_promotes_dragged_builtin_workspace_to_persisted_custom_layout() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let path = temp_preferences_path("workspace-custom-layout");
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state(),
            AppUiPreferences::default(),
            path.clone(),
        );
        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Editing);

        drag_first_splitter_to(host.root_mut(), 0.78);
        let dragged_ratio = host.root().dock().ratio();
        host.sync_workspace_layout_from_root();

        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Custom);
        assert_eq!(host.preferences().workspace_preset, WorkspacePreset::Custom);
        assert!(host.preferences().custom_workspace_layout.is_some());
        assert!((host.root().dock().ratio() - dragged_ratio).abs() < f32::EPSILON);
        let loaded = load_app_ui_preferences_from(&path);
        assert_eq!(loaded.workspace_preset, WorkspacePreset::Custom);
        assert_eq!(
            loaded.custom_workspace_layout,
            host.preferences().custom_workspace_layout
        );

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn host_persists_toggle_panel_hidden_custom_layout() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let path = temp_preferences_path("workspace-toggle-layout");
        let mut host = AppUiHost::new_with_preferences_path(
            workspace_app_state(),
            AppUiPreferences::default(),
            path.clone(),
        );
        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));
        let pending = PendingUiActions::default();

        pending.push(Action::TogglePanel(PanelKind::Inspector));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Custom);
        let layout = host.preferences().custom_workspace_layout.as_ref().expect("layout");
        assert!(!layout.contains_panel(PanelKind::Inspector));
        assert!(layout.contains_panel(PanelKind::Viewer));
        let loaded = load_app_ui_preferences_from(&path);
        assert_eq!(loaded.workspace_preset, WorkspacePreset::Custom);
        assert_eq!(
            loaded.custom_workspace_layout,
            host.preferences().custom_workspace_layout
        );

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn host_persists_workspace_changes_from_panel_focus_fallback() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let path = temp_preferences_path("workspace-focus-preferences");
        let mut host = AppUiHost::new_with_preferences_path(
            AppState::new(),
            AppUiPreferences {
                version: 1,
                theme_preference: ThemePreference::Dark,
                workspace_preset: WorkspacePreset::Editing,
                recent_projects: Vec::new(),
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: None,
                waveform_display: WaveformDisplay::BottomAligned,
                viewer_canvas_background: ViewerCanvasBackground::Checkerboard,
                audio_output_device: Default::default(),
            },
            path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(Action::FocusPanel(PanelKind::Export));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Export);
        assert_eq!(host.preferences().workspace_preset, WorkspacePreset::Export);
        assert_eq!(
            load_app_ui_preferences_from(&path).workspace_preset,
            WorkspacePreset::Export
        );

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn host_reports_unknown_app_shell_actions_as_status_errors() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(Action::Custom {
            namespace: crate::app::ui_actions::APP_SHELL_NAMESPACE.into(),
            name: "missing_command".into(),
            payload: serde_json::Value::Null,
        });
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(
            host.app_state().status_hint.as_ref().is_some_and(|(message, is_error)| {
                *is_error && message.contains("missing_command")
            })
        );
    }

    #[test]
    fn host_ignores_unavailable_editor_actions_before_dispatch() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(Action::ImportMedia(vec![PathBuf::from("E:/media/a.mov")]));
        pending.push(Action::Undo);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().status_hint.is_none());
        assert!(!host.app_state().can_undo_action());
    }

    #[test]
    fn host_ignores_unavailable_typed_timeline_actions_before_dispatch() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(workspace_app_state());
        let could_undo_before = host.app_state().can_undo_action();
        let time_base = host.app_state().active_sequence().expect("sequence").time_base();
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::timeline_move_clip_action(
            crate::app::ui_actions::TimelineMoveClipPayload {
                target_track_id: TrackId::new(),
                clip_id: ClipId::new(),
                position: FramePosition::new(12, time_base),
            },
        ));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().status_hint.is_none());
        assert_eq!(host.app_state().can_undo_action(), could_undo_before);
    }

    #[test]
    fn host_ignores_unavailable_app_shell_dialogs_before_platform_access() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(AppState::new());
        let pending = PendingUiActions::default();
        let platform = CountingPlatform::default();

        pending.push(crate::app::ui_actions::app_shell_import_media_dialog_action());
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(platform.open_file_dialog_calls.load(Ordering::Relaxed), 0);
        assert!(host.app_state().status_hint.is_none());
    }

    #[test]
    fn host_import_media_dialog_places_selected_files_in_target_asset_folder() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let library_root = temp_asset_library_dir("asset-dialog-import-library");
        let media_root = temp_asset_library_dir("asset-dialog-import-media");
        std::fs::create_dir_all(&media_root).expect("media root");
        let media_path = media_root.join("dialog-tone.wav");
        write_minimal_wav(&media_path);
        let library = AssetLibrary::open(library_root.clone()).expect("open asset library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        let mut state = workspace_app_state();
        state.test_set_asset_library(Some(library));
        let mut host = AppUiHost::new(state);
        let pending = PendingUiActions::default();
        let platform = ProjectDialogPlatform {
            open_paths: Some(vec![media_path.clone()]),
            save_path: None,
        };

        pending.push(
            crate::app::ui_actions::app_shell_import_media_dialog_action_with_target(
                crate::app::ui_actions::ImportMediaDialogPayload {
                    folder_id: Some(folder_id.clone()),
                },
            ),
        );
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.app_state().pending_media_import_batches(), 1);
        assert!(host
            .app_state()
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("正在导入")));

        poll_host_background_tasks_until_imports_idle(&mut host);

        let assets = host
            .app_state()
            .asset_library()
            .expect("library")
            .list_assets()
            .expect("list assets");
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].name, "dialog-tone.wav");
        assert_eq!(assets[0].folder_id.as_deref(), Some(folder_id.as_str()));
        assert!(host
            .app_state()
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("已导入 1")));

        let _ = std::fs::remove_dir_all(library_root);
        let _ = std::fs::remove_dir_all(media_root);
    }

    #[test]
    fn host_background_poll_commits_prepared_asset_relink() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let library_root = temp_asset_library_dir("asset-relink-poll-library");
        let media_root = temp_asset_library_dir("asset-relink-poll-media");
        std::fs::create_dir_all(&media_root).expect("media root");
        let original_path = media_root.join("original.wav");
        let replacement_path = media_root.join("replacement.wav");
        write_minimal_wav(&original_path);
        write_minimal_wav(&replacement_path);
        let library = AssetLibrary::open(library_root.clone()).expect("open asset library");
        let mut state = workspace_app_state();
        state.test_set_asset_library(Some(library));
        state
            .start_media_import_batch(vec![original_path], None)
            .expect("admit original import");
        let mut host = AppUiHost::new(state);
        poll_host_background_tasks_until_imports_idle(&mut host);
        let asset_id = host
            .app_state()
            .asset_library()
            .expect("library")
            .list_assets()
            .expect("list assets")[0]
            .id;

        host.app_state
            .borrow_mut()
            .dispatch_action(crate::app::ui_actions::assets_relink_asset_action(
                crate::app::ui_actions::AssetsRelinkAssetPayload {
                    asset_id,
                    path: replacement_path.clone(),
                },
            ))
            .expect("admit relink");
        assert_eq!(
            host.app_state().media_asset_mutation_diagnostics().outstanding,
            1
        );

        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut terminal_poll_requested_refresh = false;
        while host.app_state().media_asset_mutation_diagnostics().outstanding > 0 {
            let outcome = host.poll_background_tasks(bounds);
            if host.app_state().media_asset_mutation_diagnostics().outstanding == 0 {
                terminal_poll_requested_refresh = outcome.repaint_required;
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for host Asset relink completion"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(terminal_poll_requested_refresh);
        let asset = host
            .app_state()
            .asset_library()
            .expect("library")
            .get_asset(asset_id)
            .expect("query asset")
            .expect("asset");
        let stored_path = asset.file_path().expect("file-backed Asset path");
        assert_eq!(
            stored_path.canonicalize().expect("canonical stored path"),
            replacement_path.canonicalize().expect("canonical replacement path")
        );
        let diagnostics = host.app_state().media_asset_mutation_diagnostics();
        assert_eq!(
            diagnostics.terminals.last().expect("terminal").evidence.disposition,
            mondrian_core::ExecutionTerminalDisposition::Completed
        );

        let _ = std::fs::remove_dir_all(library_root);
        let _ = std::fs::remove_dir_all(media_root);
    }

    #[test]
    fn host_handles_asset_folder_navigation_as_shell_local_state() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let root = temp_asset_library_dir("asset-folder-navigation");
        let library = AssetLibrary::open(root.clone()).expect("open asset library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        let mut state = AppState::new();
        state.test_set_asset_library(Some(library));
        let mut host = AppUiHost::new(state);
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::assets_open_folder_action(
            crate::app::ui_actions::AssetsOpenFolderPayload { folder_id: Some(folder_id.clone()) },
        ));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.root().asset_folder_id(), Some(folder_id.as_str()));
        assert!(host.app_state().status_hint.is_none());

        pending.push(crate::app::ui_actions::assets_open_folder_action(
            crate::app::ui_actions::AssetsOpenFolderPayload { folder_id: None },
        ));
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(host.root().asset_folder_id(), None);
        assert!(host.app_state().status_hint.is_none());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn host_refresh_preserves_valid_asset_folder_and_normalizes_deleted_folder() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let root = temp_asset_library_dir("asset-folder-refresh-normalize");
        let library = AssetLibrary::open(root.clone()).expect("open asset library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        let mut state = workspace_app_state();
        state.test_set_asset_library(Some(library));
        let mut host = AppUiHost::new(state);
        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);

        host.root_mut().set_asset_folder_id(Some(folder_id.clone()));
        host.mark_dirty();
        host.refresh_if_dirty(bounds);
        assert_eq!(host.root().asset_folder_id(), Some(folder_id.as_str()));

        host.app_state()
            .asset_library()
            .expect("library")
            .delete_folder(&folder_id)
            .expect("delete folder");
        host.mark_dirty();
        host.refresh_if_dirty(bounds);

        assert_eq!(host.root().asset_folder_id(), None);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn host_clears_deleted_asset_folder_selection_after_dispatch() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let root = temp_asset_library_dir("asset-folder-delete-normalize");
        let library = AssetLibrary::open(root.clone()).expect("open asset library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        let mut state = AppState::new();
        state.test_set_asset_library(Some(library));
        let mut host = AppUiHost::new(state);
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::assets_open_folder_action(
            crate::app::ui_actions::AssetsOpenFolderPayload { folder_id: Some(folder_id.clone()) },
        ));
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        assert_eq!(host.root().asset_folder_id(), Some(folder_id.as_str()));

        pending.push(crate::app::ui_actions::assets_delete_folder_action(
            crate::app::ui_actions::AssetsDeleteFolderPayload { folder_id: folder_id.clone() },
        ));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, AppUiShellCommands::default());
        assert_eq!(host.root().asset_folder_id(), None);
        assert!(host
            .app_state()
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("Rushes")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn host_ignores_disabled_editor_action_without_false_failure() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let mut host = AppUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::assets_prepare_drag_action(
            crate::app::ui_actions::AssetsPrepareDragPayload { asset_id: AssetId::new() },
        ));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        assert_eq!(commands, AppUiShellCommands::default());
        assert!(host.app_state().status_hint.is_none());

        assert!(
            !host.ui_dirty.get(),
            "failed editor actions should refresh the root immediately"
        );
    }
}
