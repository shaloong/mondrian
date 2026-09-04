//! Bounded background CPU execution for Viewer GPU fallback.
//!
//! The Window thread may request fallback, but never performs full-frame CPU
//! composition itself. This owner accepts at most one queued request behind
//! the executing request and publishes immutable raster results back to the
//! Preview Runtime. Generation/epoch authority remains with that Runtime.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use mondrian_playback::PlaybackEpoch;
use mondrian_render_cache::{TimelineRenderCacheFrame, TimelineRenderCacheIdentity};
use mondrian_renderer::{
    color::RenderColorStageDiagnostics, CpuColorFrame, RenderColorTransformDiagnostics,
    TimelineCompositeDiagnostics, TimelineCompositeScratch,
};
use mondrian_timeline::sequence::ProgramColorContext;

use super::preview_cpu_execution::{
    composite_resolved_preview_with_signal_monitoring,
    present_preview_working_with_signal_monitoring, PreviewCpuExecutionDurations,
    PreviewWorkingCompositeOutput,
};
use super::preview_execution::PreviewOutputKey;
use super::preview_raster_frame::{
    preview_raster_presentation_contract, preview_raster_resource_key, PreviewRasterFrame,
};
use super::preview_runtime::PreviewOwnedWorkerShutdown;
use super::preview_viewer_plan::ResolvedPreviewElement;
use super::preview_work_notification::PreviewWorkNotifier;

const REQUEST_CAPACITY: usize = 1;
const RESULT_CAPACITY: usize = 1;

pub(crate) struct PreviewCpuFallbackRequest {
    pub(crate) generation: u64,
    pub(crate) epoch: PlaybackEpoch,
    pub(crate) output_key: PreviewOutputKey,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) elements: Arc<[ResolvedPreviewElement]>,
    pub(crate) color_context: ProgramColorContext,
    pub(crate) render_cache_identity: Option<TimelineRenderCacheIdentity>,
    pub(crate) cached_working: Option<CpuColorFrame>,
    pub(crate) monitoring_tap: mondrian_core::ProgramScopesTap,
    pub(crate) monitoring_settings: mondrian_core::SignalMonitoringSettings,
}

pub(crate) struct PreviewCpuFallbackReady {
    pub(crate) generation: u64,
    pub(crate) epoch: PlaybackEpoch,
    pub(crate) output_key: PreviewOutputKey,
    pub(crate) frame: PreviewRasterFrame,
    pub(crate) execution: PreviewCpuFallbackExecutionEvidence,
    pub(crate) render_cache_frame: Option<TimelineRenderCacheFrame>,
}

pub(crate) struct PreviewCpuFallbackExecutionEvidence {
    pub(crate) composite_diagnostics: TimelineCompositeDiagnostics,
    pub(crate) input_color_diagnostics: Vec<RenderColorTransformDiagnostics>,
    pub(crate) input_color_stage_diagnostics: RenderColorStageDiagnostics,
    pub(crate) color_diagnostics: RenderColorTransformDiagnostics,
    pub(crate) monitor_color_diagnostics: Option<RenderColorTransformDiagnostics>,
    pub(crate) color_stage_diagnostics: RenderColorStageDiagnostics,
    pub(crate) execution_durations: PreviewCpuExecutionDurations,
}

pub(crate) struct PreviewCpuFallbackFailed {
    pub(crate) generation: u64,
    pub(crate) epoch: PlaybackEpoch,
    pub(crate) output_key: PreviewOutputKey,
    pub(crate) reason: String,
}

pub(crate) enum PreviewCpuFallbackResult {
    Ready(Box<PreviewCpuFallbackReady>),
    Failed(PreviewCpuFallbackFailed),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewCpuFallbackSubmission {
    Scheduled,
    Busy,
    Disconnected,
}

pub(crate) struct PreviewCpuFallbackTask {
    requests: Option<mpsc::SyncSender<PreviewCpuFallbackRequest>>,
    results: Option<mpsc::Receiver<PreviewCpuFallbackResult>>,
    worker: Option<JoinHandle<()>>,
}

impl PreviewCpuFallbackTask {
    pub(crate) fn new(work_notifier: PreviewWorkNotifier) -> Result<Self, std::io::Error> {
        let (request_tx, request_rx) = mpsc::sync_channel(REQUEST_CAPACITY);
        let (result_tx, result_rx) = mpsc::sync_channel(RESULT_CAPACITY);
        let worker = thread::Builder::new()
            .name("mondrian-preview-cpu-fallback".to_owned())
            .spawn(move || cpu_fallback_worker(request_rx, result_tx, work_notifier))?;
        Ok(Self {
            requests: Some(request_tx),
            results: Some(result_rx),
            worker: Some(worker),
        })
    }

    pub(crate) fn submit(
        &self,
        request: PreviewCpuFallbackRequest,
    ) -> PreviewCpuFallbackSubmission {
        let Some(requests) = self.requests.as_ref() else {
            return PreviewCpuFallbackSubmission::Disconnected;
        };
        match requests.try_send(request) {
            Ok(()) => PreviewCpuFallbackSubmission::Scheduled,
            Err(mpsc::TrySendError::Full(_)) => PreviewCpuFallbackSubmission::Busy,
            Err(mpsc::TrySendError::Disconnected(_)) => PreviewCpuFallbackSubmission::Disconnected,
        }
    }

    pub(crate) fn try_poll(&self) -> Option<PreviewCpuFallbackResult> {
        self.results.as_ref()?.try_recv().ok()
    }

