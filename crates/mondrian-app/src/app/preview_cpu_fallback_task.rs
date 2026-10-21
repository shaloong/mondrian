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
    composite_resolved_preview_working, present_preview_working_with_signal_monitoring,
    PreviewCpuExecutionDurations, PreviewWorkingCompositeOutput,
};
use super::preview_execution::PreviewOutputKey;
use super::preview_raster_frame::{
    preview_raster_presentation_contract, preview_raster_resource_key, PreviewRasterFrame,
};
use super::preview_runtime::PreviewOwnedWorkerShutdown;
use super::preview_viewer_plan::ResolvedPreviewElement;
use super::preview_work_notification::PreviewWorkNotifier;

/// The selected consumer decides whether the working result crosses CPU display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewCpuOutputTarget {
    Raster,
    WorkingGpu,
}

/// Exact correlation for a single CPU execution and its GPU continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewCpuFallbackAttempt {
    pub(crate) generation: u64,
    pub(crate) epoch: PlaybackEpoch,
    pub(crate) semantic_key: PreviewOutputKey,
    pub(crate) output_key: PreviewOutputKey,
    pub(crate) intent: super::preview_execution::PreviewPlaybackIntent,
    pub(crate) target: PreviewCpuOutputTarget,
    pub(crate) candidate_id: Option<u64>,
    pub(crate) reusable: bool,
}

/// The existing one pending slot retains metadata, never a second pixel cache.
pub(crate) struct PreviewCpuFallbackPending {
    pub(crate) attempt: PreviewCpuFallbackAttempt,
    pub(crate) completed: bool,
    pub(crate) completed_resource: Option<mondrian_playback::ViewerFrameResourceLease>,
}

const REQUEST_CAPACITY: usize = 1;
const RESULT_CAPACITY: usize = 1;

pub(crate) struct PreviewCpuFallbackRequest {
    pub(crate) attempt: Option<PreviewCpuFallbackAttempt>,
    pub(crate) output_target: PreviewCpuOutputTarget,
    pub(crate) native_gui_work: Option<super::preview_native_gui::PreviewNativeGuiWorkBatch>,
    pub(crate) cpu_materialization_active_bytes: u64,
    pub(crate) cpu_working_set_grant: mondrian_renderer::TimelineCpuWorkingSetGrant,
    pub(crate) effect_session_config: mondrian_effects::EffectExecutionSessionConfig,
    pub(crate) cancellation: mondrian_core::ExecutionCancellationToken,
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
    // Pixel/source fields retire before this work charge.
    pub(crate) working_reservation: Option<mondrian_playback::ViewerFrameWorkLease>,
}

pub(crate) struct PreviewCpuFallbackReady {
    pub(crate) attempt: Option<PreviewCpuFallbackAttempt>,
    pub(crate) generation: u64,
    pub(crate) epoch: PlaybackEpoch,
    pub(crate) output_key: PreviewOutputKey,
    pub(crate) frame: super::preview_frame_store::PreviewViewerCpuFrame,
    pub(crate) execution: PreviewCpuFallbackExecutionEvidence,
    pub(crate) render_cache_frame: Option<TimelineRenderCacheFrame>,
    // Payload and cache-frame owners retire before their shared charge.
    pub(crate) working_resource: Option<mondrian_playback::ViewerFrameResourceLease>,
}

pub(crate) struct PreviewCpuFallbackExecutionEvidence {
    pub(crate) composite_diagnostics: TimelineCompositeDiagnostics,
    pub(crate) input_color_diagnostics: Vec<RenderColorTransformDiagnostics>,
    pub(crate) input_color_stage_diagnostics: RenderColorStageDiagnostics,
    pub(crate) color_diagnostics: Option<RenderColorTransformDiagnostics>,
    pub(crate) monitor_color_diagnostics: Option<RenderColorTransformDiagnostics>,
    pub(crate) color_stage_diagnostics: RenderColorStageDiagnostics,
    pub(crate) execution_durations: PreviewCpuExecutionDurations,
    pub(crate) processor_feedback: mondrian_effects::EffectProcessorFeedbackBatch,
}

