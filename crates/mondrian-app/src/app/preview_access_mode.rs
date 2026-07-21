//! Access-mode request admission for application media preview.
//!
//! This module owns the app-layer scheduling contract for playback, scrub, and
//! random-access preview work. It deliberately does not decode media, evaluate
//! render plans, interpret color, or convert frames.

use std::cell::Cell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::app::ui_actions::TimelineSeekSource;
use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::types::{AssetId, ColorEngine, ColorSpace};
use mondrian_core::{TimelineTime, WorkingColorSpace};
use mondrian_media::{
    preview_decode_cpu_budget, DecodedVideoRangeContract, HwAccelDeviceSelector,
    MediaFileFingerprint, PreviewDecodeAccessMode, PreviewDecodeAdaptiveHints,
    PreviewHardwareDecodeRequest,
};

pub(crate) const MEDIA_PREVIEW_JOB_QUEUE_CAPACITY: usize = 48;
pub(crate) const MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US: u64 = 50_000;
pub(crate) const MEDIA_PREVIEW_DECODE_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(2);
const MEDIA_PREVIEW_MAX_DECODE_WORKERS: usize = 2;
const MEDIA_PREVIEW_MAX_PENDING_REQUESTS: usize = MEDIA_PREVIEW_JOB_QUEUE_CAPACITY;

/// Decoder-native surface family inferred from probed source bit depth/layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MediaPreviewNativeSurfaceHint {
    Nv12,
    P010,
}

/// Stable identity for one decoded media preview request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MediaPreviewKey {
    pub(crate) asset_id: AssetId,
    pub(crate) path: PathBuf,
    pub(crate) fingerprint: Option<MediaFileFingerprint>,
    /// Exact source-local decode target and part of cache identity.
    pub(crate) source_time: TimelineTime,
    pub(crate) target_width: u32,
    pub(crate) target_height: u32,
    /// Full-resolution source width represented by the decoded sample.
    pub(crate) source_width: u32,
    /// Full-resolution source height represented by the decoded sample.
    pub(crate) source_height: u32,
    pub(crate) input_color_space: ColorSpace,
    pub(crate) input_video_range: DecodedVideoRangeContract,
    pub(crate) native_surface_hint: Option<MediaPreviewNativeSurfaceHint>,
    pub(crate) source_has_alpha: bool,
    pub(crate) alpha_interpretation: AlphaInterpretation,
    pub(crate) working_color_space: WorkingColorSpace,
    pub(crate) tone_map: bool,
    pub(crate) engine: ColorEngine,
    pub(crate) ocio_generation: u64,
}

/// Media Adapter over the Playback Module's semantic latest-wins scheduler.
#[derive(Clone)]
pub(crate) struct MediaPreviewScheduler {
    broker: MediaPreviewWorkBroker,
}

/// Scheduler-owned evidence for realtime work expired before worker completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExpiredMediaPreviewRequest {
    pub(crate) key: MediaPreviewKey,
    pub(crate) access_mode: PreviewDecodeAccessMode,
    pub(crate) demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
}

/// Preview decode request priority used by scheduler admission and job queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreviewRequestPriority {
    Prefetch,
    Current,
}

/// App-owned reason why concrete preview execution observed cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreviewCancelReason {
    Shutdown,
    Obsolete,
    PrefetchDeadline,
    PlaybackDeadline,
    PrefetchPreemptedByCurrent,
    StillPreemptedByRealtimeCurrent,
    Unknown,
}

impl MediaPreviewCancelReason {
    /// Lower this Adapter reason into playback-owned evidence vocabulary.
    pub(crate) const fn playback_cause(self) -> mondrian_playback::FrameCancellationCause {
        match self {
            Self::Shutdown => mondrian_playback::FrameCancellationCause::Shutdown,
            Self::Obsolete => mondrian_playback::FrameCancellationCause::Superseded,
            Self::PrefetchDeadline => mondrian_playback::FrameCancellationCause::PrefetchDeadline,
            Self::PlaybackDeadline => mondrian_playback::FrameCancellationCause::PlaybackDeadline,
            Self::PrefetchPreemptedByCurrent => {
                mondrian_playback::FrameCancellationCause::PrefetchPreemptedByCurrent
            }
            Self::StillPreemptedByRealtimeCurrent => {
                mondrian_playback::FrameCancellationCause::StillPreemptedByRealtimeCurrent
            }
            Self::Unknown => mondrian_playback::FrameCancellationCause::Unknown,
        }
    }
}

/// Translate the Broker's atomic cancellation disposition for a media Adapter.
pub(crate) fn media_preview_cancel_reason_from_execution(
    cancellation: mondrian_playback::FrameExecutionCancellation,
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
) -> MediaPreviewCancelReason {
    match cancellation {
        mondrian_playback::FrameExecutionCancellation::BrokerClosed { .. } => {
            MediaPreviewCancelReason::Shutdown
        }
        mondrian_playback::FrameExecutionCancellation::Superseded { .. } => {
            MediaPreviewCancelReason::Obsolete
        }
        mondrian_playback::FrameExecutionCancellation::PrefetchPreemptedByCurrent { .. } => {
            MediaPreviewCancelReason::PrefetchPreemptedByCurrent
        }
        mondrian_playback::FrameExecutionCancellation::StillPreemptedByRealtimeCurrent {
            ..
        } => MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent,
        mondrian_playback::FrameExecutionCancellation::DeadlineExpired { .. } => {
            if priority == MediaPreviewRequestPriority::Prefetch {
                MediaPreviewCancelReason::PrefetchDeadline
            } else if access_mode == PreviewDecodeAccessMode::PlaybackCursor {
                MediaPreviewCancelReason::PlaybackDeadline
            } else {
                MediaPreviewCancelReason::Unknown
            }
        }
    }
}

