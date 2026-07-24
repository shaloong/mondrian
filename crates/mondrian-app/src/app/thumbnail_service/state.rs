//! Internal thumbnail lifecycle state and bounded retention policies.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use mondrian_core::{
    AssetId, ExecutionCancellationToken, ExecutionDeadlineStatus, ExecutionPriority,
    ExecutionTerminalDisposition, ExecutionTerminalEvidence,
};
use mondrian_media::MediaFileFingerprint;
use mondrian_timeline::sequence::ProgramColorContext;

use super::analysis::ThumbnailColorContract;
use super::{
    ThumbnailFailure, ThumbnailFailureReason, ThumbnailRasterFrame, ThumbnailTerminalRecord,
    THUMBNAIL_FAILURE_CAPACITY, THUMBNAIL_TERMINAL_CAPACITY,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ThumbnailRequestKey {
    pub(super) asset_id: AssetId,
    pub(super) path: PathBuf,
    pub(super) fingerprint: MediaFileFingerprint,
    pub(super) color: ThumbnailColorContract,
}

#[derive(Debug)]
pub(super) struct ThumbnailJob {
    pub(super) key: ThumbnailRequestKey,
    pub(super) generation: u64,
    pub(super) cancellation: ExecutionCancellationToken,
}

#[derive(Debug)]
pub(super) struct ThumbnailResult {
    pub(super) key: ThumbnailRequestKey,
    pub(super) generation: u64,
    pub(super) result: Result<ThumbnailRasterFrame, ThumbnailFailure>,
    pub(super) elapsed: Duration,
}

#[derive(Debug, Clone)]
pub(super) struct PendingThumbnail {
    pub(super) generation: u64,
    pub(super) cancellation: ExecutionCancellationToken,
}

#[derive(Debug, Clone)]
pub(super) struct ThumbnailCacheEntry {
    pub(super) key: ThumbnailRequestKey,
    pub(super) frame: ThumbnailRasterFrame,
}

#[derive(Debug, Clone)]
pub(super) struct ThumbnailFailureEntry {
    pub(super) key: ThumbnailFailureKey,
    pub(super) failure: ThumbnailFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThumbnailFailureKey {
    pub(super) asset_id: AssetId,
    pub(super) path: PathBuf,
    pub(super) fingerprint: MediaFileFingerprint,
    pub(super) color: Option<ThumbnailColorContract>,
}

#[derive(Default)]
pub(super) struct ThumbnailCounters {
    pub(super) rejections: u64,
    pub(super) completions: u64,
    pub(super) failures: u64,
    pub(super) cancellations: u64,
    pub(super) superseded: u64,
    pub(super) evictions: u64,
    pub(super) failures_by_reason: HashMap<ThumbnailFailureReason, u64>,
}

pub(super) struct ThumbnailState {
    pub(super) generation: u64,
    pub(super) color_context: Option<ProgramColorContext>,
    pub(super) cache: HashMap<AssetId, ThumbnailCacheEntry>,
    pub(super) cache_lru: VecDeque<AssetId>,
    pub(super) cached_bytes: usize,
    pub(super) failures: HashMap<AssetId, ThumbnailFailureEntry>,
    pub(super) failure_lru: VecDeque<AssetId>,
    pub(super) pending: HashMap<ThumbnailRequestKey, PendingThumbnail>,
    pub(super) deferred: VecDeque<ThumbnailJob>,
    pub(super) active: HashMap<AssetId, ThumbnailRequestKey>,
    pub(super) terminal_records: VecDeque<ThumbnailTerminalRecord>,
    pub(super) counters: ThumbnailCounters,
}

impl Default for ThumbnailState {
    fn default() -> Self {
        Self {
            generation: 1,
            color_context: None,
            cache: HashMap::new(),
            cache_lru: VecDeque::new(),
            cached_bytes: 0,
            failures: HashMap::new(),
            failure_lru: VecDeque::new(),
            pending: HashMap::new(),
            deferred: VecDeque::new(),
            active: HashMap::new(),
            terminal_records: VecDeque::new(),
            counters: ThumbnailCounters::default(),
        }
    }
}

pub(super) fn touch_asset(lru: &mut VecDeque<AssetId>, asset_id: AssetId) {
    lru.retain(|candidate| *candidate != asset_id);
    lru.push_front(asset_id);
}

pub(super) fn retain_failure(
    state: &mut ThumbnailState,
    key: ThumbnailRequestKey,
    failure: ThumbnailFailure,
) {
    retain_failure_entry(
        state,
        ThumbnailFailureKey {
            asset_id: key.asset_id,
            path: key.path,
            fingerprint: key.fingerprint,
            color: Some(key.color),
        },
        failure,
    );
}

pub(super) fn retain_failure_entry(
    state: &mut ThumbnailState,
    key: ThumbnailFailureKey,
    failure: ThumbnailFailure,
) {
    tracing::debug!(
        asset_id = %key.asset_id,
        path = %key.path.display(),
        reason = failure.reason.code(),
        detail = %failure.detail,
        "asset thumbnail failed"
    );
    record_failure_counter(state, failure.reason);
    state.failures.insert(
        key.asset_id,
        ThumbnailFailureEntry { key: key.clone(), failure },
    );
    touch_asset(&mut state.failure_lru, key.asset_id);
    while state.failure_lru.len() > THUMBNAIL_FAILURE_CAPACITY {
        if let Some(asset_id) = state.failure_lru.pop_back() {
            state.failures.remove(&asset_id);
        }
    }
}

fn record_failure_counter(state: &mut ThumbnailState, reason: ThumbnailFailureReason) {
    state.counters.failures = state.counters.failures.saturating_add(1);
    let count = state.counters.failures_by_reason.entry(reason).or_default();
    *count = count.saturating_add(1);
}

pub(super) fn push_terminal(
    state: &mut ThumbnailState,
    key: &ThumbnailRequestKey,
    generation: u64,
    disposition: ExecutionTerminalDisposition,
    elapsed: Duration,
    failure: Option<ThumbnailFailureReason>,
) {
    push_terminal_identity(
        state,
        key.asset_id,
        key.fingerprint,
        generation,
        disposition,
        elapsed,
        failure,
    );
}

pub(super) fn push_terminal_identity(
    state: &mut ThumbnailState,
    asset_id: AssetId,
    fingerprint: MediaFileFingerprint,
    generation: u64,
    disposition: ExecutionTerminalDisposition,
    elapsed: Duration,
    failure: Option<ThumbnailFailureReason>,
) {
    state.terminal_records.push_back(ThumbnailTerminalRecord {
        evidence: ExecutionTerminalEvidence {
            generation,
            priority: ExecutionPriority::Background,
            disposition,
            deadline: ExecutionDeadlineStatus::NotApplicable,
        },
        asset_id,
        fingerprint,
        elapsed,
        failure,
    });
    while state.terminal_records.len() > THUMBNAIL_TERMINAL_CAPACITY {
        state.terminal_records.pop_front();
    }
}

pub(super) fn retire_deferred(state: &mut ThumbnailState) {
    let retired: Vec<_> = state.deferred.drain(..).map(|job| (job.key, job.generation)).collect();
    for (key, generation) in retired {
        state.counters.cancellations = state.counters.cancellations.saturating_add(1);
        push_terminal(
            state,
            &key,
            generation,
            ExecutionTerminalDisposition::Canceled,
            Duration::ZERO,
            Some(ThumbnailFailureReason::DecodeCanceled),
        );
    }
}
