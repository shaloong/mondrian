//! Access-mode request admission for app viewer media preview.
//!
//! This module owns the app-layer scheduling contract for playback, scrub, and
//! random-access preview work. It deliberately does not decode media, evaluate
//! render plans, interpret color, or convert frames.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use mondrian_core::types::{AssetId, ColorEngine, ColorSpace};
use mondrian_media::{PreviewDecodeAccessMode, PreviewFileFingerprint};

pub(crate) const MEDIA_PREVIEW_JOB_QUEUE_CAPACITY: usize = 48;
const MEDIA_PREVIEW_MAX_PENDING_REQUESTS: usize = MEDIA_PREVIEW_JOB_QUEUE_CAPACITY;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Stable identity for one decoded media preview request.
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

#[derive(Clone)]
/// Latest-wins scheduler for access-mode-aware preview decode work.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Preview decode request priority used by scheduler admission and job queues.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Result of admitting a preview decode request into the scheduler.
pub(crate) enum MediaPreviewRequestStatus {
    Scheduled,
    AlreadyPending { access_mode_changed: bool },
    DroppedBackpressure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Freshness classification for a completed decode result.
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
        if state.pending.len() >= self.max_pending {
            if priority == MediaPreviewRequestPriority::Current {
                if let Some(evicted) = state
                    .pending
                    .iter()
                    .find(|(_, pending)| pending.priority == MediaPreviewRequestPriority::Prefetch)
                    .map(|(key, _)| key.clone())
                {
                    state.pending.remove(&evicted);
                    bump_value(&mut state.metrics.evicted_prefetch_requests);
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
        MediaPreviewRequestStatus::Scheduled
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

    pub(crate) fn cancel_all(&self) {
        let mut state = self.state.lock().expect("media preview scheduler poisoned");
        let canceled = state.pending.len() as u64;
        state.pending.clear();
        state.latest_generation = state.latest_generation.saturating_add(1);
        state.metrics.canceled_requests = state.metrics.canceled_requests.saturating_add(canceled);
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