/// Resolve cancellation at a cooperative decoder checkpoint.
pub(crate) fn media_preview_cancel_reason_at_checkpoint(
    scheduler_cancellation: Option<mondrian_playback::FrameExecutionCancellation>,
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
    elapsed: Duration,
    deadline_at: Option<Instant>,
) -> Option<MediaPreviewCancelReason> {
    media_preview_cancel_reason(
        scheduler_cancellation,
        priority,
        access_mode,
        elapsed,
        deadline_at.is_some(),
    )
}

/// Resolve Broker-owned or bounded speculative cancellation without UI state.
pub(crate) fn media_preview_cancel_reason(
    scheduler_cancellation: Option<mondrian_playback::FrameExecutionCancellation>,
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
    elapsed: Duration,
    broker_deadline_present: bool,
) -> Option<MediaPreviewCancelReason> {
    if let Some(cancellation) = scheduler_cancellation {
        return Some(media_preview_cancel_reason_from_execution(
            cancellation,
            priority,
            access_mode,
        ));
    }
    if priority == MediaPreviewRequestPriority::Prefetch
        && access_mode == PreviewDecodeAccessMode::PlaybackCursor
        && !broker_deadline_present
        && duration_us(elapsed) >= MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US
    {
        return Some(MediaPreviewCancelReason::PrefetchDeadline);
    }
    None
}

/// Attribute request-to-checkpoint latency to the original authority instant.
pub(crate) fn media_preview_cancel_request_to_observed_us(
    reason: MediaPreviewCancelReason,
    scheduler_cancellation: Option<mondrian_playback::FrameExecutionCancellation>,
    decode_started_at: Instant,
    observed_at: Instant,
) -> Option<u64> {
    let age = match reason {
        MediaPreviewCancelReason::Shutdown
        | MediaPreviewCancelReason::Obsolete
        | MediaPreviewCancelReason::PrefetchPreemptedByCurrent
        | MediaPreviewCancelReason::StillPreemptedByRealtimeCurrent
        | MediaPreviewCancelReason::PlaybackDeadline => {
            scheduler_cancellation.and_then(|cancellation| cancellation.request_age())
        }
        MediaPreviewCancelReason::PrefetchDeadline => scheduler_cancellation
            .and_then(mondrian_playback::FrameExecutionCancellation::request_age)
            .or_else(|| {
                observed_at.saturating_duration_since(decode_started_at).checked_sub(
                    Duration::from_micros(MEDIA_PREVIEW_PREFETCH_DECODE_BUDGET_US),
                )
            }),
        MediaPreviewCancelReason::Unknown => None,
    }?;
    Some(duration_us(age))
}

/// Result of admitting a preview decode request into the scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MediaPreviewRequestStatus {
    Scheduled {
        evicted_prefetch: Option<Box<MediaPreviewKey>>,
        evicted_still: Option<Box<MediaPreviewKey>>,
    },
    #[cfg(test)]
    AlreadyPending {
        access_mode_changed: bool,
    },
    UpdatedQueued {
        priority_promoted: bool,
        access_mode_changed: bool,
        generation_changed: bool,
    },
    ReusedInFlight,
    DroppedBackpressure,
    DroppedInvalidAccessMode,
    Closed,
}

/// Freshness classification for a completed decode result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreviewCompletionStatus {
    Current,
    CacheOnly,
    Stale,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MediaPreviewCompletionResolution {
    pub(crate) status: MediaPreviewCompletionStatus,
    pub(crate) demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
    pub(crate) deadline_status: mondrian_playback::FrameWorkDeadlineStatus,
}

impl MediaPreviewCompletionStatus {
    pub(crate) fn is_current(self) -> bool {
        matches!(self, Self::Current)
    }

    pub(crate) fn should_cache(self) -> bool {
        matches!(self, Self::Current | Self::CacheOnly)
    }
}

/// Scheduler-side preview media request counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct MediaPreviewSchedulerDiagnostics {
    /// Latest render generation observed by the scheduler.
    pub latest_generation: u64,
    /// Playback runtime-clock regression episodes rejected by the Frame Work Broker.
    pub clock_regressions: u64,
    /// Requests currently waiting to decode or complete.
    pub pending_requests: usize,
    /// New requests accepted into the pending set.
    pub scheduled_requests: u64,
    /// Requests that updated an already-pending key.
    pub already_pending_requests: u64,
    /// Already-pending requests that changed the pending access mode.
    pub already_pending_access_mode_changes: u64,
    /// Requests rejected by generation or pending-window backpressure.
    pub dropped_backpressure_requests: u64,
    /// Requests rejected because the priority/access-mode pair violates the
    /// scheduler contract, such as non-playback prefetch work.
    pub dropped_invalid_access_mode_requests: u64,
    /// Requests rejected because their render generation was obsolete.
    pub dropped_obsolete_generation_requests: u64,
    /// Requests rejected because the pending window had no eligible room.
    pub dropped_pending_window_requests: u64,
    /// Worker jobs skipped because their key was no longer pending/current.
    pub skipped_decode_jobs: u64,
    /// Worker jobs skipped because no matching pending request remained.
    pub skipped_decode_missing_pending: u64,
    /// Worker jobs skipped because their access mode no longer matched pending work.
    pub skipped_decode_access_mode_mismatch: u64,
    /// Worker jobs skipped because their generation was obsolete.
    pub skipped_decode_obsolete_generation: u64,
    /// Completed jobs still relevant to the latest generation.
    pub completed_current_results: u64,
    /// Completed jobs that are not current but can still populate the preview cache.
    pub completed_cache_only_results: u64,
    /// Cache-only jobs whose pending request was already removed.
    pub completed_cache_only_missing_pending: u64,
    /// Cache-only jobs whose access mode no longer matched pending visible work.
    pub completed_cache_only_access_mode_mismatch: u64,
    /// Completed jobs that were stale by the time the UI polled them.
    pub completed_stale_results: u64,
    /// Completed jobs treated as stale because no pending request remained.
    pub completed_stale_missing_pending: u64,
    /// Completed jobs treated as stale because their access mode no longer matched.
    pub completed_stale_access_mode_mismatch: u64,
    /// Completed jobs treated as stale because their generation was obsolete.
    pub completed_stale_obsolete_generation: u64,
    /// Pending requests canceled before completion.
    pub canceled_requests: u64,
    /// Obsolete pending requests removed during generation pruning.
    pub pruned_obsolete_requests: u64,
    /// Pending prefetch requests removed so a current-frame request can run.
    pub evicted_prefetch_requests: u64,
    /// Pending still-frame requests removed so real-time current work can run.
    pub evicted_still_requests: u64,
}

