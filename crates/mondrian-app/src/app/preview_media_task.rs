//! Preview media decode worker and cooperative-cancellation implementation.
//!
//! This App Module owns codec execution, cooperative cancellation observation,
//! result publication, and bounded worker shutdown. Window and Headless
//! Adapters may consume its results, but neither owns an alternate decode loop.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use mondrian_core::{MondrianError, Resolution, TimelineTime};
use mondrian_media::{
    decode_preview_frame_cancellable, HwAccelDeviceSelector, MediaFileFingerprint,
    PreviewDecodeAccessMode, PreviewDecodeAdaptiveHints, PreviewDecodeCancellation,
    PreviewDecodeDiagnostics, PreviewDecodeOutcome, PreviewDecodeRequest,
    PreviewDecodeSessionContext, PreviewDecodeSessionContextBootstrap,
    PreviewHardwareDecodeRequest, PreviewSourceColorContract,
};
use mondrian_playback::{FrameDemandIdentity, FrameExecutionId};
use mondrian_renderer::{
    CpuEncodedColorFrame, CpuSourceColorFrame, LinearFloatSource, RenderColorStageDiagnostics,
    RenderColorTransformDiagnostics, RenderInputTransform,
};

use super::preview_access_mode::{
    media_preview_cancel_reason_at_checkpoint, media_preview_cancel_reason_from_execution,
    media_preview_cancel_request_to_observed_us, MediaPreviewCancelReason, MediaPreviewJob,
    MediaPreviewJobQueueReceive, MediaPreviewJobQueueReceiver, MediaPreviewJobQueueWait,
    MediaPreviewKey, MediaPreviewRequestPriority, MediaPreviewScheduler, MediaPreviewWorkerLane,
    MEDIA_PREVIEW_DECODE_SESSION_IDLE_TIMEOUT,
};
use super::preview_decode_residency::PreviewDecodeResidencyCoordinator;
use super::preview_execution::PreviewDecodeExecutionSummary;
use super::preview_media_frame::{
    MediaPreviewFrame, MediaPreviewGpuSourceFrame, MediaPreviewNativeSourceFrame,
};
use super::preview_scheduler_policy::{
    preview_decode_presentation_quality, MediaPreviewFailureReason,
};

const MEDIA_PREVIEW_LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_micros(250);

/// Terminal or presentable result published by one Preview media task.
#[derive(Debug)]
pub(crate) struct MediaPreviewResult {
    pub(crate) key: MediaPreviewKey,
    pub(crate) frame: Option<MediaPreviewFrame>,
    pub(crate) error: Option<String>,
    pub(crate) failure_reason: Option<MediaPreviewFailureReason>,
    pub(crate) generation: u64,
    pub(crate) priority: MediaPreviewRequestPriority,
    pub(crate) access_mode: PreviewDecodeAccessMode,
    pub(crate) queue_wait_us: u64,
    pub(crate) decode_elapsed_us: u64,
    pub(crate) deadline_at: Option<Instant>,
    pub(crate) cancel_observed_elapsed_us: Option<u64>,
    pub(crate) cancel_request_to_observed_us: Option<u64>,
    pub(crate) canceled: bool,
    pub(crate) cancellation_phase: Option<MediaPreviewCancellationPhase>,
    pub(crate) cancel_reason: Option<MediaPreviewCancelReason>,
    pub(crate) decode_cancellation: Option<PreviewDecodeCancellation>,
    pub(crate) decode_diagnostics: Option<PreviewDecodeDiagnostics>,
    pub(crate) color_diagnostics: Option<RenderColorTransformDiagnostics>,
    pub(crate) color_stage_diagnostics: Option<RenderColorStageDiagnostics>,
    pub(crate) demand_identity: Option<FrameDemandIdentity>,
    pub(crate) execution_id: Option<FrameExecutionId>,
}

/// Point at which cooperative cancellation became observable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreviewCancellationPhase {
    /// Deadline or obsolescence was resolved before codec work began.
    Queued,
    /// A worker lease began and cancellation was observed cooperatively.
    Executing,
}

