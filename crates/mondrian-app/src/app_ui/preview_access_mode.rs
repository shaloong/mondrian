//! Access-mode request admission for app viewer media preview.
//!
//! This module owns the app-layer scheduling contract for playback, scrub, and
//! random-access preview work. It deliberately does not decode media, evaluate
//! render plans, interpret color, or convert frames.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::app::ui_actions::TimelineSeekSource;
use mondrian_core::types::{AssetId, ColorEngine, ColorSpace};
use mondrian_core::WorkingColorSpace;
use mondrian_media::{
    preview_decode_cpu_budget, DecodedVideoRange, PreviewDecodeAccessMode,
    PreviewDecodeAdaptiveHints, PreviewFileFingerprint, PreviewHardwareDecodeRequest,
};

pub(crate) const MEDIA_PREVIEW_JOB_QUEUE_CAPACITY: usize = 48;
const MEDIA_PREVIEW_MAX_DECODE_WORKERS: usize = 3;
const MEDIA_PREVIEW_MAX_PENDING_REQUESTS: usize = MEDIA_PREVIEW_JOB_QUEUE_CAPACITY;

/// Stable identity for one decoded media preview request.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MediaPreviewKey {
    pub(crate) asset_id: AssetId,
    pub(crate) path: PathBuf,
    pub(crate) fingerprint: Option<PreviewFileFingerprint>,
    pub(crate) source_frame: i64,
    pub(crate) source_micros: i64,
    pub(crate) target_width: u32,
    pub(crate) target_height: u32,
    pub(crate) input_color_space: ColorSpace,
    pub(crate) input_video_range: DecodedVideoRange,
    pub(crate) working_color_space: WorkingColorSpace,
    pub(crate) tone_map: bool,
    pub(crate) engine: ColorEngine,
    pub(crate) ocio_generation: u64,
}

/// Media Adapter over the Playback Module's semantic latest-wins scheduler.
#[derive(Clone)]
pub(crate) struct MediaPreviewScheduler {
    scheduler: mondrian_playback::FrameRequestScheduler<MediaPreviewKey>,
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

impl MediaPreviewRequestPriority {
    pub(crate) fn promote_with(self, other: Self) -> Self {
        match (self, other) {
            (Self::Current, _) | (_, Self::Current) => Self::Current,
            (Self::Prefetch, Self::Prefetch) => Self::Prefetch,
        }
    }

    fn accepts_access_mode(self, access_mode: PreviewDecodeAccessMode) -> bool {
        match self {
            Self::Current => true,
            Self::Prefetch => access_mode == PreviewDecodeAccessMode::PlaybackCursor,
        }
    }
}

pub(crate) fn promoted_access_mode(
    existing_priority: MediaPreviewRequestPriority,
    existing_access_mode: PreviewDecodeAccessMode,
    requested_priority: MediaPreviewRequestPriority,
    requested_access_mode: PreviewDecodeAccessMode,
) -> PreviewDecodeAccessMode {
    if requested_priority == MediaPreviewRequestPriority::Current {
        requested_access_mode
    } else if existing_priority == MediaPreviewRequestPriority::Current {
        existing_access_mode
    } else {
        requested_access_mode
    }
}

/// Result of admitting a preview decode request into the scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MediaPreviewRequestStatus {
    Scheduled {
        evicted_prefetch: Option<Box<MediaPreviewKey>>,
        evicted_still: Option<Box<MediaPreviewKey>>,
    },
    AlreadyPending {
        access_mode_changed: bool,
    },
    DroppedBackpressure,
    DroppedInvalidAccessMode,
}

/// Freshness classification for a completed decode result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaPreviewCompletionStatus {
    Current,
    CacheOnly,
    Stale,
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
    pub(crate) source_secs: f64,
    pub(crate) generation: u64,
    pub(crate) priority: MediaPreviewRequestPriority,
    pub(crate) access_mode: PreviewDecodeAccessMode,
    pub(crate) adaptive_hints: PreviewDecodeAdaptiveHints,
    pub(crate) hardware_decode_request: PreviewHardwareDecodeRequest,
    pub(crate) enqueued_at: Instant,
    pub(crate) deadline_at: Option<Instant>,
    /// Opaque Playback Session identity; media workers only carry it.
    pub(crate) demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
}

pub(crate) struct MediaPreviewJobQueueSender {
    shared: Arc<MediaPreviewJobQueueShared>,
}

#[derive(Clone)]
pub(crate) struct MediaPreviewJobQueueReceiver {
    shared: Arc<MediaPreviewJobQueueShared>,
}

struct MediaPreviewJobQueueShared {
    state: Mutex<MediaPreviewJobQueueState>,
    changed: Condvar,
    capacity: usize,
}

struct MediaPreviewJobQueueState {
    queue: VecDeque<QueuedMediaPreviewJob>,
    closed: bool,
    dropped_expired_playback_current_jobs: u64,
}

/// Point-in-time worker transport queue depth grouped by scheduling contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct MediaPreviewJobQueueDiagnostics {
    /// Total jobs waiting in the worker transport queue.
    pub queued_jobs: usize,
    /// Current-frame jobs waiting in the worker transport queue.
    pub queued_current_jobs: usize,
    /// Prefetch jobs waiting in the worker transport queue.
    pub queued_prefetch_jobs: usize,
    /// Playback cursor jobs waiting in the worker transport queue.
    pub queued_playback_cursor_jobs: usize,
    /// Current playback jobs whose display deadline expired while queued.
    pub queued_expired_playback_current_jobs: usize,
    /// Current playback jobs dropped at dequeue because their display deadline expired.
    pub dropped_expired_playback_current_jobs: u64,
    /// Scrub cursor jobs waiting in the worker transport queue.
    pub queued_scrub_cursor_jobs: usize,
    /// Random-access still-frame jobs waiting in the worker transport queue.
    pub queued_random_access_still_jobs: usize,
    /// Jobs currently eligible for any-lane workers.
    pub queued_any_lane_eligible_jobs: usize,
    /// Jobs currently eligible for playback-lane workers.
    pub queued_playback_lane_eligible_jobs: usize,
    /// Jobs currently eligible for scrub-lane workers.
    pub queued_scrub_lane_eligible_jobs: usize,
    /// Jobs currently eligible for still-lane workers.
    pub queued_still_lane_eligible_jobs: usize,
    /// Jobs currently eligible for shared interactive-lane workers.
    pub queued_interactive_lane_eligible_jobs: usize,
    /// Whether the worker transport queue has been closed.
    pub closed: bool,
}

struct QueuedMediaPreviewJob {
    job: MediaPreviewJob,
    priority: MediaPreviewRequestPriority,
}

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
    DroppedExpiredPlaybackCurrent(MediaPreviewJob),
}

pub(crate) fn media_preview_job_queue(
    capacity: usize,
) -> (MediaPreviewJobQueueSender, MediaPreviewJobQueueReceiver) {
    let shared = Arc::new(MediaPreviewJobQueueShared {
        state: Mutex::new(MediaPreviewJobQueueState {
            queue: VecDeque::new(),
            closed: false,
            dropped_expired_playback_current_jobs: 0,
        }),
        changed: Condvar::new(),
        capacity: capacity.max(1),
    });
    (
        MediaPreviewJobQueueSender { shared: Arc::clone(&shared) },
        MediaPreviewJobQueueReceiver { shared },
    )
}

impl MediaPreviewJobQueueSender {
    pub(crate) fn clear(&self) -> usize {
        let mut state = lock_media_preview_job_queue_state(&self.shared.state);
        let cleared = state.queue.len();
        state.queue.clear();
        if cleared > 0 {
            self.shared.changed.notify_all();
        }
        cleared
    }

    pub(crate) fn close(&self) {
        let mut state = lock_media_preview_job_queue_state(&self.shared.state);
        state.queue.clear();
        state.closed = true;
        self.shared.changed.notify_all();
    }

    pub(crate) fn prune_obsolete_jobs(&self, generation: u64) -> usize {
        let mut state = lock_media_preview_job_queue_state(&self.shared.state);
        let before = state.queue.len();
        state.queue.retain(|queued| queued.job.generation >= generation);
        let pruned = before.saturating_sub(state.queue.len());
        if pruned > 0 {
            self.shared.changed.notify_all();
        }
        pruned
    }

    pub(crate) fn cancel_key(&self, key: &MediaPreviewKey) -> usize {
        let mut state = lock_media_preview_job_queue_state(&self.shared.state);
        let before = state.queue.len();
        state.queue.retain(|queued| &queued.job.key != key);
        let canceled = before.saturating_sub(state.queue.len());
        if canceled > 0 {
            self.shared.changed.notify_all();
        }
        canceled
    }

    pub(crate) fn diagnostics(&self) -> MediaPreviewJobQueueDiagnostics {
        let state = lock_media_preview_job_queue_state(&self.shared.state);
        media_preview_job_queue_diagnostics_locked(&state)
    }

