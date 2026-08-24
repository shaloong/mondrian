//! Preview media decode worker and cooperative-cancellation implementation.
//!
//! This App Module owns codec execution, cooperative cancellation observation,
//! result publication, and bounded worker shutdown. Window and Headless
//! Adapters may consume its results, but neither owns an alternate decode loop.

use std::any::Any;
use std::hash::Hash;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mondrian_core::MondrianError;
use mondrian_media::{
    decode_preview_frame_cancellable, DecodedGpuFrameHandleKind, DecodedRgbaFrameContract,
    DecodedVideoSampling, DecodedVideoSurfaceFormat, HwAccelDeviceSelector,
    PreviewDecodeAccessMode, PreviewDecodeAdaptiveHints, PreviewDecodeCancellation,
    PreviewDecodeDiagnostics, PreviewDecodeExecutionPath, PreviewDecodeKey, PreviewDecodeOutcome,
    PreviewDecodeRequest, PreviewDecodeSessionContext, PreviewDecodeSessionContextBootstrap,
    PreviewHardwareDecodeRequest,
};
use mondrian_playback::{FrameDemandIdentity, FrameExecutionId};
use mondrian_renderer::{
    CpuEncodedColorFrame, CpuSourceColorFrame, LinearFloatSource, RenderColorStageDiagnostics,
    RenderColorTransformDiagnostics, RenderInputTransform,
};

use super::preview_access_mode::{
    media_preview_cancel_reason_from_execution, media_preview_queued_expiration_reason,
    MediaPreviewCancelReason, MediaPreviewJob, MediaPreviewJobQueueReceive,
    MediaPreviewJobQueueReceiver, MediaPreviewJobQueueWait, MediaPreviewKey,
    MediaPreviewRequestPriority, MediaPreviewScheduler, MediaPreviewWorkerLane,
    MEDIA_PREVIEW_DECODE_SESSION_IDLE_TIMEOUT,
};
use super::preview_decode_residency::PreviewDecodeResidencyCoordinator;
use super::preview_execution::{
    PreviewDecodeExecutionSummary, PreviewSemanticIdentity, PreviewSemanticIdentityBuilder,
};
use super::preview_media_frame::{
    MediaPreviewFrame, MediaPreviewGpuSourceFrame, MediaPreviewNativeSourceFrame,
};
use super::preview_scheduler_policy::{
    preview_decode_presentation_quality, MediaPreviewFailureReason,
};
use super::preview_work_notification::PreviewWorkNotifier;

const MEDIA_PREVIEW_LIFECYCLE_POLL_INTERVAL: Duration = Duration::from_micros(250);
const MEDIA_PREVIEW_CANCELLATION_OBSERVER_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
const MEDIA_PREVIEW_WORKER_PANIC_ERROR_MAX_BYTES: usize = 512;
const MEDIA_PREVIEW_WORKER_PANIC_TRUNCATION_SUFFIX: &str = "...[truncated]";

type MediaPreviewCancelProbe = Box<dyn Fn() -> bool + Send + Sync + 'static>;

enum MediaPreviewCancellationObserverCommand {
    Observe(MediaPreviewCancellationObserverRequest),
    Shutdown,
}

struct MediaPreviewCancellationObserverRequest {
    execution_id: FrameExecutionId,
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
    observation: MediaPreviewCancelObservationHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaPreviewCancellationObserverTerminal {
    Canceled,
    Completed,
    Missing,
    Failed,
}

#[derive(Debug, Clone, Copy)]
struct MediaPreviewCancellationObserverResponse {
    execution_id: FrameExecutionId,
    terminal: MediaPreviewCancellationObserverTerminal,
}

#[derive(Clone)]
struct MediaPreviewCancelObservationHandle {
    observation: Arc<Mutex<MediaPreviewCancelObservation>>,
}

impl MediaPreviewCancelObservationHandle {
    fn new() -> Self {
        Self {
            observation: Arc::new(Mutex::new(MediaPreviewCancelObservation::default())),
        }
    }

    fn is_canceled(&self) -> bool {
        lock_media_preview_cancel_observation(&self.observation).reason.is_some()
    }

    fn snapshot(&self) -> MediaPreviewCancelObservation {
        *lock_media_preview_cancel_observation(&self.observation)
    }

    fn freeze(
        &self,
        evidence: mondrian_playback::FrameExecutionCancellationEvidence,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
    ) {
        let mut observation = lock_media_preview_cancel_observation(&self.observation);
        if observation.reason.is_some() {
            return;
        }
        observation.reason = Some(media_preview_cancel_reason_from_execution(
            evidence.cancellation,
            priority,
            access_mode,
        ));
        observation.logical_cancellation_observed = Some(LogicalCancellationObserved {
            execution_elapsed_us: app_duration_us(evidence.execution_age),
            request_elapsed_us: evidence.cancellation.request_age().map(app_duration_us),
        });
    }
}

struct MediaPreviewCancellationObserver {
    commands: mpsc::SyncSender<MediaPreviewCancellationObserverCommand>,
    responses: mpsc::Receiver<MediaPreviewCancellationObserverResponse>,
    worker: Option<JoinHandle<()>>,
}

impl MediaPreviewCancellationObserver {
    fn start(
        lane: MediaPreviewWorkerLane,
        scheduler: MediaPreviewScheduler,
    ) -> std::io::Result<Self> {
        let (commands, command_receiver) = mpsc::sync_channel(1);
        let (response_sender, responses) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name(format!("mondrian-preview-cancel-observer-{lane:?}"))
            .spawn(move || {
                media_preview_cancellation_observer(scheduler, command_receiver, response_sender)
            })?;
        Ok(Self { commands, responses, worker: Some(worker) })
    }