pub(crate) struct PreviewCpuFallbackFailed {
    pub(crate) attempt: Option<PreviewCpuFallbackAttempt>,
    pub(crate) generation: u64,
    pub(crate) epoch: PlaybackEpoch,
    pub(crate) output_key: PreviewOutputKey,
    pub(crate) reason: String,
    pub(crate) blocked: Option<Arc<mondrian_effects::EffectProcessorFeedbackReceipt>>,
}

#[derive(Debug, thiserror::Error)]
enum PreviewCpuFallbackFailure {
    #[error(transparent)]
    Execution(#[from] super::preview_cpu_execution::PreviewCpuExecutionError),
    #[error("{0}")]
    Adapter(String),
}
impl PreviewCpuFallbackFailure {
    fn processor_blocked(&self) -> Option<&Arc<mondrian_effects::EffectProcessorFeedbackReceipt>> {
        match self {
            Self::Execution(error) => error.processor_blocked(),
            Self::Adapter(_) => None,
        }
    }
}

pub(crate) enum PreviewCpuFallbackResult {
    Progress(super::preview_processor_feedback::PreviewProcessorProgressUpdate),
    NativeQuestion(super::preview_native_gui::NativeVisualQuestionPresentation),
    NativeGuiAction(super::preview_native_gui::NativeVisualGuiActionDelivery),
    Ready(Box<PreviewCpuFallbackReady>),
    Failed(Box<PreviewCpuFallbackFailed>),
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
            PreviewOwnedWorkerShutdown::Panicked
            | PreviewOwnedWorkerShutdown::PanickedPayloadAbandoned => {
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
    loop {
        let mut request = match requests.recv_timeout(std::time::Duration::from_millis(20)) {
            Ok(request) => request,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                scratch.reconcile_processor_guis();
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        // This same owner also closes an idle disappeared GUI. The timeout
        // is a lifecycle boundary, not another reader, queue or DSP request.
        scratch.reconcile_processor_guis();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let scope = super::preview_processor_feedback::PreviewProcessorFeedbackScope::scalar(
                &request.output_key,
                request.generation,
                request.epoch,
            );
            let action_results = results.clone();
            let action_notifier = work_notifier.clone();
            let question_results = results.clone();
            let question_notifier = work_notifier.clone();
            let question_service = request.native_gui_work.take().and_then(|work|
                work.service(RESULT_CAPACITY, move |presentation| {
                    if presentation.scope() != scope { presentation.dismiss(); return false; }
                    if question_results.try_send(PreviewCpuFallbackResult::NativeQuestion(presentation)).is_ok() {
                        question_notifier.result_became_pollable(); true
                    } else { false }
                } , move |action| {
                    if action_results.try_send(PreviewCpuFallbackResult::NativeGuiAction(action)).is_ok() {
                        action_notifier.result_became_pollable(); true
                    } else { false }
                }));
            let progress_results = results.clone();
            let progress_notifier = work_notifier.clone();
            let progress_publication = move |receipt: &mondrian_effects::EffectProcessorProgressReceipt| {
                    let update =
                        super::preview_processor_feedback::PreviewProcessorProgressUpdate {
                            scope,
                            receipt: Some(receipt.clone()),
                        };
                    if progress_results.try_send(PreviewCpuFallbackResult::Progress(update)).is_ok()
                    {
                        progress_notifier.result_became_pollable();
                    }
                };
            let sink = match question_service {
                Some(question_service) => super::preview_processor_feedback::PreviewProcessorProgressPublisher::with_question_service(
                    RESULT_CAPACITY, question_service, progress_publication),
                None => super::preview_processor_feedback::PreviewProcessorProgressPublisher::new_observer(RESULT_CAPACITY, progress_publication),
            };
            execute_cpu_fallback_observed(
                &request,
                &mut scratch,
                mondrian_effects::EffectProcessorObservation { sink: Some(sink), deadline: None },
            )
        }))
        .unwrap_or_else(|_| {
            scratch = TimelineCompositeScratch::default();
            Err(PreviewCpuFallbackFailure::Adapter(
                "Viewer CPU fallback worker panicked".to_owned(),
            ))
        });
        if request.cancellation.is_canceled() {
            continue;
        }
        let result = match result {
            Ok((frame, execution, render_cache_frame)) => {
                let resource_completion = match &frame {
                    super::preview_frame_store::PreviewViewerCpuFrame::Raster(_) => Ok(None),
                    super::preview_frame_store::PreviewViewerCpuFrame::Working(frame) => {
                        let actual_bytes = frame
                            .rgba_f32()
                            .data
                            .capacity()
                            .checked_mul(std::mem::size_of::<[f32; 4]>());
                        match request.working_reservation.take() {
                            Some(work) => match actual_bytes {
                                Some(bytes) => work.finish(bytes).map(Some).map_err(Some),
                                None => Err(Some(work)),
                            },
                            None => Err(None),
                        }
                    }
                };
                let working_resource = match resource_completion {
                    Ok(resource) => resource,
                    Err(refused_work) => {
                        // No pixel owner may cross a potentially blocking failure send.
                        // Retire the allocation before releasing its admitted work charge.
                        drop(frame);
                        drop(render_cache_frame);
                        drop(execution);
                        drop(refused_work);
                        if results.send(PreviewCpuFallbackResult::Failed(Box::new(PreviewCpuFallbackFailed {
                            attempt: request.attempt, generation: request.generation, epoch: request.epoch,
                            output_key: request.output_key, blocked: None,
                            reason: "Viewer working capacity exceeds its pre-allocation reservation".to_owned(),
                        }))).is_err() { break; }
                        work_notifier.result_became_pollable();
                        continue;
                    }
                };
                PreviewCpuFallbackResult::Ready(Box::new(PreviewCpuFallbackReady {
                    attempt: request.attempt,
                    working_resource,
                    generation: request.generation,
                    epoch: request.epoch,
                    output_key: request.output_key,
                    frame,
                    execution,
                    render_cache_frame,
                }))
            }
            Err(reason) => PreviewCpuFallbackResult::Failed(Box::new(PreviewCpuFallbackFailed {
                attempt: request.attempt,
                generation: request.generation,
                epoch: request.epoch,
                output_key: request.output_key,
                blocked: reason.processor_blocked().map(Arc::clone),
                reason: reason.to_string(),
            })),
        };
        if results.send(result).is_err() {
            break;
        }
        work_notifier.result_became_pollable();
    }
}