#[derive(Debug)]
/// Queued media preview decode job with access-mode scheduling evidence.
pub(crate) struct MediaPreviewJob {
    pub(crate) key: MediaPreviewKey,
    pub(crate) generation: u64,
    pub(crate) priority: MediaPreviewRequestPriority,
    pub(crate) access_mode: PreviewDecodeAccessMode,
    pub(crate) adaptive_hints: PreviewDecodeAdaptiveHints,
    pub(crate) hardware_decode_request: PreviewHardwareDecodeRequest,
    pub(crate) hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    pub(crate) enqueued_at: Instant,
    pub(crate) deadline_at: Option<Instant>,
    /// Opaque Playback Session identity; media workers only carry it.
    pub(crate) demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
    /// Broker execution lease, assigned only when a worker dequeues the job.
    pub(crate) execution_id: Option<mondrian_playback::FrameExecutionId>,
}

type MediaPreviewWorkBroker =
    mondrian_playback::FrameWorkBroker<MediaPreviewKey, Instant, MediaPreviewJob>;

pub(crate) struct MediaPreviewJobQueueSender {
    broker: MediaPreviewWorkBroker,
}

pub(crate) struct MediaPreviewJobQueueReceiver {
    broker: MediaPreviewWorkBroker,
    observed_lifecycle_revision: Cell<u64>,
}

impl Clone for MediaPreviewJobQueueReceiver {
    fn clone(&self) -> Self {
        Self {
            broker: self.broker.clone(),
            observed_lifecycle_revision: Cell::new(self.observed_lifecycle_revision.get()),
        }
    }
}