    fn observe(
        &self,
        execution_id: FrameExecutionId,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
    ) -> Result<MediaPreviewCancelObservationHandle, ()> {
        let observation = MediaPreviewCancelObservationHandle::new();
        self.commands
            .send(MediaPreviewCancellationObserverCommand::Observe(
                MediaPreviewCancellationObserverRequest {
                    execution_id,
                    priority,
                    access_mode,
                    observation: observation.clone(),
                },
            ))
            .map_err(|_| ())?;
        Ok(observation)
    }

    fn finish(
        &self,
        execution_id: FrameExecutionId,
    ) -> Result<MediaPreviewCancellationObserverTerminal, ()> {
        let response = self.responses.recv().map_err(|_| ())?;
        if response.execution_id != execution_id {
            return Err(());
        }
        Ok(response.terminal)
    }

    fn shutdown_and_join(&mut self) {
        let _ = self.commands.send(MediaPreviewCancellationObserverCommand::Shutdown);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            tracing::warn!("Preview media cancellation observer panicked during shutdown");
        }
    }
}

impl Drop for MediaPreviewCancellationObserver {
    fn drop(&mut self) {
        self.shutdown_and_join();
    }
}

fn media_preview_cancellation_observer(
    scheduler: MediaPreviewScheduler,
    commands: mpsc::Receiver<MediaPreviewCancellationObserverCommand>,
    responses: mpsc::SyncSender<MediaPreviewCancellationObserverResponse>,
) {
    while let Ok(command) = commands.recv() {
        let MediaPreviewCancellationObserverCommand::Observe(request) = command else {
            break;
        };
        let terminal = catch_unwind(AssertUnwindSafe(|| {
            scheduler.wait_for_execution_terminal_state(
                request.execution_id,
                MEDIA_PREVIEW_CANCELLATION_OBSERVER_WAIT,
            )
        }))
        .map_or(
            MediaPreviewCancellationObserverTerminal::Failed,
            |status| match status {
                mondrian_playback::FrameExecutionWaitStatus::Canceled(evidence) => {
                    request.observation.freeze(evidence, request.priority, request.access_mode);
                    MediaPreviewCancellationObserverTerminal::Canceled
                }
                mondrian_playback::FrameExecutionWaitStatus::Completed => {
                    MediaPreviewCancellationObserverTerminal::Completed
                }
                mondrian_playback::FrameExecutionWaitStatus::Missing => {
                    MediaPreviewCancellationObserverTerminal::Missing
                }
                mondrian_playback::FrameExecutionWaitStatus::Timeout => {
                    MediaPreviewCancellationObserverTerminal::Failed
                }
            },
        );
        if responses
            .send(MediaPreviewCancellationObserverResponse {
                execution_id: request.execution_id,
                terminal,
            })
            .is_err()
        {
            break;
        }
    }
}

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
    pub(crate) queue_disposition: MediaPreviewQueueDisposition,
    pub(crate) queue_wait_us: u64,
    pub(crate) decode_elapsed_us: u64,
    pub(crate) deadline_at: Option<Instant>,
    pub(crate) logical_cancellation_observed: Option<LogicalCancellationObserved>,
    pub(crate) canceled: bool,
    pub(crate) cancellation_phase: Option<MediaPreviewCancellationPhase>,
    pub(crate) cancel_reason: Option<MediaPreviewCancelReason>,
    pub(crate) concrete_media_checkpoint: Option<PreviewDecodeCancellation>,
    pub(crate) decode_diagnostics: Option<PreviewDecodeDiagnostics>,
    pub(crate) color_diagnostics: Option<RenderColorTransformDiagnostics>,
    pub(crate) color_stage_diagnostics: Option<RenderColorStageDiagnostics>,
    pub(crate) demand_identity: Option<FrameDemandIdentity>,
    pub(crate) execution_id: Option<FrameExecutionId>,
    /// Exact physical decode-attempt charge, released when this result is dropped.
    pub(crate) residency_work: Option<mondrian_playback::MediaWorkResourceLease>,
}

/// Broker dequeue decision that authorized or rejected worker execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreviewQueueDisposition {
    /// The broker granted an execution lease and codec work may begin.
    Ready,
    /// The broker rejected the queued binding before codec execution began.
    Expired,
}

/// Point at which cooperative cancellation became observable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreviewCancellationPhase {
    /// Deadline or obsolescence was resolved before codec work began.
    Queued,
    /// A worker lease began and cancellation was observed cooperatively.
    Executing,
}

