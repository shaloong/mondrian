//! Product adapter and instance-owned worker for Mask motion tracking.
//!
//! Requests freeze exact Clip/source mappings and admitted media revision
//! evidence on the UI thread. The worker owns FFmpeg decode residency and pure
//! Effects analysis. Completed results re-enter authoring only through one
//! revision-checked Sequence transaction.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
#[cfg(any(test, feature = "validation"))]
use std::time::Instant;

use mondrian_core::mask_data::{
    MaskShape, MaskTrackingDirection, MaskTrackingModel, MaskTrackingRecipe, MaskTrackingSettings,
};
use mondrian_core::{
    AssetId, ClipId, ExecutionCancellationToken, FramePosition, FrameRounding, MaskId,
    MediaFileFingerprint, SequenceId, SequenceRevision, SourceSampleTarget, TimelineTime,
    TrackingId,
};
use mondrian_editor_state::AuthoringSessionId;
use mondrian_effects::{
    canonicalize_tracking_shape, track_frame_pair, transform_tracking_shape, TrackingFrame,
    TrackingRegion, TrackingTransform,
};
use mondrian_media::{
    DecodedVideoMatrix, DecodedVideoRangeContract, PreviewDecodeAccessMode, PreviewDecodeOutcome,
    PreviewDecodeRequest, PreviewDecodeSessionContext, PreviewSourceColorContract,
};
use mondrian_timeline::sequence::{InputColorResolutionSource, ResolvedInputColor};
use sha2::{Digest, Sha256};

#[cfg(any(test, feature = "validation"))]
use super::endurance_shutdown::{join_workers_until, EnduranceWorkerShutdownEvidence};
use super::AppState;

const TRACKING_QUEUE_CAPACITY: usize = 4;
const TRACKING_RESULT_CACHE_CAPACITY: usize = 8;
const MAX_TRACKING_FRAMES: usize = 10_000;

/// Stable product-visible phase of one target's latest tracking request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisualTrackingPhase {
    /// Waiting for the dedicated analysis worker.
    Queued,
    /// Decoding and analyzing frame pairs.
    Analyzing,
    /// Cooperative cancellation has been requested.
    Canceling,
    /// Cancellation completed without publishing author state.
    Canceled,
    /// Generated keys were committed as one author transaction.
    Completed,
    /// The attempt failed before author publication.
    Failed,
    /// The immutable request no longer matched current author or media state.
    Stale,
    /// The worker completed without executing analysis by reusing its exact cache.
    CacheHit,
}

/// Bounded Inspector projection for one Mask's latest tracking attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct VisualTrackingStatus {
    /// Stable analysis lineage.
    pub tracking_id: TrackingId,
    /// Current request phase.
    pub phase: VisualTrackingPhase,
    /// Completed adjacent-frame pairs.
    pub completed_pairs: usize,
    /// Total adjacent-frame pairs.
    pub total_pairs: usize,
    /// Bounded failure detail for product presentation.
    pub detail: Option<String>,
}

/// Coherent aggregate diagnostics for the instance-owned Mask tracking worker.
///
/// Gauges describe the exact transport/execution/publication ownership at the
/// instant of the snapshot. Cumulative counters are monotonic for the lifetime
/// of the service and are independent of the bounded per-target status view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisualTrackingDiagnostics {
    /// Accepted jobs occupying the bounded worker transport.
    pub transport_occupied: u64,
    /// Accepted jobs still resident in the bounded worker transport.
    pub queued_transport: u64,
    /// Jobs currently owned by the analysis worker.
    pub running: u64,
    /// Terminal events sent by the worker but not yet consumed by the App.
    pub terminal_results_pending: u64,
    /// Successful outputs awaiting the App's revision-checked publication decision.
    pub awaiting_publication: u64,
    /// Accepted jobs whose terminal event has not yet been consumed.
    pub logical_outstanding: u64,
    /// Jobs successfully admitted to the bounded transport.
    pub admissions: u64,
    /// Successful analysis outputs delivered to the App event stream.
    pub completions: u64,
    /// Successful outputs served by the exact tracking-result cache.
    pub cache_hits: u64,
    /// Worker analysis or terminal-event delivery failures.
    pub failures: u64,
    /// Cooperative cancellations delivered to the App event stream.
    pub cancellations: u64,
    /// Active attempts displaced by a subsequently admitted request for the same target.
    pub superseded: u64,
    /// Successful outputs rejected by the App's publication-time revision checks.
    pub stale: u64,
    /// Requests rejected by the bounded transport or an unavailable worker.
    pub rejections: u64,
    /// Internal ownership-transition inconsistencies retained fail-closed.
    pub accounting_anomalies: u64,
    /// Whether construction attempted to create the dedicated worker.
    pub worker_startup_attempted: bool,
    /// Whether the operating-system worker thread was created.
    pub worker_started: bool,
    /// Whether the worker is currently eligible to accept transport work.
    pub worker_available: bool,
    /// Whether the created worker has exited.
    pub worker_exited: bool,
    /// Worker exits observed before an explicit service shutdown request.
    pub worker_unexpected_exits: u64,
    /// Whether any worker exit occurred before an explicit shutdown request.
    pub worker_unexpectedly_exited: bool,
}

impl VisualTrackingDiagnostics {
    /// Whether the four ownership gauges form one exact outstanding-work partition.
    pub const fn ownership_is_consistent(self) -> bool {
        self.transport_occupied == self.queued_transport
            && self.logical_outstanding
                == self
                    .queued_transport
                    .saturating_add(self.running)
                    .saturating_add(self.terminal_results_pending)
            && self.awaiting_publication <= self.terminal_results_pending
            && self.accounting_anomalies == 0
    }
}

/// Start/recompute admission failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VisualTrackingRequestError {
    /// Transport must be stopped so the authored anchor is stable.
    #[error("tracking cannot start while playback is running")]
    Playing,
    /// Stable Clip/Mask target is missing or locked.
    #[error("tracking target is unavailable or locked: {0}")]
    TargetUnavailable(String),
    /// The target is not coherent file-backed video media.
    #[error("tracking media is unavailable: {0}")]
    MediaUnavailable(String),
    /// Color interpretation rejected analysis input.
    #[error("tracking input color is unavailable: {0}")]
    ColorUnavailable(String),
    /// Exact frame-grid or source-time preparation failed.
    #[error("tracking time range is invalid: {0}")]
    InvalidRange(String),
    /// The bounded worker queue cannot admit more work.
    #[error("tracking worker queue is full")]
    QueueFull,
    /// The dedicated worker is unavailable.
    #[error("tracking worker is unavailable")]
    WorkerUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TrackingTarget {
    clip_id: ClipId,
    mask_id: MaskId,
}

#[derive(Debug, Clone)]
struct TrackingFrameRequest {
    clip_time: TimelineTime,
    source_sample: SourceSampleTarget,
}

#[derive(Debug, Clone)]
struct TrackingBinding {
    session_id: AuthoringSessionId,
    sequence_id: SequenceId,
    sequence_revision: SequenceRevision,
    target: TrackingTarget,
    asset_id: AssetId,
    source_fingerprint: MediaFileFingerprint,
}

#[derive(Debug)]
struct TrackingJob {
    tracking_id: TrackingId,
    binding: TrackingBinding,
    source_path: PathBuf,
    video_stream_index: u32,
    source_color: PreviewSourceColorContract,
    frames: Vec<TrackingFrameRequest>,
    anchor_index: usize,
    initial_shape: MaskShape,
    model: MaskTrackingModel,
    direction: MaskTrackingDirection,
    settings: MaskTrackingSettings,
    cache_key: [u8; 32],
    bypass_cache: bool,
    cancellation: ExecutionCancellationToken,
}

#[derive(Debug, Clone)]
struct TrackingAnalysisOutput {
    generated: Vec<(TimelineTime, MaskShape)>,
    anchor_time: TimelineTime,
    mean_inlier_ratio: f32,
    minimum_inlier_ratio: f32,
}