/// Point-in-time worker transport queue depth grouped by scheduling contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct MediaPreviewJobQueueDiagnostics {
    /// Total jobs waiting for a worker lease.
    pub queued_jobs: usize,
    /// Total jobs currently owned by worker execution leases.
    pub in_flight_jobs: usize,
    /// Worker leases whose result has completed but is not yet resolved.
    pub in_flight_completed_jobs: usize,
    /// Worker leases for which cancellation has already been requested.
    pub in_flight_cancellation_requested_jobs: usize,
    /// Oldest current worker lease age in microseconds.
    pub in_flight_max_age_us: u64,
    /// Oldest current worker cancellation age in microseconds.
    pub in_flight_cancellation_max_age_us: u64,
    /// Current-priority jobs waiting for a worker lease.
    pub queued_current_jobs: usize,
    /// Current-priority worker execution leases.
    pub in_flight_current_jobs: usize,
    /// Speculative jobs waiting for a worker lease.
    pub queued_prefetch_jobs: usize,
    /// Speculative worker execution leases.
    pub in_flight_prefetch_jobs: usize,
    /// Playback-class jobs waiting for a worker lease.
    pub queued_playback_cursor_jobs: usize,
    /// Playback-class worker execution leases.
    pub in_flight_playback_cursor_jobs: usize,
    /// Current playback jobs whose Adapter deadline has expired while queued.
    pub queued_expired_playback_current_jobs: usize,
    /// All queued jobs whose Broker-owned lowered deadline has expired.
    pub queued_expired_jobs: usize,
    /// Current playback jobs rejected at dequeue after their Adapter deadline.
    pub dropped_expired_playback_current_jobs: u64,
    /// All jobs rejected at dequeue after their Broker-owned lowered deadline.
    pub dropped_expired_jobs: u64,
    /// Interactive scrub jobs waiting for a worker lease.
    pub queued_scrub_cursor_jobs: usize,
    /// Interactive scrub worker execution leases.
    pub in_flight_scrub_cursor_jobs: usize,
    /// Deterministic still jobs waiting for a worker lease.
    pub queued_random_access_still_jobs: usize,
    /// Deterministic still worker execution leases.
    pub in_flight_random_access_still_jobs: usize,
    /// Execution leases on the unrestricted single-worker lane.
    pub in_flight_any_lane_jobs: usize,
    /// Execution leases on the playback-reserved lane.
    pub in_flight_playback_lane_jobs: usize,
    /// Execution leases on the scrub-reserved lane.
    pub in_flight_scrub_lane_jobs: usize,
    /// Execution leases on the still-frame-reserved lane.
    pub in_flight_still_lane_jobs: usize,
    /// Execution leases on the shared non-playback lane.
    pub in_flight_non_playback_lane_jobs: usize,
    /// Current execution leases whose lane does not accept their work class.
    pub in_flight_cross_lane_current_jobs: usize,
    /// Jobs eligible for an unrestricted worker lane.
    pub queued_any_lane_eligible_jobs: usize,
    /// Jobs directly eligible for a playback worker lane.
    pub queued_playback_lane_eligible_jobs: usize,
    /// Jobs directly eligible for an interactive scrub worker lane.
    pub queued_scrub_lane_eligible_jobs: usize,
    /// Jobs directly eligible for a deterministic still worker lane.
    pub queued_still_lane_eligible_jobs: usize,
    /// Jobs eligible for a shared non-playback worker lane.
    pub queued_non_playback_lane_eligible_jobs: usize,
    /// Whether the broker has closed worker transport.
    pub closed: bool,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MediaPreviewJobEnqueueStatus {
    Enqueued {
        evicted_prefetch: Option<Box<MediaPreviewKey>>,
        evicted_still: Option<Box<MediaPreviewKey>>,
    },
    DroppedFull,
    DroppedInvalidAccessMode,
    Closed,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MediaPreviewJobPromoteStatus {
    pub(crate) updated: bool,
    pub(crate) priority_promoted: bool,
    pub(crate) access_mode_changed: bool,
    pub(crate) generation_changed: bool,
}

#[derive(Debug)]
pub(crate) enum MediaPreviewJobQueueReceive {
    Job(MediaPreviewJob),
    DroppedExpired(MediaPreviewJob),
}

#[derive(Debug)]
/// Timed worker receive result.
///
/// The large work payload stays inline deliberately: boxing it would allocate
/// on every dequeue in the preview worker hot path merely to shrink Idle and
/// Closed values that are observed only at session lifecycle boundaries.
#[allow(clippy::large_enum_variant)]
pub(crate) enum MediaPreviewJobQueueWait {
    Work(MediaPreviewJobQueueReceive),
    Lifecycle,
    Idle,
    Closed,
}

#[cfg(test)]
pub(crate) fn media_preview_job_queue(
    capacity: usize,
) -> (MediaPreviewJobQueueSender, MediaPreviewJobQueueReceiver) {
    let broker = MediaPreviewWorkBroker::new(capacity, capacity);
    (
        MediaPreviewJobQueueSender { broker: broker.clone() },
        MediaPreviewJobQueueReceiver {
            observed_lifecycle_revision: Cell::new(broker.worker_lifecycle_revision()),
            broker,
        },
    )
}

impl MediaPreviewJobQueueSender {
    #[cfg(test)]
    pub(crate) fn clear(&self) -> usize {
        self.broker.cancel_all().1
    }

    pub(crate) fn close(&self) {
        self.broker.close();
    }

    /// Wake workers so they can execute a worker-owned lifecycle boundary.
    pub(crate) fn interrupt_workers_for_lifecycle(&self) {
        self.broker.interrupt_worker_waits();
    }

    #[cfg(test)]
    pub(crate) fn prune_obsolete_jobs(&self, generation: u64) -> usize {
        self.broker.prune_before(generation)
    }

    #[cfg(test)]
    pub(crate) fn cancel_key(&self, key: &MediaPreviewKey) -> usize {
        self.broker.cancel_key(key)
    }

    pub(crate) fn diagnostics(&self) -> MediaPreviewJobQueueDiagnostics {
        media_preview_job_queue_diagnostics(&self.broker)
    }

    #[cfg(test)]
    pub(crate) fn enqueue(&self, mut job: MediaPreviewJob) -> MediaPreviewJobEnqueueStatus {
        job.execution_id = None;
        map_enqueue_submission(self.broker.submit(frame_work_request(job)))
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn promote(
        &self,
        key: &MediaPreviewKey,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        generation: u64,
        enqueued_at: Instant,
        deadline_at: Option<Instant>,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
        adaptive_hints: PreviewDecodeAdaptiveHints,
        hardware_decode_request: PreviewHardwareDecodeRequest,
        hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    ) -> MediaPreviewJobPromoteStatus {
        if priority != MediaPreviewRequestPriority::Current {
            return MediaPreviewJobPromoteStatus::default();
        }
        if !self.broker.has_pending_key(key) {
            return MediaPreviewJobPromoteStatus::default();
        }
        match self.broker.submit(frame_work_request(MediaPreviewJob {
            key: key.clone(),
            generation,
            priority,
            access_mode,
            adaptive_hints,
            hardware_decode_request,
            hardware_decode_device_selector,
            enqueued_at,
            deadline_at,
            demand_identity,
            execution_id: None,
        })) {
            mondrian_playback::FrameWorkSubmission::UpdatedQueued {
                priority_promoted,
                work_class_changed,
                generation_changed,
            } => MediaPreviewJobPromoteStatus {
                updated: priority_promoted || work_class_changed || generation_changed,
                priority_promoted,
                access_mode_changed: work_class_changed,
                generation_changed,
            },
            mondrian_playback::FrameWorkSubmission::Queued { .. } => MediaPreviewJobPromoteStatus {
                updated: true,
                priority_promoted: true,
                access_mode_changed: true,
                generation_changed: true,
            },
            _ => MediaPreviewJobPromoteStatus::default(),
        }
    }
}

impl Drop for MediaPreviewJobQueueSender {
    fn drop(&mut self) {
        self.close();
    }
}

impl MediaPreviewJobQueueReceiver {
    #[cfg(test)]
    pub(crate) fn recv(&self) -> Option<MediaPreviewJob> {
        self.recv_for_worker(MediaPreviewWorkerLane::Any)
    }

    #[cfg(test)]
    pub(crate) fn recv_for_worker(&self, lane: MediaPreviewWorkerLane) -> Option<MediaPreviewJob> {
        match self.recv_for_worker_outcome(lane)? {
            MediaPreviewJobQueueReceive::Job(job) => Some(job),
            MediaPreviewJobQueueReceive::DroppedExpired(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn recv_for_worker_outcome(
        &self,
        lane: MediaPreviewWorkerLane,
    ) -> Option<MediaPreviewJobQueueReceive> {
        match self.broker.receive(frame_worker_lane(lane)) {
            Some(mondrian_playback::FrameWorkReceive::Ready(execution)) => Some(
                MediaPreviewJobQueueReceive::Job(media_preview_job_from_execution(execution)),
            ),
            Some(mondrian_playback::FrameWorkReceive::Expired(execution)) => {
                Some(MediaPreviewJobQueueReceive::DroppedExpired(
                    media_preview_job_from_execution(execution),
                ))
            }
            None => None,
        }
    }

    pub(crate) fn recv_for_worker_outcome_timeout(
        &self,
        lane: MediaPreviewWorkerLane,
        timeout: Duration,
    ) -> MediaPreviewJobQueueWait {
        match self.broker.receive_timeout_after_lifecycle_revision(
            frame_worker_lane(lane),
            timeout,
            self.observed_lifecycle_revision.get(),
        ) {
            mondrian_playback::FrameWorkReceiveWait::Work(
                mondrian_playback::FrameWorkReceive::Ready(execution),
            ) => MediaPreviewJobQueueWait::Work(MediaPreviewJobQueueReceive::Job(
                media_preview_job_from_execution(execution),
            )),
            mondrian_playback::FrameWorkReceiveWait::Work(
                mondrian_playback::FrameWorkReceive::Expired(execution),
            ) => MediaPreviewJobQueueWait::Work(MediaPreviewJobQueueReceive::DroppedExpired(
                media_preview_job_from_execution(execution),
            )),
            mondrian_playback::FrameWorkReceiveWait::TimedOut => MediaPreviewJobQueueWait::Idle,
            mondrian_playback::FrameWorkReceiveWait::Interrupted { revision } => {
                self.observed_lifecycle_revision.set(revision);
                MediaPreviewJobQueueWait::Lifecycle
            }
            mondrian_playback::FrameWorkReceiveWait::Closed => MediaPreviewJobQueueWait::Closed,
        }
    }
}

fn frame_work_request(
    job: MediaPreviewJob,
) -> mondrian_playback::FrameWorkRequest<MediaPreviewKey, Instant, MediaPreviewJob> {
    let deadline = job.deadline_at.map(|deadline_at| {
        let sampled_at = Instant::now();
        mondrian_playback::FrameWorkDeadline::from_remaining(
            deadline_at,
            deadline_at.saturating_duration_since(sampled_at),
        )
    });
    mondrian_playback::FrameWorkRequest {
        key: job.key.clone(),
        generation: job.generation,
        priority: frame_work_priority(job.priority),
        work_class: media_preview_frame_work_class(job.access_mode),
        demand_identity: job.demand_identity,
        deadline,
        payload: job,
    }
}

fn media_preview_job_from_execution(
    execution: mondrian_playback::FrameWorkExecution<MediaPreviewKey, Instant, MediaPreviewJob>,
) -> MediaPreviewJob {
    let mut job = execution.payload;
    job.key = execution.key;
    job.generation = execution.generation;
    job.priority = media_preview_priority(execution.priority);
    job.access_mode = preview_access_mode(execution.work_class);
    job.deadline_at = execution.deadline;
    job.demand_identity = execution.demand_identity;
    job.execution_id = Some(execution.id);
    job
}

#[cfg(test)]
fn map_enqueue_submission(
    submission: mondrian_playback::FrameWorkSubmission<MediaPreviewKey>,
) -> MediaPreviewJobEnqueueStatus {
    match submission {
        mondrian_playback::FrameWorkSubmission::Queued { evicted_prefetch, evicted_still } => {
            MediaPreviewJobEnqueueStatus::Enqueued {
                evicted_prefetch: evicted_prefetch.map(Box::new),
                evicted_still: evicted_still.map(Box::new),
            }
        }
        mondrian_playback::FrameWorkSubmission::UpdatedQueued { .. }
        | mondrian_playback::FrameWorkSubmission::ReusedInFlight => {
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        }
        mondrian_playback::FrameWorkSubmission::DroppedBackpressure => {
            MediaPreviewJobEnqueueStatus::DroppedFull
        }
        mondrian_playback::FrameWorkSubmission::DroppedInvalidClass => {
            MediaPreviewJobEnqueueStatus::DroppedInvalidAccessMode
        }
        mondrian_playback::FrameWorkSubmission::Closed => MediaPreviewJobEnqueueStatus::Closed,
    }
}

fn media_preview_job_queue_diagnostics(
    broker: &MediaPreviewWorkBroker,
) -> MediaPreviewJobQueueDiagnostics {
    let state = broker.diagnostics();
    MediaPreviewJobQueueDiagnostics {
        queued_jobs: state.queued_work,
        in_flight_jobs: state.in_flight_work,
        in_flight_completed_jobs: state.in_flight_completed,
        in_flight_cancellation_requested_jobs: state.in_flight_cancellation_requested,
        in_flight_max_age_us: state.in_flight_max_age_us,
        in_flight_cancellation_max_age_us: state.in_flight_cancellation_max_age_us,
        queued_current_jobs: state.queued_current,
        in_flight_current_jobs: state.in_flight_current,
        queued_prefetch_jobs: state.queued_prefetch,
        in_flight_prefetch_jobs: state.in_flight_prefetch,
        queued_playback_cursor_jobs: state.queued_playback,
        in_flight_playback_cursor_jobs: state.in_flight_playback,
        queued_expired_playback_current_jobs: state.queued_expired_playback_current,
        queued_expired_jobs: state.queued_expired_work,
        dropped_expired_playback_current_jobs: state.dropped_expired_playback_current,
        dropped_expired_jobs: state.dropped_expired_work,
        queued_scrub_cursor_jobs: state.queued_interactive,
        in_flight_scrub_cursor_jobs: state.in_flight_interactive,
        queued_random_access_still_jobs: state.queued_still,
        in_flight_random_access_still_jobs: state.in_flight_still,
        in_flight_any_lane_jobs: state.in_flight_any_lane,
        in_flight_playback_lane_jobs: state.in_flight_playback_lane,
        in_flight_scrub_lane_jobs: state.in_flight_interactive_lane,
        in_flight_still_lane_jobs: state.in_flight_still_lane,
        in_flight_non_playback_lane_jobs: state.in_flight_non_playback_lane,
        in_flight_cross_lane_current_jobs: state.in_flight_cross_lane_current,
        queued_any_lane_eligible_jobs: state.queued_work,
        queued_playback_lane_eligible_jobs: state.queued_playback,
        queued_scrub_lane_eligible_jobs: state.queued_interactive,
        queued_still_lane_eligible_jobs: state.queued_still,
        queued_non_playback_lane_eligible_jobs: state
            .queued_interactive
            .saturating_add(state.queued_still),
        closed: state.closed,
    }
}

fn media_preview_priority(
    priority: mondrian_playback::FrameWorkPriority,
) -> MediaPreviewRequestPriority {
    match priority {
        mondrian_playback::FrameWorkPriority::Prefetch => MediaPreviewRequestPriority::Prefetch,
        mondrian_playback::FrameWorkPriority::Current => MediaPreviewRequestPriority::Current,
    }
}

fn frame_worker_lane(lane: MediaPreviewWorkerLane) -> mondrian_playback::FrameWorkerLane {
    match lane {
        MediaPreviewWorkerLane::Any => mondrian_playback::FrameWorkerLane::Any,
        MediaPreviewWorkerLane::Playback => mondrian_playback::FrameWorkerLane::Playback,
        MediaPreviewWorkerLane::NonPlayback => mondrian_playback::FrameWorkerLane::NonPlayback,
    }
}

/// App-layer preview workload intent before it is lowered to a media access mode.
///
/// This keeps UI state interpretation out of the media crate. Callers should
/// choose an intent that describes the user interaction, then lower it through
/// [`media_preview_access_mode_for_intent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreviewAccessIntent {
    /// Sustained timeline playback or forward prefetch.
    Playback,
    /// Latest-wins playhead movement, jog/shuttle, and non-playing viewer seeks.
    InteractiveScrub,
    /// Deterministic one-off still extraction such as thumbnails or poster frames.
    DeterministicStill,
}

pub(crate) fn media_preview_viewer_access_intent(
    is_playing: bool,
    seek_source: TimelineSeekSource,
) -> MediaPreviewAccessIntent {
    if is_playing {
        MediaPreviewAccessIntent::Playback
    } else {
        match seek_source {
            TimelineSeekSource::PointerDrag => MediaPreviewAccessIntent::InteractiveScrub,
            TimelineSeekSource::Settled => MediaPreviewAccessIntent::DeterministicStill,
        }
    }
}

pub(crate) fn media_preview_access_mode_for_intent(
    intent: MediaPreviewAccessIntent,
) -> PreviewDecodeAccessMode {
    match intent {
        MediaPreviewAccessIntent::Playback => PreviewDecodeAccessMode::PlaybackCursor,
        MediaPreviewAccessIntent::InteractiveScrub => PreviewDecodeAccessMode::ScrubCursor,
        MediaPreviewAccessIntent::DeterministicStill => {
            PreviewDecodeAccessMode::RandomAccessStillFrame
        }
    }
}

pub(crate) fn media_preview_worker_count() -> usize {
    media_preview_worker_count_for(preview_decode_cpu_budget().available_parallelism)
}

pub(crate) fn media_preview_worker_count_for(parallelism: usize) -> usize {
    mondrian_media::PreviewDecodeCpuBudget::for_available_parallelism(parallelism)
        .preview_worker_count
        .clamp(1, MEDIA_PREVIEW_MAX_DECODE_WORKERS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreviewWorkerLane {
    Any,
    Playback,
    NonPlayback,
}

pub(crate) fn media_preview_worker_lane(
    worker_index: usize,
    worker_count: usize,
) -> MediaPreviewWorkerLane {
    if worker_count <= 1 {
        MediaPreviewWorkerLane::Any
    } else {
        match worker_index {
            0 => MediaPreviewWorkerLane::Playback,
            _ => MediaPreviewWorkerLane::NonPlayback,
        }
    }
}

impl Default for MediaPreviewScheduler {
    fn default() -> Self {
        Self::with_max_pending(MEDIA_PREVIEW_MAX_PENDING_REQUESTS)
    }
}

impl MediaPreviewScheduler {
    pub(crate) fn with_max_pending(max_pending: usize) -> Self {
        Self {
            broker: MediaPreviewWorkBroker::new(max_pending, MEDIA_PREVIEW_JOB_QUEUE_CAPACITY),
        }
    }

    pub(crate) fn job_queue(&self) -> (MediaPreviewJobQueueSender, MediaPreviewJobQueueReceiver) {
        (
            MediaPreviewJobQueueSender { broker: self.broker.clone() },
            MediaPreviewJobQueueReceiver {
                observed_lifecycle_revision: Cell::new(self.broker.worker_lifecycle_revision()),
                broker: self.broker.clone(),
            },
        )
    }

    pub(crate) fn begin_generation(&self) -> u64 {
        self.broker.begin_generation()
    }

    pub(crate) fn submit_job(&self, mut job: MediaPreviewJob) -> MediaPreviewRequestStatus {
        job.execution_id = None;
        match self.broker.submit(frame_work_request(job)) {
            mondrian_playback::FrameWorkSubmission::Queued { evicted_prefetch, evicted_still } => {
                MediaPreviewRequestStatus::Scheduled {
                    evicted_prefetch: evicted_prefetch.map(Box::new),
                    evicted_still: evicted_still.map(Box::new),
                }
            }
            mondrian_playback::FrameWorkSubmission::UpdatedQueued {
                priority_promoted,
                work_class_changed,
                generation_changed,
            } => MediaPreviewRequestStatus::UpdatedQueued {
                priority_promoted,
                access_mode_changed: work_class_changed,
                generation_changed,
            },
            mondrian_playback::FrameWorkSubmission::ReusedInFlight => {
                MediaPreviewRequestStatus::ReusedInFlight
            }
            mondrian_playback::FrameWorkSubmission::DroppedBackpressure => {
                MediaPreviewRequestStatus::DroppedBackpressure
            }
            mondrian_playback::FrameWorkSubmission::DroppedInvalidClass => {
                MediaPreviewRequestStatus::DroppedInvalidAccessMode
            }
            mondrian_playback::FrameWorkSubmission::Closed => MediaPreviewRequestStatus::Closed,
        }
    }

    #[cfg(test)]
    pub(crate) fn request(
        &self,
        key: MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
    ) -> MediaPreviewRequestStatus {
        self.request_with_binding(key, generation, priority, access_mode, None, None)
    }

    #[cfg(test)]
    pub(crate) fn request_with_demand_identity(
        &self,
        key: MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> MediaPreviewRequestStatus {
        self.request_with_binding(
            key,
            generation,
            priority,
            access_mode,
            demand_identity,
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn request_with_binding(
        &self,
        key: MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
        deadline_at: Option<Instant>,
    ) -> MediaPreviewRequestStatus {
        let status = self.submit_job(MediaPreviewJob {
            key,
            generation,
            priority,
            access_mode,
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            hardware_decode_device_selector: None,
            enqueued_at: Instant::now(),
            deadline_at,
            demand_identity,
            execution_id: None,
        });
        match status {
            MediaPreviewRequestStatus::UpdatedQueued { access_mode_changed, .. } => {
                MediaPreviewRequestStatus::AlreadyPending { access_mode_changed }
            }
            MediaPreviewRequestStatus::ReusedInFlight => {
                MediaPreviewRequestStatus::AlreadyPending { access_mode_changed: false }
            }
            status => status,
        }
    }

    #[cfg(test)]
    pub(crate) fn execution_cancellation(
        &self,
        id: mondrian_playback::FrameExecutionId,
    ) -> Option<mondrian_playback::FrameExecutionCancellation> {
        self.broker.execution_cancellation(id)
    }

    pub(crate) fn execution_cancellation_evidence(
        &self,
        id: mondrian_playback::FrameExecutionId,
    ) -> Option<mondrian_playback::FrameExecutionCancellationEvidence> {
        self.broker.execution_cancellation_evidence(id)
    }

    pub(crate) fn mark_execution_completed(&self, id: mondrian_playback::FrameExecutionId) -> bool {
        self.broker.mark_execution_completed(id)
    }

    #[cfg(test)]
    pub(crate) fn should_decode(
        &self,
        key: &MediaPreviewKey,
        access_mode: PreviewDecodeAccessMode,
    ) -> bool {
        self.broker.key_current(
            key,
            self.diagnostics().latest_generation,
            media_preview_frame_work_class(access_mode),
        )
    }

    #[cfg(test)]
    pub(crate) fn is_decode_current(
        &self,
        key: &MediaPreviewKey,
        generation: u64,
        access_mode: PreviewDecodeAccessMode,
    ) -> bool {
        self.broker
            .key_current(key, generation, media_preview_frame_work_class(access_mode))
    }

    #[cfg(test)]
    pub(crate) fn has_pending_current_request_other_than(&self, key: &MediaPreviewKey) -> bool {
        self.broker.has_other_current_key(key, false)
    }

    #[cfg(test)]
    pub(crate) fn has_pending_realtime_current_request_other_than(
        &self,
        key: &MediaPreviewKey,
    ) -> bool {
        self.broker.has_other_current_key(key, true)
    }

    #[cfg(test)]
    pub(crate) fn complete(
        &self,
        key: &MediaPreviewKey,
        result_generation: u64,
        access_mode: PreviewDecodeAccessMode,
    ) -> MediaPreviewCompletionStatus {
        map_completion(
            self.broker
                .resolve_unleased(
                    key.clone(),
                    result_generation,
                    media_preview_frame_work_class(access_mode),
                    None,
                    true,
                )
                .completion,
        )
    }

    pub(crate) fn resolve_execution(
        &self,
        id: mondrian_playback::FrameExecutionId,
        reusable: bool,
    ) -> MediaPreviewCompletionResolution {
        let resolution = self.broker.resolve_execution(id, reusable);
        MediaPreviewCompletionResolution {
            status: map_completion(resolution.completion),
            demand_identity: resolution.binding.and_then(|binding| binding.demand_identity),
            deadline_status: resolution.deadline,
        }
    }

    pub(crate) fn resolve_unleased(
        &self,
        key: &MediaPreviewKey,
        generation: u64,
        access_mode: PreviewDecodeAccessMode,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
        reusable: bool,
    ) -> MediaPreviewCompletionResolution {
        let resolution = self.broker.resolve_unleased(
            key.clone(),
            generation,
            media_preview_frame_work_class(access_mode),
            demand_identity,
            reusable,
        );
        MediaPreviewCompletionResolution {
            status: map_completion(resolution.completion),
            demand_identity: resolution.binding.and_then(|binding| binding.demand_identity),
            deadline_status: resolution.deadline,
        }
    }

    pub(crate) fn abandon_execution(&self, id: mondrian_playback::FrameExecutionId) {
        self.broker.abandon_execution(id);
    }

    #[cfg(test)]
    pub(crate) fn cancel(&self, key: &MediaPreviewKey) {
        self.broker.cancel_key(key);
    }

    pub(crate) fn expire_realtime_current_older_than(
        &self,
        max_age: Duration,
    ) -> Vec<ExpiredMediaPreviewRequest> {
        self.broker
            .expire_realtime_current_older_than(max_age)
            .into_iter()
            .map(|request| ExpiredMediaPreviewRequest {
                key: request.key,
                access_mode: preview_access_mode(request.binding.work_class),
                demand_identity: request.binding.demand_identity,
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn pending_still_for_realtime_current(
        &self,
        protected_key: &MediaPreviewKey,
    ) -> Vec<MediaPreviewKey> {
        self.broker.pending_still_except(protected_key)
    }

    #[cfg(test)]
    pub(crate) fn cancel_preempted_still_for_realtime_current(
        &self,
        key: &MediaPreviewKey,
    ) -> bool {
        self.broker.cancel_preempted_still(key)
    }

    pub(crate) fn cancel_all(&self) -> (u64, usize) {
        self.broker.cancel_all()
    }

    pub(crate) fn prune_obsolete(&self) -> usize {
        self.broker.prune_obsolete()
    }

    pub(crate) fn diagnostics(&self) -> MediaPreviewSchedulerDiagnostics {
        let state = self.broker.diagnostics();
        MediaPreviewSchedulerDiagnostics {
            latest_generation: state.latest_generation,
            clock_regressions: state.clock_regressions,
            pending_requests: state.pending_requests,
            scheduled_requests: state.submitted_queued,
            already_pending_requests: state
                .submitted_updated_queued
                .saturating_add(state.submitted_reused_in_flight),
            already_pending_access_mode_changes: state.submitted_work_class_changes,
            dropped_backpressure_requests: state.dropped_backpressure,
            dropped_invalid_access_mode_requests: state.dropped_invalid_class,
            dropped_obsolete_generation_requests: state.dropped_obsolete_generation,
            dropped_pending_window_requests: state
                .dropped_backpressure
                .saturating_sub(state.dropped_obsolete_generation),
            skipped_decode_jobs: state
                .skipped_missing
                .saturating_add(state.skipped_class_mismatch)
                .saturating_add(state.skipped_obsolete),
            skipped_decode_missing_pending: state.skipped_missing,
            skipped_decode_access_mode_mismatch: state.skipped_class_mismatch,
            skipped_decode_obsolete_generation: state.skipped_obsolete,
            completed_current_results: state.completed_current,
            completed_cache_only_results: state.completed_cache_only,
            completed_cache_only_missing_pending: state.completed_cache_only_missing,
            completed_cache_only_access_mode_mismatch: state.completed_cache_only_class_mismatch,
            completed_stale_results: state.completed_stale,
            completed_stale_missing_pending: state.completed_stale_missing,
            completed_stale_access_mode_mismatch: state.completed_stale_class_mismatch,
            completed_stale_obsolete_generation: state.completed_stale_obsolete,
            canceled_requests: state.canceled_requests,
            pruned_obsolete_requests: state.pruned_queued,
            evicted_prefetch_requests: state.evicted_prefetch,
            evicted_still_requests: state.evicted_still,
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.diagnostics().pending_requests
    }

    #[cfg(test)]
    pub(crate) fn has_pending_key(&self, key: &MediaPreviewKey) -> bool {
        self.broker.has_pending_key(key)
    }

    #[cfg(test)]
    pub(crate) fn begin_test_execution(
        &self,
        lane: MediaPreviewWorkerLane,
    ) -> Option<mondrian_playback::FrameExecutionId> {
        let receive = self.broker.receive(frame_worker_lane(lane))?;
        Some(match receive {
            mondrian_playback::FrameWorkReceive::Ready(execution)
            | mondrian_playback::FrameWorkReceive::Expired(execution) => execution.id,
        })
    }
}

fn map_completion(
    completion: mondrian_playback::FrameRequestCompletion,
) -> MediaPreviewCompletionStatus {
    match completion {
        mondrian_playback::FrameRequestCompletion::Current => MediaPreviewCompletionStatus::Current,
        mondrian_playback::FrameRequestCompletion::CacheOnly => {
            MediaPreviewCompletionStatus::CacheOnly
        }
        mondrian_playback::FrameRequestCompletion::Stale => MediaPreviewCompletionStatus::Stale,
    }
}

fn frame_work_priority(
    priority: MediaPreviewRequestPriority,
) -> mondrian_playback::FrameWorkPriority {
    match priority {
        MediaPreviewRequestPriority::Prefetch => mondrian_playback::FrameWorkPriority::Prefetch,
        MediaPreviewRequestPriority::Current => mondrian_playback::FrameWorkPriority::Current,
    }
}

pub(crate) fn media_preview_frame_work_class(
    access_mode: PreviewDecodeAccessMode,
) -> mondrian_playback::FrameWorkClass {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => mondrian_playback::FrameWorkClass::Playback,
        PreviewDecodeAccessMode::ScrubCursor => mondrian_playback::FrameWorkClass::Interactive,
        PreviewDecodeAccessMode::RandomAccessStillFrame => mondrian_playback::FrameWorkClass::Still,
    }
}

fn duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn preview_access_mode(work_class: mondrian_playback::FrameWorkClass) -> PreviewDecodeAccessMode {
    match work_class {
        mondrian_playback::FrameWorkClass::Playback => PreviewDecodeAccessMode::PlaybackCursor,
        mondrian_playback::FrameWorkClass::Interactive => PreviewDecodeAccessMode::ScrubCursor,
        mondrian_playback::FrameWorkClass::Still => PreviewDecodeAccessMode::RandomAccessStillFrame,
    }
}
#[cfg(test)]
#[path = "preview_access_mode/tests.rs"]
mod tests;