/// Broker-clock timestamp of the first logical cancellation observation.
///
/// This is intentionally distinct from a concrete media checkpoint such as an
/// FFmpeg I/O interrupt, packet-read boundary, or decoder receive boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LogicalCancellationObserved {
    pub(crate) execution_elapsed_us: u64,
    pub(crate) request_elapsed_us: Option<u64>,
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
    work_notifier: PreviewWorkNotifier,
    scheduler: MediaPreviewScheduler,
    shutdown: Arc<PreviewShutdownSignal>,
    residency: Arc<PreviewDecodeResidencyCoordinator>,
    decode_context_bootstrap: PreviewDecodeSessionContextBootstrap,
) {
    media_preview_worker_with_decoder(
        lane,
        jobs,
        results,
        work_notifier,
        scheduler,
        shutdown,
        residency,
        decode_context_bootstrap,
        |job, queue_wait_us, decode_context, _recovery_revision, should_cancel| {
            decode_media_preview_with_context(job, queue_wait_us, decode_context, should_cancel)
        },
    );
}

fn media_preview_worker_with_decoder<DecodeJob>(
    lane: MediaPreviewWorkerLane,
    jobs: MediaPreviewJobQueueReceiver,
    results: mpsc::SyncSender<MediaPreviewResult>,
    work_notifier: PreviewWorkNotifier,
    scheduler: MediaPreviewScheduler,
    shutdown: Arc<PreviewShutdownSignal>,
    residency: Arc<PreviewDecodeResidencyCoordinator>,
    decode_context_bootstrap: PreviewDecodeSessionContextBootstrap,
    mut decode_job: DecodeJob,
) where
    DecodeJob: FnMut(
        MediaPreviewJob,
        u64,
        &mut PreviewDecodeSessionContext,
        u64,
        MediaPreviewCancelProbe,
    ) -> MediaPreviewResult,
{
    let _exit_notification = work_notifier.worker_exit_notification();
    let mut cancellation_observer =
        match MediaPreviewCancellationObserver::start(lane, scheduler.clone()) {
            Ok(observer) => observer,
            Err(error) => {
                tracing::error!(
                    ?lane,
                    "failed to start Preview media cancellation observer: {error}"
                );
                scheduler.close();
                return;
            }
        };
    // Codec and hardware-surface residency is explicitly worker-owned. The
    // worker can now release it at lifecycle boundaries without reaching
    // through an implicit media-layer thread-local cache.
    let mut decode_context = MediaPreviewWorkerDecodeContext::new(decode_context_bootstrap);
    let mut residency_revision = 0;
    // A family retirement whose native outputs are still leased (e.g. a
    // hardware-decoded still frame retained by the Frame Store or Viewer) is
    // acknowledged immediately so the family barrier cannot stall decode
    // admission behind one renderer/lease lifetime; the codec context is then
    // retired lazily once the last native lease drops.
    let mut pending_native_retire = false;
    loop {
        if pending_native_retire && decode_context.native_outputs_released() {
            decode_context.clear();
            pending_native_retire = false;
        }
        if let Some(directive) = residency.worker_directive(lane, residency_revision) {
            if directive.retire_context() {
                if decode_context.native_outputs_released() {
                    decode_context.clear();
                    pending_native_retire = false;
                    residency.acknowledge_retirement(lane, directive.revision());
                } else {
                    residency.acknowledge_retirement(lane, directive.revision());
                    pending_native_retire = true;
                }
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
                let Some(execution_id) = job.execution_id else {
                    if shutdown.is_requested() {
                        break;
                    }
                    continue;
                };
                let mut execution_guard =
                    MediaPreviewExecutionGuard::new(scheduler.clone(), execution_id);
                if shutdown.is_requested() {
                    scheduler.abandon_execution(execution_id);
                    execution_guard.disarm();
                    break;
                }
                let queue_wait_us = app_duration_us(job.enqueued_at.elapsed());
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
                    .unwrap_or_else(|| {
                        media_preview_queued_expiration_reason(job.priority, job.access_mode)
                    });
                let logical_request_elapsed_us = scheduler_cancellation
                    .and_then(mondrian_playback::FrameExecutionCancellation::request_age)
                    .map(app_duration_us);
                let mut result = media_preview_canceled_result(
                    job,
                    queue_wait_us,
                    reason,
                    0,
                    Some(0),
                    logical_request_elapsed_us,
                );
                result.cancellation_phase = Some(MediaPreviewCancellationPhase::Queued);
                result.queue_disposition = MediaPreviewQueueDisposition::Expired;
                let publication = send_media_preview_result(
                    lane,
                    residency_revision,
                    &results,
                    &work_notifier,
                    &scheduler,
                    &shutdown,
                    &residency,
                    result,
                );
                execution_guard.disarm();
                match publication {
                    MediaPreviewResultPublication::Published => {}
                    MediaPreviewResultPublication::ResidencyTransition => continue,
                    MediaPreviewResultPublication::Stop => break,
                }
                continue;
            }
        };
        let Some(execution_id) = job.execution_id else {
            continue;
        };
        let mut execution_guard = MediaPreviewExecutionGuard::new(scheduler.clone(), execution_id);
        if shutdown.is_requested() {
            scheduler.abandon_execution(execution_id);
            execution_guard.disarm();
            break;
        }
        let queue_wait_us = app_duration_us(job.enqueued_at.elapsed());
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
            let publication = send_media_preview_result(
                lane,
                residency_revision,
                &results,
                &work_notifier,
                &scheduler,
                &shutdown,
                &residency,
                result,
            );
            execution_guard.disarm();
            match publication {
                MediaPreviewResultPublication::Published => {}
                MediaPreviewResultPublication::ResidencyTransition => continue,
                MediaPreviewResultPublication::Stop => break,
            }
            continue;
        }
        let decode_started_at = Instant::now();
        let panic_context =
            MediaPreviewWorkerPanicContext::from_job(&job, queue_wait_us, execution_id);
        let recovery_revision = decode_context.recovery_revision();
        let cancel_observation =
            match cancellation_observer.observe(execution_id, job.priority, job.access_mode) {
                Ok(observation) => observation,
                Err(()) => {
                    tracing::error!(
                        ?lane,
                        ?execution_id,
                        "Preview media cancellation observer disconnected before decode"
                    );
                    scheduler.fail_execution(execution_id);
                    execution_guard.disarm();
                    break;
                }
            };
        let result = catch_unwind(AssertUnwindSafe(|| {
            let worker_cancel_observation = cancel_observation.clone();
            decode_job(
                job,
                queue_wait_us,
                decode_context.context_mut(),
                recovery_revision,
                Box::new(move || worker_cancel_observation.is_canceled()),
            )
        }));
        let mut result = match result {
            Ok(result) => result,
            Err(payload) => {
                let panic_error = bounded_media_preview_worker_panic_error(payload.as_ref());
                let clear_panicked = decode_context.recover_after_panic();
                tracing::error!(
                    asset_id = %panic_context.key.asset_id,
                    execution_id = ?execution_id,
                    clear_panicked,
                    "{panic_error}"
                );
                panic_context.into_result(panic_error, app_duration_us(decode_started_at.elapsed()))
            }
        };
        scheduler.mark_execution_completed(execution_id);
        let terminal = match cancellation_observer.finish(execution_id) {
            Ok(terminal) => terminal,
            Err(()) => {
                tracing::error!(
                    ?lane,
                    ?execution_id,
                    "Preview media cancellation observer disconnected after decode"
                );
                scheduler.fail_execution(execution_id);
                execution_guard.disarm();
                break;
            }
        };
        match terminal {
            MediaPreviewCancellationObserverTerminal::Canceled => {
                apply_media_preview_cancel_observation(&mut result, cancel_observation.snapshot());
            }
            MediaPreviewCancellationObserverTerminal::Completed => {}
            MediaPreviewCancellationObserverTerminal::Missing => {
                apply_media_preview_cancel_observation(
                    &mut result,
                    MediaPreviewCancelObservation {
                        reason: Some(MediaPreviewCancelReason::Obsolete),
                        logical_cancellation_observed: None,
                    },
                );
            }
            MediaPreviewCancellationObserverTerminal::Failed => {
                tracing::error!(
                    ?lane,
                    ?execution_id,
                    "Preview media cancellation observer failed to reach a terminal state"
                );
                scheduler.fail_execution(execution_id);
                execution_guard.disarm();
                break;
            }
        }
        if result.canceled && result.cancel_reason.is_none() {
            result.cancel_reason = Some(MediaPreviewCancelReason::Unknown);
        }
        if result.canceled
            && let Some(evidence) = scheduler.execution_cancellation_evidence(execution_id)
        {
            result.decode_elapsed_us = app_duration_us(evidence.execution_age);
        }
        let publication = send_media_preview_result(
            lane,
            residency_revision,
            &results,
            &work_notifier,
            &scheduler,
            &shutdown,
            &residency,
            result,
        );
        execution_guard.disarm();
        match publication {
            MediaPreviewResultPublication::Published => {}
            MediaPreviewResultPublication::ResidencyTransition => continue,
            MediaPreviewResultPublication::Stop => break,
        }
    }
    cancellation_observer.shutdown_and_join();
    decode_context.clear();
}