#[derive(Debug)]
enum TrackingWorkerEvent {
    Started {
        tracking_id: TrackingId,
        target: TrackingTarget,
        total_pairs: usize,
    },
    Progress {
        tracking_id: TrackingId,
        target: TrackingTarget,
        completed_pairs: usize,
        total_pairs: usize,
    },
    Completed {
        tracking_id: TrackingId,
        binding: TrackingBinding,
        model: MaskTrackingModel,
        direction: MaskTrackingDirection,
        settings: MaskTrackingSettings,
        video_stream_index: u32,
        output: TrackingAnalysisOutput,
        cache_hit: bool,
    },
    Canceled {
        tracking_id: TrackingId,
        target: TrackingTarget,
    },
    Failed {
        tracking_id: TrackingId,
        target: TrackingTarget,
        detail: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackingTerminalOutcome {
    Completed { cache_hit: bool },
    Canceled,
    Failed,
}

impl TrackingWorkerEvent {
    fn terminal_outcome(&self) -> Option<TrackingTerminalOutcome> {
        match self {
            Self::Completed { cache_hit, .. } => {
                Some(TrackingTerminalOutcome::Completed { cache_hit: *cache_hit })
            }
            Self::Canceled { .. } => Some(TrackingTerminalOutcome::Canceled),
            Self::Failed { .. } => Some(TrackingTerminalOutcome::Failed),
            Self::Started { .. } | Self::Progress { .. } => None,
        }
    }
}

#[derive(Debug, Default)]
struct VisualTrackingInstrumentation {
    diagnostics: VisualTrackingDiagnostics,
    shutdown_requested: bool,
    mutex_poison_observed: bool,
}

impl VisualTrackingInstrumentation {
    fn snapshot(&self) -> VisualTrackingDiagnostics {
        VisualTrackingDiagnostics {
            transport_occupied: self.diagnostics.queued_transport,
            worker_unexpectedly_exited: self.diagnostics.worker_unexpected_exits != 0,
            ..self.diagnostics
        }
    }

    fn record_startup_attempt(&mut self) {
        self.diagnostics.worker_startup_attempted = true;
    }

    fn record_worker_started(&mut self) {
        self.diagnostics.worker_started = true;
        self.diagnostics.worker_available =
            !self.diagnostics.worker_exited && !self.shutdown_requested;
    }

    fn record_shutdown_requested(&mut self) {
        self.shutdown_requested = true;
        self.diagnostics.worker_available = false;
    }

    fn record_worker_exit(&mut self) {
        self.diagnostics.worker_exited = true;
        self.diagnostics.worker_available = false;
        if !self.shutdown_requested {
            self.diagnostics.worker_unexpected_exits =
                self.diagnostics.worker_unexpected_exits.saturating_add(1);
        }
    }

    fn record_admission(&mut self) {
        self.diagnostics.admissions = self.diagnostics.admissions.saturating_add(1);
        self.diagnostics.queued_transport = self.diagnostics.queued_transport.saturating_add(1);
        self.diagnostics.logical_outstanding =
            self.diagnostics.logical_outstanding.saturating_add(1);
    }

    fn record_rejection(&mut self) {
        self.diagnostics.rejections = self.diagnostics.rejections.saturating_add(1);
    }

    fn record_superseded(&mut self) {
        self.diagnostics.superseded = self.diagnostics.superseded.saturating_add(1);
    }

    fn record_worker_received_job(&mut self) {
        if self.diagnostics.queued_transport == 0 {
            self.diagnostics.accounting_anomalies =
                self.diagnostics.accounting_anomalies.saturating_add(1);
        } else {
            self.diagnostics.queued_transport -= 1;
        }
        self.diagnostics.running = self.diagnostics.running.saturating_add(1);
    }

    fn record_terminal_delivery(&mut self, outcome: TrackingTerminalOutcome, delivered: bool) {
        if self.diagnostics.running == 0 {
            self.diagnostics.accounting_anomalies =
                self.diagnostics.accounting_anomalies.saturating_add(1);
        } else {
            self.diagnostics.running -= 1;
        }
        if delivered {
            self.diagnostics.terminal_results_pending =
                self.diagnostics.terminal_results_pending.saturating_add(1);
            match outcome {
                TrackingTerminalOutcome::Completed { cache_hit } => {
                    self.diagnostics.completions = self.diagnostics.completions.saturating_add(1);
                    self.diagnostics.awaiting_publication =
                        self.diagnostics.awaiting_publication.saturating_add(1);
                    if cache_hit {
                        self.diagnostics.cache_hits = self.diagnostics.cache_hits.saturating_add(1);
                    }
                }
                TrackingTerminalOutcome::Canceled => {
                    self.diagnostics.cancellations =
                        self.diagnostics.cancellations.saturating_add(1);
                }
                TrackingTerminalOutcome::Failed => {
                    self.diagnostics.failures = self.diagnostics.failures.saturating_add(1);
                }
            }
        } else {
            self.diagnostics.failures = self.diagnostics.failures.saturating_add(1);
            if self.diagnostics.logical_outstanding == 0 {
                self.diagnostics.accounting_anomalies =
                    self.diagnostics.accounting_anomalies.saturating_add(1);
            } else {
                self.diagnostics.logical_outstanding -= 1;
            }
        }
    }

    fn record_terminal_consumed(&mut self, outcome: TrackingTerminalOutcome) {
        if self.diagnostics.terminal_results_pending == 0 {
            self.diagnostics.accounting_anomalies =
                self.diagnostics.accounting_anomalies.saturating_add(1);
        } else {
            self.diagnostics.terminal_results_pending -= 1;
        }
        if matches!(outcome, TrackingTerminalOutcome::Completed { .. }) {
            if self.diagnostics.awaiting_publication == 0 {
                self.diagnostics.accounting_anomalies =
                    self.diagnostics.accounting_anomalies.saturating_add(1);
            } else {
                self.diagnostics.awaiting_publication -= 1;
            }
        }
        if self.diagnostics.logical_outstanding == 0 {
            self.diagnostics.accounting_anomalies =
                self.diagnostics.accounting_anomalies.saturating_add(1);
        } else {
            self.diagnostics.logical_outstanding -= 1;
        }
    }

    fn record_stale(&mut self) {
        self.diagnostics.stale = self.diagnostics.stale.saturating_add(1);
    }

    fn record_publication_race_cancellation(&mut self) {
        self.diagnostics.cancellations = self.diagnostics.cancellations.saturating_add(1);
    }
}

fn lock_tracking_instrumentation(
    instrumentation: &Arc<Mutex<VisualTrackingInstrumentation>>,
) -> MutexGuard<'_, VisualTrackingInstrumentation> {
    match instrumentation.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            if !guard.mutex_poison_observed {
                guard.mutex_poison_observed = true;
                guard.diagnostics.accounting_anomalies =
                    guard.diagnostics.accounting_anomalies.saturating_add(1);
            }
            guard
        }
    }
}

struct TrackingWorkerLifecycleGuard {
    instrumentation: Arc<Mutex<VisualTrackingInstrumentation>>,
}

impl Drop for TrackingWorkerLifecycleGuard {
    fn drop(&mut self) {
        lock_tracking_instrumentation(&self.instrumentation).record_worker_exit();
    }
}

struct ActiveTrackingAttempt {
    tracking_id: TrackingId,
    cancellation: ExecutionCancellationToken,
}

pub(crate) struct VisualTrackingService {
    jobs: Option<SyncSender<TrackingJob>>,
    events: Receiver<TrackingWorkerEvent>,
    attempts: HashMap<TrackingTarget, ActiveTrackingAttempt>,
    statuses: HashMap<TrackingTarget, VisualTrackingStatus>,
    worker: Option<JoinHandle<()>>,
    instrumentation: Arc<Mutex<VisualTrackingInstrumentation>>,
}

impl VisualTrackingService {
    pub(crate) fn new() -> Self {
        let (jobs_tx, jobs_rx) = mpsc::sync_channel(TRACKING_QUEUE_CAPACITY);
        let (events_tx, events_rx) = mpsc::channel();
        let instrumentation = Arc::new(Mutex::new(VisualTrackingInstrumentation::default()));
        lock_tracking_instrumentation(&instrumentation).record_startup_attempt();
        let worker_instrumentation = Arc::clone(&instrumentation);
        let worker = std::thread::Builder::new()
            .name("mondrian-visual-tracking".to_owned())
            .spawn(move || tracking_worker(jobs_rx, events_tx, worker_instrumentation))
            .ok();
        if worker.is_some() {
            lock_tracking_instrumentation(&instrumentation).record_worker_started();
        }
        Self {
            jobs: worker.as_ref().map(|_| jobs_tx),
            events: events_rx,
            attempts: HashMap::new(),
            statuses: HashMap::new(),
            worker,
            instrumentation,
        }
    }

    fn request(&mut self, job: TrackingJob) -> Result<TrackingId, VisualTrackingRequestError> {
        let target = job.binding.target;
        let tracking_id = job.tracking_id;
        let cancellation = job.cancellation.clone();
        let total_pairs = job.frames.len().saturating_sub(1);
        let Some(sender) = self.jobs.as_ref() else {
            lock_tracking_instrumentation(&self.instrumentation).record_rejection();
            return Err(VisualTrackingRequestError::WorkerUnavailable);
        };
        let admission = {
            // Holding the diagnostics lock across the non-blocking send prevents
            // the worker from recording a receive before its admission exists.
            let mut instrumentation = lock_tracking_instrumentation(&self.instrumentation);
            let admission = sender.try_send(job);
            match &admission {
                Ok(()) => instrumentation.record_admission(),
                Err(_) => instrumentation.record_rejection(),
            }
            admission
        };
        match admission {
            Ok(()) => {
                if let Some(active) = self.attempts.remove(&target) {
                    active.cancellation.cancel();
                    lock_tracking_instrumentation(&self.instrumentation).record_superseded();
                }
                self.attempts
                    .insert(target, ActiveTrackingAttempt { tracking_id, cancellation });
                self.statuses.insert(
                    target,
                    VisualTrackingStatus {
                        tracking_id,
                        phase: VisualTrackingPhase::Queued,
                        completed_pairs: 0,
                        total_pairs,
                        detail: None,
                    },
                );
                Ok(tracking_id)
            }
            Err(TrySendError::Full(_)) => Err(VisualTrackingRequestError::QueueFull),
            Err(TrySendError::Disconnected(_)) => {
                Err(VisualTrackingRequestError::WorkerUnavailable)
            }
        }
    }

