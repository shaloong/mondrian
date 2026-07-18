//! Preview media decode worker and cooperative-cancellation implementation.
//!
//! This child Module owns codec execution and result publication while its
//! parent Preview Adapter owns render planning, caches, and diagnostics.

use super::*;

pub(super) fn media_preview_worker(
    lane: MediaPreviewWorkerLane,
    jobs: MediaPreviewJobQueueReceiver,
    results: mpsc::Sender<MediaPreviewResult>,
    scheduler: MediaPreviewScheduler,
    shutdown: Arc<PreviewShutdownSignal>,
) {
    while let Some(outcome) = jobs.recv_for_worker_outcome(lane) {
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
                let scheduler_cancellation = scheduler.execution_cancellation(execution_id);
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
                let result = media_preview_canceled_result(
                    job,
                    queue_wait_us,
                    reason,
                    0,
                    Some(0),
                    cancel_request_to_observed_us,
                );
                if !send_media_preview_result(&results, &scheduler, result) {
                    break;
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
        let scheduler_cancellation = scheduler.execution_cancellation(execution_id);
        if let Some(cancellation) = scheduler_cancellation {
            let reason = media_preview_cancel_reason_from_execution(
                cancellation,
                job.priority,
                job.access_mode,
            );
            let result = media_preview_canceled_result(
                job,
                queue_wait_us,
                reason,
                0,
                Some(0),
                cancellation.request_age().map(app_duration_us),
            );
            if !send_media_preview_result(&results, &scheduler, result) {
                break;
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
        let mut result = decode_media_preview(job, queue_wait_us, move || {
            let scheduler_cancellation = cancel_scheduler.execution_cancellation(execution_id);
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
                    observation.observed_elapsed_us = Some(app_duration_us(
                        observed_at.duration_since(decode_started_at),
                    ));
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
        if !send_media_preview_result(&results, &scheduler, result) {
            break;
        }
    }
    mondrian_media::clear_thread_local_preview_decode_session();
}

fn send_media_preview_result(
    results: &mpsc::Sender<MediaPreviewResult>,
    scheduler: &MediaPreviewScheduler,
    result: MediaPreviewResult,
) -> bool {
    let execution_id = result.execution_id;
    if let Some(execution_id) = execution_id {
        scheduler.mark_execution_completed(execution_id);
    }
    if results.send(result).is_ok() {
        true
    } else {
        if let Some(execution_id) = execution_id {
            scheduler.abandon_execution(execution_id);
        }
        false
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

pub(super) fn media_preview_canceled_result(
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
        cancel_reason: Some(reason),
        decode_diagnostics: None,
        color_diagnostics: None,
        color_stage_diagnostics: None,
        demand_identity,
        execution_id,
    }
}

pub(super) fn decode_media_preview(
    job: MediaPreviewJob,
    queue_wait_us: u64,
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
        job.source_secs,
        Some(job.key.target_width.max(1)),
        Some(job.key.target_height.max(1)),
        access_mode,
        job.key.fingerprint,
        job.adaptive_hints,
        hardware_decode_request,
        job.hardware_decode_device_selector,
        PreviewSourceColorContract::new(job.key.input_color_space, job.key.input_video_range),
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
                frame: Some(MediaPreviewFrame {
                    width,
                    height,
                    logical_width,
                    logical_height,
                    frame: None,
                    gpu_source: Some(gpu_source),
                    native_source: None,
                    signature,
                    presentation_quality,
                    decode_execution: AppUiPreviewDecodeExecutionSummary::from_path(
                        decode_execution,
                    ),
                }),
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
                cancel_reason: None,
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
                frame: Some(MediaPreviewFrame {
                    width,
                    height,
                    logical_width,
                    logical_height,
                    frame: None,
                    gpu_source: Some(gpu_source),
                    native_source: None,
                    signature,
                    presentation_quality,
                    decode_execution: AppUiPreviewDecodeExecutionSummary::from_path(
                        decode_execution,
                    ),
                }),
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
                cancel_reason: None,
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
            let width = frame.width;
            let height = frame.height;
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
                frame: Some(MediaPreviewFrame {
                    width,
                    height,
                    logical_width,
                    logical_height,
                    frame: None,
                    gpu_source: None,
                    native_source: Some(native_source),
                    signature,
                    presentation_quality,
                    decode_execution: AppUiPreviewDecodeExecutionSummary::from_path(
                        decode_execution,
                    ),
                }),
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
                cancel_reason: None,
                decode_diagnostics: Some(decode_diagnostics),
                color_diagnostics: None,
                color_stage_diagnostics: None,
                demand_identity,
                execution_id,
            }
        }
        Ok(PreviewDecodeOutcome::Canceled) => MediaPreviewResult {
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
            cancel_reason: None,
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
                cancel_reason: None,
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
        cancel_reason: None,
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
    source_secs: f64,
    max_width: Option<u32>,
    max_height: Option<u32>,
    access_mode: PreviewDecodeAccessMode,
    fingerprint: Option<PreviewFileFingerprint>,
    adaptive_hints: PreviewDecodeAdaptiveHints,
    hardware_decode_request: PreviewHardwareDecodeRequest,
    hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    source_color: PreviewSourceColorContract,
    should_cancel: impl Fn() -> bool + Send + Sync + 'static,
) -> mondrian_core::Result<PreviewDecodeOutcome> {
    let mut request = PreviewDecodeRequest::new(path, source_secs, access_mode, source_color)
        .with_max_size(max_width, max_height)
        .with_adaptive_hints(adaptive_hints)
        .with_hardware_decode_request(hardware_decode_request)
        .with_hardware_decode_device_selector(hardware_decode_device_selector);
    if let Some(fingerprint) = fingerprint {
        request = request.with_fingerprint(fingerprint);
    }
    decode_preview_frame_cancellable(request, should_cancel)
}