    pub(crate) fn shutdown_and_wait(mut self) -> PreviewOwnedWorkerShutdown {
        self.stop_worker()
    }

    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn shutdown_until(mut self, deadline: Instant) -> PreviewOwnedWorkerShutdown {
        self.stop_worker_until(deadline)
    }

    pub(crate) fn begin_shutdown(&mut self) {
        self.requests.take();
        self.results.take();
    }

    fn stop_worker(&mut self) -> PreviewOwnedWorkerShutdown {
        self.begin_shutdown();
        self.worker.take().map_or(
            PreviewOwnedWorkerShutdown::NotStarted,
            PreviewOwnedWorkerShutdown::join,
        )
    }

    fn stop_worker_until(&mut self, deadline: Instant) -> PreviewOwnedWorkerShutdown {
        self.begin_shutdown();
        self.worker.take().map_or(PreviewOwnedWorkerShutdown::NotStarted, |worker| {
            PreviewOwnedWorkerShutdown::join_until(worker, deadline)
        })
    }
}

impl Drop for PreviewCpuFallbackTask {
    fn drop(&mut self) {
        match self.stop_worker_until(Instant::now()) {
            PreviewOwnedWorkerShutdown::Panicked => {
                tracing::warn!("Preview CPU fallback worker panicked during shutdown");
            }
            PreviewOwnedWorkerShutdown::CurrentThreadSkipped => {
                tracing::warn!("Preview CPU fallback shutdown detached its current worker");
            }
            PreviewOwnedWorkerShutdown::TimedOutDetached => {
                tracing::warn!("Preview CPU fallback Drop detached its active worker");
            }
            PreviewOwnedWorkerShutdown::NotStarted | PreviewOwnedWorkerShutdown::Terminated => {}
        }
    }
}

fn cpu_fallback_worker(
    requests: mpsc::Receiver<PreviewCpuFallbackRequest>,
    results: mpsc::SyncSender<PreviewCpuFallbackResult>,
    work_notifier: PreviewWorkNotifier,
) {
    let _exit_notification = work_notifier.worker_exit_notification();
    let mut scratch = TimelineCompositeScratch::default();
    while let Ok(request) = requests.recv() {
        let result = catch_unwind(AssertUnwindSafe(|| {
            execute_cpu_fallback(&request, &mut scratch)
        }))
        .unwrap_or_else(|_| {
            scratch = TimelineCompositeScratch::default();
            Err("Viewer CPU fallback worker panicked".to_owned())
        });
        let result = match result {
            Ok((frame, execution, render_cache_frame)) => {
                PreviewCpuFallbackResult::Ready(Box::new(PreviewCpuFallbackReady {
                    generation: request.generation,
                    epoch: request.epoch,
                    output_key: request.output_key,
                    frame,
                    execution,
                    render_cache_frame,
                }))
            }
            Err(reason) => PreviewCpuFallbackResult::Failed(PreviewCpuFallbackFailed {
                generation: request.generation,
                epoch: request.epoch,
                output_key: request.output_key,
                reason,
            }),
        };
        if results.send(result).is_err() {
            break;
        }
        work_notifier.result_became_pollable();
    }
}

fn execute_cpu_fallback(
    request: &PreviewCpuFallbackRequest,
    scratch: &mut TimelineCompositeScratch,
) -> Result<
    (
        PreviewRasterFrame,
        PreviewCpuFallbackExecutionEvidence,
        Option<TimelineRenderCacheFrame>,
    ),
    String,
> {
    let contract = preview_raster_presentation_contract(&request.color_context)
        .map_err(|error| error.to_string())?;
    let execution = match &request.cached_working {
        Some(frame) => present_preview_working_with_signal_monitoring(
            PreviewWorkingCompositeOutput {
                frame: frame.clone(),
                composite_diagnostics: TimelineCompositeDiagnostics::default(),
                input_color_diagnostics: Vec::new(),
                input_color_stage_diagnostics: RenderColorStageDiagnostics::default(),
                execution_durations: PreviewCpuExecutionDurations::default(),
            },
            &request.color_context,
            request.monitoring_tap,
            request.monitoring_settings,
            scratch,
        ),
        None => composite_resolved_preview_with_signal_monitoring(
            request.width,
            request.height,
            &request.elements,
            &request.color_context,
            request.monitoring_tap,
            request.monitoring_settings,
            scratch,
        ),
    }
    .map_err(|error| error.to_string())?;
    let super::preview_cpu_execution::PreviewCompositeOutput {
        rgba,
        working_frame,
        composite_diagnostics,
        input_color_diagnostics,
        input_color_stage_diagnostics,
        color_diagnostics,
        monitor_color_diagnostics,
        color_stage_diagnostics,
        execution_durations,
    } = execution;
    let frame = PreviewRasterFrame::new(
        preview_raster_resource_key(&request.output_key),
        request.width,
        request.height,
        contract.color_space,
        rgba,
    )
    .map_err(|error| error.to_string())?;
    let render_cache_frame = request
        .render_cache_identity
        .filter(|_| request.cached_working.is_none())
        .and_then(|identity| {
            TimelineRenderCacheFrame::new(identity, working_frame.into_rgba_f32()).ok()
        });
    Ok((
        frame,
        PreviewCpuFallbackExecutionEvidence {
            composite_diagnostics,
            input_color_diagnostics,
            input_color_stage_diagnostics,
            color_diagnostics,
            monitor_color_diagnostics,
            color_stage_diagnostics,
            execution_durations,
        },
        render_cache_frame,
    ))
}