    fn cancel(&mut self, target: TrackingTarget) -> bool {
        let Some(active) = self.attempts.get(&target) else {
            return false;
        };
        active.cancellation.cancel();
        if let Some(status) = self.statuses.get_mut(&target) {
            status.phase = VisualTrackingPhase::Canceling;
        }
        true
    }

    fn status(&self, target: TrackingTarget) -> Option<&VisualTrackingStatus> {
        self.statuses.get(&target)
    }

    fn drain_events(&mut self) -> Vec<TrackingWorkerEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            if let Some(outcome) = event.terminal_outcome() {
                lock_tracking_instrumentation(&self.instrumentation)
                    .record_terminal_consumed(outcome);
            }
            events.push(event);
        }
        events
    }

    fn event_is_current(&self, target: TrackingTarget, tracking_id: TrackingId) -> bool {
        self.attempts
            .get(&target)
            .is_some_and(|active| active.tracking_id == tracking_id)
    }

    fn attempt_was_canceled(&self, target: TrackingTarget, tracking_id: TrackingId) -> bool {
        self.attempts.get(&target).is_some_and(|active| {
            active.tracking_id == tracking_id && active.cancellation.is_canceled()
        })
    }

    fn finish(
        &mut self,
        target: TrackingTarget,
        tracking_id: TrackingId,
        phase: VisualTrackingPhase,
        detail: Option<String>,
    ) {
        if !self.event_is_current(target, tracking_id) {
            return;
        }
        self.attempts.remove(&target);
        if let Some(status) = self.statuses.get_mut(&target) {
            status.phase = phase;
            status.completed_pairs = status.total_pairs;
            status.detail = detail;
        }
    }

    pub(super) fn cancel_all(&mut self) {
        for active in self.attempts.values() {
            active.cancellation.cancel();
        }
        self.attempts.clear();
        self.statuses.clear();
    }

    pub(crate) fn diagnostics(&self) -> VisualTrackingDiagnostics {
        lock_tracking_instrumentation(&self.instrumentation).snapshot()
    }

    pub(crate) fn begin_endurance_shutdown(&mut self) {
        for active in self.attempts.values() {
            active.cancellation.cancel();
        }
        lock_tracking_instrumentation(&self.instrumentation).record_shutdown_requested();
        self.jobs.take();
    }

    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn finish_endurance_shutdown(
        &mut self,
        deadline: Instant,
    ) -> EnduranceWorkerShutdownEvidence {
        self.begin_endurance_shutdown();
        let mut workers = self.worker.take().into_iter().collect::<Vec<_>>();
        let join = join_workers_until(&mut workers, deadline);
        // Drain after the join regardless of its outcome. Terminal failures
        // published during shutdown must enter the cumulative ledger, while
        // unfinished worker ownership remains visible in the gauges below.
        let _ = self.drain_events();
        let diagnostics = self.diagnostics();
        let gauges_closed = diagnostics.ownership_is_consistent()
            && diagnostics.logical_outstanding == 0
            && diagnostics.queued_transport == 0
            && diagnostics.running == 0
            && diagnostics.terminal_results_pending == 0
            && diagnostics.awaiting_publication == 0;
        if join.all_workers_returned_normally() && gauges_closed {
            self.attempts.clear();
            self.statuses.clear();
        }
        let lifecycle_anomaly = !join.all_workers_returned_normally()
            || diagnostics.worker_unexpected_exits != 0
            || diagnostics.accounting_anomalies != 0
            || !gauges_closed;
        let lifecycle_residual = if lifecycle_anomaly { 1 } else { 0 };
        let owned_resources_remaining = diagnostics.logical_outstanding.max(lifecycle_residual);
        let cumulative_failures = diagnostics
            .failures
            .saturating_add(diagnostics.rejections)
            .saturating_add(diagnostics.accounting_anomalies);
        let unexpected_normal_exits = diagnostics
            .worker_unexpected_exits
            .saturating_sub(u64::from(join.panicked_workers()));
        EnduranceWorkerShutdownEvidence::from_join(
            1,
            true,
            join,
            saturating_u64_to_usize(diagnostics.queued_transport),
            saturating_u64_to_usize(diagnostics.running),
            saturating_u64_to_usize(owned_resources_remaining),
            cumulative_failures,
        )
        .with_unexpected_worker_exits(saturating_u64_to_u32(unexpected_normal_exits))
    }
}

impl Default for VisualTrackingService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VisualTrackingService {
    fn drop(&mut self) {
        self.cancel_all();
        self.begin_endurance_shutdown();
        if let Some(worker) = self.worker.take() {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                // Qualification uses the explicit bounded receipt. Ordinary UI
                // teardown must not hang on a foreign analysis Adapter.
                drop(worker);
            }
        }
    }
}

impl AppState {
    /// Start an immutable, bounded Mask tracking request.
    pub fn start_visual_tracking(
        &mut self,
        clip_id: ClipId,
        mask_id: MaskId,
        model: MaskTrackingModel,
        direction: MaskTrackingDirection,
        settings: MaskTrackingSettings,
    ) -> Result<TrackingId, VisualTrackingRequestError> {
        self.request_visual_tracking(clip_id, mask_id, model, direction, settings, false)
    }

    /// Recompute the last completed tracking recipe against the current source revision.
    pub fn recompute_visual_tracking(
        &mut self,
        clip_id: ClipId,
        mask_id: MaskId,
    ) -> Result<TrackingId, VisualTrackingRequestError> {
        let recipe = self
            .active_sequence()
            .and_then(|sequence| find_clip(sequence, clip_id))
            .and_then(|clip| clip.mask(mask_id))
            .and_then(|mask| mask.tracking.clone())
            .ok_or_else(|| {
                VisualTrackingRequestError::TargetUnavailable(
                    "Mask has no completed tracking recipe".to_owned(),
                )
            })?;
        self.request_visual_tracking(
            clip_id,
            mask_id,
            recipe.model,
            recipe.direction,
            recipe.settings,
            true,
        )
    }

    /// Cooperatively cancel one target's queued or running attempt.
    pub fn cancel_visual_tracking(&mut self, clip_id: ClipId, mask_id: MaskId) -> bool {
        self.visual_tracking.cancel(TrackingTarget { clip_id, mask_id })
    }

    /// Latest bounded status for one target.
    pub fn visual_tracking_status(
        &self,
        clip_id: ClipId,
        mask_id: MaskId,
    ) -> Option<&VisualTrackingStatus> {
        self.visual_tracking.status(TrackingTarget { clip_id, mask_id })
    }

    /// Return one coherent aggregate snapshot of Mask tracking worker ownership.
    pub fn visual_tracking_diagnostics(&self) -> VisualTrackingDiagnostics {
        self.visual_tracking.diagnostics()
    }

