//! Internal lifecycle state and bounded retention policies.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use mondrian_assets::AssetLibrary;
use mondrian_core::{
    AssetId, ExecutionCancellationToken, ExecutionDeadlineStatus, ExecutionPriority,
    ExecutionTerminalDisposition, ExecutionTerminalEvidence,
};

use super::{
    WaveformFailureReason, WaveformTerminalRecord, WAVEFORM_FAILURE_CAPACITY, WAVEFORM_SAMPLE_RATE,
    WAVEFORM_TERMINAL_EVIDENCE_CAPACITY,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct WaveformSourceKey {
    pub(super) asset_id: AssetId,
    pub(super) source_revision: u64,
}

#[derive(Debug, Clone)]
pub(super) struct WaveformSource {
    pub(super) envelope: Arc<[f32]>,
    pub(super) total_frames: u64,
    pub(super) sample_rate: u32,
}

#[derive(Debug, Clone)]
pub(super) struct WaveformFailure {
    pub(super) reason: WaveformFailureReason,
    pub(super) detail: String,
}

impl WaveformFailure {
    pub(super) fn new(reason: WaveformFailureReason, detail: impl Into<String>) -> Self {
        Self { reason, detail: detail.into() }
    }
}

#[derive(Debug)]
pub(super) struct WaveformJob {
    pub(super) key: WaveformSourceKey,
    pub(super) generation: u64,
    pub(super) path: PathBuf,
    pub(super) total_frames: u64,
    pub(super) cancellation: ExecutionCancellationToken,
}

#[derive(Debug)]
pub(super) struct WaveformResult {
    pub(super) key: WaveformSourceKey,
    pub(super) generation: u64,
    pub(super) source: Result<WaveformSource, WaveformFailure>,
    pub(super) elapsed: Duration,
}

#[derive(Debug, Clone)]
pub(super) struct PendingWaveform {
    pub(super) generation: u64,
    pub(super) cancellation: ExecutionCancellationToken,
}

#[derive(Default)]
pub(super) struct WaveformCounters {
    pub(super) queue_rejections: u64,
    pub(super) completions: u64,
    pub(super) failures: u64,
    pub(super) cancellations: u64,
    pub(super) superseded_completions: u64,
    pub(super) cache_evictions: u64,
}

pub(super) struct WaveformState {
    pub(super) generation: u64,
    pub(super) library: Option<Arc<AssetLibrary>>,
    pub(super) sources: HashMap<WaveformSourceKey, WaveformSource>,
    pub(super) source_lru: VecDeque<WaveformSourceKey>,
    pub(super) active_keys: HashMap<AssetId, WaveformSourceKey>,
    pub(super) pending: HashMap<WaveformSourceKey, PendingWaveform>,
    pub(super) deferred: VecDeque<WaveformJob>,
    pub(super) failures: HashMap<WaveformSourceKey, WaveformFailure>,
    pub(super) failure_lru: VecDeque<WaveformSourceKey>,
    pub(super) terminal_records: VecDeque<WaveformTerminalRecord>,
    pub(super) counters: WaveformCounters,
}

impl Default for WaveformState {
    fn default() -> Self {
        Self {
            generation: 1,
            library: None,
            sources: HashMap::new(),
            source_lru: VecDeque::new(),
            active_keys: HashMap::new(),
            pending: HashMap::new(),
            deferred: VecDeque::new(),
            failures: HashMap::new(),
            failure_lru: VecDeque::new(),
            terminal_records: VecDeque::new(),
            counters: WaveformCounters::default(),
        }
    }
}

pub(super) fn duration_to_waveform_frames(duration: Duration) -> Option<u64> {
    let numerator = duration.as_nanos().checked_mul(u128::from(WAVEFORM_SAMPLE_RATE))?;
    let frames = numerator.saturating_add(999_999_999) / 1_000_000_000;
    u64::try_from(frames).ok().filter(|frames| *frames > 0)
}

pub(super) fn rotate_waveform_generation(
    state: &mut WaveformState,
    library: Option<Arc<AssetLibrary>>,
) {
    for pending in state.pending.values() {
        pending.cancellation.cancel();
    }
    let canceled_deferred: Vec<_> =
        state.deferred.iter().map(|job| (job.key.clone(), job.generation)).collect();
    for (key, generation) in canceled_deferred {
        state.counters.cancellations = state.counters.cancellations.saturating_add(1);
        push_terminal(
            state,
            &key,
            generation,
            ExecutionTerminalDisposition::Canceled,
            Duration::ZERO,
            Some(WaveformFailureReason::Canceled),
        );
    }
    state.generation = state.generation.saturating_add(1).max(1);
    state.library = library;
    state.sources.clear();
    state.source_lru.clear();
    state.active_keys.clear();
    state.pending.clear();
    state.deferred.clear();
    state.failures.clear();
    state.failure_lru.clear();
}

pub(super) fn touch_key(lru: &mut VecDeque<WaveformSourceKey>, key: &WaveformSourceKey) {
    lru.retain(|candidate| candidate != key);
    lru.push_front(key.clone());
}

pub(super) fn retain_failure_locked(
    state: &mut WaveformState,
    key: WaveformSourceKey,
    failure: WaveformFailure,
) {
    tracing::warn!(
        asset_id = %key.asset_id,
        source_revision = key.source_revision,
        reason = ?failure.reason,
        detail = %failure.detail,
        "waveform analysis failed"
    );
    state.failures.insert(key.clone(), failure);
    state.failure_lru.retain(|candidate| candidate != &key);
    state.failure_lru.push_front(key);
    while state.failure_lru.len() > WAVEFORM_FAILURE_CAPACITY {
        if let Some(evicted) = state.failure_lru.pop_back() {
            state.failures.remove(&evicted);
        }
    }
}

pub(super) fn push_terminal(
    state: &mut WaveformState,
    key: &WaveformSourceKey,
    generation: u64,
    disposition: ExecutionTerminalDisposition,
    elapsed: Duration,
    failure: Option<WaveformFailureReason>,
) {
    state.terminal_records.push_back(WaveformTerminalRecord {
        evidence: ExecutionTerminalEvidence {
            generation,
            priority: ExecutionPriority::Background,
            disposition,
            deadline: ExecutionDeadlineStatus::NotApplicable,
        },
        asset_id: key.asset_id,
        source_revision: key.source_revision,
        elapsed,
        failure,
    });
    while state.terminal_records.len() > WAVEFORM_TERMINAL_EVIDENCE_CAPACITY {
        state.terminal_records.pop_front();
    }
}
