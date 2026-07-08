//! Access-mode request admission for app viewer media preview.
//!
//! This module owns the app-layer scheduling contract for playback, scrub, and
//! random-access preview work. It deliberately does not decode media, evaluate
//! render plans, interpret color, or convert frames.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crate::app::ui_actions::TimelineSeekSource;
use mondrian_core::types::{AssetId, ColorEngine, ColorSpace};
use mondrian_media::{preview_decode_cpu_budget, PreviewDecodeAccessMode, PreviewFileFingerprint};

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
    pub(crate) working_color_space: ColorSpace,
    pub(crate) tone_map: bool,
    pub(crate) engine: ColorEngine,
}

/// Latest-wins scheduler for access-mode-aware preview decode work.
#[derive(Clone)]
pub(crate) struct MediaPreviewScheduler {
    state: Arc<Mutex<MediaPreviewSchedulerState>>,
    max_pending: usize,
}

#[derive(Default)]
struct MediaPreviewSchedulerState {
    latest_generation: u64,
    pending: HashMap<MediaPreviewKey, MediaPreviewPendingRequest>,
    metrics: MediaPreviewSchedulerMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaPreviewPendingRequest {
    generation: u64,
    priority: MediaPreviewRequestPriority,
    access_mode: PreviewDecodeAccessMode,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MediaPreviewSchedulerMetrics {
    scheduled_requests: u64,
    already_pending_requests: u64,
    already_pending_access_mode_changes: u64,
    dropped_backpressure_requests: u64,
    dropped_invalid_access_mode_requests: u64,
    dropped_obsolete_generation_requests: u64,
    dropped_pending_window_requests: u64,
    skipped_decode_jobs: u64,
    skipped_decode_missing_pending: u64,
    skipped_decode_access_mode_mismatch: u64,
    skipped_decode_obsolete_generation: u64,
    completed_current_results: u64,
    completed_cache_only_results: u64,
    completed_cache_only_missing_pending: u64,
    completed_cache_only_access_mode_mismatch: u64,
    completed_stale_results: u64,
    completed_stale_missing_pending: u64,
    completed_stale_access_mode_mismatch: u64,
    completed_stale_obsolete_generation: u64,
    canceled_requests: u64,
    pruned_obsolete_requests: u64,
    evicted_prefetch_requests: u64,
    evicted_still_requests: u64,
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
    pub(crate) enqueued_at: Instant,
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

pub(crate) fn media_preview_job_queue(
    capacity: usize,
) -> (MediaPreviewJobQueueSender, MediaPreviewJobQueueReceiver) {
    let shared = Arc::new(MediaPreviewJobQueueShared {
        state: Mutex::new(MediaPreviewJobQueueState { queue: VecDeque::new(), closed: false }),
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
        before.saturating_sub(state.queue.len())
    }

    pub(crate) fn cancel_key(&self, key: &MediaPreviewKey) -> usize {
        let mut state = lock_media_preview_job_queue_state(&self.shared.state);
        let before = state.queue.len();
        state.queue.retain(|queued| &queued.job.key != key);
        before.saturating_sub(state.queue.len())
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
        queued.job.enqueued_at = enqueued_at;
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

    pub(crate) fn recv_for_worker(&self, lane: MediaPreviewWorkerLane) -> Option<MediaPreviewJob> {
        let mut state = lock_media_preview_job_queue_state(&self.shared.state);
        loop {
            if let Some(index) = next_media_preview_job_index(&state.queue, lane) {
                return state.queue.remove(index).map(|queued| queued.job);
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

fn next_media_preview_job_index(
    queue: &VecDeque<QueuedMediaPreviewJob>,
    lane: MediaPreviewWorkerLane,
) -> Option<usize> {
    let eligible = |queued: &QueuedMediaPreviewJob| lane.accepts(queued.job.access_mode);
    queue
        .iter()
        .enumerate()
        .filter(|(_, queued)| {
            queued.priority == MediaPreviewRequestPriority::Current && eligible(queued)
        })
        .min_by_key(|(_, queued)| media_preview_current_job_rank(queued.job.access_mode))
        .map(|(index, _)| index)
        .or_else(|| queue.iter().position(eligible))
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
    fn accepts(self, access_mode: PreviewDecodeAccessMode) -> bool {
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
            state: Arc::new(Mutex::new(MediaPreviewSchedulerState::default())),
            max_pending: max_pending.max(1),
        }
    }

    pub(crate) fn begin_generation(&self) -> u64 {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        state.latest_generation = state.latest_generation.saturating_add(1);
        state.latest_generation
    }

    pub(crate) fn request(
        &self,
        key: MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
    ) -> MediaPreviewRequestStatus {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        if !priority.accepts_access_mode(access_mode) {
            bump_value(&mut state.metrics.dropped_invalid_access_mode_requests);
            return MediaPreviewRequestStatus::DroppedInvalidAccessMode;
        }
        if generation < state.latest_generation {
            bump_value(&mut state.metrics.dropped_backpressure_requests);
            bump_value(&mut state.metrics.dropped_obsolete_generation_requests);
            return MediaPreviewRequestStatus::DroppedBackpressure;
        }
        if let Some(pending) = state.pending.get_mut(&key) {
            let previous_access_mode = pending.access_mode;
            pending.access_mode =
                promoted_access_mode(pending.priority, pending.access_mode, priority, access_mode);
            pending.generation = generation;
            pending.priority = pending.priority.promote_with(priority);
            let access_mode_changed = previous_access_mode != pending.access_mode;
            Self::prune_obsolete_locked(&mut state);
            bump_value(&mut state.metrics.already_pending_requests);
            if access_mode_changed {
                bump_value(&mut state.metrics.already_pending_access_mode_changes);
            }
            return MediaPreviewRequestStatus::AlreadyPending { access_mode_changed };
        }
        Self::prune_obsolete_locked(&mut state);
        let mut evicted_prefetch = None;
        let mut evicted_still = None;
        if state.pending.len() >= self.max_pending {
            if priority == MediaPreviewRequestPriority::Current {
                if let Some(evicted) = state
                    .pending
                    .iter()
                    .find(|(_, pending)| pending.priority == MediaPreviewRequestPriority::Prefetch)
                    .map(|(key, _)| key.clone())
                {
                    state.pending.remove(&evicted);
                    evicted_prefetch = Some(Box::new(evicted));
                    bump_value(&mut state.metrics.evicted_prefetch_requests);
                } else if access_mode != PreviewDecodeAccessMode::RandomAccessStillFrame {
                    if let Some(evicted) = state
                        .pending
                        .iter()
                        .find(|(_, pending)| {
                            pending.priority == MediaPreviewRequestPriority::Current
                                && pending.access_mode
                                    == PreviewDecodeAccessMode::RandomAccessStillFrame
                        })
                        .map(|(key, _)| key.clone())
                    {
                        state.pending.remove(&evicted);
                        evicted_still = Some(Box::new(evicted));
                        bump_value(&mut state.metrics.evicted_still_requests);
                    } else {
                        bump_value(&mut state.metrics.dropped_backpressure_requests);
                        bump_value(&mut state.metrics.dropped_pending_window_requests);
                        return MediaPreviewRequestStatus::DroppedBackpressure;
                    }
                } else {
                    bump_value(&mut state.metrics.dropped_backpressure_requests);
                    bump_value(&mut state.metrics.dropped_pending_window_requests);
                    return MediaPreviewRequestStatus::DroppedBackpressure;
                }
            } else {
                bump_value(&mut state.metrics.dropped_backpressure_requests);
                bump_value(&mut state.metrics.dropped_pending_window_requests);
                return MediaPreviewRequestStatus::DroppedBackpressure;
            }
        }
        state.pending.insert(
            key,
            MediaPreviewPendingRequest { generation, priority, access_mode },
        );
        bump_value(&mut state.metrics.scheduled_requests);
        MediaPreviewRequestStatus::Scheduled { evicted_prefetch, evicted_still }
    }

    pub(crate) fn should_decode(
        &self,
        key: &MediaPreviewKey,
        access_mode: PreviewDecodeAccessMode,
    ) -> bool {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        let Some(pending) = state.pending.get(key).copied() else {
            bump_value(&mut state.metrics.skipped_decode_jobs);
            bump_value(&mut state.metrics.skipped_decode_missing_pending);
            return false;
        };
        if pending.access_mode != access_mode {
            bump_value(&mut state.metrics.skipped_decode_jobs);
            bump_value(&mut state.metrics.skipped_decode_access_mode_mismatch);
            return false;
        }
        if pending.generation >= state.latest_generation {
            return true;
        }
        state.pending.remove(key);
        bump_value(&mut state.metrics.skipped_decode_jobs);
        bump_value(&mut state.metrics.skipped_decode_obsolete_generation);
        false
    }

    pub(crate) fn is_decode_current(
        &self,
        key: &MediaPreviewKey,
        generation: u64,
        access_mode: PreviewDecodeAccessMode,
    ) -> bool {
        let state = self.state.lock().expect("media preview scheduler poisoned");
        state
            .pending
            .get(key)
            .map(|pending| {
                pending.access_mode == access_mode
                    && pending.generation >= generation
                    && pending.generation >= state.latest_generation
            })
            .unwrap_or(false)
    }

    pub(crate) fn complete(
        &self,
        key: &MediaPreviewKey,
        result_generation: u64,
        access_mode: PreviewDecodeAccessMode,
    ) -> MediaPreviewCompletionStatus {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        let result_is_latest = result_generation >= state.latest_generation;
        let Some(pending) = state.pending.get(key).copied() else {
            if result_is_latest {
                bump_value(&mut state.metrics.completed_cache_only_results);
                bump_value(&mut state.metrics.completed_cache_only_missing_pending);
                return MediaPreviewCompletionStatus::CacheOnly;
            }
            bump_value(&mut state.metrics.completed_stale_results);
            bump_value(&mut state.metrics.completed_stale_missing_pending);
            return MediaPreviewCompletionStatus::Stale;
        };
        if pending.access_mode != access_mode {
            if result_is_latest {
                bump_value(&mut state.metrics.completed_cache_only_results);
                bump_value(&mut state.metrics.completed_cache_only_access_mode_mismatch);
                return MediaPreviewCompletionStatus::CacheOnly;
            }
            bump_value(&mut state.metrics.completed_stale_results);
            bump_value(&mut state.metrics.completed_stale_access_mode_mismatch);
            return MediaPreviewCompletionStatus::Stale;
        }
        state.pending.remove(key);
        let pending_generation = pending.generation;
        let is_current = pending_generation >= state.latest_generation || result_is_latest;
        if is_current {
            bump_value(&mut state.metrics.completed_current_results);
            MediaPreviewCompletionStatus::Current
        } else {
            bump_value(&mut state.metrics.completed_stale_results);
            bump_value(&mut state.metrics.completed_stale_obsolete_generation);
            MediaPreviewCompletionStatus::Stale
        }
    }

    pub(crate) fn cancel(&self, key: &MediaPreviewKey) {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        if state.pending.remove(key).is_some() {
            bump_value(&mut state.metrics.canceled_requests);
        }
    }

    pub(crate) fn cancel_all(&self) -> u64 {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        let canceled = state.pending.len() as u64;
        state.pending.clear();
        state.latest_generation = state.latest_generation.saturating_add(1);
        state.metrics.canceled_requests = state.metrics.canceled_requests.saturating_add(canceled);
        state.latest_generation
    }

    pub(crate) fn prune_obsolete(&self) {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        Self::prune_obsolete_locked(&mut state);
    }

    fn prune_obsolete_locked(state: &mut MediaPreviewSchedulerState) {
        let latest_generation = state.latest_generation;
        let before = state.pending.len();
        state.pending.retain(|_, pending| pending.generation >= latest_generation);
        let pruned = before.saturating_sub(state.pending.len()) as u64;
        state.metrics.pruned_obsolete_requests =
            state.metrics.pruned_obsolete_requests.saturating_add(pruned);
    }

    pub(crate) fn diagnostics(&self) -> MediaPreviewSchedulerDiagnostics {
        let state = self.state.lock().expect("media preview scheduler poisoned");
        MediaPreviewSchedulerDiagnostics {
            latest_generation: state.latest_generation,
            pending_requests: state.pending.len(),
            scheduled_requests: state.metrics.scheduled_requests,
            already_pending_requests: state.metrics.already_pending_requests,
            already_pending_access_mode_changes: state.metrics.already_pending_access_mode_changes,
            dropped_backpressure_requests: state.metrics.dropped_backpressure_requests,
            dropped_invalid_access_mode_requests: state
                .metrics
                .dropped_invalid_access_mode_requests,
            dropped_obsolete_generation_requests: state
                .metrics
                .dropped_obsolete_generation_requests,
            dropped_pending_window_requests: state.metrics.dropped_pending_window_requests,
            skipped_decode_jobs: state.metrics.skipped_decode_jobs,
            skipped_decode_missing_pending: state.metrics.skipped_decode_missing_pending,
            skipped_decode_access_mode_mismatch: state.metrics.skipped_decode_access_mode_mismatch,
            skipped_decode_obsolete_generation: state.metrics.skipped_decode_obsolete_generation,
            completed_current_results: state.metrics.completed_current_results,
            completed_cache_only_results: state.metrics.completed_cache_only_results,
            completed_cache_only_missing_pending: state
                .metrics
                .completed_cache_only_missing_pending,
            completed_cache_only_access_mode_mismatch: state
                .metrics
                .completed_cache_only_access_mode_mismatch,
            completed_stale_results: state.metrics.completed_stale_results,
            completed_stale_missing_pending: state.metrics.completed_stale_missing_pending,
            completed_stale_access_mode_mismatch: state
                .metrics
                .completed_stale_access_mode_mismatch,
            completed_stale_obsolete_generation: state.metrics.completed_stale_obsolete_generation,
            canceled_requests: state.metrics.canceled_requests,
            pruned_obsolete_requests: state.metrics.pruned_obsolete_requests,
            evicted_prefetch_requests: state.metrics.evicted_prefetch_requests,
            evicted_still_requests: state.metrics.evicted_still_requests,
        }
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.state.lock().expect("media preview scheduler poisoned").pending.len()
    }
}

fn bump_value(counter: &mut u64) {
    *counter = counter.saturating_add(1);
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
            working_color_space: ColorSpace::Rec709,
            tone_map: false,
            engine: ColorEngine::MondrianSmart,
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
            enqueued_at: Instant::now(),
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
    fn media_preview_job_queue_keeps_interactive_work_off_playback_lane() {
        let (sender, receiver) = media_preview_job_queue(2);
        let scrub = test_media_key(1);
        let playback = test_media_key(2);
        let scrub_job = test_media_job(scrub, 1.0, MediaPreviewRequestPriority::Current);
        let mut playback_job =
            test_media_job(playback.clone(), 2.0, MediaPreviewRequestPriority::Prefetch);
        playback_job.access_mode = PreviewDecodeAccessMode::PlaybackCursor;

        assert_eq!(
            sender.enqueue(scrub_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );
        assert_eq!(
            sender.enqueue(playback_job),
            MediaPreviewJobEnqueueStatus::Enqueued { evicted_prefetch: None, evicted_still: None }
        );

        let playback_job = receiver
            .recv_for_worker(MediaPreviewWorkerLane::Playback)
            .expect("playback lane should skip scrub work");
        assert_eq!(playback_job.key, playback);
        assert_eq!(
            playback_job.access_mode,
            PreviewDecodeAccessMode::PlaybackCursor
        );
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
        let status = sender.promote(
            &promoted,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            7,
            1.25,
            promoted_at,
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
        assert_eq!(
            promoted_job.access_mode,
            PreviewDecodeAccessMode::ScrubCursor
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
        let status = sender.promote(
            &key,
            MediaPreviewRequestPriority::Current,
            PreviewDecodeAccessMode::ScrubCursor,
            5,
            1.0,
            refreshed_at,
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