    fn request_visual_tracking(
        &mut self,
        clip_id: ClipId,
        mask_id: MaskId,
        model: MaskTrackingModel,
        direction: MaskTrackingDirection,
        settings: MaskTrackingSettings,
        bypass_cache: bool,
    ) -> Result<TrackingId, VisualTrackingRequestError> {
        if self.is_playing() {
            return Err(VisualTrackingRequestError::Playing);
        }
        settings
            .validate()
            .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?;
        let session_id = self.authoring_session_id().ok_or_else(|| {
            VisualTrackingRequestError::TargetUnavailable("there is no open Project".to_owned())
        })?;
        let anchor_sequence_time = self
            .current_timeline_time()
            .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?;
        let project_color = self.project_color_environment().clone();
        let (sequence_id, sequence_revision, frame_rate, input_color, clip, initial_shape) = {
            let sequence = self.active_sequence().ok_or_else(|| {
                VisualTrackingRequestError::TargetUnavailable(
                    "there is no active Sequence".to_owned(),
                )
            })?;
            let (track_unlocked, clip) = sequence
                .video_tracks
                .iter()
                .find_map(|track| {
                    track
                        .clips
                        .iter()
                        .find(|clip| clip.id == clip_id)
                        .map(|clip| (!track.is_locked, clip))
                })
                .ok_or_else(|| {
                    VisualTrackingRequestError::TargetUnavailable(
                        "video Clip no longer exists".to_owned(),
                    )
                })?;
            let mask = clip.mask(mask_id).ok_or_else(|| {
                VisualTrackingRequestError::TargetUnavailable(
                    "Mask no longer belongs to the Clip".to_owned(),
                )
            })?;
            if !track_unlocked || mask.locked {
                return Err(VisualTrackingRequestError::TargetUnavailable(
                    "Track or Mask is locked".to_owned(),
                ));
            }
            let anchor_sequence_time = anchor_sequence_time.unwrap_or(sequence.playhead);
            let anchor_sequence_time = anchor_sequence_time.clamp(
                clip.position,
                clip.end_position()
                    .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?,
            );
            let anchor_clip_time = clip
                .timeline_to_clip_time(anchor_sequence_time)
                .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?;
            let initial_shape =
                canonicalize_tracking_shape(&mask.evaluate_at(anchor_clip_time).shape, model);
            let input_color = sequence
                .settings
                .root_program_color_context(&project_color)
                .map_err(|error| VisualTrackingRequestError::ColorUnavailable(error.to_string()))?
                .media_input(sequence.settings.color.input.auto_tone_map_media);
            (
                sequence.id,
                sequence.revision,
                sequence.settings.frame_rate,
                input_color,
                clip.clone(),
                initial_shape,
            )
        };
        let asset_id = clip.media_asset_id().ok_or_else(|| {
            VisualTrackingRequestError::MediaUnavailable(
                "target is generated or nested content, not file-backed media".to_owned(),
            )
        })?;
        let asset = self
            .asset_library()
            .ok_or_else(|| {
                VisualTrackingRequestError::MediaUnavailable(
                    "Project asset library is unavailable".to_owned(),
                )
            })?
            .get_asset(asset_id)
            .map_err(|error| VisualTrackingRequestError::MediaUnavailable(error.to_string()))?
            .ok_or_else(|| {
                VisualTrackingRequestError::MediaUnavailable("Asset does not exist".to_owned())
            })?;
        let source_path = asset.file_path().map(PathBuf::from).ok_or_else(|| {
            VisualTrackingRequestError::MediaUnavailable(
                "Asset has no file-backed source".to_owned(),
            )
        })?;
        let source_fingerprint = asset.source_fingerprint().ok_or_else(|| {
            VisualTrackingRequestError::MediaUnavailable(
                "Asset requires a coherent admitted source revision".to_owned(),
            )
        })?;
        let video =
            asset.media_probe().and_then(|probe| probe.primary_video()).ok_or_else(|| {
                VisualTrackingRequestError::MediaUnavailable(
                    "Asset has no admitted primary video stream".to_owned(),
                )
            })?;
        let interpretation = clip.media_interpretation().ok_or_else(|| {
            VisualTrackingRequestError::MediaUnavailable(
                "Clip has no media interpretation".to_owned(),
            )
        })?;
        let decision = input_color.missing_metadata_policy.resolve_asset_input_decision(
            interpretation.color_space_override,
            asset.interpretation,
            video.executable_color_space(),
            input_color.working_color_space,
        );
        let color_space = match decision.resolved {
            ResolvedInputColor::Color(color_space) => color_space,
            ResolvedInputColor::Data => {
                return Err(VisualTrackingRequestError::ColorUnavailable(
                    "non-color data assets cannot drive picture tracking".to_owned(),
                ));
            }
            ResolvedInputColor::Rejected => {
                return Err(VisualTrackingRequestError::ColorUnavailable(
                    "source color metadata was rejected by Sequence policy".to_owned(),
                ));
            }
        };
        let mut source_color = PreviewSourceColorContract::new(
            color_space,
            DecodedVideoRangeContract::from_interpretation(
                asset.interpretation.range,
                video.color_range,
            ),
        );
        if decision.source == InputColorResolutionSource::MissingPolicyAssumeRec709 {
            source_color = source_color.with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709);
        }
        let current_sequence_time = anchor_sequence_time.unwrap_or(clip.position);
        let (frames, anchor_index) =
            tracking_frame_requests(&clip, frame_rate, current_sequence_time, direction)?;
        let tracking_id = TrackingId::new();
        let cache_key = tracking_cache_key(
            asset_id,
            source_fingerprint,
            video.index,
            &frames,
            anchor_index,
            &initial_shape,
            model,
            direction,
            settings,
        )?;
        let job = TrackingJob {
            tracking_id,
            binding: TrackingBinding {
                session_id,
                sequence_id,
                sequence_revision,
                target: TrackingTarget { clip_id, mask_id },
                asset_id,
                source_fingerprint,
            },
            source_path,
            video_stream_index: video.index,
            source_color,
            frames,
            anchor_index,
            initial_shape,
            model,
            direction,
            settings,
            cache_key,
            bypass_cache,
            cancellation: ExecutionCancellationToken::new(),
        };
        self.visual_tracking.request(job)
    }

    /// Pump worker progress and atomically publish current completions.
    pub fn poll_visual_tracking(&mut self) -> bool {
        let events = self.visual_tracking.drain_events();
        if events.is_empty() {
            return false;
        }
        let mut changed = false;
        for event in events {
            match event {
                TrackingWorkerEvent::Started { tracking_id, target, total_pairs } => {
                    if self.visual_tracking.event_is_current(target, tracking_id)
                        && !self.visual_tracking.attempt_was_canceled(target, tracking_id)
                        && let Some(status) = self.visual_tracking.statuses.get_mut(&target)
                    {
                        status.phase = VisualTrackingPhase::Analyzing;
                        status.total_pairs = total_pairs;
                        changed = true;
                    }
                }
                TrackingWorkerEvent::Progress {
                    tracking_id,
                    target,
                    completed_pairs,
                    total_pairs,
                } => {
                    if self.visual_tracking.event_is_current(target, tracking_id)
                        && !self.visual_tracking.attempt_was_canceled(target, tracking_id)
                        && let Some(status) = self.visual_tracking.statuses.get_mut(&target)
                    {
                        status.phase = VisualTrackingPhase::Analyzing;
                        status.completed_pairs = completed_pairs.min(total_pairs);
                        status.total_pairs = total_pairs;
                        changed = true;
                    }
                }
                TrackingWorkerEvent::Canceled { tracking_id, target } => {
                    self.visual_tracking.finish(
                        target,
                        tracking_id,
                        VisualTrackingPhase::Canceled,
                        None,
                    );
                    changed = true;
                }
                TrackingWorkerEvent::Failed { tracking_id, target, detail } => {
                    self.visual_tracking.finish(
                        target,
                        tracking_id,
                        VisualTrackingPhase::Failed,
                        Some(detail.clone()),
                    );
                    self.set_status_hint(format!("跟踪失败：{detail}"), true);
                    changed = true;
                }
                TrackingWorkerEvent::Completed {
                    tracking_id,
                    binding,
                    model,
                    direction,
                    settings,
                    video_stream_index,
                    output,
                    cache_hit,
                } => {
                    if !self.visual_tracking.event_is_current(binding.target, tracking_id) {
                        continue;
                    }
                    // Cancellation wins even when the worker finished before the UI consumed
                    // its completion event. This closes the otherwise possible publication
                    // race between Cancel and a queued Completed event.
                    if self.visual_tracking.attempt_was_canceled(binding.target, tracking_id) {
                        lock_tracking_instrumentation(&self.visual_tracking.instrumentation)
                            .record_publication_race_cancellation();
                        self.visual_tracking.finish(
                            binding.target,
                            tracking_id,
                            VisualTrackingPhase::Canceled,
                            None,
                        );
                        changed = true;
                        continue;
                    }
                    let publication = self.publish_visual_tracking_result(
                        tracking_id,
                        &binding,
                        model,
                        direction,
                        settings,
                        video_stream_index,
                        output,
                    );
                    match publication {
                        Ok(()) => {
                            self.visual_tracking.finish(
                                binding.target,
                                tracking_id,
                                if cache_hit {
                                    VisualTrackingPhase::CacheHit
                                } else {
                                    VisualTrackingPhase::Completed
                                },
                                None,
                            );
                            self.set_status_hint(
                                if cache_hit {
                                    "跟踪缓存已应用"
                                } else {
                                    "跟踪完成"
                                },
                                false,
                            );
                        }
                        Err(detail) => {
                            lock_tracking_instrumentation(&self.visual_tracking.instrumentation)
                                .record_stale();
                            self.visual_tracking.finish(
                                binding.target,
                                tracking_id,
                                VisualTrackingPhase::Stale,
                                Some(detail),
                            );
                        }
                    }
                    changed = true;
                }
            }
        }
        changed
    }

    fn publish_visual_tracking_result(
        &mut self,
        tracking_id: TrackingId,
        binding: &TrackingBinding,
        model: MaskTrackingModel,
        direction: MaskTrackingDirection,
        settings: MaskTrackingSettings,
        video_stream_index: u32,
        output: TrackingAnalysisOutput,
    ) -> Result<(), String> {
        if self.authoring_session_id() != Some(binding.session_id) {
            return Err("Project Session changed while tracking".to_owned());
        }
        let sequence = self
            .sequences()
            .iter()
            .find(|sequence| sequence.id == binding.sequence_id)
            .ok_or_else(|| "Sequence was removed while tracking".to_owned())?;
        if sequence.revision != binding.sequence_revision {
            return Err("Sequence changed while tracking; recompute is required".to_owned());
        }
        let (track_unlocked, clip) = sequence
            .video_tracks
            .iter()
            .find_map(|track| {
                track
                    .clips
                    .iter()
                    .find(|clip| clip.id == binding.target.clip_id)
                    .map(|clip| (!track.is_locked, clip))
            })
            .ok_or_else(|| "Clip was removed while tracking".to_owned())?;
        let mask = clip
            .mask(binding.target.mask_id)
            .ok_or_else(|| "Mask was removed while tracking".to_owned())?;
        if !track_unlocked || mask.locked {
            return Err("Track or Mask became locked while tracking".to_owned());
        }
        if clip.media_asset_id() != Some(binding.asset_id) {
            return Err("Clip media identity changed while tracking".to_owned());
        }
        let current_fingerprint = self
            .asset_library()
            .and_then(|library| library.get_asset(binding.asset_id).ok().flatten())
            .and_then(|asset| asset.source_fingerprint());
        if current_fingerprint != Some(binding.source_fingerprint) {
            return Err("source media revision changed while tracking".to_owned());
        }
        let range_start = output
            .generated
            .first()
            .map(|entry| entry.0)
            .ok_or_else(|| "tracking result is empty".to_owned())?;
        let range_end = output
            .generated
            .last()
            .map(|entry| entry.0)
            .ok_or_else(|| "tracking result is empty".to_owned())?;
        let anchor_time = output.anchor_time;
        let recipe = MaskTrackingRecipe {
            id: tracking_id,
            model,
            direction,
            anchor_time,
            range_start,
            range_end,
            settings,
            source_fingerprint: binding.source_fingerprint,
            video_stream_index,
            mean_inlier_ratio: output.mean_inlier_ratio,
            minimum_observed_inlier_ratio: output.minimum_inlier_ratio,
        };
        let generated = output.generated;
        self.commit_sequence_edit(binding.sequence_id, "应用蒙版跟踪", |sequence| {
            let clip = sequence
                .video_tracks
                .iter_mut()
                .flat_map(|track| &mut track.clips)
                .find(|clip| clip.id == binding.target.clip_id)
                .ok_or_else(|| mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "visual_tracking_publish".to_owned(),
                    reason: "Clip no longer exists".to_owned(),
                })?;
            let changed =
                clip.apply_mask_tracking_result(binding.target.mask_id, recipe, generated)?;
            if changed {
                Ok(())
            } else {
                Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "visual_tracking_publish".to_owned(),
                    reason: "tracking result is a semantic no-op".to_owned(),
                })
            }
        })
        .map_err(|error| error.to_string())
    }
}

