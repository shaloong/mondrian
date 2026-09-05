#![allow(deprecated)]
//! Mondrian app UI winit/wgpu product window.
//!
//! Binary entrypoints stay thin and call this module. The product shell owns
//! native event-loop wiring, renderer setup, shell command application, and the
//! bridge between widget-dispatched actions and `AppState`.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(feature = "validation")]
use std::sync::Mutex;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
#[cfg(feature = "validation")]
use std::{cell::RefCell, rc::Rc};

#[cfg(feature = "validation")]
use crate::app::endurance_recovery::EnduranceRecoveryOperationReceipt;
use crate::app::preview_execution::{
    PreviewGpuFrame, PreviewGpuFrameStaging, PreviewGpuFrameState,
    PreviewGpuHeterogeneousExecution, PreviewGpuWorkingInput,
};
use crate::app::preview_gpu_output_blocker::{
    PreviewGpuOutputBlocker, PreviewGpuOutputBlockerBreakdown,
};
use crate::app::preview_runtime::{PreviewColorRejection, PreviewVisualGpuCompletionDisposition};
use crate::app::preview_work_notification::{PreviewWorkRevision, PreviewWorkWatch};
use crate::app::ui_actions::app_shell_quit_action;
#[cfg(feature = "validation")]
use crate::app::viewer_gpu_device_progress::ViewerGpuDeviceGenerationTerminalKind;
use crate::app::viewer_gpu_device_progress::{
    ViewerGpuDeviceGenerationId, ViewerGpuDeviceGenerationMember,
    ViewerGpuDeviceGenerationRetirement, ViewerGpuDeviceGenerationTerminal,
    ViewerGpuDeviceProgressObservation, ViewerGpuDeviceProgressOwner,
    ViewerGpuDeviceProgressReserveError, ViewerGpuDeviceProgressShutdownEvidence,
    ViewerGpuDeviceProgressWake,
};
use crate::app::viewer_gpu_output_health::{
    classify_viewer_gpu_output_health,
    ViewerGpuOutputAttemptOutcome as AppUiViewerGpuOutputOutcome,
    ViewerGpuOutputHealthCounts as AppUiViewerGpuOutputHealthCounts,
    ViewerGpuOutputHealthStatus as AppUiViewerGpuOutputHealthStatus,
    ViewerGpuOutputHealthSummary as AppUiViewerGpuOutputHealthSummary,
};
use crate::app::viewer_gpu_output_residency::{
    declared_viewer_gpu_output_residency,
    executed_viewer_gpu_output_residency as preview_gpu_composite_frame_residency,
    ViewerGpuOutputFrameResidency as AppUiViewerGpuOutputFrameResidency,
};
use crate::app::viewer_gpu_publication::{ViewerGpuPhysicalPublication, ViewerGpuPublicationSlots};
use crate::app::viewer_gpu_submission::{
    ViewerGpuCompletedSubmission, ViewerGpuSubmissionAdmissionError, ViewerGpuSubmissionId,
    ViewerGpuSubmissionLifecycle, ViewerGpuSubmissionPoll, ViewerGpuSubmissionQuarantine,
    ViewerGpuSubmissionQuarantineReason,
};
#[cfg(feature = "validation")]
use crate::app::AppEnduranceShutdownEvidence;
use crate::app::{AppState, FramePresentationDisposition};
use crate::app_ui::action_queue::PendingUiActions;
use crate::app_ui::background_runtime::AppUiBackgroundRuntimeOwner;
#[cfg(feature = "validation")]
pub use crate::app_ui::event_loop_owner::{
    AppUiEventLoopConstructionFailure, AppUiEventLoopConstructionFailureKind,
    AppUiEventLoopShutdownEvidence, AppUiEventLoopShutdownReceipt,
    AppUiEventLoopShutdownReceiptError,
};
use crate::app_ui::host::AppUiServiceShutdownEvidence;
use crate::app_ui::host::{
    AppUiBackgroundTaskPollOutcome, AppUiHost, AppUiMode, AppUiShellCommands,
};
use crate::app_ui::product_logging::init_product_tracing;
#[cfg(test)]
use crate::app_ui::product_logging::DEFAULT_APP_UI_LOG_FILTER;
#[cfg(not(test))]
use crate::app_ui::product_logging::FORCED_PROCESS_EXIT_CODE;
use crate::app_ui::rendering::{
    AppUiBackendEvent, AppUiFrameMetrics, AppUiFramePressure, AppUiFrameRenderer,
    AppUiRenderDiagnosticReporter,
};
use crate::app_ui::runtime::{
    winit_cursor_icon_for_ui_state, winit_modifiers_to_ui_modifiers,
    winit_mouse_button_to_ui_button, winit_scroll_delta_to_ui_delta, WinitUiRuntime,
};
use crate::app_ui::shortcuts::{register_shortcuts, AppUiShortcutOverride};
use crate::app_ui::startup::{STARTUP_WINDOW_HEIGHT, STARTUP_WINDOW_WIDTH};
#[cfg(feature = "validation")]
use crate::app_ui::window_outer_receipt::AppUiWindowNativeReturnEvidence;
#[cfg(feature = "validation")]
use crate::app_ui::window_outer_receipt::AppUiWindowOuterShutdownEvidence;
#[cfg(feature = "validation")]
pub use crate::app_ui::window_outer_receipt::{
    AppUiWindowClosedEvidence, AppUiWindowClosedOutcome, AppUiWindowClosedReceipt,
    AppUiWindowClosedReceiptError, AppUiWindowRunReceipt, AppUiWindowRunReceiptError,
};
use mondrian_core::types::ColorSpace;
use mondrian_core::{ProgramScopeScale, ProgramScopesTap, SignalComplianceContract, WaveformMode};
use mondrian_editor_state::state::PanelKind;
use mondrian_platform::SystemPlatformService;
#[cfg(test)]
use mondrian_renderer::color::ProgramOutputBoundary;
use mondrian_renderer::{
    color::{
        ProgramOutputRole, RenderColorStageDiagnostics, RenderGpuOutputBoundaryRuntimeDiagnostics,
        RenderGpuOutputRuntimeDiagnosticsReport, RenderGpuOutputStageDiagnosticsReport,
    },
    native_video_texture_device_features, ocio_lut_filtering_device_features,
    request_adapter_with_native_video_preference, GpuProgramScopesRequest, GpuSignalMonitorRequest,
    ViewerGpuExecutionError, ViewerGpuExecutionRequest, ViewerGpuExecutionRuntime,
    ViewerGpuOutputPrecision, ViewerGpuPresentationOutputLease,
    ViewerHeterogeneousGpuCompletedBatch, ViewerSourceRect,
};
use mondrian_ui_core::focus::FocusManager;
use mondrian_ui_core::shortcut::{ShortcutManager, ShortcutScope};
use mondrian_ui_core::types::*;
use mondrian_ui_core::TreeWalker;
use mondrian_ui_events::EventRouter;
use mondrian_ui_renderer::{command::DrawEncoder, ExternalTextureKey, ExternalTextureTransfer};
use mondrian_ui_theme::ThemePreset;
use mondrian_ui_tooltip::TooltipManagerImpl;
use mondrian_ui_widgets::{VideoScopesSettings, ViewerExternalTexturePresentation};
use sha2::{Digest, Sha256};

const APP_UI_WINDOW_PRODUCT_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(750);

fn control_flow_wake_no_later_than(
    current: winit::event_loop::ControlFlow,
    deadline: Instant,
) -> winit::event_loop::ControlFlow {
    use winit::event_loop::ControlFlow;

    match current {
        ControlFlow::Poll => ControlFlow::Poll,
        ControlFlow::Wait => ControlFlow::WaitUntil(deadline),
        ControlFlow::WaitUntil(existing) => ControlFlow::WaitUntil(existing.min(deadline)),
    }
}

fn queue_preview_work_event(pending: &AtomicBool, send: impl FnOnce() -> bool) -> bool {
    if pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    let mut reset = PendingEventResetOnDrop::new(pending);
    if !send() {
        return false;
    }
    reset.disarm();
    true
}

struct PendingEventResetOnDrop<'a> {
    pending: &'a AtomicBool,
    armed: bool,
}

impl<'a> PendingEventResetOnDrop<'a> {
    const fn new(pending: &'a AtomicBool) -> Self {
        Self { pending, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingEventResetOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.pending.store(false, Ordering::Release);
        }
    }
}

fn rearm_preview_work_event(
    pending: &AtomicBool,
    drain_target_revision: PreviewWorkRevision,
    watch: &PreviewWorkWatch,
    send: impl FnOnce() -> bool,
) -> bool {
    // Publishers retain the pending bit throughout the bounded drain, so a
    // completion burst cannot enqueue one native event per result. Clear only
    // after sampling what that drain could have observed.
    pending.store(false, Ordering::Release);
    if watch.revision() != drain_target_revision {
        return queue_preview_work_event(pending, send);
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerHeterogeneousCompletionPoll {
    Idle,
    Pending,
    TerminalChange,
}

// ═══════════════════════════════════════════════════════════════════════════
// Main
// ═══════════════════════════════════════════════════════════════════════════

const VIEWER_GPU_OUTPUT_DIAGNOSTICS_OUTPUT_ENV: &str = "MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT";
const VIEWER_QUALIFICATION_RUN_ID_ENV: &str = "MONDRIAN_VIEWER_QUALIFICATION_RUN_ID";
const WORKSPACE_WINDOW_WIDTH: f32 = 1600.0;
const WORKSPACE_WINDOW_HEIGHT: f32 = 900.0;
const WORKSPACE_MIN_WIDTH: f32 = 1024.0;
const WORKSPACE_MIN_HEIGHT: f32 = 600.0;
const APP_UI_DISPLAY_CONTRACT_REFRESH_HISTORY_LIMIT: usize = 8;
const APP_UI_EVENT_LOOP_SLOW_STAGE_BUDGET_US: u64 = 50_000;
const APP_UI_BUFFERING_INTERACTIVE_WAKE_DELAY: Duration = Duration::from_millis(16);
const VIEWER_HETEROGENEOUS_COMPLETION_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(feature = "validation")]
const MAXIMUM_SURFACE_REOPEN_VALIDATION_BATCH_CYCLES: usize = 24;
static NEXT_APP_UI_SURFACE_GENERATION_ID: AtomicU64 = AtomicU64::new(1);

/// Process-local identity of one concrete Window/Surface generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct AppUiSurfaceGenerationId(u64);

impl AppUiSurfaceGenerationId {
    fn next() -> Result<Self, AppUiSurfaceGenerationError> {
        NEXT_APP_UI_SURFACE_GENERATION_ID
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map(Self)
            .map_err(|_| AppUiSurfaceGenerationError::IdentityExhausted)
    }

    const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, thiserror::Error)]
enum AppUiSurfaceGenerationError {
    #[error("app UI Surface generation identity space is exhausted")]
    IdentityExhausted,
}

/// Owner-derived facts for one real Window/Surface and wgpu Device recovery.
///
/// Construction remains inside this Module because only the Window event-loop
/// owner can observe physical presentation and consume the retiring device.
#[cfg(feature = "validation")]
pub(crate) struct SurfaceDeviceReopenFacts {
    cycle_index: u32,
    operation_id: String,
    sequence_binding_sha256: String,
    surface_generation_before: u64,
    surface_generation_after: u64,
    device_generation_before: u64,
    device_generation_after: u64,
    shutdown_receipt_json: String,
    shutdown_receipt_sha256: String,
    reopened_contract_json: String,
    reopened_contract_sha256: String,
}

#[cfg(feature = "validation")]
impl SurfaceDeviceReopenFacts {
    pub(crate) const fn cycle_index(&self) -> u32 {
        self.cycle_index
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub(crate) fn sequence_binding_sha256(&self) -> &str {
        &self.sequence_binding_sha256
    }

    pub(crate) const fn surface_generation_before(&self) -> u64 {
        self.surface_generation_before
    }

    pub(crate) const fn surface_generation_after(&self) -> u64 {
        self.surface_generation_after
    }

    pub(crate) const fn device_generation_before(&self) -> u64 {
        self.device_generation_before
    }

    pub(crate) const fn device_generation_after(&self) -> u64 {
        self.device_generation_after
    }

    pub(crate) fn shutdown_receipt_sha256(&self) -> &str {
        &self.shutdown_receipt_sha256
    }

    pub(crate) fn shutdown_receipt_json(&self) -> &str {
        &self.shutdown_receipt_json
    }

    pub(crate) fn reopened_contract_sha256(&self) -> &str {
        &self.reopened_contract_sha256
    }

    pub(crate) fn reopened_contract_json(&self) -> &str {
        &self.reopened_contract_json
    }
}

#[cfg(feature = "validation")]
#[derive(Debug, Clone)]
struct AppUiSurfaceDeviceReopenRequest {
    cycle_index: u32,
    operation_id: String,
}

#[cfg(feature = "validation")]
struct AppUiSurfaceDeviceReopenValidation {
    state: AppUiSurfaceDeviceReopenValidationState,
    result: Arc<Mutex<Option<Result<EnduranceRecoveryOperationReceipt, String>>>>,
    deadline: Instant,
    recovery_pump: Option<crate::app::endurance_product_runtime::EnduranceSurfaceRecoveryPump>,
    recovery_pump_return:
        Rc<RefCell<Option<crate::app::endurance_product_runtime::EnduranceSurfaceRecoveryPump>>>,
}

#[cfg(feature = "validation")]
impl Drop for AppUiSurfaceDeviceReopenValidation {
    fn drop(&mut self) {
        *self.recovery_pump_return.borrow_mut() = self.recovery_pump.take();
    }
}

#[cfg(feature = "validation")]
enum AppUiValidationWindowShutdown {
    PendingHostActive {
        host: AppUiServiceShutdownEvidence,
        gpu: AppUiActiveWindowGpuShutdownEvidence,
    },
    PendingActive {
        runtime: crate::app_ui::background_runtime::AppUiBackgroundRuntimeShutdownEvidence,
        host: AppUiServiceShutdownEvidence,
        gpu: AppUiActiveWindowGpuShutdownEvidence,
    },
    PendingActivePublicationFailure {
        runtime: crate::app_ui::background_runtime::AppUiBackgroundRuntimeShutdownEvidence,
        host: AppUiServiceShutdownEvidence,
        gpu: AppUiActiveWindowGpuShutdownEvidence,
    },
    IncompleteHostFallback {
        host: AppUiServiceShutdownEvidence,
    },
    Complete(AppUiWindowOuterShutdownEvidence),
}

#[cfg(feature = "validation")]
struct AppUiValidationReturnedState {
    app_state: AppState,
    shutdown: AppUiValidationWindowShutdown,
}

#[cfg(feature = "validation")]
type AppUiValidationReturnSlot = Rc<RefCell<Option<AppUiValidationReturnedState>>>;

struct AppUiHostSessionOwner {
    host: Option<AppUiHost>,
    #[cfg(feature = "validation")]
    validation_return: Option<AppUiValidationReturnSlot>,
    #[cfg(feature = "validation")]
    validation_shutdown_deadline: Option<Instant>,
}

impl AppUiHostSessionOwner {
    fn new(
        host: AppUiHost,
        #[cfg(feature = "validation")] validation_return: Option<AppUiValidationReturnSlot>,
        #[cfg(feature = "validation")] validation_shutdown_deadline: Option<Instant>,
    ) -> Self {
        Self {
            host: Some(host),
            #[cfg(feature = "validation")]
            validation_return,
            #[cfg(feature = "validation")]
            validation_shutdown_deadline,
        }
    }

    fn shutdown_until(
        &mut self,
        deadline: Instant,
        gpu_shutdown: AppUiActiveWindowGpuShutdownEvidence,
    ) -> Result<AppUiServiceShutdownEvidence, String> {
        #[cfg(feature = "validation")]
        if let Some(return_slot) = self.validation_return.as_ref()
            && return_slot.borrow().is_some()
        {
            return Err(
                "validation Window return slot was already occupied before Host shutdown"
                    .to_owned(),
            );
        }
        let host = self
            .host
            .take()
            .ok_or_else(|| "Window Host was already consumed before shutdown".to_owned())?;
        let (app_state, ui_shutdown) = host.into_app_state_until(deadline);
        #[cfg(feature = "validation")]
        if let Some(return_slot) = self.validation_return.take() {
            *return_slot.borrow_mut() = Some(AppUiValidationReturnedState {
                app_state,
                shutdown: AppUiValidationWindowShutdown::PendingHostActive {
                    host: ui_shutdown,
                    gpu: gpu_shutdown,
                },
            });
            return Ok(ui_shutdown);
        }
        let _ = gpu_shutdown;
        drop(app_state);
        Ok(ui_shutdown)
    }
}

impl std::ops::Deref for AppUiHostSessionOwner {
    type Target = AppUiHost;

    fn deref(&self) -> &Self::Target {
        let Some(host) = self.host.as_ref() else {
            unreachable!("App UI host session was accessed after consuming return")
        };
        host
    }
}

impl std::ops::DerefMut for AppUiHostSessionOwner {
    fn deref_mut(&mut self) -> &mut Self::Target {
        let Some(host) = self.host.as_mut() else {
            unreachable!("App UI host session was accessed after consuming return")
        };
        host
    }
}

impl Drop for AppUiHostSessionOwner {
    fn drop(&mut self) {
        #[cfg(feature = "validation")]
        if let Some(return_slot) = self.validation_return.take() {
            let Some(host) = self.host.take() else {
                tracing::error!("validation Window lost its App UI host before state return");
                return;
            };
            let Some(deadline) = self.validation_shutdown_deadline.take() else {
                tracing::error!(
                    "validation Window Host fallback had no caller-owned shutdown deadline; retaining Host authority"
                );
                std::mem::forget(host);
                return;
            };
            let (app_state, ui_shutdown) = host.into_app_state_until(deadline);
            let mut returned = return_slot.borrow_mut();
            if returned.is_some() {
                tracing::error!(
                    "validation Window attempted to return AppState more than once; retaining duplicate authority"
                );
                std::mem::forget(app_state);
                return;
            }
            *returned = Some(AppUiValidationReturnedState {
                app_state,
                shutdown: AppUiValidationWindowShutdown::IncompleteHostFallback {
                    host: ui_shutdown,
                },
            });
        }
    }
}

#[cfg(feature = "validation")]
fn record_validation_active_runtime_shutdown(
    return_slot: &AppUiValidationReturnSlot,
    runtime: crate::app_ui::background_runtime::AppUiBackgroundRuntimeShutdownEvidence,
) -> Result<(), String> {
    let mut returned = return_slot.borrow_mut();
    let state = returned.as_mut().ok_or_else(|| {
        "validation Window returned no App owner before Runtime shutdown".to_owned()
    })?;
    let (host, gpu) = match &state.shutdown {
        AppUiValidationWindowShutdown::PendingHostActive { host, gpu } => (*host, gpu.clone()),
        AppUiValidationWindowShutdown::PendingActive { .. }
        | AppUiValidationWindowShutdown::PendingActivePublicationFailure { .. } => {
            return Err("validation Window Runtime shutdown was recorded twice".to_owned());
        }
        AppUiValidationWindowShutdown::IncompleteHostFallback { .. } => {
            return Err(
                "validation Window used fallback Host shutdown before Runtime closure".to_owned(),
            );
        }
        AppUiValidationWindowShutdown::Complete(_) => {
            return Err("validation Window shutdown evidence was completed twice".to_owned());
        }
    };
    state.shutdown = AppUiValidationWindowShutdown::PendingActive { runtime, host, gpu };
    Ok(())
}

#[cfg(feature = "validation")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct AppUiSurfacePictureContract {
    sequence_id: String,
    frame: i64,
    width: u32,
    height: u32,
    output_target: AppUiViewerGpuOutputTarget,
    output_color_space: ColorSpace,
    monitor_color_space: ColorSpace,
    tone_map: bool,
    display_view: Option<AppUiViewerGpuOutputDisplayView>,
    frame_residency: AppUiViewerGpuOutputFrameResidency,
    display_contract_sha256: String,
}

#[cfg(feature = "validation")]
#[derive(Debug)]
enum AppUiSurfaceDeviceReopenValidationState {
    AwaitingOriginalPicture(AppUiSurfaceDeviceReopenRequest),
    AwaitingReopenedPicture(Box<AppUiSurfaceDeviceReopenAwaitingPicture>),
    Complete,
}

#[cfg(feature = "validation")]
#[derive(Debug)]
struct AppUiSurfaceDeviceReopenAwaitingPicture {
    request: AppUiSurfaceDeviceReopenRequest,
    sequence_binding_sha256: String,
    original_picture: AppUiSurfacePictureContract,
    surface_generation_before: u64,
    surface_generation_after: u64,
    device_generation_before: u64,
    device_generation_after: u64,
    shutdown_receipt_json: String,
    shutdown_receipt_sha256: String,
}

#[cfg(feature = "validation")]
#[derive(Debug, serde::Serialize)]
struct AppUiViewerGpuShutdownContract {
    schema_version: u32,
    worker_started: bool,
    worker_terminated: bool,
    worker_panicked: bool,
    timed_out: bool,
    retirement_requested: bool,
    retirement_handoff_accepted: bool,
    retirement_completed: bool,
    renderer_retirement: Option<mondrian_renderer::ViewerGpuRetirementReceipt>,
    generation_terminal_kind: Option<&'static str>,
}

#[cfg(feature = "validation")]
#[derive(Debug, serde::Serialize)]
struct AppUiReopenedSurfaceContract<'a> {
    schema_version: u32,
    surface_generation: u64,
    device_generation: u64,
    actual_surface_presented: bool,
    original_picture_sha256: &'a str,
    reopened_picture_json: &'a str,
    reopened_picture_sha256: &'a str,
}

#[cfg(feature = "validation")]
fn canonical_json_and_sha256(
    value: &impl serde::Serialize,
) -> Result<(String, String), serde_json::Error> {
    let canonical_json = serde_json::to_string(value)?;
    let sha256 = Sha256::digest(canonical_json.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((canonical_json, sha256))
}

#[cfg(feature = "validation")]
fn exact_presented_surface_picture(
    host: &AppUiHost,
    session: &AppUiWindowSession,
    frame_result: crate::app_ui::rendering::AppUiFrameResult,
) -> Option<AppUiSurfacePictureContract> {
    let crate::app_ui::rendering::AppUiFrameResult::Presented { diagnostics, metrics, .. } =
        frame_result
    else {
        return None;
    };
    // UI-shell glyph/raster diagnostics are not evidence about the Viewer
    // picture. The qualification boundary is the external texture batch and
    // its concrete registration/presentation path.
    if diagnostics.external_texture_failures != 0
        || metrics.external_texture_batches == 0
        || metrics.external_texture_failures != 0
        || session.viewer_gpu_presentation.published_output().is_none()
        || !matches!(
            session.viewer_gpu_output_telemetry.health_summary().status,
            AppUiViewerGpuOutputHealthStatus::Ready | AppUiViewerGpuOutputHealthStatus::Degraded
        )
    {
        return None;
    }
    let context = session.viewer_gpu_output_telemetry.last_frame_context.as_ref()?;
    let state = host.app_state();
    let sequence = state.active_sequence()?;
    if context.sequence_id != sequence.id.to_string()
        || context.frame != state.current_frame()
        || context.output_target != AppUiViewerGpuOutputTarget::Display
        || context.preview_candidate_state != AppUiViewerGpuOutputPreviewCandidateState::Ready
        || !context.frame_residency.execution_observed
    {
        return None;
    }
    let display_contract_sha256 = session
        .display_snapshot
        .as_ref()?
        .contract_identity()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Some(AppUiSurfacePictureContract {
        sequence_id: context.sequence_id.clone(),
        frame: context.frame,
        width: context.width,
        height: context.height,
        output_target: context.output_target,
        output_color_space: context.output_color_space,
        monitor_color_space: context.monitor_color_space,
        tone_map: context.tone_map,
        display_view: context.display_view.clone(),
        frame_residency: context.frame_residency.clone(),
        display_contract_sha256,
    })
}

#[cfg(feature = "validation")]
fn seal_clean_viewer_gpu_shutdown(
    evidence: ViewerGpuDeviceProgressShutdownEvidence,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    if !evidence.qualifies_normal_runtime() {
        return Err(
            format!("Viewer GPU device generation did not retire cleanly: {evidence:?}").into(),
        );
    }
    canonical_json_and_sha256(&AppUiViewerGpuShutdownContract {
        schema_version: 2,
        worker_started: evidence.worker_started,
        worker_terminated: evidence.worker_terminated,
        worker_panicked: evidence.worker_panicked,
        timed_out: evidence.timed_out,
        retirement_requested: evidence.retirement_requested,
        retirement_handoff_accepted: evidence.retirement_handoff_accepted,
        retirement_completed: evidence.retirement_completed,
        renderer_retirement: evidence.renderer_retirement,
        generation_terminal_kind: evidence.generation_terminal_kind.map(|kind| match kind {
            ViewerGpuDeviceGenerationTerminalKind::DeviceLost => "device_lost",
            ViewerGpuDeviceGenerationTerminalKind::DeviceDestroyed => "device_destroyed",
            ViewerGpuDeviceGenerationTerminalKind::ProgressFailure => "progress_failure",
        }),
    })
    .map_err(Into::into)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppUiWindowRole {
    Startup,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppUiUserEvent {
    PreviewWorkAvailable,
    ViewerGpuCompletionAvailable,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct WindowChrome {
    title: &'static str,
    width: f32,
    height: f32,
    transparent: bool,
    decorations: bool,
    rounded_corners: bool,
    resizable: bool,
    min_size: Option<(f32, f32)>,
    max_size: Option<(f32, f32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowCornerPreference {
    Default,
    Round,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceLifecycleReason {
    Resize,
    ScaleFactorChanged,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AppUiViewerGpuOutputTelemetry {
    invocations: u64,
    non_workspace_skips: u64,
    current_skips: u64,
    loading_skips: u64,
    unavailable_skips: u64,
    invalid_texture_keys: u64,
    display_contract_blockers: u64,
    display_contract_hdr_surface_blockers: u64,
    display_contract_surface_color_space_blockers: u64,
    display_presentation_reconfigure_candidates: u64,
    display_presentation_payload_blockers: u64,
    display_presentation_unsupported_contracts: u64,
    display_contract_refreshes: u64,
    display_contract_refresh_generation: u64,
    prepare_attempts_timed: u64,
    accumulated_prepare_duration_us: u64,
    max_prepare_duration_us: u64,
    last_prepare_duration_us: Option<u64>,
    record_failures: u64,
    missing_output_textures: u64,
    registered_frames: u64,
    rejected_external_frames: u64,
    health_counts: AppUiViewerGpuOutputHealthCounts,
    accumulated_stage_diagnostics: RenderColorStageDiagnostics,
    last_stage_diagnostics: Option<RenderColorStageDiagnostics>,
    last_spatial_runtime: Option<mondrian_renderer::GpuViewerSpatialRuntimeDiagnostics>,
    last_compositor_uniform_arena: Option<mondrian_renderer::GpuCompositorUniformArenaDiagnostics>,
    last_compositor_texture_bindings:
        Option<mondrian_renderer::GpuCompositorTextureBindingDiagnostics>,
    last_compositor_creative_luts: Option<mondrian_renderer::GpuCreativeLutCacheDiagnostics>,
    last_frame_context: Option<AppUiViewerGpuOutputFrameContext>,
    last_preview_candidate_id: Option<u64>,
    last_preview_candidate_state: Option<AppUiViewerGpuOutputPreviewCandidateState>,
    last_display_contract_blocker: Option<AppUiDisplayBoundaryBlockerDiagnostics>,
    last_display_presentation_readiness: Option<AppUiDisplayPresentationReadinessDiagnostics>,
    last_display_issue_refresh_generation: Option<u64>,
    recent_display_contract_refreshes: Vec<AppUiDisplayContractRefreshEvent>,
    last_display_contract_refresh: Option<AppUiDisplayContractRefreshEvent>,
    last_outcome: Option<AppUiViewerGpuOutputOutcome>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
struct AppUiViewerGpuOutputDiagnostics {
    /// Supervisor-provided nonce shared by one non-spliceable Viewer run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    qualification_run_id: Option<String>,
    /// Process instance generated once at product startup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process_instance_id: Option<String>,
    /// OS process ID retained for acquisition correlation, not as identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process_id: Option<u32>,
    /// Strictly increasing record number inside the process instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    qualification_record_sequence: Option<u64>,
    /// SHA-256 of the currently executing product image when qualification
    /// diagnostics are enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_image_sha256: Option<String>,
    /// Exact active wgpu Adapter identity driving the Viewer surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    renderer_adapter: Option<AppUiRendererAdapterDiagnostics>,
    /// Exact active Window display target used by the native probes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_target: Option<AppUiDisplayTarget>,
    invocations: u64,
    non_workspace_skips: u64,
    current_skips: u64,
    loading_skips: u64,
    unavailable_skips: u64,
    invalid_texture_keys: u64,
    display_contract_blockers: u64,
    display_contract_hdr_surface_blockers: u64,
    display_contract_surface_color_space_blockers: u64,
    display_presentation_reconfigure_candidates: u64,
    display_presentation_payload_blockers: u64,
    display_presentation_unsupported_contracts: u64,
    display_contract_refreshes: u64,
    /// Actual UI presentation path observed on the reported frame.
    ui_surface_carrier_active: bool,
    /// Whether the cached carrier attachments were rebuilt on this frame.
    ui_surface_carrier_target_rebuilt: bool,
    /// External-texture batches actually submitted to the UI composition pass.
    presented_external_texture_batches: usize,
    /// Target-transfer Viewer batches actually submitted.
    presented_surface_code_value_batches: usize,
    /// ICC/device-code Viewer batches actually submitted.
    presented_device_code_value_batches: usize,
    prepare_attempts_timed: u64,
    accumulated_prepare_duration_us: u64,
    max_prepare_duration_us: u64,
    last_prepare_duration_us: Option<u64>,
    record_failures: u64,
    missing_output_textures: u64,
    registered_frames: u64,
    rejected_external_frames: u64,
    stage_total_stages: u64,
    stage_upload_stages: u64,
    stage_gpu_color_stages: u64,
    stage_readback_stages: u64,
    stage_gpu_blockers: u64,
    stage_gpu_shader_module_blockers: u64,
    stage_gpu_ocio_resource_blockers: u64,
    stage_gpu_wrapper_blockers: u64,
    stage_gpu_render_pipeline_blockers: u64,
    stage_pixels: u64,
    accumulated_stage_report: RenderGpuOutputStageDiagnosticsReport,
    last_stage_report: Option<RenderGpuOutputStageDiagnosticsReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spatial_runtime: Option<mondrian_renderer::GpuViewerSpatialRuntimeDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compositor_uniform_arena: Option<mondrian_renderer::GpuCompositorUniformArenaDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compositor_texture_bindings: Option<mondrian_renderer::GpuCompositorTextureBindingDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compositor_creative_luts: Option<mondrian_renderer::GpuCreativeLutCacheDiagnostics>,
    runtime_report: RenderGpuOutputRuntimeDiagnosticsReport,
    health: AppUiViewerGpuOutputHealthSummary,
    health_counts: AppUiViewerGpuOutputHealthCounts,
    last_frame_context: Option<AppUiViewerGpuOutputFrameContext>,
    last_preview_candidate_id: Option<u64>,
    last_preview_candidate_state: Option<AppUiViewerGpuOutputPreviewCandidateState>,
    last_color_rejection: Option<PreviewColorRejection>,
    last_display_contract_blocker: Option<AppUiDisplayBoundaryBlockerDiagnostics>,
    last_display_presentation_readiness: Option<AppUiDisplayPresentationReadinessDiagnostics>,
    recent_display_contract_refreshes: Vec<AppUiDisplayContractRefreshEvent>,
    last_display_contract_refresh: Option<AppUiDisplayContractRefreshEvent>,
    display_issue_summary: Option<AppUiDisplayIssueSummary>,
    last_outcome: Option<AppUiViewerGpuOutputOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_snapshot: Option<DisplaySnapshotDiagnostics>,
    /// Canonical display contract source value retained for qualification
    /// replay; `display_snapshot.contract_sha256` is derived from this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_output_contract: Option<mondrian_core::display_contract::DisplayOutputSnapshot>,
    /// Complete sampled ICC processor/LUT identity used by the Viewer frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_calibration_identity_sha256: Option<String>,
    /// ICC rendering intent used to build the sampled calibration LUT.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_calibration_rendering_intent: Option<mondrian_core::IccRenderingIntent>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct AppUiRendererAdapterDiagnostics {
    name: String,
    vendor_id: String,
    device_id: String,
    device_type: String,
    driver: String,
    driver_info: String,
    backend: String,
}

impl AppUiRendererAdapterDiagnostics {
    fn from_adapter(adapter: &wgpu::Adapter) -> Self {
        let info = adapter.get_info();
        Self {
            name: info.name,
            vendor_id: format!("{:04x}", info.vendor),
            device_id: format!("{:04x}", info.device),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver,
            driver_info: info.driver_info,
            backend: format!("{:?}", info.backend),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct DisplaySnapshotDiagnostics {
    display_name: Option<String>,
    platform: String,
    scale_factor_ppm: u32,
    surface_format: String,
    surface_color_space: String,
    surface_hdr_mode: String,
    requested_viewer_mode: String,
    resolved_output_color_space: String,
    ocio_display: Option<String>,
    ocio_view: Option<String>,
    monitor_profile_status: String,
    hdr_status: String,
    validation_status: String,
    blocker_count: u64,
    blocker_codes: Vec<String>,
    warning_count: u64,
    /// Complete 256-bit Display Output Contract identity.
    contract_sha256: String,
    contract_diagnostic_key: u64,
}

impl DisplaySnapshotDiagnostics {
    fn from_snapshot(snapshot: &mondrian_core::display_contract::DisplayOutputSnapshot) -> Self {
        Self {
            display_name: snapshot.display_id.name.clone(),
            platform: snapshot.platform.to_string(),
            scale_factor_ppm: snapshot.scale_factor.0,
            surface_format: snapshot.surface_format.clone(),
            surface_color_space: snapshot.surface_color_space.clone(),
            surface_hdr_mode: snapshot.surface_hdr_mode.clone(),
            requested_viewer_mode: snapshot.requested_viewer_mode.clone(),
            resolved_output_color_space: snapshot.resolved_output_color_space.clone(),
            ocio_display: snapshot.ocio_display.clone(),
            ocio_view: snapshot.ocio_view.clone(),
            monitor_profile_status: snapshot.monitor_profile_status.to_string(),
            hdr_status: snapshot.hdr_status.to_string(),
            validation_status: snapshot.validation_status.to_string(),
            blocker_count: snapshot.blockers.len() as u64,
            blocker_codes: snapshot.blockers.iter().map(|b| b.code().to_owned()).collect(),
            warning_count: snapshot.warnings.len() as u64,
            contract_sha256: snapshot
                .contract_identity()
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(""),
            contract_diagnostic_key: snapshot.contract_identity().diagnostic_key(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct AppUiViewerGpuOutputFrameContext {
    sequence_id: String,
    frame: i64,
    width: u32,
    height: u32,
    external_texture_key: String,
    output_target: AppUiViewerGpuOutputTarget,
    /// Program Output identity before preview-only monitor adaptation.
    output_color_space: ColorSpace,
    /// Local monitor identity after preview-only colorimetric adaptation.
    monitor_color_space: ColorSpace,
    tone_map: bool,
    preview_candidate_id: Option<u64>,
    preview_candidate_state: AppUiViewerGpuOutputPreviewCandidateState,
    display_view: Option<AppUiViewerGpuOutputDisplayView>,
    frame_residency: AppUiViewerGpuOutputFrameResidency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum AppUiViewerGpuOutputTarget {
    Display,
    Export,
    ReferenceOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct AppUiViewerGpuOutputDisplayView {
    display: String,
    view: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum AppUiDisplayContractRefreshReasonDiagnostic {
    Resize,
    ScaleFactorChanged,
    WindowMoved,
    DisplayPolicyChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum AppUiViewerGpuOutputPreviewCandidateState {
    Current,
    Transparent,
    Loading,
    Unavailable,
    Ready,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct AppUiDisplayOutputContractSnapshot {
    display_target: AppUiDisplayTarget,
    surface_format: AppUiSurfaceFormatDiagnostic,
    surface_color_space: AppUiSurfaceColorSpaceDiagnostic,
    surface_encoding: AppUiSurfaceEncodingDiagnostic,
    surface_hdr_mode: AppUiSurfaceHdrMode,
    display_tone_map_headroom_ppm: Option<u32>,
    available_surface_formats: Vec<AppUiSurfaceFormatDiagnostic>,
    format_color_spaces: Vec<AppUiSurfaceFormatColorSpacesDiagnostic>,
    present_modes: Vec<AppUiPresentModeDiagnostic>,
    alpha_modes: Vec<AppUiCompositeAlphaModeDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct AppUiDisplayContractRefreshEvent {
    reason: AppUiDisplayContractRefreshReasonDiagnostic,
    previous: AppUiDisplayOutputContractSnapshot,
    next: AppUiDisplayOutputContractSnapshot,
    renderer_rebuilt: bool,
    display_target_changed: bool,
    surface_format_changed: bool,
    surface_color_space_changed: bool,
    surface_hdr_mode_changed: bool,
    display_tone_map_headroom_changed: bool,
    available_surface_formats_changed: bool,
    format_color_spaces_changed: bool,
    present_modes_changed: bool,
    alpha_modes_changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum AppUiPresentModeDiagnostic {
    Fifo,
    FifoRelaxed,
    Immediate,
    Mailbox,
    AutoVsync,
    AutoNoVsync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum AppUiCompositeAlphaModeDiagnostic {
    Auto,
    Opaque,
    PreMultiplied,
    PostMultiplied,
    Inherit,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct AppUiSurfaceFormatColorSpacesDiagnostic {
    format: AppUiSurfaceFormatDiagnostic,
    srgb: bool,
    extended_srgb_linear: bool,
    display_p3: bool,
    bt2100_pq: bool,
    bt2100_hlg: bool,
    extended_srgb: bool,
    extended_display_p3: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum AppUiDisplayIssueReason {
    HdrOutputRequiresHdrSurface,
    OutputColorSpaceRequiresSurfaceColorSpace,
    ReconfigurationRequired,
    UnsupportedPresentationIntent,
    UnsupportedSurfaceContract,
    /// OS-level ICC profile, EDR, or HDR behavior is not supported on this
    /// platform. Display management cannot guarantee correct color presentation.
    #[allow(dead_code)] // Used by budget evaluator for forward-compat classification
    OsDisplayProfileUnsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct AppUiDisplayIssueSummary {
    reason: AppUiDisplayIssueReason,
    output_color_space: ColorSpace,
    preceding_display_contract_refresh: Option<AppUiDisplayContractRefreshEvent>,
    display_target: Option<AppUiDisplayTarget>,
    current_surface_format: Option<AppUiSurfaceFormatDiagnostic>,
    current_surface_color_space: Option<AppUiSurfaceColorSpaceDiagnostic>,
    current_surface_encoding: Option<AppUiSurfaceEncodingDiagnostic>,
    selected_surface_format: Option<AppUiSurfaceFormatDiagnostic>,
    selected_surface_color_space: Option<AppUiSurfaceColorSpaceDiagnostic>,
    selected_surface_encoding: Option<AppUiSurfaceEncodingDiagnostic>,
    surface_hdr_mode: Option<AppUiSurfaceHdrMode>,
    desired_surface_format: Option<AppUiSurfaceFormatDiagnostic>,
    desired_surface_color_space: Option<AppUiSurfaceColorSpaceDiagnostic>,
    desired_surface_encoding: Option<AppUiSurfaceEncodingDiagnostic>,
    desired_surface_hdr_mode: Option<AppUiSurfaceHdrMode>,
    payload_blocker: Option<AppUiDisplayPresentationPayloadBlocker>,
    supported_surface_color_space_count: Option<u8>,
    target_surface_color_space_supported: Option<bool>,
}

impl AppUiViewerGpuOutputTelemetry {
    fn diagnostics(
        &self,
        runtime_report: RenderGpuOutputRuntimeDiagnosticsReport,
    ) -> AppUiViewerGpuOutputDiagnostics {
        let mut display_issue_summary = display_issue_summary(
            self.last_display_contract_blocker,
            self.last_display_presentation_readiness,
        );
        if self.last_display_issue_refresh_generation
            == Some(self.display_contract_refresh_generation)
            && let Some(issue) = display_issue_summary.as_mut()
        {
            issue.preceding_display_contract_refresh = self.last_display_contract_refresh.clone();
        }
        let health = self.health_summary();
        AppUiViewerGpuOutputDiagnostics {
            qualification_run_id: None,
            process_instance_id: None,
            process_id: None,
            qualification_record_sequence: None,
            runtime_image_sha256: None,
            renderer_adapter: None,
            display_target: None,
            invocations: self.invocations,
            non_workspace_skips: self.non_workspace_skips,
            current_skips: self.current_skips,
            loading_skips: self.loading_skips,
            unavailable_skips: self.unavailable_skips,
            invalid_texture_keys: self.invalid_texture_keys,
            display_contract_blockers: self.display_contract_blockers,
            display_contract_hdr_surface_blockers: self.display_contract_hdr_surface_blockers,
            display_contract_surface_color_space_blockers: self
                .display_contract_surface_color_space_blockers,
            display_presentation_reconfigure_candidates: self
                .display_presentation_reconfigure_candidates,
            display_presentation_payload_blockers: self.display_presentation_payload_blockers,
            display_presentation_unsupported_contracts: self
                .display_presentation_unsupported_contracts,
            display_contract_refreshes: self.display_contract_refreshes,
            ui_surface_carrier_active: false,
            ui_surface_carrier_target_rebuilt: false,
            presented_external_texture_batches: 0,
            presented_surface_code_value_batches: 0,
            presented_device_code_value_batches: 0,
            prepare_attempts_timed: self.prepare_attempts_timed,
            accumulated_prepare_duration_us: self.accumulated_prepare_duration_us,
            max_prepare_duration_us: self.max_prepare_duration_us,
            last_prepare_duration_us: self.last_prepare_duration_us,
            record_failures: self.record_failures,
            missing_output_textures: self.missing_output_textures,
            registered_frames: self.registered_frames,
            rejected_external_frames: self.rejected_external_frames,
            stage_total_stages: self.accumulated_stage_diagnostics.total_stages,
            stage_upload_stages: self.accumulated_stage_diagnostics.upload_stages,
            stage_gpu_color_stages: self.accumulated_stage_diagnostics.gpu_color_stages,
            stage_readback_stages: self.accumulated_stage_diagnostics.readback_stages,
            stage_gpu_blockers: self.accumulated_stage_diagnostics.gpu_blockers,
            stage_gpu_shader_module_blockers: self
                .accumulated_stage_diagnostics
                .gpu_blocker_breakdown
                .shader_module_not_prepared,
            stage_gpu_ocio_resource_blockers: self
                .accumulated_stage_diagnostics
                .gpu_blocker_breakdown
                .ocio_resource_bind_group_not_prepared,
            stage_gpu_wrapper_blockers: self
                .accumulated_stage_diagnostics
                .gpu_blocker_breakdown
                .fullscreen_wrapper_not_prepared,
            stage_gpu_render_pipeline_blockers: self
                .accumulated_stage_diagnostics
                .gpu_blocker_breakdown
                .render_pipeline_not_prepared,
            stage_pixels: self.accumulated_stage_diagnostics.stage_pixels,
            accumulated_stage_report: self.accumulated_stage_diagnostics.into(),
            last_stage_report: self.last_stage_diagnostics.map(Into::into),
            spatial_runtime: self.last_spatial_runtime,
            compositor_uniform_arena: self.last_compositor_uniform_arena,
            compositor_texture_bindings: self.last_compositor_texture_bindings,
            compositor_creative_luts: self.last_compositor_creative_luts,
            runtime_report,
            health,
            health_counts: self.health_counts,
            last_frame_context: self.last_frame_context.clone(),
            last_preview_candidate_id: self.last_preview_candidate_id,
            last_preview_candidate_state: self.last_preview_candidate_state,
            last_color_rejection: None,
            last_display_contract_blocker: self.last_display_contract_blocker,
            last_display_presentation_readiness: self.last_display_presentation_readiness,
            recent_display_contract_refreshes: self.recent_display_contract_refreshes.clone(),
            last_display_contract_refresh: self.last_display_contract_refresh.clone(),
            display_issue_summary,
            last_outcome: self.last_outcome,
            display_snapshot: None,
            display_output_contract: None,
            display_calibration_identity_sha256: None,
            display_calibration_rendering_intent: None,
        }
    }

    fn record_invocation(&mut self) {
        self.invocations = self.invocations.saturating_add(1);
        self.last_stage_diagnostics = None;
        self.last_spatial_runtime = None;
        self.last_compositor_uniform_arena = None;
        self.last_compositor_texture_bindings = None;
        self.last_compositor_creative_luts = None;
        self.last_frame_context = None;
        self.last_preview_candidate_id = None;
        self.last_preview_candidate_state = None;
        self.last_display_contract_blocker = None;
        self.last_display_presentation_readiness = None;
        self.last_display_issue_refresh_generation = None;
        self.last_outcome = None;
    }

    fn health_summary(&self) -> AppUiViewerGpuOutputHealthSummary {
        let presentation_ready = self
            .last_display_presentation_readiness
            .map(|readiness| readiness.status == AppUiDisplayPresentationReadinessStatus::Current)
            .unwrap_or(true);
        classify_viewer_gpu_output_health(
            self.last_outcome,
            presentation_ready,
            self.last_stage_diagnostics,
        )
    }

    fn record_preview_candidate_state(
        &mut self,
        state: AppUiViewerGpuOutputPreviewCandidateState,
        preview_candidate_id: Option<u64>,
    ) {
        self.last_preview_candidate_id = preview_candidate_id;
        self.last_preview_candidate_state = Some(state);
    }

    fn record_frame_context(
        &mut self,
        frame: &PreviewGpuFrame,
        external_texture_key: String,
        frame_residency: AppUiViewerGpuOutputFrameResidency,
    ) {
        self.last_frame_context = Some(AppUiViewerGpuOutputFrameContext::from_frame(
            frame,
            external_texture_key,
            frame_residency,
        ));
    }

    fn record_prepare_duration(&mut self, duration: Duration) {
        let elapsed_us = duration.as_micros().min(u128::from(u64::MAX)) as u64;
        self.prepare_attempts_timed = self.prepare_attempts_timed.saturating_add(1);
        self.accumulated_prepare_duration_us =
            self.accumulated_prepare_duration_us.saturating_add(elapsed_us);
        self.max_prepare_duration_us = self.max_prepare_duration_us.max(elapsed_us);
        self.last_prepare_duration_us = Some(elapsed_us);
    }

    fn record_actual_frame_residency(&mut self, residency: AppUiViewerGpuOutputFrameResidency) {
        if let Some(context) = self.last_frame_context.as_mut() {
            context.frame_residency = residency;
        }
    }

    fn record_spatial_runtime(
        &mut self,
        diagnostics: mondrian_renderer::GpuViewerSpatialRuntimeDiagnostics,
    ) {
        self.last_spatial_runtime = Some(diagnostics);
    }

    fn record_compositor_uniform_arena(
        &mut self,
        diagnostics: mondrian_renderer::GpuCompositorUniformArenaDiagnostics,
    ) {
        self.last_compositor_uniform_arena = Some(diagnostics);
    }

    fn record_compositor_texture_bindings(
        &mut self,
        diagnostics: mondrian_renderer::GpuCompositorTextureBindingDiagnostics,
    ) {
        self.last_compositor_texture_bindings = Some(diagnostics);
    }

    fn record_compositor_creative_luts(
        &mut self,
        diagnostics: mondrian_renderer::GpuCreativeLutCacheDiagnostics,
    ) {
        self.last_compositor_creative_luts = Some(diagnostics);
    }

    fn record_non_workspace_skip(&mut self) {
        self.non_workspace_skips = self.non_workspace_skips.saturating_add(1);
        self.last_outcome = Some(AppUiViewerGpuOutputOutcome::NonWorkspace);
        self.record_health_count(AppUiViewerGpuOutputHealthStatus::Waiting);
    }

    fn record_current_skip(&mut self) {
        self.current_skips = self.current_skips.saturating_add(1);
        self.last_outcome = Some(AppUiViewerGpuOutputOutcome::Current);
        self.record_health_count(AppUiViewerGpuOutputHealthStatus::Waiting);
    }

    fn record_loading_skip(&mut self) {
        self.loading_skips = self.loading_skips.saturating_add(1);
        self.last_outcome = Some(AppUiViewerGpuOutputOutcome::Loading);
        self.record_health_count(AppUiViewerGpuOutputHealthStatus::Waiting);
    }

    fn record_unavailable_skip(&mut self) {
        self.unavailable_skips = self.unavailable_skips.saturating_add(1);
        self.last_outcome = Some(AppUiViewerGpuOutputOutcome::Unavailable);
        self.record_health_count(AppUiViewerGpuOutputHealthStatus::Waiting);
    }

    fn record_invalid_texture_key(&mut self) {
        self.invalid_texture_keys = self.invalid_texture_keys.saturating_add(1);
        self.last_outcome = Some(AppUiViewerGpuOutputOutcome::InvalidTextureKey);
        self.record_health_count(AppUiViewerGpuOutputHealthStatus::Waiting);
    }

    fn record_display_contract_blocker(&mut self, blocker: &AppUiDisplayBoundaryBlocker) {
        self.display_contract_blockers = self.display_contract_blockers.saturating_add(1);
        let diagnostics = blocker.diagnostics();
        match diagnostics.kind {
            AppUiDisplayBoundaryBlockerKind::HdrOutputRequiresHdrSurface => {
                self.display_contract_hdr_surface_blockers =
                    self.display_contract_hdr_surface_blockers.saturating_add(1);
            }
            AppUiDisplayBoundaryBlockerKind::OutputColorSpaceRequiresSurfaceColorSpace => {
                self.display_contract_surface_color_space_blockers =
                    self.display_contract_surface_color_space_blockers.saturating_add(1);
            }
        }
        self.last_display_contract_blocker = Some(diagnostics);
        self.last_display_issue_refresh_generation = Some(self.display_contract_refresh_generation);
        self.last_outcome = Some(AppUiViewerGpuOutputOutcome::DisplayContractBlocked);
        self.record_health_count(AppUiViewerGpuOutputHealthStatus::Blocked);
    }

    fn record_display_presentation_readiness(
        &mut self,
        diagnostics: AppUiDisplayPresentationReadinessDiagnostics,
    ) {
        match diagnostics.status {
            AppUiDisplayPresentationReadinessStatus::Current => {
                self.last_display_issue_refresh_generation = None;
            }
            AppUiDisplayPresentationReadinessStatus::ReconfigurationRequired => {
                self.display_presentation_reconfigure_candidates =
                    self.display_presentation_reconfigure_candidates.saturating_add(1);
                self.last_display_issue_refresh_generation =
                    Some(self.display_contract_refresh_generation);
            }
            AppUiDisplayPresentationReadinessStatus::UnsupportedPresentationIntent
            | AppUiDisplayPresentationReadinessStatus::UnsupportedSurfaceContract => {
                self.display_presentation_unsupported_contracts =
                    self.display_presentation_unsupported_contracts.saturating_add(1);
                self.last_display_issue_refresh_generation =
                    Some(self.display_contract_refresh_generation);
            }
        }
        self.last_display_presentation_readiness = Some(diagnostics);
    }

    fn record_display_contract_refresh(
        &mut self,
        reason: DisplayOutputContractRefreshReason,
        previous: &AppUiDisplayOutputContract,
        next: &AppUiDisplayOutputContract,
        renderer_rebuilt: bool,
    ) {
        self.display_contract_refreshes = self.display_contract_refreshes.saturating_add(1);
        self.display_contract_refresh_generation =
            self.display_contract_refresh_generation.saturating_add(1);
        let event = AppUiDisplayContractRefreshEvent::new(reason, previous, next, renderer_rebuilt);
        if self.recent_display_contract_refreshes.len()
            >= APP_UI_DISPLAY_CONTRACT_REFRESH_HISTORY_LIMIT
        {
            self.recent_display_contract_refreshes.remove(0);
        }
        self.recent_display_contract_refreshes.push(event.clone());
        self.last_display_contract_refresh = Some(event);
    }

    fn record_record_failure(&mut self) {
        self.record_failures = self.record_failures.saturating_add(1);
        self.last_outcome = Some(AppUiViewerGpuOutputOutcome::RecordFailed);
        self.record_health_count(AppUiViewerGpuOutputHealthStatus::Failed);
    }

    fn record_missing_output_texture(&mut self) {
        self.missing_output_textures = self.missing_output_textures.saturating_add(1);
        self.last_outcome = Some(AppUiViewerGpuOutputOutcome::OutputTextureMissing);
        self.record_health_count(AppUiViewerGpuOutputHealthStatus::Failed);
    }

    fn record_registered_frame(&mut self, diagnostics: RenderColorStageDiagnostics) {
        self.registered_frames = self.registered_frames.saturating_add(1);
        self.accumulated_stage_diagnostics.accumulate(diagnostics);
        self.last_stage_diagnostics = Some(diagnostics);
        self.last_outcome = Some(AppUiViewerGpuOutputOutcome::Registered);
        self.record_health_count(self.health_summary().status);
    }

    fn record_rejected_external_frame(&mut self, diagnostics: RenderColorStageDiagnostics) {
        self.rejected_external_frames = self.rejected_external_frames.saturating_add(1);
        self.accumulated_stage_diagnostics.accumulate(diagnostics);
        self.last_stage_diagnostics = Some(diagnostics);
        self.last_outcome = Some(AppUiViewerGpuOutputOutcome::ExternalFrameRejected);
        self.record_health_count(AppUiViewerGpuOutputHealthStatus::Rejected);
    }

    fn record_health_count(&mut self, status: AppUiViewerGpuOutputHealthStatus) {
        self.health_counts.record(status);
    }
}

impl AppUiViewerGpuOutputFrameContext {
    fn from_frame(
        frame: &PreviewGpuFrame,
        external_texture_key: String,
        frame_residency: AppUiViewerGpuOutputFrameResidency,
    ) -> Self {
        Self {
            sequence_id: frame.sequence_id.to_string(),
            frame: frame.frame,
            width: frame.width,
            height: frame.height,
            external_texture_key,
            output_target: AppUiViewerGpuOutputTarget::from(frame.program_output_boundary.target()),
            output_color_space: frame.program_output_boundary.output_color_space(),
            monitor_color_space: frame.monitor_adaptation.monitor_color_space(),
            tone_map: frame.program_output_boundary.tone_map(),
            preview_candidate_id: Some(frame.candidate_id()),
            preview_candidate_state: AppUiViewerGpuOutputPreviewCandidateState::Ready,
            display_view: frame.program_output_boundary.ocio_display_view().map(|display_view| {
                AppUiViewerGpuOutputDisplayView {
                    display: display_view.display.clone(),
                    view: display_view.view.clone(),
                }
            }),
            frame_residency,
        }
    }
}

impl From<ProgramOutputRole> for AppUiViewerGpuOutputTarget {
    fn from(target: ProgramOutputRole) -> Self {
        match target {
            ProgramOutputRole::Display => Self::Display,
            ProgramOutputRole::Export => Self::Export,
            ProgramOutputRole::ReferenceOutput => Self::ReferenceOutput,
        }
    }
}

fn display_issue_summary(
    blocker: Option<AppUiDisplayBoundaryBlockerDiagnostics>,
    readiness: Option<AppUiDisplayPresentationReadinessDiagnostics>,
) -> Option<AppUiDisplayIssueSummary> {
    if let Some(blocker) = blocker {
        return Some(AppUiDisplayIssueSummary::from_contract_blocker(blocker));
    }
    readiness.and_then(AppUiDisplayIssueSummary::from_presentation_readiness)
}

impl AppUiDisplayIssueSummary {
    fn from_contract_blocker(blocker: AppUiDisplayBoundaryBlockerDiagnostics) -> Self {
        let reason = match blocker.kind {
            AppUiDisplayBoundaryBlockerKind::HdrOutputRequiresHdrSurface => {
                AppUiDisplayIssueReason::HdrOutputRequiresHdrSurface
            }
            AppUiDisplayBoundaryBlockerKind::OutputColorSpaceRequiresSurfaceColorSpace => {
                AppUiDisplayIssueReason::OutputColorSpaceRequiresSurfaceColorSpace
            }
        };
        Self {
            reason,
            output_color_space: blocker.output_color_space,
            preceding_display_contract_refresh: None,
            display_target: None,
            current_surface_format: None,
            current_surface_color_space: None,
            current_surface_encoding: None,
            selected_surface_format: Some(blocker.selected_surface_format),
            selected_surface_color_space: Some(blocker.selected_surface_color_space),
            selected_surface_encoding: Some(blocker.selected_surface_encoding),
            surface_hdr_mode: Some(blocker.surface_hdr_mode),
            desired_surface_format: None,
            desired_surface_color_space: target_display_surface_color_space(
                blocker.output_color_space,
            ),
            desired_surface_encoding: target_display_surface_color_space(
                blocker.output_color_space,
            )
            .map(app_ui_surface_color_space_diagnostic_to_encoding),
            desired_surface_hdr_mode: target_display_surface_color_space(
                blocker.output_color_space,
            )
            .map(app_ui_surface_hdr_mode_from_diagnostic),
            payload_blocker: None,
            supported_surface_color_space_count: Some(blocker.supported_surface_color_space_count),
            target_surface_color_space_supported: Some(
                blocker.supports_output_surface_color_space(),
            ),
        }
    }

    fn from_presentation_readiness(
        readiness: AppUiDisplayPresentationReadinessDiagnostics,
    ) -> Option<Self> {
        let reason = match readiness.status {
            AppUiDisplayPresentationReadinessStatus::Current => return None,
            AppUiDisplayPresentationReadinessStatus::ReconfigurationRequired => {
                AppUiDisplayIssueReason::ReconfigurationRequired
            }
            AppUiDisplayPresentationReadinessStatus::UnsupportedPresentationIntent => {
                AppUiDisplayIssueReason::UnsupportedPresentationIntent
            }
            AppUiDisplayPresentationReadinessStatus::UnsupportedSurfaceContract => {
                AppUiDisplayIssueReason::UnsupportedSurfaceContract
            }
        };
        Some(Self {
            reason,
            output_color_space: readiness.output_color_space,
            preceding_display_contract_refresh: None,
            display_target: None,
            current_surface_format: Some(readiness.current_surface_format),
            current_surface_color_space: Some(readiness.current_surface_color_space),
            current_surface_encoding: Some(readiness.current_surface_encoding),
            selected_surface_format: None,
            selected_surface_color_space: None,
            selected_surface_encoding: None,
            surface_hdr_mode: Some(readiness.current_surface_hdr_mode),
            desired_surface_format: readiness.desired_surface_format,
            desired_surface_color_space: readiness.desired_surface_color_space,
            desired_surface_encoding: readiness.desired_surface_encoding,
            desired_surface_hdr_mode: readiness.desired_surface_hdr_mode,
            payload_blocker: readiness.payload_blocker,
            supported_surface_color_space_count: None,
            target_surface_color_space_supported: readiness
                .desired_surface_color_space
                .map(|desired| desired != AppUiSurfaceColorSpaceDiagnostic::Other),
        })
    }
}

impl AppUiDisplayOutputContractSnapshot {
    fn from_contract(contract: &AppUiDisplayOutputContract) -> Self {
        Self {
            display_target: contract.display_target.clone(),
            surface_format: app_ui_surface_format_diagnostic(contract.surface_color.format),
            surface_color_space: app_ui_surface_color_space_diagnostic(
                contract.surface_color.color_space,
            ),
            surface_encoding: app_ui_surface_encoding_diagnostic(contract.surface_color.encoding),
            surface_hdr_mode: contract.surface_color.hdr_mode,
            display_tone_map_headroom_ppm: app_ui_display_tone_map_headroom_ppm(
                &contract.display_hdr_info,
            ),
            available_surface_formats: contract
                .available_formats
                .iter()
                .copied()
                .map(app_ui_surface_format_diagnostic)
                .collect(),
            format_color_spaces: contract
                .format_color_spaces
                .iter()
                .map(AppUiSurfaceFormatColorSpacesDiagnostic::from_contract)
                .collect(),
            present_modes: contract
                .present_modes
                .iter()
                .copied()
                .map(app_ui_present_mode_diagnostic)
                .collect(),
            alpha_modes: contract
                .alpha_modes
                .iter()
                .copied()
                .map(app_ui_composite_alpha_mode_diagnostic)
                .collect(),
        }
    }
}

impl AppUiDisplayContractRefreshEvent {
    fn new(
        reason: DisplayOutputContractRefreshReason,
        previous: &AppUiDisplayOutputContract,
        next: &AppUiDisplayOutputContract,
        renderer_rebuilt: bool,
    ) -> Self {
        Self {
            reason: AppUiDisplayContractRefreshReasonDiagnostic::from_reason(reason),
            previous: AppUiDisplayOutputContractSnapshot::from_contract(previous),
            next: AppUiDisplayOutputContractSnapshot::from_contract(next),
            renderer_rebuilt,
            display_target_changed: previous.display_target != next.display_target,
            surface_format_changed: previous.surface_color.format != next.surface_color.format,
            surface_color_space_changed: previous.surface_color.color_space
                != next.surface_color.color_space,
            surface_hdr_mode_changed: previous.surface_color.hdr_mode
                != next.surface_color.hdr_mode,
            display_tone_map_headroom_changed: app_ui_display_tone_map_headroom_ppm(
                &previous.display_hdr_info,
            ) != app_ui_display_tone_map_headroom_ppm(
                &next.display_hdr_info,
            ),
            available_surface_formats_changed: previous.available_formats != next.available_formats,
            format_color_spaces_changed: previous.format_color_spaces != next.format_color_spaces,
            present_modes_changed: previous.present_modes != next.present_modes,
            alpha_modes_changed: previous.alpha_modes != next.alpha_modes,
        }
    }
}

impl AppUiSurfaceFormatColorSpacesDiagnostic {
    fn from_contract(value: &AppUiSurfaceFormatColorSpaces) -> Self {
        Self {
            format: app_ui_surface_format_diagnostic(value.format),
            srgb: value.srgb,
            extended_srgb_linear: value.extended_srgb_linear,
            display_p3: value.display_p3,
            bt2100_pq: value.bt2100_pq,
            bt2100_hlg: value.bt2100_hlg,
            extended_srgb: value.extended_srgb,
            extended_display_p3: value.extended_display_p3,
        }
    }
}

impl AppUiDisplayBoundaryBlockerDiagnostics {
    fn supports_output_surface_color_space(self) -> bool {
        match target_display_surface_color_space(self.output_color_space) {
            Some(AppUiSurfaceColorSpaceDiagnostic::Srgb) => self.supports_srgb,
            Some(AppUiSurfaceColorSpaceDiagnostic::DisplayP3) => self.supports_display_p3,
            Some(AppUiSurfaceColorSpaceDiagnostic::ExtendedSrgbLinear) => {
                self.supports_extended_srgb_linear
            }
            Some(AppUiSurfaceColorSpaceDiagnostic::ExtendedSrgb) => self.supports_extended_srgb,
            Some(AppUiSurfaceColorSpaceDiagnostic::ExtendedDisplayP3) => {
                self.supports_extended_display_p3
            }
            Some(AppUiSurfaceColorSpaceDiagnostic::Bt2100Pq) => self.supports_bt2100_pq,
            Some(AppUiSurfaceColorSpaceDiagnostic::Bt2100Hlg) => self.supports_bt2100_hlg,
            Some(AppUiSurfaceColorSpaceDiagnostic::Other) | None => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayOutputContractRefreshReason {
    SurfaceLifecycle(SurfaceLifecycleReason),
    WindowMoved,
    DisplayPolicyChanged,
}

impl AppUiDisplayContractRefreshReasonDiagnostic {
    fn from_reason(reason: DisplayOutputContractRefreshReason) -> Self {
        match reason {
            DisplayOutputContractRefreshReason::SurfaceLifecycle(
                SurfaceLifecycleReason::Resize,
            ) => Self::Resize,
            DisplayOutputContractRefreshReason::SurfaceLifecycle(
                SurfaceLifecycleReason::ScaleFactorChanged,
            ) => Self::ScaleFactorChanged,
            DisplayOutputContractRefreshReason::WindowMoved => Self::WindowMoved,
            DisplayOutputContractRefreshReason::DisplayPolicyChanged => Self::DisplayPolicyChanged,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SurfaceLifecycleUpdate {
    reconfigure_surface: bool,
    relayout_root: bool,
    request_redraw: bool,
    bounds: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppUiEventLoopStage {
    DrainActions,
    RedrawRequested,
    PrepareViewerGpuPreview,
    RefreshIfDirty,
    PaintAndRender,
    PollBackgroundTasks,
    AdvancePlaybackClock,
}

impl AppUiEventLoopStage {
    const COUNT: usize = 7;

    const fn index(self) -> usize {
        match self {
            Self::DrainActions => 0,
            Self::RedrawRequested => 1,
            Self::PrepareViewerGpuPreview => 2,
            Self::RefreshIfDirty => 3,
            Self::PaintAndRender => 4,
            Self::PollBackgroundTasks => 5,
            Self::AdvancePlaybackClock => 6,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::DrainActions => "drain_actions",
            Self::RedrawRequested => "redraw_requested",
            Self::PrepareViewerGpuPreview => "prepare_viewer_gpu_preview",
            Self::RefreshIfDirty => "refresh_if_dirty",
            Self::PaintAndRender => "paint_and_render",
            Self::PollBackgroundTasks => "poll_background_tasks",
            Self::AdvancePlaybackClock => "advance_playback_clock",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AppUiEventLoopStageStats {
    calls: u64,
    slow_calls: u64,
    accumulated_duration_us: u64,
    max_duration_us: u64,
    last_duration_us: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AppUiEventLoopTelemetry {
    stages: [AppUiEventLoopStageStats; AppUiEventLoopStage::COUNT],
}

impl AppUiEventLoopTelemetry {
    fn record_stage_duration(&mut self, stage: AppUiEventLoopStage, duration: Duration) {
        let duration_us = duration.as_micros().min(u128::from(u64::MAX)) as u64;
        let stats = &mut self.stages[stage.index()];
        let previous_max = stats.max_duration_us;
        stats.calls = stats.calls.saturating_add(1);
        stats.accumulated_duration_us = stats.accumulated_duration_us.saturating_add(duration_us);
        stats.max_duration_us = stats.max_duration_us.max(duration_us);
        stats.last_duration_us = Some(duration_us);
        if duration_us >= APP_UI_EVENT_LOOP_SLOW_STAGE_BUDGET_US {
            stats.slow_calls = stats.slow_calls.saturating_add(1);
            if duration_us >= previous_max {
                tracing::warn!(
                    stage = stage.as_str(),
                    duration_us,
                    max_duration_us = stats.max_duration_us,
                    slow_calls = stats.slow_calls,
                    budget_us = APP_UI_EVENT_LOOP_SLOW_STAGE_BUDGET_US,
                    "app UI event loop stage exceeded responsiveness budget"
                );
            }
        }
    }

    #[cfg(test)]
    fn stage_stats(&self, stage: AppUiEventLoopStage) -> AppUiEventLoopStageStats {
        self.stages[stage.index()]
    }
}

struct AppUiWindowSession {
    // Move-only generation members are transferred to the non-UI progress
    // domain by explicit shutdown. `Drop` is fallback-only; the Window thread
    // never directly joins or cancels GPU work.
    viewer_gpu_device_progress: ViewerGpuDeviceGenerationMember<ViewerGpuDeviceProgressOwner>,
    surface_generation_id: AppUiSurfaceGenerationId,
    role: AppUiWindowRole,
    window: Arc<winit::window::Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    display_output_contract: AppUiDisplayOutputContract,
    display_snapshot: Option<mondrian_core::display_contract::DisplayOutputSnapshot>,
    display_calibration: Option<Arc<mondrian_core::display_calibration::DisplayCalibrationLut3d>>,
    renderer_adapter: AppUiRendererAdapterDiagnostics,
    color_engine: mondrian_core::ColorEngine,
    display_management_policy: mondrian_core::color_models::DisplayManagementPolicy,
    frame_renderer: AppUiFrameRenderer,
    renderer_device: wgpu::Device,
    renderer_queue: wgpu::Queue,
    viewer_gpu_execution: ViewerGpuDeviceGenerationMember<ViewerGpuExecutionRuntime>,
    viewer_gpu_presentation: WindowViewerGpuPresentationState,
    viewer_gpu_submissions: ViewerGpuSubmissionLifecycle<
        WindowViewerGpuSubmissionOwner,
        ViewerHeterogeneousGpuCompletedBatch,
    >,
    /// Bounded CPU-complete ticketless horizon. It owns no GPU submission or
    /// presentation lease; an exact entry may either become the immediate
    /// successor or acquire the current Frame Demand after a clock jump.
    staged_viewer_gpu_successors: PreviewGpuFrameStaging,
    viewer_gpu_deferred_cleanup: WindowViewerGpuDeferredCleanup,
    program_scopes_registered: bool,
    program_scopes_refresh_requested: bool,
    program_scopes_analysis_identity: Option<(WaveformMode, ProgramScopeScale, ProgramScopesTap)>,
    viewer_gpu_output_telemetry: AppUiViewerGpuOutputTelemetry,
    render_diagnostic_reporter: AppUiRenderDiagnosticReporter,
    router: EventRouter,
    ui_runtime: WinitUiRuntime,
    last_cursor: Point,
    last_window_cursor_icon: Option<winit::window::CursorIcon>,
    current_bounds: std::cell::Cell<Rect>,
    modifiers_state: Modifiers,
    pending_initial_redraw: bool,
    event_loop_failure: Option<String>,
    event_loop_telemetry: AppUiEventLoopTelemetry,
    playback_thread_scheduling: mondrian_platform::PlaybackThreadScheduling,
}

fn synchronize_playback_thread_scheduling(host: &AppUiHost, session: &mut AppUiWindowSession) {
    if let Err(error) = session.playback_thread_scheduling.synchronize(host.is_playback_active()) {
        tracing::warn!(%error, "native playback thread scheduling unavailable");
    }
}

#[cfg(feature = "validation")]
const fn validation_window_advances_playback(surface_reopen_active: bool) -> bool {
    !surface_reopen_active
}

fn poll_window_background_tasks(
    host: &mut AppUiHost,
    session: &mut AppUiWindowSession,
) -> AppUiBackgroundTaskPollOutcome {
    let poll_started = Instant::now();
    let outcome = host.poll_background_tasks(session.current_bounds.get());
    // A submitted heterogeneous batch may still reference every allocation
    // owned by this runtime. Product-policy trim/reconfigure decisions remain
    // pending until exact GPU completion retires that batch.
    if !session.viewer_gpu_submissions.is_occupied() {
        host.apply_preview_execution_resource_decision(&mut *session.viewer_gpu_execution);
    }
    session.event_loop_telemetry.record_stage_duration(
        AppUiEventLoopStage::PollBackgroundTasks,
        poll_started.elapsed(),
    );
    outcome
}

/// Window-owned state retained from queue submission through actual GPU
/// completion.
///
/// The complete frame remains here because its media-protection leases are the
/// Frame Store's native-resource ledger authority. A renderer-retained AVFrame
/// protects the physical decoder object but cannot replace those leases.
struct WindowViewerGpuSubmissionOwner {
    frame: Box<PreviewGpuFrame>,
    terminal: Option<PreviewGpuHeterogeneousExecution>,
    texture_key: ExternalTextureKey,
    texture_registered: bool,
    output_lease: Option<ViewerGpuPresentationOutputLease>,
    presentation: ViewerExternalTexturePresentation,
    stage_diagnostics: RenderColorStageDiagnostics,
    program_scopes: Option<mondrian_renderer::GpuProgramScopesRecord>,
    program_scopes_requested: bool,
    program_scopes_analysis_identity: Option<(WaveformMode, ProgramScopeScale, ProgramScopesTap)>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum WindowViewerGpuDeferredCleanup {
    #[default]
    None,
    ClearFrameResources,
    Reset,
}

type WindowViewerGpuPublishedOutput = ViewerGpuPhysicalPublication<
    crate::app::preview_execution::PreviewOutputKey,
    ExternalTextureKey,
    ViewerGpuPresentationOutputLease,
>;

type WindowViewerGpuPublicationSlots = ViewerGpuPublicationSlots<
    crate::app::preview_execution::PreviewOutputKey,
    ExternalTextureKey,
    ViewerGpuPresentationOutputLease,
>;

/// Window-only ownership of the texture registration currently published by the Viewer.
#[derive(Default)]
struct WindowViewerGpuPresentationState {
    publications: WindowViewerGpuPublicationSlots,
    presentation: Option<ViewerExternalTexturePresentation>,
}

impl WindowViewerGpuPresentationState {
    fn published_output(&self) -> Option<&WindowViewerGpuPublishedOutput> {
        self.publications.current()
    }

    fn take_published_output_for_submission(
        &mut self,
        submission_id: ViewerGpuSubmissionId,
    ) -> Option<WindowViewerGpuPublishedOutput> {
        self.publications.take_for_submission(submission_id)
    }

    fn presentation(&self) -> Option<ViewerExternalTexturePresentation> {
        self.presentation
    }

    fn set_presentation(&mut self, presentation: ViewerExternalTexturePresentation) {
        self.presentation = Some(presentation);
    }

    fn take_presentation(&mut self) -> bool {
        self.presentation.take().is_some()
    }

    fn clear(&mut self) -> [Option<WindowViewerGpuPublishedOutput>; 2] {
        self.presentation = None;
        self.publications.drain()
    }
}

struct WindowViewerGpuGenerationRetirement {
    runtime: mondrian_renderer::ViewerGpuExecutionRetirement,
    lifecycle: ViewerGpuSubmissionLifecycle<
        WindowViewerGpuSubmissionOwner,
        ViewerHeterogeneousGpuCompletedBatch,
    >,
    _presentation: WindowViewerGpuPresentationState,
    _renderer_device: wgpu::Device,
    _renderer_queue: wgpu::Queue,
    _deferred_cleanup: WindowViewerGpuDeferredCleanup,
    _completed_submissions: Vec<
        ViewerGpuCompletedSubmission<
            WindowViewerGpuSubmissionOwner,
            ViewerHeterogeneousGpuCompletedBatch,
        >,
    >,
    _lost_submission_owners: Vec<WindowViewerGpuSubmissionOwner>,
    native_retirement_error_logged: bool,
}

#[derive(Debug, thiserror::Error)]
enum WindowViewerGpuGenerationRetirementTakeError {
    #[error("Window Viewer GPU generation has no live progress owner")]
    MissingProgressOwner,
    #[error("Window Viewer GPU generation lost its execution runtime")]
    MissingExecutionRuntime,
}

impl ViewerGpuDeviceGenerationRetirement for WindowViewerGpuGenerationRetirement {
    fn label(&self) -> &'static str {
        "Window Viewer GPU device generation"
    }

    fn poll_retirement(
        &mut self,
        terminal: Option<&ViewerGpuDeviceGenerationTerminal>,
    ) -> Option<crate::app::viewer_gpu_device_progress::ViewerGpuDeviceGenerationRetirementReceipt>
    {
        let renderer = match self.runtime.poll() {
            Ok(receipt) => receipt,
            Err(error) => {
                if !self.native_retirement_error_logged {
                    tracing::error!(
                        %error,
                        "Window Viewer GPU retirement could not prove native copy-fence progress"
                    );
                    self.native_retirement_error_logged = true;
                }
                None
            }
        };

        match self.lifecycle.poll(Instant::now()) {
            ViewerGpuSubmissionPoll::Completed(completed) => {
                self._completed_submissions.push(completed);
            }
            ViewerGpuSubmissionPoll::RetiredAfterQuarantine(retired) => {
                tracing::warn!(
                    submission_id = retired.submission_id.get(),
                    reason = ?retired.reason,
                    "Window Viewer force-retired a quarantined GPU submission whose completion callback was lost"
                );
                self._lost_submission_owners.push(retired.owner);
            }
            ViewerGpuSubmissionPoll::Idle
            | ViewerGpuSubmissionPoll::Pending { .. }
            | ViewerGpuSubmissionPoll::QuarantineStarted(_) => {}
        }

        // Actual wgpu loss is safe terminal evidence for wgpu work only. The
        // independent D3D decoder-copy fence above must still be ready before
        // the media/lifecycle owner can move out of the callback slot.
        if renderer.is_some()
            && terminal.is_some_and(ViewerGpuDeviceGenerationTerminal::wgpu_work_is_terminal)
            && self.lifecycle.is_occupied()
        {
            self._lost_submission_owners
                .extend(self.lifecycle.retire_owners_after_wgpu_device_loss());
        }

        if self.lifecycle.is_occupied() {
            return None;
        }
        renderer.map(|renderer| {
            crate::app::viewer_gpu_device_progress::ViewerGpuDeviceGenerationRetirementReceipt {
                renderer: Some(renderer),
            }
        })
    }
}

impl Drop for AppUiWindowSession {
    fn drop(&mut self) {
        if self.viewer_gpu_device_progress.generation_id().is_none() {
            // A replacement shell has not yet received the shared generation;
            // it has no execution runtime or upload worker to retire.
            return;
        }
        match self.take_viewer_gpu_generation_retirement() {
            Ok((progress, retirement)) => progress.retire_device_generation(retirement),
            Err(error) => tracing::error!(%error),
        }
    }
}

/// Independent publication and device-generation closure facts for an active
/// Window session. Publication cleanup failure never suppresses the raw GPU
/// worker/Renderer receipt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct AppUiActiveWindowGpuShutdownEvidence {
    publication_cleanup: Result<(), String>,
    retirement: AppUiWindowGpuRetirementEvidence,
}

impl AppUiActiveWindowGpuShutdownEvidence {
    pub(super) fn qualifies_normal_runtime(&self) -> bool {
        self.publication_cleanup.is_ok()
            && matches!(
                &self.retirement,
                AppUiWindowGpuRetirementEvidence::Retired(evidence)
                    if evidence.qualifies_created_inventory(true)
            )
    }

    fn qualification_failure(&self) -> Option<String> {
        (!self.qualifies_normal_runtime())
            .then(|| format!("Window Viewer GPU generation did not close cleanly: {self:?}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum AppUiWindowGpuRetirementEvidence {
    Retired(ViewerGpuDeviceProgressShutdownEvidence),
    OwnershipFault(AppUiWindowGpuOwnershipFault),
    ShutdownPanicked(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum AppUiWindowGpuOwnershipFault {
    MissingProgressOwner,
    MissingExecutionRuntime,
}

#[cfg(feature = "validation")]
struct AppUiSurfaceDeviceReopenTransition {
    surface_generation_before: u64,
    surface_generation_after: u64,
    device_generation_before: u64,
    device_generation_after: u64,
    shutdown_receipt_json: String,
    shutdown_receipt_sha256: String,
}

fn request_app_ui_device(
    adapter: &wgpu::Adapter,
) -> Result<(wgpu::Device, wgpu::Queue), wgpu::RequestDeviceError> {
    let descriptor = wgpu::DeviceDescriptor {
        required_features: native_video_texture_device_features(adapter.features())
            | ocio_lut_filtering_device_features(adapter.features()),
        ..wgpu::DeviceDescriptor::default()
    };
    pollster::block_on(adapter.request_device(&descriptor))
}

#[cfg(feature = "validation")]
fn close_failed_viewer_gpu_startup(
    startup: crate::app::viewer_gpu_startup::ViewerGpuStartupOwner,
    deadline: Instant,
    primary: impl std::fmt::Display,
) -> Box<dyn std::error::Error> {
    let cleanup = startup.shutdown_until(deadline);
    let cleanup_released = cleanup.is_some_and(|receipt| receipt.all_created_resources_released());
    format!(
        "{primary}; Viewer GPU startup cleanup_all_created_resources_released={cleanup_released}; cleanup={cleanup:?}"
    )
    .into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AppUiPreActiveWindowStartupStage {
    HostOwned,
    WindowCreated,
    SurfaceCreated,
    AdapterSelected,
    DeviceQueueCreated,
    ViewerGpuProgressStarted,
    PreviewWakerInstalled,
    WindowPrepared,
    WindowActivated,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum AppUiPreActiveViewerGpuShutdownEvidence {
    NotStarted,
    Shutdown(Option<crate::app::viewer_gpu_startup::ViewerGpuStartupShutdownEvidence>),
    ShutdownPanicked(String),
}

impl AppUiPreActiveViewerGpuShutdownEvidence {
    fn all_created_resources_released(&self) -> bool {
        match self {
            Self::NotStarted => true,
            Self::Shutdown(Some(evidence)) => evidence.all_created_resources_released(),
            Self::Shutdown(None) | Self::ShutdownPanicked(_) => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct AppUiPreActiveWindowShutdownEvidence {
    last_stage: AppUiPreActiveWindowStartupStage,
    rust_native_authority_released_on_event_loop_thread: bool,
    viewer_gpu: AppUiPreActiveViewerGpuShutdownEvidence,
}

impl AppUiPreActiveWindowShutdownEvidence {
    pub(super) fn all_created_resources_released(&self) -> bool {
        self.rust_native_authority_released_on_event_loop_thread
            && self.viewer_gpu.all_created_resources_released()
    }
}

struct AppUiPreActiveWindowStartupFailure {
    primary: String,
    shutdown: AppUiPreActiveWindowShutdownEvidence,
}

struct AppUiInitialWindowCandidate {
    preview_work_event_proxy: winit::event_loop::EventLoopProxy<AppUiUserEvent>,
    preview_work_watch: PreviewWorkWatch,
    preview_work_event_pending: Arc<AtomicBool>,
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    session: AppUiWindowSession,
    session_bounds: Rect,
}

fn prepare_initial_window_candidate(
    event_loop: &winit::event_loop::EventLoop<AppUiUserEvent>,
    host: &AppUiHost,
    deadline: Instant,
) -> Result<AppUiInitialWindowCandidate, AppUiPreActiveWindowStartupFailure> {
    catch_pre_active_window_construction(deadline, |last_stage, viewer_gpu_startup| {
        let preview_work_event_proxy = event_loop.create_proxy();
        let startup_window = Arc::new(
            event_loop
                .create_window(window_attributes_for_role(AppUiWindowRole::Startup))
                .map_err(|error| format!("could not create startup Window: {error}"))?,
        );
        *last_stage = AppUiPreActiveWindowStartupStage::WindowCreated;

        let instance_desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        let instance = wgpu::Instance::new(instance_desc);
        let startup_surface = instance
            .create_surface(startup_window.clone())
            .map_err(|error| format!("could not create startup Surface: {error}"))?;
        *last_stage = AppUiPreActiveWindowStartupStage::SurfaceCreated;

        let adapter = pollster::block_on(request_adapter_with_native_video_preference(
            &instance,
            &wgpu::RequestAdapterOptions {
                compatible_surface: Some(&startup_surface),
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            },
        ))
        .map_err(|_| "No suitable GPU adapter".to_owned())?;
        *last_stage = AppUiPreActiveWindowStartupStage::AdapterSelected;

        let adapter_info = adapter.get_info();
        crate::app_ui::about_dialog::SYSTEM_INFO
            .set(crate::app_ui::about_dialog::AboutSystemInfo {
                pkg_version: env!("CARGO_PKG_VERSION").to_owned(),
                rust_version: env!("CARGO_PKG_RUST_VERSION").to_owned(),
                os: if cfg!(windows) {
                    "Windows"
                } else {
                    std::env::consts::OS
                }
                .to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                os_version: String::new(),
                wgpu_backend: format!("{:?}", adapter_info.backend),
                gpu_name: adapter_info.name,
            })
            .ok();

        let (device, queue) = request_app_ui_device(&adapter)
            .map_err(|error| format!("could not create startup Device/Queue: {error}"))?;
        *last_stage = AppUiPreActiveWindowStartupStage::DeviceQueueCreated;

        let viewer_gpu_completion_event_proxy = preview_work_event_proxy.clone();
        let viewer_gpu_progress_wake = ViewerGpuDeviceProgressWake::new(move || {
            let _ = viewer_gpu_completion_event_proxy
                .send_event(AppUiUserEvent::ViewerGpuCompletionAvailable);
        });
        *viewer_gpu_startup = Some(
            crate::app::viewer_gpu_startup::ViewerGpuStartupOwner::new(
                &device,
                &queue,
                viewer_gpu_progress_wake,
            )
            .map_err(|error| format!("could not start Viewer GPU progress: {error}"))?,
        );
        *last_stage = AppUiPreActiveWindowStartupStage::ViewerGpuProgressStarted;

        let preview_work_watch = host.preview_work_watch();
        let preview_work_event_pending = Arc::new(AtomicBool::new(false));
        let worker_event_pending = Arc::clone(&preview_work_event_pending);
        let worker_event_proxy = preview_work_event_proxy.clone();
        if let Err(failure) = preview_work_watch.install_waker(move || {
            queue_preview_work_event(&worker_event_pending, || {
                worker_event_proxy.send_event(AppUiUserEvent::PreviewWorkAvailable).is_ok()
            });
        }) {
            let (reason, callback) = failure.into_parts();
            drop(callback);
            return Err(reason.to_string());
        }
        *last_stage = AppUiPreActiveWindowStartupStage::PreviewWakerInstalled;

        let startup = viewer_gpu_startup
            .as_mut()
            .ok_or_else(|| "Viewer GPU startup owner was lost before preparation".to_owned())?;
        let prepared_session = AppUiPreparedWindowSession::prepare(
            AppUiWindowRole::Startup,
            startup_window,
            startup_surface,
            &adapter,
            &device,
            &queue,
            host,
            Some(startup),
        )
        .map_err(|error| error.to_string())?;
        *last_stage = AppUiPreActiveWindowStartupStage::WindowPrepared;
        let activated_session =
            prepared_session.activate(Some(startup)).map_err(|error| error.to_string())?;
        *last_stage = AppUiPreActiveWindowStartupStage::WindowActivated;
        let (session, session_bounds) = activated_session.into_parts();

        Ok::<_, String>(AppUiInitialWindowCandidate {
            preview_work_event_proxy,
            preview_work_watch,
            preview_work_event_pending,
            instance,
            adapter,
            device,
            queue,
            session,
            session_bounds,
        })
    })
}

fn catch_pre_active_window_construction<T>(
    deadline: Instant,
    build: impl FnOnce(
        &mut AppUiPreActiveWindowStartupStage,
        &mut Option<crate::app::viewer_gpu_startup::ViewerGpuStartupOwner>,
    ) -> Result<T, String>,
) -> Result<T, AppUiPreActiveWindowStartupFailure> {
    let mut last_stage = AppUiPreActiveWindowStartupStage::HostOwned;
    let mut viewer_gpu_startup = None;
    let build = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        build(&mut last_stage, &mut viewer_gpu_startup)
    }));
    match build {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(primary)) => Err(close_pre_active_native_construction(
            primary,
            last_stage,
            viewer_gpu_startup,
            deadline,
        )),
        Err(payload) => Err(close_pre_active_native_construction(
            crate::app::execution_panic_diagnostic::execution_panic_diagnostic(
                payload,
                "pre-active Window construction",
            )
            .to_string(),
            last_stage,
            viewer_gpu_startup,
            deadline,
        )),
    }
}

fn close_pre_active_native_construction(
    primary: String,
    last_stage: AppUiPreActiveWindowStartupStage,
    viewer_gpu_startup: Option<crate::app::viewer_gpu_startup::ViewerGpuStartupOwner>,
    deadline: Instant,
) -> AppUiPreActiveWindowStartupFailure {
    let viewer_gpu = match viewer_gpu_startup {
        None => AppUiPreActiveViewerGpuShutdownEvidence::NotStarted,
        Some(startup) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            startup.shutdown_until(deadline)
        })) {
            Ok(evidence) => AppUiPreActiveViewerGpuShutdownEvidence::Shutdown(evidence),
            Err(payload) => AppUiPreActiveViewerGpuShutdownEvidence::ShutdownPanicked(
                crate::app::execution_panic_diagnostic::execution_panic_diagnostic(
                    payload,
                    "pre-active Viewer GPU shutdown",
                )
                .to_string(),
            ),
        },
    };
    AppUiPreActiveWindowStartupFailure {
        primary,
        shutdown: AppUiPreActiveWindowShutdownEvidence {
            last_stage,
            // The construction closure has returned or unwound before this
            // evidence is built, so its Window/Surface/Adapter/Device owners
            // have been consumed on the event-loop thread. This is not a claim
            // that the OS compositor or native driver reported termination.
            rust_native_authority_released_on_event_loop_thread: true,
            viewer_gpu,
        },
    }
}

#[cfg(feature = "validation")]
#[allow(clippy::too_many_arguments)]
fn reopen_window_surface_and_device(
    elwt: &winit::event_loop::ActiveEventLoop,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    device: &mut wgpu::Device,
    queue: &mut wgpu::Queue,
    host: &mut AppUiHost,
    session: &mut AppUiWindowSession,
    event_proxy: &winit::event_loop::EventLoopProxy<AppUiUserEvent>,
    deadline: Instant,
) -> Result<AppUiSurfaceDeviceReopenTransition, Box<dyn std::error::Error>> {
    let surface_generation_before = session.surface_generation_id.get();
    let device_generation_before = session
        .viewer_gpu_device_progress
        .generation_id()
        .ok_or("Window Viewer GPU device generation is missing")?
        .get();

    // Prepare fresh platform/GPU admission before revoking the old window.
    // The activated candidate becomes the outer Session owner before any old
    // generation operation can fail, but remains Host-unpublished until the
    // old owner has been consumed and proved retired.
    let next_window = Arc::new(elwt.create_window(window_attributes_for_role(session.role))?);
    let next_surface = instance.create_surface(next_window.clone())?;
    let (next_device, next_queue) = request_app_ui_device(adapter)?;
    let completion_proxy = event_proxy.clone();
    let mut next_startup = crate::app::viewer_gpu_startup::ViewerGpuStartupOwner::new(
        &next_device,
        &next_queue,
        ViewerGpuDeviceProgressWake::new(move || {
            let _ = completion_proxy.send_event(AppUiUserEvent::ViewerGpuCompletionAvailable);
        }),
    )?;

    let prepared = match AppUiPreparedWindowSession::prepare(
        session.role,
        next_window,
        next_surface,
        adapter,
        &next_device,
        &next_queue,
        host,
        Some(&mut next_startup),
    ) {
        Ok(prepared) => prepared,
        Err(primary) => {
            return Err(close_failed_viewer_gpu_startup(
                next_startup,
                deadline,
                primary,
            ));
        }
    };
    let surface_generation_after = prepared.surface_generation_id().get();
    let device_generation_after = next_startup.generation_id().get();
    if surface_generation_after == surface_generation_before
        || device_generation_after == device_generation_before
    {
        return Err(close_failed_viewer_gpu_startup(
            next_startup,
            deadline,
            "Surface/device recovery reused a consumed generation identity",
        ));
    }

    let activated = match prepared.activate(Some(&mut next_startup)) {
        Ok(session) => session,
        Err(primary) => {
            return Err(close_failed_viewer_gpu_startup(
                next_startup,
                deadline,
                primary,
            ));
        }
    };
    let (mut retired_session, bounds) = activated.into_parts();
    let (shutdown_receipt_json, shutdown_receipt_sha256) = with_window_candidate_installed(
        session,
        &mut retired_session,
        |active_session, retired_session| {
            clear_viewer_spatial_presentation(retired_session, host);
            retired_session.window.set_visible(false);
            let (retiring_progress, retirement) = retired_session
                .take_viewer_gpu_generation_retirement()
                .map_err(|error| -> Box<dyn std::error::Error> { error.to_string().into() })?;
            let shutdown = retiring_progress.retire_device_generation_until(retirement, deadline);
            let sealed = seal_clean_viewer_gpu_shutdown(shutdown)?;
            publish_active_window_session(host, active_session, bounds);
            Ok::<_, Box<dyn std::error::Error>>(sealed)
        },
    )?;
    session.window.set_visible(true);
    session.window.request_redraw();
    *device = next_device;
    *queue = next_queue;
    drop(retired_session);
    Ok(AppUiSurfaceDeviceReopenTransition {
        surface_generation_before,
        surface_generation_after,
        device_generation_before,
        device_generation_after,
        shutdown_receipt_json,
        shutdown_receipt_sha256,
    })
}

#[cfg(feature = "validation")]
fn publish_surface_reopen_validation_failure(
    validation: &AppUiSurfaceDeviceReopenValidation,
    error: impl Into<String>,
) {
    let error = error.into();
    match validation.result.lock() {
        Ok(mut result) if result.is_none() => *result = Some(Err(error)),
        Ok(_) => {}
        Err(_) => tracing::error!("Surface/device reopen result lock poisoned"),
    }
}

#[cfg(feature = "validation")]
#[allow(clippy::too_many_arguments)]
fn advance_surface_device_reopen_validation(
    validation: &mut AppUiSurfaceDeviceReopenValidation,
    frame_result: crate::app_ui::rendering::AppUiFrameResult,
    elwt: &winit::event_loop::ActiveEventLoop,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    device: &mut wgpu::Device,
    queue: &mut wgpu::Queue,
    host: &mut AppUiHost,
    session: &mut AppUiWindowSession,
    event_proxy: &winit::event_loop::EventLoopProxy<AppUiUserEvent>,
) -> Result<bool, Box<dyn std::error::Error>> {
    if Instant::now() >= validation.deadline {
        return Err("Surface/device reopen validation deadline elapsed".into());
    }
    let Some(picture) = exact_presented_surface_picture(host, session, frame_result) else {
        return Ok(false);
    };
    let phase = std::mem::replace(
        &mut validation.state,
        AppUiSurfaceDeviceReopenValidationState::Complete,
    );
    match phase {
        AppUiSurfaceDeviceReopenValidationState::AwaitingOriginalPicture(request) => {
            let sequence_binding_sha256 = {
                let app = host.app_state();
                crate::app::endurance_playback::current_sequence_binding_sha256(&app)
                    .ok_or("Surface/device reopen requires one active Sequence")?
            };
            let transition = reopen_window_surface_and_device(
                elwt,
                instance,
                adapter,
                device,
                queue,
                host,
                session,
                event_proxy,
                validation.deadline,
            )?;
            validation.state = AppUiSurfaceDeviceReopenValidationState::AwaitingReopenedPicture(
                Box::new(AppUiSurfaceDeviceReopenAwaitingPicture {
                    request,
                    sequence_binding_sha256,
                    original_picture: picture,
                    surface_generation_before: transition.surface_generation_before,
                    surface_generation_after: transition.surface_generation_after,
                    device_generation_before: transition.device_generation_before,
                    device_generation_after: transition.device_generation_after,
                    shutdown_receipt_json: transition.shutdown_receipt_json,
                    shutdown_receipt_sha256: transition.shutdown_receipt_sha256,
                }),
            );
            Ok(false)
        }
        AppUiSurfaceDeviceReopenValidationState::AwaitingReopenedPicture(awaiting) => {
            let AppUiSurfaceDeviceReopenAwaitingPicture {
                request,
                sequence_binding_sha256,
                original_picture,
                surface_generation_before,
                surface_generation_after,
                device_generation_before,
                device_generation_after,
                shutdown_receipt_json,
                shutdown_receipt_sha256,
            } = *awaiting;
            let current_binding = {
                let app = host.app_state();
                crate::app::endurance_playback::current_sequence_binding_sha256(&app)
                    .ok_or("reopened Surface lost its active Sequence binding")?
            };
            let current_surface_generation = session.surface_generation_id.get();
            let current_device_generation = session
                .viewer_gpu_device_progress
                .generation_id()
                .ok_or("reopened Surface lost its device generation owner")?
                .get();
            if current_binding != sequence_binding_sha256
                || picture != original_picture
                || current_surface_generation != surface_generation_after
                || current_device_generation != device_generation_after
            {
                return Err(
                    "reopened Surface did not present the exact original Timeline picture contract"
                        .into(),
                );
            }
            let (_, original_picture_sha256) = canonical_json_and_sha256(&original_picture)?;
            let (reopened_picture_json, reopened_picture_sha256) =
                canonical_json_and_sha256(&picture)?;
            if original_picture_sha256 != reopened_picture_sha256 {
                return Err(
                    "reopened Surface picture digest differs from the original presentation".into(),
                );
            }
            let (reopened_contract_json, reopened_contract_sha256) =
                canonical_json_and_sha256(&AppUiReopenedSurfaceContract {
                    schema_version: 2,
                    surface_generation: surface_generation_after,
                    device_generation: device_generation_after,
                    actual_surface_presented: true,
                    original_picture_sha256: &original_picture_sha256,
                    reopened_picture_json: &reopened_picture_json,
                    reopened_picture_sha256: &reopened_picture_sha256,
                })?;
            let receipt = EnduranceRecoveryOperationReceipt::from_surface_device_reopen_facts(
                SurfaceDeviceReopenFacts {
                    cycle_index: request.cycle_index,
                    operation_id: request.operation_id,
                    sequence_binding_sha256,
                    surface_generation_before,
                    surface_generation_after,
                    device_generation_before,
                    device_generation_after,
                    shutdown_receipt_json,
                    shutdown_receipt_sha256,
                    reopened_contract_json,
                    reopened_contract_sha256,
                },
            )?;
            let mut result = validation
                .result
                .lock()
                .map_err(|_| "Surface/device reopen result lock poisoned")?;
            if result.is_some() {
                return Err("Surface/device reopen validation produced duplicate evidence".into());
            }
            *result = Some(Ok(receipt));
            validation.state = AppUiSurfaceDeviceReopenValidationState::Complete;
            Ok(true)
        }
        AppUiSurfaceDeviceReopenValidationState::Complete => {
            Err("Surface/device reopen validation advanced after completion".into())
        }
    }
}

/// Run the app UI Mondrian editor window.
pub fn run_app_ui() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "validation")]
    return run_app_ui_with_initial_state(AppState::new(), None, None);
    #[cfg(not(feature = "validation"))]
    run_app_ui_with_initial_state(AppState::new())
}

/// Process-local reusable desktop event-loop owner for endurance validation.
///
/// Winit permits only one event-loop owner per process on the supported
/// desktop platforms. Each validation run is an orthogonal Window/GPU session;
/// no Window or device owner crosses a return from `run_on_demand`.
#[cfg(feature = "validation")]
pub(crate) struct AppUiReusableEventLoop {
    event_loop: winit::event_loop::EventLoop<AppUiUserEvent>,
}

#[cfg(feature = "validation")]
impl AppUiReusableEventLoop {
    pub(crate) fn new() -> Result<Self, AppUiEventLoopConstructionFailure> {
        Ok(Self {
            event_loop: winit::event_loop::EventLoop::<AppUiUserEvent>::with_user_event()
                .build()
                .map_err(AppUiEventLoopConstructionFailure::from)?,
        })
    }

    fn event_loop_mut(&mut self) -> &mut winit::event_loop::EventLoop<AppUiUserEvent> {
        &mut self.event_loop
    }

    fn shutdown(self) -> AppUiEventLoopShutdownEvidence {
        let Self { event_loop } = self;
        drop(event_loop);
        AppUiEventLoopShutdownEvidence::after_owner_drop()
    }

    pub(crate) fn reopen_surface_device_with_pump(
        &mut self,
        initial_state: AppState,
        recovery_pump: crate::app::endurance_product_runtime::EnduranceSurfaceRecoveryPump,
        cycle_index: u32,
        operation_id: String,
        timeout: Duration,
    ) -> AppUiSurfaceDeviceReopenRun {
        run_app_ui_surface_device_reopen_validation_returning_state_inner(
            self.event_loop_mut(),
            initial_state,
            Some(recovery_pump),
            cycle_index,
            operation_id,
            timeout,
        )
    }
}

#[cfg(feature = "validation")]
/// Run one real Window/Surface and Device reopen over an already-open Project.
///
/// The event loop exits only after the fresh generation presents the exact
/// original Timeline picture or after a fail-closed timeout/error.
pub fn run_app_ui_surface_device_reopen_validation(
    initial_state: AppState,
    cycle_index: u32,
    operation_id: String,
    timeout: Duration,
) -> Result<AppUiSurfaceDeviceReopenValidationBatch, AppUiSurfaceDeviceReopenValidationError> {
    run_app_ui_surface_device_reopen_validation_batch(
        initial_state,
        vec![AppUiSurfaceDeviceReopenValidationRequest { cycle_index, operation_id, timeout }],
    )
}

/// One operation in a same-process Surface/Device validation batch.
#[cfg(feature = "validation")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUiSurfaceDeviceReopenValidationRequest {
    /// Recovery-cycle identity bound into the sealed receipt.
    pub cycle_index: u32,
    /// Unique operation identity bound into the sealed receipt.
    pub operation_id: String,
    /// Absolute wall-clock budget for this Window session.
    pub timeout: Duration,
}

#[cfg(feature = "validation")]
struct AppUiPreparedSurfaceDeviceReopenValidationRequest {
    cycle_index: u32,
    operation_id: String,
    timeout: Duration,
}

/// Mutually exclusive terminal class for a Surface/Device validation batch.
#[cfg(feature = "validation")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppUiSurfaceDeviceReopenValidationFailureKind {
    /// No operation was supplied.
    EmptyBatch,
    /// The bounded operation count was exceeded.
    TooManyOperations,
    /// A cycle/operation identity was invalid or replayed within the batch.
    InvalidOrReplayedIdentity,
    /// At least one operation supplied a zero timeout.
    ZeroTimeout,
    /// At least one absolute operation deadline could not be represented.
    DeadlineOverflow,
    /// No process-local event-loop owner could be constructed.
    EventLoopConstruction(AppUiEventLoopConstructionFailureKind),
    /// A per-operation absolute deadline could not be frozen before Window entry.
    OperationDeadlineOverflow,
    /// One Window operation failed after the event loop existed.
    WindowOperation,
    /// Operations succeeded but consuming App shutdown was incomplete.
    AppShutdownIncomplete,
}

/// Successful batch outcome retaining every final owner handback receipt.
#[cfg(feature = "validation")]
#[derive(Debug)]
pub struct AppUiSurfaceDeviceReopenValidationBatch {
    receipts: Vec<AppUiWindowRunReceipt>,
    event_loop_shutdown: AppUiEventLoopShutdownEvidence,
    app_shutdown: AppEnduranceShutdownEvidence,
}

#[cfg(feature = "validation")]
impl AppUiSurfaceDeviceReopenValidationBatch {
    /// Successful per-Window receipts in request order.
    pub fn receipts(&self) -> &[AppUiWindowRunReceipt] {
        &self.receipts
    }

    /// Event-loop Rust-owner handback evidence.
    pub const fn event_loop_shutdown(&self) -> AppUiEventLoopShutdownEvidence {
        self.event_loop_shutdown
    }

    /// Seal the EventLoop Rust-owner handback for durable batch evidence.
    pub fn event_loop_shutdown_receipt(
        &self,
    ) -> Result<AppUiEventLoopShutdownReceipt, AppUiEventLoopShutdownReceiptError> {
        AppUiEventLoopShutdownReceipt::seal(self.event_loop_shutdown)
    }

    /// Exact final consuming App shutdown evidence.
    pub const fn app_shutdown(&self) -> &AppEnduranceShutdownEvidence {
        &self.app_shutdown
    }

    /// Whether EventLoop and App ownership both returned cleanly.
    pub fn all_returned_authority_released(&self) -> bool {
        self.event_loop_shutdown.rust_owner_released() && self.app_shutdown.all_resources_released()
    }
}

#[cfg(feature = "validation")]
#[derive(Debug)]
enum AppUiSurfaceDeviceReopenRequestFailureKind {
    EmptyBatch,
    TooManyOperations,
    InvalidOrReplayedIdentity,
    ZeroTimeout,
    DeadlineOverflow,
}

#[cfg(feature = "validation")]
#[derive(Debug)]
enum AppUiSurfaceDeviceReopenPrimaryFailure {
    Request {
        kind: AppUiSurfaceDeviceReopenRequestFailureKind,
        diagnostic: String,
    },
    EventLoopConstruction {
        failure: AppUiEventLoopConstructionFailure,
    },
    OperationAdmission {
        cycle_index: u32,
        operation_id: String,
        diagnostic: String,
        event_loop_shutdown: AppUiEventLoopShutdownEvidence,
    },
    WindowOperation {
        cycle_index: u32,
        operation_id: String,
        diagnostic: String,
        window_shutdown: Option<Box<AppUiWindowClosedEvidence>>,
        event_loop_shutdown: AppUiEventLoopShutdownEvidence,
    },
    AppShutdownIncomplete {
        diagnostic: String,
        event_loop_shutdown: AppUiEventLoopShutdownEvidence,
    },
}

#[cfg(feature = "validation")]
impl AppUiSurfaceDeviceReopenPrimaryFailure {
    fn request(
        kind: AppUiSurfaceDeviceReopenRequestFailureKind,
        diagnostic: impl Into<String>,
    ) -> Self {
        Self::Request { kind, diagnostic: diagnostic.into() }
    }
}

/// Typed terminal failure retaining completed work and every available owner receipt.
#[cfg(feature = "validation")]
#[derive(Debug)]
pub struct AppUiSurfaceDeviceReopenValidationError {
    primary: Box<AppUiSurfaceDeviceReopenPrimaryFailure>,
    completed_receipts: Vec<AppUiWindowRunReceipt>,
    app_shutdown: Box<AppEnduranceShutdownEvidence>,
    cleanup_diagnostic: Option<String>,
}

#[cfg(feature = "validation")]
impl AppUiSurfaceDeviceReopenValidationError {
    /// Stable primary failure class; App cleanup never replaces it.
    pub fn kind(&self) -> AppUiSurfaceDeviceReopenValidationFailureKind {
        match self.primary.as_ref() {
            AppUiSurfaceDeviceReopenPrimaryFailure::Request { kind, .. } => match kind {
                AppUiSurfaceDeviceReopenRequestFailureKind::EmptyBatch => {
                    AppUiSurfaceDeviceReopenValidationFailureKind::EmptyBatch
                }
                AppUiSurfaceDeviceReopenRequestFailureKind::TooManyOperations => {
                    AppUiSurfaceDeviceReopenValidationFailureKind::TooManyOperations
                }
                AppUiSurfaceDeviceReopenRequestFailureKind::InvalidOrReplayedIdentity => {
                    AppUiSurfaceDeviceReopenValidationFailureKind::InvalidOrReplayedIdentity
                }
                AppUiSurfaceDeviceReopenRequestFailureKind::ZeroTimeout => {
                    AppUiSurfaceDeviceReopenValidationFailureKind::ZeroTimeout
                }
                AppUiSurfaceDeviceReopenRequestFailureKind::DeadlineOverflow => {
                    AppUiSurfaceDeviceReopenValidationFailureKind::DeadlineOverflow
                }
            },
            AppUiSurfaceDeviceReopenPrimaryFailure::EventLoopConstruction { failure } => {
                AppUiSurfaceDeviceReopenValidationFailureKind::EventLoopConstruction(failure.kind())
            }
            AppUiSurfaceDeviceReopenPrimaryFailure::OperationAdmission { .. } => {
                AppUiSurfaceDeviceReopenValidationFailureKind::OperationDeadlineOverflow
            }
            AppUiSurfaceDeviceReopenPrimaryFailure::WindowOperation { .. } => {
                AppUiSurfaceDeviceReopenValidationFailureKind::WindowOperation
            }
            AppUiSurfaceDeviceReopenPrimaryFailure::AppShutdownIncomplete { .. } => {
                AppUiSurfaceDeviceReopenValidationFailureKind::AppShutdownIncomplete
            }
        }
    }

    /// Receipts completed before the terminal failure.
    pub fn completed_receipts(&self) -> &[AppUiWindowRunReceipt] {
        &self.completed_receipts
    }

    /// Failed Window cycle identity, when a Window operation started.
    pub fn failed_cycle_index(&self) -> Option<u32> {
        match self.primary.as_ref() {
            AppUiSurfaceDeviceReopenPrimaryFailure::OperationAdmission { cycle_index, .. }
            | AppUiSurfaceDeviceReopenPrimaryFailure::WindowOperation { cycle_index, .. } => {
                Some(*cycle_index)
            }
            _ => None,
        }
    }

    /// Failed Window operation identity, when a Window operation started.
    pub fn failed_operation_id(&self) -> Option<&str> {
        match self.primary.as_ref() {
            AppUiSurfaceDeviceReopenPrimaryFailure::OperationAdmission { operation_id, .. }
            | AppUiSurfaceDeviceReopenPrimaryFailure::WindowOperation { operation_id, .. } => {
                Some(operation_id)
            }
            _ => None,
        }
    }

    /// Original primary diagnostic before any App cleanup annotation.
    pub fn primary_diagnostic(&self) -> &str {
        match self.primary.as_ref() {
            AppUiSurfaceDeviceReopenPrimaryFailure::Request { diagnostic, .. }
            | AppUiSurfaceDeviceReopenPrimaryFailure::OperationAdmission { diagnostic, .. }
            | AppUiSurfaceDeviceReopenPrimaryFailure::WindowOperation { diagnostic, .. }
            | AppUiSurfaceDeviceReopenPrimaryFailure::AppShutdownIncomplete {
                diagnostic, ..
            } => diagnostic,
            AppUiSurfaceDeviceReopenPrimaryFailure::EventLoopConstruction { failure } => {
                failure.diagnostic()
            }
        }
    }

    /// Additional App cleanup diagnostic, if cleanup failed after a primary error.
    pub fn cleanup_diagnostic(&self) -> Option<&str> {
        self.cleanup_diagnostic.as_deref()
    }

    /// Whether exact outer Window shutdown evidence was returned.
    pub fn window_shutdown(&self) -> Option<&AppUiWindowClosedEvidence> {
        match self.primary.as_ref() {
            AppUiSurfaceDeviceReopenPrimaryFailure::WindowOperation { window_shutdown, .. } => {
                window_shutdown.as_deref()
            }
            _ => None,
        }
    }

    /// Event-loop shutdown evidence, present only after successful construction.
    pub fn event_loop_shutdown(&self) -> Option<AppUiEventLoopShutdownEvidence> {
        match self.primary.as_ref() {
            AppUiSurfaceDeviceReopenPrimaryFailure::WindowOperation {
                event_loop_shutdown, ..
            }
            | AppUiSurfaceDeviceReopenPrimaryFailure::OperationAdmission {
                event_loop_shutdown,
                ..
            }
            | AppUiSurfaceDeviceReopenPrimaryFailure::AppShutdownIncomplete {
                event_loop_shutdown,
                ..
            } => Some(*event_loop_shutdown),
            AppUiSurfaceDeviceReopenPrimaryFailure::Request { .. }
            | AppUiSurfaceDeviceReopenPrimaryFailure::EventLoopConstruction { .. } => None,
        }
    }

    /// Seal EventLoop handback evidence when construction succeeded.
    pub fn event_loop_shutdown_receipt(
        &self,
    ) -> Result<Option<AppUiEventLoopShutdownReceipt>, AppUiEventLoopShutdownReceiptError> {
        self.event_loop_shutdown().map(AppUiEventLoopShutdownReceipt::seal).transpose()
    }

    /// Seal exact outer Window closure evidence when a Window run returned it.
    pub fn window_shutdown_receipt(
        &self,
    ) -> Result<Option<AppUiWindowClosedReceipt>, AppUiWindowClosedReceiptError> {
        self.window_shutdown().map(AppUiWindowClosedEvidence::seal_receipt).transpose()
    }

    /// Exact final consuming App shutdown evidence.
    pub fn app_shutdown(&self) -> &AppEnduranceShutdownEvidence {
        &self.app_shutdown
    }

    /// Whether all authority applicable to this failure class returned cleanly.
    pub fn all_returned_authority_released(&self) -> bool {
        let primary_released = match self.primary.as_ref() {
            AppUiSurfaceDeviceReopenPrimaryFailure::Request { .. }
            | AppUiSurfaceDeviceReopenPrimaryFailure::EventLoopConstruction { .. } => true,
            AppUiSurfaceDeviceReopenPrimaryFailure::OperationAdmission {
                event_loop_shutdown,
                ..
            } => event_loop_shutdown.rust_owner_released(),
            AppUiSurfaceDeviceReopenPrimaryFailure::WindowOperation {
                window_shutdown,
                event_loop_shutdown,
                ..
            } => {
                event_loop_shutdown.rust_owner_released()
                    && window_shutdown
                        .as_deref()
                        .is_some_and(AppUiWindowClosedEvidence::all_owned_authority_released)
            }
            AppUiSurfaceDeviceReopenPrimaryFailure::AppShutdownIncomplete {
                event_loop_shutdown,
                ..
            } => event_loop_shutdown.rust_owner_released(),
        };
        primary_released
            && self.app_shutdown.all_resources_released()
            && self.cleanup_diagnostic.is_none()
    }
}

#[cfg(feature = "validation")]
impl std::fmt::Display for AppUiSurfaceDeviceReopenValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.primary.as_ref() {
            AppUiSurfaceDeviceReopenPrimaryFailure::EventLoopConstruction { failure } => {
                write!(
                    formatter,
                    "could not create the Surface validation event loop: {failure}"
                )?;
            }
            _ => formatter.write_str(self.primary_diagnostic())?,
        }
        if let Some(cleanup) = &self.cleanup_diagnostic {
            write!(formatter, "; consuming App cleanup also failed: {cleanup}")?;
        }
        Ok(())
    }
}

#[cfg(feature = "validation")]
impl std::error::Error for AppUiSurfaceDeviceReopenValidationError {}

/// Run orthogonal Surface/Device reopen sessions on one process-local event loop.
///
/// The same App owner crosses every operation, while each Window, Surface,
/// Device, Queue, and callback session is destroyed before the next on-demand
/// event-loop run. The App is consumed by a bounded endurance shutdown before
/// this function returns.
#[cfg(feature = "validation")]
pub fn run_app_ui_surface_device_reopen_validation_batch(
    initial_state: AppState,
    requests: Vec<AppUiSurfaceDeviceReopenValidationRequest>,
) -> Result<AppUiSurfaceDeviceReopenValidationBatch, AppUiSurfaceDeviceReopenValidationError> {
    run_app_ui_surface_device_reopen_validation_batch_with_factory(
        initial_state,
        requests,
        AppUiReusableEventLoop::new,
    )
}

#[cfg(feature = "validation")]
fn run_app_ui_surface_device_reopen_validation_batch_with_factory(
    initial_state: AppState,
    requests: Vec<AppUiSurfaceDeviceReopenValidationRequest>,
    event_loop_factory: impl FnOnce()
        -> Result<AppUiReusableEventLoop, AppUiEventLoopConstructionFailure>,
) -> Result<AppUiSurfaceDeviceReopenValidationBatch, AppUiSurfaceDeviceReopenValidationError> {
    let mut state = initial_state;
    if requests.is_empty() {
        return finish_surface_device_validation(
            state,
            Vec::new(),
            Err(AppUiSurfaceDeviceReopenPrimaryFailure::request(
                AppUiSurfaceDeviceReopenRequestFailureKind::EmptyBatch,
                "Surface/device validation batch must not be empty",
            )),
        );
    }
    if requests.len() > MAXIMUM_SURFACE_REOPEN_VALIDATION_BATCH_CYCLES {
        return finish_surface_device_validation(
            state,
            Vec::new(),
            Err(AppUiSurfaceDeviceReopenPrimaryFailure::request(
                AppUiSurfaceDeviceReopenRequestFailureKind::TooManyOperations,
                format!(
                    "Surface/device validation batch exceeds {} operations",
                    MAXIMUM_SURFACE_REOPEN_VALIDATION_BATCH_CYCLES
                ),
            )),
        );
    }
    let unique_cycles = requests
        .iter()
        .map(|request| request.cycle_index)
        .collect::<std::collections::BTreeSet<_>>();
    let unique_operations = requests
        .iter()
        .map(|request| request.operation_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if unique_cycles.len() != requests.len()
        || unique_operations.len() != requests.len()
        || requests
            .iter()
            .any(|request| !valid_surface_validation_operation_id(&request.operation_id))
    {
        return finish_surface_device_validation(
            state,
            Vec::new(),
            Err(AppUiSurfaceDeviceReopenPrimaryFailure::request(
                AppUiSurfaceDeviceReopenRequestFailureKind::InvalidOrReplayedIdentity,
                "Surface/device validation batch identities are invalid or replayed",
            )),
        );
    }
    if requests.iter().any(|request| request.timeout.is_zero()) {
        return finish_surface_device_validation(
            state,
            Vec::new(),
            Err(AppUiSurfaceDeviceReopenPrimaryFailure::request(
                AppUiSurfaceDeviceReopenRequestFailureKind::ZeroTimeout,
                "Surface/device reopen validation timeout must be nonzero",
            )),
        );
    }
    let deadline_probe = Instant::now();
    if requests
        .iter()
        .any(|request| deadline_probe.checked_add(request.timeout).is_none())
    {
        return finish_surface_device_validation(
            state,
            Vec::new(),
            Err(AppUiSurfaceDeviceReopenPrimaryFailure::request(
                AppUiSurfaceDeviceReopenRequestFailureKind::DeadlineOverflow,
                "Surface/device reopen validation deadline overflow",
            )),
        );
    }
    let mut prepared_requests = Vec::with_capacity(requests.len());
    for request in requests {
        prepared_requests.push(AppUiPreparedSurfaceDeviceReopenValidationRequest {
            cycle_index: request.cycle_index,
            operation_id: request.operation_id,
            timeout: request.timeout,
        });
    }
    let mut event_loop = match event_loop_factory() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            return finish_surface_device_validation(
                state,
                Vec::new(),
                Err(
                    AppUiSurfaceDeviceReopenPrimaryFailure::EventLoopConstruction {
                        failure: error,
                    },
                ),
            );
        }
    };
    let mut receipts = Vec::with_capacity(prepared_requests.len());
    for request in prepared_requests {
        let failed_cycle_index = request.cycle_index;
        let failed_operation_id = request.operation_id.clone();
        let Some(deadline) = Instant::now().checked_add(request.timeout) else {
            let event_loop_shutdown = event_loop.shutdown();
            return finish_surface_device_validation(
                state,
                receipts,
                Err(AppUiSurfaceDeviceReopenPrimaryFailure::OperationAdmission {
                    cycle_index: failed_cycle_index,
                    operation_id: failed_operation_id,
                    diagnostic: "Surface/device reopen operation deadline overflow".to_owned(),
                    event_loop_shutdown,
                }),
            );
        };
        let run = run_app_ui_surface_device_reopen_validation_returning_state_with_deadline(
            event_loop.event_loop_mut(),
            state,
            None,
            request.cycle_index,
            request.operation_id,
            deadline,
        );
        state = run.app_state;
        match run.result {
            Ok(receipt) => receipts.push(receipt),
            Err(error) => {
                let event_loop_shutdown = event_loop.shutdown();
                return finish_surface_device_validation(
                    state,
                    receipts,
                    Err(AppUiSurfaceDeviceReopenPrimaryFailure::WindowOperation {
                        cycle_index: failed_cycle_index,
                        operation_id: failed_operation_id,
                        diagnostic: error,
                        window_shutdown: run.shutdown.map(Box::new),
                        event_loop_shutdown,
                    }),
                );
            }
        }
    }
    let event_loop_shutdown = event_loop.shutdown();
    finish_surface_device_validation(state, receipts, Ok(event_loop_shutdown))
}

#[cfg(feature = "validation")]
fn valid_surface_validation_operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(feature = "validation")]
fn finish_surface_device_validation(
    app_state: AppState,
    completed_receipts: Vec<AppUiWindowRunReceipt>,
    result: Result<AppUiEventLoopShutdownEvidence, AppUiSurfaceDeviceReopenPrimaryFailure>,
) -> Result<AppUiSurfaceDeviceReopenValidationBatch, AppUiSurfaceDeviceReopenValidationError> {
    let (shutdown_deadline, deadline_failure) =
        match Instant::now().checked_add(Duration::from_secs(30)) {
            Some(deadline) => (deadline, None),
            None => (
                Instant::now(),
                Some("Surface/device validation App shutdown deadline overflow".to_owned()),
            ),
        };
    let shutdown = app_state.shutdown_for_endurance(shutdown_deadline);
    let cleanup_failure = deadline_failure.or_else(|| {
        (!shutdown.all_resources_released())
            .then(|| format!("Surface/device validation App shutdown was incomplete: {shutdown:?}"))
    });
    match result {
        Ok(event_loop_shutdown) if cleanup_failure.is_none() => {
            Ok(AppUiSurfaceDeviceReopenValidationBatch {
                receipts: completed_receipts,
                event_loop_shutdown,
                app_shutdown: shutdown,
            })
        }
        Ok(event_loop_shutdown) => Err(AppUiSurfaceDeviceReopenValidationError {
            primary: Box::new(
                AppUiSurfaceDeviceReopenPrimaryFailure::AppShutdownIncomplete {
                    diagnostic: cleanup_failure
                        .clone()
                        .expect("incomplete App shutdown has a diagnostic"),
                    event_loop_shutdown,
                },
            ),
            completed_receipts,
            app_shutdown: Box::new(shutdown),
            cleanup_diagnostic: None,
        }),
        Err(primary) => Err(AppUiSurfaceDeviceReopenValidationError {
            primary: Box::new(primary),
            completed_receipts,
            app_shutdown: Box::new(shutdown),
            cleanup_diagnostic: cleanup_failure,
        }),
    }
}

#[cfg(feature = "validation")]
pub(crate) struct AppUiSurfaceDeviceReopenRun {
    pub(crate) app_state: AppState,
    pub(crate) result: Result<AppUiWindowRunReceipt, String>,
    pub(crate) shutdown: Option<AppUiWindowClosedEvidence>,
    pub(crate) recovery_pump:
        Option<crate::app::endurance_product_runtime::EnduranceSurfaceRecoveryPump>,
}

#[cfg(all(feature = "validation", test))]
pub(crate) fn run_app_ui_surface_device_reopen_validation_returning_state(
    initial_state: AppState,
    cycle_index: u32,
    operation_id: String,
    timeout: Duration,
) -> AppUiSurfaceDeviceReopenRun {
    if timeout.is_zero() {
        return AppUiSurfaceDeviceReopenRun {
            app_state: initial_state,
            result: Err("Surface/device reopen validation timeout must be nonzero".to_owned()),
            shutdown: None,
            recovery_pump: None,
        };
    }
    if Instant::now().checked_add(timeout).is_none() {
        return AppUiSurfaceDeviceReopenRun {
            app_state: initial_state,
            result: Err("Surface/device reopen validation deadline overflow".to_owned()),
            shutdown: None,
            recovery_pump: None,
        };
    }
    let mut event_loop = match AppUiReusableEventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            return AppUiSurfaceDeviceReopenRun {
                app_state: initial_state,
                result: Err(format!(
                    "could not create the Surface validation event loop: {error}"
                )),
                shutdown: None,
                recovery_pump: None,
            };
        }
    };
    run_app_ui_surface_device_reopen_validation_returning_state_inner(
        event_loop.event_loop_mut(),
        initial_state,
        None,
        cycle_index,
        operation_id,
        timeout,
    )
}

#[cfg(feature = "validation")]
fn run_app_ui_surface_device_reopen_validation_returning_state_inner(
    event_loop: &mut winit::event_loop::EventLoop<AppUiUserEvent>,
    initial_state: AppState,
    recovery_pump: Option<crate::app::endurance_product_runtime::EnduranceSurfaceRecoveryPump>,
    cycle_index: u32,
    operation_id: String,
    timeout: Duration,
) -> AppUiSurfaceDeviceReopenRun {
    if timeout.is_zero() {
        return AppUiSurfaceDeviceReopenRun {
            app_state: initial_state,
            result: Err("Surface/device reopen validation timeout must be nonzero".to_owned()),
            shutdown: None,
            recovery_pump,
        };
    }
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return AppUiSurfaceDeviceReopenRun {
            app_state: initial_state,
            result: Err("Surface/device reopen validation deadline overflow".to_owned()),
            shutdown: None,
            recovery_pump,
        };
    };
    run_app_ui_surface_device_reopen_validation_returning_state_with_deadline(
        event_loop,
        initial_state,
        recovery_pump,
        cycle_index,
        operation_id,
        deadline,
    )
}

#[cfg(feature = "validation")]
fn run_app_ui_surface_device_reopen_validation_returning_state_with_deadline(
    event_loop: &mut winit::event_loop::EventLoop<AppUiUserEvent>,
    initial_state: AppState,
    recovery_pump: Option<crate::app::endurance_product_runtime::EnduranceSurfaceRecoveryPump>,
    cycle_index: u32,
    operation_id: String,
    deadline: Instant,
) -> AppUiSurfaceDeviceReopenRun {
    let result = Arc::new(Mutex::new(None));
    let returned_state: AppUiValidationReturnSlot = Rc::new(RefCell::new(None));
    let recovery_pump_return = Rc::new(RefCell::new(None));
    let validation = AppUiSurfaceDeviceReopenValidation {
        state: AppUiSurfaceDeviceReopenValidationState::AwaitingOriginalPicture(
            AppUiSurfaceDeviceReopenRequest { cycle_index, operation_id },
        ),
        result: Arc::clone(&result),
        deadline,
        recovery_pump,
        recovery_pump_return: Rc::clone(&recovery_pump_return),
    };
    let ui_result = run_app_ui_with_initial_state_on_event_loop(
        event_loop,
        initial_state,
        Some(validation),
        Some(Rc::clone(&returned_state)),
    );
    let returned = returned_state.borrow_mut().take();
    let recovery_pump = recovery_pump_return.borrow_mut().take();
    let AppUiValidationReturnedState { app_state, shutdown } = returned
        .unwrap_or_else(|| panic!("validation Window exited without returning its AppState owner"));
    let native = AppUiWindowNativeReturnEvidence::after_window_function_return();
    let (shutdown, outer_failure) = match shutdown {
        AppUiValidationWindowShutdown::PendingActive { runtime, host, gpu } => (
            Some(AppUiWindowOuterShutdownEvidence::active_exited(
                runtime, host, gpu, native,
            )),
            None,
        ),
        AppUiValidationWindowShutdown::PendingActivePublicationFailure { runtime, host, gpu } => (
            Some(AppUiWindowOuterShutdownEvidence::active_publication_failed(
                runtime, host, gpu, native,
            )),
            None,
        ),
        AppUiValidationWindowShutdown::Complete(evidence) => (Some(evidence), None),
        AppUiValidationWindowShutdown::PendingHostActive { .. } => (
            None,
            Some("Window validation returned before Runtime shutdown evidence".to_owned()),
        ),
        AppUiValidationWindowShutdown::IncompleteHostFallback { host } => (
            None,
            Some(format!(
                "Window validation used incomplete Host fallback shutdown: {host:?}"
            )),
        ),
    };
    let outer_failure = outer_failure.or_else(|| {
        shutdown.as_ref().and_then(|evidence| {
            (!evidence.all_owned_authority_released())
                .then(|| format!("Window validation owner shutdown did not qualify: {evidence:?}"))
        })
    });
    let published = match result.lock() {
        Ok(mut receipt) => receipt.take(),
        Err(_) => Some(Err("Surface/device reopen result lock poisoned".to_owned())),
    };
    let operation_result = merge_window_operation_result(
        published,
        ui_result.map_err(|error| error.to_string()),
        outer_failure,
    );
    let operation_result = operation_result.and_then(|recovery| {
        let evidence = shutdown
            .clone()
            .ok_or_else(|| "Window validation returned no complete outer evidence".to_owned())?;
        AppUiWindowRunReceipt::seal_active(recovery, evidence)
            .map_err(|error| format!("could not seal Window run receipt: {error}"))
    });
    AppUiSurfaceDeviceReopenRun {
        app_state,
        result: operation_result,
        shutdown: shutdown.map(AppUiWindowClosedEvidence::new),
        recovery_pump,
    }
}

/// Preserve a published operation failure independently of loop and cleanup failures.
#[cfg(feature = "validation")]
fn merge_window_operation_result<T>(
    published: Option<Result<T, String>>,
    window: Result<(), String>,
    cleanup: Option<String>,
) -> Result<T, String> {
    let result = match (published, window) {
        (Some(Err(primary)), Err(window)) => {
            Err(format!("{primary}; Window execution also failed: {window}"))
        }
        (_, Err(window)) => Err(window),
        (Some(result), Ok(())) => result,
        (None, Ok(())) => {
            Err("Surface/device reopen validation exited without a receipt".to_owned())
        }
    };
    match (result, cleanup) {
        (Err(primary), Some(cleanup)) => Err(format!(
            "{primary}; Window owner cleanup also failed: {cleanup}"
        )),
        (Ok(_), Some(cleanup)) => Err(cleanup),
        (result, None) => result,
    }
}

fn merge_window_cleanup_failures(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(first), Some(second)) => Some(format!("{first}; {second}")),
        (Some(failure), None) | (None, Some(failure)) => Some(failure),
        (None, None) => None,
    }
}

fn with_window_candidate_installed<T, R>(
    active: &mut T,
    candidate: &mut T,
    operation: impl FnOnce(&T, &mut T) -> R,
) -> R {
    std::mem::swap(active, candidate);
    operation(active, candidate)
}

#[derive(Debug, thiserror::Error)]
enum AppUiEventLoopFailure {
    #[error("App UI event loop failed: {0}")]
    Execution(String),
    #[error("{0}")]
    Panicked(String),
    #[error("{primary}; Window callback also failed: {callback}")]
    Multiple {
        primary: Box<AppUiEventLoopFailure>,
        callback: String,
    },
}

fn catch_app_ui_event_loop(
    run: impl FnOnce() -> Result<(), winit::error::EventLoopError>,
) -> Result<(), AppUiEventLoopFailure> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(AppUiEventLoopFailure::Execution(error.to_string())),
        Err(payload) => Err(AppUiEventLoopFailure::Panicked(
            crate::app::execution_panic_diagnostic::execution_panic_diagnostic(
                payload,
                "App UI event loop",
            )
            .to_string(),
        )),
    }
}

fn merge_app_ui_event_loop_failure(
    event_loop_result: Result<(), AppUiEventLoopFailure>,
    callback_failure: Option<String>,
) -> Result<(), AppUiEventLoopFailure> {
    match (event_loop_result, callback_failure) {
        (Ok(()), Some(callback)) => Err(AppUiEventLoopFailure::Execution(callback)),
        (Err(primary), Some(callback)) => {
            Err(AppUiEventLoopFailure::Multiple { primary: Box::new(primary), callback })
        }
        (result, None) => result,
    }
}

fn catch_window_viewer_gpu_shutdown(
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
    deadline: Instant,
) -> AppUiActiveWindowGpuShutdownEvidence {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        session.shutdown_viewer_gpu_until(host, deadline)
    })) {
        Ok(evidence) => evidence,
        Err(payload) => {
            let diagnostic = crate::app::execution_panic_diagnostic::execution_panic_diagnostic(
                payload,
                "Window Viewer GPU shutdown",
            );
            AppUiActiveWindowGpuShutdownEvidence {
                publication_cleanup: Ok(()),
                retirement: AppUiWindowGpuRetirementEvidence::ShutdownPanicked(
                    diagnostic.to_string(),
                ),
            }
        }
    }
}

fn run_app_ui_with_initial_state(
    initial_state: AppState,
    #[cfg(feature = "validation")] surface_reopen_validation: Option<
        AppUiSurfaceDeviceReopenValidation,
    >,
    #[cfg(feature = "validation")] validation_return: Option<AppUiValidationReturnSlot>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop =
        winit::event_loop::EventLoop::<AppUiUserEvent>::with_user_event().build()?;
    run_app_ui_with_initial_state_on_event_loop(
        &mut event_loop,
        initial_state,
        #[cfg(feature = "validation")]
        surface_reopen_validation,
        #[cfg(feature = "validation")]
        validation_return,
    )
}

fn run_app_ui_with_initial_state_on_event_loop(
    event_loop: &mut winit::event_loop::EventLoop<AppUiUserEvent>,
    initial_state: AppState,
    #[cfg(feature = "validation")] mut surface_reopen_validation: Option<
        AppUiSurfaceDeviceReopenValidation,
    >,
    #[cfg(feature = "validation")] validation_return: Option<AppUiValidationReturnSlot>,
) -> Result<(), Box<dyn std::error::Error>> {
    let platform = SystemPlatformService;
    let tracing_guard = init_product_tracing(&platform);
    #[cfg(feature = "validation")]
    let validation_shutdown_deadline =
        surface_reopen_validation.as_ref().map(|validation| validation.deadline);
    #[cfg(not(feature = "validation"))]
    let validation_shutdown_deadline: Option<Instant> = None;
    let host_startup_deadline = validation_shutdown_deadline.unwrap_or_else(|| {
        Instant::now().checked_add(Duration::from_secs(5)).unwrap_or_else(Instant::now)
    });
    #[cfg(feature = "validation")]
    let background_runtime = match AppUiBackgroundRuntimeOwner::start(host_startup_deadline) {
        Ok(owner) => owner,
        Err(failure) => {
            let closed = failure.shutdown_until(host_startup_deadline);
            let runtime_shutdown = closed.shutdown;
            let runtime_cleanup_released = runtime_shutdown.all_created_resources_released();
            if let Some(return_slot) = validation_return.as_ref() {
                *return_slot.borrow_mut() = Some(AppUiValidationReturnedState {
                    app_state: initial_state,
                    shutdown: AppUiValidationWindowShutdown::Complete(
                        AppUiWindowOuterShutdownEvidence::runtime_startup_failed(
                            runtime_shutdown.clone(),
                        ),
                    ),
                });
            }
            return Err(format!(
                "{}; Window background runtime startup cleanup_all_created_resources_released={}; cleanup={:?}",
                closed.diagnostic,
                runtime_cleanup_released,
                runtime_shutdown
            )
            .into());
        }
    };
    #[cfg(not(feature = "validation"))]
    let background_runtime = match AppUiBackgroundRuntimeOwner::start(host_startup_deadline) {
        Ok(owner) => owner,
        Err(failure) => {
            let closed = failure.shutdown_until(host_startup_deadline);
            return Err(format!(
                "{}; Window background runtime startup cleanup_all_created_resources_released={}; cleanup={:?}",
                closed.diagnostic,
                closed.shutdown.all_created_resources_released(),
                closed.shutdown
            )
            .into());
        }
    };
    let background_runtime_guard = background_runtime.enter();
    let host = match AppUiHost::try_new(initial_state) {
        Ok(host) => host,
        Err(failure) => {
            let primary = failure.diagnostic().clone();
            let closed = failure.shutdown_until(host_startup_deadline);
            let cleanup_released = closed.shutdown.all_created_resources_released();
            drop(background_runtime_guard);
            let runtime_shutdown = background_runtime.shutdown_until(host_startup_deadline);
            let runtime_cleanup_released = runtime_shutdown.all_created_resources_released();
            #[cfg(feature = "validation")]
            if let Some(return_slot) = validation_return.as_ref() {
                *return_slot.borrow_mut() = Some(AppUiValidationReturnedState {
                    app_state: closed.app_state,
                    shutdown: AppUiValidationWindowShutdown::Complete(
                        AppUiWindowOuterShutdownEvidence::host_startup_failed(
                            runtime_shutdown.clone(),
                            closed.shutdown.clone(),
                        ),
                    ),
                });
            }
            #[cfg(not(feature = "validation"))]
            drop(closed.app_state);
            return Err(format!(
                "{primary}; Window Host startup cleanup_all_created_resources_released={cleanup_released}; cleanup={:?}; Window background runtime cleanup_all_created_resources_released={runtime_cleanup_released}; cleanup={runtime_shutdown:?}",
                closed.shutdown,
            )
            .into());
        }
    };
    let mut host = host;

    tracing::info!("Mondrian app UI starting");

    let initial_candidate = match prepare_initial_window_candidate(
        event_loop,
        &host,
        host_startup_deadline,
    ) {
        Ok(candidate) => candidate,
        Err(failure) => {
            let native_cleanup_released = failure.shutdown.all_created_resources_released();
            let native_cleanup = failure.shutdown;
            let primary = failure.primary;
            let (app_state, ui_shutdown) = host.into_app_state_until(host_startup_deadline);
            let ui_failure = (!ui_shutdown.all_resources_released())
                .then(|| format!("Window UI services did not close cleanly: {ui_shutdown:?}"));
            drop(background_runtime_guard);
            let runtime_shutdown = background_runtime.shutdown_until(host_startup_deadline);
            let runtime_failure = runtime_shutdown.qualification_failure();
            #[cfg(feature = "validation")]
            if let Some(return_slot) = validation_return.as_ref() {
                *return_slot.borrow_mut() = Some(AppUiValidationReturnedState {
                    app_state,
                    shutdown: AppUiValidationWindowShutdown::Complete(
                        AppUiWindowOuterShutdownEvidence::pre_active_failed(
                            runtime_shutdown,
                            ui_shutdown,
                            native_cleanup.clone(),
                        ),
                    ),
                });
            }
            #[cfg(not(feature = "validation"))]
            drop(app_state);
            let cleanup_failure = merge_window_cleanup_failures(ui_failure, runtime_failure);
            let native_context = format!(
                "pre-active Window cleanup_all_created_resources_released={native_cleanup_released}; cleanup={native_cleanup:?}"
            );
            return match cleanup_failure {
                Some(cleanup) => Err(format!("{primary}; {native_context}; {cleanup}").into()),
                None => Err(format!("{primary}; {native_context}").into()),
            };
        }
    };
    let AppUiInitialWindowCandidate {
        preview_work_event_proxy,
        preview_work_watch,
        preview_work_event_pending,
        instance,
        adapter,
        device: initial_device,
        queue: initial_queue,
        mut session,
        session_bounds,
    } = initial_candidate;
    #[cfg(feature = "validation")]
    let (mut device, mut queue) = (initial_device, initial_queue);
    #[cfg(not(feature = "validation"))]
    let (device, queue) = (initial_device, initial_queue);
    let pending_actions = PendingUiActions::default();
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        publish_active_window_session(&mut host, &session, session_bounds);
        let _ = host.set_system_theme_preset(winit_theme_to_theme_preset(session.window.theme()));
        tracing::info!(
            surface_generation = session.surface_generation_id.get(),
            device_generation = session
                .viewer_gpu_device_progress
                .generation_id()
                .map(ViewerGpuDeviceGenerationId::get),
            "UI initialized — {}x{}",
            session.config.width,
            session.config.height
        );
        session.window.set_visible(true);
        session.window.request_redraw();
    })) {
        let primary = crate::app::execution_panic_diagnostic::execution_panic_diagnostic(
            payload,
            "initial Window Host publication",
        )
        .to_string();
        let gpu_shutdown =
            catch_window_viewer_gpu_shutdown(&mut session, &host, host_startup_deadline);
        let gpu_failure = gpu_shutdown.qualification_failure();
        let (app_state, ui_shutdown) = host.into_app_state_until(host_startup_deadline);
        let ui_failure = (!ui_shutdown.all_resources_released())
            .then(|| format!("Window UI services did not close cleanly: {ui_shutdown:?}"));
        drop(background_runtime_guard);
        let runtime_shutdown = background_runtime.shutdown_until(host_startup_deadline);
        let runtime_failure = runtime_shutdown.qualification_failure();
        #[cfg(feature = "validation")]
        if let Some(return_slot) = validation_return.as_ref() {
            *return_slot.borrow_mut() = Some(AppUiValidationReturnedState {
                app_state,
                shutdown: AppUiValidationWindowShutdown::PendingActivePublicationFailure {
                    runtime: runtime_shutdown,
                    host: ui_shutdown,
                    gpu: gpu_shutdown,
                },
            });
        }
        #[cfg(not(feature = "validation"))]
        {
            drop(app_state);
            drop(gpu_shutdown);
        }
        let cleanup_failure = merge_window_cleanup_failures(
            merge_window_cleanup_failures(gpu_failure, ui_failure),
            runtime_failure,
        );
        return match cleanup_failure {
            Some(cleanup) => Err(format!(
                "{primary}; initial Window publication cleanup also failed: {cleanup}"
            )
            .into()),
            None => Err(primary.into()),
        };
    }

    let mut host = AppUiHostSessionOwner::new(
        host,
        #[cfg(feature = "validation")]
        validation_return.clone(),
        #[cfg(feature = "validation")]
        validation_shutdown_deadline,
    );

    use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
    let event_loop_result = catch_app_ui_event_loop(|| {
        #[allow(deprecated)]
        event_loop.run_on_demand(|event, elwt| {
        use winit::event::ElementState;
        use winit::event::{Event, WindowEvent};
        use winit::event_loop::ControlFlow;
        elwt.set_control_flow(ControlFlow::Wait);
        let dispatch_action = |action| pending_actions.push(action);

        match event {
            Event::UserEvent(AppUiUserEvent::PreviewWorkAvailable) => {
                let drain_target_revision = preview_work_watch.revision();
                let background_tasks =
                    poll_window_background_tasks(&mut host, &mut session);
                if background_tasks.quit_requested {
                    elwt.exit();
                }
                rearm_preview_work_event(
                    &preview_work_event_pending,
                    drain_target_revision,
                    &preview_work_watch,
                    || {
                        preview_work_event_proxy
                            .send_event(AppUiUserEvent::PreviewWorkAvailable)
                            .is_ok()
                    },
                );
                if background_tasks.repaint_required {
                    sync_window_session_role(
                        &mut host,
                        elwt,
                        &instance,
                        &adapter,
                        &device,
                        &mut session,
                    );
                    session.window.request_redraw();
                }
                if background_tasks.needs_follow_up_poll {
                    elwt.set_control_flow(ControlFlow::Poll);
                }
            }
            Event::UserEvent(AppUiUserEvent::ViewerGpuCompletionAvailable) => {
                if poll_viewer_heterogeneous_completion(&device, &mut session, &host)
                    == ViewerHeterogeneousCompletionPoll::TerminalChange
                {
                    sync_window_session_role(
                        &mut host,
                        elwt,
                        &instance,
                        &adapter,
                        &device,
                        &mut session,
                    );
                    session.window.request_redraw();
                }
            }
            Event::WindowEvent { window_id, event } if window_id == session.window.id() => {
                match event {
                    WindowEvent::CloseRequested => {
                        pending_actions.push(native_close_request_action());
                        let should_redraw = drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        if should_redraw {
                            session.window.request_redraw();
                        }
                    }

                    WindowEvent::ModifiersChanged(modifiers) => {
                        session.modifiers_state = winit_modifiers_to_ui_modifiers(modifiers);
                    }

                    WindowEvent::ThemeChanged(theme) => {
                        if host.set_system_theme_preset(winit_theme_to_theme_preset(Some(theme))) {
                            host.refresh_if_dirty(session.current_bounds.get());
                        }
                        session.window.request_redraw();
                    }

                    WindowEvent::Focused(false) => {
                        reset_modifiers_on_window_focus_loss(&mut session.modifiers_state);
                        if should_route_focus_lost_to_ui(session.ui_runtime.is_eyedropper_active())
                        {
                            let _ = session.ui_runtime.route_window_event(
                                &session.window,
                                &mut session.router,
                                host.active_root_mut(),
                                UiEvent::FocusLost,
                                &dispatch_action,
                            );
                            drain_actions_and_sync_window_session(
                                &mut host,
                                &pending_actions,
                                &platform,
                                elwt,
                                &instance,
                                &adapter,
                                &device,
                                &mut session,
                            );
                            host.sync_workspace_layout_from_root();
                        } else {
                            elwt.set_control_flow(ControlFlow::Poll);
                        }
                        session.window.request_redraw();
                    }

                    WindowEvent::KeyboardInput { event: key_event, .. } => {
                        let result = session.ui_runtime.route_keyboard_input(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            &key_event,
                            &mut session.modifiers_state,
                            &dispatch_action,
                        );
                        drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        if result == EventResult::Ignored
                            && should_exit_on_ignored_keyboard_input(
                                session.role,
                                &key_event.logical_key,
                            )
                        {
                            elwt.exit();
                        }
                        update_window_cursor_icon(&host, &mut session);
                        session.window.request_redraw();
                    }

                    WindowEvent::Ime(ime) => {
                        let _ = session.ui_runtime.route_ime_event(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            ime,
                            &dispatch_action,
                        );
                        drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        session.window.request_redraw();
                    }

                    WindowEvent::RedrawRequested => {
                        let redraw_started = Instant::now();
                        sync_window_session_role(
                            &mut host,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        let prepare_started = Instant::now();
                        prepare_viewer_gpu_preview(
                            &device,
                            &queue,
                            &mut session,
                            &host,
                        );
                        session.event_loop_telemetry.record_stage_duration(
                            AppUiEventLoopStage::PrepareViewerGpuPreview,
                            prepare_started.elapsed(),
                        );
                        let refresh_started = Instant::now();
                        host.refresh_if_dirty(session.current_bounds.get());
                        session.event_loop_telemetry.record_stage_duration(
                            AppUiEventLoopStage::RefreshIfDirty,
                            refresh_started.elapsed(),
                        );
                        let paint_started = Instant::now();
                        let mut encoder = DrawEncoder::new();
                        let theme = mondrian_ui_theme::current_theme();
                        let b = session.current_bounds.get();
                        if session.role == AppUiWindowRole::Workspace {
                            encoder.draw_rect(b, theme.colors.background, 0.0);
                        }
                        TreeWalker::paint_clipped(host.active_root(), &mut encoder, &theme, b);
                        session.ui_runtime.paint_shell_overlays(
                            &mut encoder,
                            &theme,
                            b,
                            session.last_cursor,
                            &session.router,
                        );
                        let size = session.window.inner_size();
                        let frame_result = session.frame_renderer.render_draw_commands(
                            &device,
                            &queue,
                            &session.surface,
                            &session.config,
                            (size.width, size.height),
                            encoder.finish(),
                        );
                        if let Some(diagnostics) =
                            session.render_diagnostic_reporter.changed_failure(frame_result)
                        {
                            tracing::warn!(
                                "app UI render resource failures: missing_glyphs={}, raster_image_failures={}, unsupported_raster_color_spaces={}, external_texture_failures={}",
                                diagnostics.text_missing_glyphs,
                                diagnostics.raster_image_failures,
                                diagnostics.unsupported_raster_color_spaces,
                                diagnostics.external_texture_failures
                            );
                            host.mark_dirty();
                            session.window.request_redraw();
                        }
                        if let Some(event) =
                            session.render_diagnostic_reporter.changed_backend_event(frame_result)
                        {
                            log_backend_event(event);
                        }
                        if let Some(pressure) =
                            session.render_diagnostic_reporter.changed_pressure(frame_result)
                        {
                            log_frame_pressure(pressure);
                        }
                        trace_color_output_runtime(
                            session.viewer_gpu_execution.color_output_diagnostics(),
                            &session.display_output_contract,
                        );
                        trace_viewer_gpu_output_telemetry(
                            &host,
                            &session.viewer_gpu_output_telemetry,
                            &session.display_output_contract.display_target,
                            session.viewer_gpu_execution.color_output_diagnostics().into(),
                            session.display_snapshot.as_ref(),
                            session.display_calibration.as_deref(),
                            Some(session.display_management_policy.icc_rendering_intent()),
                            Some(&session.renderer_adapter),
                            frame_result.metrics(),
                        );
                        #[cfg(feature = "validation")]
                        if let Some(validation) = surface_reopen_validation.as_mut() {
                            let surface_generation_before = session.surface_generation_id;
                            match advance_surface_device_reopen_validation(
                                validation,
                                frame_result,
                                elwt,
                                &instance,
                                &adapter,
                                &mut device,
                                &mut queue,
                                &mut host,
                                &mut session,
                                &preview_work_event_proxy,
                            ) {
                                Ok(true) => {
                                    elwt.exit();
                                    return;
                                }
                                Ok(false)
                                    if session.surface_generation_id
                                        != surface_generation_before =>
                                {
                                    // The fresh generation already requested
                                    // its own redraw. Do not mutate it using
                                    // the retiring frame's result.
                                    return;
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    publish_surface_reopen_validation_failure(
                                        validation,
                                        error.to_string(),
                                    );
                                    elwt.exit();
                                    return;
                                }
                            }
                        }
                        if frame_result.needs_follow_up_redraw() {
                            session.window.request_redraw();
                        }
                        session.pending_initial_redraw = false;
                        session.event_loop_telemetry.record_stage_duration(
                            AppUiEventLoopStage::PaintAndRender,
                            paint_started.elapsed(),
                        );
                        session.event_loop_telemetry.record_stage_duration(
                            AppUiEventLoopStage::RedrawRequested,
                            redraw_started.elapsed(),
                        );
                    }

                    WindowEvent::Resized(new_size) => {
                        apply_surface_lifecycle_update(
                            SurfaceLifecycleReason::Resize,
                            (new_size.width, new_size.height),
                            &adapter,
                            &device,
                            &mut session,
                            &mut host,
                        );
                    }

                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        let size = session.window.inner_size();
                        tracing::debug!(
                            scale_factor,
                            width = size.width,
                            height = size.height,
                            "app UI window scale factor changed"
                        );
                        apply_surface_lifecycle_update(
                            SurfaceLifecycleReason::ScaleFactorChanged,
                            (size.width, size.height),
                            &adapter,
                            &device,
                            &mut session,
                            &mut host,
                        );
                    }

                    WindowEvent::Moved(position) => {
                        tracing::debug!(
                            x = position.x,
                            y = position.y,
                            "app UI window moved; refreshing display output contract"
                        );
                        refresh_display_output_contract(
                            DisplayOutputContractRefreshReason::WindowMoved,
                            &adapter,
                            &device,
                            &mut session,
                            &mut host,
                        );
                    }

                    WindowEvent::HoveredFile(path) => {
                        let result = session.ui_runtime.route_hovered_file(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            path,
                            session.last_cursor,
                            &dispatch_action,
                        );
                        if let Some(diagnostic) = native_file_hover_diagnostic(result) {
                            log_native_file_dnd_diagnostic(diagnostic);
                        }
                        session.window.request_redraw();
                    }

                    WindowEvent::HoveredFileCancelled => {
                        let result = session.ui_runtime.route_hovered_file_cancelled(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            &dispatch_action,
                        );
                        if let Some(diagnostic) = native_file_hover_cancelled_diagnostic(result) {
                            log_native_file_dnd_diagnostic(diagnostic);
                        }
                        session.window.request_redraw();
                    }

                    WindowEvent::DroppedFile(path) => {
                        let ui_path = path.clone();
                        let paths = vec![path];
                        let result = session.ui_runtime.route_dropped_file(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            ui_path,
                            session.last_cursor,
                            &dispatch_action,
                        );
                        let handling = native_file_drop_handling(result, paths);
                        if let Some(action) = handling.action {
                            pending_actions.push(action);
                        }
                        if let Some(diagnostic) = handling.diagnostic {
                            log_native_file_dnd_diagnostic(diagnostic);
                        }
                        // Defer tree rebuild while pointer capture is active
                        if session.router.captured().is_none() {
                            drain_actions_and_sync_window_session(
                                &mut host,
                                &pending_actions,
                                &platform,
                                elwt,
                                &instance,
                                &adapter,
                                &device,
                                &mut session,
                            );
                        }
                    }

                    WindowEvent::CursorMoved { position, .. } => {
                        session.last_cursor = Point::new(position.x as f32, position.y as f32);
                        session
                            .ui_runtime
                            .update_eyedropper_preview_at_window_point(
                                &session.window,
                                session.last_cursor,
                            );
                        let _ = session.ui_runtime.route_window_event(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            UiEvent::MouseMove {
                                position: session.last_cursor,
                                modifiers: session.modifiers_state,
                            },
                            &dispatch_action,
                        );
                        // Skip tree rebuild while pointer capture is active (drag in progress)
                        if session.router.captured().is_none() {
                            drain_actions_and_sync_window_session(
                                &mut host,
                                &pending_actions,
                                &platform,
                                elwt,
                                &instance,
                                &adapter,
                                &device,
                                &mut session,
                            );
                        } else {
                            session.window.request_redraw();
                        }
                        update_window_cursor_icon(&host, &mut session);
                    }

                    WindowEvent::MouseInput { state, button, .. } => {
                        let sync_workspace_layout = state == ElementState::Released
                            && button == winit::event::MouseButton::Left;
                        let evt = match state {
                            ElementState::Pressed => UiEvent::MouseDown {
                                position: session.last_cursor,
                                button: winit_mouse_button_to_ui_button(button),
                                modifiers: session.modifiers_state,
                            },
                            ElementState::Released => UiEvent::MouseUp {
                                position: session.last_cursor,
                                button: winit_mouse_button_to_ui_button(button),
                                modifiers: session.modifiers_state,
                            },
                        };
                        let is_press =
                            matches!(evt, UiEvent::MouseDown { button: MouseButton::Left, .. });
                        if is_press && session.ui_runtime.is_eyedropper_active() {
                            session.ui_runtime.finish_eyedropper_at_window_point(
                                &session.window,
                                &mut session.router,
                                host.active_root_mut(),
                                session.last_cursor,
                                &dispatch_action,
                            );
                        } else {
                            let _ = session.ui_runtime.route_window_event(
                                &session.window,
                                &mut session.router,
                                host.active_root_mut(),
                                evt,
                                &dispatch_action,
                            );
                        }
                        drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        if sync_workspace_layout {
                            host.sync_workspace_layout_from_root();
                        }
                        update_window_cursor_icon(&host, &mut session);
                        session.window.request_redraw();
                    }

                    WindowEvent::MouseWheel { delta, .. } => {
                        let _ = session.ui_runtime.route_window_event(
                            &session.window,
                            &mut session.router,
                            host.active_root_mut(),
                            UiEvent::MouseWheel {
                                delta: winit_scroll_delta_to_ui_delta(delta),
                                position: session.last_cursor,
                                modifiers: session.modifiers_state,
                            },
                            &dispatch_action,
                        );
                        drain_actions_and_sync_window_session(
                            &mut host,
                            &pending_actions,
                            &platform,
                            elwt,
                            &instance,
                            &adapter,
                            &device,
                            &mut session,
                        );
                        session.window.request_redraw();
                    }

                    _ => {}
                }
            }

            Event::AboutToWait => {
                #[cfg(feature = "validation")]
                if let Some(validation) = surface_reopen_validation.as_mut() {
                    if Instant::now() >= validation.deadline {
                        publish_surface_reopen_validation_failure(
                            validation,
                            "Surface/device reopen validation deadline elapsed",
                        );
                        elwt.exit();
                        return;
                    }
                    let pump_result = validation
                        .recovery_pump
                        .as_mut()
                        .map_or(Ok(()), |pump| {
                            host.with_validation_app_state_mut(|app| pump.pump_window(app))
                        });
                    if let Err(detail) = pump_result {
                        publish_surface_reopen_validation_failure(
                            validation,
                            format!("Surface recovery companion pump failed: {detail}"),
                        );
                        elwt.exit();
                        return;
                    }
                    if Instant::now() >= validation.deadline {
                        publish_surface_reopen_validation_failure(
                            validation,
                            "Surface/device reopen validation deadline elapsed during companion pumping",
                        );
                        elwt.exit();
                        return;
                    }
                    if let Some(pump) = validation.recovery_pump.as_ref() {
                        elwt.set_control_flow(control_flow_wake_no_later_than(
                            elwt.control_flow(),
                            pump.next_pump_at(),
                        ));
                    }
                }
                session
                    .ui_runtime
                    .drive_timers(&session.window, &mut session.router, elwt);
                let playback_clock_started = Instant::now();
                #[cfg(feature = "validation")]
                let playback_changed = if !validation_window_advances_playback(
                    surface_reopen_validation.is_some(),
                ) {
                    false
                } else {
                    synchronize_playback_thread_scheduling(&host, &mut session);
                    let changed = host.advance_playback_clock(
                        Instant::now(),
                        session.current_bounds.get(),
                    );
                    synchronize_playback_thread_scheduling(&host, &mut session);
                    changed
                };
                #[cfg(not(feature = "validation"))]
                let playback_changed = {
                    synchronize_playback_thread_scheduling(&host, &mut session);
                    let changed = host.advance_playback_clock(
                        Instant::now(),
                        session.current_bounds.get(),
                    );
                    synchronize_playback_thread_scheduling(&host, &mut session);
                    changed
                };
                session.event_loop_telemetry.record_stage_duration(
                    AppUiEventLoopStage::AdvancePlaybackClock,
                    playback_clock_started.elapsed(),
                );
                let background_tasks =
                    poll_window_background_tasks(&mut host, &mut session);
                if background_tasks.quit_requested {
                    elwt.exit();
                }
                if playback_changed || background_tasks.repaint_required {
                    sync_window_session_role(
                        &mut host,
                        elwt,
                        &instance,
                        &adapter,
                        &device,
                        &mut session,
                    );
                    session.window.request_redraw();
                }
                if background_tasks.needs_follow_up_poll {
                    elwt.set_control_flow(ControlFlow::Poll);
                }
                let heterogeneous_completion = drive_viewer_heterogeneous_completion(
                    &device,
                    &mut session,
                    &host,
                    Instant::now(),
                );
                if heterogeneous_completion.redraw_required {
                    sync_window_session_role(
                        &mut host,
                        elwt,
                        &instance,
                        &adapter,
                        &device,
                        &mut session,
                    );
                    session.window.request_redraw();
                }
                if let Some(next_wake) = heterogeneous_completion.next_wake {
                    elwt.set_control_flow(control_flow_wake_no_later_than(
                        elwt.control_flow(),
                        next_wake,
                    ));
                }
                if let Some(delay) = host.playback_next_wake_delay() {
                    elwt.set_control_flow(control_flow_wake_no_later_than(
                        elwt.control_flow(),
                        Instant::now() + app_ui_interactive_playback_wake_delay(&host, delay),
                    ));
                }
                if session.pending_initial_redraw {
                    session.window.request_redraw();
                    elwt.set_control_flow(ControlFlow::Poll);
                }
                if session.ui_runtime.is_eyedropper_active() {
                    session.ui_runtime.poll_eyedropper(
                        &session.window,
                        &mut session.router,
                        host.active_root_mut(),
                        &mut session.last_cursor,
                        session.modifiers_state,
                        &dispatch_action,
                    );
                    drain_actions_and_sync_window_session(
                        &mut host,
                        &pending_actions,
                        &platform,
                        elwt,
                        &instance,
                        &adapter,
                        &device,
                        &mut session,
                    );
                    session.window.request_redraw();
                    elwt.set_control_flow(ControlFlow::Poll);
                }
                elwt.set_control_flow(control_flow_wake_no_later_than(
                    elwt.control_flow(),
                    host.next_execution_resource_observation_deadline(),
                ));
                #[cfg(feature = "validation")]
                if let Some(validation) = surface_reopen_validation.as_ref() {
                    elwt.set_control_flow(control_flow_wake_no_later_than(
                        elwt.control_flow(),
                        validation.deadline,
                    ));
                }
            }
                _ => {}
            }
        })
    });
    let event_loop_result =
        merge_app_ui_event_loop_failure(event_loop_result, session.event_loop_failure.take());
    let shutdown_deadline = validation_shutdown_deadline.unwrap_or_else(|| {
        Instant::now()
            .checked_add(APP_UI_WINDOW_PRODUCT_SHUTDOWN_TIMEOUT)
            .unwrap_or_else(Instant::now)
    });
    let gpu_shutdown = catch_window_viewer_gpu_shutdown(&mut session, &host, shutdown_deadline);
    let gpu_failure = gpu_shutdown.qualification_failure();
    let ui_failure = match host.shutdown_until(shutdown_deadline, gpu_shutdown) {
        Ok(evidence) if evidence.all_resources_released() => None,
        Ok(evidence) => Some(format!(
            "Window UI services did not close cleanly: {evidence:?}"
        )),
        Err(error) => Some(error),
    };

    drop(background_runtime_guard);
    let runtime_shutdown = background_runtime.shutdown_until(shutdown_deadline);
    let runtime_failure = runtime_shutdown.qualification_failure();
    #[cfg(feature = "validation")]
    let outer_record_failure = validation_return.as_ref().and_then(|return_slot| {
        record_validation_active_runtime_shutdown(return_slot, runtime_shutdown)
            .err()
            .map(|error| format!("Window outer shutdown evidence was incomplete: {error}"))
    });
    #[cfg(not(feature = "validation"))]
    let outer_record_failure: Option<String> = None;
    tracing::info!("Mondrian app UI stopped");
    drop(tracing_guard);

    let cleanup_failure = merge_window_cleanup_failures(
        merge_window_cleanup_failures(
            merge_window_cleanup_failures(gpu_failure, ui_failure),
            runtime_failure,
        ),
        outer_record_failure,
    );
    match (event_loop_result, cleanup_failure) {
        (Err(primary), Some(cleanup)) => {
            Err(format!("{primary}; Window cleanup also failed: {cleanup}").into())
        }
        (Err(primary), None) => Err(primary.into()),
        (Ok(()), Some(cleanup)) => Err(cleanup.into()),
        (Ok(()), None) => Ok(()),
    }
}

#[cfg(not(test))]
pub(crate) fn arm_process_exit_watchdog() {
    let _ = std::thread::Builder::new().name("mondrian-exit-watchdog".to_owned()).spawn(|| {
        // The UI has already completed its guarded close and initiated
        // event-loop shutdown before this watchdog is armed. Give winit,
        // audio, media, and GPU resources a bounded grace period, then
        // bypass process-wide DLL/destructor teardown: std::process::exit
        // can itself deadlock when a third-party detach hook needs a lock
        // held by another terminating thread.
        std::thread::sleep(Duration::from_secs(2));
        tracing::error!(
            exit_code = FORCED_PROCESS_EXIT_CODE,
            "process-exit watchdog deadline elapsed; forcing process termination"
        );
        // Give the non-blocking product log writer one bounded opportunity to
        // persist the terminal marker before the no-destructor exit.
        std::thread::sleep(Duration::from_millis(100));
        terminate_process_without_cleanup();
    });
}

#[cfg(all(not(test), target_os = "windows"))]
fn terminate_process_without_cleanup() -> ! {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

    // SAFETY: the pseudo-handle always refers to this process. This is the
    // final fallback after the application-level close contract has completed;
    // skipping DLL detach is intentional to avoid third-party teardown locks.
    unsafe {
        let _ = TerminateProcess(GetCurrentProcess(), FORCED_PROCESS_EXIT_CODE);
    }
    std::process::abort()
}

#[cfg(all(not(test), unix))]
fn terminate_process_without_cleanup() -> ! {
    // SAFETY: application-level shutdown has completed. `_exit` deliberately
    // skips process-wide destructors that may be blocked in media/GPU drivers.
    unsafe { libc::_exit(FORCED_PROCESS_EXIT_CODE as libc::c_int) }
}

#[cfg(all(not(test), not(any(target_os = "windows", unix))))]
fn terminate_process_without_cleanup() -> ! {
    std::process::abort()
}

fn app_ui_display_output_contract(
    window: &winit::window::Window,
    surface: &wgpu::Surface<'static>,
    adapter: &wgpu::Adapter,
    intent: AppUiSurfacePresentationIntent,
) -> Result<AppUiDisplayOutputContract, AppUiSurfaceColorContractError> {
    let capabilities = surface.get_capabilities(adapter);
    let display_target = app_ui_display_target_for_window(window);
    let display_hdr_info = surface.display_hdr_info(adapter);
    let qualified_intent = match intent {
        AppUiSurfacePresentationIntent::DisplayOutput(output_color_space)
            if output_color_space.is_hdr()
                && !super::display_probe_impl::active_display_hdr_presentation_ready(
                    display_target.position,
                    display_target.physical_size,
                    display_target.native_display_id,
                    display_hdr_info.clone(),
                ) =>
        {
            tracing::warn!(
                ?intent,
                display_target = ?display_target,
                "HDR format/color-space enumeration is not backed by an active HDR compositor and monitor; retaining fail-closed SDR UI carrier"
            );
            AppUiSurfacePresentationIntent::SdrSrgb
        }
        _ => intent,
    };
    let surface_color = choose_app_ui_surface_color_contract(&capabilities, qualified_intent).or_else(
        |requested_error| {
            tracing::warn!(
                ?intent,
                "requested display surface contract is unavailable; retaining fail-closed SDR UI carrier: {requested_error}"
            );
            choose_app_ui_surface_format(&capabilities)
        },
    )?;
    Ok(AppUiDisplayOutputContract {
        surface_color,
        display_target,
        display_hdr_info,
        available_formats: capabilities.formats.clone(),
        format_color_spaces: app_ui_surface_format_color_spaces(&capabilities),
        present_modes: capabilities.present_modes,
        alpha_modes: capabilities.alpha_modes,
    })
}

fn app_ui_surface_presentation_intent_for_role(
    role: AppUiWindowRole,
    host: &AppUiHost,
) -> AppUiSurfacePresentationIntent {
    if role == AppUiWindowRole::Startup {
        return AppUiSurfacePresentationIntent::SdrSrgb;
    }
    let (engine, policy) = host.resolved_display_color_management();
    match policy.resolve_output_color_space(&engine, host.active_program_output_color_space()) {
        Ok(output_color_space) => AppUiSurfacePresentationIntent::DisplayOutput(output_color_space),
        Err(error) => {
            tracing::warn!(
                "display policy cannot resolve a monitor target; retaining fail-closed SDR UI carrier: {error}"
            );
            AppUiSurfacePresentationIntent::SdrSrgb
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppUiSurfaceColorContract {
    format: wgpu::TextureFormat,
    color_space: wgpu::SurfaceColorSpace,
    encoding: AppUiSurfaceEncoding,
    hdr_mode: AppUiSurfaceHdrMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppUiSurfaceEncoding {
    Srgb,
    Pq,
    Hlg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum AppUiSurfaceHdrMode {
    SdrOnly,
    HdrPq,
    HdrHlg,
}

#[derive(Debug, Clone, PartialEq)]
struct AppUiDisplayOutputContract {
    surface_color: AppUiSurfaceColorContract,
    display_target: AppUiDisplayTarget,
    display_hdr_info: wgpu::DisplayHdrInfo,
    available_formats: Vec<wgpu::TextureFormat>,
    format_color_spaces: Vec<AppUiSurfaceFormatColorSpaces>,
    present_modes: Vec<wgpu::PresentMode>,
    alpha_modes: Vec<wgpu::CompositeAlphaMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppUiSurfaceFormatColorSpaces {
    format: wgpu::TextureFormat,
    srgb: bool,
    extended_srgb_linear: bool,
    display_p3: bool,
    bt2100_pq: bool,
    bt2100_hlg: bool,
    extended_srgb: bool,
    extended_display_p3: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct AppUiDisplayTarget {
    name: Option<String>,
    position: (i32, i32),
    physical_size: (u32, u32),
    native_display_id: Option<u64>,
    native_display_path_id: Option<String>,
    scale_factor_ppm: u32,
    refresh_rate_millihertz: Option<u32>,
}

impl AppUiDisplayOutputContract {
    fn presentation_readiness_for_color_space(
        &self,
        output_color_space: ColorSpace,
    ) -> AppUiDisplayPresentationReadinessDiagnostics {
        if app_ui_surface_color_space_matches_display_output(
            self.surface_color.color_space,
            output_color_space,
        ) {
            return self.display_presentation_readiness_current(output_color_space);
        }

        let intent = AppUiSurfacePresentationIntent::DisplayOutput(output_color_space);
        let desired =
            choose_app_ui_surface_color_contract(&self.surface_capabilities_snapshot(), intent);
        match desired {
            Ok(desired_surface) => AppUiDisplayPresentationReadinessDiagnostics {
                status: AppUiDisplayPresentationReadinessStatus::ReconfigurationRequired,
                output_color_space,
                current_surface_format: app_ui_surface_format_diagnostic(self.surface_color.format),
                current_surface_color_space: app_ui_surface_color_space_diagnostic(
                    self.surface_color.color_space,
                ),
                current_surface_encoding: app_ui_surface_encoding_diagnostic(
                    self.surface_color.encoding,
                ),
                current_surface_hdr_mode: self.surface_color.hdr_mode,
                desired_surface_format: Some(app_ui_surface_format_diagnostic(
                    desired_surface.format,
                )),
                desired_surface_color_space: Some(app_ui_surface_color_space_diagnostic(
                    desired_surface.color_space,
                )),
                desired_surface_encoding: Some(app_ui_surface_encoding_diagnostic(
                    desired_surface.encoding,
                )),
                desired_surface_hdr_mode: Some(desired_surface.hdr_mode),
                payload_blocker: None,
            },
            Err(err) if err.required_color_space.is_none() => self
                .display_presentation_readiness_unsupported(
                    output_color_space,
                    AppUiDisplayPresentationReadinessStatus::UnsupportedPresentationIntent,
                    None,
                ),
            Err(err) => self.display_presentation_readiness_unsupported(
                output_color_space,
                AppUiDisplayPresentationReadinessStatus::UnsupportedSurfaceContract,
                err.required_color_space,
            ),
        }
    }

    #[cfg(test)]
    fn presentation_readiness_for_boundary(
        &self,
        boundary: &ProgramOutputBoundary,
    ) -> AppUiDisplayPresentationReadinessDiagnostics {
        if boundary.target() != ProgramOutputRole::Display {
            return self.display_presentation_readiness_current(boundary.output_color_space());
        }
        self.presentation_readiness_for_color_space(boundary.output_color_space())
    }

    fn display_presentation_readiness_current(
        &self,
        output_color_space: ColorSpace,
    ) -> AppUiDisplayPresentationReadinessDiagnostics {
        AppUiDisplayPresentationReadinessDiagnostics {
            status: AppUiDisplayPresentationReadinessStatus::Current,
            output_color_space,
            current_surface_format: app_ui_surface_format_diagnostic(self.surface_color.format),
            current_surface_color_space: app_ui_surface_color_space_diagnostic(
                self.surface_color.color_space,
            ),
            current_surface_encoding: app_ui_surface_encoding_diagnostic(
                self.surface_color.encoding,
            ),
            current_surface_hdr_mode: self.surface_color.hdr_mode,
            desired_surface_format: Some(app_ui_surface_format_diagnostic(
                self.surface_color.format,
            )),
            desired_surface_color_space: Some(app_ui_surface_color_space_diagnostic(
                self.surface_color.color_space,
            )),
            desired_surface_encoding: Some(app_ui_surface_encoding_diagnostic(
                self.surface_color.encoding,
            )),
            desired_surface_hdr_mode: Some(self.surface_color.hdr_mode),
            payload_blocker: None,
        }
    }

    fn display_presentation_readiness_unsupported(
        &self,
        output_color_space: ColorSpace,
        status: AppUiDisplayPresentationReadinessStatus,
        desired_color_space: Option<wgpu::SurfaceColorSpace>,
    ) -> AppUiDisplayPresentationReadinessDiagnostics {
        AppUiDisplayPresentationReadinessDiagnostics {
            status,
            output_color_space,
            current_surface_format: app_ui_surface_format_diagnostic(self.surface_color.format),
            current_surface_color_space: app_ui_surface_color_space_diagnostic(
                self.surface_color.color_space,
            ),
            current_surface_encoding: app_ui_surface_encoding_diagnostic(
                self.surface_color.encoding,
            ),
            current_surface_hdr_mode: self.surface_color.hdr_mode,
            desired_surface_format: None,
            desired_surface_color_space: desired_color_space
                .map(app_ui_surface_color_space_diagnostic),
            desired_surface_encoding: desired_color_space
                .map(app_ui_surface_encoding)
                .map(app_ui_surface_encoding_diagnostic),
            desired_surface_hdr_mode: desired_color_space.map(app_ui_surface_hdr_mode),
            payload_blocker: None,
        }
    }

    #[cfg(test)]
    fn boundary_blocker(
        &self,
        boundary: &ProgramOutputBoundary,
    ) -> Option<AppUiDisplayBoundaryBlocker> {
        if boundary.target() != ProgramOutputRole::Display {
            return None;
        }
        self.boundary_blocker_for_color_space(boundary.output_color_space())
    }

    fn boundary_blocker_for_color_space(
        &self,
        output_color_space: ColorSpace,
    ) -> Option<AppUiDisplayBoundaryBlocker> {
        let supported_surface_color_spaces =
            self.supported_surface_color_spaces_for_selected_format();
        if output_color_space.is_hdr()
            && self.surface_color.hdr_mode == AppUiSurfaceHdrMode::SdrOnly
        {
            return Some(AppUiDisplayBoundaryBlocker::HdrOutputRequiresHdrSurface {
                output_color_space,
                selected_surface_format: self.surface_color.format,
                selected_surface_color_space: self.surface_color.color_space,
                selected_surface_encoding: self.surface_color.encoding,
                surface_hdr_mode: self.surface_color.hdr_mode,
                supported_surface_color_spaces,
            });
        }

        if app_ui_surface_color_space_matches_display_output(
            self.surface_color.color_space,
            output_color_space,
        ) {
            return None;
        }

        Some(
            AppUiDisplayBoundaryBlocker::OutputColorSpaceRequiresSurfaceColorSpace {
                output_color_space,
                selected_surface_format: self.surface_color.format,
                selected_surface_color_space: self.surface_color.color_space,
                selected_surface_encoding: self.surface_color.encoding,
                surface_hdr_mode: self.surface_color.hdr_mode,
                supported_surface_color_spaces,
            },
        )
    }

    fn supported_surface_color_spaces_for_selected_format(&self) -> Vec<wgpu::SurfaceColorSpace> {
        self.format_color_spaces
            .iter()
            .find(|format_color_spaces| format_color_spaces.format == self.surface_color.format)
            .map(AppUiSurfaceFormatColorSpaces::supported_surface_color_spaces)
            .unwrap_or_default()
    }

    fn surface_capabilities_snapshot(&self) -> wgpu::SurfaceCapabilities {
        wgpu::SurfaceCapabilities {
            formats: self.available_formats.clone(),
            format_capabilities: self
                .format_color_spaces
                .iter()
                .map(AppUiSurfaceFormatColorSpaces::surface_format_capabilities)
                .collect(),
            present_modes: self.present_modes.clone(),
            alpha_modes: self.alpha_modes.clone(),
            usages: wgpu::TextureUsages::RENDER_ATTACHMENT,
        }
    }
}

fn display_output_contract_requires_gpu_preview_invalidation(
    previous: &AppUiDisplayOutputContract,
    next: &AppUiDisplayOutputContract,
) -> bool {
    previous != next
}

fn display_output_contract_requires_renderer_rebuild(
    previous: &AppUiDisplayOutputContract,
    next: &AppUiDisplayOutputContract,
) -> bool {
    previous.surface_color.format != next.surface_color.format
        || previous.surface_color.color_space != next.surface_color.color_space
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AppUiDisplayBoundaryBlocker {
    HdrOutputRequiresHdrSurface {
        output_color_space: ColorSpace,
        selected_surface_format: wgpu::TextureFormat,
        selected_surface_color_space: wgpu::SurfaceColorSpace,
        selected_surface_encoding: AppUiSurfaceEncoding,
        surface_hdr_mode: AppUiSurfaceHdrMode,
        supported_surface_color_spaces: Vec<wgpu::SurfaceColorSpace>,
    },
    OutputColorSpaceRequiresSurfaceColorSpace {
        output_color_space: ColorSpace,
        selected_surface_format: wgpu::TextureFormat,
        selected_surface_color_space: wgpu::SurfaceColorSpace,
        selected_surface_encoding: AppUiSurfaceEncoding,
        surface_hdr_mode: AppUiSurfaceHdrMode,
        supported_surface_color_spaces: Vec<wgpu::SurfaceColorSpace>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum AppUiDisplayBoundaryBlockerKind {
    HdrOutputRequiresHdrSurface,
    OutputColorSpaceRequiresSurfaceColorSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum AppUiSurfaceColorSpaceDiagnostic {
    Srgb,
    DisplayP3,
    ExtendedSrgbLinear,
    ExtendedSrgb,
    ExtendedDisplayP3,
    Bt2100Pq,
    Bt2100Hlg,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum AppUiSurfaceFormatDiagnostic {
    Bgra8UnormSrgb,
    Rgba8UnormSrgb,
    Rgba16Float,
    Rgb10a2Unorm,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum AppUiSurfaceEncodingDiagnostic {
    Srgb,
    Pq,
    Hlg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum AppUiDisplayPresentationReadinessStatus {
    Current,
    ReconfigurationRequired,
    UnsupportedPresentationIntent,
    UnsupportedSurfaceContract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
enum AppUiDisplayPresentationPayloadBlocker {
    /// Legacy diagnostic retained so persisted qualification evidence remains
    /// readable; the production carrier no longer emits this blocker.
    #[allow(dead_code)]
    UiExternalTextureCompositingRequiresSdrSrgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
struct AppUiDisplayPresentationReadinessDiagnostics {
    status: AppUiDisplayPresentationReadinessStatus,
    output_color_space: ColorSpace,
    current_surface_format: AppUiSurfaceFormatDiagnostic,
    current_surface_color_space: AppUiSurfaceColorSpaceDiagnostic,
    current_surface_encoding: AppUiSurfaceEncodingDiagnostic,
    current_surface_hdr_mode: AppUiSurfaceHdrMode,
    desired_surface_format: Option<AppUiSurfaceFormatDiagnostic>,
    desired_surface_color_space: Option<AppUiSurfaceColorSpaceDiagnostic>,
    desired_surface_encoding: Option<AppUiSurfaceEncodingDiagnostic>,
    desired_surface_hdr_mode: Option<AppUiSurfaceHdrMode>,
    payload_blocker: Option<AppUiDisplayPresentationPayloadBlocker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
struct AppUiDisplayBoundaryBlockerDiagnostics {
    kind: AppUiDisplayBoundaryBlockerKind,
    output_color_space: ColorSpace,
    selected_surface_format: AppUiSurfaceFormatDiagnostic,
    selected_surface_color_space: AppUiSurfaceColorSpaceDiagnostic,
    selected_surface_encoding: AppUiSurfaceEncodingDiagnostic,
    surface_hdr_mode: AppUiSurfaceHdrMode,
    supported_surface_color_space_count: u8,
    supports_srgb: bool,
    supports_display_p3: bool,
    supports_extended_srgb_linear: bool,
    supports_extended_srgb: bool,
    supports_extended_display_p3: bool,
    supports_bt2100_pq: bool,
    supports_bt2100_hlg: bool,
}

impl AppUiDisplayBoundaryBlocker {
    fn diagnostics(&self) -> AppUiDisplayBoundaryBlockerDiagnostics {
        match self {
            Self::HdrOutputRequiresHdrSurface {
                output_color_space,
                selected_surface_format,
                selected_surface_color_space,
                selected_surface_encoding,
                surface_hdr_mode,
                supported_surface_color_spaces,
            } => AppUiDisplayBoundaryBlockerDiagnostics::new(
                AppUiDisplayBoundaryBlockerKind::HdrOutputRequiresHdrSurface,
                *output_color_space,
                *selected_surface_format,
                *selected_surface_color_space,
                *selected_surface_encoding,
                *surface_hdr_mode,
                supported_surface_color_spaces,
            ),
            Self::OutputColorSpaceRequiresSurfaceColorSpace {
                output_color_space,
                selected_surface_format,
                selected_surface_color_space,
                selected_surface_encoding,
                surface_hdr_mode,
                supported_surface_color_spaces,
            } => AppUiDisplayBoundaryBlockerDiagnostics::new(
                AppUiDisplayBoundaryBlockerKind::OutputColorSpaceRequiresSurfaceColorSpace,
                *output_color_space,
                *selected_surface_format,
                *selected_surface_color_space,
                *selected_surface_encoding,
                *surface_hdr_mode,
                supported_surface_color_spaces,
            ),
        }
    }
}

impl AppUiDisplayBoundaryBlockerDiagnostics {
    fn new(
        kind: AppUiDisplayBoundaryBlockerKind,
        output_color_space: ColorSpace,
        selected_surface_format: wgpu::TextureFormat,
        selected_surface_color_space: wgpu::SurfaceColorSpace,
        selected_surface_encoding: AppUiSurfaceEncoding,
        surface_hdr_mode: AppUiSurfaceHdrMode,
        supported_surface_color_spaces: &[wgpu::SurfaceColorSpace],
    ) -> Self {
        Self {
            kind,
            output_color_space,
            selected_surface_format: app_ui_surface_format_diagnostic(selected_surface_format),
            selected_surface_color_space: app_ui_surface_color_space_diagnostic(
                selected_surface_color_space,
            ),
            selected_surface_encoding: app_ui_surface_encoding_diagnostic(
                selected_surface_encoding,
            ),
            surface_hdr_mode,
            supported_surface_color_space_count: supported_surface_color_spaces
                .len()
                .min(u8::MAX as usize) as u8,
            supports_srgb: supported_surface_color_spaces.contains(&wgpu::SurfaceColorSpace::Srgb),
            supports_display_p3: supported_surface_color_spaces
                .contains(&wgpu::SurfaceColorSpace::DisplayP3),
            supports_extended_srgb_linear: supported_surface_color_spaces
                .contains(&wgpu::SurfaceColorSpace::ExtendedSrgbLinear),
            supports_extended_srgb: supported_surface_color_spaces
                .contains(&wgpu::SurfaceColorSpace::ExtendedSrgb),
            supports_extended_display_p3: supported_surface_color_spaces
                .contains(&wgpu::SurfaceColorSpace::ExtendedDisplayP3),
            supports_bt2100_pq: supported_surface_color_spaces
                .contains(&wgpu::SurfaceColorSpace::Bt2100Pq),
            supports_bt2100_hlg: supported_surface_color_spaces
                .contains(&wgpu::SurfaceColorSpace::Bt2100Hlg),
        }
    }
}

fn app_ui_surface_color_space_diagnostic(
    color_space: wgpu::SurfaceColorSpace,
) -> AppUiSurfaceColorSpaceDiagnostic {
    match color_space {
        wgpu::SurfaceColorSpace::Srgb => AppUiSurfaceColorSpaceDiagnostic::Srgb,
        wgpu::SurfaceColorSpace::DisplayP3 => AppUiSurfaceColorSpaceDiagnostic::DisplayP3,
        wgpu::SurfaceColorSpace::ExtendedSrgbLinear => {
            AppUiSurfaceColorSpaceDiagnostic::ExtendedSrgbLinear
        }
        wgpu::SurfaceColorSpace::ExtendedSrgb => AppUiSurfaceColorSpaceDiagnostic::ExtendedSrgb,
        wgpu::SurfaceColorSpace::ExtendedDisplayP3 => {
            AppUiSurfaceColorSpaceDiagnostic::ExtendedDisplayP3
        }
        wgpu::SurfaceColorSpace::Bt2100Pq => AppUiSurfaceColorSpaceDiagnostic::Bt2100Pq,
        wgpu::SurfaceColorSpace::Bt2100Hlg => AppUiSurfaceColorSpaceDiagnostic::Bt2100Hlg,
        _ => AppUiSurfaceColorSpaceDiagnostic::Other,
    }
}

impl AppUiSurfaceFormatColorSpaces {
    fn surface_format_capabilities(&self) -> wgpu::SurfaceFormatCapabilities {
        let mut color_spaces = wgpu::SurfaceColorSpaces::empty();
        if self.srgb {
            color_spaces |= wgpu::SurfaceColorSpaces::SRGB;
        }
        if self.extended_srgb_linear {
            color_spaces |= wgpu::SurfaceColorSpaces::EXTENDED_SRGB_LINEAR;
        }
        if self.display_p3 {
            color_spaces |= wgpu::SurfaceColorSpaces::DISPLAY_P3;
        }
        if self.bt2100_pq {
            color_spaces |= wgpu::SurfaceColorSpaces::BT2100_PQ;
        }
        if self.bt2100_hlg {
            color_spaces |= wgpu::SurfaceColorSpaces::BT2100_HLG;
        }
        if self.extended_srgb {
            color_spaces |= wgpu::SurfaceColorSpaces::EXTENDED_SRGB;
        }
        if self.extended_display_p3 {
            color_spaces |= wgpu::SurfaceColorSpaces::EXTENDED_DISPLAY_P3;
        }
        wgpu::SurfaceFormatCapabilities { format: self.format, color_spaces }
    }

    fn supported_surface_color_spaces(&self) -> Vec<wgpu::SurfaceColorSpace> {
        let mut color_spaces = Vec::with_capacity(7);
        if self.srgb {
            color_spaces.push(wgpu::SurfaceColorSpace::Srgb);
        }
        if self.display_p3 {
            color_spaces.push(wgpu::SurfaceColorSpace::DisplayP3);
        }
        if self.extended_srgb_linear {
            color_spaces.push(wgpu::SurfaceColorSpace::ExtendedSrgbLinear);
        }
        if self.extended_srgb {
            color_spaces.push(wgpu::SurfaceColorSpace::ExtendedSrgb);
        }
        if self.extended_display_p3 {
            color_spaces.push(wgpu::SurfaceColorSpace::ExtendedDisplayP3);
        }
        if self.bt2100_pq {
            color_spaces.push(wgpu::SurfaceColorSpace::Bt2100Pq);
        }
        if self.bt2100_hlg {
            color_spaces.push(wgpu::SurfaceColorSpace::Bt2100Hlg);
        }
        color_spaces
    }
}

fn app_ui_surface_format_diagnostic(format: wgpu::TextureFormat) -> AppUiSurfaceFormatDiagnostic {
    match format {
        wgpu::TextureFormat::Bgra8UnormSrgb => AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb,
        wgpu::TextureFormat::Rgba8UnormSrgb => AppUiSurfaceFormatDiagnostic::Rgba8UnormSrgb,
        wgpu::TextureFormat::Rgba16Float => AppUiSurfaceFormatDiagnostic::Rgba16Float,
        wgpu::TextureFormat::Rgb10a2Unorm => AppUiSurfaceFormatDiagnostic::Rgb10a2Unorm,
        _ => AppUiSurfaceFormatDiagnostic::Other,
    }
}

fn app_ui_surface_encoding_diagnostic(
    encoding: AppUiSurfaceEncoding,
) -> AppUiSurfaceEncodingDiagnostic {
    match encoding {
        AppUiSurfaceEncoding::Srgb => AppUiSurfaceEncodingDiagnostic::Srgb,
        AppUiSurfaceEncoding::Pq => AppUiSurfaceEncodingDiagnostic::Pq,
        AppUiSurfaceEncoding::Hlg => AppUiSurfaceEncodingDiagnostic::Hlg,
    }
}

fn app_ui_present_mode_diagnostic(mode: wgpu::PresentMode) -> AppUiPresentModeDiagnostic {
    match mode {
        wgpu::PresentMode::Fifo => AppUiPresentModeDiagnostic::Fifo,
        wgpu::PresentMode::FifoRelaxed => AppUiPresentModeDiagnostic::FifoRelaxed,
        wgpu::PresentMode::Immediate => AppUiPresentModeDiagnostic::Immediate,
        wgpu::PresentMode::Mailbox => AppUiPresentModeDiagnostic::Mailbox,
        wgpu::PresentMode::AutoVsync => AppUiPresentModeDiagnostic::AutoVsync,
        wgpu::PresentMode::AutoNoVsync => AppUiPresentModeDiagnostic::AutoNoVsync,
    }
}

fn app_ui_composite_alpha_mode_diagnostic(
    mode: wgpu::CompositeAlphaMode,
) -> AppUiCompositeAlphaModeDiagnostic {
    match mode {
        wgpu::CompositeAlphaMode::Auto => AppUiCompositeAlphaModeDiagnostic::Auto,
        wgpu::CompositeAlphaMode::Opaque => AppUiCompositeAlphaModeDiagnostic::Opaque,
        wgpu::CompositeAlphaMode::PreMultiplied => AppUiCompositeAlphaModeDiagnostic::PreMultiplied,
        wgpu::CompositeAlphaMode::PostMultiplied => {
            AppUiCompositeAlphaModeDiagnostic::PostMultiplied
        }
        wgpu::CompositeAlphaMode::Inherit => AppUiCompositeAlphaModeDiagnostic::Inherit,
    }
}

fn app_ui_display_tone_map_headroom_ppm(display_hdr_info: &wgpu::DisplayHdrInfo) -> Option<u32> {
    display_hdr_info.tone_map_headroom().map(|headroom| {
        ((headroom as f64) * 1_000_000.0).round().clamp(0.0, u32::MAX as f64) as u32
    })
}

fn app_ui_surface_color_space_matches_display_output(
    surface_color_space: wgpu::SurfaceColorSpace,
    output_color_space: ColorSpace,
) -> bool {
    app_ui_surface_color_space_for_intent(AppUiSurfacePresentationIntent::DisplayOutput(
        output_color_space,
    )) == Some(surface_color_space)
}

fn target_display_surface_color_space(
    output_color_space: ColorSpace,
) -> Option<AppUiSurfaceColorSpaceDiagnostic> {
    app_ui_surface_color_space_for_intent(AppUiSurfacePresentationIntent::DisplayOutput(
        output_color_space,
    ))
    .map(app_ui_surface_color_space_diagnostic)
}

fn app_ui_surface_hdr_mode_from_diagnostic(
    color_space: AppUiSurfaceColorSpaceDiagnostic,
) -> AppUiSurfaceHdrMode {
    match color_space {
        AppUiSurfaceColorSpaceDiagnostic::Bt2100Pq => AppUiSurfaceHdrMode::HdrPq,
        AppUiSurfaceColorSpaceDiagnostic::Bt2100Hlg => AppUiSurfaceHdrMode::HdrHlg,
        AppUiSurfaceColorSpaceDiagnostic::Srgb
        | AppUiSurfaceColorSpaceDiagnostic::DisplayP3
        | AppUiSurfaceColorSpaceDiagnostic::ExtendedSrgbLinear
        | AppUiSurfaceColorSpaceDiagnostic::ExtendedSrgb
        | AppUiSurfaceColorSpaceDiagnostic::ExtendedDisplayP3
        | AppUiSurfaceColorSpaceDiagnostic::Other => AppUiSurfaceHdrMode::SdrOnly,
    }
}

fn app_ui_surface_color_space_diagnostic_to_encoding(
    color_space: AppUiSurfaceColorSpaceDiagnostic,
) -> AppUiSurfaceEncodingDiagnostic {
    match color_space {
        AppUiSurfaceColorSpaceDiagnostic::Bt2100Pq => AppUiSurfaceEncodingDiagnostic::Pq,
        AppUiSurfaceColorSpaceDiagnostic::Bt2100Hlg => AppUiSurfaceEncodingDiagnostic::Hlg,
        AppUiSurfaceColorSpaceDiagnostic::Srgb
        | AppUiSurfaceColorSpaceDiagnostic::DisplayP3
        | AppUiSurfaceColorSpaceDiagnostic::ExtendedSrgbLinear
        | AppUiSurfaceColorSpaceDiagnostic::ExtendedSrgb
        | AppUiSurfaceColorSpaceDiagnostic::ExtendedDisplayP3
        | AppUiSurfaceColorSpaceDiagnostic::Other => AppUiSurfaceEncodingDiagnostic::Srgb,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppUiSurfaceColorContractError {
    intent: AppUiSurfacePresentationIntent,
    required_color_space: Option<wgpu::SurfaceColorSpace>,
    available_formats: Vec<wgpu::TextureFormat>,
}

impl std::fmt::Display for AppUiSurfaceColorContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "app UI surface cannot satisfy {:?} presentation; required color space: {:?}; available formats: {:?}",
            self.intent, self.required_color_space, self.available_formats
        )
    }
}

impl std::error::Error for AppUiSurfaceColorContractError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppUiSurfacePresentationIntent {
    SdrSrgb,
    DisplayOutput(ColorSpace),
}

fn choose_app_ui_surface_format(
    capabilities: &wgpu::SurfaceCapabilities,
) -> Result<AppUiSurfaceColorContract, AppUiSurfaceColorContractError> {
    choose_app_ui_surface_color_contract(capabilities, AppUiSurfacePresentationIntent::SdrSrgb)
}

fn choose_app_ui_surface_color_contract(
    capabilities: &wgpu::SurfaceCapabilities,
    intent: AppUiSurfacePresentationIntent,
) -> Result<AppUiSurfaceColorContract, AppUiSurfaceColorContractError> {
    let required_color_space = app_ui_surface_color_space_for_intent(intent).ok_or_else(|| {
        AppUiSurfaceColorContractError {
            intent,
            required_color_space: None,
            available_formats: capabilities.formats.clone(),
        }
    })?;
    if let Some(format) =
        choose_app_ui_surface_format_for_color_space(capabilities, required_color_space)
    {
        return Ok(AppUiSurfaceColorContract {
            format,
            color_space: required_color_space,
            encoding: app_ui_surface_encoding(required_color_space),
            hdr_mode: app_ui_surface_hdr_mode(required_color_space),
        });
    }

    Err(AppUiSurfaceColorContractError {
        intent,
        required_color_space: Some(required_color_space),
        available_formats: capabilities.formats.clone(),
    })
}

fn app_ui_surface_color_space_for_intent(
    intent: AppUiSurfacePresentationIntent,
) -> Option<wgpu::SurfaceColorSpace> {
    match intent {
        AppUiSurfacePresentationIntent::SdrSrgb => Some(wgpu::SurfaceColorSpace::Srgb),
        AppUiSurfacePresentationIntent::DisplayOutput(ColorSpace::Rec709 | ColorSpace::Srgb) => {
            Some(wgpu::SurfaceColorSpace::Srgb)
        }
        AppUiSurfacePresentationIntent::DisplayOutput(ColorSpace::DisplayP3) => {
            Some(wgpu::SurfaceColorSpace::DisplayP3)
        }
        AppUiSurfacePresentationIntent::DisplayOutput(ColorSpace::Rec2100Pq) => {
            Some(wgpu::SurfaceColorSpace::Bt2100Pq)
        }
        AppUiSurfacePresentationIntent::DisplayOutput(ColorSpace::Rec2100Hlg) => {
            Some(wgpu::SurfaceColorSpace::Bt2100Hlg)
        }
        AppUiSurfacePresentationIntent::DisplayOutput(
            ColorSpace::Rec601Pal
            | ColorSpace::Rec601Ntsc
            | ColorSpace::Rec2020
            | ColorSpace::LinearRec709
            | ColorSpace::LinearRec2020
            | ColorSpace::LinearP3D65
            | ColorSpace::Aces2065_1
            | ColorSpace::AcesCg
            | ColorSpace::AcesCct
            | ColorSpace::AppleLogBt2020
            | ColorSpace::SonySLog2SGamut
            | ColorSpace::SonySLog3SGamut3
            | ColorSpace::SonySLog3SGamut3Cine
            | ColorSpace::ArriLogC3WideGamut3
            | ColorSpace::ArriLogC4WideGamut4
            | ColorSpace::CanonLog2CinemaGamutD55
            | ColorSpace::CanonLog3CinemaGamutD55
            | ColorSpace::PanasonicVLogVGamut
            | ColorSpace::RedLog3G10WideGamutRgb
            | ColorSpace::BlackmagicFilmWideGamutGen5
            | ColorSpace::DjiDLogDGamut
            | ColorSpace::DavinciIntermediateWideGamut,
        ) => None,
    }
}

fn choose_app_ui_surface_format_for_color_space(
    capabilities: &wgpu::SurfaceCapabilities,
    color_space: wgpu::SurfaceColorSpace,
) -> Option<wgpu::TextureFormat> {
    if matches!(
        color_space,
        wgpu::SurfaceColorSpace::Srgb | wgpu::SurfaceColorSpace::DisplayP3
    ) {
        return capabilities.formats.iter().copied().find(|format| {
            is_srgb_surface_format(*format)
                && app_ui_surface_format_supports_color_space(capabilities, *format, color_space)
        });
    }

    let preferred_formats = match color_space {
        wgpu::SurfaceColorSpace::Bt2100Pq | wgpu::SurfaceColorSpace::Bt2100Hlg => &[
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureFormat::Rgb10a2Unorm,
        ][..],
        _ => &[wgpu::TextureFormat::Rgba16Float][..],
    };
    preferred_formats
        .iter()
        .copied()
        .find(|format| {
            app_ui_surface_format_supports_color_space(capabilities, *format, color_space)
        })
        .or_else(|| {
            capabilities
                .format_capabilities
                .iter()
                .map(|format_capabilities| format_capabilities.format)
                .find(|format| {
                    app_ui_surface_format_supports_color_space(capabilities, *format, color_space)
                        && app_ui_surface_format_matches_color_space_encoding(*format, color_space)
                })
        })
}

fn app_ui_surface_format_supports_color_space(
    capabilities: &wgpu::SurfaceCapabilities,
    format: wgpu::TextureFormat,
    color_space: wgpu::SurfaceColorSpace,
) -> bool {
    color_space
        .to_color_spaces()
        .is_some_and(|required| capabilities.color_spaces(format).contains(required))
}

fn app_ui_surface_format_matches_color_space_encoding(
    format: wgpu::TextureFormat,
    color_space: wgpu::SurfaceColorSpace,
) -> bool {
    match color_space {
        wgpu::SurfaceColorSpace::Srgb | wgpu::SurfaceColorSpace::DisplayP3 => {
            is_srgb_surface_format(format)
        }
        wgpu::SurfaceColorSpace::Bt2100Pq | wgpu::SurfaceColorSpace::Bt2100Hlg => {
            is_hdr_surface_format(format)
        }
        _ => format == wgpu::TextureFormat::Rgba16Float,
    }
}

fn app_ui_surface_encoding(color_space: wgpu::SurfaceColorSpace) -> AppUiSurfaceEncoding {
    match color_space {
        wgpu::SurfaceColorSpace::Bt2100Pq => AppUiSurfaceEncoding::Pq,
        wgpu::SurfaceColorSpace::Bt2100Hlg => AppUiSurfaceEncoding::Hlg,
        _ => AppUiSurfaceEncoding::Srgb,
    }
}

fn app_ui_surface_hdr_mode(color_space: wgpu::SurfaceColorSpace) -> AppUiSurfaceHdrMode {
    match color_space {
        wgpu::SurfaceColorSpace::Bt2100Pq => AppUiSurfaceHdrMode::HdrPq,
        wgpu::SurfaceColorSpace::Bt2100Hlg => AppUiSurfaceHdrMode::HdrHlg,
        _ => AppUiSurfaceHdrMode::SdrOnly,
    }
}

fn is_srgb_surface_format(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Rgba8UnormSrgb
    )
}

fn is_hdr_surface_format(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Rgba16Float | wgpu::TextureFormat::Rgb10a2Unorm
    )
}

fn app_ui_surface_format_color_spaces(
    capabilities: &wgpu::SurfaceCapabilities,
) -> Vec<AppUiSurfaceFormatColorSpaces> {
    capabilities
        .format_capabilities
        .iter()
        .map(|capability| {
            let color_spaces = capability.color_spaces;
            AppUiSurfaceFormatColorSpaces {
                format: capability.format,
                srgb: color_spaces.contains(wgpu::SurfaceColorSpaces::SRGB),
                extended_srgb_linear: color_spaces
                    .contains(wgpu::SurfaceColorSpaces::EXTENDED_SRGB_LINEAR),
                display_p3: color_spaces.contains(wgpu::SurfaceColorSpaces::DISPLAY_P3),
                bt2100_pq: color_spaces.contains(wgpu::SurfaceColorSpaces::BT2100_PQ),
                bt2100_hlg: color_spaces.contains(wgpu::SurfaceColorSpaces::BT2100_HLG),
                extended_srgb: color_spaces.contains(wgpu::SurfaceColorSpaces::EXTENDED_SRGB),
                extended_display_p3: color_spaces
                    .contains(wgpu::SurfaceColorSpaces::EXTENDED_DISPLAY_P3),
            }
        })
        .collect()
}

fn app_ui_display_target_for_window(window: &winit::window::Window) -> AppUiDisplayTarget {
    let Some(monitor) = window.current_monitor() else {
        return AppUiDisplayTarget {
            name: None,
            position: (0, 0),
            physical_size: (0, 0),
            native_display_id: None,
            native_display_path_id: None,
            scale_factor_ppm: 0,
            refresh_rate_millihertz: None,
        };
    };
    let position = monitor.position();
    let size = monitor.size();
    let scale_factor_ppm =
        (monitor.scale_factor() * 1_000_000.0).round().clamp(0.0, u32::MAX as f64) as u32;
    AppUiDisplayTarget {
        name: monitor.name(),
        position: (position.x, position.y),
        physical_size: (size.width, size.height),
        native_display_id: app_ui_monitor_native_display_id(&monitor),
        native_display_path_id: app_ui_monitor_native_display_path_id(&monitor),
        scale_factor_ppm,
        refresh_rate_millihertz: monitor.refresh_rate_millihertz(),
    }
}

#[cfg(target_os = "macos")]
fn app_ui_monitor_native_display_id(monitor: &winit::monitor::MonitorHandle) -> Option<u64> {
    use winit::platform::macos::MonitorHandleExtMacOS as _;
    Some(u64::from(monitor.native_id()))
}

#[cfg(not(target_os = "macos"))]
fn app_ui_monitor_native_display_id(_monitor: &winit::monitor::MonitorHandle) -> Option<u64> {
    None
}

#[cfg(target_os = "windows")]
fn app_ui_monitor_native_display_path_id(
    monitor: &winit::monitor::MonitorHandle,
) -> Option<String> {
    use winit::platform::windows::MonitorHandleExtWindows as _;
    let value = monitor.native_id();
    (!value.trim().is_empty()).then_some(value)
}

#[cfg(target_os = "macos")]
fn app_ui_monitor_native_display_path_id(
    monitor: &winit::monitor::MonitorHandle,
) -> Option<String> {
    use winit::platform::macos::MonitorHandleExtMacOS as _;
    Some(monitor.native_id().to_string())
}

#[cfg(target_os = "linux")]
fn app_ui_monitor_native_display_path_id(
    monitor: &winit::monitor::MonitorHandle,
) -> Option<String> {
    use winit::platform::x11::MonitorHandleExtX11 as _;
    Some(monitor.native_id().to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn app_ui_monitor_native_display_path_id(
    _monitor: &winit::monitor::MonitorHandle,
) -> Option<String> {
    None
}

fn log_backend_event(event: AppUiBackendEvent) {
    match event {
        AppUiBackendEvent::SurfaceSuboptimal
        | AppUiBackendEvent::SurfaceOutdated
        | AppUiBackendEvent::SurfaceLost
        | AppUiBackendEvent::SurfaceUnavailable => {
            tracing::warn!(?event, "app UI render backend fallback")
        }
        AppUiBackendEvent::SurfaceTimeout | AppUiBackendEvent::SurfaceOccluded => {
            tracing::debug!(?event, "app UI render backend skipped frame")
        }
    }
}

fn log_frame_pressure(pressure: AppUiFramePressure) {
    let metrics = pressure.metrics;
    if pressure.high_upload || pressure.image_atlas_pressure {
        tracing::warn!(
            slow_frame = pressure.slow_frame,
            high_upload = pressure.high_upload,
            image_atlas_pressure = pressure.image_atlas_pressure,
            frame_cpu_time_micros = metrics.frame_cpu_time_micros,
            renderer_cpu_time_micros = metrics.renderer_cpu_time_micros,
            total_upload_bytes = metrics.total_upload_bytes,
            glyph_upload_bytes = metrics.glyph_upload_bytes,
            raster_image_upload_bytes = metrics.raster_image_upload_bytes,
            renderer_upload_bytes = metrics.renderer_upload_bytes,
            command_count = metrics.command_count,
            batch_count = metrics.batch_count,
            vertex_count = metrics.vertex_count,
            image_atlas_entries = metrics.image_atlas_entries,
            image_atlas_occupancy_bps = metrics.image_atlas_occupancy_bps,
            image_atlas_largest_free_rect_pixels = metrics.image_atlas_largest_free_rect_pixels,
            image_atlas_page_resets_this_frame = metrics.image_atlas_page_resets_this_frame,
            image_atlas_failed_allocations = metrics.image_atlas_failed_allocations,
            external_texture_entries = metrics.external_texture_entries,
            external_texture_failures = metrics.external_texture_failures,
            external_texture_batches = metrics.external_texture_batches,
            "app UI render frame pressure"
        );
        return;
    }

    tracing::debug!(
        slow_frame = pressure.slow_frame,
        high_upload = pressure.high_upload,
        image_atlas_pressure = pressure.image_atlas_pressure,
        frame_cpu_time_micros = metrics.frame_cpu_time_micros,
        renderer_cpu_time_micros = metrics.renderer_cpu_time_micros,
        total_upload_bytes = metrics.total_upload_bytes,
        glyph_upload_bytes = metrics.glyph_upload_bytes,
        raster_image_upload_bytes = metrics.raster_image_upload_bytes,
        renderer_upload_bytes = metrics.renderer_upload_bytes,
        command_count = metrics.command_count,
        batch_count = metrics.batch_count,
        vertex_count = metrics.vertex_count,
        image_atlas_entries = metrics.image_atlas_entries,
        image_atlas_occupancy_bps = metrics.image_atlas_occupancy_bps,
        image_atlas_largest_free_rect_pixels = metrics.image_atlas_largest_free_rect_pixels,
        image_atlas_page_resets_this_frame = metrics.image_atlas_page_resets_this_frame,
        image_atlas_failed_allocations = metrics.image_atlas_failed_allocations,
        external_texture_entries = metrics.external_texture_entries,
        external_texture_failures = metrics.external_texture_failures,
        external_texture_batches = metrics.external_texture_batches,
        "app UI render frame pressure"
    );
}

fn trace_color_output_runtime(
    diagnostics: RenderGpuOutputBoundaryRuntimeDiagnostics,
    display_output_contract: &AppUiDisplayOutputContract,
) {
    let surface_color_contract = display_output_contract.surface_color;
    tracing::trace!(
        surface_format = ?surface_color_contract.format,
        surface_color_space = ?surface_color_contract.color_space,
        surface_encoding = ?surface_color_contract.encoding,
        surface_hdr_mode = ?surface_color_contract.hdr_mode,
        display_name = ?display_output_contract.display_target.name,
        display_position = ?display_output_contract.display_target.position,
        display_physical_size = ?display_output_contract.display_target.physical_size,
        display_scale_factor_ppm = display_output_contract.display_target.scale_factor_ppm,
        display_refresh_rate_millihertz =
            ?display_output_contract.display_target.refresh_rate_millihertz,
        display_hdr_info = ?display_output_contract.display_hdr_info,
        display_tone_map_headroom =
            ?display_output_contract.display_hdr_info.tone_map_headroom(),
        available_surface_format_count = display_output_contract.available_formats.len(),
        format_color_space_count = display_output_contract.format_color_spaces.len(),
        format_color_spaces = ?display_output_contract.format_color_spaces,
        present_modes = ?display_output_contract.present_modes,
        alpha_modes = ?display_output_contract.alpha_modes,
        shader_cache_entries = diagnostics.shader_cache.entries,
        shader_cache_hits = diagnostics.shader_cache.hits,
        shader_cache_misses = diagnostics.shader_cache.misses,
        shader_cache_extraction_failures = diagnostics.shader_cache.extraction_failures,
        backend_prep_resource_entries = diagnostics.backend_prep.resources.entries,
        backend_object_entries = diagnostics.backend_objects.entries,
        backend_object_hits = diagnostics.backend_objects.hits,
        backend_object_misses = diagnostics.backend_objects.misses,
        backend_object_failures = diagnostics.backend_objects.failures,
        frame_table_entries = diagnostics.frame_table_entries,
        next_frame_id = diagnostics.next_frame_id,
        "app UI GPU output color runtime diagnostics"
    );
}

fn viewer_gpu_output_diagnostics(
    host: &AppUiHost,
    telemetry: &AppUiViewerGpuOutputTelemetry,
    display_target: &AppUiDisplayTarget,
    runtime_report: RenderGpuOutputRuntimeDiagnosticsReport,
    display_snapshot: Option<&mondrian_core::display_contract::DisplayOutputSnapshot>,
    display_calibration: Option<&mondrian_core::display_calibration::DisplayCalibrationLut3d>,
    display_calibration_rendering_intent: Option<mondrian_core::IccRenderingIntent>,
    renderer_adapter: Option<&AppUiRendererAdapterDiagnostics>,
    frame_metrics: AppUiFrameMetrics,
) -> AppUiViewerGpuOutputDiagnostics {
    let mut diagnostics = telemetry.diagnostics(runtime_report);
    if viewer_gpu_output_diagnostics_output_path().is_some() {
        diagnostics.qualification_run_id = viewer_qualification_run_id();
        diagnostics.process_instance_id = Some(viewer_process_instance_id().to_owned());
        diagnostics.process_id = Some(std::process::id());
        diagnostics.qualification_record_sequence = Some(next_viewer_qualification_record());
        diagnostics.runtime_image_sha256 = viewer_runtime_image_sha256();
    }
    diagnostics.last_color_rejection = host.current_viewer_color_rejection();
    if let Some(issue) = diagnostics.display_issue_summary.as_mut() {
        issue.display_target = Some(display_target.clone());
    }
    diagnostics.display_snapshot = display_snapshot.map(DisplaySnapshotDiagnostics::from_snapshot);
    diagnostics.display_output_contract = display_snapshot.cloned();
    diagnostics.display_calibration_identity_sha256 = display_calibration.map(|calibration| {
        calibration
            .identity()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join("")
    });
    diagnostics.display_calibration_rendering_intent =
        display_calibration.map(|_| display_calibration_rendering_intent.unwrap_or_default());
    diagnostics.renderer_adapter = renderer_adapter.cloned();
    diagnostics.display_target = Some(display_target.clone());
    diagnostics.ui_surface_carrier_active = frame_metrics.surface_carrier_active;
    diagnostics.ui_surface_carrier_target_rebuilt = frame_metrics.surface_carrier_target_rebuilt;
    diagnostics.presented_external_texture_batches = frame_metrics.external_texture_batches;
    diagnostics.presented_surface_code_value_batches = frame_metrics.surface_code_value_batches;
    diagnostics.presented_device_code_value_batches = frame_metrics.device_code_value_batches;
    diagnostics
}

fn trace_viewer_gpu_output_telemetry(
    host: &AppUiHost,
    telemetry: &AppUiViewerGpuOutputTelemetry,
    display_target: &AppUiDisplayTarget,
    runtime_report: RenderGpuOutputRuntimeDiagnosticsReport,
    display_snapshot: Option<&mondrian_core::display_contract::DisplayOutputSnapshot>,
    display_calibration: Option<&mondrian_core::display_calibration::DisplayCalibrationLut3d>,
    display_calibration_rendering_intent: Option<mondrian_core::IccRenderingIntent>,
    renderer_adapter: Option<&AppUiRendererAdapterDiagnostics>,
    frame_metrics: AppUiFrameMetrics,
) {
    let diagnostics = viewer_gpu_output_diagnostics(
        host,
        telemetry,
        display_target,
        runtime_report,
        display_snapshot,
        display_calibration,
        display_calibration_rendering_intent,
        renderer_adapter,
        frame_metrics,
    );
    tracing::trace!(
        invocations = diagnostics.invocations,
        non_workspace_skips = diagnostics.non_workspace_skips,
        current_skips = diagnostics.current_skips,
        loading_skips = diagnostics.loading_skips,
        unavailable_skips = diagnostics.unavailable_skips,
        invalid_texture_keys = diagnostics.invalid_texture_keys,
        display_contract_blockers = diagnostics.display_contract_blockers,
        display_contract_hdr_surface_blockers = diagnostics.display_contract_hdr_surface_blockers,
        display_contract_surface_color_space_blockers =
            diagnostics.display_contract_surface_color_space_blockers,
        display_presentation_reconfigure_candidates =
            diagnostics.display_presentation_reconfigure_candidates,
        display_presentation_payload_blockers = diagnostics.display_presentation_payload_blockers,
        display_presentation_unsupported_contracts =
            diagnostics.display_presentation_unsupported_contracts,
        ui_surface_carrier_active = diagnostics.ui_surface_carrier_active,
        ui_surface_carrier_target_rebuilt = diagnostics.ui_surface_carrier_target_rebuilt,
        presented_external_texture_batches = diagnostics.presented_external_texture_batches,
        presented_surface_code_value_batches = diagnostics.presented_surface_code_value_batches,
        presented_device_code_value_batches = diagnostics.presented_device_code_value_batches,
        creative_lut_cache_hits = diagnostics
            .compositor_creative_luts
            .map_or(0, |value| value.cache_hits),
        creative_lut_cache_misses = diagnostics
            .compositor_creative_luts
            .map_or(0, |value| value.cache_misses),
        creative_lut_texture_uploads = diagnostics
            .compositor_creative_luts
            .map_or(0, |value| value.texture_uploads),
        creative_lut_resident_entries = diagnostics
            .compositor_creative_luts
            .map_or(0, |value| value.resident_entries),
        creative_lut_resident_texture_bytes = diagnostics
            .compositor_creative_luts
            .map_or(0, |value| value.resident_texture_bytes),
        record_failures = diagnostics.record_failures,
        missing_output_textures = diagnostics.missing_output_textures,
        registered_frames = diagnostics.registered_frames,
        rejected_external_frames = diagnostics.rejected_external_frames,
        stage_total_stages = diagnostics.stage_total_stages,
        stage_upload_stages = diagnostics.stage_upload_stages,
        stage_gpu_color_stages = diagnostics.stage_gpu_color_stages,
        stage_readback_stages = diagnostics.stage_readback_stages,
        stage_gpu_blockers = diagnostics.stage_gpu_blockers,
        stage_gpu_shader_module_blockers = diagnostics.stage_gpu_shader_module_blockers,
        stage_gpu_ocio_resource_blockers = diagnostics.stage_gpu_ocio_resource_blockers,
        stage_gpu_wrapper_blockers = diagnostics.stage_gpu_wrapper_blockers,
        stage_gpu_render_pipeline_blockers = diagnostics.stage_gpu_render_pipeline_blockers,
        stage_pixels = diagnostics.stage_pixels,
        viewer_output_health_status = ?diagnostics.health.status,
        viewer_output_ready = diagnostics.health.viewer_output_ready,
        native_gpu_boundary_ready = diagnostics.health.native_gpu_boundary_ready,
        display_boundary_ready = diagnostics.health.display_boundary_ready,
        presentation_ready = diagnostics.health.presentation_ready,
        stage_sequence_ready = diagnostics.health.stage_sequence_ready,
        no_gpu_blockers = diagnostics.health.no_gpu_blockers,
        output_texture_available = diagnostics.health.output_texture_available,
        external_texture_registered = diagnostics.health.external_texture_registered,
        health_count_waiting = diagnostics.health_counts.waiting,
        health_count_blocked = diagnostics.health_counts.blocked,
        health_count_failed = diagnostics.health_counts.failed,
        health_count_rejected = diagnostics.health_counts.rejected,
        health_count_degraded = diagnostics.health_counts.degraded,
        health_count_ready = diagnostics.health_counts.ready,
        last_preview_candidate_id = diagnostics.last_preview_candidate_id,
        last_preview_candidate_state = ?diagnostics.last_preview_candidate_state,
        last_frame_context = ?diagnostics.last_frame_context,
        last_color_rejection = ?diagnostics.last_color_rejection,
        last_display_contract_blocker = ?diagnostics.last_display_contract_blocker,
        last_display_presentation_readiness = ?diagnostics.last_display_presentation_readiness,
        display_issue_summary = ?diagnostics.display_issue_summary,
        last_outcome = ?diagnostics.last_outcome,
        "app UI viewer GPU output telemetry"
    );
    write_viewer_gpu_output_diagnostics_if_needed(&diagnostics);
}

fn write_viewer_gpu_output_diagnostics_if_needed(diagnostics: &AppUiViewerGpuOutputDiagnostics) {
    let Some(path) = viewer_gpu_output_diagnostics_output_path() else {
        return;
    };
    let result = write_viewer_gpu_output_diagnostics_to_path(&path, diagnostics);
    if let Err(err) = result {
        tracing::warn!(
            output_path = %path.display(),
            "failed to write viewer GPU output diagnostics JSONL: {err:?}"
        );
    }
}

fn write_viewer_gpu_output_diagnostics_to_path(
    path: &Path,
    diagnostics: &AppUiViewerGpuOutputDiagnostics,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let report_json = serde_json::to_string(diagnostics)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{report_json}")?;
    Ok(())
}

fn viewer_gpu_output_diagnostics_output_path() -> Option<PathBuf> {
    std::env::var_os(VIEWER_GPU_OUTPUT_DIAGNOSTICS_OUTPUT_ENV).map(PathBuf::from)
}

fn viewer_qualification_run_id() -> Option<String> {
    std::env::var(VIEWER_QUALIFICATION_RUN_ID_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn viewer_process_instance_id() -> &'static str {
    static PROCESS_INSTANCE_ID: OnceLock<String> = OnceLock::new();
    PROCESS_INSTANCE_ID.get_or_init(|| uuid::Uuid::new_v4().to_string()).as_str()
}

fn next_viewer_qualification_record() -> u64 {
    static RECORD_SEQUENCE: AtomicU64 = AtomicU64::new(1);
    RECORD_SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

fn viewer_runtime_image_sha256() -> Option<String> {
    static RUNTIME_IMAGE_SHA256: OnceLock<Option<String>> = OnceLock::new();
    RUNTIME_IMAGE_SHA256
        .get_or_init(|| {
            #[cfg(target_os = "linux")]
            let mut file = File::open("/proc/self/exe").ok()?;
            #[cfg(not(target_os = "linux"))]
            let mut file = File::open(std::env::current_exe().ok()?).ok()?;
            let mut buffer = [0_u8; 128 * 1024];
            let mut hasher = Sha256::new();
            loop {
                let read = file.read(&mut buffer).ok()?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            Some(format!("{:x}", hasher.finalize()))
        })
        .clone()
}

fn app_ui_interactive_playback_wake_delay(host: &AppUiHost, delay: Duration) -> Duration {
    if host.is_playback_frame_pending() {
        delay.min(APP_UI_BUFFERING_INTERACTIVE_WAKE_DELAY)
    } else {
        delay
    }
}

/// Drive the exact in-flight heterogeneous Viewer completion and discard any
/// late callbacks from previously abandoned submissions.
///
/// Returns `true` while an exact candidate is still in flight, in which case
/// the caller must not clear renderer frame resources or issue another Viewer
/// submission.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ViewerHeterogeneousCompletionDrive {
    redraw_required: bool,
    next_wake: Option<Instant>,
}

fn defer_viewer_gpu_cleanup(
    session: &mut AppUiWindowSession,
    requested: WindowViewerGpuDeferredCleanup,
) {
    session.viewer_gpu_deferred_cleanup = match (session.viewer_gpu_deferred_cleanup, requested) {
        (WindowViewerGpuDeferredCleanup::Reset, _) | (_, WindowViewerGpuDeferredCleanup::Reset) => {
            WindowViewerGpuDeferredCleanup::Reset
        }
        (WindowViewerGpuDeferredCleanup::ClearFrameResources, _)
        | (_, WindowViewerGpuDeferredCleanup::ClearFrameResources) => {
            WindowViewerGpuDeferredCleanup::ClearFrameResources
        }
        _ => WindowViewerGpuDeferredCleanup::None,
    };
}

fn apply_deferred_viewer_gpu_cleanup(session: &mut AppUiWindowSession) {
    if session.viewer_gpu_submissions.is_occupied() {
        return;
    }
    match std::mem::take(&mut session.viewer_gpu_deferred_cleanup) {
        WindowViewerGpuDeferredCleanup::None => {}
        WindowViewerGpuDeferredCleanup::ClearFrameResources => {
            session.viewer_gpu_execution.clear_frame_resources();
        }
        WindowViewerGpuDeferredCleanup::Reset => session.viewer_gpu_execution.reset(),
    }
}

fn retire_window_viewer_gpu_registration(
    session: &mut AppUiWindowSession,
    _host: &AppUiHost,
    owner: &mut WindowViewerGpuSubmissionOwner,
) {
    if owner.texture_registered {
        owner.texture_registered = false;
        session.frame_renderer.unregister_external_texture(&owner.texture_key);
    }
}

fn retire_window_published_gpu_output(session: &mut AppUiWindowSession, host: &AppUiHost) -> bool {
    let outputs = session.viewer_gpu_presentation.publications.drain();
    let mut retired = false;
    for published in outputs.into_iter().flatten() {
        retired = true;
        session.frame_renderer.unregister_external_texture(published.artifact());
        let _ = host.clear_external_viewer_frame_for_artifact(
            published.output_key(),
            published.artifact().as_str(),
        );
        drop(published);
    }
    retired
}

fn begin_window_viewer_gpu_quarantine(
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
    quarantine: ViewerGpuSubmissionQuarantine,
    cleanup: WindowViewerGpuDeferredCleanup,
) {
    if let Some(published) = session
        .viewer_gpu_presentation
        .take_published_output_for_submission(quarantine.submission_id)
    {
        session.frame_renderer.unregister_external_texture(published.artifact());
        let _ = host.clear_external_viewer_frame_for_artifact(
            published.output_key(),
            published.artifact().as_str(),
        );
        drop(published);
    }
    if let Some(owner) = session.viewer_gpu_submissions.owner_mut(quarantine.submission_id) {
        if owner.texture_registered {
            owner.texture_registered = false;
            session.frame_renderer.unregister_external_texture(&owner.texture_key);
        }
        if let Some(terminal) = owner.terminal.take() {
            let _ = host.fail_heterogeneous_viewer_gpu(terminal);
        }
    }
    defer_viewer_gpu_cleanup(session, cleanup);
    tracing::warn!(
        submission_id = quarantine.submission_id.get(),
        reason = ?quarantine.reason,
        "Viewer GPU submission entered retirement-only quarantine"
    );
}

fn publish_completed_window_viewer_gpu_owner(
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
    submission_id: ViewerGpuSubmissionId,
    owner: &mut WindowViewerGpuSubmissionOwner,
) -> bool {
    if let Some(terminal) = session.viewer_gpu_device_progress.generation_terminal() {
        tracing::warn!(
            submission_id = submission_id.get(),
            reason = terminal.reason,
            "completed Window Viewer output was not published from a terminal device generation"
        );
        retire_window_viewer_gpu_registration(session, host, owner);
        return false;
    }
    if !owner.texture_registered {
        tracing::error!(
            submission_id = submission_id.get(),
            "completed Window publication has no registered texture artifact"
        );
        return false;
    }
    let Some(output_lease) = owner.output_lease.take() else {
        tracing::error!(
            submission_id = submission_id.get(),
            "completed Window publication has no physical output lease"
        );
        retire_window_viewer_gpu_registration(session, host, owner);
        return false;
    };
    let disposition = host.set_external_viewer_frame(
        &owner.frame,
        owner.texture_key.as_str().to_owned(),
        owner.presentation,
    );
    if let Some(terminal) = session.viewer_gpu_device_progress.generation_terminal() {
        let _ = host.clear_external_viewer_frame_for_artifact(
            &owner.frame.output_key,
            owner.texture_key.as_str(),
        );
        retire_window_viewer_gpu_registration(session, host, owner);
        drop(output_lease);
        tracing::warn!(
            submission_id = submission_id.get(),
            reason = terminal.reason,
            "completed Window Viewer publication raced a terminal device generation and was revoked"
        );
        return false;
    }
    match disposition {
        FramePresentationDisposition::Presented(_) | FramePresentationDisposition::NoDemand => {
            let previous = session.viewer_gpu_presentation.publications.publish_current(
                submission_id,
                owner.frame.output_key.clone(),
                owner.texture_key.clone(),
                output_lease,
            );
            owner.texture_registered = false;
            if let Some(previous) = previous {
                session.frame_renderer.unregister_external_texture(previous.artifact());
                drop(previous);
            }
            session
                .viewer_gpu_output_telemetry
                .record_registered_frame(owner.stage_diagnostics);
            true
        }
        FramePresentationDisposition::DroppedLate(_) => {
            retire_window_viewer_gpu_registration(session, host, owner);
            drop(output_lease);
            false
        }
        FramePresentationDisposition::OutputRejected
        | FramePresentationDisposition::LostAuthority => {
            session
                .viewer_gpu_output_telemetry
                .record_rejected_external_frame(owner.stage_diagnostics);
            retire_window_viewer_gpu_registration(session, host, owner);
            drop(output_lease);
            false
        }
    }
}

fn poll_viewer_heterogeneous_completion(
    device: &wgpu::Device,
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
) -> ViewerHeterogeneousCompletionPoll {
    let now = Instant::now();
    let mut observation_time = now;
    let mut device_failure = None;
    let mut callback_barrier_observed = false;
    while let Some(observation) = session.viewer_gpu_device_progress.try_observe() {
        match observation {
            ViewerGpuDeviceProgressObservation::WaitSatisfied { submission_id, observed_at } => {
                // Callback retirement can open the next slot before this
                // non-authoritative observation is drained. Never let the old
                // physical identity affect a replacement lifecycle.
                if session.viewer_gpu_submissions.contains(submission_id) {
                    observation_time = observation_time.max(observed_at);
                    callback_barrier_observed = true;
                }
            }
            ViewerGpuDeviceProgressObservation::RendererCleanupSatisfied { .. } => {
                // The wake opens a retry opportunity; this cleanup barrier has
                // no semantic completion or lifecycle-deadline authority.
            }
            ViewerGpuDeviceProgressObservation::DevicePollFailed {
                submission_id,
                reason,
                observed_at,
            } => {
                observation_time = observation_time.max(observed_at);
                device_failure = Some((Some(submission_id), reason));
            }
        }
    }
    // The callback-installed terminal is authoritative even with no active
    // submission and even if a work-done observation arrived from the same
    // `Device::poll`. This also drives revocation of an ordinary current slot.
    if let Some(terminal) = session.viewer_gpu_device_progress.generation_terminal() {
        observation_time = observation_time.max(terminal.observed_at);
        device_failure = Some((terminal.submission_id, terminal.reason));
    }
    let mut submission_poll = if callback_barrier_observed {
        session.viewer_gpu_submissions.poll(observation_time)
    } else {
        session.viewer_gpu_submissions.poll_deadline_only(observation_time)
    };
    if let ViewerGpuSubmissionPoll::Completed(completed) = &mut submission_poll {
        if let Some((failed_submission_id, error)) = device_failure {
            let failure_context =
                window_viewer_gpu_generation_failure_context(failed_submission_id);
            if completed.quarantine_reason.is_none() {
                completed.quarantine_reason =
                    Some(ViewerGpuSubmissionQuarantineReason::DevicePollFailed(
                        format!("device generation terminal {failure_context}: {error}"),
                    ));
            }
            let fallback_reason = format!(
                "Viewer GPU device generation failed {failure_context} while completing submission {}; GPU output remains disabled until the generation is rebuilt: {error}",
                completed.submission_id.get()
            );
            host.record_preview_gpu_output_blocker(
                &PreviewGpuOutputBlocker::CpuFallbackRequested { reason: fallback_reason.clone() },
            );
            host.request_viewer_cpu_fallback(fallback_reason);
            unregister_program_scopes_textures(session);
            if !retire_window_published_gpu_output(session, host) {
                host.clear_external_viewer_frame();
            }
        }
        return resolve_window_viewer_gpu_submission_poll(session, host, device, submission_poll);
    }
    if let ViewerGpuSubmissionPoll::Pending { submission_id, .. } = &submission_poll {
        if let Some((failed_submission_id, error)) = device_failure {
            let failure_context =
                window_viewer_gpu_generation_failure_context(failed_submission_id);
            let generation_reason = format!(
                "device generation terminal {failure_context} while submission {} remained active: {error}",
                submission_id.get()
            );
            let quarantines = session
                .viewer_gpu_submissions
                .quarantine_all_after_device_failure(generation_reason);
            if let Some(first) = quarantines.first() {
                let fallback_reason = format!(
                    "Viewer GPU device generation failed while driving submission {}; GPU output remains disabled until the generation is rebuilt: {error}",
                    first.submission_id.get()
                );
                host.record_preview_gpu_output_blocker(
                    &PreviewGpuOutputBlocker::CpuFallbackRequested {
                        reason: fallback_reason.clone(),
                    },
                );
                host.request_viewer_cpu_fallback(fallback_reason);
                for quarantine in quarantines {
                    begin_window_viewer_gpu_quarantine(
                        session,
                        host,
                        quarantine,
                        WindowViewerGpuDeferredCleanup::Reset,
                    );
                }
                unregister_program_scopes_textures(session);
                if !retire_window_published_gpu_output(session, host) {
                    host.clear_external_viewer_frame();
                }
                return ViewerHeterogeneousCompletionPoll::TerminalChange;
            }
        }
    } else if let Some((failed_submission_id, error)) = device_failure {
        let failure_context = window_viewer_gpu_generation_failure_context(failed_submission_id);
        let fallback_reason = format!(
            "Viewer GPU device generation failed {failure_context}; GPU output remains disabled until the generation is rebuilt: {error}"
        );
        host.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::CpuFallbackRequested {
            reason: fallback_reason.clone(),
        });
        host.request_viewer_cpu_fallback(fallback_reason);
        unregister_program_scopes_textures(session);
        if !retire_window_published_gpu_output(session, host) {
            host.clear_external_viewer_frame();
        }
    }
    resolve_window_viewer_gpu_submission_poll(session, host, device, submission_poll)
}

fn window_viewer_gpu_generation_failure_context(
    submission_id: Option<ViewerGpuSubmissionId>,
) -> String {
    submission_id.map_or_else(
        || "outside an active Viewer submission".to_owned(),
        |submission_id| format!("after submission {}", submission_id.get()),
    )
}

fn resolve_window_viewer_gpu_submission_poll(
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
    device: &wgpu::Device,
    poll: ViewerGpuSubmissionPoll<
        WindowViewerGpuSubmissionOwner,
        ViewerHeterogeneousGpuCompletedBatch,
    >,
) -> ViewerHeterogeneousCompletionPoll {
    match poll {
        ViewerGpuSubmissionPoll::Idle => {
            apply_deferred_viewer_gpu_cleanup(session);
            ViewerHeterogeneousCompletionPoll::Idle
        }
        ViewerGpuSubmissionPoll::Pending { .. } => ViewerHeterogeneousCompletionPoll::Pending,
        ViewerGpuSubmissionPoll::QuarantineStarted(quarantine) => {
            host.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::UnsupportedFeature {
                feature: "heterogeneous_viewer_gpu_completion_timeout".to_owned(),
                reason: format!(
                    "Viewer GPU submission {} exceeded its non-renewing {:?} completion deadline",
                    quarantine.submission_id.get(),
                    VIEWER_HETEROGENEOUS_COMPLETION_TIMEOUT,
                ),
            });
            begin_window_viewer_gpu_quarantine(
                session,
                host,
                quarantine,
                WindowViewerGpuDeferredCleanup::ClearFrameResources,
            );
            ViewerHeterogeneousCompletionPoll::TerminalChange
        }
        ViewerGpuSubmissionPoll::RetiredAfterQuarantine(retired) => {
            // The exact completion callback never arrived within the bounded
            // grace after quarantine. Retire the owner through the same
            // retirement path without callback evidence.
            let quarantine = ViewerGpuSubmissionQuarantine {
                submission_id: retired.submission_id,
                reason: retired.reason,
            };
            begin_window_viewer_gpu_quarantine(
                session,
                host,
                quarantine,
                WindowViewerGpuDeferredCleanup::ClearFrameResources,
            );
            let mut owner = retired.owner;
            if owner.texture_registered {
                owner.texture_registered = false;
                session.frame_renderer.unregister_external_texture(&owner.texture_key);
            }
            if let Some(terminal) = owner.terminal.take() {
                let _ = host.fail_heterogeneous_viewer_gpu(terminal);
            }
            ViewerHeterogeneousCompletionPoll::TerminalChange
        }
        ViewerGpuSubmissionPoll::Completed(completed) => {
            complete_window_viewer_gpu_submission(session, host, device, completed);
            apply_deferred_viewer_gpu_cleanup(session);
            ViewerHeterogeneousCompletionPoll::TerminalChange
        }
    }
}

fn complete_window_viewer_gpu_submission(
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
    device: &wgpu::Device,
    completed: ViewerGpuCompletedSubmission<
        WindowViewerGpuSubmissionOwner,
        ViewerHeterogeneousGpuCompletedBatch,
    >,
) {
    let ViewerGpuCompletedSubmission {
        submission_id,
        mut owner,
        completion,
        mut quarantine_reason,
        ..
    } = completed;
    if let Some(terminal) = session.viewer_gpu_device_progress.generation_terminal() {
        quarantine_reason.get_or_insert_with(|| {
            ViewerGpuSubmissionQuarantineReason::DevicePollFailed(format!(
                "device generation terminal {}: {}",
                window_viewer_gpu_generation_failure_context(terminal.submission_id),
                terminal.reason
            ))
        });
    }
    if quarantine_reason.is_some() {
        if let Some(published) = session
            .viewer_gpu_presentation
            .take_published_output_for_submission(submission_id)
        {
            session.frame_renderer.unregister_external_texture(published.artifact());
            let _ = host.clear_external_viewer_frame_for_artifact(
                published.output_key(),
                published.artifact().as_str(),
            );
            drop(published);
        }
        retire_window_viewer_gpu_registration(session, host, &mut owner);
        if let Some(terminal) = owner.terminal.take() {
            let _ = host.fail_heterogeneous_viewer_gpu(terminal);
        }
        return;
    }
    let Some(terminal) = owner.terminal.take() else {
        // Ordinary publication is queue-ordered. Its lease already moved into
        // the current physical slot on success; this callback only retires the
        // submitted frame/media owner.
        retire_window_viewer_gpu_registration(session, host, &mut owner);
        return;
    };
    match host.finalize_heterogeneous_viewer_gpu(
        terminal,
        &completion,
        owner.frame.is_successor_preparation(),
    ) {
        Ok(PreviewVisualGpuCompletionDisposition::PublishCurrent) => {
            if let Some(scopes) = owner.program_scopes.as_ref() {
                if let Err(error) = register_program_scopes_textures(
                    session,
                    device,
                    scopes,
                    owner.program_scopes_analysis_identity,
                ) {
                    unregister_program_scopes_textures(session);
                    session.program_scopes_refresh_requested = true;
                    tracing::warn!(%error, "heterogeneous GPU scope registration failed");
                }
            } else {
                unregister_program_scopes_textures(session);
                session.program_scopes_refresh_requested = owner.program_scopes_requested;
            }
            let _ =
                publish_completed_window_viewer_gpu_owner(session, host, submission_id, &mut owner);
        }
        Ok(PreviewVisualGpuCompletionDisposition::PrepareSuccessor) => {
            let _ = retain_completed_window_viewer_gpu_successor(
                session,
                host,
                submission_id,
                &mut owner,
            );
        }
        Ok(
            PreviewVisualGpuCompletionDisposition::Release
            | PreviewVisualGpuCompletionDisposition::TerminalCandidate(_),
        ) => retire_window_viewer_gpu_registration(session, host, &mut owner),
        Err(error) => {
            retire_window_viewer_gpu_registration(session, host, &mut owner);
            host.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::UnsupportedFeature {
                feature: "heterogeneous_viewer_gpu_evidence".to_owned(),
                reason: error.clone(),
            });
            tracing::warn!("heterogeneous Viewer completion evidence rejected: {error}");
        }
    }
}

fn retain_completed_window_viewer_gpu_successor(
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
    submission_id: ViewerGpuSubmissionId,
    owner: &mut WindowViewerGpuSubmissionOwner,
) -> bool {
    if !owner.frame.is_successor_preparation()
        || owner.frame.presentation_ticket().is_some()
        || !owner.texture_registered
    {
        tracing::error!(
            submission_id = submission_id.get(),
            "completed heterogeneous successor has an invalid publication contract"
        );
        retire_window_viewer_gpu_registration(session, host, owner);
        return false;
    }
    let Some(output_lease) = owner.output_lease.take() else {
        tracing::error!(
            submission_id = submission_id.get(),
            "completed heterogeneous successor has no physical output lease"
        );
        retire_window_viewer_gpu_registration(session, host, owner);
        return false;
    };
    let Some(visible_output) = mondrian_ui_widgets::ViewerExternalTextureFrame::new_spatial(
        owner.texture_key.as_str().to_owned(),
        owner.presentation,
    ) else {
        retire_window_viewer_gpu_registration(session, host, owner);
        drop(output_lease);
        return false;
    };
    host.register_prepared_viewer_gpu_successor(&owner.frame, visible_output);
    if session.viewer_gpu_device_progress.generation_terminal().is_some() {
        let _ = host.clear_external_viewer_frame_for_artifact(
            &owner.frame.output_key,
            owner.texture_key.as_str(),
        );
        retire_window_viewer_gpu_registration(session, host, owner);
        drop(output_lease);
        return false;
    }
    owner.texture_registered = false;
    if let Some(previous) = session.viewer_gpu_presentation.publications.publish_prepared(
        submission_id,
        owner.frame.output_key.clone(),
        owner.texture_key.clone(),
        output_lease,
    ) {
        session.frame_renderer.unregister_external_texture(previous.artifact());
        let _ = host.clear_external_viewer_frame_for_artifact(
            previous.output_key(),
            previous.artifact().as_str(),
        );
        drop(previous);
    }
    session
        .viewer_gpu_output_telemetry
        .record_registered_frame(owner.stage_diagnostics);
    let _ = host.observe_prepared_viewer_gpu_preroll();
    true
}

fn drive_viewer_heterogeneous_completion(
    device: &wgpu::Device,
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
    now: Instant,
) -> ViewerHeterogeneousCompletionDrive {
    let poll = poll_viewer_heterogeneous_completion(device, session, host);
    ViewerHeterogeneousCompletionDrive {
        redraw_required: poll == ViewerHeterogeneousCompletionPoll::TerminalChange,
        next_wake: session.viewer_gpu_submissions.next_wake().map(|wake| wake.max(now)),
    }
}

fn fail_viewer_gpu_frame(host: &AppUiHost, frame: &mut PreviewGpuFrame) {
    if let Some(execution) = frame.take_heterogeneous_gpu_execution() {
        let _ = host.fail_heterogeneous_viewer_gpu(execution);
    }
}

fn report_successful_viewer_gpu_fallbacks(
    reasons: &[String],
    mut record_diagnostic: impl FnMut(&str),
) {
    for reason in reasons {
        record_diagnostic(reason);
    }
}

fn prepare_viewer_gpu_preview(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
) {
    let prepare_started = Instant::now();
    macro_rules! finish_prepare {
        () => {{
            session
                .viewer_gpu_output_telemetry
                .record_prepare_duration(prepare_started.elapsed());
            return;
        }};
    }

    let _ = poll_viewer_heterogeneous_completion(device, session, host);
    if let Some(terminal) = session.viewer_gpu_device_progress.generation_terminal() {
        let fallback_reason = format!(
            "Window Viewer GPU device generation is terminal; publication remains disabled until rebuild: {}",
            terminal.reason
        );
        unregister_program_scopes_textures(session);
        if !retire_window_published_gpu_output(session, host) {
            host.clear_external_viewer_frame();
        }
        host.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::CpuFallbackRequested {
            reason: fallback_reason.clone(),
        });
        host.request_viewer_cpu_fallback(fallback_reason);
        finish_prepare!();
    }
    let submission_in_flight = session.viewer_gpu_submissions.is_occupied();
    if session.viewer_gpu_submissions.is_at_capacity() {
        stage_window_viewer_gpu_lookahead(session, host);
        session
            .viewer_gpu_output_telemetry
            .record_prepare_duration(prepare_started.elapsed());
        return;
    }
    let expected_prepared_output = host.exact_prepared_viewer_gpu_output_key();
    if let Some(stale) = session
        .viewer_gpu_presentation
        .publications
        .retire_prepared_unless(expected_prepared_output.as_ref())
    {
        let _ = host.clear_external_viewer_frame_for_artifact(
            stale.output_key(),
            stale.artifact().as_str(),
        );
        session.frame_renderer.unregister_external_texture(stale.artifact());
        drop(stale);
    }
    if !host.preflight_pending_viewer_gpu_presentation() {
        session
            .viewer_gpu_output_telemetry
            .record_prepare_duration(prepare_started.elapsed());
        return;
    }
    if !submission_in_flight {
        host.apply_preview_execution_resource_decision(&mut *session.viewer_gpu_execution);
    }

    // Native import copies decoder surfaces into renderer-owned textures. The
    // source must live through that GPU copy, but retaining it until the next
    // decoded frame creates a circular wait when the decoder pool is bounded.
    // Advance copy-fence retirement on every prepare tick, including Loading
    // ticks for a following seek.
    if !submission_in_flight
        && let Err(error) = session.viewer_gpu_execution.retire_completed_native_import_sources()
    {
        let fallback_reason = format!("native video source retirement failed: {error}");
        unregister_program_scopes_textures(session);
        host.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::UnsupportedFeature {
            feature: "native_video_source_retirement".to_owned(),
            reason: error.to_string(),
        });
        tracing::warn!("native video source retirement failed: {error}");
        host.request_viewer_cpu_fallback(fallback_reason);
        if !retire_window_published_gpu_output(session, host) {
            host.clear_external_viewer_frame();
        }
        finish_prepare!();
    }

    session.viewer_gpu_output_telemetry.record_invocation();
    if session.role != AppUiWindowRole::Workspace {
        unregister_program_scopes_textures(session);
        session.program_scopes_refresh_requested = false;
        session.viewer_gpu_output_telemetry.record_non_workspace_skip();
        finish_prepare!();
    }
    let program_scopes_requested = host.is_panel_active(PanelKind::Scopes);
    let scopes_settings = host.video_scopes_settings();
    let program_scopes_analysis_identity =
        program_scopes_requested.then_some(scopes_settings.analysis_identity());
    if !program_scopes_requested {
        unregister_program_scopes_textures(session);
        session.program_scopes_refresh_requested = false;
    }
    if session.program_scopes_registered
        && session.program_scopes_analysis_identity != program_scopes_analysis_identity
    {
        unregister_program_scopes_textures(session);
        host.clear_external_viewer_frame();
        session.program_scopes_refresh_requested = true;
    }
    if host.should_defer_gpu_preview_prepare_for_interaction() {
        session.viewer_gpu_output_telemetry.record_preview_candidate_state(
            AppUiViewerGpuOutputPreviewCandidateState::Loading,
            None,
        );
        session.viewer_gpu_output_telemetry.record_loading_skip();
        finish_prepare!();
    }
    let Some(presentation_geometry) = host.viewer_presentation_geometry() else {
        clear_viewer_spatial_presentation(session, host);
        session.viewer_gpu_output_telemetry.record_unavailable_skip();
        finish_prepare!();
    };
    synchronize_viewer_spatial_presentation(session, host, presentation_geometry.presentation);
    if program_scopes_requested
        && !session.program_scopes_registered
        && !session.program_scopes_refresh_requested
    {
        // A Viewer frame may already be current when the user opens Scopes.
        // Reissue that same candidate once so Program Output is still resident
        // at the renderer boundary instead of measuring monitor output.
        host.clear_external_viewer_frame();
        session.program_scopes_refresh_requested = true;
    }
    let staged_current = session
        .staged_viewer_gpu_successors
        .take_exact(host.viewer_gpu_current_intent())
        .and_then(|frame| host.bind_staged_gpu_frame_for_current(frame));
    let current_candidate = staged_current.map_or_else(
        || host.gpu_preview_frame_for_current_state(),
        PreviewGpuFrameState::Ready,
    );
    let mut frame = match current_candidate {
        PreviewGpuFrameState::Ready(frame) => frame,
        PreviewGpuFrameState::Current(candidate) => {
            let physical_slot_is_exact =
                if let Some(output_key) = host.exact_current_viewer_gpu_output_key() {
                    let promotion = session
                        .viewer_gpu_presentation
                        .publications
                        .promote_prepared_exact(&output_key);
                    let exact_output_available = promotion.exact_output_available();
                    if let Some(previous) = promotion.into_retired() {
                        session.frame_renderer.unregister_external_texture(previous.artifact());
                        drop(previous);
                    }
                    exact_output_available
                } else {
                    false
                };
            let physical_is_exact = physical_slot_is_exact
                && session.viewer_gpu_presentation.published_output().is_some_and(|physical| {
                    host.has_external_viewer_frame_artifact(
                        physical.output_key(),
                        physical.artifact().as_str(),
                    )
                });
            if physical_is_exact
                && session.viewer_gpu_device_progress.generation_terminal().is_none()
            {
                let _ = host.present_current_viewer_output(candidate);
            } else {
                let _ = retire_window_published_gpu_output(session, host);
                host.clear_external_viewer_frame();
                tracing::warn!(
                    "semantic Viewer output reported Current without an exact Window physical slot"
                );
            }
            session.viewer_gpu_output_telemetry.record_preview_candidate_state(
                AppUiViewerGpuOutputPreviewCandidateState::Current,
                None,
            );
            session.viewer_gpu_output_telemetry.record_current_skip();
            if program_scopes_requested {
                finish_prepare!();
            }
            let staged = host
                .viewer_gpu_successor_intent()
                .and_then(|intent| session.staged_viewer_gpu_successors.take_exact(intent));
            match staged.map_or_else(
                || host.gpu_preview_successor_for_current_state(),
                PreviewGpuFrameState::Ready,
            ) {
                PreviewGpuFrameState::Ready(frame) => {
                    stage_window_viewer_gpu_lookahead(session, host);
                    frame
                }
                PreviewGpuFrameState::Prepared
                | PreviewGpuFrameState::Current(_)
                | PreviewGpuFrameState::Transparent(_)
                | PreviewGpuFrameState::Loading
                | PreviewGpuFrameState::Unavailable(_) => {
                    stage_window_viewer_gpu_lookahead(session, host);
                    finish_prepare!()
                }
            }
        }
        PreviewGpuFrameState::Prepared => {
            session.viewer_gpu_output_telemetry.record_current_skip();
            finish_prepare!();
        }
        PreviewGpuFrameState::Transparent(candidate) => {
            let disposition = host.present_transparent_viewer_output(candidate);
            if matches!(
                disposition,
                FramePresentationDisposition::Presented(_) | FramePresentationDisposition::NoDemand
            ) {
                let _ = retire_window_published_gpu_output(session, host);
            }
            unregister_program_scopes_textures(session);
            session.program_scopes_refresh_requested = program_scopes_requested;
            session.viewer_gpu_output_telemetry.record_preview_candidate_state(
                AppUiViewerGpuOutputPreviewCandidateState::Transparent,
                None,
            );
            session.viewer_gpu_output_telemetry.record_current_skip();
            finish_prepare!();
        }
        PreviewGpuFrameState::Loading => {
            session.viewer_gpu_output_telemetry.record_preview_candidate_state(
                AppUiViewerGpuOutputPreviewCandidateState::Loading,
                None,
            );
            session.viewer_gpu_output_telemetry.record_loading_skip();
            finish_prepare!();
        }
        PreviewGpuFrameState::Unavailable(reason) => {
            unregister_program_scopes_textures(session);
            session.program_scopes_refresh_requested = program_scopes_requested;
            session.viewer_gpu_output_telemetry.record_preview_candidate_state(
                AppUiViewerGpuOutputPreviewCandidateState::Unavailable,
                None,
            );
            session.viewer_gpu_output_telemetry.record_unavailable_skip();
            tracing::debug!(
                code = reason.code(),
                stage = ?reason.stage(),
                detail = reason.detail(),
                "Viewer GPU Preview candidate is unavailable"
            );
            finish_prepare!();
        }
    };
    if !frame.is_successor_preparation()
        && !host.preflight_viewer_gpu_presentation(frame.presentation_ticket())
    {
        finish_prepare!();
    }
    let Some(texture_key_base) = ExternalTextureKey::new(format!(
        "{}:{}",
        frame.external_texture_key(),
        presentation_geometry.presentation.key_suffix()
    )) else {
        session.viewer_gpu_output_telemetry.record_invalid_texture_key();
        tracing::warn!(
            sequence_id = %frame.sequence_id,
            frame = frame.frame,
            "viewer GPU preview produced an invalid external texture key"
        );
        fail_viewer_gpu_frame(host, &mut frame);
        host.clear_external_viewer_frame();
        finish_prepare!();
    };
    let declared_residency = declared_viewer_gpu_output_residency(&frame);
    session.viewer_gpu_output_telemetry.record_frame_context(
        &frame,
        texture_key_base.as_str().to_owned(),
        declared_residency,
    );
    session.viewer_gpu_output_telemetry.record_preview_candidate_state(
        AppUiViewerGpuOutputPreviewCandidateState::Ready,
        Some(frame.candidate_id()),
    );
    let presentation_readiness = session
        .display_output_contract
        .presentation_readiness_for_color_space(frame.monitor_adaptation.monitor_color_space());
    session
        .viewer_gpu_output_telemetry
        .record_display_presentation_readiness(presentation_readiness);
    if presentation_readiness.status != AppUiDisplayPresentationReadinessStatus::Current {
        tracing::warn!(
            sequence_id = %frame.sequence_id,
            frame = frame.frame,
            width = frame.width,
            height = frame.height,
            presentation_readiness = ?presentation_readiness,
            "viewer GPU preview display presentation is not ready for the requested boundary"
        );
    }

    if let Some(ref snapshot) = session.display_snapshot
        && !snapshot.is_valid()
    {
        tracing::warn!(
            sequence_id = %frame.sequence_id,
            frame = frame.frame,
            validation_status = ?snapshot.validation_status,
            blockers = ?snapshot.blockers,
            monitor_profile = ?snapshot.monitor_profile_status,
            hdr_status = ?snapshot.hdr_status,
            "v2 display output contract invalid — blocking preview"
        );
        let preview_blockers =
            crate::app::preview_display_contract::preview_blockers_from_snapshot(snapshot);
        for blocker in &snapshot.blockers {
            host.record_preview_gpu_output_blocker(
                &preview_blockers
                    .iter()
                    .find(|b| b.code() == blocker.code())
                    .cloned()
                    .unwrap_or_else(|| PreviewGpuOutputBlocker::UnsupportedFeature {
                        feature: blocker.code().to_owned(),
                        reason: format!("{blocker:?}"),
                    }),
            );
        }
        unregister_program_scopes_textures(session);
        fail_viewer_gpu_frame(host, &mut frame);
        host.clear_external_viewer_frame();
        finish_prepare!();
    }

    if let Some(blocker) = session
        .display_output_contract
        .boundary_blocker_for_color_space(frame.monitor_adaptation.monitor_color_space())
    {
        unregister_program_scopes_textures(session);
        session.viewer_gpu_output_telemetry.record_display_contract_blocker(&blocker);
        match &blocker {
            AppUiDisplayBoundaryBlocker::HdrOutputRequiresHdrSurface {
                output_color_space,
                selected_surface_format,
                ..
            } => {
                host.record_preview_gpu_output_blocker(
                    &PreviewGpuOutputBlocker::SurfaceContractMismatch {
                        surface_format: format!("{selected_surface_format:?}"),
                        output_color_space: format!("{output_color_space:?}"),
                    },
                );
            }
            AppUiDisplayBoundaryBlocker::OutputColorSpaceRequiresSurfaceColorSpace {
                output_color_space,
                selected_surface_format,
                ..
            } => {
                host.record_preview_gpu_output_blocker(
                    &PreviewGpuOutputBlocker::SurfaceContractMismatch {
                        surface_format: format!("{selected_surface_format:?}"),
                        output_color_space: format!("{output_color_space:?}"),
                    },
                );
            }
        }
        let supported_surface_color_spaces = session
            .display_output_contract
            .supported_surface_color_spaces_for_selected_format();
        tracing::warn!(
            sequence_id = %frame.sequence_id,
            frame = frame.frame,
            width = frame.width,
            height = frame.height,
            program_output_color_space = ?frame.program_output_boundary.output_color_space(),
            monitor_color_space = ?frame.monitor_adaptation.monitor_color_space(),
            display_target = ?session.display_output_contract.display_target,
            surface_format = ?session.display_output_contract.surface_color.format,
            surface_color_space = ?session.display_output_contract.surface_color.color_space,
            surface_hdr_mode = ?session.display_output_contract.surface_color.hdr_mode,
            supported_surface_color_spaces = ?supported_surface_color_spaces,
            blocker = ?blocker,
            "viewer GPU preview output boundary blocked by display output contract"
        );
        fail_viewer_gpu_frame(host, &mut frame);
        host.clear_external_viewer_frame();
        finish_prepare!();
    }

    // Progress capacity is part of submission admission, not fallible
    // post-submit bookkeeping. Once `Queue::submit` returns, committing its
    // exact index through this move-only permit cannot reject the batch.
    let progress_permit = match session.viewer_gpu_device_progress.reserve_submission() {
        Ok(permit) => permit,
        Err(ViewerGpuDeviceProgressReserveError::Backpressured) => finish_prepare!(),
        Err(error) => {
            let reason = format!(
                "Window Viewer GPU output is unavailable until the device generation is rebuilt: {error}"
            );
            fail_viewer_gpu_frame(host, &mut frame);
            unregister_program_scopes_textures(session);
            if !retire_window_published_gpu_output(session, host) {
                host.clear_external_viewer_frame();
            }
            host.record_preview_gpu_output_blocker(
                &PreviewGpuOutputBlocker::CpuFallbackRequested { reason: reason.clone() },
            );
            host.request_viewer_cpu_fallback(reason);
            finish_prepare!();
        }
    };
    let completion_signal = progress_permit.completion_signal();
    let reservation = match session.viewer_gpu_submissions.reserve() {
        Ok(reservation) => reservation,
        Err(ViewerGpuSubmissionAdmissionError::Backpressured) => finish_prepare!(),
        Err(ViewerGpuSubmissionAdmissionError::IdentityExhausted) => {
            fail_viewer_gpu_frame(host, &mut frame);
            host.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::UnsupportedFeature {
                feature: "viewer_gpu_submission_identity".to_owned(),
                reason: "Viewer GPU submission identity space is exhausted".to_owned(),
            });
            finish_prepare!();
        }
    };
    let submission_id = reservation.submission_id();
    let Some(texture_key) = ExternalTextureKey::new(format!(
        "{}:submission:{}",
        texture_key_base.as_str(),
        submission_id.get()
    )) else {
        fail_viewer_gpu_frame(host, &mut frame);
        host.clear_external_viewer_frame();
        finish_prepare!();
    };
    session.viewer_gpu_execution.clear_frame_resources();

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("app_ui_viewer_gpu_preview_output_encoder"),
    });
    let heterogeneous_inputs = frame.take_heterogeneous_gpu_inputs();
    let layers = match &frame.working_input {
        PreviewGpuWorkingInput::GpuComposite { layers } => layers,
    };
    let display_calibration = match session.display_calibration.clone() {
        Some(calibration) => {
            if let Err(error) =
                validate_display_calibration_proof(session.display_snapshot.as_ref(), &calibration)
            {
                host.record_preview_gpu_output_blocker(
                    &PreviewGpuOutputBlocker::UnsupportedFeature {
                        feature: "viewer_display_calibration_proof".to_owned(),
                        reason: error.clone(),
                    },
                );
                tracing::warn!(
                    sequence_id = %frame.sequence_id,
                    frame = frame.frame,
                    "viewer GPU preview display calibration proof failed: {error}"
                );
                fail_viewer_gpu_frame(host, &mut frame);
                host.clear_external_viewer_frame();
                finish_prepare!();
            }
            Some(calibration)
        }
        None => None,
    };
    let output_precision = ViewerGpuOutputPrecision::minimum_for_display(
        frame.monitor_adaptation.monitor_color_space(),
        display_calibration.is_some(),
    );
    let source_rect = presentation_geometry.presentation.normalized_source_rect();
    let mut record = match session.viewer_gpu_execution.record(
        device,
        queue,
        &mut encoder,
        ViewerGpuExecutionRequest {
            sequence_id: frame.sequence_id,
            timeline_frame: frame.frame,
            width: frame.width,
            height: frame.height,
            working_color_space: frame.working_color_space,
            layers,
            heterogeneous_inputs,
            program_output_boundary: &frame.program_output_boundary,
            monitor_adaptation: &frame.monitor_adaptation,
            source_rect: ViewerSourceRect {
                x: source_rect.x,
                y: source_rect.y,
                width: source_rect.width,
                height: source_rect.height,
            },
            output_width: presentation_geometry.presentation.output_width,
            output_height: presentation_geometry.presentation.output_height,
            output_precision,
            display_calibration,
            program_scopes: viewer_program_scopes_request(
                program_scopes_requested,
                frame.program_output_boundary.output_color_space(),
                frame.monitor_adaptation.monitor_color_space(),
                scopes_settings,
            )
            .unwrap_or_else(|error| {
                tracing::warn!(
                    sequence_id = %frame.sequence_id,
                    frame = frame.frame,
                    %error,
                    "active scopes panel rejected the Program Output signal"
                );
                None
            }),
            signal_monitoring: viewer_signal_monitor_request(
                frame.program_output_boundary.output_color_space(),
                frame.monitor_adaptation.monitor_color_space(),
                scopes_settings,
            )
            .unwrap_or_else(|error| {
                tracing::warn!(
                    sequence_id = %frame.sequence_id,
                    frame = frame.frame,
                    %error,
                    "Viewer signal-monitoring controls rejected the selected signal"
                );
                None
            }),
        },
    ) {
        Ok(record) => record,
        Err(error) => {
            if let ViewerGpuExecutionError::Backpressure(reason) = &error {
                progress_permit.drive_renderer_cleanup(submission_id);
                tracing::debug!(
                    sequence_id = %frame.sequence_id,
                    frame = frame.frame,
                    width = frame.width,
                    height = frame.height,
                    reason,
                    "viewer GPU preview retained the current output under bounded backpressure"
                );
                finish_prepare!();
            }
            fail_viewer_gpu_frame(host, &mut frame);
            match &error {
                error if error.working_composite_blocker().is_some() => {
                    host.record_preview_gpu_compositing(
                        mondrian_renderer::GpuCompositingDiagnostics {
                            cpu_fallback_composites: 1,
                            cpu_composited_pixels: u64::from(frame.width)
                                .saturating_mul(u64::from(frame.height)),
                            first_blocker: error.working_composite_blocker(),
                            ..mondrian_renderer::GpuCompositingDiagnostics::default()
                        },
                    );
                }
                error if error.is_program_output_failure() => {
                    session.viewer_gpu_output_telemetry.record_record_failure();
                    host.record_preview_cpu_output_fallback(frame.width, frame.height);
                    if let Some(breakdown) = error.program_output_blocker_breakdown() {
                        host.record_preview_gpu_output_blocker_breakdown(
                            PreviewGpuOutputBlockerBreakdown::from_renderer_breakdown(breakdown),
                        );
                    } else {
                        host.record_preview_gpu_output_blocker(
                            &PreviewGpuOutputBlocker::CpuFallbackRequested {
                                reason: error.to_string(),
                            },
                        );
                    }
                }
                _ => {
                    host.record_preview_gpu_output_blocker(
                        &PreviewGpuOutputBlocker::CpuFallbackRequested {
                            reason: error.to_string(),
                        },
                    );
                }
            }
            tracing::warn!(
                sequence_id = %frame.sequence_id,
                frame = frame.frame,
                width = frame.width,
                height = frame.height,
                "viewer GPU preview recording failed: {error}"
            );
            host.request_viewer_cpu_fallback(error.to_string());
            finish_prepare!();
        }
    };
    host.record_preview_gpu_compositing(record.compositing_diagnostics);
    let uniform_arena = session.viewer_gpu_execution.compositor_uniform_arena_diagnostics();
    session
        .viewer_gpu_output_telemetry
        .record_compositor_uniform_arena(uniform_arena);
    let texture_bindings = session.viewer_gpu_execution.compositor_texture_binding_diagnostics();
    session
        .viewer_gpu_output_telemetry
        .record_compositor_texture_bindings(texture_bindings);
    let creative_luts = session.viewer_gpu_execution.compositor_creative_lut_diagnostics();
    session
        .viewer_gpu_output_telemetry
        .record_compositor_creative_luts(creative_luts);
    session
        .viewer_gpu_output_telemetry
        .record_spatial_runtime(record.spatial_diagnostics);
    session.viewer_gpu_output_telemetry.record_actual_frame_residency(
        preview_gpu_composite_frame_residency(
            record.residency,
            session.viewer_gpu_execution.native_import_support(),
        ),
    );
    report_successful_viewer_gpu_fallbacks(&record.fallback_reasons, |reason| {
        host.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::CpuFallbackRequested {
            reason: reason.to_owned(),
        });
    });
    let heterogeneous_recorded = record.heterogeneous_continuation_count() != 0;
    let stage_diagnostics = record.stage_diagnostics;
    if let Some(terminal) = session.viewer_gpu_device_progress.generation_terminal() {
        let reason = format!(
            "Window Viewer GPU device generation became terminal before queue submission: {}",
            terminal.reason
        );
        drop(record);
        drop(encoder);
        drop(progress_permit);
        fail_viewer_gpu_frame(host, &mut frame);
        unregister_program_scopes_textures(session);
        if !retire_window_published_gpu_output(session, host) {
            host.clear_external_viewer_frame();
        }
        host.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::CpuFallbackRequested {
            reason: reason.clone(),
        });
        tracing::error!(%reason, "Window Viewer rejected terminal-generation submission");
        finish_prepare!();
    }
    let submission_index = queue.submit(std::iter::once(encoder.finish()));
    let heterogeneous_submission = record.assert_adapter_submission(submission_index.clone());
    let output_lease = match session.viewer_gpu_execution.take_presentation_output(&mut record) {
        Ok(lease) => lease,
        Err(error) => {
            let fallback_reason = format!("submitted Viewer output lease transfer failed: {error}");
            session.viewer_gpu_output_telemetry.record_missing_output_texture();
            tracing::error!(
                sequence_id = %frame.sequence_id,
                frame = frame.frame,
                "submitted Viewer GPU output could not transfer its physical lease: {error}"
            );
            let terminal = frame.take_heterogeneous_gpu_execution();
            let submitted_at = Instant::now();
            let completion_deadline = submitted_at
                .checked_add(VIEWER_HETEROGENEOUS_COMPLETION_TIMEOUT)
                .unwrap_or(submitted_at);
            let owner = WindowViewerGpuSubmissionOwner {
                frame,
                terminal,
                texture_key,
                texture_registered: false,
                output_lease: None,
                presentation: presentation_geometry.presentation,
                stage_diagnostics: record.stage_diagnostics,
                program_scopes: record.program_scopes.take(),
                program_scopes_requested,
                program_scopes_analysis_identity,
            };
            reservation.commit(
                owner,
                completion_deadline,
                move |callback| {
                    heterogeneous_submission.register_completion_callback(queue, callback);
                },
                move || {
                    completion_signal.mark_observed();
                },
            );
            progress_permit.commit(submission_id, submission_index);
            if let Some(quarantine) =
                session.viewer_gpu_submissions.quarantine_submission_after_authority_revocation(
                    submission_id,
                    format!("submitted Viewer output lease transfer failed: {error}"),
                )
            {
                begin_window_viewer_gpu_quarantine(
                    session,
                    host,
                    quarantine,
                    WindowViewerGpuDeferredCleanup::ClearFrameResources,
                );
            }
            host.request_viewer_cpu_fallback(fallback_reason);
            finish_prepare!();
        }
    };
    let authority_error = if heterogeneous_recorded && !frame.has_heterogeneous_gpu_execution() {
        Some("renderer recorded a heterogeneous continuation without a visual Broker lease")
    } else if !heterogeneous_recorded && frame.has_heterogeneous_gpu_execution() {
        Some("visual Broker lease produced no renderer GPU continuation")
    } else {
        None
    };
    let terminal = if heterogeneous_recorded {
        frame.take_heterogeneous_gpu_execution()
    } else {
        None
    };
    let submitted_at = Instant::now();
    let completion_deadline = submitted_at
        .checked_add(VIEWER_HETEROGENEOUS_COMPLETION_TIMEOUT)
        .unwrap_or(submitted_at);
    let owner = WindowViewerGpuSubmissionOwner {
        frame,
        terminal,
        texture_key,
        texture_registered: false,
        output_lease: Some(output_lease),
        presentation: presentation_geometry.presentation,
        stage_diagnostics,
        program_scopes: record.program_scopes.take(),
        program_scopes_requested,
        program_scopes_analysis_identity,
    };
    reservation.commit(
        owner,
        completion_deadline,
        move |callback| {
            heterogeneous_submission.register_completion_callback(queue, callback);
        },
        move || {
            completion_signal.mark_observed();
        },
    );
    progress_permit.commit(submission_id, submission_index);

    let registration = {
        let owner = session.viewer_gpu_submissions.owner(submission_id);
        owner
            .and_then(|owner| owner.output_lease.as_ref())
            .ok_or_else(|| "submitted Window output lease is missing".to_owned())
            .and_then(|lease| {
                session
                    .frame_renderer
                    .register_external_texture_view(
                        device,
                        owner
                            .map(|owner| owner.texture_key.clone())
                            .ok_or_else(|| "submitted Window owner is missing".to_owned())?,
                        lease.texture_view(),
                        if session.display_calibration.is_some() {
                            ExternalTextureTransfer::DeviceCodeValuesOpaque
                        } else {
                            ExternalTextureTransfer::SurfaceCodeValuesOpaque
                        },
                    )
                    .map_err(|error| error.to_string())
            })
    };
    if let Err(error) = registration {
        host.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::UnsupportedFeature {
            feature: "viewer_encoded_code_value_presentation".to_owned(),
            reason: error.clone(),
        });
        if let Some(quarantine) =
            session.viewer_gpu_submissions.quarantine_submission_after_authority_revocation(
                submission_id,
                format!("Window texture registration failed: {error}"),
            )
        {
            begin_window_viewer_gpu_quarantine(
                session,
                host,
                quarantine,
                WindowViewerGpuDeferredCleanup::ClearFrameResources,
            );
        }
        finish_prepare!();
    }
    if let Some(owner) = session.viewer_gpu_submissions.owner_mut(submission_id) {
        owner.texture_registered = true;
    }
    if let Some(reason) = authority_error {
        host.record_preview_gpu_output_blocker(&PreviewGpuOutputBlocker::UnsupportedFeature {
            feature: "heterogeneous_viewer_gpu_lease".to_owned(),
            reason: reason.to_owned(),
        });
        if let Some(quarantine) = session
            .viewer_gpu_submissions
            .quarantine_submission_after_authority_revocation(submission_id, reason.to_owned())
        {
            begin_window_viewer_gpu_quarantine(
                session,
                host,
                quarantine,
                WindowViewerGpuDeferredCleanup::ClearFrameResources,
            );
        }
        finish_prepare!();
    }
    if heterogeneous_recorded {
        finish_prepare!();
    }
    register_ordinary_window_program_scopes(session, device, submission_id);
    publish_ordinary_window_viewer_gpu_submission(session, host, submission_id);
    session
        .viewer_gpu_output_telemetry
        .record_prepare_duration(prepare_started.elapsed());
}

/// Warm a bounded CPU-side horizon beyond the immediate successor.
///
/// The staged frame is deliberately not registered with Preview semantics and
/// owns no physical Viewer output. Transport rotation drops it through RAII.
fn stage_window_viewer_gpu_lookahead(session: &mut AppUiWindowSession, host: &AppUiHost) {
    const FIRST_LOOKAHEAD_OFFSET: usize = 2;
    const LAST_LOOKAHEAD_OFFSET: usize = 4;
    let expected = std::iter::once(host.viewer_gpu_successor_intent())
        .chain(
            (FIRST_LOOKAHEAD_OFFSET..=LAST_LOOKAHEAD_OFFSET)
                .map(|offset| host.viewer_gpu_lookahead_intent(offset)),
        )
        .flatten()
        .collect::<Vec<_>>();
    session.staged_viewer_gpu_successors.retain_only(&expected);

    for offset in FIRST_LOOKAHEAD_OFFSET..=LAST_LOOKAHEAD_OFFSET {
        let Some(intent) = host.viewer_gpu_lookahead_intent(offset) else {
            continue;
        };
        if session.staged_viewer_gpu_successors.contains(intent) {
            continue;
        }
        if let PreviewGpuFrameState::Ready(frame) =
            host.gpu_preview_lookahead_for_current_state(offset)
            && frame.is_successor_preparation()
            && frame.playback_intent() == intent
        {
            let PreviewGpuWorkingInput::GpuComposite { layers } = &frame.working_input;
            if let Err(error) = session.viewer_gpu_execution.prepare_cpu_yuv_uploads(layers) {
                tracing::warn!(
                    %error,
                    timeline_frame = frame.frame,
                    "failed to prewarm compact CPU YUV Viewer lookahead"
                );
            }
            session.staged_viewer_gpu_successors.stage(frame);
        }
        // Preserve current-frame priority: each event-loop turn may fill only
        // the nearest missing speculative coordinate. Later turns extend the
        // same bounded horizon without a three-request burst.
        break;
    }
}

fn register_ordinary_window_program_scopes(
    session: &mut AppUiWindowSession,
    device: &wgpu::Device,
    submission_id: ViewerGpuSubmissionId,
) {
    let scopes = session
        .viewer_gpu_submissions
        .owner_mut(submission_id)
        .and_then(|owner| owner.program_scopes.take());
    if let Some(scopes) = scopes {
        let identity = session
            .viewer_gpu_submissions
            .owner(submission_id)
            .and_then(|owner| owner.program_scopes_analysis_identity);
        if let Err(error) = register_program_scopes_textures(session, device, &scopes, identity) {
            unregister_program_scopes_textures(session);
            session.program_scopes_refresh_requested = true;
            tracing::warn!(%error, "ordinary GPU Program Output scope registration failed");
        }
    } else {
        unregister_program_scopes_textures(session);
        let requested = session
            .viewer_gpu_submissions
            .owner(submission_id)
            .is_some_and(|owner| owner.program_scopes_requested);
        session.program_scopes_refresh_requested = requested;
    }
}

fn publish_ordinary_window_viewer_gpu_submission(
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
    submission_id: ViewerGpuSubmissionId,
) {
    if let Some(terminal) = session.viewer_gpu_device_progress.generation_terminal() {
        tracing::warn!(
            submission_id = submission_id.get(),
            reason = terminal.reason,
            "ordinary Window Viewer output was not published from a terminal device generation"
        );
        return;
    }
    let Some((output_key, texture_key, presentation, output_lease)) =
        session.viewer_gpu_submissions.owner_mut(submission_id).and_then(|owner| {
            if !owner.texture_registered {
                return None;
            }
            Some((
                owner.frame.output_key.clone(),
                owner.texture_key.clone(),
                owner.presentation,
                owner.output_lease.take()?,
            ))
        })
    else {
        tracing::error!(
            submission_id = submission_id.get(),
            "ordinary Window publication requires a registered texture and physical output lease"
        );
        return;
    };
    let successor_preparation = session
        .viewer_gpu_submissions
        .owner(submission_id)
        .is_some_and(|owner| owner.frame.is_successor_preparation());
    if successor_preparation {
        let Some(visible_output) = mondrian_ui_widgets::ViewerExternalTextureFrame::new_spatial(
            texture_key.as_str().to_owned(),
            presentation,
        ) else {
            session.frame_renderer.unregister_external_texture(&texture_key);
            drop(output_lease);
            return;
        };
        let Some(owner) = session.viewer_gpu_submissions.owner(submission_id) else {
            session.frame_renderer.unregister_external_texture(&texture_key);
            drop(output_lease);
            return;
        };
        host.register_prepared_viewer_gpu_successor(&owner.frame, visible_output);
        if session.viewer_gpu_device_progress.generation_terminal().is_some() {
            let _ =
                host.clear_external_viewer_frame_for_artifact(&output_key, texture_key.as_str());
            session.frame_renderer.unregister_external_texture(&texture_key);
            drop(output_lease);
            return;
        }
        if let Some(owner) = session.viewer_gpu_submissions.owner_mut(submission_id) {
            owner.texture_registered = false;
        }
        if let Some(previous) = session.viewer_gpu_presentation.publications.publish_prepared(
            submission_id,
            output_key,
            texture_key,
            output_lease,
        ) {
            session.frame_renderer.unregister_external_texture(previous.artifact());
            let _ = host.clear_external_viewer_frame_for_artifact(
                previous.output_key(),
                previous.artifact().as_str(),
            );
            drop(previous);
        }
        if let Some(owner) = session.viewer_gpu_submissions.owner(submission_id) {
            session
                .viewer_gpu_output_telemetry
                .record_registered_frame(owner.stage_diagnostics);
        }
        let _ = host.observe_prepared_viewer_gpu_preroll();
        return;
    }
    let disposition = {
        let Some(owner) = session.viewer_gpu_submissions.owner(submission_id) else {
            tracing::error!(
                submission_id = submission_id.get(),
                "ordinary Window publication lost its submission owner"
            );
            session.frame_renderer.unregister_external_texture(&texture_key);
            drop(output_lease);
            return;
        };
        host.set_external_viewer_frame(&owner.frame, texture_key.as_str().to_owned(), presentation)
    };
    if let Some(terminal) = session.viewer_gpu_device_progress.generation_terminal() {
        let _ = host.clear_external_viewer_frame_for_artifact(&output_key, texture_key.as_str());
        if let Some(owner) = session.viewer_gpu_submissions.owner_mut(submission_id)
            && owner.texture_registered
        {
            owner.texture_registered = false;
            session.frame_renderer.unregister_external_texture(&owner.texture_key);
        }
        drop(output_lease);
        tracing::warn!(
            submission_id = submission_id.get(),
            reason = terminal.reason,
            "ordinary Window Viewer publication raced a terminal device generation and was revoked"
        );
        return;
    }
    match disposition {
        FramePresentationDisposition::Presented(_) | FramePresentationDisposition::NoDemand => {
            if let Some(owner) = session.viewer_gpu_submissions.owner_mut(submission_id) {
                owner.texture_registered = false;
            } else {
                // The lifecycle cannot concurrently remove an owner on the
                // Window event thread. Fail closed if that invariant is ever
                // violated after the semantic commit.
                let _ = host
                    .clear_external_viewer_frame_for_artifact(&output_key, texture_key.as_str());
                session.frame_renderer.unregister_external_texture(&texture_key);
                drop(output_lease);
                tracing::error!(
                    submission_id = submission_id.get(),
                    "ordinary Window owner disappeared after presentation commit"
                );
                return;
            }
            let previous = session.viewer_gpu_presentation.publications.publish_current(
                submission_id,
                output_key,
                texture_key,
                output_lease,
            );
            if let Some(previous) = previous {
                session.frame_renderer.unregister_external_texture(previous.artifact());
                drop(previous);
            }
            if let Some(owner) = session.viewer_gpu_submissions.owner(submission_id) {
                session
                    .viewer_gpu_output_telemetry
                    .record_registered_frame(owner.stage_diagnostics);
            }
        }
        FramePresentationDisposition::DroppedLate(_) => {
            if let Some(owner) = session.viewer_gpu_submissions.owner_mut(submission_id)
                && owner.texture_registered
            {
                owner.texture_registered = false;
                session.frame_renderer.unregister_external_texture(&owner.texture_key);
            }
            drop(output_lease);
        }
        FramePresentationDisposition::OutputRejected
        | FramePresentationDisposition::LostAuthority => {
            if let Some(owner) = session.viewer_gpu_submissions.owner_mut(submission_id) {
                session
                    .viewer_gpu_output_telemetry
                    .record_rejected_external_frame(owner.stage_diagnostics);
                if owner.texture_registered {
                    owner.texture_registered = false;
                    session.frame_renderer.unregister_external_texture(&owner.texture_key);
                }
            }
            drop(output_lease);
        }
    }
}

fn viewer_program_scopes_request(
    active: bool,
    program_output_color_space: ColorSpace,
    monitor_output_color_space: ColorSpace,
    settings: VideoScopesSettings,
) -> Result<Option<GpuProgramScopesRequest>, mondrian_core::ProgramColorScopeError> {
    active
        .then(|| {
            let signal_color_space = match settings.tap {
                ProgramScopesTap::ProgramOutput => program_output_color_space,
                ProgramScopesTap::MonitorOutput => monitor_output_color_space,
            };
            GpuProgramScopesRequest::with_controls(
                signal_color_space,
                settings.waveform_mode,
                settings.scale,
                settings.tap,
                256,
                512,
            )
        })
        .transpose()
}

fn viewer_signal_monitor_request(
    program_output_color_space: ColorSpace,
    monitor_output_color_space: ColorSpace,
    settings: VideoScopesSettings,
) -> Result<Option<GpuSignalMonitorRequest>, mondrian_renderer::GpuSignalMonitorError> {
    if !settings.monitoring.is_active() {
        return Ok(None);
    }
    let signal_color_space = match settings.tap {
        ProgramScopesTap::ProgramOutput => program_output_color_space,
        ProgramScopesTap::MonitorOutput => monitor_output_color_space,
    };
    GpuSignalMonitorRequest::new(
        SignalComplianceContract::normalized_rgb(signal_color_space)?,
        settings.monitoring,
        settings.tap,
    )
    .map(Some)
}

fn synchronize_viewer_spatial_presentation(
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
    presentation: ViewerExternalTexturePresentation,
) {
    if session.viewer_gpu_presentation.presentation() == Some(presentation) {
        return;
    }
    clear_viewer_spatial_presentation(session, host);
    session.viewer_gpu_presentation.set_presentation(presentation);
}

fn clear_viewer_spatial_presentation(session: &mut AppUiWindowSession, host: &AppUiHost) {
    let cleanup_deferred = cancel_viewer_gpu_submission(
        session,
        host,
        WindowViewerGpuDeferredCleanup::ClearFrameResources,
    );
    let had_presentation = session.viewer_gpu_presentation.take_presentation();
    let _ = retire_window_published_gpu_output(session, host);
    if !cleanup_deferred {
        session.viewer_gpu_execution.clear_frame_resources();
    }
    unregister_program_scopes_textures(session);
    if had_presentation {
        host.clear_external_viewer_frame();
    }
}

fn cancel_viewer_gpu_submission(
    session: &mut AppUiWindowSession,
    host: &AppUiHost,
    deferred_cleanup: WindowViewerGpuDeferredCleanup,
) -> bool {
    let quarantines =
        session
            .viewer_gpu_submissions
            .quarantine_all_after_authority_revocation(format!(
                "Window Viewer cleanup requested: {deferred_cleanup:?}"
            ));
    if quarantines.is_empty() {
        return false;
    }
    for quarantine in quarantines {
        begin_window_viewer_gpu_quarantine(session, host, quarantine, deferred_cleanup);
    }
    true
}

fn register_program_scopes_textures(
    session: &mut AppUiWindowSession,
    device: &wgpu::Device,
    scopes: &mondrian_renderer::GpuProgramScopesRecord,
    analysis_identity: Option<(WaveformMode, ProgramScopeScale, ProgramScopesTap)>,
) -> Result<(), String> {
    // Claim all stable keys before the first fallible insertion so callers can
    // roll back a partially registered texture set transactionally.
    session.program_scopes_registered = true;
    for (key, view) in [
        (
            crate::app_ui::scopes::HISTOGRAM_TEXTURE_KEY,
            &scopes.histogram_view,
        ),
        (
            crate::app_ui::scopes::WAVEFORM_TEXTURE_KEY,
            &scopes.waveform_view,
        ),
        (
            crate::app_ui::scopes::VECTORSCOPE_TEXTURE_KEY,
            &scopes.vectorscope_view,
        ),
    ] {
        let key = ExternalTextureKey::new(key)
            .ok_or_else(|| "Program Output scope texture key is empty".to_owned())?;
        session
            .frame_renderer
            .register_external_texture_view(device, key, view, ExternalTextureTransfer::Linear)
            .map_err(|error| error.to_string())?;
    }
    session.program_scopes_refresh_requested = false;
    session.program_scopes_analysis_identity = analysis_identity;
    Ok(())
}

fn unregister_program_scopes_textures(session: &mut AppUiWindowSession) {
    session.program_scopes_analysis_identity = None;
    if !session.program_scopes_registered {
        return;
    }
    for raw_key in [
        crate::app_ui::scopes::HISTOGRAM_TEXTURE_KEY,
        crate::app_ui::scopes::WAVEFORM_TEXTURE_KEY,
        crate::app_ui::scopes::VECTORSCOPE_TEXTURE_KEY,
    ] {
        if let Some(key) = ExternalTextureKey::new(raw_key) {
            session.frame_renderer.unregister_external_texture(&key);
        }
    }
    session.program_scopes_registered = false;
}

fn validate_display_calibration_proof(
    snapshot: Option<&mondrian_core::display_contract::DisplayOutputSnapshot>,
    calibration: &mondrian_core::display_calibration::DisplayCalibrationLut3d,
) -> Result<(), String> {
    let Some(snapshot) = snapshot else {
        return Err("display calibration LUT has no matching display snapshot".to_owned());
    };
    match snapshot.monitor_profile_status {
        mondrian_core::display_contract::MonitorProfileStatus::ManagedIccCalibration {
            source_color_space,
            profile_fingerprint,
        } if source_color_space == calibration.source_color_space()
            && profile_fingerprint == calibration.profile_fingerprint() =>
        {
            Ok(())
        }
        ref status => Err(format!(
            "display calibration LUT does not match snapshot processor proof: {status}"
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeFileDndDiagnostic {
    HoverUnhandled,
    HoverCancelUnhandled,
    DropImportedAsMedia { file_count: usize },
    DropIgnoredEmpty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeFileDropHandling {
    action: Option<mondrian_editor_state::Action>,
    diagnostic: Option<NativeFileDndDiagnostic>,
}

fn native_file_hover_diagnostic(result: EventResult) -> Option<NativeFileDndDiagnostic> {
    (result == EventResult::Ignored).then_some(NativeFileDndDiagnostic::HoverUnhandled)
}

fn native_file_hover_cancelled_diagnostic(result: EventResult) -> Option<NativeFileDndDiagnostic> {
    (result == EventResult::Ignored).then_some(NativeFileDndDiagnostic::HoverCancelUnhandled)
}

fn native_file_drop_handling(result: EventResult, paths: Vec<PathBuf>) -> NativeFileDropHandling {
    if result == EventResult::Handled {
        return NativeFileDropHandling { action: None, diagnostic: None };
    }

    if paths.is_empty() {
        return NativeFileDropHandling {
            action: None,
            diagnostic: Some(NativeFileDndDiagnostic::DropIgnoredEmpty),
        };
    }

    let file_count = paths.len();
    NativeFileDropHandling {
        action: Some(mondrian_editor_state::Action::ImportMedia(paths)),
        diagnostic: Some(NativeFileDndDiagnostic::DropImportedAsMedia { file_count }),
    }
}

fn log_native_file_dnd_diagnostic(diagnostic: NativeFileDndDiagnostic) {
    match diagnostic {
        NativeFileDndDiagnostic::HoverUnhandled => {
            tracing::debug!("app UI native file hover was not handled by widgets")
        }
        NativeFileDndDiagnostic::HoverCancelUnhandled => {
            tracing::debug!("app UI native file hover cancellation had no active widget target")
        }
        NativeFileDndDiagnostic::DropImportedAsMedia { file_count } => {
            tracing::info!(
                file_count,
                "app UI native file drop fell back to media import"
            )
        }
        NativeFileDndDiagnostic::DropIgnoredEmpty => {
            tracing::warn!("app UI native file drop ignored empty path list")
        }
    }
}

fn should_route_focus_lost_to_ui(eyedropper_active: bool) -> bool {
    !eyedropper_active
}

fn reset_modifiers_on_window_focus_loss(modifiers: &mut Modifiers) {
    *modifiers = Modifiers::none();
}

fn should_exit_on_ignored_keyboard_input(
    _role: AppUiWindowRole,
    _key: &winit::keyboard::Key,
) -> bool {
    false
}

fn native_close_request_action() -> mondrian_editor_state::Action {
    app_shell_quit_action()
}

fn surface_lifecycle_update(
    reason: SurfaceLifecycleReason,
    current_size: (u32, u32),
    next_size: (u32, u32),
) -> SurfaceLifecycleUpdate {
    if next_size.0 == 0 || next_size.1 == 0 {
        return SurfaceLifecycleUpdate {
            reconfigure_surface: false,
            relayout_root: false,
            request_redraw: false,
            bounds: None,
        };
    }

    let size_changed = current_size != next_size;
    let relayout_root = size_changed || reason == SurfaceLifecycleReason::ScaleFactorChanged;
    let bounds = relayout_root.then(|| Rect::new(0.0, 0.0, next_size.0 as f32, next_size.1 as f32));

    SurfaceLifecycleUpdate {
        reconfigure_surface: size_changed,
        relayout_root,
        request_redraw: relayout_root,
        bounds,
    }
}

fn apply_surface_lifecycle_update(
    reason: SurfaceLifecycleReason,
    next_size: (u32, u32),
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    session: &mut AppUiWindowSession,
    host: &mut AppUiHost,
) {
    let update = surface_lifecycle_update(
        reason,
        (session.config.width, session.config.height),
        next_size,
    );
    if update.reconfigure_surface {
        session.config.width = next_size.0;
        session.config.height = next_size.1;
        session.surface.configure(device, &session.config);
    }
    refresh_display_output_contract(
        DisplayOutputContractRefreshReason::SurfaceLifecycle(reason),
        adapter,
        device,
        session,
        host,
    );
    if update.relayout_root
        && let Some(bounds) = update.bounds
    {
        session.current_bounds.set(bounds);
        TreeWalker::layout(host.active_root_mut(), bounds);
    }
    if update.request_redraw {
        session.window.request_redraw();
    }
}

fn refresh_display_output_contract(
    reason: DisplayOutputContractRefreshReason,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    session: &mut AppUiWindowSession,
    host: &mut AppUiHost,
) {
    let previous = session.display_output_contract.clone();
    let intent = app_ui_surface_presentation_intent_for_role(session.role, host);
    let next =
        match app_ui_display_output_contract(&session.window, &session.surface, adapter, intent) {
            Ok(contract) => contract,
            Err(err) => {
                tracing::warn!(
                    ?reason,
                    "app UI display output contract refresh failed: {err}"
                );
                invalidate_display_dependent_gpu_preview(session, host);
                return;
            }
        };

    let contract_requires_invalidation =
        display_output_contract_requires_gpu_preview_invalidation(&previous, &next);
    let policy_requires_snapshot_refresh = matches!(
        reason,
        DisplayOutputContractRefreshReason::DisplayPolicyChanged
    );
    if !contract_requires_invalidation && !policy_requires_snapshot_refresh {
        return;
    }

    let renderer_rebuilt = contract_requires_invalidation
        && display_output_contract_requires_renderer_rebuild(&previous, &next);
    let replacement_renderer = if renderer_rebuilt {
        match AppUiFrameRenderer::new_for_surface(
            device,
            next.surface_color.format,
            next.surface_color.color_space,
        ) {
            Ok(renderer) => Some(renderer),
            Err(error) => {
                tracing::error!(
                    ?reason,
                    surface_format = ?next.surface_color.format,
                    surface_color_space = ?next.surface_color.color_space,
                    "refusing invalid UI surface presentation carrier: {error}"
                );
                invalidate_display_dependent_gpu_preview(session, host);
                return;
            }
        }
    } else {
        None
    };
    let (color_engine, display_management_policy) = host.resolved_display_color_management();

    let reason_str = format!("{reason:?}");
    let previous_display_name = previous.display_target.name.clone();
    let new_display_name = next.display_target.name.clone();

    let display_resolution = super::display_probe_impl::resolve_display_snapshot(
        super::display_probe_impl::DisplaySnapshotTarget {
            name: next.display_target.name.clone(),
            position: next.display_target.position,
            physical_size: next.display_target.physical_size,
            native_display_id: next.display_target.native_display_id,
            scale_factor: next.display_target.scale_factor_ppm as f64 / 1_000_000.0,
        },
        next.surface_color.format,
        next.surface_color.color_space,
        &format!("{:?}", next.surface_color.hdr_mode),
        &next.supported_surface_color_spaces_for_selected_format(),
        next.display_hdr_info.clone(),
        &color_engine,
        &display_management_policy,
        host.active_program_output_color_space(),
        &reason_str,
    );
    let snapshot = display_resolution.snapshot;

    if let Some(ref prev_snapshot) = session.display_snapshot
        && prev_snapshot.display_id != snapshot.display_id
    {
        tracing::warn!(
            previous_display = ?previous_display_name,
            new_display = ?new_display_name,
            "display changed — previous contract may be stale"
        );
    }

    let snapshot_blockers =
        crate::app::preview_display_contract::preview_blockers_from_snapshot(&snapshot);
    for blocker in &snapshot_blockers {
        host.record_preview_gpu_output_blocker(blocker);
    }

    let previous_identity = session.display_snapshot.as_ref().map(|s| s.contract_identity());
    let new_identity = snapshot.contract_identity();

    session.display_snapshot = Some(snapshot);
    session.display_calibration = display_resolution.calibration;
    host.set_display_output_snapshot(session.display_snapshot.as_ref());
    session.color_engine = color_engine;
    session.display_management_policy = display_management_policy;

    session.viewer_gpu_output_telemetry.record_display_contract_refresh(
        reason,
        &previous,
        &next,
        renderer_rebuilt,
    );
    session.display_output_contract = next;
    session.config.format = session.display_output_contract.surface_color.format;
    session.config.color_space = session.display_output_contract.surface_color.color_space;
    let contract_changed = previous_identity != Some(new_identity);
    if contract_changed {
        // Revoke semantic and physical authority through the old renderer
        // before replacing its registration table. An in-flight owner keeps
        // the execution Reset deferred until its exact callback retires.
        invalidate_display_dependent_gpu_preview(session, host);
    }
    if renderer_rebuilt {
        session.surface.configure(device, &session.config);
        if let Some(renderer) = replacement_renderer {
            session.frame_renderer = renderer;
        }
        host.set_native_decoded_frame_import_support(
            session.viewer_gpu_execution.native_import_support(),
            session.viewer_gpu_execution.native_decode_device_root(),
        );
    }

    tracing::info!(
        ?reason,
        renderer_rebuilt,
        contract_changed,
        previous_identity = ?previous_identity,
        new_identity = ?new_identity,
        display_target = ?session.display_output_contract.display_target,
        surface_format = ?session.display_output_contract.surface_color.format,
        surface_color_space = ?session.display_output_contract.surface_color.color_space,
        surface_hdr_mode = ?session.display_output_contract.surface_color.hdr_mode,
        "app UI display output contract refreshed"
    );
    session.window.request_redraw();
}

fn invalidate_display_dependent_gpu_preview(session: &mut AppUiWindowSession, host: &AppUiHost) {
    let cleanup_deferred =
        cancel_viewer_gpu_submission(session, host, WindowViewerGpuDeferredCleanup::Reset);
    for previous in session.viewer_gpu_presentation.clear().into_iter().flatten() {
        session.frame_renderer.unregister_external_texture(previous.artifact());
        let _ = host.clear_external_viewer_frame_for_artifact(
            previous.output_key(),
            previous.artifact().as_str(),
        );
        drop(previous);
    }
    unregister_program_scopes_textures(session);
    if !cleanup_deferred {
        session.viewer_gpu_execution.reset();
    }
    host.clear_external_viewer_frame();
    host.mark_dirty();
}

/// A fully prepared native Window/Surface/Renderer candidate that has not
/// changed Host-visible display or Viewer publication state.
struct AppUiPreparedWindowSession {
    session: AppUiWindowSession,
    bounds: Rect,
}

struct AppUiActivatedWindowSession {
    session: AppUiWindowSession,
    bounds: Rect,
}

impl AppUiPreparedWindowSession {
    #[cfg(feature = "validation")]
    fn surface_generation_id(&self) -> AppUiSurfaceGenerationId {
        self.session.surface_generation_id
    }

    fn prepare(
        role: AppUiWindowRole,
        window: Arc<winit::window::Window>,
        surface: wgpu::Surface<'static>,
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        host: &AppUiHost,
        viewer_gpu_startup: Option<&mut crate::app::viewer_gpu_startup::ViewerGpuStartupOwner>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let surface_generation_id = AppUiSurfaceGenerationId::next()?;
        apply_window_corner_preference(&window, window_corner_preference_for_role(role));

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(adapter, size.width, size.height)
            .ok_or("Failed surface config")?;
        let intent = app_ui_surface_presentation_intent_for_role(role, host);
        let display_output_contract =
            app_ui_display_output_contract(&window, &surface, adapter, intent)?;
        let surface_color_contract = display_output_contract.surface_color;
        config.format = surface_color_contract.format;
        config.color_space = surface_color_contract.color_space;
        surface.configure(device, &config);
        tracing::info!(
            format = ?surface_color_contract.format,
            color_space = ?surface_color_contract.color_space,
            encoding = ?surface_color_contract.encoding,
            hdr_mode = ?surface_color_contract.hdr_mode,
            display_target = ?display_output_contract.display_target,
            display_hdr_info = ?display_output_contract.display_hdr_info,
            display_tone_map_headroom =
                ?display_output_contract.display_hdr_info.tone_map_headroom(),
            available_formats = ?display_output_contract.available_formats,
            format_color_spaces = ?display_output_contract.format_color_spaces,
            present_modes = ?display_output_contract.present_modes,
            alpha_modes = ?display_output_contract.alpha_modes,
            "app UI surface color contract"
        );

        let bounds = Rect::new(0.0, 0.0, size.width as f32, size.height as f32);
        let (color_engine, display_management_policy) = host.resolved_display_color_management();
        let initial_display_resolution = super::display_probe_impl::resolve_display_snapshot(
            super::display_probe_impl::DisplaySnapshotTarget {
                name: display_output_contract.display_target.name.clone(),
                position: display_output_contract.display_target.position,
                physical_size: display_output_contract.display_target.physical_size,
                native_display_id: display_output_contract.display_target.native_display_id,
                scale_factor: display_output_contract.display_target.scale_factor_ppm as f64
                    / 1_000_000.0,
            },
            display_output_contract.surface_color.format,
            display_output_contract.surface_color.color_space,
            &format!("{:?}", display_output_contract.surface_color.hdr_mode),
            &display_output_contract.supported_surface_color_spaces_for_selected_format(),
            display_output_contract.display_hdr_info.clone(),
            &color_engine,
            &display_management_policy,
            host.active_program_output_color_space(),
            "Startup",
        );
        let initial_snapshot = initial_display_resolution.snapshot;
        let renderer_adapter = AppUiRendererAdapterDiagnostics::from_adapter(adapter);
        let frame_renderer =
            AppUiFrameRenderer::new_for_surface(device, config.format, config.color_space)?;
        if let Some(startup) = viewer_gpu_startup {
            startup.install_runtime(ViewerGpuExecutionRuntime::new(adapter, device, queue)?);
            startup
                .runtime()
                .expect("installed Window runtime")
                .install_cpu_yuv_upload_waker(host.preview_work_watch().completion_waker());
        }

        let session = AppUiWindowSession {
            viewer_gpu_device_progress: ViewerGpuDeviceGenerationMember::empty(),
            surface_generation_id,
            role,
            window,
            surface,
            config: config.clone(),
            display_output_contract,
            display_snapshot: Some(initial_snapshot),
            display_calibration: initial_display_resolution.calibration,
            renderer_adapter,
            color_engine,
            display_management_policy,
            frame_renderer,
            renderer_device: device.clone(),
            renderer_queue: queue.clone(),
            viewer_gpu_execution: ViewerGpuDeviceGenerationMember::empty(),
            viewer_gpu_presentation: WindowViewerGpuPresentationState::default(),
            viewer_gpu_submissions: ViewerGpuSubmissionLifecycle::new(),
            staged_viewer_gpu_successors: PreviewGpuFrameStaging::default(),
            viewer_gpu_deferred_cleanup: WindowViewerGpuDeferredCleanup::None,
            program_scopes_registered: false,
            program_scopes_refresh_requested: false,
            program_scopes_analysis_identity: None,
            viewer_gpu_output_telemetry: AppUiViewerGpuOutputTelemetry::default(),
            render_diagnostic_reporter: AppUiRenderDiagnosticReporter::default(),
            router: build_event_router(
                host.active_root().id(),
                &host.preferences().shortcut_overrides,
            ),
            ui_runtime: WinitUiRuntime::new(),
            last_cursor: Point::new(0.0, 0.0),
            last_window_cursor_icon: None,
            current_bounds: std::cell::Cell::new(bounds),
            modifiers_state: Modifiers::none(),
            pending_initial_redraw: true,
            event_loop_failure: None,
            event_loop_telemetry: AppUiEventLoopTelemetry::default(),
            playback_thread_scheduling: mondrian_platform::PlaybackThreadScheduling::default(),
        };
        Ok(Self { session, bounds })
    }

    fn activate(
        mut self,
        viewer_gpu_startup: Option<&mut crate::app::viewer_gpu_startup::ViewerGpuStartupOwner>,
    ) -> Result<AppUiActivatedWindowSession, Box<dyn std::error::Error>> {
        if let Some(startup) = viewer_gpu_startup {
            let (progress, runtime) = startup
                .activate()
                .ok_or("prepared Window GPU generation was incomplete at publication")?;
            self.session.viewer_gpu_device_progress =
                ViewerGpuDeviceGenerationMember::new(progress);
            self.session.viewer_gpu_execution = ViewerGpuDeviceGenerationMember::new(runtime);
        }
        Ok(AppUiActivatedWindowSession { session: self.session, bounds: self.bounds })
    }
}

impl AppUiActivatedWindowSession {
    fn into_parts(self) -> (AppUiWindowSession, Rect) {
        (self.session, self.bounds)
    }
}

fn publish_active_window_session(host: &mut AppUiHost, session: &AppUiWindowSession, bounds: Rect) {
    // These are the only Host-visible candidate mutations. Callers first place
    // an activated GPU generation in an outer active-session owner so a panic
    // here cannot strand it in a temporary candidate Drop.
    TreeWalker::layout(host.active_root_mut(), bounds);
    host.set_display_output_snapshot(session.display_snapshot.as_ref());
    host.clear_viewer_cpu_fallback();
    if session.viewer_gpu_device_progress.generation_id().is_some() {
        host.set_native_decoded_frame_import_support(
            session.viewer_gpu_execution.native_import_support(),
            session.viewer_gpu_execution.native_decode_device_root(),
        );
    }
}

impl AppUiWindowSession {
    fn shutdown_viewer_gpu_until(
        &mut self,
        host: &AppUiHost,
        deadline: Instant,
    ) -> AppUiActiveWindowGpuShutdownEvidence {
        let publication_cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            clear_viewer_spatial_presentation(self, host);
        }))
        .map_err(|payload| {
            crate::app::execution_panic_diagnostic::execution_panic_diagnostic(
                payload,
                "Window Viewer GPU publication cleanup",
            )
            .to_string()
        });
        let retirement = match self.take_viewer_gpu_generation_retirement() {
            Ok((progress, retirement)) => AppUiWindowGpuRetirementEvidence::Retired(
                progress.retire_device_generation_until(retirement, deadline),
            ),
            Err(WindowViewerGpuGenerationRetirementTakeError::MissingProgressOwner) => {
                AppUiWindowGpuRetirementEvidence::OwnershipFault(
                    AppUiWindowGpuOwnershipFault::MissingProgressOwner,
                )
            }
            Err(WindowViewerGpuGenerationRetirementTakeError::MissingExecutionRuntime) => {
                AppUiWindowGpuRetirementEvidence::OwnershipFault(
                    AppUiWindowGpuOwnershipFault::MissingExecutionRuntime,
                )
            }
        };
        AppUiActiveWindowGpuShutdownEvidence { publication_cleanup, retirement }
    }

    fn take_viewer_gpu_generation_retirement(
        &mut self,
    ) -> Result<
        (
            ViewerGpuDeviceProgressOwner,
            WindowViewerGpuGenerationRetirement,
        ),
        WindowViewerGpuGenerationRetirementTakeError,
    > {
        let progress = self
            .viewer_gpu_device_progress
            .take()
            .ok_or(WindowViewerGpuGenerationRetirementTakeError::MissingProgressOwner)?;
        let Some(runtime) = self.viewer_gpu_execution.take() else {
            // Keep the progress authority in the session so the typed owning
            // fault remains observable. The final fallback Drop may detach it,
            // but this extraction seam never silently leaks or loses it.
            self.viewer_gpu_device_progress = ViewerGpuDeviceGenerationMember::new(progress);
            return Err(WindowViewerGpuGenerationRetirementTakeError::MissingExecutionRuntime);
        };
        let lifecycle = std::mem::replace(
            &mut self.viewer_gpu_submissions,
            ViewerGpuSubmissionLifecycle::new(),
        );
        Ok((
            progress,
            WindowViewerGpuGenerationRetirement {
                runtime: runtime.into_retirement(),
                lifecycle,
                _presentation: std::mem::take(&mut self.viewer_gpu_presentation),
                _renderer_device: self.renderer_device.clone(),
                _renderer_queue: self.renderer_queue.clone(),
                _deferred_cleanup: std::mem::take(&mut self.viewer_gpu_deferred_cleanup),
                _completed_submissions: Vec::new(),
                _lost_submission_owners: Vec::new(),
                native_retirement_error_logged: false,
            },
        ))
    }
}

fn build_event_router(
    root_id: mondrian_ui_core::types::WidgetId,
    shortcut_overrides: &[AppUiShortcutOverride],
) -> EventRouter {
    let mut router = EventRouter::with_platform_and_tooltip(
        root_id,
        Box::new(SystemPlatformService),
        Box::new(TooltipManagerImpl::new(450)),
    );
    register_shortcuts(&mut router, shortcut_overrides);
    router
}

fn drain_actions_and_sync_window_session(
    host: &mut AppUiHost,
    pending_actions: &PendingUiActions,
    platform: &dyn mondrian_platform::PlatformService,
    elwt: &winit::event_loop::ActiveEventLoop,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    session: &mut AppUiWindowSession,
) -> bool {
    let stage_started = Instant::now();
    let previous_color_engine = session.color_engine.clone();
    let previous_display_policy = session.display_management_policy.clone();
    let previous_program_output = host.active_program_output_color_space();
    let commands =
        host.drain_pending_actions(pending_actions, session.current_bounds.get(), platform);
    rebuild_global_shortcuts(&mut session.router, &host.preferences().shortcut_overrides);
    let should_sync_window = shell_commands_should_sync_window_session(commands);
    apply_shell_commands(commands, &session.window, elwt);
    if should_sync_window {
        sync_window_session_role(host, elwt, instance, adapter, device, session);
        if session.role == AppUiWindowRole::Workspace {
            let (next_color_engine, next_display_policy) = host.resolved_display_color_management();
            if display_color_management_changed(
                &previous_color_engine,
                &previous_display_policy,
                &next_color_engine,
                &next_display_policy,
            ) || previous_program_output != host.active_program_output_color_space()
            {
                refresh_display_output_contract(
                    DisplayOutputContractRefreshReason::DisplayPolicyChanged,
                    adapter,
                    device,
                    session,
                    host,
                );
            }
        }
    }
    session
        .event_loop_telemetry
        .record_stage_duration(AppUiEventLoopStage::DrainActions, stage_started.elapsed());
    should_sync_window
}

fn display_color_management_changed(
    previous_engine: &mondrian_core::ColorEngine,
    previous_policy: &mondrian_core::color_models::DisplayManagementPolicy,
    next_engine: &mondrian_core::ColorEngine,
    next_policy: &mondrian_core::color_models::DisplayManagementPolicy,
) -> bool {
    previous_engine != next_engine || previous_policy != next_policy
}

fn shell_commands_should_sync_window_session(commands: AppUiShellCommands) -> bool {
    !commands.quit
}

fn rebuild_global_shortcuts(
    router: &mut EventRouter,
    shortcut_overrides: &[AppUiShortcutOverride],
) {
    router.shortcut_manager_mut().clear_scope(ShortcutScope::Global);
    register_shortcuts(router, shortcut_overrides);
}

fn sync_window_session_role(
    host: &mut AppUiHost,
    elwt: &winit::event_loop::ActiveEventLoop,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    session: &mut AppUiWindowSession,
) {
    let target_role = window_role_for_mode(host.mode());
    if session.role == target_role {
        return;
    }
    if let Err(err) =
        replace_window_session(target_role, elwt, instance, adapter, device, host, session)
    {
        let failure = format!("failed to replace app UI native window: {err}");
        tracing::error!("{failure}");
        if session.event_loop_failure.is_none() {
            session.event_loop_failure = Some(failure);
        }
        elwt.exit();
    }
}

fn replace_window_session(
    role: AppUiWindowRole,
    elwt: &winit::event_loop::ActiveEventLoop,
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    host: &mut AppUiHost,
    session: &mut AppUiWindowSession,
) -> Result<(), Box<dyn std::error::Error>> {
    let old_role = session.role;
    let old_surface_generation = session.surface_generation_id;
    let window = Arc::new(elwt.create_window(window_attributes_for_role(role))?);
    let surface = instance.create_surface(window.clone())?;
    let queue = session.renderer_queue.clone();
    let prepared = AppUiPreparedWindowSession::prepare(
        role, window, surface, adapter, device, &queue, host, None,
    )?;
    let activated = prepared.activate(None)?;
    let (mut next_session, bounds) = activated.into_parts();
    // Native-window replacement does not replace the wgpu device generation.
    // The complete candidate is prepared before the active Window is hidden or
    // its publication authority is revoked.
    clear_viewer_spatial_presentation(session, host);
    session.window.set_visible(false);
    handoff_window_viewer_gpu_device_generation(
        &mut session.viewer_gpu_device_progress,
        &mut next_session.viewer_gpu_device_progress,
        &mut session.viewer_gpu_execution,
        &mut next_session.viewer_gpu_execution,
        &mut session.viewer_gpu_submissions,
        &mut next_session.viewer_gpu_submissions,
        &mut session.viewer_gpu_deferred_cleanup,
        &mut next_session.viewer_gpu_deferred_cleanup,
    );
    with_window_candidate_installed(
        session,
        &mut next_session,
        |active_session, _retired_session| {
            publish_active_window_session(host, active_session, bounds);
        },
    );
    tracing::info!(
        ?old_role,
        ?role,
        surface_generation_before = old_surface_generation.get(),
        surface_generation_after = session.surface_generation_id.get(),
        device_generation = session
            .viewer_gpu_device_progress
            .generation_id()
            .map(ViewerGpuDeviceGenerationId::get),
        "app UI native window replaced"
    );
    session.window.set_visible(true);
    session.window.request_redraw();
    drop(next_session);
    Ok(())
}

fn handoff_window_viewer_gpu_device_generation<P, E, O, C>(
    retiring_progress: &mut P,
    replacement_progress: &mut P,
    retiring_execution: &mut E,
    replacement_execution: &mut E,
    retiring_submissions: &mut ViewerGpuSubmissionLifecycle<O, C>,
    replacement_submissions: &mut ViewerGpuSubmissionLifecycle<O, C>,
    retiring_cleanup: &mut WindowViewerGpuDeferredCleanup,
    replacement_cleanup: &mut WindowViewerGpuDeferredCleanup,
) {
    // The surface/window generation is replaceable; the device-progress
    // worker, execution runtime, callback receiver, retained owners, and their
    // pending cleanup form one indivisible device-generation authority. In
    // particular, an in-flight owner may still protect native decoder
    // resources allocated by the retiring execution runtime.
    std::mem::swap(retiring_progress, replacement_progress);
    std::mem::swap(retiring_execution, replacement_execution);
    std::mem::swap(retiring_submissions, replacement_submissions);
    std::mem::swap(retiring_cleanup, replacement_cleanup);
}

fn window_role_for_mode(mode: AppUiMode) -> AppUiWindowRole {
    match mode {
        AppUiMode::Startup => AppUiWindowRole::Startup,
        AppUiMode::Workspace => AppUiWindowRole::Workspace,
    }
}

fn window_chrome_for_role(role: AppUiWindowRole) -> WindowChrome {
    match role {
        AppUiWindowRole::Startup => WindowChrome {
            title: "Mondrian",
            width: STARTUP_WINDOW_WIDTH,
            height: STARTUP_WINDOW_HEIGHT,
            transparent: true,
            decorations: false,
            rounded_corners: true,
            resizable: false,
            min_size: Some((STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT)),
            max_size: Some((STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT)),
        },
        AppUiWindowRole::Workspace => WindowChrome {
            title: "Mondrian",
            width: WORKSPACE_WINDOW_WIDTH,
            height: WORKSPACE_WINDOW_HEIGHT,
            transparent: false,
            decorations: false,
            rounded_corners: true,
            resizable: true,
            min_size: Some((WORKSPACE_MIN_WIDTH, WORKSPACE_MIN_HEIGHT)),
            max_size: None,
        },
    }
}

fn logical_size(width: f32, height: f32) -> winit::dpi::LogicalSize<f64> {
    winit::dpi::LogicalSize::new(width as f64, height as f64)
}

fn winit_theme_to_theme_preset(theme: Option<winit::window::Theme>) -> ThemePreset {
    match theme {
        Some(winit::window::Theme::Light) => ThemePreset::Light,
        Some(winit::window::Theme::Dark) | None => ThemePreset::Dark,
    }
}

fn window_attributes_for_role(role: AppUiWindowRole) -> winit::window::WindowAttributes {
    let chrome = window_chrome_for_role(role);
    let mut attrs = winit::window::Window::default_attributes()
        .with_title(chrome.title)
        .with_inner_size(logical_size(chrome.width, chrome.height))
        .with_transparent(chrome.transparent)
        .with_decorations(chrome.decorations)
        .with_resizable(chrome.resizable)
        .with_visible(false);
    if let Some(icon) = app_window_icon() {
        attrs = attrs.with_window_icon(Some(icon));
    }
    if let Some((w, h)) = chrome.min_size {
        attrs = attrs.with_min_inner_size(logical_size(w, h));
    }
    if let Some((w, h)) = chrome.max_size {
        attrs = attrs.with_max_inner_size(logical_size(w, h));
    }
    attrs
}

fn app_window_icon() -> Option<winit::window::Icon> {
    const ICON_SIZE: u32 = 64;
    let rgba = crate::product_assets::rasterize_svg_rgba(
        include_str!("../../assets/favicon.svg"),
        ICON_SIZE,
        ICON_SIZE,
    )?;
    winit::window::Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).ok()
}

fn window_corner_preference_for_role(role: AppUiWindowRole) -> WindowCornerPreference {
    if window_chrome_for_role(role).rounded_corners {
        WindowCornerPreference::Round
    } else {
        WindowCornerPreference::Default
    }
}

fn apply_window_corner_preference(
    window: &winit::window::Window,
    preference: WindowCornerPreference,
) {
    platform_window_chrome::apply_window_corner_preference(window, preference);
}

fn update_window_cursor_icon(host: &AppUiHost, session: &mut AppUiWindowSession) {
    let next = window_cursor_icon(host, session);
    if session.last_window_cursor_icon != Some(next) {
        session.window.set_cursor_icon(next);
        session.last_window_cursor_icon = Some(next);
    }
}

fn window_cursor_icon(host: &AppUiHost, session: &AppUiWindowSession) -> winit::window::CursorIcon {
    winit_cursor_icon_for_ui_state(
        session.ui_runtime.is_eyedropper_active(),
        session.ui_runtime.widget_cursor_request(),
        splitter_direction_at_cursor(host, session.last_cursor),
        focused_widget_accepts_text_input(
            host.active_root(),
            session.router.focus_manager().focused_widget(),
        ),
    )
}

fn splitter_direction_at_cursor(host: &AppUiHost, cursor: Point) -> Option<SplitDirection> {
    if host.mode() != AppUiMode::Workspace {
        return None;
    }
    host.root()
        .dock()
        .collect_grab_zones()
        .iter()
        .find(|(zone, _)| zone.contains(cursor))
        .map(|(_, direction)| *direction)
}

fn focused_widget_accepts_text_input(
    root: &dyn mondrian_ui_core::Widget,
    focused: Option<WidgetId>,
) -> bool {
    focused.is_some_and(|id| widget_tree_accepts_text_input(root, id))
}

fn widget_tree_accepts_text_input(
    widget: &dyn mondrian_ui_core::Widget,
    focused: WidgetId,
) -> bool {
    if widget.id() == focused {
        return widget.accepts_text_input();
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index)
            && widget_tree_accepts_text_input(child, focused)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
fn window_bounds_for_role(role: AppUiWindowRole) -> Rect {
    let chrome = window_chrome_for_role(role);
    Rect::new(0.0, 0.0, chrome.width, chrome.height)
}

fn apply_shell_commands(
    commands: AppUiShellCommands,
    window: &winit::window::Window,
    elwt: &winit::event_loop::ActiveEventLoop,
) {
    if commands.toggle_fullscreen {
        toggle_window_fullscreen(window);
    }
    if commands.toggle_maximize {
        window.set_maximized(!window.is_maximized());
    }
    if commands.minimize {
        window.set_minimized(true);
    }
    if commands.begin_window_drag {
        let _ = window.drag_window();
    }
    if commands.quit {
        elwt.exit();
    }
}

fn toggle_window_fullscreen(window: &winit::window::Window) {
    if window.fullscreen().is_some() {
        window.set_fullscreen(None);
    } else {
        window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(
            window.current_monitor(),
        )));
    }
}

#[cfg(target_os = "windows")]
mod platform_window_chrome {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;

    use super::WindowCornerPreference;

    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_DEFAULT: u32 = 0;
    const DWMWCP_ROUND: u32 = 2;

    pub(super) fn apply_window_corner_preference(
        window: &winit::window::Window,
        preference: WindowCornerPreference,
    ) {
        let Ok(window_handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = window_handle.as_raw() else {
            return;
        };

        let preference = match preference {
            WindowCornerPreference::Default => DWMWCP_DEFAULT,
            WindowCornerPreference::Round => DWMWCP_ROUND,
        };
        let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;

        // SAFETY: winit owns a live top-level HWND on this thread and the
        // attribute payload is a pointer to a properly sized DWORD value for
        // the duration of the call.
        let result = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                std::ptr::addr_of!(preference).cast(),
                std::mem::size_of_val(&preference) as u32,
            )
        };
        if result < 0 {
            tracing::debug!(
                hresult = result,
                "failed to apply Windows DWM window corner preference"
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform_window_chrome {
    use super::WindowCornerPreference;

    pub(super) fn apply_window_corner_preference(
        _window: &winit::window::Window,
        _preference: WindowCornerPreference,
    ) {
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pre_active_construction_error_releases_scope_before_building_evidence() {
        struct DropProbe(std::sync::Arc<std::sync::atomic::AtomicBool>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Release);
            }
        }

        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = std::sync::Arc::clone(&dropped);
        let failure = super::catch_pre_active_window_construction(
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            |stage, _viewer_gpu_startup| {
                *stage = super::AppUiPreActiveWindowStartupStage::SurfaceCreated;
                let _probe = DropProbe(observed);
                Err::<(), _>("injected native construction error".to_owned())
            },
        )
        .expect_err("injected error should fail construction");

        assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(failure.primary, "injected native construction error");
        assert_eq!(
            failure.shutdown.last_stage,
            super::AppUiPreActiveWindowStartupStage::SurfaceCreated
        );
        assert!(failure.shutdown.rust_native_authority_released_on_event_loop_thread);
        assert!(matches!(
            failure.shutdown.viewer_gpu,
            super::AppUiPreActiveViewerGpuShutdownEvidence::NotStarted
        ));
        assert!(failure.shutdown.all_created_resources_released());
    }

    #[test]
    fn pre_active_construction_panic_preserves_stage_and_primary_diagnostic() {
        let failure = super::catch_pre_active_window_construction(
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            |stage, _viewer_gpu_startup| -> Result<(), String> {
                *stage = super::AppUiPreActiveWindowStartupStage::DeviceQueueCreated;
                panic!("injected native construction panic")
            },
        )
        .expect_err("injected panic should fail construction");

        assert!(failure.primary.contains("injected native construction panic"));
        assert_eq!(
            failure.shutdown.last_stage,
            super::AppUiPreActiveWindowStartupStage::DeviceQueueCreated
        );
        assert!(failure.shutdown.all_created_resources_released());
    }

    #[test]
    fn caught_event_loop_panic_returns_borrowed_owner_to_the_outer_transaction() {
        let mut owner_marker = 0_u8;
        let result = catch_app_ui_event_loop(|| {
            owner_marker = 1;
            panic!("injected event-loop callback panic")
        });

        assert!(matches!(result, Err(AppUiEventLoopFailure::Panicked(_))));
        owner_marker = 2;
        assert_eq!(owner_marker, 2);
    }

    #[test]
    fn callback_failure_cannot_become_a_normal_event_loop_exit() {
        let result = merge_app_ui_event_loop_failure(
            Ok(()),
            Some("injected role replacement failure".to_owned()),
        );

        assert!(matches!(
            result,
            Err(AppUiEventLoopFailure::Execution(failure))
                if failure == "injected role replacement failure"
        ));
    }

    #[test]
    fn event_loop_and_callback_failures_are_both_retained() {
        let result = merge_app_ui_event_loop_failure(
            Err(AppUiEventLoopFailure::Panicked(
                "event-loop panic".to_owned(),
            )),
            Some("role replacement failure".to_owned()),
        );

        assert!(matches!(
            result,
            Err(AppUiEventLoopFailure::Multiple { primary, callback })
                if matches!(*primary, AppUiEventLoopFailure::Panicked(ref panic)
                    if panic == "event-loop panic")
                    && callback == "role replacement failure"
        ));
    }

    #[test]
    fn candidate_owner_is_installed_before_fallible_host_publication() {
        let mut active = "old-generation";
        let mut candidate = "candidate-generation";

        let publication = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_window_candidate_installed(&mut active, &mut candidate, |installed, retired| {
                assert_eq!(*installed, "candidate-generation");
                assert_eq!(*retired, "old-generation");
                panic!("injected Host publication panic");
            });
        }));

        assert!(publication.is_err());
        assert_eq!(active, "candidate-generation");
        assert_eq!(candidate, "old-generation");
    }

    #[test]
    fn candidate_owner_remains_installed_when_retirement_fails() {
        let mut active = "old-generation";
        let mut candidate = "candidate-generation";

        let transition =
            with_window_candidate_installed(&mut active, &mut candidate, |installed, retired| {
                assert_eq!(*installed, "candidate-generation");
                assert_eq!(*retired, "old-generation");
                Err::<(), _>("injected retirement failure")
            });

        assert_eq!(transition, Err("injected retirement failure"));
        assert_eq!(active, "candidate-generation");
        assert_eq!(candidate, "old-generation");
    }

    #[cfg(feature = "validation")]
    fn clean_final_window_gpu_shutdown() -> AppUiActiveWindowGpuShutdownEvidence {
        AppUiActiveWindowGpuShutdownEvidence {
            publication_cleanup: Ok(()),
            retirement: AppUiWindowGpuRetirementEvidence::Retired(
                ViewerGpuDeviceProgressShutdownEvidence {
                    worker_started: true,
                    worker_terminated: true,
                    worker_panicked: false,
                    timed_out: false,
                    retirement_requested: true,
                    retirement_handoff_accepted: true,
                    retirement_completed: true,
                    renderer_retirement: Some(mondrian_renderer::ViewerGpuRetirementReceipt {
                        cpu_yuv_upload: mondrian_renderer::ViewerCpuYuvUploadWorkerExit::Returned,
                        native_device_removed: false,
                    }),
                    generation_terminal_kind: None,
                },
            ),
        }
    }

    #[cfg(feature = "validation")]
    #[test]
    fn final_window_gpu_qualification_requires_active_publication_and_raw_retirement() {
        let clean = clean_final_window_gpu_shutdown();
        assert!(clean.qualifies_normal_runtime());
        assert!(clean.qualification_failure().is_none());

        let mut dirty_publication = clean;
        dirty_publication.publication_cleanup = Err("publication panic".to_owned());
        assert!(!dirty_publication.qualifies_normal_runtime());
        assert!(dirty_publication
            .qualification_failure()
            .is_some_and(|failure| failure.contains("publication panic")));
    }

    #[cfg(feature = "validation")]
    #[test]
    fn window_result_preserves_operation_loop_and_cleanup_failures() {
        let result = super::merge_window_operation_result::<u32>(
            Some(Err("original operation".to_owned())),
            Ok(()),
            Some("thumbnail timeout".to_owned()),
        )
        .expect_err("cleanup cannot replace operation failure");
        assert!(result.starts_with("original operation"));
        assert!(result.contains("thumbnail timeout"));
        let result = super::merge_window_operation_result::<u32>(
            Some(Err("original operation".to_owned())),
            Err("event loop".to_owned()),
            Some("catalog timeout".to_owned()),
        )
        .expect_err("all failures retained");
        assert!(result.starts_with("original operation"));
        assert!(result.contains("event loop"));
        assert!(result.contains("catalog timeout"));
        assert_eq!(
            super::merge_window_operation_result(Some(Ok(7)), Ok(()), None),
            Ok(7)
        );
        assert!(super::merge_window_operation_result::<u32>(None, Ok(()), None).is_err());
        assert!(super::merge_window_operation_result(
            Some(Ok(7)),
            Ok(()),
            Some("missing receipt".to_owned())
        )
        .is_err());
        assert_eq!(
            super::merge_window_cleanup_failures(
                Some("GPU timeout".to_owned()),
                Some("UI timeout".to_owned())
            ),
            Some("GPU timeout; UI timeout".to_owned())
        );
    }
    use super::*;
    use crate::app::preview_work_notification::preview_work_notification_channel;
    use crate::app::viewer_gpu_output_residency::{
        ViewerGpuOutputDecodeResidency as AppUiViewerGpuOutputDecodeResidency,
        ViewerGpuOutputInputTransformPath as AppUiViewerGpuOutputInputTransformPath,
        ViewerGpuOutputWorkingResidency as AppUiViewerGpuOutputWorkingResidency,
    };
    use mondrian_core::WorkingColorSpace;
    use mondrian_editor_state::Action;
    use mondrian_media::{DecodedFrameResidency, DecodedGpuFrameHandleKind};
    use mondrian_renderer::{
        GpuNativeDecodedFrameImportSupport, GpuNativeDecodedFrameTextureFormat,
        GpuNativeDecodedFrameVideoSampling, GpuVideoChromaLocation, GpuVideoRange,
        ViewerGpuExecutionResidency, ViewerGpuNativeVideoFacts,
    };
    use mondrian_ui_core::widget::{EventContext, PaintContext};
    use mondrian_ui_core::Widget;
    use std::sync::Mutex;

    #[test]
    fn qualification_diagnostics_bind_full_contract_and_runtime_image() {
        let snapshot = mondrian_core::display_probe::FakeDisplayProbe::sdr_pass().snapshot;
        let diagnostics = DisplaySnapshotDiagnostics::from_snapshot(&snapshot);
        assert_eq!(diagnostics.contract_sha256.len(), 64);
        assert!(diagnostics.contract_sha256.chars().all(|value| value.is_ascii_hexdigit()));

        let runtime_image = viewer_runtime_image_sha256().expect("test runtime image must hash");
        assert_eq!(runtime_image.len(), 64);
        assert!(runtime_image.chars().all(|value| value.is_ascii_hexdigit()));
    }

    #[test]
    fn delayed_viewer_callback_survives_native_window_replacement() {
        type CompletionCallback = Box<dyn FnOnce(u64) + Send + 'static>;

        let now = Instant::now();
        let callback = Arc::new(Mutex::new(None::<CompletionCallback>));
        let mut retiring_progress = "active-device-generation".to_owned();
        let mut replacement_progress = "unused-replacement-worker".to_owned();
        let mut retiring_execution = "retained-frame-resource-runtime".to_owned();
        let mut replacement_execution = "unused-replacement-runtime".to_owned();
        let mut retiring_submissions = ViewerGpuSubmissionLifecycle::<String, u64>::new();
        let mut replacement_submissions = ViewerGpuSubmissionLifecycle::<String, u64>::new();
        let mut retiring_cleanup = WindowViewerGpuDeferredCleanup::Reset;
        let mut replacement_cleanup = WindowViewerGpuDeferredCleanup::None;

        let reservation = retiring_submissions.reserve().expect("reserve old Window submission");
        let submission_id = reservation.submission_id();
        let callback_slot = Arc::clone(&callback);
        reservation.commit(
            "retained-media-owner".to_owned(),
            now + Duration::from_secs(1),
            move |registered| {
                *callback_slot.lock().expect("callback slot") = Some(registered);
            },
            || {},
        );

        handoff_window_viewer_gpu_device_generation(
            &mut retiring_progress,
            &mut replacement_progress,
            &mut retiring_execution,
            &mut replacement_execution,
            &mut retiring_submissions,
            &mut replacement_submissions,
            &mut retiring_cleanup,
            &mut replacement_cleanup,
        );

        assert_eq!(replacement_progress, "active-device-generation");
        assert_eq!(replacement_execution, "retained-frame-resource-runtime");
        assert_eq!(replacement_cleanup, WindowViewerGpuDeferredCleanup::Reset);
        assert!(matches!(
            retiring_submissions.poll(now),
            ViewerGpuSubmissionPoll::Idle
        ));
        callback.lock().expect("callback slot").take().expect("registered callback")(73);
        let completed = match replacement_submissions.poll(now) {
            ViewerGpuSubmissionPoll::Completed(completed) => completed,
            _ => panic!("replacement must retain the exact in-flight lifecycle"),
        };
        assert_eq!(completed.submission_id, submission_id);
        assert_eq!(completed.owner, "retained-media-owner");
        assert_eq!(completed.completion, 73);
    }

    #[test]
    fn preview_completion_burst_queues_one_native_event_until_consumed() {
        let (notifier, watch) = preview_work_notification_channel();
        let pending = Arc::new(AtomicBool::new(false));
        let queued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_pending = Arc::clone(&pending);
        let callback_queued = Arc::clone(&queued);
        watch
            .install_waker(move || {
                queue_preview_work_event(&callback_pending, || {
                    callback_queued.fetch_add(1, Ordering::AcqRel);
                    true
                });
            })
            .unwrap_or_else(|failure| panic!("{}", failure.reason));

        for _ in 0..128 {
            notifier.result_became_pollable();
        }

        assert!(pending.load(Ordering::Acquire));
        assert_eq!(queued.load(Ordering::Acquire), 1);
    }

    #[test]
    fn publication_during_bounded_drain_is_requeued_after_rearm() {
        let (notifier, watch) = preview_work_notification_channel();
        let pending = Arc::new(AtomicBool::new(false));
        let queued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_pending = Arc::clone(&pending);
        let callback_queued = Arc::clone(&queued);
        watch
            .install_waker(move || {
                queue_preview_work_event(&callback_pending, || {
                    callback_queued.fetch_add(1, Ordering::AcqRel);
                    true
                });
            })
            .unwrap_or_else(|failure| panic!("{}", failure.reason));
        let drain_target_revision = watch.revision();

        // The publisher observes an already queued event and intentionally
        // coalesces. Rearm must compare revisions after clearing that bit.
        notifier.result_became_pollable();
        assert_eq!(queued.load(Ordering::Acquire), 1);
        let rearm_queued = Arc::clone(&queued);
        assert!(rearm_preview_work_event(
            &pending,
            drain_target_revision,
            &watch,
            move || {
                rearm_queued.fetch_add(1, Ordering::AcqRel);
                true
            }
        ));
        assert!(pending.load(Ordering::Acquire));
        assert_eq!(queued.load(Ordering::Acquire), 2);
    }

    #[test]
    fn panicking_native_event_send_restores_the_coalescing_bit() {
        let pending = AtomicBool::new(false);

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            queue_preview_work_event(&pending, || panic!("test native event send panic"));
        }));

        assert!(panic.is_err());
        assert!(!pending.load(Ordering::Acquire));
        assert!(queue_preview_work_event(&pending, || true));
    }

    #[test]
    fn resource_deadline_never_downgrades_polling() {
        let deadline = Instant::now() + Duration::from_secs(1);

        assert_eq!(
            control_flow_wake_no_later_than(winit::event_loop::ControlFlow::Poll, deadline),
            winit::event_loop::ControlFlow::Poll
        );
    }

    #[test]
    fn resource_deadline_arms_an_idle_event_loop() {
        let deadline = Instant::now() + Duration::from_secs(1);

        assert_eq!(
            control_flow_wake_no_later_than(winit::event_loop::ControlFlow::Wait, deadline),
            winit::event_loop::ControlFlow::WaitUntil(deadline)
        );
    }

    #[test]
    fn resource_deadline_preserves_the_earliest_wakeup() {
        let now = Instant::now();
        let earlier = now + Duration::from_millis(100);
        let later = now + Duration::from_secs(1);

        assert_eq!(
            control_flow_wake_no_later_than(
                winit::event_loop::ControlFlow::WaitUntil(earlier),
                later,
            ),
            winit::event_loop::ControlFlow::WaitUntil(earlier)
        );
        assert_eq!(
            control_flow_wake_no_later_than(
                winit::event_loop::ControlFlow::WaitUntil(later),
                earlier,
            ),
            winit::event_loop::ControlFlow::WaitUntil(earlier)
        );
    }

    #[test]
    fn changing_only_the_color_engine_refreshes_display_management() {
        let policy = mondrian_core::color_models::DisplayManagementPolicy::default();
        assert!(display_color_management_changed(
            &mondrian_core::ColorEngine::mondrian_standard(),
            &policy,
            &mondrian_core::ColorEngine::Aces {
                preset: mondrian_core::AcesConfigPreset::default(),
            },
            &policy,
        ));
    }

    #[test]
    fn hidden_scopes_create_no_viewer_gpu_request() {
        assert_eq!(
            viewer_program_scopes_request(
                false,
                ColorSpace::Rec709,
                ColorSpace::DisplayP3,
                VideoScopesSettings::default(),
            ),
            Ok(None)
        );
        let visible = viewer_program_scopes_request(
            true,
            ColorSpace::Rec709,
            ColorSpace::DisplayP3,
            VideoScopesSettings::default(),
        )
        .expect("supported Program Output")
        .expect("visible request");
        assert_eq!(visible.signal_color_space(), ColorSpace::Rec709);
        assert_eq!(visible.waveform_mode(), WaveformMode::Luma);

        let monitor_settings = VideoScopesSettings {
            waveform_mode: WaveformMode::RgbParade,
            scale: ProgramScopeScale::Nits1000,
            tap: ProgramScopesTap::MonitorOutput,
            ..VideoScopesSettings::default()
        };
        let monitor = viewer_program_scopes_request(
            true,
            ColorSpace::Rec709,
            ColorSpace::DisplayP3,
            monitor_settings,
        )
        .expect("supported Monitor Output")
        .expect("visible monitor request");
        assert_eq!(monitor.signal_color_space(), ColorSpace::DisplayP3);
        assert_eq!(monitor.waveform_mode(), WaveformMode::RgbParade);
        assert_eq!(monitor.scale(), ProgramScopeScale::Nits1000);
        assert_eq!(monitor.tap(), ProgramScopesTap::MonitorOutput);
    }

    #[test]
    fn viewer_monitoring_is_independent_from_scope_panel_visibility() {
        assert!(viewer_signal_monitor_request(
            ColorSpace::Rec709,
            ColorSpace::DisplayP3,
            VideoScopesSettings::default(),
        )
        .expect("inactive settings")
        .is_none());
        let settings = VideoScopesSettings {
            tap: ProgramScopesTap::MonitorOutput,
            monitoring: mondrian_core::SignalMonitoringSettings {
                false_color: true,
                zebra: true,
                gamut_alarm: true,
                ..Default::default()
            },
            ..VideoScopesSettings::default()
        };
        let request =
            viewer_signal_monitor_request(ColorSpace::Rec709, ColorSpace::DisplayP3, settings)
                .expect("supported monitoring signal")
                .expect("active request");
        assert_eq!(request.tap, ProgramScopesTap::MonitorOutput);
        assert_eq!(request.compliance.signal_color_space, ColorSpace::DisplayP3);
    }

    #[test]
    fn display_calibration_proof_requires_exact_source_and_full_fingerprint() {
        let edge = 17_u16;
        let mut samples = Vec::new();
        let denominator = f32::from(edge - 1);
        for blue in 0..edge {
            for green in 0..edge {
                for red in 0..edge {
                    samples.extend_from_slice(&[
                        f32::from(red) / denominator,
                        f32::from(green) / denominator,
                        f32::from(blue) / denominator,
                        1.0,
                    ]);
                }
            }
        }
        let fingerprint =
            mondrian_core::display_calibration::IccProfileFingerprint::from_bytes(b"profile-a");
        let calibration =
            mondrian_core::display_calibration::DisplayCalibrationLut3d::from_rgba32f_samples(
                ColorSpace::Srgb,
                fingerprint,
                edge,
                samples,
            )
            .expect("test calibration");
        let mut snapshot = mondrian_core::display_probe::FakeDisplayProbe::sdr_pass().snapshot;
        snapshot.monitor_profile_status =
            mondrian_core::display_contract::MonitorProfileStatus::ManagedIccCalibration {
                source_color_space: ColorSpace::Srgb,
                profile_fingerprint: fingerprint,
            };

        assert!(validate_display_calibration_proof(Some(&snapshot), &calibration).is_ok());

        snapshot.monitor_profile_status =
            mondrian_core::display_contract::MonitorProfileStatus::ManagedIccCalibration {
                source_color_space: ColorSpace::Srgb,
                profile_fingerprint:
                    mondrian_core::display_calibration::IccProfileFingerprint::from_bytes(
                        b"profile-b",
                    ),
            };
        assert!(validate_display_calibration_proof(Some(&snapshot), &calibration).is_err());
    }

    #[test]
    fn srgb_surface_format_detection_matches_presentation_formats() {
        assert!(is_srgb_surface_format(wgpu::TextureFormat::Bgra8UnormSrgb));
        assert!(is_srgb_surface_format(wgpu::TextureFormat::Rgba8UnormSrgb));
        assert!(!is_srgb_surface_format(wgpu::TextureFormat::Bgra8Unorm));
        assert!(!is_srgb_surface_format(wgpu::TextureFormat::Rgba8Unorm));
    }

    fn test_surface_capabilities(
        formats: Vec<wgpu::TextureFormat>,
        format_capabilities: Vec<wgpu::SurfaceFormatCapabilities>,
    ) -> wgpu::SurfaceCapabilities {
        wgpu::SurfaceCapabilities {
            formats,
            format_capabilities,
            present_modes: vec![wgpu::PresentMode::Fifo],
            alpha_modes: vec![wgpu::CompositeAlphaMode::Auto],
            usages: wgpu::TextureUsages::RENDER_ATTACHMENT,
        }
    }

    fn native_import_support_unavailable() -> GpuNativeDecodedFrameImportSupport {
        GpuNativeDecodedFrameImportSupport::unavailable()
    }

    fn native_video_sampling() -> GpuNativeDecodedFrameVideoSampling {
        GpuNativeDecodedFrameVideoSampling::from_source_color_space(
            ColorSpace::Rec709,
            GpuVideoRange::Limited,
            8,
            GpuVideoChromaLocation::Left,
        )
    }

    #[test]
    fn surface_format_choice_prefers_srgb_without_fallback() {
        let capabilities = test_surface_capabilities(
            vec![
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                wgpu::TextureFormat::Bgra8UnormSrgb,
            ],
            vec![
                wgpu::SurfaceFormatCapabilities {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    color_spaces: wgpu::SurfaceColorSpaces::SRGB
                        | wgpu::SurfaceColorSpaces::DISPLAY_P3,
                },
                wgpu::SurfaceFormatCapabilities {
                    format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    color_spaces: wgpu::SurfaceColorSpaces::SRGB,
                },
            ],
        );

        assert_eq!(
            choose_app_ui_surface_format(&capabilities),
            Ok(AppUiSurfaceColorContract {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                color_space: wgpu::SurfaceColorSpace::Srgb,
                encoding: AppUiSurfaceEncoding::Srgb,
                hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
            })
        );
    }

    #[test]
    fn surface_format_choice_rejects_non_srgb_formats() {
        let capabilities = test_surface_capabilities(
            vec![
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureFormat::Rgba8Unorm,
            ],
            Vec::new(),
        );

        assert_eq!(
            choose_app_ui_surface_format(&capabilities),
            Err(AppUiSurfaceColorContractError {
                intent: AppUiSurfacePresentationIntent::SdrSrgb,
                required_color_space: Some(wgpu::SurfaceColorSpace::Srgb),
                available_formats: vec![
                    wgpu::TextureFormat::Bgra8Unorm,
                    wgpu::TextureFormat::Rgba8Unorm,
                ],
            })
        );
    }

    #[test]
    fn rec601_output_requires_display_transform_before_surface_presentation() {
        assert_eq!(
            app_ui_surface_color_space_for_intent(AppUiSurfacePresentationIntent::DisplayOutput(
                ColorSpace::Rec601Pal
            )),
            None
        );
        assert_eq!(
            app_ui_surface_color_space_for_intent(AppUiSurfacePresentationIntent::DisplayOutput(
                ColorSpace::Rec601Ntsc
            )),
            None
        );
    }

    #[test]
    fn surface_format_choice_rejects_srgb_format_without_srgb_color_space() {
        let capabilities = test_surface_capabilities(
            vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            vec![wgpu::SurfaceFormatCapabilities {
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                color_spaces: wgpu::SurfaceColorSpaces::DISPLAY_P3,
            }],
        );

        assert_eq!(
            choose_app_ui_surface_format(&capabilities),
            Err(AppUiSurfaceColorContractError {
                intent: AppUiSurfacePresentationIntent::SdrSrgb,
                required_color_space: Some(wgpu::SurfaceColorSpace::Srgb),
                available_formats: vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            })
        );
    }

    #[test]
    fn surface_format_choice_can_select_display_p3_surface_contract() {
        let capabilities = test_surface_capabilities(
            vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            vec![wgpu::SurfaceFormatCapabilities {
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                color_spaces: wgpu::SurfaceColorSpaces::SRGB | wgpu::SurfaceColorSpaces::DISPLAY_P3,
            }],
        );

        assert_eq!(
            choose_app_ui_surface_color_contract(
                &capabilities,
                AppUiSurfacePresentationIntent::DisplayOutput(ColorSpace::DisplayP3),
            ),
            Ok(AppUiSurfaceColorContract {
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                color_space: wgpu::SurfaceColorSpace::DisplayP3,
                encoding: AppUiSurfaceEncoding::Srgb,
                hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
            })
        );
    }

    #[test]
    fn surface_format_choice_can_select_pq_hdr_surface_contract() {
        let capabilities = test_surface_capabilities(
            vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            vec![
                wgpu::SurfaceFormatCapabilities {
                    format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    color_spaces: wgpu::SurfaceColorSpaces::SRGB,
                },
                wgpu::SurfaceFormatCapabilities {
                    format: wgpu::TextureFormat::Rgba16Float,
                    color_spaces: wgpu::SurfaceColorSpaces::BT2100_PQ,
                },
                wgpu::SurfaceFormatCapabilities {
                    format: wgpu::TextureFormat::Rgb10a2Unorm,
                    color_spaces: wgpu::SurfaceColorSpaces::BT2100_PQ,
                },
            ],
        );

        assert_eq!(
            choose_app_ui_surface_color_contract(
                &capabilities,
                AppUiSurfacePresentationIntent::DisplayOutput(ColorSpace::Rec2100Pq),
            ),
            Ok(AppUiSurfaceColorContract {
                format: wgpu::TextureFormat::Rgba16Float,
                color_space: wgpu::SurfaceColorSpace::Bt2100Pq,
                encoding: AppUiSurfaceEncoding::Pq,
                hdr_mode: AppUiSurfaceHdrMode::HdrPq,
            })
        );
    }

    #[test]
    fn surface_format_choice_can_select_hlg_hdr_surface_contract() {
        let capabilities = test_surface_capabilities(
            vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            vec![
                wgpu::SurfaceFormatCapabilities {
                    format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    color_spaces: wgpu::SurfaceColorSpaces::SRGB,
                },
                wgpu::SurfaceFormatCapabilities {
                    format: wgpu::TextureFormat::Rgb10a2Unorm,
                    color_spaces: wgpu::SurfaceColorSpaces::BT2100_HLG,
                },
            ],
        );

        assert_eq!(
            choose_app_ui_surface_color_contract(
                &capabilities,
                AppUiSurfacePresentationIntent::DisplayOutput(ColorSpace::Rec2100Hlg),
            ),
            Ok(AppUiSurfaceColorContract {
                format: wgpu::TextureFormat::Rgb10a2Unorm,
                color_space: wgpu::SurfaceColorSpace::Bt2100Hlg,
                encoding: AppUiSurfaceEncoding::Hlg,
                hdr_mode: AppUiSurfaceHdrMode::HdrHlg,
            })
        );
    }

    #[test]
    fn surface_format_choice_rejects_hdr_on_srgb_only_surface() {
        let capabilities = test_surface_capabilities(
            vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            vec![wgpu::SurfaceFormatCapabilities {
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                color_spaces: wgpu::SurfaceColorSpaces::SRGB,
            }],
        );

        assert_eq!(
            choose_app_ui_surface_color_contract(
                &capabilities,
                AppUiSurfacePresentationIntent::DisplayOutput(ColorSpace::Rec2100Hlg),
            ),
            Err(AppUiSurfaceColorContractError {
                intent: AppUiSurfacePresentationIntent::DisplayOutput(ColorSpace::Rec2100Hlg),
                required_color_space: Some(wgpu::SurfaceColorSpace::Bt2100Hlg),
                available_formats: vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            })
        );
    }

    #[test]
    fn surface_format_choice_rejects_log_as_presentation_contract() {
        let capabilities = test_surface_capabilities(
            vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            vec![wgpu::SurfaceFormatCapabilities {
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                color_spaces: wgpu::SurfaceColorSpaces::SRGB,
            }],
        );

        assert_eq!(
            choose_app_ui_surface_color_contract(
                &capabilities,
                AppUiSurfacePresentationIntent::DisplayOutput(ColorSpace::SonySLog3SGamut3Cine,),
            ),
            Err(AppUiSurfaceColorContractError {
                intent: AppUiSurfacePresentationIntent::DisplayOutput(
                    ColorSpace::SonySLog3SGamut3Cine,
                ),
                required_color_space: None,
                available_formats: vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            })
        );
    }

    fn test_display_output_contract() -> AppUiDisplayOutputContract {
        AppUiDisplayOutputContract {
            surface_color: AppUiSurfaceColorContract {
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                color_space: wgpu::SurfaceColorSpace::Srgb,
                encoding: AppUiSurfaceEncoding::Srgb,
                hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
            },
            display_target: AppUiDisplayTarget {
                name: Some("test-display".to_owned()),
                position: (0, 0),
                physical_size: (3840, 2160),
                native_display_id: None,
                native_display_path_id: None,
                scale_factor_ppm: 1_000_000,
                refresh_rate_millihertz: Some(60_000),
            },
            display_hdr_info: wgpu::DisplayHdrInfo::default(),
            available_formats: vec![wgpu::TextureFormat::Bgra8UnormSrgb],
            format_color_spaces: vec![AppUiSurfaceFormatColorSpaces {
                format: wgpu::TextureFormat::Bgra8UnormSrgb,
                srgb: true,
                extended_srgb_linear: false,
                display_p3: false,
                bt2100_pq: false,
                bt2100_hlg: false,
                extended_srgb: false,
                extended_display_p3: false,
            }],
            present_modes: vec![wgpu::PresentMode::Fifo],
            alpha_modes: vec![wgpu::CompositeAlphaMode::Auto],
        }
    }

    #[test]
    fn display_output_contract_blocks_hdr_boundary_on_sdr_surface() {
        let contract = test_display_output_contract();
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::Rec2100Pq,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(
            contract.boundary_blocker(&boundary),
            Some(AppUiDisplayBoundaryBlocker::HdrOutputRequiresHdrSurface {
                output_color_space: ColorSpace::Rec2100Pq,
                selected_surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
                selected_surface_color_space: wgpu::SurfaceColorSpace::Srgb,
                selected_surface_encoding: AppUiSurfaceEncoding::Srgb,
                surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                supported_surface_color_spaces: vec![wgpu::SurfaceColorSpace::Srgb],
            })
        );
        assert_eq!(
            contract.boundary_blocker(&boundary).map(|blocker| blocker.diagnostics()),
            Some(AppUiDisplayBoundaryBlockerDiagnostics {
                kind: AppUiDisplayBoundaryBlockerKind::HdrOutputRequiresHdrSurface,
                output_color_space: ColorSpace::Rec2100Pq,
                selected_surface_format: AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb,
                selected_surface_color_space: AppUiSurfaceColorSpaceDiagnostic::Srgb,
                selected_surface_encoding: AppUiSurfaceEncodingDiagnostic::Srgb,
                surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                supported_surface_color_space_count: 1,
                supports_srgb: true,
                supports_display_p3: false,
                supports_extended_srgb_linear: false,
                supports_extended_srgb: false,
                supports_extended_display_p3: false,
                supports_bt2100_pq: false,
                supports_bt2100_hlg: false,
            })
        );
    }

    #[test]
    fn display_output_contract_accepts_sdr_boundary_on_srgb_surface() {
        let contract = test_display_output_contract();
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::Rec709,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(contract.boundary_blocker(&boundary), None);
    }

    #[test]
    fn display_output_contract_accepts_srgb_boundary_on_srgb_surface() {
        let contract = test_display_output_contract();
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::Srgb,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(contract.boundary_blocker(&boundary), None);
    }

    #[test]
    fn display_presentation_readiness_is_current_for_matching_srgb_surface() {
        let contract = test_display_output_contract();
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::Rec709,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(
            contract.presentation_readiness_for_boundary(&boundary),
            AppUiDisplayPresentationReadinessDiagnostics {
                status: AppUiDisplayPresentationReadinessStatus::Current,
                output_color_space: ColorSpace::Rec709,
                current_surface_format: AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb,
                current_surface_color_space: AppUiSurfaceColorSpaceDiagnostic::Srgb,
                current_surface_encoding: AppUiSurfaceEncodingDiagnostic::Srgb,
                current_surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                desired_surface_format: Some(AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb),
                desired_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::Srgb),
                desired_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
                desired_surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
                payload_blocker: None,
            }
        );
    }

    #[test]
    fn display_output_contract_blocks_display_p3_boundary_on_srgb_surface() {
        let mut contract = test_display_output_contract();
        contract.format_color_spaces[0].display_p3 = true;
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::DisplayP3,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(
            contract.boundary_blocker(&boundary),
            Some(
                AppUiDisplayBoundaryBlocker::OutputColorSpaceRequiresSurfaceColorSpace {
                    output_color_space: ColorSpace::DisplayP3,
                    selected_surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    selected_surface_color_space: wgpu::SurfaceColorSpace::Srgb,
                    selected_surface_encoding: AppUiSurfaceEncoding::Srgb,
                    surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                    supported_surface_color_spaces: vec![
                        wgpu::SurfaceColorSpace::Srgb,
                        wgpu::SurfaceColorSpace::DisplayP3,
                    ],
                }
            )
        );
    }

    #[test]
    fn display_presentation_readiness_reports_supported_p3_reconfiguration_required() {
        let mut contract = test_display_output_contract();
        contract.format_color_spaces[0].display_p3 = true;
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::DisplayP3,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(
            contract.presentation_readiness_for_boundary(&boundary),
            AppUiDisplayPresentationReadinessDiagnostics {
                status: AppUiDisplayPresentationReadinessStatus::ReconfigurationRequired,
                output_color_space: ColorSpace::DisplayP3,
                current_surface_format: AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb,
                current_surface_color_space: AppUiSurfaceColorSpaceDiagnostic::Srgb,
                current_surface_encoding: AppUiSurfaceEncodingDiagnostic::Srgb,
                current_surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                desired_surface_format: Some(AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb),
                desired_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::DisplayP3),
                desired_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
                desired_surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
                payload_blocker: None,
            }
        );
    }

    #[test]
    fn display_output_contract_accepts_display_p3_boundary_on_display_p3_surface() {
        let mut contract = test_display_output_contract();
        contract.surface_color.color_space = wgpu::SurfaceColorSpace::DisplayP3;
        contract.format_color_spaces[0].display_p3 = true;
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::DisplayP3,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(contract.boundary_blocker(&boundary), None);
    }

    #[test]
    fn display_output_contract_accepts_pq_boundary_on_pq_surface() {
        let mut contract = test_display_output_contract();
        contract.surface_color.color_space = wgpu::SurfaceColorSpace::Bt2100Pq;
        contract.surface_color.encoding = AppUiSurfaceEncoding::Pq;
        contract.surface_color.hdr_mode = AppUiSurfaceHdrMode::HdrPq;
        contract.format_color_spaces[0].bt2100_pq = true;
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::Rec2100Pq,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(contract.boundary_blocker(&boundary), None);
    }

    #[test]
    fn display_output_contract_blocks_pq_boundary_on_hlg_surface_as_color_space_mismatch() {
        let mut contract = test_display_output_contract();
        contract.surface_color.color_space = wgpu::SurfaceColorSpace::Bt2100Hlg;
        contract.surface_color.encoding = AppUiSurfaceEncoding::Hlg;
        contract.surface_color.hdr_mode = AppUiSurfaceHdrMode::HdrHlg;
        contract.format_color_spaces[0].bt2100_pq = true;
        contract.format_color_spaces[0].bt2100_hlg = true;
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::Rec2100Pq,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(
            contract.boundary_blocker(&boundary),
            Some(
                AppUiDisplayBoundaryBlocker::OutputColorSpaceRequiresSurfaceColorSpace {
                    output_color_space: ColorSpace::Rec2100Pq,
                    selected_surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    selected_surface_color_space: wgpu::SurfaceColorSpace::Bt2100Hlg,
                    selected_surface_encoding: AppUiSurfaceEncoding::Hlg,
                    surface_hdr_mode: AppUiSurfaceHdrMode::HdrHlg,
                    supported_surface_color_spaces: vec![
                        wgpu::SurfaceColorSpace::Srgb,
                        wgpu::SurfaceColorSpace::Bt2100Pq,
                        wgpu::SurfaceColorSpace::Bt2100Hlg,
                    ],
                }
            )
        );
    }

    #[test]
    fn display_output_contract_blocks_rec2020_boundary_without_surface_contract() {
        let mut contract = test_display_output_contract();
        contract.format_color_spaces[0].display_p3 = true;
        contract.format_color_spaces[0].bt2100_pq = true;
        contract.format_color_spaces[0].bt2100_hlg = true;
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::Rec2020,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(
            contract.boundary_blocker(&boundary),
            Some(
                AppUiDisplayBoundaryBlocker::OutputColorSpaceRequiresSurfaceColorSpace {
                    output_color_space: ColorSpace::Rec2020,
                    selected_surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    selected_surface_color_space: wgpu::SurfaceColorSpace::Srgb,
                    selected_surface_encoding: AppUiSurfaceEncoding::Srgb,
                    surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                    supported_surface_color_spaces: vec![
                        wgpu::SurfaceColorSpace::Srgb,
                        wgpu::SurfaceColorSpace::DisplayP3,
                        wgpu::SurfaceColorSpace::Bt2100Pq,
                        wgpu::SurfaceColorSpace::Bt2100Hlg,
                    ],
                }
            )
        );
    }

    #[test]
    fn display_output_contract_blocks_log_boundary_without_surface_contract() {
        let contract = test_display_output_contract();
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::SonySLog3SGamut3Cine,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(
            contract.boundary_blocker(&boundary),
            Some(
                AppUiDisplayBoundaryBlocker::OutputColorSpaceRequiresSurfaceColorSpace {
                    output_color_space: ColorSpace::SonySLog3SGamut3Cine,
                    selected_surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    selected_surface_color_space: wgpu::SurfaceColorSpace::Srgb,
                    selected_surface_encoding: AppUiSurfaceEncoding::Srgb,
                    surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                    supported_surface_color_spaces: vec![wgpu::SurfaceColorSpace::Srgb],
                }
            )
        );
    }

    #[test]
    fn display_presentation_readiness_reports_unsupported_log_presentation_intent() {
        let contract = test_display_output_contract();
        let boundary = ProgramOutputBoundary::display(
            ColorSpace::SonySLog3SGamut3Cine,
            false,
            mondrian_core::ColorEngine::mondrian_standard(),
        );

        assert_eq!(
            contract.presentation_readiness_for_boundary(&boundary),
            AppUiDisplayPresentationReadinessDiagnostics {
                status: AppUiDisplayPresentationReadinessStatus::UnsupportedPresentationIntent,
                output_color_space: ColorSpace::SonySLog3SGamut3Cine,
                current_surface_format: AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb,
                current_surface_color_space: AppUiSurfaceColorSpaceDiagnostic::Srgb,
                current_surface_encoding: AppUiSurfaceEncodingDiagnostic::Srgb,
                current_surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                desired_surface_format: None,
                desired_surface_color_space: None,
                desired_surface_encoding: None,
                desired_surface_hdr_mode: None,
                payload_blocker: None,
            }
        );
    }

    #[test]
    fn surface_format_supported_color_spaces_have_deterministic_order() {
        let color_spaces = AppUiSurfaceFormatColorSpaces {
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            srgb: true,
            extended_srgb_linear: true,
            display_p3: true,
            bt2100_pq: true,
            bt2100_hlg: true,
            extended_srgb: true,
            extended_display_p3: true,
        };

        assert_eq!(
            color_spaces.supported_surface_color_spaces(),
            vec![
                wgpu::SurfaceColorSpace::Srgb,
                wgpu::SurfaceColorSpace::DisplayP3,
                wgpu::SurfaceColorSpace::ExtendedSrgbLinear,
                wgpu::SurfaceColorSpace::ExtendedSrgb,
                wgpu::SurfaceColorSpace::ExtendedDisplayP3,
                wgpu::SurfaceColorSpace::Bt2100Pq,
                wgpu::SurfaceColorSpace::Bt2100Hlg,
            ]
        );
    }

    #[test]
    fn app_ui_event_loop_telemetry_records_slow_stage_stats() {
        let mut telemetry = AppUiEventLoopTelemetry::default();

        telemetry.record_stage_duration(
            AppUiEventLoopStage::PollBackgroundTasks,
            Duration::from_micros(APP_UI_EVENT_LOOP_SLOW_STAGE_BUDGET_US - 1),
        );
        telemetry.record_stage_duration(
            AppUiEventLoopStage::PollBackgroundTasks,
            Duration::from_micros(APP_UI_EVENT_LOOP_SLOW_STAGE_BUDGET_US),
        );

        let stats = telemetry.stage_stats(AppUiEventLoopStage::PollBackgroundTasks);
        assert_eq!(stats.calls, 2);
        assert_eq!(stats.slow_calls, 1);
        assert_eq!(
            stats.accumulated_duration_us,
            APP_UI_EVENT_LOOP_SLOW_STAGE_BUDGET_US.saturating_mul(2).saturating_sub(1)
        );
        assert_eq!(
            stats.max_duration_us,
            APP_UI_EVENT_LOOP_SLOW_STAGE_BUDGET_US
        );
        assert_eq!(
            stats.last_duration_us,
            Some(APP_UI_EVENT_LOOP_SLOW_STAGE_BUDGET_US)
        );
    }

    #[test]
    fn app_ui_event_loop_stage_names_are_stable() {
        assert_eq!(AppUiEventLoopStage::DrainActions.as_str(), "drain_actions");
        assert_eq!(
            AppUiEventLoopStage::RedrawRequested.as_str(),
            "redraw_requested"
        );
        assert_eq!(
            AppUiEventLoopStage::PrepareViewerGpuPreview.as_str(),
            "prepare_viewer_gpu_preview"
        );
        assert_eq!(
            AppUiEventLoopStage::RefreshIfDirty.as_str(),
            "refresh_if_dirty"
        );
        assert_eq!(
            AppUiEventLoopStage::PaintAndRender.as_str(),
            "paint_and_render"
        );
        assert_eq!(
            AppUiEventLoopStage::PollBackgroundTasks.as_str(),
            "poll_background_tasks"
        );
        assert_eq!(
            AppUiEventLoopStage::AdvancePlaybackClock.as_str(),
            "advance_playback_clock"
        );
    }

    #[test]
    fn viewer_gpu_output_telemetry_records_skip_and_failure_outcomes() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        let blocker = AppUiDisplayBoundaryBlocker::HdrOutputRequiresHdrSurface {
            output_color_space: ColorSpace::Rec2100Pq,
            selected_surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
            selected_surface_color_space: wgpu::SurfaceColorSpace::Srgb,
            selected_surface_encoding: AppUiSurfaceEncoding::Srgb,
            surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
            supported_surface_color_spaces: vec![wgpu::SurfaceColorSpace::Srgb],
        };

        telemetry.record_invocation();
        telemetry.record_non_workspace_skip();
        telemetry.record_invocation();
        telemetry.record_current_skip();
        telemetry.record_invocation();
        telemetry.record_loading_skip();
        telemetry.record_invocation();
        telemetry.record_unavailable_skip();
        telemetry.record_invocation();
        telemetry.record_invalid_texture_key();
        telemetry.record_invocation();
        telemetry.record_display_contract_blocker(&blocker);
        telemetry.record_invocation();
        telemetry.record_record_failure();
        telemetry.record_invocation();
        telemetry.record_missing_output_texture();

        assert_eq!(telemetry.invocations, 8);
        assert_eq!(telemetry.non_workspace_skips, 1);
        assert_eq!(telemetry.current_skips, 1);
        assert_eq!(telemetry.loading_skips, 1);
        assert_eq!(telemetry.unavailable_skips, 1);
        assert_eq!(telemetry.invalid_texture_keys, 1);
        assert_eq!(telemetry.display_contract_blockers, 1);
        assert_eq!(telemetry.display_contract_hdr_surface_blockers, 1);
        assert_eq!(telemetry.display_contract_surface_color_space_blockers, 0);
        assert_eq!(telemetry.record_failures, 1);
        assert_eq!(telemetry.missing_output_textures, 1);
        assert_eq!(telemetry.last_display_contract_blocker, None);
        assert_eq!(
            telemetry.last_outcome,
            Some(AppUiViewerGpuOutputOutcome::OutputTextureMissing)
        );
        assert_eq!(
            telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default()).health,
            AppUiViewerGpuOutputHealthSummary {
                status: AppUiViewerGpuOutputHealthStatus::Failed,
                display_boundary_ready: true,
                presentation_ready: true,
                ..AppUiViewerGpuOutputHealthSummary::default()
            }
        );
        assert_eq!(
            telemetry
                .diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default())
                .health_counts,
            AppUiViewerGpuOutputHealthCounts {
                waiting: 5,
                blocked: 1,
                failed: 2,
                ..AppUiViewerGpuOutputHealthCounts::default()
            }
        );
    }

    #[test]
    fn viewer_gpu_output_telemetry_classifies_display_boundary_blockers() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        let hdr_blocker = AppUiDisplayBoundaryBlocker::HdrOutputRequiresHdrSurface {
            output_color_space: ColorSpace::Rec2100Pq,
            selected_surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
            selected_surface_color_space: wgpu::SurfaceColorSpace::Srgb,
            selected_surface_encoding: AppUiSurfaceEncoding::Srgb,
            surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
            supported_surface_color_spaces: vec![
                wgpu::SurfaceColorSpace::Srgb,
                wgpu::SurfaceColorSpace::Bt2100Pq,
            ],
        };
        let p3_blocker = AppUiDisplayBoundaryBlocker::OutputColorSpaceRequiresSurfaceColorSpace {
            output_color_space: ColorSpace::DisplayP3,
            selected_surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
            selected_surface_color_space: wgpu::SurfaceColorSpace::Srgb,
            selected_surface_encoding: AppUiSurfaceEncoding::Srgb,
            surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
            supported_surface_color_spaces: vec![
                wgpu::SurfaceColorSpace::Srgb,
                wgpu::SurfaceColorSpace::DisplayP3,
            ],
        };

        telemetry.record_display_contract_blocker(&hdr_blocker);
        telemetry.record_display_contract_blocker(&p3_blocker);

        assert_eq!(
            telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default()),
            AppUiViewerGpuOutputDiagnostics {
                display_contract_blockers: 2,
                display_contract_hdr_surface_blockers: 1,
                display_contract_surface_color_space_blockers: 1,
                last_display_contract_blocker: Some(AppUiDisplayBoundaryBlockerDiagnostics {
                    kind:
                        AppUiDisplayBoundaryBlockerKind::OutputColorSpaceRequiresSurfaceColorSpace,
                    output_color_space: ColorSpace::DisplayP3,
                    selected_surface_format: AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb,
                    selected_surface_color_space: AppUiSurfaceColorSpaceDiagnostic::Srgb,
                    selected_surface_encoding: AppUiSurfaceEncodingDiagnostic::Srgb,
                    surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                    supported_surface_color_space_count: 2,
                    supports_srgb: true,
                    supports_display_p3: true,
                    supports_extended_srgb_linear: false,
                    supports_extended_srgb: false,
                    supports_extended_display_p3: false,
                    supports_bt2100_pq: false,
                    supports_bt2100_hlg: false,
                }),
                display_issue_summary: Some(AppUiDisplayIssueSummary {
                    reason: AppUiDisplayIssueReason::OutputColorSpaceRequiresSurfaceColorSpace,
                    output_color_space: ColorSpace::DisplayP3,
                    preceding_display_contract_refresh: None,
                    display_target: None,
                    current_surface_format: None,
                    current_surface_color_space: None,
                    current_surface_encoding: None,
                    selected_surface_format: Some(AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb),
                    selected_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::Srgb),
                    selected_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
                    surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
                    desired_surface_format: None,
                    desired_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::DisplayP3,),
                    desired_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
                    desired_surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
                    payload_blocker: None,
                    supported_surface_color_space_count: Some(2),
                    target_surface_color_space_supported: Some(true),
                }),
                health: AppUiViewerGpuOutputHealthSummary {
                    status: AppUiViewerGpuOutputHealthStatus::Blocked,
                    display_boundary_ready: false,
                    presentation_ready: true,
                    ..AppUiViewerGpuOutputHealthSummary::default()
                },
                health_counts: AppUiViewerGpuOutputHealthCounts {
                    blocked: 2,
                    ..AppUiViewerGpuOutputHealthCounts::default()
                },
                last_outcome: Some(AppUiViewerGpuOutputOutcome::DisplayContractBlocked),
                ..AppUiViewerGpuOutputDiagnostics::default()
            }
        );
    }

    #[test]
    fn viewer_gpu_output_telemetry_records_display_presentation_readiness() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        let readiness = AppUiDisplayPresentationReadinessDiagnostics {
            status: AppUiDisplayPresentationReadinessStatus::ReconfigurationRequired,
            output_color_space: ColorSpace::DisplayP3,
            current_surface_format: AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb,
            current_surface_color_space: AppUiSurfaceColorSpaceDiagnostic::Srgb,
            current_surface_encoding: AppUiSurfaceEncodingDiagnostic::Srgb,
            current_surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
            desired_surface_format: Some(AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb),
            desired_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::DisplayP3),
            desired_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
            desired_surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
            payload_blocker: None,
        };

        telemetry.record_display_presentation_readiness(readiness);

        assert_eq!(
            telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default()),
            AppUiViewerGpuOutputDiagnostics {
                display_presentation_reconfigure_candidates: 1,
                last_display_presentation_readiness: Some(readiness),
                display_issue_summary: Some(AppUiDisplayIssueSummary {
                    reason: AppUiDisplayIssueReason::ReconfigurationRequired,
                    output_color_space: ColorSpace::DisplayP3,
                    preceding_display_contract_refresh: None,
                    display_target: None,
                    current_surface_format: Some(AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb),
                    current_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::Srgb),
                    current_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
                    selected_surface_format: None,
                    selected_surface_color_space: None,
                    selected_surface_encoding: None,
                    surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
                    desired_surface_format: Some(AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb),
                    desired_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::DisplayP3,),
                    desired_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
                    desired_surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
                    payload_blocker: None,
                    supported_surface_color_space_count: None,
                    target_surface_color_space_supported: Some(true),
                }),
                ..AppUiViewerGpuOutputDiagnostics::default()
            }
        );
    }

    #[test]
    fn viewer_gpu_output_diagnostics_summarize_display_contract_blocker() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        telemetry.record_display_contract_blocker(
            &AppUiDisplayBoundaryBlocker::HdrOutputRequiresHdrSurface {
                output_color_space: ColorSpace::Rec2100Pq,
                selected_surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
                selected_surface_color_space: wgpu::SurfaceColorSpace::Srgb,
                selected_surface_encoding: AppUiSurfaceEncoding::Srgb,
                surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                supported_surface_color_spaces: vec![
                    wgpu::SurfaceColorSpace::Srgb,
                    wgpu::SurfaceColorSpace::Bt2100Pq,
                ],
            },
        );

        assert_eq!(
            telemetry
                .diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default())
                .display_issue_summary,
            Some(AppUiDisplayIssueSummary {
                reason: AppUiDisplayIssueReason::HdrOutputRequiresHdrSurface,
                output_color_space: ColorSpace::Rec2100Pq,
                preceding_display_contract_refresh: None,
                display_target: None,
                current_surface_format: None,
                current_surface_color_space: None,
                current_surface_encoding: None,
                selected_surface_format: Some(AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb),
                selected_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::Srgb),
                selected_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
                surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
                desired_surface_format: None,
                desired_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::Bt2100Pq),
                desired_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Pq),
                desired_surface_hdr_mode: Some(AppUiSurfaceHdrMode::HdrPq),
                payload_blocker: None,
                supported_surface_color_space_count: Some(2),
                target_surface_color_space_supported: Some(true),
            })
        );
    }

    #[test]
    fn viewer_gpu_output_diagnostics_summarize_reconfiguration_required() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        let readiness = AppUiDisplayPresentationReadinessDiagnostics {
            status: AppUiDisplayPresentationReadinessStatus::ReconfigurationRequired,
            output_color_space: ColorSpace::DisplayP3,
            current_surface_format: AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb,
            current_surface_color_space: AppUiSurfaceColorSpaceDiagnostic::Srgb,
            current_surface_encoding: AppUiSurfaceEncodingDiagnostic::Srgb,
            current_surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
            desired_surface_format: Some(AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb),
            desired_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::DisplayP3),
            desired_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
            desired_surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
            payload_blocker: None,
        };
        telemetry.record_display_presentation_readiness(readiness);

        assert_eq!(
            telemetry
                .diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default())
                .display_issue_summary,
            Some(AppUiDisplayIssueSummary {
                reason: AppUiDisplayIssueReason::ReconfigurationRequired,
                output_color_space: ColorSpace::DisplayP3,
                preceding_display_contract_refresh: None,
                display_target: None,
                current_surface_format: Some(AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb),
                current_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::Srgb),
                current_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
                selected_surface_format: None,
                selected_surface_color_space: None,
                selected_surface_encoding: None,
                surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
                desired_surface_format: Some(AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb),
                desired_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::DisplayP3),
                desired_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
                desired_surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
                payload_blocker: None,
                supported_surface_color_space_count: None,
                target_surface_color_space_supported: Some(true),
            })
        );
    }

    #[test]
    fn viewer_gpu_output_diagnostics_attach_display_target_to_issue_summary() {
        let _theme_guard = crate::app_ui::test_utils::theme_test_guard();
        let host = AppUiHost::new(AppState::new());
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        telemetry.record_display_contract_blocker(
            &AppUiDisplayBoundaryBlocker::HdrOutputRequiresHdrSurface {
                output_color_space: ColorSpace::Rec2100Pq,
                selected_surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
                selected_surface_color_space: wgpu::SurfaceColorSpace::Srgb,
                selected_surface_encoding: AppUiSurfaceEncoding::Srgb,
                surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                supported_surface_color_spaces: vec![
                    wgpu::SurfaceColorSpace::Srgb,
                    wgpu::SurfaceColorSpace::Bt2100Pq,
                ],
            },
        );
        let display_target = AppUiDisplayTarget {
            name: Some("Reference Monitor".to_owned()),
            position: (1920, 0),
            physical_size: (3840, 2160),
            native_display_id: None,
            native_display_path_id: None,
            scale_factor_ppm: 1_000_000,
            refresh_rate_millihertz: Some(60_000),
        };

        let diagnostics = viewer_gpu_output_diagnostics(
            &host,
            &telemetry,
            &display_target,
            RenderGpuOutputRuntimeDiagnosticsReport::default(),
            None,
            None,
            None,
            None,
            AppUiFrameMetrics::default(),
        );

        assert_eq!(
            diagnostics.display_issue_summary.expect("display issue summary").display_target,
            Some(display_target)
        );
    }

    #[test]
    fn viewer_gpu_output_diagnostics_preserve_display_contract_refresh_history() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        let previous = test_display_output_contract();
        let mut next = previous.clone();
        next.display_target.name = Some("hdr-display".to_owned());
        next.display_target.position = (3840, 0);
        next.surface_color.format = wgpu::TextureFormat::Rgba16Float;
        next.surface_color.color_space = wgpu::SurfaceColorSpace::Bt2100Pq;
        next.surface_color.encoding = AppUiSurfaceEncoding::Pq;
        next.surface_color.hdr_mode = AppUiSurfaceHdrMode::HdrPq;
        next.available_formats.push(wgpu::TextureFormat::Rgba16Float);
        next.format_color_spaces.push(AppUiSurfaceFormatColorSpaces {
            format: wgpu::TextureFormat::Rgba16Float,
            srgb: false,
            extended_srgb_linear: false,
            display_p3: false,
            bt2100_pq: true,
            bt2100_hlg: false,
            extended_srgb: false,
            extended_display_p3: false,
        });
        next.present_modes.push(wgpu::PresentMode::Immediate);
        next.alpha_modes.push(wgpu::CompositeAlphaMode::Opaque);

        telemetry.record_display_contract_refresh(
            DisplayOutputContractRefreshReason::WindowMoved,
            &previous,
            &next,
            true,
        );

        let diagnostics = telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default());

        assert_eq!(diagnostics.display_contract_refreshes, 1);
        assert_eq!(diagnostics.recent_display_contract_refreshes.len(), 1);
        assert_eq!(
            diagnostics.last_display_contract_refresh,
            Some(AppUiDisplayContractRefreshEvent {
                reason: AppUiDisplayContractRefreshReasonDiagnostic::WindowMoved,
                previous: AppUiDisplayOutputContractSnapshot::from_contract(&previous),
                next: AppUiDisplayOutputContractSnapshot::from_contract(&next),
                renderer_rebuilt: true,
                display_target_changed: true,
                surface_format_changed: true,
                surface_color_space_changed: true,
                surface_hdr_mode_changed: true,
                display_tone_map_headroom_changed: false,
                available_surface_formats_changed: true,
                format_color_spaces_changed: true,
                present_modes_changed: true,
                alpha_modes_changed: true,
            })
        );
    }

    #[test]
    fn display_policy_changed_refresh_reason_has_diagnostic_code() {
        assert_eq!(
            AppUiDisplayContractRefreshReasonDiagnostic::from_reason(
                DisplayOutputContractRefreshReason::DisplayPolicyChanged
            ),
            AppUiDisplayContractRefreshReasonDiagnostic::DisplayPolicyChanged
        );
    }

    #[test]
    fn viewer_gpu_output_diagnostics_correlate_issue_with_latest_refresh() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        let previous = test_display_output_contract();
        let mut next = previous.clone();
        next.display_target.position = (3840, 0);
        telemetry.record_display_contract_refresh(
            DisplayOutputContractRefreshReason::WindowMoved,
            &previous,
            &next,
            false,
        );
        telemetry.record_display_contract_blocker(
            &AppUiDisplayBoundaryBlocker::HdrOutputRequiresHdrSurface {
                output_color_space: ColorSpace::Rec2100Pq,
                selected_surface_format: wgpu::TextureFormat::Bgra8UnormSrgb,
                selected_surface_color_space: wgpu::SurfaceColorSpace::Srgb,
                selected_surface_encoding: AppUiSurfaceEncoding::Srgb,
                surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                supported_surface_color_spaces: vec![wgpu::SurfaceColorSpace::Srgb],
            },
        );

        let diagnostics = telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default());

        assert_eq!(
            diagnostics
                .display_issue_summary
                .expect("display issue summary")
                .preceding_display_contract_refresh
                .expect("preceding refresh")
                .reason,
            AppUiDisplayContractRefreshReasonDiagnostic::WindowMoved
        );
    }

    #[test]
    fn viewer_gpu_output_telemetry_accumulates_recorded_stage_diagnostics() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        let first = RenderColorStageDiagnostics {
            total_stages: 2,
            upload_stages: 1,
            gpu_color_stages: 1,
            stage_pixels: 20,
            ..RenderColorStageDiagnostics::default()
        };
        let second = RenderColorStageDiagnostics {
            total_stages: 3,
            upload_stages: 1,
            gpu_color_stages: 1,
            readback_stages: 1,
            gpu_blockers: 1,
            gpu_blocker_breakdown: mondrian_renderer::color::RenderColorStageGpuBlockerBreakdown {
                render_pipeline_not_prepared: 1,
                ..mondrian_renderer::color::RenderColorStageGpuBlockerBreakdown::default()
            },
            stage_pixels: 30,
            ..RenderColorStageDiagnostics::default()
        };

        telemetry.record_registered_frame(first);
        telemetry.record_rejected_external_frame(second);

        assert_eq!(telemetry.registered_frames, 1);
        assert_eq!(telemetry.rejected_external_frames, 1);
        assert_eq!(
            telemetry.last_outcome,
            Some(AppUiViewerGpuOutputOutcome::ExternalFrameRejected)
        );
        assert_eq!(
            telemetry.accumulated_stage_diagnostics,
            RenderColorStageDiagnostics {
                total_stages: 5,
                upload_stages: 2,
                gpu_color_stages: 2,
                readback_stages: 1,
                gpu_blockers: 1,
                gpu_blocker_breakdown:
                    mondrian_renderer::color::RenderColorStageGpuBlockerBreakdown {
                        render_pipeline_not_prepared: 1,
                        ..mondrian_renderer::color::RenderColorStageGpuBlockerBreakdown::default()
                    },
                stage_pixels: 50,
                ..RenderColorStageDiagnostics::default()
            }
        );
        assert_eq!(
            telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default()),
            AppUiViewerGpuOutputDiagnostics {
                registered_frames: 1,
                rejected_external_frames: 1,
                accumulated_stage_report: RenderGpuOutputStageDiagnosticsReport {
                    total_stages: 5,
                    upload_stages: 2,
                    gpu_color_stages: 2,
                    readback_stages: 1,
                    gpu_blockers: 1,
                    gpu_blocker_breakdown:
                        mondrian_renderer::color::RenderColorStageGpuBlockerBreakdown {
                            render_pipeline_not_prepared: 1,
                            ..mondrian_renderer::color::RenderColorStageGpuBlockerBreakdown::default(
                            )
                        },
                    stage_pixels: 50,
                },
                last_stage_report: Some(RenderGpuOutputStageDiagnosticsReport {
                    total_stages: 3,
                    upload_stages: 1,
                    gpu_color_stages: 1,
                    readback_stages: 1,
                    gpu_blockers: 1,
                    gpu_blocker_breakdown:
                        mondrian_renderer::color::RenderColorStageGpuBlockerBreakdown {
                            render_pipeline_not_prepared: 1,
                            ..mondrian_renderer::color::RenderColorStageGpuBlockerBreakdown::default(
                            )
                        },
                    stage_pixels: 30,
                }),
                stage_total_stages: 5,
                stage_upload_stages: 2,
                stage_gpu_color_stages: 2,
                stage_readback_stages: 1,
                stage_gpu_blockers: 1,
                stage_gpu_render_pipeline_blockers: 1,
                stage_pixels: 50,
                health: AppUiViewerGpuOutputHealthSummary {
                    status: AppUiViewerGpuOutputHealthStatus::Rejected,
                    display_boundary_ready: true,
                    presentation_ready: true,
                    output_texture_available: true,
                    ..AppUiViewerGpuOutputHealthSummary::default()
                },
                health_counts: AppUiViewerGpuOutputHealthCounts {
                    rejected: 1,
                    ready: 1,
                    ..AppUiViewerGpuOutputHealthCounts::default()
                },
                last_outcome: Some(AppUiViewerGpuOutputOutcome::ExternalFrameRejected),
                ..AppUiViewerGpuOutputDiagnostics::default()
            }
        );
    }

    #[test]
    fn viewer_gpu_output_diagnostics_include_spatial_runtime_evidence() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        let spatial = mondrian_renderer::GpuViewerSpatialRuntimeDiagnostics {
            pipeline_builds: 1,
            records: 2,
            passthrough_frames: 0,
            prefilter_passes: 3,
            lanczos_passes: 4,
            output_pixels: 5,
        };
        telemetry.record_spatial_runtime(spatial);

        let diagnostics = telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default());
        assert_eq!(diagnostics.spatial_runtime, Some(spatial));
        assert!(serde_json::to_string(&diagnostics)
            .expect("serialize Viewer diagnostics")
            .contains("\"spatial_runtime\""));

        telemetry.record_invocation();
        assert!(telemetry
            .diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default())
            .spatial_runtime
            .is_none());
    }

    #[test]
    fn successful_viewer_gpu_fallback_evidence_does_not_replace_the_completed_frame() {
        let reasons = vec!["native import used the admitted CPU working source".to_owned()];
        let mut diagnostics = Vec::new();

        report_successful_viewer_gpu_fallbacks(&reasons, |reason| {
            diagnostics.push(reason.to_owned());
        });

        assert_eq!(diagnostics, reasons);
    }

    #[test]
    fn viewer_gpu_output_diagnostics_include_uniform_arena_evidence() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        let arena = mondrian_renderer::GpuCompositorUniformArenaDiagnostics {
            buffer_creations: 1,
            uniform_writes: 7,
            high_watermark_slots: 3,
            high_watermark_pages: 1,
            frame_resets: 2,
        };
        telemetry.record_compositor_uniform_arena(arena);

        let diagnostics = telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default());
        assert_eq!(diagnostics.compositor_uniform_arena, Some(arena));
        assert!(serde_json::to_string(&diagnostics)
            .expect("serialize Viewer diagnostics")
            .contains("\"compositor_uniform_arena\""));

        telemetry.record_invocation();
        assert!(telemetry
            .diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default())
            .compositor_uniform_arena
            .is_none());
    }

    #[test]
    fn viewer_gpu_output_diagnostics_include_creative_lut_residency_evidence() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        let creative_luts = mondrian_renderer::GpuCreativeLutCacheDiagnostics {
            cache_hits: 5,
            cache_misses: 2,
            texture_uploads: 2,
            evictions: 1,
            oversized_bypasses: 1,
            resident_entries: 1,
            resident_texture_bytes: 32_768,
        };
        telemetry.record_compositor_creative_luts(creative_luts);

        let diagnostics = telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default());
        assert_eq!(diagnostics.compositor_creative_luts, Some(creative_luts));
        assert!(serde_json::to_string(&diagnostics)
            .expect("serialize Viewer diagnostics")
            .contains("\"compositor_creative_luts\""));

        telemetry.record_invocation();
        assert!(telemetry
            .diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default())
            .compositor_creative_luts
            .is_none());
    }

    #[test]
    fn viewer_gpu_output_telemetry_records_prepare_duration() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();

        telemetry.record_prepare_duration(Duration::from_micros(400));
        telemetry.record_prepare_duration(Duration::from_micros(900));

        let diagnostics = telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default());
        assert_eq!(diagnostics.prepare_attempts_timed, 2);
        assert_eq!(diagnostics.accumulated_prepare_duration_us, 1_300);
        assert_eq!(diagnostics.max_prepare_duration_us, 900);
        assert_eq!(diagnostics.last_prepare_duration_us, Some(900));
    }

    #[test]
    fn preview_gpu_composite_residency_reports_gpu_ocio_input_for_media_layers() {
        let residency = preview_gpu_composite_frame_residency(
            ViewerGpuExecutionResidency {
                media_layers: 2,
                gpu_input_layers: 2,
                ..ViewerGpuExecutionResidency::default()
            },
            native_import_support_unavailable(),
        );

        assert_eq!(
            residency.decode_residency,
            AppUiViewerGpuOutputDecodeResidency::CpuDecodedRgba
        );
        assert_eq!(
            residency.working_residency,
            AppUiViewerGpuOutputWorkingResidency::GpuWorkingCompositeExecuted
        );
        assert_eq!(
            residency.input_transform_path,
            AppUiViewerGpuOutputInputTransformPath::GpuOcio
        );
        assert!(!residency.zero_copy);
        assert!(residency.low_copy);
        assert_eq!(residency.upload_count, 2);
        assert!(residency.reason.contains("GPU OCIO input"));
        let native_video_import = residency
            .native_video_import
            .expect("media path reports native import readiness");
        assert_eq!(
            native_video_import.status,
            crate::app::native_video_import::NativeVideoImportReadinessStatus::CpuDecodedMedia
        );
        assert!(!native_video_import.zero_copy_ready);
    }

    #[test]
    fn preview_gpu_composite_residency_preserves_native_decoder_facts() {
        let residency = preview_gpu_composite_frame_residency(
            ViewerGpuExecutionResidency {
                media_layers: 1,
                native_decoder_gpu_layers: 1,
                gpu_input_layers: 1,
                native_video_import: Some(ViewerGpuNativeVideoFacts {
                    decoder_residency: DecodedFrameResidency::GpuTexture,
                    decoder_handle_kind: Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
                    source_texture_format: Some(GpuNativeDecodedFrameTextureFormat::Nv12),
                    source_video_sampling: Some(native_video_sampling()),
                }),
                ..ViewerGpuExecutionResidency::default()
            },
            native_import_support_unavailable(),
        );

        assert_eq!(
            residency.decode_residency,
            AppUiViewerGpuOutputDecodeResidency::NativeGpuDecoded
        );
        assert!(!residency.zero_copy);
        assert!(residency.low_copy);
        assert!(residency.reason.contains("Native decoded media"));
        let native_video_import = residency
            .native_video_import
            .expect("native decoded media reports import readiness");
        assert!(native_video_import.decoder_gpu_resident);
        assert_eq!(
            native_video_import.decoder_handle_kind.as_deref(),
            Some("D3D11Texture2D")
        );
        assert_ne!(
            native_video_import.status,
            crate::app::native_video_import::NativeVideoImportReadinessStatus::CpuDecodedMedia
        );
    }

    #[test]
    fn preview_gpu_composite_residency_does_not_invent_a_native_copy_mode() {
        let residency = preview_gpu_composite_frame_residency(
            ViewerGpuExecutionResidency {
                media_layers: 1,
                native_decoder_gpu_layers: 1,
                native_video_import: Some(ViewerGpuNativeVideoFacts {
                    decoder_residency: DecodedFrameResidency::GpuTexture,
                    decoder_handle_kind: Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
                    source_texture_format: Some(GpuNativeDecodedFrameTextureFormat::Nv12),
                    source_video_sampling: Some(native_video_sampling()),
                }),
                ..ViewerGpuExecutionResidency::default()
            },
            native_import_support_unavailable(),
        );

        assert_eq!(
            residency.decode_residency,
            AppUiViewerGpuOutputDecodeResidency::NativeGpuDecoded
        );
        assert_eq!(
            residency.input_transform_path,
            AppUiViewerGpuOutputInputTransformPath::GpuNativeVideoImport
        );
        assert_eq!(residency.upload_count, 0);
        assert!(!residency.zero_copy);
        assert!(!residency.low_copy);
        assert_eq!(residency.native_bridge_copy_count, 0);
        let native_video_import = residency
            .native_video_import
            .expect("native decoded media reports import readiness");
        assert!(native_video_import.decoder_gpu_resident);
        assert_ne!(
            native_video_import.status,
            crate::app::native_video_import::NativeVideoImportReadinessStatus::CpuDecodedMedia
        );
    }

    #[test]
    fn preview_gpu_composite_residency_treats_cpu_transfer_as_cpu_decoded() {
        let residency = preview_gpu_composite_frame_residency(
            ViewerGpuExecutionResidency {
                media_layers: 1,
                gpu_input_layers: 1,
                native_video_import: Some(ViewerGpuNativeVideoFacts {
                    decoder_residency: DecodedFrameResidency::CpuRgba,
                    decoder_handle_kind: Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
                    source_texture_format: Some(GpuNativeDecodedFrameTextureFormat::Nv12),
                    source_video_sampling: Some(native_video_sampling()),
                }),
                ..ViewerGpuExecutionResidency::default()
            },
            GpuNativeDecodedFrameImportSupport::ready_zero_copy(
                vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
                vec![GpuNativeDecodedFrameTextureFormat::Nv12],
            ),
        );

        assert_eq!(
            residency.decode_residency,
            AppUiViewerGpuOutputDecodeResidency::CpuDecodedRgba
        );
        assert!(!residency.zero_copy);
        let native_video_import = residency
            .native_video_import
            .expect("media path reports native import readiness");
        assert_eq!(
            native_video_import.status,
            crate::app::native_video_import::NativeVideoImportReadinessStatus::CpuDecodedMedia
        );
        assert!(!native_video_import.decoder_gpu_resident);
        assert!(!native_video_import.zero_copy_ready);
    }

    #[test]
    fn preview_gpu_composite_residency_reports_mixed_gpu_input_fallback() {
        let residency = preview_gpu_composite_frame_residency(
            ViewerGpuExecutionResidency {
                media_layers: 2,
                gpu_input_layers: 1,
                cpu_upload_layers: 1,
                gpu_input_failures: 1,
                ..ViewerGpuExecutionResidency::default()
            },
            native_import_support_unavailable(),
        );

        assert_eq!(
            residency.input_transform_path,
            AppUiViewerGpuOutputInputTransformPath::MixedCpuOcioAndGpuOcio
        );
        assert_eq!(residency.upload_count, 2);
        assert!(residency.reason.contains("succeeded for 1 media layer"));
        assert!(residency.reason.contains("1 GPU input failure"));
    }

    #[test]
    fn preview_gpu_composite_residency_omits_native_video_report_for_procedural_layers() {
        let residency = preview_gpu_composite_frame_residency(
            ViewerGpuExecutionResidency {
                procedural_layers: 1,
                ..ViewerGpuExecutionResidency::default()
            },
            native_import_support_unavailable(),
        );

        assert_eq!(
            residency.decode_residency,
            AppUiViewerGpuOutputDecodeResidency::ProceduralGpuNative
        );
        assert!(residency.native_video_import.is_none());
    }

    #[test]
    fn viewer_gpu_output_health_reports_ready_registered_native_boundary() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        telemetry.record_invocation();
        telemetry.record_display_presentation_readiness(
            AppUiDisplayPresentationReadinessDiagnostics {
                status: AppUiDisplayPresentationReadinessStatus::Current,
                output_color_space: ColorSpace::Srgb,
                current_surface_format: AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb,
                current_surface_color_space: AppUiSurfaceColorSpaceDiagnostic::Srgb,
                current_surface_encoding: AppUiSurfaceEncodingDiagnostic::Srgb,
                current_surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                desired_surface_format: None,
                desired_surface_color_space: None,
                desired_surface_encoding: None,
                desired_surface_hdr_mode: None,
                payload_blocker: None,
            },
        );
        telemetry.record_registered_frame(RenderColorStageDiagnostics {
            total_stages: 2,
            upload_stages: 1,
            gpu_color_stages: 1,
            stage_pixels: 20,
            ..RenderColorStageDiagnostics::default()
        });

        assert_eq!(
            telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default()).health,
            AppUiViewerGpuOutputHealthSummary {
                status: AppUiViewerGpuOutputHealthStatus::Ready,
                viewer_output_ready: true,
                native_gpu_boundary_ready: true,
                display_boundary_ready: true,
                presentation_ready: true,
                stage_sequence_ready: true,
                no_gpu_blockers: true,
                output_texture_available: true,
                external_texture_registered: true,
            }
        );
        assert_eq!(
            telemetry
                .diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default())
                .health_counts,
            AppUiViewerGpuOutputHealthCounts {
                ready: 1,
                ..AppUiViewerGpuOutputHealthCounts::default()
            }
        );
    }

    #[test]
    fn viewer_gpu_output_health_reports_degraded_registered_presentation() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        telemetry.record_invocation();
        let readiness = AppUiDisplayPresentationReadinessDiagnostics {
            status: AppUiDisplayPresentationReadinessStatus::ReconfigurationRequired,
            output_color_space: ColorSpace::DisplayP3,
            current_surface_format: AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb,
            current_surface_color_space: AppUiSurfaceColorSpaceDiagnostic::Srgb,
            current_surface_encoding: AppUiSurfaceEncodingDiagnostic::Srgb,
            current_surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
            desired_surface_format: Some(AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb),
            desired_surface_color_space: Some(AppUiSurfaceColorSpaceDiagnostic::DisplayP3),
            desired_surface_encoding: Some(AppUiSurfaceEncodingDiagnostic::Srgb),
            desired_surface_hdr_mode: Some(AppUiSurfaceHdrMode::SdrOnly),
            payload_blocker: None,
        };
        telemetry.record_display_presentation_readiness(readiness);
        telemetry.record_registered_frame(RenderColorStageDiagnostics {
            total_stages: 2,
            upload_stages: 1,
            gpu_color_stages: 1,
            stage_pixels: 20,
            ..RenderColorStageDiagnostics::default()
        });

        assert_eq!(
            telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default()).health,
            AppUiViewerGpuOutputHealthSummary {
                status: AppUiViewerGpuOutputHealthStatus::Degraded,
                viewer_output_ready: false,
                native_gpu_boundary_ready: true,
                display_boundary_ready: true,
                presentation_ready: false,
                stage_sequence_ready: true,
                no_gpu_blockers: true,
                output_texture_available: true,
                external_texture_registered: true,
            }
        );
        assert_eq!(
            telemetry
                .diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default())
                .health_counts,
            AppUiViewerGpuOutputHealthCounts {
                degraded: 1,
                ..AppUiViewerGpuOutputHealthCounts::default()
            }
        );
    }

    #[test]
    fn viewer_gpu_output_health_resets_last_stage_on_new_invocation() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        telemetry.record_invocation();
        telemetry.record_registered_frame(RenderColorStageDiagnostics {
            total_stages: 2,
            upload_stages: 1,
            gpu_color_stages: 1,
            stage_pixels: 20,
            ..RenderColorStageDiagnostics::default()
        });
        telemetry.record_invocation();
        telemetry.record_loading_skip();

        assert_eq!(
            telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default()).health,
            AppUiViewerGpuOutputHealthSummary {
                status: AppUiViewerGpuOutputHealthStatus::Waiting,
                display_boundary_ready: true,
                presentation_ready: true,
                ..AppUiViewerGpuOutputHealthSummary::default()
            }
        );
    }

    #[test]
    fn viewer_gpu_output_diagnostics_jsonl_includes_health_summary() {
        let mut telemetry = AppUiViewerGpuOutputTelemetry::default();
        let previous_contract = test_display_output_contract();
        let mut next_contract = previous_contract.clone();
        next_contract.display_target.position = (3840, 0);
        telemetry.record_display_contract_refresh(
            DisplayOutputContractRefreshReason::WindowMoved,
            &previous_contract,
            &next_contract,
            false,
        );
        telemetry.record_invocation();
        telemetry.record_display_presentation_readiness(
            AppUiDisplayPresentationReadinessDiagnostics {
                status: AppUiDisplayPresentationReadinessStatus::Current,
                output_color_space: ColorSpace::Srgb,
                current_surface_format: AppUiSurfaceFormatDiagnostic::Bgra8UnormSrgb,
                current_surface_color_space: AppUiSurfaceColorSpaceDiagnostic::Srgb,
                current_surface_encoding: AppUiSurfaceEncodingDiagnostic::Srgb,
                current_surface_hdr_mode: AppUiSurfaceHdrMode::SdrOnly,
                desired_surface_format: None,
                desired_surface_color_space: None,
                desired_surface_encoding: None,
                desired_surface_hdr_mode: None,
                payload_blocker: None,
            },
        );
        telemetry.record_registered_frame(RenderColorStageDiagnostics {
            total_stages: 2,
            upload_stages: 1,
            gpu_color_stages: 1,
            stage_pixels: 20,
            ..RenderColorStageDiagnostics::default()
        });
        telemetry.record_prepare_duration(Duration::from_micros(1_234));
        telemetry.last_frame_context = Some(AppUiViewerGpuOutputFrameContext {
            sequence_id: "sequence-for-jsonl".to_owned(),
            frame: 42,
            width: 1920,
            height: 1080,
            external_texture_key: "app-ui.viewer.gpu:sequence-for-jsonl:1920x1080:feed".to_owned(),
            output_target: AppUiViewerGpuOutputTarget::Display,
            output_color_space: ColorSpace::Srgb,
            monitor_color_space: ColorSpace::Srgb,
            tone_map: false,
            preview_candidate_id: Some(2),
            preview_candidate_state: AppUiViewerGpuOutputPreviewCandidateState::Ready,
            display_view: Some(AppUiViewerGpuOutputDisplayView {
                display: "sRGB Display".to_owned(),
                view: "Standard".to_owned(),
            }),
            frame_residency: AppUiViewerGpuOutputFrameResidency {
                decode_residency: AppUiViewerGpuOutputDecodeResidency::CpuDecodedRgba,
                working_residency:
                    AppUiViewerGpuOutputWorkingResidency::GpuWorkingCompositeExecuted,
                input_transform_path: AppUiViewerGpuOutputInputTransformPath::CpuOcio,
                execution_observed: true,
                zero_copy: false,
                low_copy: true,
                upload_count: 1,
                native_bridge_copy_count: 0,
                readback_count: 0,
                reason: "test residency".to_owned(),
                native_video_import: None,
            },
        });
        let mut diagnostics =
            telemetry.diagnostics(RenderGpuOutputRuntimeDiagnosticsReport::default());
        diagnostics.qualification_run_id = Some("qualification-run".to_owned());
        diagnostics.process_instance_id = Some("process-instance".to_owned());
        diagnostics.process_id = Some(42);
        diagnostics.qualification_record_sequence = Some(7);
        diagnostics.runtime_image_sha256 = Some("a".repeat(64));
        diagnostics.renderer_adapter = Some(AppUiRendererAdapterDiagnostics {
            name: "Qualification GPU".to_owned(),
            vendor_id: "10de".to_owned(),
            device_id: "2684".to_owned(),
            device_type: "DiscreteGpu".to_owned(),
            driver: "qualified-driver".to_owned(),
            driver_info: "qualified-driver-info".to_owned(),
            backend: "Dx12".to_owned(),
        });
        let display_output_contract =
            mondrian_core::display_probe::FakeDisplayProbe::sdr_pass().snapshot;
        diagnostics.display_snapshot = Some(DisplaySnapshotDiagnostics::from_snapshot(
            &display_output_contract,
        ));
        diagnostics.display_output_contract = Some(display_output_contract);
        diagnostics.display_calibration_identity_sha256 = Some("b".repeat(64));
        diagnostics.display_calibration_rendering_intent =
            Some(mondrian_core::IccRenderingIntent::RelativeColorimetric);
        diagnostics.last_color_rejection = Some(PreviewColorRejection {
            asset_id: mondrian_core::types::AssetId::new(),
            path: PathBuf::from("E:/media/missing-color-tags.mov"),
            missing_metadata_policy:
                mondrian_timeline::sequence::MissingColorMetadataPolicy::RejectMedia,
            source:
                mondrian_timeline::sequence::InputColorResolutionSource::MissingPolicyRejectMedia,
            override_color_space: None,
            executable_color_space: None,
            working_color_space: WorkingColorSpace::LinearRec2020,
            diagnostic_summary: "source=MissingMetadata,warnings=missing_cicp".to_string(),
            diagnostic_issue_summary: mondrian_media::VideoColorDiagnosticIssueSummary {
                executable_color_space: None,
                source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                has_raw_cicp_metadata: false,
                metadata_hint_count: 0,
                evidence_count: 0,
                warning_count: 1,
                multiple_metadata_hints: 0,
                ignored_metadata_hints: 0,
                metadata_hint_overrides_cicp_tags: 0,
                lower_priority_metadata_hints: 0,
                ignored_lower_priority_metadata_hints: 0,
                partial_cicp_tags: 0,
                missing_cicp_tags: 1,
                unsupported_cicp_tags: 0,
                decoder_unavailable: 0,
                hdr_side_data_count: 0,
                has_mastering_display_metadata: false,
                has_content_light_metadata: false,
                has_dynamic_hdr10_plus: false,
                has_dolby_vision_config: false,
                has_icc_profile: false,
                icc_cicp_mismatch: 0,
                icc_profile_unmapped: 0,
                has_user_visible_warnings: true,
            },
        });
        let output_path = std::env::temp_dir().join(format!(
            "mondrian-viewer-gpu-output-diagnostics-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after unix epoch")
                .as_nanos()
        ));

        write_viewer_gpu_output_diagnostics_to_path(&output_path, &diagnostics)
            .expect("write viewer GPU output JSONL");
        let contents = std::fs::read_to_string(&output_path).expect("read viewer GPU output JSONL");
        let _ = std::fs::remove_file(&output_path);
        let json: serde_json::Value =
            serde_json::from_str(contents.trim()).expect("parse viewer GPU output JSONL");

        assert_eq!(json["health"]["status"], "Ready");
        assert_eq!(json["health"]["viewer_output_ready"], true);
        assert_eq!(json["health"]["native_gpu_boundary_ready"], true);
        assert_eq!(json["health"]["presentation_ready"], true);
        assert_eq!(json["health_counts"]["ready"], 1);
        assert_eq!(json["health_counts"]["degraded"], 0);
        assert_eq!(json["health_counts"]["failed"], 0);
        assert_eq!(json["qualification_run_id"], "qualification-run");
        assert_eq!(json["process_instance_id"], "process-instance");
        assert_eq!(json["process_id"], 42);
        assert_eq!(json["qualification_record_sequence"], 7);
        assert_eq!(json["runtime_image_sha256"], "a".repeat(64));
        assert_eq!(json["renderer_adapter"]["backend"], "Dx12");
        assert_eq!(
            json["display_snapshot"]["contract_sha256"].as_str().map(str::len),
            Some(64)
        );
        assert_eq!(
            json["display_output_contract"]["surface_color_space"],
            "Srgb"
        );
        assert_eq!(json["display_calibration_identity_sha256"], "b".repeat(64));
        assert_eq!(
            json["display_calibration_rendering_intent"],
            "RelativeColorimetric"
        );
        assert_eq!(json["stage_gpu_color_stages"], 1);
        assert_eq!(json["accumulated_stage_report"]["gpu_color_stages"], 1);
        assert_eq!(json["accumulated_stage_report"]["upload_stages"], 1);
        assert_eq!(json["last_stage_report"]["gpu_color_stages"], 1);
        assert_eq!(json["runtime_report"]["shader_cache_entries"], 0);
        assert_eq!(json["runtime_report"]["backend_object_entries"], 0);
        assert_eq!(json["last_outcome"], "Registered");
        assert_eq!(json["display_contract_refreshes"], 1);
        assert_eq!(json["prepare_attempts_timed"], 1);
        assert_eq!(json["accumulated_prepare_duration_us"], 1234);
        assert_eq!(json["max_prepare_duration_us"], 1234);
        assert_eq!(json["last_prepare_duration_us"], 1234);
        assert_eq!(
            json["recent_display_contract_refreshes"][0]["reason"],
            "WindowMoved"
        );
        assert_eq!(
            json["last_display_contract_refresh"]["display_target_changed"],
            true
        );
        assert_eq!(
            json["last_frame_context"]["sequence_id"],
            "sequence-for-jsonl"
        );
        assert_eq!(json["last_frame_context"]["frame"], 42);
        assert_eq!(json["last_frame_context"]["width"], 1920);
        assert_eq!(json["last_frame_context"]["height"], 1080);
        assert_eq!(json["last_frame_context"]["output_target"], "Display");
        assert_eq!(json["last_frame_context"]["output_color_space"], "Srgb");
        assert_eq!(json["last_frame_context"]["preview_candidate_id"], 2);
        assert_eq!(
            json["last_frame_context"]["preview_candidate_state"],
            "Ready"
        );
        assert_eq!(
            json["last_frame_context"]["display_view"]["view"],
            "Standard"
        );
        assert_eq!(
            json["last_frame_context"]["frame_residency"]["decode_residency"],
            "CpuDecodedRgba"
        );
        assert_eq!(
            json["last_frame_context"]["frame_residency"]["working_residency"],
            "GpuWorkingCompositeExecuted"
        );
        assert_eq!(
            json["last_frame_context"]["frame_residency"]["execution_observed"],
            true
        );
        assert_eq!(
            json["last_frame_context"]["frame_residency"]["input_transform_path"],
            "CpuOcio"
        );
        assert_eq!(
            json["last_frame_context"]["frame_residency"]["zero_copy"],
            false
        );
        assert_eq!(
            json["last_frame_context"]["frame_residency"]["upload_count"],
            1
        );
        assert_eq!(
            json["last_color_rejection"]["diagnostic_issue_summary"]["missing_cicp_tags"],
            1
        );
        assert_eq!(
            json["last_color_rejection"]["missing_metadata_policy"],
            "RejectMedia"
        );
    }

    #[test]
    fn display_output_contract_change_invalidates_gpu_preview_resources() {
        let first = test_display_output_contract();
        let mut second = first.clone();
        second.display_target.position = (3840, 0);

        assert!(display_output_contract_requires_gpu_preview_invalidation(
            &first, &second
        ));
        assert!(!display_output_contract_requires_renderer_rebuild(
            &first, &second
        ));
    }

    #[test]
    fn display_output_contract_surface_format_change_rebuilds_renderer() {
        let first = test_display_output_contract();
        let mut second = first.clone();
        second.surface_color.format = wgpu::TextureFormat::Rgba8UnormSrgb;

        assert!(display_output_contract_requires_gpu_preview_invalidation(
            &first, &second
        ));
        assert!(display_output_contract_requires_renderer_rebuild(
            &first, &second
        ));
    }

    #[test]
    fn display_output_contract_surface_color_space_change_rebuilds_renderer() {
        let first = test_display_output_contract();
        let mut second = first.clone();
        second.surface_color.color_space = wgpu::SurfaceColorSpace::DisplayP3;

        assert!(display_output_contract_requires_gpu_preview_invalidation(
            &first, &second
        ));
        assert!(display_output_contract_requires_renderer_rebuild(
            &first, &second
        ));
    }

    struct CursorFocusWidget {
        id: WidgetId,
        accepts_text: bool,
        children: Vec<Box<dyn Widget>>,
    }

    impl CursorFocusWidget {
        fn new(accepts_text: bool) -> Self {
            Self {
                id: WidgetId::new(),
                accepts_text,
                children: Vec::new(),
            }
        }

        fn with_children(children: Vec<Box<dyn Widget>>) -> Self {
            Self { id: WidgetId::new(), accepts_text: false, children }
        }
    }

    impl Widget for CursorFocusWidget {
        fn id(&self) -> WidgetId {
            self.id
        }

        fn measure(&self, _constraint: LayoutConstraint) -> Size {
            Size::ZERO
        }

        fn layout(&mut self, _bounds: Rect) {}

        fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
            EventResult::Ignored
        }

        fn paint(&self, _ctx: &mut PaintContext) {}

        fn child_count(&self) -> usize {
            self.children.len()
        }

        fn child(&self, index: usize) -> Option<&dyn Widget> {
            self.children.get(index).map(|child| child.as_ref())
        }

        fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
            match self.children.get_mut(index) {
                Some(child) => Some(child.as_mut()),
                None => None,
            }
        }

        fn accepts_text_input(&self) -> bool {
            self.accepts_text
        }
    }

    #[test]
    fn default_log_filter_keeps_noisy_gpu_crates_at_warning() {
        assert!(DEFAULT_APP_UI_LOG_FILTER.contains("wgpu_core=warn"));
        assert!(DEFAULT_APP_UI_LOG_FILTER.contains("wgpu_hal=warn"));
        assert!(DEFAULT_APP_UI_LOG_FILTER.contains("naga=warn"));
    }

    #[test]
    fn focus_loss_is_deferred_while_desktop_eyedropper_is_active() {
        assert!(!should_route_focus_lost_to_ui(true));
        assert!(should_route_focus_lost_to_ui(false));
    }

    #[test]
    fn window_focus_loss_resets_tracked_modifiers() {
        let mut modifiers = Modifiers { ctrl: true, alt: true, shift: true, meta: true };

        reset_modifiers_on_window_focus_loss(&mut modifiers);

        assert_eq!(modifiers, Modifiers::none());
    }

    #[test]
    fn ignored_keyboard_input_never_exits_native_windows() {
        for role in [AppUiWindowRole::Startup, AppUiWindowRole::Workspace] {
            assert!(!should_exit_on_ignored_keyboard_input(
                role,
                &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
            ));
            assert!(!should_exit_on_ignored_keyboard_input(
                role,
                &winit::keyboard::Key::Character("q".into()),
            ));
        }
    }

    #[test]
    fn native_close_request_uses_app_shell_quit_action() {
        assert_eq!(native_close_request_action(), app_shell_quit_action());
    }

    #[test]
    fn quit_shell_command_skips_window_role_sync_and_redraw() {
        assert!(!shell_commands_should_sync_window_session(
            AppUiShellCommands { quit: true, ..AppUiShellCommands::default() }
        ));
        assert!(shell_commands_should_sync_window_session(
            AppUiShellCommands {
                toggle_fullscreen: true,
                ..AppUiShellCommands::default()
            }
        ));
    }

    #[test]
    fn native_file_hover_diagnostics_only_report_unhandled_routes() {
        assert_eq!(
            native_file_hover_diagnostic(EventResult::Ignored),
            Some(NativeFileDndDiagnostic::HoverUnhandled)
        );
        assert_eq!(native_file_hover_diagnostic(EventResult::Handled), None);
        assert_eq!(
            native_file_hover_cancelled_diagnostic(EventResult::Ignored),
            Some(NativeFileDndDiagnostic::HoverCancelUnhandled)
        );
        assert_eq!(
            native_file_hover_cancelled_diagnostic(EventResult::Handled),
            None
        );
    }

    #[test]
    fn native_file_drop_ignored_by_widgets_falls_back_to_media_import() {
        let paths = vec![
            PathBuf::from("E:/media/a.mov"),
            PathBuf::from("E:/media/b.wav"),
        ];

        assert_eq!(
            native_file_drop_handling(EventResult::Ignored, paths.clone()),
            NativeFileDropHandling {
                action: Some(Action::ImportMedia(paths)),
                diagnostic: Some(NativeFileDndDiagnostic::DropImportedAsMedia { file_count: 2 }),
            }
        );
    }

    #[test]
    fn native_file_drop_handled_by_widgets_does_not_fallback_import() {
        assert_eq!(
            native_file_drop_handling(EventResult::Handled, vec![PathBuf::from("E:/media/a.mov")]),
            NativeFileDropHandling { action: None, diagnostic: None }
        );
    }

    #[test]
    fn native_file_drop_empty_ignored_route_does_not_import() {
        assert_eq!(
            native_file_drop_handling(EventResult::Ignored, Vec::new()),
            NativeFileDropHandling {
                action: None,
                diagnostic: Some(NativeFileDndDiagnostic::DropIgnoredEmpty),
            }
        );
    }

    #[test]
    fn surface_lifecycle_ignores_zero_sized_windows() {
        for reason in [
            SurfaceLifecycleReason::Resize,
            SurfaceLifecycleReason::ScaleFactorChanged,
        ] {
            assert_eq!(
                surface_lifecycle_update(reason, (1280, 720), (0, 720)),
                SurfaceLifecycleUpdate {
                    reconfigure_surface: false,
                    relayout_root: false,
                    request_redraw: false,
                    bounds: None,
                }
            );
            assert_eq!(
                surface_lifecycle_update(reason, (1280, 720), (1280, 0)),
                SurfaceLifecycleUpdate {
                    reconfigure_surface: false,
                    relayout_root: false,
                    request_redraw: false,
                    bounds: None,
                }
            );
        }
    }

    #[test]
    fn surface_lifecycle_reconfigures_and_relayouts_on_real_resize() {
        assert_eq!(
            surface_lifecycle_update(SurfaceLifecycleReason::Resize, (1280, 720), (1600, 900)),
            SurfaceLifecycleUpdate {
                reconfigure_surface: true,
                relayout_root: true,
                request_redraw: true,
                bounds: Some(Rect::new(0.0, 0.0, 1600.0, 900.0)),
            }
        );
    }

    #[test]
    fn surface_lifecycle_skips_redundant_same_size_resize() {
        assert_eq!(
            surface_lifecycle_update(SurfaceLifecycleReason::Resize, (1280, 720), (1280, 720)),
            SurfaceLifecycleUpdate {
                reconfigure_surface: false,
                relayout_root: false,
                request_redraw: false,
                bounds: None,
            }
        );
    }

    #[test]
    fn surface_lifecycle_relayouts_same_size_dpi_change() {
        assert_eq!(
            surface_lifecycle_update(
                SurfaceLifecycleReason::ScaleFactorChanged,
                (1280, 720),
                (1280, 720),
            ),
            SurfaceLifecycleUpdate {
                reconfigure_surface: false,
                relayout_root: true,
                request_redraw: true,
                bounds: Some(Rect::new(0.0, 0.0, 1280.0, 720.0)),
            }
        );
    }

    #[test]
    fn surface_lifecycle_reconfigures_dpi_size_changes() {
        assert_eq!(
            surface_lifecycle_update(
                SurfaceLifecycleReason::ScaleFactorChanged,
                (1280, 720),
                (1920, 1080),
            ),
            SurfaceLifecycleUpdate {
                reconfigure_surface: true,
                relayout_root: true,
                request_redraw: true,
                bounds: Some(Rect::new(0.0, 0.0, 1920.0, 1080.0)),
            }
        );
    }

    #[test]
    fn focused_widget_accepts_text_input_finds_nested_text_owner() {
        let text_child = CursorFocusWidget::new(true);
        let text_id = text_child.id;
        let button_child = CursorFocusWidget::new(false);
        let button_id = button_child.id;
        let root = CursorFocusWidget::with_children(vec![
            Box::new(button_child),
            Box::new(CursorFocusWidget::with_children(vec![Box::new(text_child)])),
        ]);

        assert!(focused_widget_accepts_text_input(&root, Some(text_id)));
        assert!(!focused_widget_accepts_text_input(&root, Some(button_id)));
        assert!(!focused_widget_accepts_text_input(&root, None));
        assert!(!focused_widget_accepts_text_input(
            &root,
            Some(WidgetId::new())
        ));
    }

    #[test]
    fn rebuilding_global_shortcuts_applies_overrides_immediately() {
        use crate::app_ui::shortcuts::{AppUiShortcutBinding, AppUiShortcutKey};
        use mondrian_editor_state::Action;
        use mondrian_ui_core::shortcut::{ShortcutContext, ShortcutManager};

        let mut router = build_event_router(WidgetId::new(), &[]);
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::S,
                Modifiers::ctrl(),
                ShortcutContext::default(),
            ),
            Some(Action::SaveProject)
        );

        let overrides = vec![AppUiShortcutOverride {
            id: "file.save_project".to_owned(),
            binding: Some(AppUiShortcutBinding {
                key: AppUiShortcutKey::I,
                ctrl: true,
                alt: true,
                shift: false,
                meta: false,
            }),
        }];

        rebuild_global_shortcuts(&mut router, &overrides);

        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::S,
                Modifiers::ctrl(),
                ShortcutContext::default(),
            ),
            None
        );
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::I,
                Modifiers { ctrl: true, alt: true, shift: false, meta: false },
                ShortcutContext::default(),
            ),
            Some(Action::SaveProject)
        );
    }

    #[test]
    fn startup_window_chrome_is_fixed_and_undecorated() {
        let chrome = window_chrome_for_role(AppUiWindowRole::Startup);

        assert_eq!(chrome.title, "Mondrian");
        assert_eq!(chrome.width, STARTUP_WINDOW_WIDTH);
        assert_eq!(chrome.height, STARTUP_WINDOW_HEIGHT);
        assert!(chrome.transparent);
        assert!(!chrome.decorations);
        assert!(chrome.rounded_corners);
        assert!(!chrome.resizable);
        assert_eq!(
            chrome.min_size,
            Some((STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT))
        );
        assert_eq!(
            chrome.max_size,
            Some((STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT))
        );
    }

    #[test]
    fn workspace_window_chrome_is_resizable_product_workspace() {
        let chrome = window_chrome_for_role(AppUiWindowRole::Workspace);

        assert_eq!(chrome.title, "Mondrian");
        assert_eq!(chrome.width, WORKSPACE_WINDOW_WIDTH);
        assert_eq!(chrome.height, WORKSPACE_WINDOW_HEIGHT);
        assert!(!chrome.transparent);
        assert!(!chrome.decorations);
        assert!(chrome.rounded_corners);
        assert!(chrome.resizable);
        assert_eq!(
            chrome.min_size,
            Some((WORKSPACE_MIN_WIDTH, WORKSPACE_MIN_HEIGHT))
        );
        assert_eq!(chrome.max_size, None);
    }

    #[test]
    fn app_window_icon_decodes_embedded_favicon() {
        assert!(app_window_icon().is_some());
    }

    #[test]
    fn window_role_bounds_match_requested_chrome_size() {
        for role in [AppUiWindowRole::Startup, AppUiWindowRole::Workspace] {
            let chrome = window_chrome_for_role(role);
            let bounds = window_bounds_for_role(role);

            assert_eq!(bounds.x, 0.0);
            assert_eq!(bounds.y, 0.0);
            assert_eq!(bounds.width, chrome.width);
            assert_eq!(bounds.height, chrome.height);
        }
    }

    #[test]
    fn ui_modes_map_to_distinct_native_window_roles() {
        assert_eq!(
            window_role_for_mode(AppUiMode::Startup),
            AppUiWindowRole::Startup
        );
        assert_eq!(
            window_role_for_mode(AppUiMode::Workspace),
            AppUiWindowRole::Workspace
        );
    }

    #[test]
    fn workspace_window_requests_platform_rounded_corners() {
        assert_eq!(
            window_corner_preference_for_role(AppUiWindowRole::Startup),
            WindowCornerPreference::Round
        );
        assert_eq!(
            window_corner_preference_for_role(AppUiWindowRole::Workspace),
            WindowCornerPreference::Round
        );
    }

    #[cfg(feature = "validation")]
    #[test]
    fn rejected_surface_validation_returns_its_unconsumed_app_owner() {
        let run = run_app_ui_surface_device_reopen_validation_returning_state(
            AppState::new(),
            0,
            "invalid-timeout".to_owned(),
            Duration::ZERO,
        );

        assert!(run.result.is_err());
        let shutdown = run.app_state.shutdown_for_endurance(
            Instant::now()
                .checked_add(Duration::from_secs(5))
                .expect("test shutdown deadline"),
        );
        assert!(shutdown.all_resources_released(), "{shutdown:#?}");
    }

    #[cfg(feature = "validation")]
    #[test]
    fn surface_reopen_window_does_not_advance_settled_playback() {
        assert!(!validation_window_advances_playback(true));
        assert!(validation_window_advances_playback(false));
    }

    #[cfg(feature = "validation")]
    #[test]
    fn surface_validation_batch_rejects_unbounded_work_before_event_loop_creation() {
        let requests = (0..=MAXIMUM_SURFACE_REOPEN_VALIDATION_BATCH_CYCLES)
            .map(|cycle| AppUiSurfaceDeviceReopenValidationRequest {
                cycle_index: u32::try_from(cycle).expect("bounded test cycle"),
                operation_id: format!("surface.c{cycle}"),
                timeout: Duration::from_secs(1),
            })
            .collect();
        let error = run_app_ui_surface_device_reopen_validation_batch(AppState::new(), requests)
            .expect_err("unbounded batch must fail");

        assert_eq!(
            error.kind(),
            AppUiSurfaceDeviceReopenValidationFailureKind::TooManyOperations
        );
        assert!(error.to_string().contains("exceeds 24 operations"));
        assert!(error.completed_receipts().is_empty());
        assert!(error.window_shutdown().is_none());
        assert!(error.event_loop_shutdown().is_none());
        assert!(error.all_returned_authority_released(), "{error:#?}");
    }

    #[cfg(feature = "validation")]
    #[test]
    fn surface_validation_request_failures_do_not_construct_an_event_loop() {
        let cases = [
            (
                Vec::new(),
                AppUiSurfaceDeviceReopenValidationFailureKind::EmptyBatch,
            ),
            (
                vec![AppUiSurfaceDeviceReopenValidationRequest {
                    cycle_index: 1,
                    operation_id: "surface.c1".to_owned(),
                    timeout: Duration::ZERO,
                }],
                AppUiSurfaceDeviceReopenValidationFailureKind::ZeroTimeout,
            ),
            (
                vec![
                    AppUiSurfaceDeviceReopenValidationRequest {
                        cycle_index: 1,
                        operation_id: "surface.same".to_owned(),
                        timeout: Duration::from_secs(1),
                    },
                    AppUiSurfaceDeviceReopenValidationRequest {
                        cycle_index: 2,
                        operation_id: "surface.same".to_owned(),
                        timeout: Duration::from_secs(1),
                    },
                ],
                AppUiSurfaceDeviceReopenValidationFailureKind::InvalidOrReplayedIdentity,
            ),
            (
                vec![AppUiSurfaceDeviceReopenValidationRequest {
                    cycle_index: 1,
                    operation_id: "surface.c1".to_owned(),
                    timeout: Duration::MAX,
                }],
                AppUiSurfaceDeviceReopenValidationFailureKind::DeadlineOverflow,
            ),
        ];

        for (requests, expected_kind) in cases {
            let factory_called = std::cell::Cell::new(false);
            let error = run_app_ui_surface_device_reopen_validation_batch_with_factory(
                AppState::new(),
                requests,
                || {
                    factory_called.set(true);
                    unreachable!("invalid request must fail before EventLoop construction")
                },
            )
            .expect_err("invalid request must fail");

            assert_eq!(error.kind(), expected_kind);
            assert!(!factory_called.get());
            assert!(error.completed_receipts().is_empty());
            assert!(error.window_shutdown().is_none());
            assert!(error.event_loop_shutdown().is_none());
            assert!(error
                .window_shutdown_receipt()
                .expect("absent Window receipt should be valid")
                .is_none());
            assert!(error
                .event_loop_shutdown_receipt()
                .expect("absent EventLoop receipt should be valid")
                .is_none());
            assert!(error.all_returned_authority_released(), "{error:#?}");
        }
    }

    #[cfg(feature = "validation")]
    #[test]
    fn surface_validation_event_loop_construction_failure_returns_typed_app_ownership() {
        let request = AppUiSurfaceDeviceReopenValidationRequest {
            cycle_index: 7,
            operation_id: "surface.c7".to_owned(),
            timeout: Duration::from_secs(1),
        };
        let error = run_app_ui_surface_device_reopen_validation_batch_with_factory(
            AppState::new(),
            vec![request],
            || {
                Err(AppUiEventLoopConstructionFailure::synthetic(
                    AppUiEventLoopConstructionFailureKind::OperatingSystem,
                    "injected EventLoop construction failure",
                ))
            },
        )
        .expect_err("injected EventLoop construction must fail");

        assert_eq!(
            error.kind(),
            AppUiSurfaceDeviceReopenValidationFailureKind::EventLoopConstruction(
                AppUiEventLoopConstructionFailureKind::OperatingSystem
            )
        );
        assert!(error.to_string().contains("injected EventLoop construction failure"));
        assert!(error.completed_receipts().is_empty());
        assert_eq!(error.failed_cycle_index(), None);
        assert_eq!(error.failed_operation_id(), None);
        assert!(error.window_shutdown().is_none());
        assert!(error.event_loop_shutdown().is_none());
        assert!(error
            .window_shutdown_receipt()
            .expect("construction failure has no Window receipt")
            .is_none());
        assert!(error
            .event_loop_shutdown_receipt()
            .expect("construction failure has no EventLoop shutdown receipt")
            .is_none());
        assert!(error.all_returned_authority_released(), "{error:#?}");
    }
}