/// Shared stop signal for one bounded Preview media worker group.
#[derive(Default)]
pub(crate) struct PreviewShutdownSignal {
    requested: AtomicBool,
}

impl PreviewShutdownSignal {
    pub(crate) fn request(&self) -> bool {
        self.requested.swap(true, Ordering::AcqRel)
    }

    pub(crate) fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

pub(crate) fn media_preview_worker(
    lane: MediaPreviewWorkerLane,
    jobs: MediaPreviewJobQueueReceiver,
    results: mpsc::SyncSender<MediaPreviewResult>,
    scheduler: MediaPreviewScheduler,
    shutdown: Arc<PreviewShutdownSignal>,
    residency: Arc<PreviewDecodeResidencyCoordinator>,
    decode_context_bootstrap: PreviewDecodeSessionContextBootstrap,
) {
    // Codec and hardware-surface residency is explicitly worker-owned. The
    // worker can now release it at lifecycle boundaries without reaching
    // through an implicit media-layer thread-local cache.
    let mut decode_context = decode_context_bootstrap.build();
    let mut residency_revision = 0;
    loop {
        if let Some(directive) = residency.worker_directive(lane, residency_revision) {
            if directive.retire_context() {
                // Published native outputs retain AVFrame references after the
                // codec call returns. Do not acknowledge family retirement
                // until the Frame Store/completion/renderer ownership chain has
                // released every such lease.
                if !decode_context.native_outputs_released() {
                    if shutdown.is_requested() {
                        break;
                    }
                    std::thread::sleep(MEDIA_PREVIEW_LIFECYCLE_POLL_INTERVAL);
                    continue;
                }
                decode_context.clear();
                residency.acknowledge_retirement(lane, directive.revision());
            }
            residency_revision = directive.revision();
        }

        let outcome = match jobs
            .recv_for_worker_outcome_timeout(lane, MEDIA_PREVIEW_DECODE_SESSION_IDLE_TIMEOUT)
        {
            MediaPreviewJobQueueWait::Work(outcome) => outcome,
            MediaPreviewJobQueueWait::Lifecycle => continue,
            MediaPreviewJobQueueWait::Idle => {
                decode_context.clear();
                continue;
            }
            MediaPreviewJobQueueWait::Closed => break,
        };
        let job = match outcome {
            MediaPreviewJobQueueReceive::Job(job) => job,
            MediaPreviewJobQueueReceive::DroppedExpired(job) => {
                if shutdown.is_requested() {
                    if let Some(execution_id) = job.execution_id {
                        scheduler.abandon_execution(execution_id);
                    }
                    break;
                }
                let queue_wait_us = app_duration_us(job.enqueued_at.elapsed());
                let Some(execution_id) = job.execution_id else {
                    continue;
                };
                let scheduler_evidence = scheduler.execution_cancellation_evidence(execution_id);
                let scheduler_cancellation =
                    scheduler_evidence.map(|evidence| evidence.cancellation);
                let reason = scheduler_cancellation
                    .map(|cancellation| {
                        media_preview_cancel_reason_from_execution(
                            cancellation,
                            job.priority,
                            job.access_mode,
                        )
                    })
                    .unwrap_or(MediaPreviewCancelReason::Unknown);
                let cancel_request_to_observed_us = scheduler_cancellation
                    .and_then(mondrian_playback::FrameExecutionCancellation::request_age)
                    .map(app_duration_us);
                let mut result = media_preview_canceled_result(
                    job,
                    queue_wait_us,
                    reason,
                    0,
                    Some(0),
                    cancel_request_to_observed_us,
                );
                result.cancellation_phase = Some(MediaPreviewCancellationPhase::Queued);
                match send_media_preview_result(
                    lane,
                    residency_revision,
                    &results,
                    &scheduler,
                    &shutdown,
                    &residency,
                    result,
                ) {
                    MediaPreviewResultPublication::Published => {}
                    MediaPreviewResultPublication::ResidencyTransition => continue,
                    MediaPreviewResultPublication::Stop => break,
                }
                continue;
            }
        };
        if shutdown.is_requested() {
            if let Some(execution_id) = job.execution_id {
                scheduler.abandon_execution(execution_id);
            }
            break;
        }
        let queue_wait_us = app_duration_us(job.enqueued_at.elapsed());
        let Some(execution_id) = job.execution_id else {
            continue;
        };
        let scheduler_evidence = scheduler.execution_cancellation_evidence(execution_id);
        if let Some(evidence) = scheduler_evidence {
            let cancellation = evidence.cancellation;
            let reason = media_preview_cancel_reason_from_execution(
                cancellation,
                job.priority,
                job.access_mode,
            );
            let execution_age_us = app_duration_us(evidence.execution_age);
            let result = media_preview_canceled_result(
                job,
                queue_wait_us,
                reason,
                execution_age_us,
                Some(execution_age_us),
                cancellation.request_age().map(app_duration_us),
            );
            match send_media_preview_result(
                lane,
                residency_revision,
                &results,
                &scheduler,
                &shutdown,
                &residency,
                result,
            ) {
                MediaPreviewResultPublication::Published => {}
                MediaPreviewResultPublication::ResidencyTransition => continue,
                MediaPreviewResultPublication::Stop => break,
            }
            continue;
        }
        let cancel_priority = job.priority;
        let cancel_access_mode = job.access_mode;
        let cancel_deadline_at = job.deadline_at;
        let cancel_scheduler = scheduler.clone();
        let decode_started_at = Instant::now();
        let cancel_observation = Arc::new(Mutex::new(MediaPreviewCancelObservation::default()));
        let worker_cancel_observation = Arc::clone(&cancel_observation);
        let mut result =
            decode_media_preview_with_context(job, queue_wait_us, &mut decode_context, move || {
                let scheduler_evidence =
                    cancel_scheduler.execution_cancellation_evidence(execution_id);
                let scheduler_cancellation =
                    scheduler_evidence.map(|evidence| evidence.cancellation);
                let reason = media_preview_cancel_reason_at_checkpoint(
                    scheduler_cancellation,
                    cancel_priority,
                    cancel_access_mode,
                    decode_started_at.elapsed(),
                    cancel_deadline_at,
                );
                if let Some(reason) = reason {
                    let observed_at = Instant::now();
                    let mut observation =
                        lock_media_preview_cancel_observation(&worker_cancel_observation);
                    if observation.observed_elapsed_us.is_none() {
                        observation.observed_elapsed_us = Some(
                            scheduler_evidence
                                .map(|evidence| app_duration_us(evidence.execution_age))
                                .unwrap_or_else(|| {
                                    app_duration_us(observed_at.duration_since(decode_started_at))
                                }),
                        );
                        observation.request_to_observed_us =
                            media_preview_cancel_request_to_observed_us(
                                reason,
                                scheduler_cancellation,
                                decode_started_at,
                                observed_at,
                            );
                    }
                    observation.reason = Some(reason);
                }
                reason.is_some()
            });
        let observation = *lock_media_preview_cancel_observation(&cancel_observation);
        if result.canceled && result.cancel_reason.is_none() {
            result.cancel_reason =
                Some(observation.reason.unwrap_or(MediaPreviewCancelReason::Unknown));
        }
        if result.canceled && result.cancel_observed_elapsed_us.is_none() {
            result.cancel_observed_elapsed_us = observation.observed_elapsed_us;
        }
        if result.canceled && result.cancel_request_to_observed_us.is_none() {
            result.cancel_request_to_observed_us = observation.request_to_observed_us;
        }
        if result.canceled {
            if let Some(evidence) = scheduler.execution_cancellation_evidence(execution_id) {
                result.decode_elapsed_us = app_duration_us(evidence.execution_age);
            }
        }
        match send_media_preview_result(
            lane,
            residency_revision,
            &results,
            &scheduler,
            &shutdown,
            &residency,
            result,
        ) {
            MediaPreviewResultPublication::Published => {}
            MediaPreviewResultPublication::ResidencyTransition => continue,
            MediaPreviewResultPublication::Stop => break,
        }
    }
    decode_context.clear();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaPreviewResultPublication {
    Published,
    ResidencyTransition,
    Stop,
}

fn send_media_preview_result(
    lane: MediaPreviewWorkerLane,
    residency_revision: u64,
    results: &mpsc::SyncSender<MediaPreviewResult>,
    scheduler: &MediaPreviewScheduler,
    shutdown: &PreviewShutdownSignal,
    residency: &PreviewDecodeResidencyCoordinator,
    mut result: MediaPreviewResult,
) -> MediaPreviewResultPublication {
    let execution_id = result.execution_id;
    if let Some(execution_id) = execution_id {
        scheduler.mark_execution_completed(execution_id);
    }

    loop {
        if shutdown.is_requested() {
            abandon_media_preview_result(scheduler, execution_id);
            return MediaPreviewResultPublication::Stop;
        }
        if residency
            .worker_directive(lane, residency_revision)
            .is_some_and(|directive| directive.retire_context())
        {
            // A completed native result may own a decoder surface even before
            // it reaches the foreground pump. Do not let completion-channel
            // backpressure delay destruction of an obsolete surface pool.
            resolve_retired_media_preview_result(scheduler, execution_id);
            return MediaPreviewResultPublication::ResidencyTransition;
        }
        match results.try_send(result) {
            Ok(()) => return MediaPreviewResultPublication::Published,
            Err(mpsc::TrySendError::Full(returned)) => {
                result = returned;
                std::thread::sleep(MEDIA_PREVIEW_LIFECYCLE_POLL_INTERVAL);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                abandon_media_preview_result(scheduler, execution_id);
                return MediaPreviewResultPublication::Stop;
            }
        }
    }
}

fn abandon_media_preview_result(
    scheduler: &MediaPreviewScheduler,
    execution_id: Option<FrameExecutionId>,
) {
    if let Some(execution_id) = execution_id {
        scheduler.abandon_execution(execution_id);
    }
}

fn resolve_retired_media_preview_result(
    scheduler: &MediaPreviewScheduler,
    execution_id: Option<FrameExecutionId>,
) {
    if let Some(execution_id) = execution_id {
        // The transport-family transition invalidates publication authority,
        // but the Runtime remains alive. Resolve the completed binding as
        // non-reusable so it cannot survive as hidden pending work.
        let _ = scheduler.resolve_execution(execution_id, false);
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct MediaPreviewCancelObservation {
    reason: Option<MediaPreviewCancelReason>,
    observed_elapsed_us: Option<u64>,
    request_to_observed_us: Option<u64>,
}

fn lock_media_preview_cancel_observation(
    observation: &Mutex<MediaPreviewCancelObservation>,
) -> std::sync::MutexGuard<'_, MediaPreviewCancelObservation> {
    observation.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn media_preview_canceled_result(
    job: MediaPreviewJob,
    queue_wait_us: u64,
    reason: MediaPreviewCancelReason,
    decode_elapsed_us: u64,
    cancel_observed_elapsed_us: Option<u64>,
    cancel_request_to_observed_us: Option<u64>,
) -> MediaPreviewResult {
    let demand_identity = job.demand_identity;
    let execution_id = job.execution_id;
    MediaPreviewResult {
        key: job.key,
        frame: None,
        error: None,
        failure_reason: None,
        generation: job.generation,
        priority: job.priority,
        access_mode: job.access_mode,
        queue_wait_us,
        decode_elapsed_us,
        deadline_at: job.deadline_at,
        cancel_observed_elapsed_us,
        cancel_request_to_observed_us,
        canceled: true,
        cancellation_phase: Some(MediaPreviewCancellationPhase::Executing),
        cancel_reason: Some(reason),
        decode_cancellation: None,
        decode_diagnostics: None,
        color_diagnostics: None,
        color_stage_diagnostics: None,
        demand_identity,
        execution_id,
    }
}

#[cfg(test)]
pub(crate) fn decode_media_preview(
    job: MediaPreviewJob,
    queue_wait_us: u64,
    should_cancel: impl Fn() -> bool + Send + Sync + 'static,
) -> MediaPreviewResult {
    decode_media_preview_inner(job, queue_wait_us, None, should_cancel)
}

fn decode_media_preview_with_context(
    job: MediaPreviewJob,
    queue_wait_us: u64,
    decode_context: &mut PreviewDecodeSessionContext,
    should_cancel: impl Fn() -> bool + Send + Sync + 'static,
) -> MediaPreviewResult {
    decode_media_preview_inner(job, queue_wait_us, Some(decode_context), should_cancel)
}

fn decode_media_preview_inner(
    job: MediaPreviewJob,
    queue_wait_us: u64,
    decode_context: Option<&mut PreviewDecodeSessionContext>,
    should_cancel: impl Fn() -> bool + Send + Sync + 'static,
) -> MediaPreviewResult {
    let decode_started_at = Instant::now();
    let signature = media_preview_frame_signature(&job.key);
    let logical_width = job.key.source_width;
    let logical_height = job.key.source_height;
    let priority = job.priority;
    let access_mode = job.access_mode;
    let deadline_at = job.deadline_at;
    let demand_identity = job.demand_identity;
    let execution_id = job.execution_id;
    let hardware_decode_request = if job.key.source_has_alpha {
        PreviewHardwareDecodeRequest::Auto
    } else {
        job.hardware_decode_request
    };
    let decode_outcome = decode_media_preview_for_access_mode(
        job.key.path.as_path(),
        job.key.source_time,
        Some(job.key.target_width.max(1)),
        Some(job.key.target_height.max(1)),
        access_mode,
        job.key.fingerprint,
        job.adaptive_hints,
        hardware_decode_request,
        job.hardware_decode_device_selector,
        PreviewSourceColorContract::new(job.key.input_color_space, job.key.input_video_range),
        decode_context,
        should_cancel,
    );
    let decode_elapsed_us = app_duration_us(decode_started_at.elapsed());
    match decode_outcome {
        Ok(PreviewDecodeOutcome::Frame(frame)) => {
            let decode_diagnostics = frame.diagnostics;
            let decode_execution = frame.decode_execution;
            let presentation_quality = preview_decode_presentation_quality(&decode_diagnostics);
            let width = frame.width;
            let height = frame.height;
            let source = CpuEncodedColorFrame::source_rgba8_shared(
                width,
                height,
                job.key.input_color_space,
                frame.into_shared_data(),
            );
            let source = match CpuSourceColorFrame::from(source)
                .normalize_alpha(job.key.alpha_interpretation)
            {
                Ok(source) => source,
                Err(error) => {
                    return media_preview_alpha_failure(
                        job,
                        queue_wait_us,
                        decode_elapsed_us,
                        decode_diagnostics,
                        error.to_string(),
                    );
                }
            };
            let input_transform = RenderInputTransform::to_working(
                job.key.working_color_space,
                job.key.tone_map,
                job.key.engine.clone(),
            );
            let gpu_source = MediaPreviewGpuSourceFrame::from_decode_diagnostics(
                source,
                input_transform,
                decode_diagnostics,
            );
            MediaPreviewResult {
                key: job.key,
                frame: Some(MediaPreviewFrame::from_source(
                    gpu_source,
                    Resolution { width: logical_width, height: logical_height },
                    signature,
                    presentation_quality,
                    PreviewDecodeExecutionSummary::from_path(decode_execution),
                )),
                error: None,
                failure_reason: None,
                generation: job.generation,
                priority,
                access_mode,
                queue_wait_us,
                decode_elapsed_us,
                deadline_at,
                cancel_observed_elapsed_us: None,
                cancel_request_to_observed_us: None,
                canceled: false,
                cancellation_phase: None,
                cancel_reason: None,
                decode_cancellation: None,
                decode_diagnostics: Some(decode_diagnostics),
                color_diagnostics: None,
                color_stage_diagnostics: None,
                demand_identity,
                execution_id,
            }
        }
        Ok(PreviewDecodeOutcome::FloatFrame(frame)) => {
            let decode_diagnostics = frame.diagnostics;
            let decode_execution = frame.decode_execution;
            let presentation_quality = preview_decode_presentation_quality(&decode_diagnostics);
            let width = frame.width;
            let height = frame.height;
            let source = LinearFloatSource::new_shared(
                width,
                height,
                job.key.input_color_space,
                frame.into_shared_data(),
            );
            let source = match CpuSourceColorFrame::from(source)
                .normalize_alpha(job.key.alpha_interpretation)
            {
                Ok(source) => source,
                Err(error) => {
                    return media_preview_alpha_failure(
                        job,
                        queue_wait_us,
                        decode_elapsed_us,
                        decode_diagnostics,
                        error.to_string(),
                    );
                }
            };
            let input_transform = RenderInputTransform::to_working(
                job.key.working_color_space,
                job.key.tone_map,
                job.key.engine.clone(),
            );
            let gpu_source = MediaPreviewGpuSourceFrame::from_decode_diagnostics(
                source,
                input_transform,
                decode_diagnostics,
            );
            MediaPreviewResult {
                key: job.key,
                frame: Some(MediaPreviewFrame::from_source(
                    gpu_source,
                    Resolution { width: logical_width, height: logical_height },
                    signature,
                    presentation_quality,
                    PreviewDecodeExecutionSummary::from_path(decode_execution),
                )),
                error: None,
                failure_reason: None,
                generation: job.generation,
                priority,
                access_mode,
                queue_wait_us,
                decode_elapsed_us,
                deadline_at,
                cancel_observed_elapsed_us: None,
                cancel_request_to_observed_us: None,
                canceled: false,
                cancellation_phase: None,
                cancel_reason: None,
                decode_cancellation: None,
                decode_diagnostics: Some(decode_diagnostics),
                color_diagnostics: None,
                color_stage_diagnostics: None,
                demand_identity,
                execution_id,
            }
        }
        Ok(PreviewDecodeOutcome::NativeGpuFrame(frame)) => {
            let decode_diagnostics = frame.diagnostics;
            if job.key.source_has_alpha {
                return media_preview_alpha_failure(
                    job,
                    queue_wait_us,
                    decode_elapsed_us,
                    decode_diagnostics,
                    "alpha-bearing media reached an opaque native GPU preview surface".to_owned(),
                );
            }
            let decode_execution = decode_diagnostics.execution_path();
            let presentation_quality = preview_decode_presentation_quality(&decode_diagnostics);
            let input_transform = RenderInputTransform::to_working_gpu(
                job.key.working_color_space,
                job.key.tone_map,
                job.key.engine.clone(),
            );
            let native_source = MediaPreviewNativeSourceFrame::from_native_frame(
                frame,
                job.key.input_color_space,
                input_transform,
            );
            MediaPreviewResult {
                key: job.key,
                frame: Some(MediaPreviewFrame::from_native(
                    native_source,
                    Resolution { width: logical_width, height: logical_height },
                    signature,
                    presentation_quality,
                    PreviewDecodeExecutionSummary::from_path(decode_execution),
                )),
                error: None,
                failure_reason: None,
                generation: job.generation,
                priority,
                access_mode,
                queue_wait_us,
                decode_elapsed_us,
                deadline_at,
                cancel_observed_elapsed_us: None,
                cancel_request_to_observed_us: None,
                canceled: false,
                cancellation_phase: None,
                cancel_reason: None,
                decode_cancellation: None,
                decode_diagnostics: Some(decode_diagnostics),
                color_diagnostics: None,
                color_stage_diagnostics: None,
                demand_identity,
                execution_id,
            }
        }
        Ok(PreviewDecodeOutcome::Canceled(cancellation)) => MediaPreviewResult {
            key: job.key,
            frame: None,
            error: None,
            failure_reason: None,
            generation: job.generation,
            priority,
            access_mode,
            queue_wait_us,
            decode_elapsed_us,
            deadline_at,
            cancel_observed_elapsed_us: None,
            cancel_request_to_observed_us: None,
            canceled: true,
            cancellation_phase: Some(MediaPreviewCancellationPhase::Executing),
            cancel_reason: None,
            decode_cancellation: Some(cancellation),
            decode_diagnostics: None,
            color_diagnostics: None,
            color_stage_diagnostics: None,
            demand_identity,
            execution_id,
        },
        Err(err) => {
            let failure_reason = media_preview_failure_reason(&err);
            MediaPreviewResult {
                key: job.key,
                frame: None,
                error: Some(err.to_string()),
                failure_reason: Some(failure_reason),
                generation: job.generation,
                priority,
                access_mode,
                queue_wait_us,
                decode_elapsed_us,
                deadline_at,
                cancel_observed_elapsed_us: None,
                cancel_request_to_observed_us: None,
                canceled: false,
                cancellation_phase: None,
                cancel_reason: None,
                decode_cancellation: None,
                decode_diagnostics: None,
                color_diagnostics: None,
                color_stage_diagnostics: None,
                demand_identity,
                execution_id,
            }
        }
    }
}

fn media_preview_alpha_failure(
    job: MediaPreviewJob,
    queue_wait_us: u64,
    decode_elapsed_us: u64,
    decode_diagnostics: PreviewDecodeDiagnostics,
    error: String,
) -> MediaPreviewResult {
    MediaPreviewResult {
        key: job.key,
        frame: None,
        error: Some(error),
        failure_reason: Some(MediaPreviewFailureReason::DecodeError),
        generation: job.generation,
        priority: job.priority,
        access_mode: job.access_mode,
        queue_wait_us,
        decode_elapsed_us,
        deadline_at: job.deadline_at,
        cancel_observed_elapsed_us: None,
        cancel_request_to_observed_us: None,
        canceled: false,
        cancellation_phase: None,
        cancel_reason: None,
        decode_cancellation: None,
        decode_diagnostics: Some(decode_diagnostics),
        color_diagnostics: None,
        color_stage_diagnostics: None,
        demand_identity: job.demand_identity,
        execution_id: job.execution_id,
    }
}

fn media_preview_failure_reason(err: &MondrianError) -> MediaPreviewFailureReason {
    match err {
        MondrianError::DecodeTimeout { .. } => MediaPreviewFailureReason::Timeout,
        MondrianError::DecodeBudgetExhausted { .. } => {
            MediaPreviewFailureReason::ForwardDecodeBudgetExhausted
        }
        _ => MediaPreviewFailureReason::DecodeError,
    }
}

fn decode_media_preview_for_access_mode(
    path: &Path,
    source_time: TimelineTime,
    max_width: Option<u32>,
    max_height: Option<u32>,
    access_mode: PreviewDecodeAccessMode,
    fingerprint: Option<MediaFileFingerprint>,
    adaptive_hints: PreviewDecodeAdaptiveHints,
    hardware_decode_request: PreviewHardwareDecodeRequest,
    hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    source_color: PreviewSourceColorContract,
    decode_context: Option<&mut PreviewDecodeSessionContext>,
    should_cancel: impl Fn() -> bool + Send + Sync + 'static,
) -> mondrian_core::Result<PreviewDecodeOutcome> {
    let mut request = PreviewDecodeRequest::new(path, source_time, access_mode, source_color)
        .with_max_size(max_width, max_height)
        .with_adaptive_hints(adaptive_hints)
        .with_hardware_decode_request(hardware_decode_request)
        .with_hardware_decode_device_selector(hardware_decode_device_selector);
    if let Some(fingerprint) = fingerprint {
        request = request.with_fingerprint(fingerprint);
    }
    match decode_context {
        Some(context) => context.decode_cancellable(request, should_cancel),
        None => decode_preview_frame_cancellable(request, should_cancel),
    }
}

fn app_duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn media_preview_frame_signature(key: &MediaPreviewKey) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
#[path = "preview_media_task/tests.rs"]
mod tests;