fn find_clip(
    sequence: &mondrian_timeline::Sequence,
    clip_id: ClipId,
) -> Option<&mondrian_timeline::Clip> {
    sequence
        .video_tracks
        .iter()
        .flat_map(|track| &track.clips)
        .find(|clip| clip.id == clip_id)
}

fn tracking_frame_requests(
    clip: &mondrian_timeline::Clip,
    frame_rate: mondrian_core::Rational,
    anchor_sequence_time: TimelineTime,
    direction: MaskTrackingDirection,
) -> Result<(Vec<TrackingFrameRequest>, usize), VisualTrackingRequestError> {
    let first_frame = clip
        .position
        .to_frame_position(frame_rate, FrameRounding::Ceil)
        .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?
        .frame;
    let end = clip
        .end_position()
        .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?;
    let end_frame = end
        .to_frame_position(frame_rate, FrameRounding::Ceil)
        .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?
        .frame;
    let mut anchor_frame = anchor_sequence_time
        .to_frame_position(frame_rate, FrameRounding::Nearest)
        .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?
        .frame;
    anchor_frame = anchor_frame.clamp(first_frame, end_frame.saturating_sub(1));
    let (selected_start, selected_end) = match direction {
        MaskTrackingDirection::Forward => (anchor_frame, end_frame),
        MaskTrackingDirection::Backward => (first_frame, anchor_frame.saturating_add(1)),
        MaskTrackingDirection::Both => (first_frame, end_frame),
    };
    let count = selected_end.saturating_sub(selected_start) as usize;
    if count == 0 || count > MAX_TRACKING_FRAMES {
        return Err(VisualTrackingRequestError::InvalidRange(format!(
            "tracking range must contain 1..={MAX_TRACKING_FRAMES} frames"
        )));
    }
    let mut frames = Vec::with_capacity(count);
    for frame in selected_start..selected_end {
        let sequence_time = TimelineTime::from_frame_position(FramePosition::new(
            frame,
            mondrian_core::Rational::new(frame_rate.den, frame_rate.num),
        ))
        .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?;
        if sequence_time < clip.position || sequence_time >= end {
            continue;
        }
        frames.push(TrackingFrameRequest {
            clip_time: clip
                .timeline_to_clip_time(sequence_time)
                .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?,
            source_sample: clip
                .timeline_to_source_sample(sequence_time)
                .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?,
        });
    }
    let anchor_index = frames
        .iter()
        .position(|request| {
            clip.clip_to_timeline_time(request.clip_time)
                .ok()
                .and_then(|time| time.to_frame_position(frame_rate, FrameRounding::Nearest).ok())
                .is_some_and(|position| position.frame == anchor_frame)
        })
        .ok_or_else(|| {
            VisualTrackingRequestError::InvalidRange(
                "anchor frame is outside the visible Clip range".to_owned(),
            )
        })?;
    Ok((frames, anchor_index))
}

fn tracking_cache_key(
    asset_id: AssetId,
    fingerprint: MediaFileFingerprint,
    video_stream_index: u32,
    frames: &[TrackingFrameRequest],
    anchor_index: usize,
    shape: &MaskShape,
    model: MaskTrackingModel,
    direction: MaskTrackingDirection,
    settings: MaskTrackingSettings,
) -> Result<[u8; 32], VisualTrackingRequestError> {
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.visual-tracking.v1\0");
    hasher.update(asset_id.to_string().as_bytes());
    hasher.update(
        serde_json::to_vec(&fingerprint)
            .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?,
    );
    hasher.update(video_stream_index.to_le_bytes());
    hasher.update(anchor_index.to_le_bytes());
    for frame in frames {
        hasher.update(
            serde_json::to_vec(&(frame.clip_time, frame.source_sample))
                .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?,
        );
    }
    hasher.update(
        serde_json::to_vec(&(shape, model, direction, settings))
            .map_err(|error| VisualTrackingRequestError::InvalidRange(error.to_string()))?,
    );
    Ok(hasher.finalize().into())
}

#[cfg(any(test, feature = "validation"))]
fn saturating_u64_to_usize(value: u64) -> usize {
    match usize::try_from(value) {
        Ok(value) => value,
        Err(_) => usize::MAX,
    }
}

#[cfg(any(test, feature = "validation"))]
fn saturating_u64_to_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn publish_tracking_terminal_event(
    events: &mpsc::Sender<TrackingWorkerEvent>,
    instrumentation: &Arc<Mutex<VisualTrackingInstrumentation>>,
    event: TrackingWorkerEvent,
) {
    let Some(outcome) = event.terminal_outcome() else {
        let mut instrumentation = lock_tracking_instrumentation(instrumentation);
        instrumentation.diagnostics.accounting_anomalies =
            instrumentation.diagnostics.accounting_anomalies.saturating_add(1);
        return;
    };
    // The unbounded event send cannot wait on the App. Keeping the ownership
    // lock across it orders terminal publication before any receiver-side
    // consumption can update the same aggregate gauges.
    let mut instrumentation = lock_tracking_instrumentation(instrumentation);
    let delivered = events.send(event).is_ok();
    instrumentation.record_terminal_delivery(outcome, delivered);
}

fn tracking_worker(
    jobs: Receiver<TrackingJob>,
    events: mpsc::Sender<TrackingWorkerEvent>,
    instrumentation: Arc<Mutex<VisualTrackingInstrumentation>>,
) {
    let _lifecycle = TrackingWorkerLifecycleGuard { instrumentation: Arc::clone(&instrumentation) };
    let mut decode_context = PreviewDecodeSessionContext::new();
    let mut cache: VecDeque<([u8; 32], TrackingAnalysisOutput)> = VecDeque::new();
    while let Ok(job) = jobs.recv() {
        lock_tracking_instrumentation(&instrumentation).record_worker_received_job();
        let target = job.binding.target;
        if job.cancellation.is_canceled() {
            publish_tracking_terminal_event(
                &events,
                &instrumentation,
                TrackingWorkerEvent::Canceled { tracking_id: job.tracking_id, target },
            );
            continue;
        }
        let total_pairs = job.frames.len().saturating_sub(1);
        let _ = events.send(TrackingWorkerEvent::Started {
            tracking_id: job.tracking_id,
            target,
            total_pairs,
        });
        if !job.bypass_cache
            && let Some((_, output)) = cache.iter().find(|(key, _)| *key == job.cache_key)
        {
            publish_tracking_terminal_event(
                &events,
                &instrumentation,
                TrackingWorkerEvent::Completed {
                    tracking_id: job.tracking_id,
                    binding: job.binding,
                    model: job.model,
                    direction: job.direction,
                    settings: job.settings,
                    video_stream_index: job.video_stream_index,
                    output: output.clone(),
                    cache_hit: true,
                },
            );
            continue;
        }
        match analyze_tracking_job(&job, &mut decode_context, &events) {
            Ok(output) => {
                cache.retain(|(key, _)| *key != job.cache_key);
                cache.push_back((job.cache_key, output.clone()));
                while cache.len() > TRACKING_RESULT_CACHE_CAPACITY {
                    cache.pop_front();
                }
                publish_tracking_terminal_event(
                    &events,
                    &instrumentation,
                    TrackingWorkerEvent::Completed {
                        tracking_id: job.tracking_id,
                        binding: job.binding,
                        model: job.model,
                        direction: job.direction,
                        settings: job.settings,
                        video_stream_index: job.video_stream_index,
                        output,
                        cache_hit: false,
                    },
                );
            }
            Err(_detail) if job.cancellation.is_canceled() => {
                publish_tracking_terminal_event(
                    &events,
                    &instrumentation,
                    TrackingWorkerEvent::Canceled { tracking_id: job.tracking_id, target },
                );
            }
            Err(detail) => {
                publish_tracking_terminal_event(
                    &events,
                    &instrumentation,
                    TrackingWorkerEvent::Failed { tracking_id: job.tracking_id, target, detail },
                );
            }
        }
    }
    decode_context.clear();
}