struct MediaPreviewExecutionGuard {
    scheduler: MediaPreviewScheduler,
    execution_id: FrameExecutionId,
    armed: bool,
}

impl MediaPreviewExecutionGuard {
    fn new(scheduler: MediaPreviewScheduler, execution_id: FrameExecutionId) -> Self {
        Self { scheduler, execution_id, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for MediaPreviewExecutionGuard {
    fn drop(&mut self) {
        if self.armed {
            // This is the last-resort exact-binding cleanup for panics outside
            // the per-job catch boundary (context recovery, tracing, result
            // publication, or future worker-local glue). The job/result stack
            // independently drops its move-only physical residency attempt.
            self.scheduler.fail_execution(self.execution_id);
        }
    }
}

struct MediaPreviewWorkerDecodeContext {
    bootstrap: PreviewDecodeSessionContextBootstrap,
    context: PreviewDecodeSessionContext,
    recovery_revision: u64,
}

impl MediaPreviewWorkerDecodeContext {
    fn new(bootstrap: PreviewDecodeSessionContextBootstrap) -> Self {
        let context = bootstrap.clone_for_sequential_recovery().build();
        Self { bootstrap, context, recovery_revision: 0 }
    }

    fn context_mut(&mut self) -> &mut PreviewDecodeSessionContext {
        &mut self.context
    }

    fn recovery_revision(&self) -> u64 {
        self.recovery_revision
    }

    fn native_outputs_released(&self) -> bool {
        self.context.native_outputs_released()
    }

    fn clear(&mut self) {
        self.context.clear();
    }

    /// Retire every potentially poisoned codec/demux resource before the next job.
    ///
    /// The replacement is built from the same worker bootstrap only after the
    /// old context has stopped executing. A panic in best-effort cleanup is
    /// contained as well; replacement construction does not reuse Session state.
    fn recover_after_panic(&mut self) -> bool {
        let clear_panicked = catch_unwind(AssertUnwindSafe(|| self.context.clear())).is_err();
        self.context = self.bootstrap.clone_for_sequential_recovery().build();
        self.recovery_revision = self.recovery_revision.saturating_add(1);
        clear_panicked
    }
}

struct MediaPreviewWorkerPanicContext {
    key: MediaPreviewKey,
    generation: u64,
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
    queue_wait_us: u64,
    deadline_at: Option<Instant>,
    demand_identity: Option<FrameDemandIdentity>,
    execution_id: FrameExecutionId,
}

impl MediaPreviewWorkerPanicContext {
    fn from_job(job: &MediaPreviewJob, queue_wait_us: u64, execution_id: FrameExecutionId) -> Self {
        Self {
            key: job.key.clone(),
            generation: job.generation,
            priority: job.priority,
            access_mode: job.access_mode,
            queue_wait_us,
            deadline_at: job.deadline_at,
            demand_identity: job.demand_identity,
            execution_id,
        }
    }

    fn into_result(self, error: String, decode_elapsed_us: u64) -> MediaPreviewResult {
        MediaPreviewResult {
            key: self.key,
            frame: None,
            error: Some(error),
            failure_reason: Some(MediaPreviewFailureReason::WorkerPanicked),
            generation: self.generation,
            priority: self.priority,
            access_mode: self.access_mode,
            queue_disposition: MediaPreviewQueueDisposition::Ready,
            queue_wait_us: self.queue_wait_us,
            decode_elapsed_us,
            deadline_at: self.deadline_at,
            logical_cancellation_observed: None,
            canceled: false,
            cancellation_phase: None,
            cancel_reason: None,
            concrete_media_checkpoint: None,
            decode_diagnostics: None,
            color_diagnostics: None,
            color_stage_diagnostics: None,
            demand_identity: self.demand_identity,
            execution_id: Some(self.execution_id),
            // The panicked stack already dropped the move-only attempt lease.
            // A failure result must never retain or fabricate physical residency.
            residency_work: None,
        }
    }
}

fn bounded_media_preview_worker_panic_error(payload: &(dyn Any + Send)) -> String {
    const PREFIX: &str = "preview media worker panicked: ";
    let detail = payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("panic payload is not a string");
    let detail_budget = MEDIA_PREVIEW_WORKER_PANIC_ERROR_MAX_BYTES.saturating_sub(PREFIX.len());
    let detail = bounded_media_preview_worker_panic_detail(detail, detail_budget);
    let mut error = String::with_capacity(PREFIX.len().saturating_add(detail.len()));
    error.push_str(PREFIX);
    error.push_str(&detail);
    error
}

fn bounded_media_preview_worker_panic_detail(detail: &str, max_bytes: usize) -> String {
    if detail.len() <= max_bytes {
        return detail.to_owned();
    }
    if max_bytes <= MEDIA_PREVIEW_WORKER_PANIC_TRUNCATION_SUFFIX.len() {
        return MEDIA_PREVIEW_WORKER_PANIC_TRUNCATION_SUFFIX[..max_bytes].to_owned();
    }
    let content_budget =
        max_bytes.saturating_sub(MEDIA_PREVIEW_WORKER_PANIC_TRUNCATION_SUFFIX.len());
    let mut content_end = content_budget.min(detail.len());
    while content_end > 0 && !detail.is_char_boundary(content_end) {
        content_end -= 1;
    }
    let mut bounded = String::with_capacity(max_bytes);
    bounded.push_str(&detail[..content_end]);
    bounded.push_str(MEDIA_PREVIEW_WORKER_PANIC_TRUNCATION_SUFFIX);
    bounded
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
    work_notifier: &PreviewWorkNotifier,
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
            Ok(()) => {
                work_notifier.result_became_pollable();
                return MediaPreviewResultPublication::Published;
            }
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
    logical_cancellation_observed: Option<LogicalCancellationObserved>,
}

fn lock_media_preview_cancel_observation(
    observation: &Mutex<MediaPreviewCancelObservation>,
) -> std::sync::MutexGuard<'_, MediaPreviewCancelObservation> {
    observation.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn apply_media_preview_cancel_observation(
    result: &mut MediaPreviewResult,
    observation: MediaPreviewCancelObservation,
) {
    let Some(reason) = observation.reason else {
        return;
    };
    result.frame = None;
    result.error = None;
    result.failure_reason = None;
    result.canceled = true;
    result.cancellation_phase = Some(MediaPreviewCancellationPhase::Executing);
    result.cancel_reason = Some(reason);
    result.logical_cancellation_observed = observation.logical_cancellation_observed;
}

pub(crate) fn media_preview_canceled_result(
    job: MediaPreviewJob,
    queue_wait_us: u64,
    reason: MediaPreviewCancelReason,
    decode_elapsed_us: u64,
    logical_execution_elapsed_us: Option<u64>,
    logical_request_elapsed_us: Option<u64>,
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
        queue_disposition: MediaPreviewQueueDisposition::Ready,
        queue_wait_us,
        decode_elapsed_us,
        deadline_at: job.deadline_at,
        logical_cancellation_observed: logical_execution_elapsed_us.map(|execution_elapsed_us| {
            LogicalCancellationObserved {
                execution_elapsed_us,
                request_elapsed_us: logical_request_elapsed_us,
            }
        }),
        canceled: true,
        cancellation_phase: Some(MediaPreviewCancellationPhase::Executing),
        cancel_reason: Some(reason),
        concrete_media_checkpoint: None,
        decode_diagnostics: None,
        color_diagnostics: None,
        color_stage_diagnostics: None,
        demand_identity,
        execution_id,
        residency_work: job.residency_work,
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

pub(crate) fn decode_media_preview_with_context(
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
    let logical_resolution = job.key.source_resolution;
    let picture_geometry = job.key.picture_geometry;
    let priority = job.priority;
    let access_mode = job.access_mode;
    let deadline_at = job.deadline_at;
    let demand_identity = job.demand_identity;
    let execution_id = job.execution_id;
    let hardware_decode_request = if job.key.source_has_alpha() {
        PreviewHardwareDecodeRequest::Auto
    } else {
        job.hardware_decode_request
    };
    let decode_outcome = decode_media_preview_for_access_mode(
        &job.key.decode,
        access_mode,
        job.adaptive_hints,
        hardware_decode_request,
        job.hardware_decode_device_selector,
        decode_context,
        should_cancel,
    );
    let decode_elapsed_us = app_duration_us(decode_started_at.elapsed());
    match decode_outcome {
        Ok(PreviewDecodeOutcome::Frame(frame)) => {
            let decode_diagnostics = frame.diagnostics;
            let decode_execution = frame.decode_execution;
            let presentation_quality =
                match preview_decode_presentation_quality(&decode_diagnostics) {
                    Ok(quality) => quality,
                    Err(reason) => {
                        return media_preview_temporal_failure(
                            job,
                            queue_wait_us,
                            decode_elapsed_us,
                            decode_diagnostics,
                            reason,
                        );
                    }
                };
            let width = frame.width;
            let height = frame.height;
            let frame_identity = media_preview_frame_identity(
                &job.key,
                MediaPreviewDecodedFrameEvidence::CpuRgba8 {
                    width,
                    height,
                    color_contract: frame.color_contract,
                    selected_pts: decode_diagnostics.selected_pts,
                    surface_format: decode_diagnostics.decoded_surface_format,
                    sampling: decode_diagnostics.decoded_video_sampling,
                    execution: decode_execution,
                },
            );
            let source = CpuEncodedColorFrame::source_rgba8_shared(
                width,
                height,
                job.key.decode.source_color().color_space,
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
                job.key.input_tone_map,
                job.key.engine.clone(),
            );
            let gpu_source = MediaPreviewGpuSourceFrame::from_decode_diagnostics(
                source,
                input_transform,
                decode_diagnostics,
            );
            MediaPreviewResult {
                key: job.key,
                frame: Some(
                    MediaPreviewFrame::from_source(
                        gpu_source,
                        logical_resolution,
                        frame_identity,
                        presentation_quality,
                        PreviewDecodeExecutionSummary::from_path(decode_execution),
                    )
                    .with_picture_geometry(picture_geometry)
                    .with_cross_call_reuse(decode_diagnostics.selected_pts.is_some()),
                ),
                error: None,
                failure_reason: None,
                generation: job.generation,
                priority,
                access_mode,
                queue_disposition: MediaPreviewQueueDisposition::Ready,
                queue_wait_us,
                decode_elapsed_us,
                deadline_at,
                logical_cancellation_observed: None,
                canceled: false,
                cancellation_phase: None,
                cancel_reason: None,
                concrete_media_checkpoint: None,
                decode_diagnostics: Some(decode_diagnostics),
                color_diagnostics: None,
                color_stage_diagnostics: None,
                demand_identity,
                execution_id,
                residency_work: job.residency_work,
            }
        }
        Ok(PreviewDecodeOutcome::FloatFrame(frame)) => {
            let decode_diagnostics = frame.diagnostics;
            let decode_execution = frame.decode_execution;
            let presentation_quality =
                match preview_decode_presentation_quality(&decode_diagnostics) {
                    Ok(quality) => quality,
                    Err(reason) => {
                        return media_preview_temporal_failure(
                            job,
                            queue_wait_us,
                            decode_elapsed_us,
                            decode_diagnostics,
                            reason,
                        );
                    }
                };
            let width = frame.width;
            let height = frame.height;
            let frame_identity = media_preview_frame_identity(
                &job.key,
                MediaPreviewDecodedFrameEvidence::CpuLinearRgbaF32 {
                    width,
                    height,
                    color_contract: frame.color_contract,
                    selected_pts: decode_diagnostics.selected_pts,
                    surface_format: decode_diagnostics.decoded_surface_format,
                    sampling: decode_diagnostics.decoded_video_sampling,
                    execution: decode_execution,
                },
            );
            let source = LinearFloatSource::new_shared(
                width,
                height,
                job.key.decode.source_color().color_space,
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
                job.key.input_tone_map,
                job.key.engine.clone(),
            );
            let gpu_source = MediaPreviewGpuSourceFrame::from_decode_diagnostics(
                source,
                input_transform,
                decode_diagnostics,
            );
            MediaPreviewResult {
                key: job.key,
                frame: Some(
                    MediaPreviewFrame::from_source(
                        gpu_source,
                        logical_resolution,
                        frame_identity,
                        presentation_quality,
                        PreviewDecodeExecutionSummary::from_path(decode_execution),
                    )
                    .with_picture_geometry(picture_geometry)
                    .with_cross_call_reuse(decode_diagnostics.selected_pts.is_some()),
                ),
                error: None,
                failure_reason: None,
                generation: job.generation,
                priority,
                access_mode,
                queue_disposition: MediaPreviewQueueDisposition::Ready,
                queue_wait_us,
                decode_elapsed_us,
                deadline_at,
                logical_cancellation_observed: None,
                canceled: false,
                cancellation_phase: None,
                cancel_reason: None,
                concrete_media_checkpoint: None,
                decode_diagnostics: Some(decode_diagnostics),
                color_diagnostics: None,
                color_stage_diagnostics: None,
                demand_identity,
                execution_id,
                residency_work: job.residency_work,
            }
        }
        Ok(PreviewDecodeOutcome::NativeGpuFrame(frame)) => {
            let decode_diagnostics = frame.diagnostics;
            if job.key.source_has_alpha() {
                return media_preview_alpha_failure(
                    job,
                    queue_wait_us,
                    decode_elapsed_us,
                    decode_diagnostics,
                    "alpha-bearing media reached an opaque native GPU preview surface".to_owned(),
                );
            }
            let decode_execution = decode_diagnostics.execution_path();
            let presentation_quality =
                match preview_decode_presentation_quality(&decode_diagnostics) {
                    Ok(quality) => quality,
                    Err(reason) => {
                        return media_preview_temporal_failure(
                            job,
                            queue_wait_us,
                            decode_elapsed_us,
                            decode_diagnostics,
                            reason,
                        );
                    }
                };
            let frame_identity = media_preview_frame_identity(
                &job.key,
                MediaPreviewDecodedFrameEvidence::NativeGpu {
                    width: frame.width,
                    height: frame.height,
                    selected_pts: decode_diagnostics.selected_pts,
                    surface_format: frame.surface_format,
                    sampling: decode_diagnostics.decoded_video_sampling,
                    handle_kind: frame.handle_kind(),
                    execution: decode_execution,
                },
            );
            let input_transform = RenderInputTransform::to_working_gpu(
                job.key.working_color_space,
                job.key.input_tone_map,
                job.key.engine.clone(),
            );
            // The sampled extent is the native surface's own representation
            // extent, never an output/composition extent. Scaling to the
            // composition target is owned by the compositor/spatial stage.
            let sampled_resolution =
                mondrian_core::Resolution { width: frame.width, height: frame.height };
            let native_source = MediaPreviewNativeSourceFrame::from_native_frame(
                frame,
                job.key.decode.source_color().color_space,
                input_transform,
            );
            MediaPreviewResult {
                key: job.key,
                frame: Some(
                    MediaPreviewFrame::from_native(
                        native_source,
                        sampled_resolution,
                        logical_resolution,
                        frame_identity,
                        presentation_quality,
                        PreviewDecodeExecutionSummary::from_path(decode_execution),
                    )
                    .with_picture_geometry(picture_geometry)
                    .with_cross_call_reuse(decode_diagnostics.selected_pts.is_some()),
                ),
                error: None,
                failure_reason: None,
                generation: job.generation,
                priority,
                access_mode,
                queue_disposition: MediaPreviewQueueDisposition::Ready,
                queue_wait_us,
                decode_elapsed_us,
                deadline_at,
                logical_cancellation_observed: None,
                canceled: false,
                cancellation_phase: None,
                cancel_reason: None,
                concrete_media_checkpoint: None,
                decode_diagnostics: Some(decode_diagnostics),
                color_diagnostics: None,
                color_stage_diagnostics: None,
                demand_identity,
                execution_id,
                residency_work: job.residency_work,
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
            queue_disposition: MediaPreviewQueueDisposition::Ready,
            queue_wait_us,
            decode_elapsed_us,
            deadline_at,
            logical_cancellation_observed: None,
            canceled: true,
            cancellation_phase: Some(MediaPreviewCancellationPhase::Executing),
            cancel_reason: None,
            concrete_media_checkpoint: Some(cancellation),
            decode_diagnostics: None,
            color_diagnostics: None,
            color_stage_diagnostics: None,
            demand_identity,
            execution_id,
            residency_work: job.residency_work,
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
                queue_disposition: MediaPreviewQueueDisposition::Ready,
                queue_wait_us,
                decode_elapsed_us,
                deadline_at,
                logical_cancellation_observed: None,
                canceled: false,
                cancellation_phase: None,
                cancel_reason: None,
                concrete_media_checkpoint: None,
                decode_diagnostics: None,
                color_diagnostics: None,
                color_stage_diagnostics: None,
                demand_identity,
                execution_id,
                residency_work: job.residency_work,
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
        queue_disposition: MediaPreviewQueueDisposition::Ready,
        queue_wait_us,
        decode_elapsed_us,
        deadline_at: job.deadline_at,
        logical_cancellation_observed: None,
        canceled: false,
        cancellation_phase: None,
        cancel_reason: None,
        concrete_media_checkpoint: None,
        decode_diagnostics: Some(decode_diagnostics),
        color_diagnostics: None,
        color_stage_diagnostics: None,
        demand_identity: job.demand_identity,
        execution_id: job.execution_id,
        residency_work: job.residency_work,
    }
}

fn media_preview_temporal_failure(
    job: MediaPreviewJob,
    queue_wait_us: u64,
    decode_elapsed_us: u64,
    decode_diagnostics: PreviewDecodeDiagnostics,
    reason: MediaPreviewFailureReason,
) -> MediaPreviewResult {
    MediaPreviewResult {
        key: job.key,
        frame: None,
        error: Some(format!(
            "exact {:?} preview request selected a different source timestamp: requested={:?}, selected={:?}",
            decode_diagnostics.access_mode,
            decode_diagnostics.requested_pts,
            decode_diagnostics.selected_pts,
        )),
        failure_reason: Some(reason),
        generation: job.generation,
        priority: job.priority,
        access_mode: job.access_mode,
        queue_disposition: MediaPreviewQueueDisposition::Ready,
        queue_wait_us,
        decode_elapsed_us,
        deadline_at: job.deadline_at,
        logical_cancellation_observed: None,
        canceled: false,
        cancellation_phase: None,
        cancel_reason: None,
        concrete_media_checkpoint: None,
        decode_diagnostics: Some(decode_diagnostics),
        color_diagnostics: None,
        color_stage_diagnostics: None,
        demand_identity: job.demand_identity,
        execution_id: job.execution_id,
        residency_work: job.residency_work,
    }
}

fn media_preview_failure_reason(err: &MondrianError) -> MediaPreviewFailureReason {
    match err {
        MondrianError::DecodeTimeout { .. } => MediaPreviewFailureReason::Timeout,
        MondrianError::DecodeBudgetExhausted { .. } => {
            MediaPreviewFailureReason::ForwardDecodeBudgetExhausted
        }
        MondrianError::DecodeTemporalMismatch { .. } => MediaPreviewFailureReason::TemporalMismatch,
        _ => MediaPreviewFailureReason::DecodeError,
    }
}

fn decode_media_preview_for_access_mode(
    key: &PreviewDecodeKey,
    access_mode: PreviewDecodeAccessMode,
    adaptive_hints: PreviewDecodeAdaptiveHints,
    hardware_decode_request: PreviewHardwareDecodeRequest,
    hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    decode_context: Option<&mut PreviewDecodeSessionContext>,
    should_cancel: impl Fn() -> bool + Send + Sync + 'static,
) -> mondrian_core::Result<PreviewDecodeOutcome> {
    let request = PreviewDecodeRequest::from_key(key, access_mode)
        .with_adaptive_hints(adaptive_hints)
        .with_hardware_decode_request(hardware_decode_request)
        .with_hardware_decode_device_selector(hardware_decode_device_selector);
    match decode_context {
        Some(context) => context.decode_cancellable(request, should_cancel),
        None => decode_preview_frame_cancellable(request, should_cancel),
    }
}

fn app_duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MediaPreviewDecodedFrameEvidence {
    CpuRgba8 {
        width: u32,
        height: u32,
        color_contract: DecodedRgbaFrameContract,
        selected_pts: Option<i64>,
        surface_format: DecodedVideoSurfaceFormat,
        sampling: DecodedVideoSampling,
        execution: PreviewDecodeExecutionPath,
    },
    CpuLinearRgbaF32 {
        width: u32,
        height: u32,
        color_contract: DecodedRgbaFrameContract,
        selected_pts: Option<i64>,
        surface_format: DecodedVideoSurfaceFormat,
        sampling: DecodedVideoSampling,
        execution: PreviewDecodeExecutionPath,
    },
    NativeGpu {
        width: u32,
        height: u32,
        selected_pts: Option<i64>,
        surface_format: DecodedVideoSurfaceFormat,
        sampling: DecodedVideoSampling,
        handle_kind: DecodedGpuFrameHandleKind,
        execution: PreviewDecodeExecutionPath,
    },
}

fn media_preview_frame_identity(
    key: &MediaPreviewKey,
    evidence: MediaPreviewDecodedFrameEvidence,
) -> PreviewSemanticIdentity {
    let mut builder =
        PreviewSemanticIdentityBuilder::new(b"mondrian.preview.decoded-media-frame.v2");
    key.hash(&mut builder);
    evidence.hash(&mut builder);
    builder.finish_identity()
}

#[cfg(test)]
#[path = "preview_media_task/tests.rs"]
mod tests;