    pub(crate) fn enqueue(&self, job: MediaPreviewJob) -> MediaPreviewJobEnqueueStatus {
        let mut state = lock_media_preview_job_queue_state(&self.shared.state);
        if state.closed {
            return MediaPreviewJobEnqueueStatus::Closed;
        }
        let priority = job.priority;
        if !priority.accepts_access_mode(job.access_mode) {
            return MediaPreviewJobEnqueueStatus::DroppedInvalidAccessMode;
        }

        let mut evicted_prefetch = None;
        let mut evicted_still = None;
        if state.queue.len() >= self.shared.capacity {
            if priority == MediaPreviewRequestPriority::Current {
                if let Some(index) = state
                    .queue
                    .iter()
                    .position(|queued| queued.priority == MediaPreviewRequestPriority::Prefetch)
                {
                    let evicted = state.queue.remove(index);
                    evicted_prefetch = evicted.map(|queued| Box::new(queued.job.key));
                } else if job.access_mode != PreviewDecodeAccessMode::RandomAccessStillFrame {
                    if let Some(index) = state.queue.iter().position(|queued| {
                        queued.priority == MediaPreviewRequestPriority::Current
                            && queued.job.access_mode
                                == PreviewDecodeAccessMode::RandomAccessStillFrame
                    }) {
                        let evicted = state.queue.remove(index);
                        evicted_still = evicted.map(|queued| Box::new(queued.job.key));
                    } else {
                        return MediaPreviewJobEnqueueStatus::DroppedFull;
                    }
                } else {
                    return MediaPreviewJobEnqueueStatus::DroppedFull;
                }
            } else {
                return MediaPreviewJobEnqueueStatus::DroppedFull;
            }
        }

        state.queue.push_back(QueuedMediaPreviewJob { job, priority });
        // Workers have lane-specific eligibility. Wake all workers so a queued
        // item for one lane cannot remain asleep behind workers that are
        // waiting on a different lane.
        self.shared.changed.notify_all();
        MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch, evicted_still }
    }

    pub(crate) fn promote(
        &self,
        key: &MediaPreviewKey,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        generation: u64,
        source_secs: f64,
        enqueued_at: Instant,
        deadline_at: Option<Instant>,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
        adaptive_hints: PreviewDecodeAdaptiveHints,
        hardware_decode_request: PreviewHardwareDecodeRequest,
    ) -> MediaPreviewJobPromoteStatus {
        if priority != MediaPreviewRequestPriority::Current {
            return MediaPreviewJobPromoteStatus::default();
        }
        let mut state = lock_media_preview_job_queue_state(&self.shared.state);
        let Some(queued) = state.queue.iter_mut().find(|queued| &queued.job.key == key) else {
            return MediaPreviewJobPromoteStatus::default();
        };
        let previous = queued.priority;
        let previous_access_mode = queued.job.access_mode;
        let previous_generation = queued.job.generation;
        queued.job.access_mode =
            promoted_access_mode(previous, previous_access_mode, priority, access_mode);
        queued.priority = queued.priority.promote_with(priority);
        queued.job.priority = queued.priority;
        queued.job.generation = generation;
        queued.job.source_secs = source_secs;
        queued.job.adaptive_hints = adaptive_hints;
        queued.job.hardware_decode_request = hardware_decode_request;
        queued.job.enqueued_at = enqueued_at;
        queued.job.deadline_at = deadline_at;
        queued.job.demand_identity = demand_identity;
        let priority_promoted = previous != queued.priority;
        let access_mode_changed = previous_access_mode != queued.job.access_mode;
        let generation_changed = previous_generation != queued.job.generation;
        let updated = priority_promoted || access_mode_changed || generation_changed;
        if updated {
            self.shared.changed.notify_all();
        }
        MediaPreviewJobPromoteStatus {
            updated,
            priority_promoted,
            access_mode_changed,
            generation_changed,
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
            MediaPreviewJobQueueReceive::DroppedExpiredPlaybackCurrent(_) => None,
        }
    }

    pub(crate) fn recv_for_worker_outcome(
        &self,
        lane: MediaPreviewWorkerLane,
    ) -> Option<MediaPreviewJobQueueReceive> {
        let mut state = lock_media_preview_job_queue_state(&self.shared.state);
        loop {
            if let Some(index) = next_media_preview_job_index(&state.queue, lane) {
                let Some(queued) = state.queue.remove(index) else {
                    continue;
                };
                if media_preview_job_is_expired_playback_current(&queued, Instant::now()) {
                    state.dropped_expired_playback_current_jobs =
                        state.dropped_expired_playback_current_jobs.saturating_add(1);
                    return Some(MediaPreviewJobQueueReceive::DroppedExpiredPlaybackCurrent(
                        queued.job,
                    ));
                }
                return Some(MediaPreviewJobQueueReceive::Job(queued.job));
            }
            if state.closed {
                return None;
            }
            state = match self.shared.changed.wait(state) {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }
}

fn media_preview_job_queue_diagnostics_locked(
    state: &MediaPreviewJobQueueState,
) -> MediaPreviewJobQueueDiagnostics {
    let mut diagnostics = MediaPreviewJobQueueDiagnostics {
        queued_jobs: state.queue.len(),
        dropped_expired_playback_current_jobs: state.dropped_expired_playback_current_jobs,
        closed: state.closed,
        ..MediaPreviewJobQueueDiagnostics::default()
    };
    let now = Instant::now();
    for queued in &state.queue {
        if MediaPreviewWorkerLane::Any.accepts(queued.job.access_mode) {
            diagnostics.queued_any_lane_eligible_jobs =
                diagnostics.queued_any_lane_eligible_jobs.saturating_add(1);
        }
        if MediaPreviewWorkerLane::Playback.accepts(queued.job.access_mode) {
            diagnostics.queued_playback_lane_eligible_jobs =
                diagnostics.queued_playback_lane_eligible_jobs.saturating_add(1);
        }
        if MediaPreviewWorkerLane::Scrub.accepts(queued.job.access_mode) {
            diagnostics.queued_scrub_lane_eligible_jobs =
                diagnostics.queued_scrub_lane_eligible_jobs.saturating_add(1);
        }
        if MediaPreviewWorkerLane::Still.accepts(queued.job.access_mode) {
            diagnostics.queued_still_lane_eligible_jobs =
                diagnostics.queued_still_lane_eligible_jobs.saturating_add(1);
        }
        if MediaPreviewWorkerLane::Interactive.accepts(queued.job.access_mode) {
            diagnostics.queued_interactive_lane_eligible_jobs =
                diagnostics.queued_interactive_lane_eligible_jobs.saturating_add(1);
        }
        match queued.priority {
            MediaPreviewRequestPriority::Current => {
                diagnostics.queued_current_jobs = diagnostics.queued_current_jobs.saturating_add(1);
                if media_preview_job_is_expired_playback_current(queued, now) {
                    diagnostics.queued_expired_playback_current_jobs =
                        diagnostics.queued_expired_playback_current_jobs.saturating_add(1);
                }
            }
            MediaPreviewRequestPriority::Prefetch => {
                diagnostics.queued_prefetch_jobs =
                    diagnostics.queued_prefetch_jobs.saturating_add(1);
            }
        }
        match queued.job.access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => {
                diagnostics.queued_playback_cursor_jobs =
                    diagnostics.queued_playback_cursor_jobs.saturating_add(1);
            }
            PreviewDecodeAccessMode::ScrubCursor => {
                diagnostics.queued_scrub_cursor_jobs =
                    diagnostics.queued_scrub_cursor_jobs.saturating_add(1);
            }
            PreviewDecodeAccessMode::RandomAccessStillFrame => {
                diagnostics.queued_random_access_still_jobs =
                    diagnostics.queued_random_access_still_jobs.saturating_add(1);
            }
        }
    }
    diagnostics
}

fn next_media_preview_job_index(
    queue: &VecDeque<QueuedMediaPreviewJob>,
    lane: MediaPreviewWorkerLane,
) -> Option<usize> {
    let now = Instant::now();
    let eligible = |queued: &QueuedMediaPreviewJob| lane.accepts(queued.job.access_mode);
    let current_job =
        |queued: &QueuedMediaPreviewJob| queued.priority == MediaPreviewRequestPriority::Current;
    queue
        .iter()
        .enumerate()
        .filter(|(_, queued)| current_job(queued))
        .min_by_key(|(_, queued)| media_preview_current_job_selection_key(queued, lane, now))
        .map(|(index, _)| index)
        .or_else(|| queue.iter().position(eligible))
}

fn media_preview_current_job_selection_key(
    queued: &QueuedMediaPreviewJob,
    lane: MediaPreviewWorkerLane,
    now: Instant,
) -> (bool, bool, u8) {
    (
        media_preview_job_deadline_expired_at(queued.job.deadline_at, now),
        !lane.accepts(queued.job.access_mode),
        media_preview_current_job_rank(queued.job.access_mode),
    )
}

fn media_preview_job_deadline_expired_at(deadline_at: Option<Instant>, now: Instant) -> bool {
    deadline_at.is_some_and(|deadline| now >= deadline)
}

fn media_preview_job_is_expired_playback_current(
    queued: &QueuedMediaPreviewJob,
    now: Instant,
) -> bool {
    queued.priority == MediaPreviewRequestPriority::Current
        && queued.job.access_mode == PreviewDecodeAccessMode::PlaybackCursor
        && media_preview_job_deadline_expired_at(queued.job.deadline_at, now)
}

fn media_preview_current_job_rank(access_mode: PreviewDecodeAccessMode) -> u8 {
    match access_mode {
        PreviewDecodeAccessMode::ScrubCursor => 0,
        PreviewDecodeAccessMode::PlaybackCursor => 1,
        PreviewDecodeAccessMode::RandomAccessStillFrame => 2,
    }
}

fn lock_media_preview_job_queue_state(
    state: &Mutex<MediaPreviewJobQueueState>,
) -> std::sync::MutexGuard<'_, MediaPreviewJobQueueState> {
    match state.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
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
    Scrub,
    Still,
    Interactive,
}