fn analyze_tracking_job(
    job: &TrackingJob,
    decode_context: &mut PreviewDecodeSessionContext,
    events: &mpsc::Sender<TrackingWorkerEvent>,
) -> Result<TrackingAnalysisOutput, String> {
    let anchor = decode_tracking_frame(job, job.anchor_index, decode_context)?;
    let mut generated = vec![(
        job.frames[job.anchor_index].clip_time,
        job.initial_shape.clone(),
    )];
    let mut quality = Vec::with_capacity(job.frames.len().saturating_sub(1));
    let mut completed_pairs = 0;
    if matches!(
        job.direction,
        MaskTrackingDirection::Forward | MaskTrackingDirection::Both
    ) {
        analyze_direction(
            job,
            job.anchor_index..job.frames.len().saturating_sub(1),
            true,
            anchor.clone(),
            decode_context,
            &mut generated,
            &mut quality,
            &mut completed_pairs,
            events,
        )?;
    }
    if matches!(
        job.direction,
        MaskTrackingDirection::Backward | MaskTrackingDirection::Both
    ) {
        analyze_direction(
            job,
            (1..=job.anchor_index).rev(),
            false,
            anchor,
            decode_context,
            &mut generated,
            &mut quality,
            &mut completed_pairs,
            events,
        )?;
    }
    generated.sort_by_key(|entry| entry.0);
    let (mean_inlier_ratio, minimum_inlier_ratio) = if quality.is_empty() {
        (1.0, 1.0)
    } else {
        (
            quality.iter().sum::<f32>() / quality.len() as f32,
            quality.iter().copied().fold(1.0_f32, f32::min),
        )
    };
    Ok(TrackingAnalysisOutput {
        generated,
        anchor_time: job.frames[job.anchor_index].clip_time,
        mean_inlier_ratio,
        minimum_inlier_ratio,
    })
}

#[allow(clippy::too_many_arguments)]
fn analyze_direction(
    job: &TrackingJob,
    indices: impl Iterator<Item = usize>,
    forward: bool,
    mut reference_frame: TrackingFrame,
    decode_context: &mut PreviewDecodeSessionContext,
    generated: &mut Vec<(TimelineTime, MaskShape)>,
    quality: &mut Vec<f32>,
    completed_pairs: &mut usize,
    events: &mpsc::Sender<TrackingWorkerEvent>,
) -> Result<(), String> {
    let mut accumulated = TrackingTransform::IDENTITY;
    let mut reference_shape = job.initial_shape.clone();
    for index in indices {
        if job.cancellation.is_canceled() {
            return Err("tracking canceled".to_owned());
        }
        let candidate_index = if forward { index + 1 } else { index - 1 };
        let candidate_frame = decode_tracking_frame(job, candidate_index, decode_context)?;
        let region =
            TrackingRegion::from_shape(&reference_shape).map_err(|error| error.to_string())?;
        let observation = track_frame_pair(
            &reference_frame,
            &candidate_frame,
            region,
            job.model,
            job.settings,
            {
                let cancellation = job.cancellation.clone();
                move || cancellation.is_canceled()
            },
        )
        .map_err(|error| format!("frame pair {index}->{candidate_index}: {error}"))?;
        accumulated = observation
            .transform
            .compose_after(accumulated)
            .map_err(|error| error.to_string())?;
        let shape = transform_tracking_shape(&job.initial_shape, accumulated, job.model)
            .map_err(|error| error.to_string())?;
        reference_shape = shape.clone();
        reference_frame = candidate_frame;
        generated.push((job.frames[candidate_index].clip_time, shape));
        quality.push(observation.quality.inlier_ratio);
        *completed_pairs = completed_pairs.saturating_add(1);
        let _ = events.send(TrackingWorkerEvent::Progress {
            tracking_id: job.tracking_id,
            target: job.binding.target,
            completed_pairs: *completed_pairs,
            total_pairs: job.frames.len().saturating_sub(1),
        });
    }
    Ok(())
}