#[cfg(test)]
fn execute_cpu_fallback(
    request: &PreviewCpuFallbackRequest,
    scratch: &mut TimelineCompositeScratch,
) -> Result<
    (
        super::preview_frame_store::PreviewViewerCpuFrame,
        PreviewCpuFallbackExecutionEvidence,
        Option<TimelineRenderCacheFrame>,
    ),
    PreviewCpuFallbackFailure,
> {
    execute_cpu_fallback_observed(
        request,
        scratch,
        mondrian_effects::EffectProcessorObservation::default(),
    )
}

fn execute_cpu_fallback_observed(
    request: &PreviewCpuFallbackRequest,
    scratch: &mut TimelineCompositeScratch,
    observation: mondrian_effects::EffectProcessorObservation,
) -> Result<
    (
        super::preview_frame_store::PreviewViewerCpuFrame,
        PreviewCpuFallbackExecutionEvidence,
        Option<TimelineRenderCacheFrame>,
    ),
    PreviewCpuFallbackFailure,
> {
    let mut checkpoint = || {
        request
            .cancellation
            .is_canceled()
            .then_some(mondrian_effects::EffectProcessorStopReason::Canceled)
    };
    let mut control = mondrian_effects::EffectProcessorControl::new(&mut checkpoint);
    control.bind_observation(observation);
    control
        .check()
        .map_err(|error| PreviewCpuFallbackFailure::Adapter(error.to_string()))?;
    let raster_contract = match request.output_target {
        PreviewCpuOutputTarget::Raster => Some(
            preview_raster_presentation_contract(&request.color_context)
                .map_err(|error| PreviewCpuFallbackFailure::Adapter(error.to_string()))?,
        ),
        PreviewCpuOutputTarget::WorkingGpu => None,
    };
    scratch.reconfigure_cpu_working_set(request.cpu_working_set_grant);
    scratch.reconfigure_effect_execution(request.effect_session_config);
    scratch.bind_effect_execution_generation(request.generation);
    scratch
        .admit_cpu_active_working_set(
            request.cpu_materialization_active_bytes,
            mondrian_renderer::TimelineCpuCompositePrecision::Float32,
        )
        .map_err(|error| PreviewCpuFallbackFailure::Adapter(error.to_string()))?;
    let working = match &request.cached_working {
        Some(frame) => PreviewWorkingCompositeOutput {
            frame: frame.clone(),
            composite_diagnostics: TimelineCompositeDiagnostics::default(),
            input_color_diagnostics: Vec::new(),
            input_color_stage_diagnostics: RenderColorStageDiagnostics::default(),
            execution_durations: PreviewCpuExecutionDurations::default(),
        },
        None => composite_resolved_preview_working(
            request.width,
            request.height,
            &request.elements,
            &request.color_context,
            scratch,
            &mut control,
        )?,
    };
    control
        .check()
        .map_err(|error| PreviewCpuFallbackFailure::Adapter(error.to_string()))?;
    let (
        frame,
        working_frame,
        composite_diagnostics,
        input_color_diagnostics,
        input_color_stage_diagnostics,
        color_diagnostics,
        monitor_color_diagnostics,
        color_stage_diagnostics,
        execution_durations,
    ) = match request.output_target {
        PreviewCpuOutputTarget::WorkingGpu => {
            let PreviewWorkingCompositeOutput {
                frame,
                composite_diagnostics,
                input_color_diagnostics,
                input_color_stage_diagnostics,
                execution_durations,
            } = working;
            // The immutable Arc is shared; no raster conversion or pixel copy.
            (
                super::preview_frame_store::PreviewViewerCpuFrame::Working(frame.clone()),
                frame,
                composite_diagnostics,
                input_color_diagnostics,
                input_color_stage_diagnostics,
                None,
                None,
                RenderColorStageDiagnostics::default(),
                execution_durations,
            )
        }
        PreviewCpuOutputTarget::Raster => {
            let Some(contract) = raster_contract else {
                return Err(PreviewCpuFallbackFailure::Adapter(
                    "Raster output contract was not admitted".to_owned(),
                ));
            };
            let execution = present_preview_working_with_signal_monitoring(
                working,
                &request.color_context,
                request.monitoring_tap,
                request.monitoring_settings,
                scratch,
            )?;
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
            let raster = PreviewRasterFrame::new(
                preview_raster_resource_key(&request.output_key),
                request.width,
                request.height,
                contract.color_space,
                rgba,
            )
            .map_err(|error| PreviewCpuFallbackFailure::Adapter(error.to_string()))?;
            (
                super::preview_frame_store::PreviewViewerCpuFrame::Raster(raster),
                working_frame,
                composite_diagnostics,
                input_color_diagnostics,
                input_color_stage_diagnostics,
                Some(color_diagnostics),
                monitor_color_diagnostics,
                color_stage_diagnostics,
                execution_durations,
            )
        }
    };
    control
        .check()
        .map_err(|error| PreviewCpuFallbackFailure::Adapter(error.to_string()))?;
    let render_cache_frame = request
        .render_cache_identity
        .filter(|_| {
            request.cached_working.is_none()
                && request.output_target == PreviewCpuOutputTarget::Raster
        })
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
            processor_feedback: control
                .take_feedback()
                .map_err(|error| PreviewCpuFallbackFailure::Adapter(error.to_string()))?,
        },
        render_cache_frame,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn working_request() -> PreviewCpuFallbackRequest {
        let sequence = mondrian_timeline::Sequence::new("CPU working output");
        PreviewCpuFallbackRequest {
            attempt: None,
            working_reservation: None,
            output_target: PreviewCpuOutputTarget::WorkingGpu,
            native_gui_work: None,
            cancellation: mondrian_core::ExecutionCancellationToken::new(),
            cpu_materialization_active_bytes: 16 * 16,
            cpu_working_set_grant: mondrian_renderer::TimelineCpuWorkingSetGrant {
                max_active_bytes: 1024 * 1024,
                max_retained_scratch_bytes: 1024 * 1024,
            },
            effect_session_config: mondrian_effects::EffectExecutionSessionConfig::uncached(
                1024 * 1024,
            ),
            generation: 1,
            epoch: mondrian_playback::PlaybackEngine::new(sequence.time_base(), Default::default())
                .expect("engine")
                .snapshot()
                .epoch,
            output_key: PreviewOutputKey::new(
                sequence.id,
                1,
                1,
                super::super::preview_execution::PreviewSemanticIdentityBuilder::new(
                    b"working-output",
                )
                .finish_identity(),
            ),
            width: 1,
            height: 1,
            elements: Arc::from([]),
            color_context: sequence
                .settings
                .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
                .expect("context"),
            render_cache_identity: None,
            cached_working: None,
            monitoring_tap: Default::default(),
            monitoring_settings: Default::default(),
        }
    }

    #[test]
    fn cpu_working_completion_preserves_hdr_alpha_and_skips_display_conversion() {
        let mut request = working_request();
        let original = CpuColorFrame::working(mondrian_core::WorkingRgbaF32Frame {
            width: 1,
            height: 1,
            data: vec![[-0.5, 3.0, 0.8, 0.5]],
            alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
            color_space: request.color_context.working_color_space(),
        });
        request.cached_working = Some(original.clone());
        let mut scratch = TimelineCompositeScratch::default();
        let (frame, evidence, cache) =
            execute_cpu_fallback(&request, &mut scratch).expect("working completion");
        let super::super::preview_frame_store::PreviewViewerCpuFrame::Working(frame) = frame else {
            panic!("working output was converted to a display raster");
        };
        assert!(frame.shares_storage_with(&original));
        assert_eq!(frame.descriptor(), original.descriptor());
        assert_eq!(frame.rgba_f32().data, [[-0.5, 3.0, 0.8, 0.5]]);
        assert_eq!(evidence.color_diagnostics, None);
        assert_eq!(
            evidence.color_stage_diagnostics,
            RenderColorStageDiagnostics::default()
        );
        assert_eq!(evidence.execution_durations.cpu_output_boundary_us, 0);
        assert!(cache.is_none());
        request.cancellation.cancel();
        assert!(execute_cpu_fallback(&request, &mut scratch).is_err());
    }

    #[test]
    fn cpu_working_adjustment_uses_the_complete_shared_compositor() {
        let mut request = working_request();
        let identity = mondrian_effects::compile_reference_render_graph(
            mondrian_effects::EffectGraphBuilderState::new().finish(),
        )
        .expect("identity");
        let mut graph = mondrian_effects::EffectGraphBuilderState::new();
        graph.append_unary(mondrian_effects::EffectRenderOp::GaussianBlur { radius: 1.0 });
        let blur = mondrian_effects::compile_reference_render_graph(graph.finish())
            .expect("CPU adjustment");
        request.elements = Arc::from([
            ResolvedPreviewElement::SolidColor(mondrian_renderer::TimelineSolidColorLayer {
                color: mondrian_core::types::Color { r: -0.5, g: 3.0, b: 0.8, a: 0.5 },
                opacity: 1.0,
                blend_mode: mondrian_core::types::BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: identity,
                frame_seed: 0,
            }),
            ResolvedPreviewElement::Adjustment(mondrian_renderer::TimelineAdjustmentLayer {
                effect_graph: blur,
                opacity: 0.4,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Mix,
                frame_seed: 0,
            }),
        ]);
        let (frame, evidence, _) =
            execute_cpu_fallback(&request, &mut TimelineCompositeScratch::default())
                .expect("whole CPU composition");
        let super::super::preview_frame_store::PreviewViewerCpuFrame::Working(frame) = frame else {
            panic!("CPU adjustment was displayed through a fixed raster");
        };
        assert_eq!(frame.rgba_f32().data.len(), 1);
        assert_eq!(
            frame.descriptor().alpha,
            mondrian_core::ColorFrameAlpha::StraightCoverage
        );
        // A normalized blur of one constant pixel changes neither its associated
        // RGB nor coverage. Blending that unchanged adjustment also preserves it.
        let coverage = 0.5_f32;
        let associated = [-0.5_f32, 3.0, 0.8].map(|channel| channel * coverage);
        let expected = [
            associated[0] / coverage,
            associated[1] / coverage,
            associated[2] / coverage,
            coverage,
        ];
        for (actual, expected) in frame.rgba_f32().data[0].into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() < 1e-6,
                "actual={actual} expected={expected}"
            );
        }
        assert!(evidence.composite_diagnostics.float_linear_composites > 0);
        assert_eq!(evidence.composite_diagnostics.legacy_rgba8_composites, 0);
        assert_eq!(evidence.execution_durations.cpu_output_boundary_us, 0);
    }

    #[test]
    fn cpu_materialization_worker_installs_owner_grant_before_allocating() {
        let sequence = mondrian_timeline::Sequence::new("CPU root admission");
        let grant = mondrian_renderer::TimelineCpuWorkingSetGrant {
            max_active_bytes: 512 * 1024 * 1024,
            max_retained_scratch_bytes: 0,
        };
        let mut request = PreviewCpuFallbackRequest {
            attempt: None,
            working_reservation: None,
            output_target: PreviewCpuOutputTarget::Raster,
            native_gui_work: None,
            cancellation: mondrian_core::ExecutionCancellationToken::new(),
            cpu_materialization_active_bytes: 663_552_000,
            cpu_working_set_grant: grant,
            effect_session_config: mondrian_effects::EffectExecutionSessionConfig::uncached(
                128 * 1024 * 1024,
            ),
            generation: 1,
            epoch: mondrian_playback::PlaybackEngine::new(sequence.time_base(), Default::default())
                .expect("engine")
                .snapshot()
                .epoch,
            output_key: PreviewOutputKey::new(
                sequence.id,
                3840,
                2160,
                super::super::preview_execution::PreviewSemanticIdentityBuilder::new(
                    b"cpu-grant-test",
                )
                .finish_identity(),
            ),
            width: 3840,
            height: 2160,
            elements: Arc::from([]),
            color_context: sequence
                .settings
                .root_program_color_context(&mondrian_core::ProjectColorEnvironment::default())
                .expect("context"),
            render_cache_identity: None,
            cached_working: None,
            monitoring_tap: Default::default(),
            monitoring_settings: Default::default(),
        };
        let mut scratch = TimelineCompositeScratch::default();
        let initial_grant = scratch.cpu_working_set_diagnostics().grant;
        request.cancellation.cancel();
        assert!(execute_cpu_fallback(&request, &mut scratch).is_err());
        assert_eq!(scratch.cpu_working_set_diagnostics().grant, initial_grant);
        assert_eq!(
            scratch.cpu_working_set_diagnostics().retained_scratch_bytes,
            0
        );
        request.cancellation = mondrian_core::ExecutionCancellationToken::new();

        let error = match execute_cpu_fallback(&request, &mut scratch) {
            Err(error) => error,
            Ok(_) => panic!("unfunded CPU root executed"),
        };
        assert!(
            error.to_string().contains("663552000") && error.to_string().contains("536870912"),
            "{error}"
        );
        assert_eq!(scratch.cpu_working_set_diagnostics().grant, grant);
        assert_eq!(
            scratch.effect_execution_diagnostics().max_working_bytes,
            request.effect_session_config.max_working_bytes
        );
        assert_eq!(scratch.effect_execution_diagnostics().max_cache_bytes, 0);
        assert_eq!(
            scratch.effect_execution_diagnostics().generation,
            Some(request.generation)
        );
        assert_eq!(
            scratch.cpu_working_set_diagnostics().retained_scratch_bytes,
            0
        );
    }

    #[test]
    fn cpu_raster_contract_refuses_before_processor_and_scratch_mutation() {
        use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
        static NEXT_TYPE: AtomicU64 = AtomicU64::new(1);
        let effect_type = mondrian_effects::EffectType::External(format!(
            "test.preview.raster-preflight.{}",
            NEXT_TYPE.fetch_add(1, Ordering::Relaxed)
        ));
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = Arc::clone(&calls);
        mondrian_effects::register_effect_definition(
            mondrian_effects::EffectDefinition::new(
                effect_type.key(),
                "Raster preflight",
                Default::default(),
                mondrian_effects::EffectColorDomainContract::SCENE_LINEAR,
            )
            .with_execution_contract(mondrian_effects::EffectExecutionContract {
                execution_modes: mondrian_effects::EffectExecutionModes::CPU_F32,
                determinism: mondrian_effects::EffectDeterminism::Nondeterministic,
                state_model: mondrian_effects::EffectStateModel::Stateless,
                temporal_input: mondrian_effects::EffectTemporalInputExtent::CURRENT_FRAME,
                roi_propagation: mondrian_effects::EffectRoiPropagation::UnknownRequiresFullFrame,
                resource_lifetime: mondrian_effects::EffectResourceLifetime::Frame,
                topology: mondrian_effects::EffectGraphTopology::LinearChain,
            })
            .with_custom_float_render_backend(
                Arc::new(|_, _| Ok(Some(serde_json::json!({})))),
                None,
                mondrian_effects::EffectCachePolicy::Uncacheable,
                Arc::new(move |_, _, _| {
                    observed_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }),
            ),
        )
        .expect("actual custom processor");
        let mut request = working_request();
        let sequence = mondrian_timeline::Sequence::new("nested working-only output");
        request.color_context = sequence
            .settings
            .nested_render_color_context(
                &request.color_context,
                mondrian_timeline::sequence::NestedColorProcessing::ForceParentWorkingSpace,
            )
            .expect("actual nested context");
        assert!(preview_raster_presentation_contract(&request.color_context).is_err());
        let graph = mondrian_effects::PreparedEffectProgram::prepare(
            &[mondrian_effects::EffectNode::new(effect_type)],
            &[],
            request.color_context.working_color_space(),
        )
        .expect("actual prepared effect")
        .evaluate(mondrian_core::TimelineTime::ZERO)
        .expect("actual graph");
        request.output_target = PreviewCpuOutputTarget::Raster;
        request.elements = Arc::from([ResolvedPreviewElement::Adjustment(
            mondrian_renderer::TimelineAdjustmentLayer {
                effect_graph: graph,
                opacity: 1.0,
                blend_policy: mondrian_effects::EffectPassBlendPolicy::Mix,
                frame_seed: 0,
            },
        )]);
        let mut scratch = TimelineCompositeScratch::default();
        let original = scratch.cpu_working_set_diagnostics();
        assert!(execute_cpu_fallback(&request, &mut scratch).is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "unsupported raster must not enter a processor"
        );
        assert_eq!(
            scratch.cpu_working_set_diagnostics(),
            original,
            "preflight refusal must not mutate scratch admission"
        );
    }
}