impl MediaPreviewWorkerLane {
    pub(crate) fn accepts(self, access_mode: PreviewDecodeAccessMode) -> bool {
        match self {
            Self::Any => true,
            Self::Playback => access_mode == PreviewDecodeAccessMode::PlaybackCursor,
            Self::Scrub => access_mode == PreviewDecodeAccessMode::ScrubCursor,
            Self::Still => access_mode == PreviewDecodeAccessMode::RandomAccessStillFrame,
            Self::Interactive => access_mode != PreviewDecodeAccessMode::PlaybackCursor,
        }
    }
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
            1 if worker_count >= 3 => MediaPreviewWorkerLane::Scrub,
            2 if worker_count >= 3 => MediaPreviewWorkerLane::Still,
            _ => MediaPreviewWorkerLane::Interactive,
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
            scheduler: mondrian_playback::FrameRequestScheduler::with_max_pending(max_pending),
        }
    }

    pub(crate) fn begin_generation(&self) -> u64 {
        self.scheduler.begin_generation()
    }

    #[cfg(test)]
    pub(crate) fn request(
        &self,
        key: MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
    ) -> MediaPreviewRequestStatus {
        self.request_with_demand_identity(key, generation, priority, access_mode, None)
    }

    pub(crate) fn request_with_demand_identity(
        &self,
        key: MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> MediaPreviewRequestStatus {
        match self.scheduler.request(
            key,
            generation,
            frame_work_priority(priority),
            frame_work_class(access_mode),
            demand_identity,
        ) {
            mondrian_playback::FrameRequestAdmission::Scheduled {
                evicted_prefetch,
                evicted_still,
            } => MediaPreviewRequestStatus::Scheduled {
                evicted_prefetch: evicted_prefetch.map(Box::new),
                evicted_still: evicted_still.map(Box::new),
            },
            mondrian_playback::FrameRequestAdmission::AlreadyPending { work_class_changed } => {
                MediaPreviewRequestStatus::AlreadyPending {
                    access_mode_changed: work_class_changed,
                }
            }
            mondrian_playback::FrameRequestAdmission::DroppedBackpressure => {
                MediaPreviewRequestStatus::DroppedBackpressure
            }
            mondrian_playback::FrameRequestAdmission::DroppedInvalidClass => {
                MediaPreviewRequestStatus::DroppedInvalidAccessMode
            }
        }
    }

    pub(crate) fn should_decode(
        &self,
        key: &MediaPreviewKey,
        access_mode: PreviewDecodeAccessMode,
    ) -> bool {
        self.scheduler.should_execute(key, frame_work_class(access_mode))
    }

    pub(crate) fn is_decode_current(
        &self,
        key: &MediaPreviewKey,
        generation: u64,
        access_mode: PreviewDecodeAccessMode,
    ) -> bool {
        self.scheduler
            .is_execution_current(key, generation, frame_work_class(access_mode))
    }

    pub(crate) fn has_pending_current_request_other_than(&self, key: &MediaPreviewKey) -> bool {
        self.scheduler.has_other_current(key, false)
    }

    pub(crate) fn has_pending_realtime_current_request_other_than(
        &self,
        key: &MediaPreviewKey,
    ) -> bool {
        self.scheduler.has_other_current(key, true)
    }

    pub(crate) fn complete(
        &self,
        key: &MediaPreviewKey,
        result_generation: u64,
        access_mode: PreviewDecodeAccessMode,
    ) -> MediaPreviewCompletionStatus {
        match self.scheduler.complete(key, result_generation, frame_work_class(access_mode)) {
            mondrian_playback::FrameRequestCompletion::Current => {
                MediaPreviewCompletionStatus::Current
            }
            mondrian_playback::FrameRequestCompletion::CacheOnly => {
                MediaPreviewCompletionStatus::CacheOnly
            }
            mondrian_playback::FrameRequestCompletion::Stale => MediaPreviewCompletionStatus::Stale,
        }
    }

    pub(crate) fn cancel(&self, key: &MediaPreviewKey) {
        self.scheduler.cancel(key);
    }

    pub(crate) fn expire_realtime_current_older_than(
        &self,
        max_age: Duration,
    ) -> Vec<ExpiredMediaPreviewRequest> {
        self.scheduler
            .expire_realtime_current_older_than(max_age)
            .into_iter()
            .map(|request| ExpiredMediaPreviewRequest {
                key: request.key,
                access_mode: preview_access_mode(request.work_class),
                demand_identity: request.demand_identity,
            })
            .collect()
    }

    pub(crate) fn pending_still_for_realtime_current(
        &self,
        protected_key: &MediaPreviewKey,
    ) -> Vec<MediaPreviewKey> {
        self.scheduler.pending_still_except(protected_key)
    }

    pub(crate) fn cancel_preempted_still_for_realtime_current(
        &self,
        key: &MediaPreviewKey,
    ) -> bool {
        self.scheduler.cancel_preempted_still(key)
    }

    pub(crate) fn cancel_all(&self) -> u64 {
        self.scheduler.cancel_all()
    }

    pub(crate) fn prune_obsolete(&self) {
        self.scheduler.prune_obsolete();
    }

    pub(crate) fn diagnostics(&self) -> MediaPreviewSchedulerDiagnostics {
        let state = self.scheduler.diagnostics();
        MediaPreviewSchedulerDiagnostics {
            latest_generation: state.latest_generation,
            pending_requests: state.pending_requests,
            scheduled_requests: state.scheduled_requests,
            already_pending_requests: state.already_pending_requests,
            already_pending_access_mode_changes: state.already_pending_class_changes,
            dropped_backpressure_requests: state.dropped_backpressure_requests,
            dropped_invalid_access_mode_requests: state.dropped_invalid_class_requests,
            dropped_obsolete_generation_requests: state.dropped_obsolete_generation_requests,
            dropped_pending_window_requests: state.dropped_pending_window_requests,
            skipped_decode_jobs: state.skipped_work,
            skipped_decode_missing_pending: state.skipped_missing_pending,
            skipped_decode_access_mode_mismatch: state.skipped_class_mismatch,
            skipped_decode_obsolete_generation: state.skipped_obsolete_generation,
            completed_current_results: state.completed_current,
            completed_cache_only_results: state.completed_cache_only,
            completed_cache_only_missing_pending: state.completed_cache_only_missing_pending,
            completed_cache_only_access_mode_mismatch: state.completed_cache_only_class_mismatch,
            completed_stale_results: state.completed_stale,
            completed_stale_missing_pending: state.completed_stale_missing_pending,
            completed_stale_access_mode_mismatch: state.completed_stale_class_mismatch,
            completed_stale_obsolete_generation: state.completed_stale_obsolete_generation,
            canceled_requests: state.canceled_requests,
            pruned_obsolete_requests: state.pruned_obsolete_requests,
            evicted_prefetch_requests: state.evicted_prefetch_requests,
            evicted_still_requests: state.evicted_still_requests,
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.scheduler.pending_len()
    }

    #[cfg(test)]
    pub(crate) fn has_pending_key(&self, key: &MediaPreviewKey) -> bool {
        self.scheduler.has_pending_key(key)
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

fn frame_work_class(access_mode: PreviewDecodeAccessMode) -> mondrian_playback::FrameWorkClass {
    match access_mode {
        PreviewDecodeAccessMode::PlaybackCursor => mondrian_playback::FrameWorkClass::Playback,
        PreviewDecodeAccessMode::ScrubCursor => mondrian_playback::FrameWorkClass::Interactive,
        PreviewDecodeAccessMode::RandomAccessStillFrame => mondrian_playback::FrameWorkClass::Still,
    }
}

fn preview_access_mode(work_class: mondrian_playback::FrameWorkClass) -> PreviewDecodeAccessMode {
    match work_class {
        mondrian_playback::FrameWorkClass::Playback => PreviewDecodeAccessMode::PlaybackCursor,
        mondrian_playback::FrameWorkClass::Interactive => PreviewDecodeAccessMode::ScrubCursor,
        mondrian_playback::FrameWorkClass::Still => PreviewDecodeAccessMode::RandomAccessStillFrame,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn source_micros(source_secs: f64) -> i64 {
        (source_secs.max(0.0) * 1_000_000.0).round() as i64
    }

    fn test_media_key(source_frame: i64) -> MediaPreviewKey {
        MediaPreviewKey {
            asset_id: AssetId::new(),
            path: PathBuf::from(format!("E:/media/{source_frame}.mov")),
            fingerprint: None,
            source_frame,
            source_micros: source_micros(source_frame as f64),
            target_width: 320,
            target_height: 180,
            input_color_space: ColorSpace::Rec709,
            input_video_range: DecodedVideoRange::Limited,
            working_color_space: WorkingColorSpace::LinearRec709,
            tone_map: false,
            engine: ColorEngine::MondrianSmart,
            ocio_generation: mondrian_core::ocio_config_generation(),
        }
    }

    fn test_media_job(
        key: MediaPreviewKey,
        source_secs: f64,
        priority: MediaPreviewRequestPriority,
    ) -> MediaPreviewJob {
        test_media_job_with_generation(key, source_secs, 1, priority)
    }

    fn test_media_job_with_generation(
        key: MediaPreviewKey,
        source_secs: f64,
        generation: u64,
        priority: MediaPreviewRequestPriority,
    ) -> MediaPreviewJob {
        MediaPreviewJob {
            key,
            source_secs,
            generation,
            priority,
            access_mode: test_access_mode_for_priority(priority),
            adaptive_hints: PreviewDecodeAdaptiveHints::default(),
            hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
            enqueued_at: Instant::now(),
            deadline_at: None,
            demand_identity: None,
        }
    }

    fn test_access_mode_for_priority(
        priority: MediaPreviewRequestPriority,
    ) -> PreviewDecodeAccessMode {
        match priority {
            MediaPreviewRequestPriority::Current => PreviewDecodeAccessMode::ScrubCursor,
            MediaPreviewRequestPriority::Prefetch => PreviewDecodeAccessMode::PlaybackCursor,
        }
    }

    fn test_scheduler_request(
        scheduler: &MediaPreviewScheduler,
        key: MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
    ) -> MediaPreviewRequestStatus {
        scheduler.request(
            key,
            generation,
            priority,
            test_access_mode_for_priority(priority),
        )
    }

    fn scheduled_request() -> MediaPreviewRequestStatus {
        MediaPreviewRequestStatus::Scheduled { evicted_prefetch: None, evicted_still: None }
    }

    fn test_scheduler_should_decode(
        scheduler: &MediaPreviewScheduler,
        key: &MediaPreviewKey,
        priority: MediaPreviewRequestPriority,
    ) -> bool {
        scheduler.should_decode(key, test_access_mode_for_priority(priority))
    }

    fn test_scheduler_is_decode_current(
        scheduler: &MediaPreviewScheduler,
        key: &MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
    ) -> bool {
        scheduler.is_decode_current(key, generation, test_access_mode_for_priority(priority))
    }

    fn test_scheduler_complete(
        scheduler: &MediaPreviewScheduler,
        key: &MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
    ) -> bool {
        scheduler
            .complete(key, generation, test_access_mode_for_priority(priority))
            .is_current()
    }

    fn test_scheduler_completion(
        scheduler: &MediaPreviewScheduler,
        key: &MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
    ) -> MediaPreviewCompletionStatus {
        scheduler.complete(key, generation, test_access_mode_for_priority(priority))
    }

    #[test]
    fn media_preview_scheduler_rejects_non_playback_prefetch_requests() {
        let scheduler = MediaPreviewScheduler::default();
        let generation = scheduler.begin_generation();
        let scrub = test_media_key(1);
        let still = test_media_key(2);

        assert_eq!(
            scheduler.request(
                scrub,
                generation,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            MediaPreviewRequestStatus::DroppedInvalidAccessMode
        );
        assert_eq!(
            scheduler.request(
                still,
                generation,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ),
            MediaPreviewRequestStatus::DroppedInvalidAccessMode
        );

        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.pending_requests, 0);
        assert_eq!(diagnostics.scheduled_requests, 0);
        assert_eq!(diagnostics.dropped_invalid_access_mode_requests, 2);
        assert_eq!(diagnostics.dropped_backpressure_requests, 0);
    }

    #[test]
    fn media_preview_scheduler_skips_obsolete_generations() {
        let scheduler = MediaPreviewScheduler::default();
        let first_generation = scheduler.begin_generation();
        let key = test_media_key(1);
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                key.clone(),
                first_generation,
                MediaPreviewRequestPriority::Current,
            ),
            scheduled_request()
        );

        scheduler.begin_generation();

        assert!(!test_scheduler_should_decode(
            &scheduler,
            &key,
            MediaPreviewRequestPriority::Current
        ));
        assert_eq!(scheduler.pending_len(), 0);
        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.skipped_decode_obsolete_generation, 1);
        assert_eq!(diagnostics.skipped_decode_missing_pending, 0);
        assert_eq!(diagnostics.skipped_decode_access_mode_mismatch, 0);
    }

    #[test]
    fn media_preview_scheduler_replaces_playback_prefetch_with_scrub_current() {
        let scheduler = MediaPreviewScheduler::default();
        let first_generation = scheduler.begin_generation();
        let key = test_media_key(1);
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                key.clone(),
                first_generation,
                MediaPreviewRequestPriority::Prefetch,
            ),
            scheduled_request()
        );

        let second_generation = scheduler.begin_generation();
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                key.clone(),
                second_generation,
                MediaPreviewRequestPriority::Current,
            ),
            MediaPreviewRequestStatus::AlreadyPending { access_mode_changed: true }
        );

        assert!(test_scheduler_should_decode(
            &scheduler,
            &key,
            MediaPreviewRequestPriority::Current
        ));
        assert!(!test_scheduler_complete(
            &scheduler,
            &key,
            first_generation,
            MediaPreviewRequestPriority::Prefetch
        ));
        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.already_pending_access_mode_changes, 1);
        assert_eq!(diagnostics.completed_stale_access_mode_mismatch, 1);
        assert_eq!(scheduler.pending_len(), 1);
        assert!(test_scheduler_complete(
            &scheduler,
            &key,
            second_generation,
            MediaPreviewRequestPriority::Current
        ));
        assert_eq!(scheduler.pending_len(), 0);
    }

    #[test]
    fn media_preview_scheduler_reports_other_pending_current_pressure() {
        let scheduler = MediaPreviewScheduler::default();
        let generation = scheduler.begin_generation();
        let prefetch = test_media_key(1);
        let current = test_media_key(2);

        assert_eq!(
            test_scheduler_request(
                &scheduler,
                prefetch.clone(),
                generation,
                MediaPreviewRequestPriority::Prefetch,
            ),
            scheduled_request()
        );
        assert!(!scheduler.has_pending_current_request_other_than(&prefetch));

        assert_eq!(
            test_scheduler_request(
                &scheduler,
                current.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
            ),
            scheduled_request()
        );

        assert!(scheduler.has_pending_current_request_other_than(&prefetch));
        assert!(!scheduler.has_pending_current_request_other_than(&current));
    }

    #[test]
    fn media_preview_scheduler_reports_realtime_current_pressure_for_still_work() {
        let scheduler = MediaPreviewScheduler::with_max_pending(3);
        let generation = scheduler.begin_generation();
        let still = test_media_key(1);
        let other_still = test_media_key(2);
        let scrub = test_media_key(3);

        assert_eq!(
            scheduler.request(
                still.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ),
            scheduled_request()
        );
        assert_eq!(
            scheduler.request(
                other_still.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ),
            scheduled_request()
        );
        assert!(!scheduler.has_pending_realtime_current_request_other_than(&still));

        assert_eq!(
            scheduler.request(
                scrub.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            scheduled_request()
        );

        assert!(scheduler.has_pending_realtime_current_request_other_than(&still));
        assert!(!scheduler.has_pending_realtime_current_request_other_than(&scrub));
    }

    #[test]
    fn media_preview_scheduler_expires_only_stalled_realtime_current_requests() {
        let scheduler = MediaPreviewScheduler::with_max_pending(3);
        let generation = scheduler.begin_generation();
        let scrub = test_media_key(1);
        let playback = test_media_key(2);
        let still = test_media_key(3);

        assert_eq!(
            scheduler.request(
                scrub.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            scheduled_request()
        );
        assert_eq!(
            scheduler.request(
                playback.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
            ),
            scheduled_request()
        );
        assert_eq!(
            scheduler.request(
                still.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ),
            scheduled_request()
        );

        assert!(scheduler.expire_realtime_current_older_than(Duration::from_secs(60)).is_empty());
        assert_eq!(scheduler.pending_len(), 3);

        let expired = scheduler.expire_realtime_current_older_than(Duration::ZERO);
        assert_eq!(expired.len(), 2);
        assert!(expired.iter().any(|request| request.key == scrub));
        assert!(expired.iter().any(|request| request.key == playback));
        assert!(!expired.iter().any(|request| request.key == still));
        assert!(!scheduler.has_pending_key(&scrub));
        assert!(!scheduler.has_pending_key(&playback));
        assert!(scheduler.has_pending_key(&still));
        assert_eq!(scheduler.diagnostics().canceled_requests, 2);
    }

    #[test]
    fn pending_playback_identity_tracks_latest_demand_for_same_media_key() {
        let scheduler = MediaPreviewScheduler::with_max_pending(1);
        let generation = scheduler.begin_generation();
        let key = test_media_key(4);
        let mut engine = mondrian_playback::PlaybackEngine::new(
            mondrian_core::Rational::new(1, 25),
            mondrian_playback::PlaybackPolicy::default(),
        )
        .expect("playback engine");
        engine
            .play(10, mondrian_playback::MonotonicTimestamp::ZERO)
            .expect("first demand");
        let first = engine.frame_demand().expect("first frame demand").identity();
        engine
            .play(10, mondrian_playback::MonotonicTimestamp::ZERO)
            .expect("second demand");
        let second = engine.frame_demand().expect("second frame demand").identity();
        assert_ne!(first, second);

        assert_eq!(
            scheduler.request_with_demand_identity(
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                Some(first),
            ),
            scheduled_request()
        );
        assert_eq!(
            scheduler.request_with_demand_identity(
                key,
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::PlaybackCursor,
                Some(second),
            ),
            MediaPreviewRequestStatus::AlreadyPending { access_mode_changed: false }
        );

        let expired = scheduler.expire_realtime_current_older_than(Duration::ZERO);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].demand_identity, Some(second));
    }

    #[test]
    fn media_preview_scheduler_keeps_same_key_in_flight_decode_current_after_rerequest() {
        let scheduler = MediaPreviewScheduler::default();
        let first_generation = scheduler.begin_generation();
        let key = test_media_key(1);

        assert_eq!(
            test_scheduler_request(
                &scheduler,
                key.clone(),
                first_generation,
                MediaPreviewRequestPriority::Current,
            ),
            scheduled_request()
        );
        assert!(test_scheduler_is_decode_current(
            &scheduler,
            &key,
            first_generation,
            MediaPreviewRequestPriority::Current
        ));

        let second_generation = scheduler.begin_generation();
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                key.clone(),
                second_generation,
                MediaPreviewRequestPriority::Current,
            ),
            MediaPreviewRequestStatus::AlreadyPending { access_mode_changed: false }
        );

        assert!(
            test_scheduler_is_decode_current(
                &scheduler,
                &key,
                first_generation,
                MediaPreviewRequestPriority::Current
            ),
            "same frame/key decode must survive UI generation refreshes"
        );
        assert!(test_scheduler_complete(
            &scheduler,
            &key,
            first_generation,
            MediaPreviewRequestPriority::Current
        ));
    }

    #[test]
    fn media_preview_scheduler_prunes_obsolete_pending_requests() {
        let scheduler = MediaPreviewScheduler::default();
        let first_generation = scheduler.begin_generation();
        let first = test_media_key(1);
        let second = test_media_key(2);
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                first.clone(),
                first_generation,
                MediaPreviewRequestPriority::Current,
            ),
            scheduled_request()
        );
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                second.clone(),
                first_generation,
                MediaPreviewRequestPriority::Current,
            ),
            scheduled_request()
        );

        scheduler.begin_generation();
        scheduler.prune_obsolete();

        assert_eq!(scheduler.pending_len(), 0);
        assert!(!test_scheduler_should_decode(
            &scheduler,
            &first,
            MediaPreviewRequestPriority::Current
        ));
        assert!(!test_scheduler_should_decode(
            &scheduler,
            &second,
            MediaPreviewRequestPriority::Current
        ));
    }

    #[test]
    fn media_preview_scheduler_drops_new_requests_when_pending_window_is_full() {
        let scheduler = MediaPreviewScheduler::with_max_pending(2);
        let generation = scheduler.begin_generation();
        let first = test_media_key(1);
        let second = test_media_key(2);
        let third = test_media_key(3);

        assert_eq!(
            test_scheduler_request(
                &scheduler,
                first,
                generation,
                MediaPreviewRequestPriority::Current
            ),
            scheduled_request()
        );
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                second,
                generation,
                MediaPreviewRequestPriority::Current
            ),
            scheduled_request()
        );
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                third,
                generation,
                MediaPreviewRequestPriority::Current
            ),
            MediaPreviewRequestStatus::DroppedBackpressure
        );

        assert_eq!(scheduler.pending_len(), 2);
    }

    #[test]
    fn media_preview_scheduler_rejects_obsolete_generation_requests() {
        let scheduler = MediaPreviewScheduler::default();
        let first_generation = scheduler.begin_generation();
        scheduler.begin_generation();

        assert_eq!(
            test_scheduler_request(
                &scheduler,
                test_media_key(1),
                first_generation,
                MediaPreviewRequestPriority::Current,
            ),
            MediaPreviewRequestStatus::DroppedBackpressure
        );
        assert_eq!(scheduler.pending_len(), 0);
        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.dropped_backpressure_requests, 1);
        assert_eq!(diagnostics.dropped_obsolete_generation_requests, 1);
        assert_eq!(diagnostics.dropped_pending_window_requests, 0);
    }

    #[test]
    fn media_preview_scheduler_reports_request_and_drop_diagnostics() {
        let scheduler = MediaPreviewScheduler::with_max_pending(1);
        let generation = scheduler.begin_generation();
        let first = test_media_key(1);
        let second = test_media_key(2);

        assert_eq!(
            test_scheduler_request(
                &scheduler,
                first.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
            ),
            scheduled_request()
        );
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                first.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
            ),
            MediaPreviewRequestStatus::AlreadyPending { access_mode_changed: false }
        );
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                second.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
            ),
            MediaPreviewRequestStatus::DroppedBackpressure
        );

        scheduler.begin_generation();
        scheduler.prune_obsolete();
        assert!(!test_scheduler_should_decode(
            &scheduler,
            &first,
            MediaPreviewRequestPriority::Current
        ));
        assert!(!test_scheduler_complete(
            &scheduler,
            &second,
            generation,
            MediaPreviewRequestPriority::Current
        ));

        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.latest_generation, generation + 1);
        assert_eq!(diagnostics.pending_requests, 0);
        assert_eq!(diagnostics.scheduled_requests, 1);
        assert_eq!(diagnostics.already_pending_requests, 1);
        assert_eq!(diagnostics.already_pending_access_mode_changes, 0);
        assert_eq!(diagnostics.dropped_backpressure_requests, 1);
        assert_eq!(diagnostics.dropped_obsolete_generation_requests, 0);
        assert_eq!(diagnostics.dropped_pending_window_requests, 1);
        assert_eq!(diagnostics.pruned_obsolete_requests, 1);
        assert_eq!(diagnostics.skipped_decode_jobs, 1);
        assert_eq!(diagnostics.skipped_decode_missing_pending, 1);
        assert_eq!(diagnostics.skipped_decode_access_mode_mismatch, 0);
        assert_eq!(diagnostics.skipped_decode_obsolete_generation, 0);
        assert_eq!(diagnostics.completed_current_results, 0);
        assert_eq!(diagnostics.completed_stale_results, 1);
        assert_eq!(diagnostics.completed_stale_missing_pending, 1);
        assert_eq!(diagnostics.completed_stale_access_mode_mismatch, 0);
        assert_eq!(diagnostics.completed_stale_obsolete_generation, 0);
    }

    #[test]
    fn media_preview_scheduler_reports_obsolete_completion_reason() {
        let scheduler = MediaPreviewScheduler::default();
        let generation = scheduler.begin_generation();
        let key = test_media_key(1);
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
            ),
            scheduled_request()
        );

        scheduler.begin_generation();

        assert!(!test_scheduler_complete(
            &scheduler,
            &key,
            generation,
            MediaPreviewRequestPriority::Current
        ));
        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.completed_stale_results, 1);
        assert_eq!(diagnostics.completed_stale_obsolete_generation, 1);
        assert_eq!(diagnostics.completed_stale_missing_pending, 0);
        assert_eq!(diagnostics.completed_stale_access_mode_mismatch, 0);
    }

    #[test]
    fn media_preview_scheduler_cancel_all_obsoletes_in_flight_decode() {
        let scheduler = MediaPreviewScheduler::with_max_pending(4);
        let key = test_media_key(1);
        let generation = scheduler.begin_generation();

        assert_eq!(
            test_scheduler_request(
                &scheduler,
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current
            ),
            scheduled_request()
        );
        assert!(test_scheduler_is_decode_current(
            &scheduler,
            &key,
            generation,
            MediaPreviewRequestPriority::Current
        ));

        let canceled_generation = scheduler.cancel_all();

        assert!(!test_scheduler_is_decode_current(
            &scheduler,
            &key,
            generation,
            MediaPreviewRequestPriority::Current
        ));
        assert_eq!(
            canceled_generation,
            scheduler.diagnostics().latest_generation
        );
        assert!(canceled_generation > generation);
        assert_eq!(scheduler.pending_len(), 0);
        assert_eq!(scheduler.diagnostics().canceled_requests, 1);
    }

    #[test]
    fn media_preview_scheduler_canceled_same_generation_completion_is_cache_only() {
        let scheduler = MediaPreviewScheduler::with_max_pending(4);
        let key = test_media_key(1);
        let generation = scheduler.begin_generation();

        assert_eq!(
            test_scheduler_request(
                &scheduler,
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Prefetch,
            ),
            scheduled_request()
        );

        scheduler.cancel(&key);

        assert_eq!(
            test_scheduler_completion(
                &scheduler,
                &key,
                generation,
                MediaPreviewRequestPriority::Prefetch
            ),
            MediaPreviewCompletionStatus::CacheOnly
        );
        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.completed_current_results, 0);
        assert_eq!(diagnostics.completed_cache_only_results, 1);
        assert_eq!(diagnostics.completed_cache_only_missing_pending, 1);
        assert_eq!(diagnostics.completed_stale_results, 0);
    }

    #[test]
    fn media_preview_scheduler_access_mode_mismatch_is_cache_only_when_latest() {
        let scheduler = MediaPreviewScheduler::with_max_pending(4);
        let key = test_media_key(1);
        let generation = scheduler.begin_generation();

        assert_eq!(
            test_scheduler_request(
                &scheduler,
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Prefetch,
            ),
            scheduled_request()
        );
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                key.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
            ),
            MediaPreviewRequestStatus::AlreadyPending { access_mode_changed: true }
        );

        assert_eq!(
            test_scheduler_completion(
                &scheduler,
                &key,
                generation,
                MediaPreviewRequestPriority::Prefetch,
            ),
            MediaPreviewCompletionStatus::CacheOnly
        );

        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.completed_current_results, 0);
        assert_eq!(diagnostics.completed_cache_only_results, 1);
        assert_eq!(diagnostics.completed_cache_only_access_mode_mismatch, 1);
        assert_eq!(diagnostics.completed_stale_access_mode_mismatch, 0);
        assert_eq!(scheduler.pending_len(), 1);
    }

    #[test]
    fn media_preview_scheduler_current_request_evicts_prefetch_when_window_is_full() {
        let scheduler = MediaPreviewScheduler::with_max_pending(1);
        let generation = scheduler.begin_generation();
        let prefetch = test_media_key(1);
        let current = test_media_key(2);

        assert_eq!(
            test_scheduler_request(
                &scheduler,
                prefetch.clone(),
                generation,
                MediaPreviewRequestPriority::Prefetch,
            ),
            scheduled_request()
        );
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                current.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
            ),
            MediaPreviewRequestStatus::Scheduled {
                evicted_prefetch: Some(Box::new(prefetch.clone())),
                evicted_still: None,
            }
        );

        assert_eq!(scheduler.pending_len(), 1);
        assert!(!test_scheduler_should_decode(
            &scheduler,
            &prefetch,
            MediaPreviewRequestPriority::Prefetch
        ));
        assert!(test_scheduler_should_decode(
            &scheduler,
            &current,
            MediaPreviewRequestPriority::Current
        ));
        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.evicted_prefetch_requests, 1);
        assert_eq!(diagnostics.dropped_backpressure_requests, 0);
    }

    #[test]
    fn media_preview_scheduler_prefetch_does_not_evict_current_request() {
        let scheduler = MediaPreviewScheduler::with_max_pending(1);
        let generation = scheduler.begin_generation();
        let current = test_media_key(1);
        let prefetch = test_media_key(2);

        assert_eq!(
            test_scheduler_request(
                &scheduler,
                current.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
            ),
            scheduled_request()
        );
        assert_eq!(
            test_scheduler_request(
                &scheduler,
                prefetch,
                generation,
                MediaPreviewRequestPriority::Prefetch
            ),
            MediaPreviewRequestStatus::DroppedBackpressure
        );

        assert_eq!(scheduler.pending_len(), 1);
        assert!(test_scheduler_should_decode(
            &scheduler,
            &current,
            MediaPreviewRequestPriority::Current
        ));
        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.evicted_prefetch_requests, 0);
        assert_eq!(diagnostics.dropped_backpressure_requests, 1);
    }

    #[test]
    fn media_preview_scheduler_realtime_current_evicts_pending_still_when_full() {
        let scheduler = MediaPreviewScheduler::with_max_pending(1);
        let generation = scheduler.begin_generation();
        let still = test_media_key(1);
        let scrub = test_media_key(2);

        assert_eq!(
            scheduler.request(
                still.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ),
            scheduled_request()
        );
        assert_eq!(
            scheduler.request(
                scrub.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            MediaPreviewRequestStatus::Scheduled {
                evicted_prefetch: None,
                evicted_still: Some(Box::new(still.clone())),
            }
        );

        assert_eq!(scheduler.pending_len(), 1);
        assert!(!scheduler.should_decode(&still, PreviewDecodeAccessMode::RandomAccessStillFrame));
        assert!(scheduler.should_decode(&scrub, PreviewDecodeAccessMode::ScrubCursor));
        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.evicted_still_requests, 1);
        assert_eq!(diagnostics.dropped_backpressure_requests, 0);
    }

    #[test]
    fn media_preview_scheduler_realtime_current_preempts_pending_still_before_window_is_full() {
        let scheduler = MediaPreviewScheduler::with_max_pending(4);
        let generation = scheduler.begin_generation();
        let still = test_media_key(1);
        let scrub = test_media_key(2);

        assert_eq!(
            scheduler.request(
                still.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ),
            scheduled_request()
        );
        assert_eq!(
            scheduler.request(
                scrub.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            scheduled_request()
        );

        assert_eq!(
            scheduler.pending_still_for_realtime_current(&scrub),
            vec![still.clone()]
        );
        assert!(scheduler.cancel_preempted_still_for_realtime_current(&still));
        assert_eq!(scheduler.pending_len(), 1);
        assert!(!scheduler.should_decode(&still, PreviewDecodeAccessMode::RandomAccessStillFrame));
        assert!(scheduler.should_decode(&scrub, PreviewDecodeAccessMode::ScrubCursor));
        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.evicted_still_requests, 1);
        assert_eq!(diagnostics.dropped_backpressure_requests, 0);
    }

    #[test]
    fn media_preview_scheduler_still_does_not_evict_realtime_current_when_full() {
        let scheduler = MediaPreviewScheduler::with_max_pending(1);
        let generation = scheduler.begin_generation();
        let scrub = test_media_key(1);
        let still = test_media_key(2);

        assert_eq!(
            scheduler.request(
                scrub.clone(),
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
            ),
            scheduled_request()
        );
        assert_eq!(
            scheduler.request(
                still,
                generation,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::RandomAccessStillFrame,
            ),
            MediaPreviewRequestStatus::DroppedBackpressure
        );

        assert_eq!(scheduler.pending_len(), 1);
        assert!(scheduler.should_decode(&scrub, PreviewDecodeAccessMode::ScrubCursor));
        let diagnostics = scheduler.diagnostics();
        assert_eq!(diagnostics.evicted_still_requests, 0);
        assert_eq!(diagnostics.dropped_pending_window_requests, 1);
    }

    #[test]
    fn media_preview_job_queue_current_request_evicts_prefetch_when_full() {
        let (sender, receiver) = media_preview_job_queue(1);
        let prefetch = test_media_key(1);
        let current = test_media_key(2);
        let prefetch_job =
            test_media_job(prefetch.clone(), 1.0, MediaPreviewRequestPriority::Prefetch);
        let current_job =
            test_media_job(current.clone(), 2.0, MediaPreviewRequestPriority::Current);

        assert_eq!(
            sender.enqueue(prefetch_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(current_job),
            MediaPreviewJobEnqueueStatus::Enqueued {
                evicted_prefetch: Some(Box::new(prefetch)),
                evicted_still: None,
            }
        );

        let next = receiver.recv().expect("queued current job");
        assert_eq!(next.key, current);
    }

    #[test]
    fn media_preview_job_queue_realtime_current_evicts_still_when_full() {
        let (sender, receiver) = media_preview_job_queue(1);
        let still = test_media_key(1);
        let scrub = test_media_key(2);
        let mut still_job =
            test_media_job(still.clone(), 1.0, MediaPreviewRequestPriority::Current);
        still_job.access_mode = PreviewDecodeAccessMode::RandomAccessStillFrame;
        let mut scrub_job =
            test_media_job(scrub.clone(), 2.0, MediaPreviewRequestPriority::Current);
        scrub_job.access_mode = PreviewDecodeAccessMode::ScrubCursor;

        assert_eq!(
            sender.enqueue(still_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(scrub_job),
            MediaPreviewJobEnqueueStatus::Enqueued {
                evicted_prefetch: None,
                evicted_still: Some(Box::new(still)),
            }
        );

        let next = receiver.recv().expect("queued scrub job");
        assert_eq!(next.key, scrub);
        assert_eq!(next.access_mode, PreviewDecodeAccessMode::ScrubCursor);
    }

    #[test]
    fn media_preview_job_queue_prioritizes_scrub_before_still_on_shared_lane() {
        let (sender, receiver) = media_preview_job_queue(2);
        let still = test_media_key(1);
        let scrub = test_media_key(2);
        let mut still_job =
            test_media_job(still.clone(), 1.0, MediaPreviewRequestPriority::Current);
        still_job.access_mode = PreviewDecodeAccessMode::RandomAccessStillFrame;
        let mut scrub_job =
            test_media_job(scrub.clone(), 2.0, MediaPreviewRequestPriority::Current);
        scrub_job.access_mode = PreviewDecodeAccessMode::ScrubCursor;

        assert_eq!(
            sender.enqueue(still_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(scrub_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        let scrub_job = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Interactive)
            .expect("interactive lane should prefer scrub over still");
        assert_eq!(scrub_job.key, scrub);
        assert_eq!(scrub_job.access_mode, PreviewDecodeAccessMode::ScrubCursor);
        assert_eq!(receiver.recv().expect("still job").key, still);
    }

    #[test]
    fn media_preview_job_queue_rejects_non_playback_prefetch_jobs() {
        let (sender, receiver) = media_preview_job_queue(2);
        let key = test_media_key(1);
        let mut job = test_media_job(key, 1.0, MediaPreviewRequestPriority::Prefetch);
        job.access_mode = PreviewDecodeAccessMode::RandomAccessStillFrame;

        assert_eq!(
            sender.enqueue(job),
            MediaPreviewJobEnqueueStatus::DroppedInvalidAccessMode
        );

        sender.close();
        assert!(receiver.recv().is_none());
    }

    #[test]
    fn media_preview_job_queue_pops_current_before_prefetch() {
        let (sender, receiver) = media_preview_job_queue(3);
        let first_prefetch = test_media_key(1);
        let current = test_media_key(2);
        let second_prefetch = test_media_key(3);

        assert_eq!(
            sender.enqueue(test_media_job(
                first_prefetch.clone(),
                1.0,
                MediaPreviewRequestPriority::Prefetch,
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(test_media_job(
                current.clone(),
                2.0,
                MediaPreviewRequestPriority::Current
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(test_media_job(
                second_prefetch.clone(),
                3.0,
                MediaPreviewRequestPriority::Prefetch,
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        assert_eq!(receiver.recv().expect("current").key, current);
        assert_eq!(receiver.recv().expect("first prefetch").key, first_prefetch);
        assert_eq!(
            receiver.recv().expect("second prefetch").key,
            second_prefetch
        );
    }

    #[test]
    fn media_preview_job_queue_keeps_playback_cursor_on_playback_worker() {
        let (sender, receiver) = media_preview_job_queue(2);
        let playback = test_media_key(1);
        let still = test_media_key(2);
        let mut playback_job =
            test_media_job(playback.clone(), 1.0, MediaPreviewRequestPriority::Current);
        playback_job.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
        let mut still_job =
            test_media_job(still.clone(), 2.0, MediaPreviewRequestPriority::Current);
        still_job.access_mode = PreviewDecodeAccessMode::RandomAccessStillFrame;

        assert_eq!(
            sender.enqueue(playback_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(still_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        let interactive_job = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Interactive)
            .expect("non-playback worker should skip playback cursor work");
        assert_eq!(interactive_job.key, still);

        let playback_job = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Playback)
            .expect("playback worker should retain playback cursor work");
        assert_eq!(playback_job.key, playback);
    }

    #[test]
    fn media_preview_job_queue_current_work_steals_idle_playback_lane_before_prefetch() {
        let (sender, receiver) = media_preview_job_queue(2);
        let scrub = test_media_key(1);
        let playback_prefetch = test_media_key(2);
        let scrub_job = test_media_job(scrub.clone(), 1.0, MediaPreviewRequestPriority::Current);
        let mut playback_job = test_media_job(
            playback_prefetch.clone(),
            2.0,
            MediaPreviewRequestPriority::Prefetch,
        );
        playback_job.access_mode = PreviewDecodeAccessMode::PlaybackCursor;

        assert_eq!(
            sender.enqueue(scrub_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(playback_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        let scrub_job = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Playback)
            .expect("idle playback lane should help visible current work before prefetch");
        assert_eq!(scrub_job.key, scrub);
        assert_eq!(scrub_job.access_mode, PreviewDecodeAccessMode::ScrubCursor);

        let playback_job = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Playback)
            .expect("playback lane should still own playback prefetch after current work");
        assert_eq!(playback_job.key, playback_prefetch);
        assert_eq!(
            playback_job.access_mode,
            PreviewDecodeAccessMode::PlaybackCursor
        );
    }

    #[test]
    fn media_preview_job_queue_drops_expired_playback_current_at_dequeue() {
        let (sender, receiver) = media_preview_job_queue(2);
        let expired_playback = test_media_key(1);
        let fresh_scrub = test_media_key(2);
        let mut expired_playback_job = test_media_job(
            expired_playback.clone(),
            1.0,
            MediaPreviewRequestPriority::Current,
        );
        expired_playback_job.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
        expired_playback_job.deadline_at =
            Some(Instant::now() - std::time::Duration::from_millis(1));
        let mut fresh_scrub_job = test_media_job(
            fresh_scrub.clone(),
            2.0,
            MediaPreviewRequestPriority::Current,
        );
        fresh_scrub_job.access_mode = PreviewDecodeAccessMode::ScrubCursor;

        assert_eq!(
            sender.enqueue(expired_playback_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(fresh_scrub_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        let scrub_job = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Playback)
            .expect("fresh current scrub should beat expired playback on idle playback lane");
        assert_eq!(scrub_job.key, fresh_scrub);
        assert_eq!(scrub_job.access_mode, PreviewDecodeAccessMode::ScrubCursor);

        match receiver
            .recv_for_worker_outcome(MediaPreviewWorkerLane::Playback)
            .expect("expired playback job should produce a structured queue outcome")
        {
            MediaPreviewJobQueueReceive::DroppedExpiredPlaybackCurrent(expired_job) => {
                assert_eq!(expired_job.key, expired_playback);
                assert_eq!(
                    expired_job.access_mode,
                    PreviewDecodeAccessMode::PlaybackCursor
                );
            }
            MediaPreviewJobQueueReceive::Job(job) => {
                panic!("expired playback job must not be dispatched for decode: {job:?}");
            }
        }
        let diagnostics = sender.diagnostics();
        assert_eq!(diagnostics.queued_expired_playback_current_jobs, 0);
        assert_eq!(diagnostics.dropped_expired_playback_current_jobs, 1);
    }

    #[test]
    fn media_preview_job_queue_diagnostics_counts_expired_playback_current_jobs() {
        let (sender, _receiver) = media_preview_job_queue(4);
        let expired = test_media_key(1);
        let fresh = test_media_key(2);
        let scrub = test_media_key(3);
        let mut expired_job = test_media_job(expired, 1.0, MediaPreviewRequestPriority::Current);
        expired_job.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
        expired_job.deadline_at = Some(Instant::now() - std::time::Duration::from_millis(1));
        let mut fresh_job = test_media_job(fresh, 2.0, MediaPreviewRequestPriority::Current);
        fresh_job.access_mode = PreviewDecodeAccessMode::PlaybackCursor;
        fresh_job.deadline_at = Some(Instant::now() + std::time::Duration::from_secs(1));
        let mut scrub_job = test_media_job(scrub, 3.0, MediaPreviewRequestPriority::Current);
        scrub_job.access_mode = PreviewDecodeAccessMode::ScrubCursor;

        assert_eq!(
            sender.enqueue(expired_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(fresh_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(scrub_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        let diagnostics = sender.diagnostics();

        assert_eq!(diagnostics.queued_current_jobs, 3);
        assert_eq!(diagnostics.queued_playback_cursor_jobs, 2);
        assert_eq!(diagnostics.queued_expired_playback_current_jobs, 1);
    }

    #[test]
    fn media_preview_job_queue_splits_scrub_and_still_on_dedicated_lanes() {
        let (sender, receiver) = media_preview_job_queue(2);
        let still = test_media_key(1);
        let scrub = test_media_key(2);
        let mut still_job =
            test_media_job(still.clone(), 1.0, MediaPreviewRequestPriority::Current);
        still_job.access_mode = PreviewDecodeAccessMode::RandomAccessStillFrame;
        let mut scrub_job =
            test_media_job(scrub.clone(), 2.0, MediaPreviewRequestPriority::Current);
        scrub_job.access_mode = PreviewDecodeAccessMode::ScrubCursor;

        assert_eq!(
            sender.enqueue(still_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(scrub_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        let scrub_job = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Scrub)
            .expect("scrub lane should skip still work");
        assert_eq!(scrub_job.key, scrub);
        assert_eq!(scrub_job.access_mode, PreviewDecodeAccessMode::ScrubCursor);

        let still_job = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Still)
            .expect("still lane should retain still work");
        assert_eq!(still_job.key, still);
        assert_eq!(
            still_job.access_mode,
            PreviewDecodeAccessMode::RandomAccessStillFrame
        );
    }

    #[test]
    fn media_preview_job_queue_promotes_existing_prefetch_to_current() {
        let (sender, receiver) = media_preview_job_queue(2);
        let promoted = test_media_key(1);
        let other_prefetch = test_media_key(2);

        assert_eq!(
            sender.enqueue(test_media_job(
                promoted.clone(),
                1.0,
                MediaPreviewRequestPriority::Prefetch
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(test_media_job(
                other_prefetch.clone(),
                2.0,
                MediaPreviewRequestPriority::Prefetch,
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        let promoted_at = Instant::now();
        let mut playback_engine = mondrian_playback::PlaybackEngine::new(
            mondrian_core::Rational::new(1, 25),
            mondrian_playback::PlaybackPolicy::default(),
        )
        .expect("playback engine");
        playback_engine
            .play(10, mondrian_playback::MonotonicTimestamp::ZERO)
            .expect("playback demand");
        let demand_identity = playback_engine.frame_demand().expect("frame demand").identity();
        let status = sender.promote(
            &promoted,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            7,
            1.25,
            promoted_at,
            None,
            Some(demand_identity),
            PreviewDecodeAdaptiveHints::default(),
            PreviewHardwareDecodeRequest::PreferGpuResident,
        );
        assert_eq!(
            status,
            MediaPreviewJobPromoteStatus {
                updated: true,
                priority_promoted: true,
                access_mode_changed: true,
                generation_changed: true,
            }
        );
        let promoted_job = receiver.recv().expect("promoted current");
        assert_eq!(promoted_job.key, promoted);
        assert_eq!(promoted_job.priority, MediaPreviewRequestPriority::Current);
        assert_eq!(promoted_job.generation, 7);
        assert_eq!(promoted_job.source_secs, 1.25);
        assert_eq!(promoted_job.enqueued_at, promoted_at);
        assert_eq!(promoted_job.deadline_at, None);
        assert_eq!(promoted_job.demand_identity, Some(demand_identity));
        assert_eq!(
            promoted_job.access_mode,
            PreviewDecodeAccessMode::ScrubCursor
        );
        assert_eq!(
            promoted_job.hardware_decode_request,
            PreviewHardwareDecodeRequest::PreferGpuResident
        );
        assert_eq!(
            receiver.recv().expect("remaining prefetch").key,
            other_prefetch
        );
    }

    #[test]
    fn media_preview_job_queue_promote_refreshes_current_generation_without_priority_metric() {
        let (sender, receiver) = media_preview_job_queue(1);
        let key = test_media_key(1);

        assert_eq!(
            sender.enqueue(test_media_job_with_generation(
                key.clone(),
                1.0,
                2,
                MediaPreviewRequestPriority::Current,
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        let refreshed_at = Instant::now();
        let refreshed_deadline = Some(refreshed_at);
        let status = sender.promote(
            &key,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            5,
            1.0,
            refreshed_at,
            refreshed_deadline,
            None,
            PreviewDecodeAdaptiveHints::default(),
            PreviewHardwareDecodeRequest::Auto,
        );

        assert_eq!(
            status,
            MediaPreviewJobPromoteStatus {
                updated: true,
                priority_promoted: false,
                access_mode_changed: false,
                generation_changed: true,
            }
        );
        let job = receiver.recv().expect("refreshed current job");
        assert_eq!(job.key, key);
        assert_eq!(job.priority, MediaPreviewRequestPriority::Current);
        assert_eq!(job.access_mode, PreviewDecodeAccessMode::ScrubCursor);
        assert_eq!(job.generation, 5);
        assert_eq!(job.enqueued_at, refreshed_at);
        assert_eq!(job.deadline_at, refreshed_deadline);
    }

    #[test]
    fn media_preview_job_queue_prunes_obsolete_jobs_before_current_work() {
        let (sender, receiver) = media_preview_job_queue(3);
        let old_current = test_media_key(1);
        let old_prefetch = test_media_key(2);
        let fresh_prefetch = test_media_key(3);
        let current = test_media_key(4);

        assert_eq!(
            sender.enqueue(test_media_job_with_generation(
                old_current.clone(),
                1.0,
                1,
                MediaPreviewRequestPriority::Current,
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(test_media_job_with_generation(
                old_prefetch.clone(),
                2.0,
                1,
                MediaPreviewRequestPriority::Prefetch,
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(test_media_job_with_generation(
                fresh_prefetch.clone(),
                3.0,
                3,
                MediaPreviewRequestPriority::Prefetch,
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        assert_eq!(sender.prune_obsolete_jobs(3), 2);
        assert_eq!(
            sender.enqueue(test_media_job_with_generation(
                current.clone(),
                4.0,
                3,
                MediaPreviewRequestPriority::Current,
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        assert_eq!(receiver.recv().expect("current").key, current);
        assert_eq!(receiver.recv().expect("fresh prefetch").key, fresh_prefetch);
    }

    #[test]
    fn media_preview_job_queue_cancels_all_queued_jobs_for_key() {
        let (sender, receiver) = media_preview_job_queue(4);
        let canceled = test_media_key(1);
        let retained = test_media_key(2);

        assert_eq!(
            sender.enqueue(test_media_job(
                canceled.clone(),
                1.0,
                MediaPreviewRequestPriority::Prefetch
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(test_media_job(
                retained.clone(),
                2.0,
                MediaPreviewRequestPriority::Prefetch
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(test_media_job(
                canceled.clone(),
                3.0,
                MediaPreviewRequestPriority::Current
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        assert_eq!(sender.cancel_key(&canceled), 2);
        assert_eq!(receiver.recv().expect("retained job").key, retained);
        sender.close();
        assert!(receiver.recv().is_none());
    }

    #[test]
    fn media_preview_job_queue_clear_removes_all_queued_jobs() {
        let (sender, receiver) = media_preview_job_queue(4);
        let first = test_media_key(1);
        let second = test_media_key(2);

        assert_eq!(
            sender.enqueue(test_media_job(
                first,
                1.0,
                MediaPreviewRequestPriority::Current,
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(test_media_job(
                second,
                2.0,
                MediaPreviewRequestPriority::Prefetch,
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        assert_eq!(sender.clear(), 2);
        assert_eq!(sender.diagnostics().queued_jobs, 0);
        sender.close();
        assert!(receiver.recv().is_none());
    }

    #[test]
    fn media_preview_job_queue_diagnostics_break_down_current_depth_by_access_mode() {
        let (sender, _receiver) = media_preview_job_queue(4);
        let playback = test_media_key(1);
        let scrub = test_media_key(2);
        let still = test_media_key(3);

        assert_eq!(
            sender.enqueue(test_media_job(
                playback,
                1.0,
                MediaPreviewRequestPriority::Prefetch
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(test_media_job(
                scrub,
                2.0,
                MediaPreviewRequestPriority::Current
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(MediaPreviewJob {
                key: still,
                source_secs: 3.0,
                generation: 1,
                priority: MediaPreviewRequestPriority::Current,
                access_mode: PreviewDecodeAccessMode::RandomAccessStillFrame,
                adaptive_hints: PreviewDecodeAdaptiveHints::default(),
                hardware_decode_request: PreviewHardwareDecodeRequest::Auto,
                enqueued_at: Instant::now(),
                deadline_at: None,
                demand_identity: None,
            }),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        assert_eq!(
            sender.diagnostics(),
            MediaPreviewJobQueueDiagnostics {
                queued_jobs: 3,
                queued_current_jobs: 2,
                queued_prefetch_jobs: 1,
                queued_playback_cursor_jobs: 1,
                queued_expired_playback_current_jobs: 0,
                dropped_expired_playback_current_jobs: 0,
                queued_scrub_cursor_jobs: 1,
                queued_random_access_still_jobs: 1,
                queued_any_lane_eligible_jobs: 3,
                queued_playback_lane_eligible_jobs: 1,
                queued_scrub_lane_eligible_jobs: 1,
                queued_still_lane_eligible_jobs: 1,
                queued_interactive_lane_eligible_jobs: 2,
                closed: false,
            }
        );

        sender.close();
        assert_eq!(
            sender.diagnostics(),
            MediaPreviewJobQueueDiagnostics {
                closed: true,
                ..MediaPreviewJobQueueDiagnostics::default()
            }
        );
    }

    #[test]
    fn media_preview_job_queue_promote_returns_false_for_missing_or_prefetch() {
        let (sender, _receiver) = media_preview_job_queue(1);
        let key = test_media_key(1);

        assert_eq!(
            sender.promote(
                &key,
                MediaPreviewRequestPriority::Current,
                PreviewDecodeAccessMode::ScrubCursor,
                1,
                1.0,
                Instant::now(),
                None,
                None,
                PreviewDecodeAdaptiveHints::default(),
                PreviewHardwareDecodeRequest::Auto,
            ),
            MediaPreviewJobPromoteStatus::default()
        );
        assert_eq!(
            sender.promote(
                &key,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                1,
                1.0,
                Instant::now(),
                None,
                None,
                PreviewDecodeAdaptiveHints::default(),
                PreviewHardwareDecodeRequest::Auto,
            ),
            MediaPreviewJobPromoteStatus::default()
        );
    }

    #[test]
    fn media_preview_job_queue_clear_and_close_release_workers() {
        let (sender, receiver) = media_preview_job_queue(2);
        let first = test_media_key(1);
        let second = test_media_key(2);

        assert_eq!(
            sender.enqueue(test_media_job(
                first,
                1.0,
                MediaPreviewRequestPriority::Prefetch
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(test_media_job(
                second,
                2.0,
                MediaPreviewRequestPriority::Prefetch
            )),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        assert_eq!(sender.clear(), 2);
        sender.close();

        assert!(receiver.recv().is_none());
        assert_eq!(
            sender.enqueue(test_media_job(
                test_media_key(3),
                3.0,
                MediaPreviewRequestPriority::Current
            )),
            MediaPreviewJobEnqueueStatus::Closed
        );
    }

    #[test]
    fn media_preview_worker_count_reserves_cpu_capacity() {
        assert_eq!(media_preview_worker_count_for(0), 1);
        assert_eq!(media_preview_worker_count_for(1), 1);
        assert_eq!(media_preview_worker_count_for(5), 1);
        assert_eq!(media_preview_worker_count_for(6), 2);
        assert_eq!(media_preview_worker_count_for(7), 2);
        assert_eq!(media_preview_worker_count_for(8), 2);
        assert_eq!(media_preview_worker_count_for(11), 2);
        assert_eq!(media_preview_worker_count_for(12), 3);
        assert_eq!(media_preview_worker_count_for(32), 3);
    }

    #[test]
    fn media_preview_worker_lane_reserves_playback_only_when_parallel() {
        assert_eq!(media_preview_worker_lane(0, 1), MediaPreviewWorkerLane::Any);
        assert_eq!(
            media_preview_worker_lane(0, 2),
            MediaPreviewWorkerLane::Playback
        );
        assert_eq!(
            media_preview_worker_lane(1, 2),
            MediaPreviewWorkerLane::Interactive
        );
        assert_eq!(
            media_preview_worker_lane(0, 3),
            MediaPreviewWorkerLane::Playback
        );
        assert_eq!(
            media_preview_worker_lane(1, 3),
            MediaPreviewWorkerLane::Scrub
        );
        assert_eq!(
            media_preview_worker_lane(2, 3),
            MediaPreviewWorkerLane::Still
        );
    }

    #[test]
    fn media_preview_viewer_access_intent_tracks_playback_state() {
        assert_eq!(
            media_preview_viewer_access_intent(true, TimelineSeekSource::Settled),
            MediaPreviewAccessIntent::Playback
        );
        assert_eq!(
            media_preview_viewer_access_intent(false, TimelineSeekSource::PointerDrag),
            MediaPreviewAccessIntent::InteractiveScrub
        );
        assert_eq!(
            media_preview_viewer_access_intent(false, TimelineSeekSource::Settled),
            MediaPreviewAccessIntent::DeterministicStill
        );
    }

    #[test]
    fn media_preview_access_intent_lowers_to_explicit_access_modes() {
        assert_eq!(
            media_preview_access_mode_for_intent(MediaPreviewAccessIntent::Playback),
            PreviewDecodeAccessMode::PlaybackCursor
        );
        assert_eq!(
            media_preview_access_mode_for_intent(MediaPreviewAccessIntent::InteractiveScrub),
            PreviewDecodeAccessMode::ScrubCursor
        );
        assert_eq!(
            media_preview_access_mode_for_intent(MediaPreviewAccessIntent::DeterministicStill),
            PreviewDecodeAccessMode::RandomAccessStillFrame
        );
    }
}