fn decode_tracking_frame(
    job: &TrackingJob,
    index: usize,
    decode_context: &mut PreviewDecodeSessionContext,
) -> Result<TrackingFrame, String> {
    let request = PreviewDecodeRequest::new(
        &job.source_path,
        job.frames[index].source_sample,
        PreviewDecodeAccessMode::RandomAccessStillFrame,
        job.source_color,
    )
    .with_video_stream_index(job.video_stream_index)
    .with_max_size(
        Some(job.settings.analysis_max_dimension),
        Some(job.settings.analysis_max_dimension),
    )
    .with_fingerprint(job.binding.source_fingerprint);
    let cancellation = job.cancellation.clone();
    match decode_context.decode_cancellable(request, move || cancellation.is_canceled()) {
        Ok(PreviewDecodeOutcome::Frame(frame)) => {
            TrackingFrame::from_rgba8(frame.width, frame.height, frame.rgba())
                .map_err(|error| error.to_string())
        }
        Ok(PreviewDecodeOutcome::FloatFrame(frame)) => {
            TrackingFrame::from_rgba_f32(frame.width, frame.height, frame.rgba())
                .map_err(|error| error.to_string())
        }
        Ok(PreviewDecodeOutcome::Canceled(_)) => Err("tracking decode canceled".to_owned()),
        Ok(PreviewDecodeOutcome::CpuYuvFrame(_)) => {
            Err("tracking requested CPU RGBA but decoder returned CPU YUV".to_owned())
        }
        Ok(PreviewDecodeOutcome::NativeGpuFrame(_)) => {
            Err("tracking requested CPU RGBA but decoder returned a GPU frame".to_owned())
        }
        Err(error) => Err(format!("tracking decode failed: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use mondrian_assets::{AssetLibrary, AssetMediaProbeCandidate};
    use mondrian_core::mask_data::{MaskComponent, MaskEvaluation};
    use mondrian_timeline::{Clip, Sequence};

    use super::*;

    fn generate_translation_fixture(root: &std::path::Path) -> Option<PathBuf> {
        let mut availability =
            mondrian_media::ffmpeg_command().expect("admit tracking fixture probe");
        let development_search_path = availability.get_program() == std::ffi::OsStr::new("ffmpeg");
        match availability.arg("-version").output() {
            Err(error)
                if development_search_path && error.kind() == std::io::ErrorKind::NotFound =>
            {
                return None
            }
            Err(error) => panic!("tracking fixture version probe could not execute: {error}"),
            Ok(output) => assert!(
                output.status.success(),
                "tracking fixture version probe failed: {output:?}"
            ),
        }
        let frames = root.join("frames");
        std::fs::create_dir_all(&frames).expect("create tracking frames");
        for frame_index in 0..5_u32 {
            let mut image = image::RgbaImage::from_pixel(160, 120, image::Rgba([12, 12, 12, 255]));
            let dx = frame_index * 2;
            for y in 24..88_u32 {
                for x in 28..108_u32 {
                    let block_x = x / 3;
                    let block_y = y / 3;
                    let mut hash =
                        block_x.wrapping_mul(0x9e37_79b9) ^ block_y.wrapping_mul(0x85eb_ca6b);
                    hash ^= hash >> 16;
                    let value = 35_u8.saturating_add((hash & 0xb7) as u8);
                    image.put_pixel(x + dx, y, image::Rgba([value, value, value, 255]));
                }
            }
            image
                .save(frames.join(format!("frame{frame_index:03}.png")))
                .expect("write tracking source frame");
        }
        let video = root.join("translation.mp4");
        let status = mondrian_media::ffmpeg_command()
            .expect("admit tracking fixture encoder")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-framerate",
                "10",
            ])
            .arg("-i")
            .arg(frames.join("frame%03d.png"))
            .args([
                "-an",
                "-c:v",
                "mpeg4",
                "-q:v",
                "2",
                "-color_range",
                "tv",
                "-colorspace",
                "bt709",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
            ])
            .arg(&video)
            .status()
            .expect("run ffmpeg tracking fixture encoder");
        status.success().then_some(video)
    }

    fn wait_for_terminal(state: &mut AppState, clip_id: ClipId, mask_id: MaskId) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            state.poll_visual_tracking();
            let phase = state.visual_tracking_status(clip_id, mask_id).map(|status| status.phase);
            if phase.is_some_and(|phase| {
                matches!(
                    phase,
                    VisualTrackingPhase::Completed
                        | VisualTrackingPhase::CacheHit
                        | VisualTrackingPhase::Canceled
                        | VisualTrackingPhase::Failed
                        | VisualTrackingPhase::Stale
                )
            }) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "tracking attempt timed out: {phase:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn started_instrumentation() -> Arc<Mutex<VisualTrackingInstrumentation>> {
        let instrumentation = Arc::new(Mutex::new(VisualTrackingInstrumentation::default()));
        {
            let mut state = lock_tracking_instrumentation(&instrumentation);
            state.record_startup_attempt();
            state.record_worker_started();
        }
        instrumentation
    }

    #[test]
    fn aggregate_diagnostics_partition_idle_queue_running_terminal_and_consumed() {
        let instrumentation = started_instrumentation();
        let idle = lock_tracking_instrumentation(&instrumentation).snapshot();
        assert!(idle.worker_startup_attempted);
        assert!(idle.worker_started);
        assert!(idle.worker_available);
        assert!(idle.ownership_is_consistent());
        assert_eq!(idle.logical_outstanding, 0);

        {
            let mut state = lock_tracking_instrumentation(&instrumentation);
            state.record_admission();
        }
        let queued = lock_tracking_instrumentation(&instrumentation).snapshot();
        assert_eq!(queued.transport_occupied, 1);
        assert_eq!(queued.queued_transport, 1);
        assert_eq!(queued.running, 0);
        assert_eq!(queued.logical_outstanding, 1);
        assert_eq!(queued.admissions, 1);
        assert!(queued.ownership_is_consistent());

        {
            let mut state = lock_tracking_instrumentation(&instrumentation);
            state.record_worker_received_job();
        }
        let running = lock_tracking_instrumentation(&instrumentation).snapshot();
        assert_eq!(running.transport_occupied, 0);
        assert_eq!(running.running, 1);
        assert_eq!(running.logical_outstanding, 1);
        assert!(running.ownership_is_consistent());

        {
            let mut state = lock_tracking_instrumentation(&instrumentation);
            state.record_terminal_delivery(
                TrackingTerminalOutcome::Completed { cache_hit: true },
                true,
            );
        }
        let terminal = lock_tracking_instrumentation(&instrumentation).snapshot();
        assert_eq!(terminal.running, 0);
        assert_eq!(terminal.terminal_results_pending, 1);
        assert_eq!(terminal.awaiting_publication, 1);
        assert_eq!(terminal.logical_outstanding, 1);
        assert_eq!(terminal.completions, 1);
        assert_eq!(terminal.cache_hits, 1);
        assert!(terminal.ownership_is_consistent());

        {
            let mut state = lock_tracking_instrumentation(&instrumentation);
            state.record_terminal_consumed(TrackingTerminalOutcome::Completed { cache_hit: true });
        }
        let consumed = lock_tracking_instrumentation(&instrumentation).snapshot();
        assert_eq!(consumed.logical_outstanding, 0);
        assert_eq!(consumed.terminal_results_pending, 0);
        assert_eq!(consumed.awaiting_publication, 0);
        assert_eq!(consumed.completions, 1);
        assert_eq!(consumed.cache_hits, 1);
        assert!(consumed.ownership_is_consistent());
    }

    #[test]
    fn cumulative_diagnostics_do_not_depend_on_status_or_cache_retention() {
        let instrumentation = started_instrumentation();
        {
            let mut state = lock_tracking_instrumentation(&instrumentation);
            state.record_admission();
            state.record_worker_received_job();
            state.record_terminal_delivery(TrackingTerminalOutcome::Failed, true);
            state.record_terminal_consumed(TrackingTerminalOutcome::Failed);
            state.record_rejection();
            state.record_superseded();
            state.record_stale();
        }
        let first = lock_tracking_instrumentation(&instrumentation).snapshot();
        assert_eq!(first.admissions, 1);
        assert_eq!(first.failures, 1);
        assert_eq!(first.rejections, 1);
        assert_eq!(first.superseded, 1);
        assert_eq!(first.stale, 1);
        assert_eq!(first.logical_outstanding, 0);
        assert!(first.ownership_is_consistent());

        {
            let mut state = lock_tracking_instrumentation(&instrumentation);
            state.record_admission();
            state.record_worker_received_job();
            state.record_terminal_delivery(TrackingTerminalOutcome::Canceled, true);
            state.record_terminal_consumed(TrackingTerminalOutcome::Canceled);
        }
        let second = lock_tracking_instrumentation(&instrumentation).snapshot();
        assert_eq!(second.admissions, 2);
        assert_eq!(second.failures, 1);
        assert_eq!(second.cancellations, 1);
        assert_eq!(second.rejections, 1);
        assert_eq!(second.superseded, 1);
        assert_eq!(second.stale, 1);
        assert!(second.ownership_is_consistent());
    }

    #[test]
    fn endurance_shutdown_drains_terminal_failure_and_preserves_its_ledger() {
        let instrumentation = started_instrumentation();
        {
            let mut state = lock_tracking_instrumentation(&instrumentation);
            state.record_admission();
            state.record_worker_received_job();
        }
        let (jobs_tx, jobs_rx) = mpsc::sync_channel::<TrackingJob>(1);
        let (events_tx, events_rx) = mpsc::channel();
        let worker_instrumentation = Arc::clone(&instrumentation);
        let worker = std::thread::spawn(move || {
            let _lifecycle = TrackingWorkerLifecycleGuard {
                instrumentation: Arc::clone(&worker_instrumentation),
            };
            let _ = jobs_rx.recv();
            publish_tracking_terminal_event(
                &events_tx,
                &worker_instrumentation,
                TrackingWorkerEvent::Failed {
                    tracking_id: TrackingId::new(),
                    target: TrackingTarget { clip_id: ClipId::new(), mask_id: MaskId::new() },
                    detail: "injected shutdown failure".to_owned(),
                },
            );
        });
        let mut service = VisualTrackingService {
            jobs: Some(jobs_tx),
            events: events_rx,
            attempts: HashMap::new(),
            statuses: HashMap::new(),
            worker: Some(worker),
            instrumentation,
        };

        let evidence = service.finish_endurance_shutdown(Instant::now() + Duration::from_secs(1));
        let diagnostics = service.diagnostics();
        assert_eq!(evidence.terminated_workers, 1);
        assert_eq!(evidence.cumulative_failures, 1);
        assert_eq!(evidence.queued_work_remaining, 0);
        assert_eq!(evidence.running_work_remaining, 0);
        assert_eq!(evidence.owned_resources_remaining, 0);
        assert_eq!(diagnostics.failures, 1);
        assert_eq!(diagnostics.logical_outstanding, 0);
        assert_eq!(diagnostics.terminal_results_pending, 0);
        assert!(!diagnostics.worker_available);
        assert!(diagnostics.worker_exited);
        assert!(!diagnostics.worker_unexpectedly_exited);
        assert!(diagnostics.ownership_is_consistent());
    }

    #[test]
    fn endurance_shutdown_receipt_keeps_rejections_in_the_cumulative_ledger() {
        let instrumentation = started_instrumentation();
        {
            let mut state = lock_tracking_instrumentation(&instrumentation);
            state.diagnostics.failures = 2;
            state.diagnostics.rejections = 3;
            state.diagnostics.accounting_anomalies = 4;
        }
        let (jobs_tx, jobs_rx) = mpsc::sync_channel::<TrackingJob>(1);
        let (_events_tx, events_rx) = mpsc::channel();
        let worker_instrumentation = Arc::clone(&instrumentation);
        let worker = std::thread::spawn(move || {
            let _lifecycle =
                TrackingWorkerLifecycleGuard { instrumentation: worker_instrumentation };
            let _ = jobs_rx.recv();
        });
        let mut service = VisualTrackingService {
            jobs: Some(jobs_tx),
            events: events_rx,
            attempts: HashMap::new(),
            statuses: HashMap::new(),
            worker: Some(worker),
            instrumentation,
        };

        let evidence = service.finish_endurance_shutdown(Instant::now() + Duration::from_secs(1));

        assert_eq!(evidence.cumulative_failures, 9);
    }

    #[test]
    fn panic_and_timeout_shutdowns_retain_outstanding_worker_ownership() {
        let panic_instrumentation = started_instrumentation();
        {
            let mut state = lock_tracking_instrumentation(&panic_instrumentation);
            state.record_admission();
            state.record_worker_received_job();
        }
        let (panic_jobs_tx, panic_jobs_rx) = mpsc::sync_channel::<TrackingJob>(1);
        let (_panic_events_tx, panic_events_rx) = mpsc::channel();
        let panic_worker_instrumentation = Arc::clone(&panic_instrumentation);
        let panic_worker = std::thread::spawn(move || {
            let _lifecycle =
                TrackingWorkerLifecycleGuard { instrumentation: panic_worker_instrumentation };
            let _ = panic_jobs_rx.recv();
            panic!("injected tracking worker panic");
        });
        let mut panic_service = VisualTrackingService {
            jobs: Some(panic_jobs_tx),
            events: panic_events_rx,
            attempts: HashMap::new(),
            statuses: HashMap::new(),
            worker: Some(panic_worker),
            instrumentation: panic_instrumentation,
        };
        let panic_evidence =
            panic_service.finish_endurance_shutdown(Instant::now() + Duration::from_secs(1));
        assert_eq!(panic_evidence.panicked_workers, 1);
        assert_eq!(panic_evidence.running_work_remaining, 1);
        assert_eq!(panic_evidence.owned_resources_remaining, 1);
        assert!(!panic_evidence.all_workers_terminated());

        let timeout_instrumentation = started_instrumentation();
        {
            let mut state = lock_tracking_instrumentation(&timeout_instrumentation);
            state.record_admission();
            state.record_worker_received_job();
        }
        let release = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let (timeout_jobs_tx, timeout_jobs_rx) = mpsc::sync_channel::<TrackingJob>(1);
        let (_timeout_events_tx, timeout_events_rx) = mpsc::channel();
        let timeout_worker_instrumentation = Arc::clone(&timeout_instrumentation);
        let timeout_worker = std::thread::spawn(move || {
            let _lifecycle =
                TrackingWorkerLifecycleGuard { instrumentation: timeout_worker_instrumentation };
            let _ = timeout_jobs_rx.recv();
            while !worker_release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        });
        let mut timeout_service = VisualTrackingService {
            jobs: Some(timeout_jobs_tx),
            events: timeout_events_rx,
            attempts: HashMap::new(),
            statuses: HashMap::new(),
            worker: Some(timeout_worker),
            instrumentation: Arc::clone(&timeout_instrumentation),
        };
        let timeout_evidence = timeout_service.finish_endurance_shutdown(Instant::now());
        assert_eq!(timeout_evidence.timed_out_workers, 1);
        assert_eq!(timeout_evidence.detached_workers, 1);
        assert_eq!(timeout_evidence.running_work_remaining, 1);
        assert_eq!(timeout_evidence.owned_resources_remaining, 1);
        assert!(!timeout_evidence.all_workers_terminated());
        release.store(true, Ordering::Release);
        let exit_deadline = Instant::now() + Duration::from_secs(1);
        while !lock_tracking_instrumentation(&timeout_instrumentation).snapshot().worker_exited
            && Instant::now() < exit_deadline
        {
            std::thread::yield_now();
        }
        assert!(lock_tracking_instrumentation(&timeout_instrumentation).snapshot().worker_exited);
    }

    #[test]
    fn real_media_tracking_is_atomic_undoable_cacheable_cancelable_and_stale_safe() {
        let root = tempfile::tempdir().expect("tracking fixture root");
        let Some(video_path) = generate_translation_fixture(root.path()) else {
            return;
        };
        let canonical_path = std::fs::canonicalize(&video_path).expect("canonical video path");
        let media_info = mondrian_media::probe_media_info(&canonical_path).expect("probe fixture");
        let fingerprint = MediaFileFingerprint::capture(&canonical_path);
        assert!(fingerprint.authorizes_reuse());
        let library = AssetLibrary::open(root.path().join("library")).expect("open Asset Library");
        let asset_id = library
            .commit_media_probe(
                AssetMediaProbeCandidate::new(canonical_path, fingerprint, media_info)
                    .expect("admit media probe"),
                None,
            )
            .expect("commit media probe");

        let mut sequence = Sequence::new("Tracking integration");
        sequence.settings.frame_rate = mondrian_core::Rational::FPS_10;
        sequence.add_video_track();
        let mut clip = Clip::new(
            asset_id,
            TimelineTime::ZERO,
            TimelineTime::new(1, 2).expect("five frames"),
        )
        .expect("media Clip");
        let mask = MaskComponent::new(
            "Tracked Window".to_owned(),
            MaskEvaluation {
                shape: MaskShape::Rectangle {
                    x: 0.12,
                    y: 0.14,
                    width: 0.68,
                    height: 0.68,
                    corner_radius: 0.0,
                },
                ..Default::default()
            },
        );
        let mask_id = mask.id;
        clip.add_mask_component(mask);
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("place Clip");

        let mut state = AppState::new();
        state.test_set_asset_library(Some(library));
        state.test_set_sequence(Some(sequence));
        state.seek(0).expect("anchor first frame");
        let settings = MaskTrackingSettings {
            analysis_max_dimension: 160,
            max_features: 160,
            search_radius: 8,
            patch_radius: 4,
            minimum_inlier_ratio: 0.45,
        };
        let tracking_id = state
            .start_visual_tracking(
                clip_id,
                mask_id,
                MaskTrackingModel::ObjectTranslation,
                MaskTrackingDirection::Forward,
                settings,
            )
            .expect("start tracking");
        wait_for_terminal(&mut state, clip_id, mask_id);
        assert_eq!(
            state.visual_tracking_status(clip_id, mask_id).map(|status| status.phase),
            Some(VisualTrackingPhase::Completed)
        );
        let tracked = find_clip(state.active_sequence().expect("Sequence"), clip_id)
            .and_then(|clip| clip.mask(mask_id))
            .expect("tracked Mask");
        assert_eq!(tracked.shape_keyframes.len(), 5);
        assert_eq!(
            tracked.tracking.as_ref().map(|recipe| recipe.id),
            Some(tracking_id)
        );
        let first_x = match &tracked.shape_keyframes[0].shape {
            MaskShape::Rectangle { x, .. } => *x,
            _ => panic!("object tracking must preserve Rectangle geometry"),
        };
        let last_x = match &tracked.shape_keyframes[4].shape {
            MaskShape::Rectangle { x, .. } => *x,
            _ => panic!("object tracking must preserve Rectangle geometry"),
        };
        assert!(
            last_x > first_x + 0.025,
            "tracked motion was too small: {first_x}->{last_x}"
        );

        assert!(state.undo_timeline().expect("undo tracking"));
        let undone = find_clip(state.active_sequence().expect("Sequence"), clip_id)
            .and_then(|clip| clip.mask(mask_id))
            .expect("undone Mask");
        assert_eq!(undone.shape_keyframes.len(), 1);
        assert!(undone.tracking.is_none());
        assert!(state.redo_timeline().expect("redo tracking"));

        state.seek(0).expect("restore anchor");
        state
            .start_visual_tracking(
                clip_id,
                mask_id,
                MaskTrackingModel::ObjectTranslation,
                MaskTrackingDirection::Forward,
                settings,
            )
            .expect("start cacheable tracking");
        wait_for_terminal(&mut state, clip_id, mask_id);
        assert_eq!(
            state.visual_tracking_status(clip_id, mask_id).map(|status| status.phase),
            Some(VisualTrackingPhase::CacheHit)
        );

        let before_cancel = state.active_sequence().expect("Sequence").clone();
        state
            .start_visual_tracking(
                clip_id,
                mask_id,
                MaskTrackingModel::ObjectTranslation,
                MaskTrackingDirection::Forward,
                settings,
            )
            .expect("start cancelable tracking");
        assert!(state.cancel_visual_tracking(clip_id, mask_id));
        wait_for_terminal(&mut state, clip_id, mask_id);
        assert_eq!(
            state.visual_tracking_status(clip_id, mask_id).map(|status| status.phase),
            Some(VisualTrackingPhase::Canceled)
        );
        assert_eq!(
            state.active_sequence().expect("Sequence"),
            &before_cancel,
            "canceled analysis must not partially publish author state"
        );

        state
            .start_visual_tracking(
                clip_id,
                mask_id,
                MaskTrackingModel::ObjectTranslation,
                MaskTrackingDirection::Forward,
                settings,
            )
            .expect("start stale tracking");
        let sequence_id = state.active_sequence().expect("Sequence").id;
        state
            .commit_sequence_edit(sequence_id, "使跟踪结果过期", |sequence| {
                sequence.name.push_str(" (edited)");
                Ok(())
            })
            .expect("advance Sequence revision");
        let revision_after_edit = state.active_sequence().expect("Sequence").revision;
        let tracked_before_stale_completion =
            find_clip(state.active_sequence().expect("Sequence"), clip_id)
                .and_then(|clip| clip.mask(mask_id))
                .expect("tracked Mask")
                .clone();
        wait_for_terminal(&mut state, clip_id, mask_id);
        assert_eq!(
            state.visual_tracking_status(clip_id, mask_id).map(|status| status.phase),
            Some(VisualTrackingPhase::Stale)
        );
        assert_eq!(
            state.active_sequence().expect("Sequence").revision,
            revision_after_edit,
            "stale completion must not create an author transaction"
        );
        assert_eq!(
            find_clip(state.active_sequence().expect("Sequence"), clip_id)
                .and_then(|clip| clip.mask(mask_id)),
            Some(&tracked_before_stale_completion),
            "stale completion must not replace generated keys or recipe"
        );
    }
}
