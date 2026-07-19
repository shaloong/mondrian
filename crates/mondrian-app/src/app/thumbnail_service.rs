//! UI-independent bounded asset-thumbnail execution service.
//!
//! This Module owns source/color identity, admission, generation cancellation,
//! cache residency, failure memory, and terminal evidence. A Window Adapter may
//! project a resident raster into a Widget payload but owns no media work.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::{
    AssetId, ExecutionCancellationToken, ExecutionTerminalDisposition, ExecutionTerminalEvidence,
};
use mondrian_media::PreviewFileFingerprint;
use mondrian_timeline::sequence::ColorContext;
use parking_lot::Mutex;

use analysis::{thumbnail_worker, ThumbnailColorContract};
use state::{
    push_terminal, push_terminal_identity, retain_failure, retain_failure_entry, retire_deferred,
    touch_asset, PendingThumbnail, ThumbnailCacheEntry, ThumbnailFailureKey, ThumbnailJob,
    ThumbnailRequestKey, ThumbnailResult, ThumbnailState,
};

mod analysis;
mod state;
#[cfg(test)]
mod tests;

const THUMBNAIL_CACHE_ENTRY_CAPACITY: usize = 512;
const THUMBNAIL_CACHE_BYTE_BUDGET: usize = 128 * 1024 * 1024;
const THUMBNAIL_FAILURE_CAPACITY: usize = 256;
const THUMBNAIL_TERMINAL_CAPACITY: usize = 512;
const THUMBNAIL_PENDING_CAPACITY: usize = 512;
const THUMBNAIL_JOB_QUEUE_CAPACITY: usize = 16;
const THUMBNAIL_MAX_DEFERRED_DISPATCH_PER_POLL: usize = 8;
const THUMBNAIL_MAX_COMPLETED_RESULTS_PER_POLL: usize = 8;
const THUMBNAIL_COMPLETED_RESULTS_POLL_BUDGET: Duration = Duration::from_micros(2_000);

/// Stable machine-readable reason for a thumbnail failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThumbnailFailureReason {
    /// Source path is missing or inaccessible.
    MissingSourceFile,
    /// Asset ingest has no primary video stream contract.
    MissingVideoStreamContract,
    /// Non-color YUV data cannot be interpreted as a presentation thumbnail.
    NonColorDataUnsupported,
    /// Missing-metadata policy rejected the input identity.
    InputColorRejected,
    /// An internal working identity reached a presentation-only boundary.
    InternalOutputIdentity,
    /// The raster contract cannot represent the requested encoded output.
    UnsupportedRasterOutput,
    /// Background thumbnail worker is unavailable.
    WorkerUnavailable,
    /// Bounded service admission was exhausted.
    AdmissionRejected,
    /// Media decode failed.
    DecodeFailed,
    /// Deterministic still decode was canceled.
    DecodeCanceled,
    /// Still decode unexpectedly returned a GPU-resident frame.
    UnexpectedGpuFrame,
    /// Source-to-working transform failed.
    InputTransformFailed,
    /// Working-to-display transform failed.
    OutputTransformFailed,
    /// Final raster payload shape was invalid.
    InvalidRasterPayload,
}

impl ThumbnailFailureReason {
    /// Stable diagnostic code for logs, tests, and telemetry.
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingSourceFile => "missing_source_file",
            Self::MissingVideoStreamContract => "missing_video_stream_contract",
            Self::NonColorDataUnsupported => "non_color_data_unsupported",
            Self::InputColorRejected => "input_color_rejected",
            Self::InternalOutputIdentity => "internal_output_identity",
            Self::UnsupportedRasterOutput => "unsupported_raster_output",
            Self::WorkerUnavailable => "worker_unavailable",
            Self::AdmissionRejected => "admission_rejected",
            Self::DecodeFailed => "decode_failed",
            Self::DecodeCanceled => "decode_canceled",
            Self::UnexpectedGpuFrame => "unexpected_gpu_frame",
            Self::InputTransformFailed => "input_transform_failed",
            Self::OutputTransformFailed => "output_transform_failed",
            Self::InvalidRasterPayload => "invalid_raster_payload",
        }
    }
}

/// Structured thumbnail failure retained by the service and panel model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailFailure {
    /// Stable failure category.
    pub reason: ThumbnailFailureReason,
    /// Diagnostic detail for logs and support tooling.
    pub detail: String,
}

impl ThumbnailFailure {
    /// Build a structured thumbnail failure.
    pub fn new(reason: ThumbnailFailureReason, detail: impl Into<String>) -> Self {
        Self { reason, detail: detail.into() }
    }
}

/// Encoded color identity of a final thumbnail raster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThumbnailRasterColorSpace {
    /// IEC 61966-2-1 sRGB encoded RGB values.
    Srgb,
}

/// Validated UI-independent thumbnail raster.
#[derive(Debug, Clone)]
pub struct ThumbnailRasterFrame {
    /// Stable renderer-resource identity.
    pub resource_key: String,
    /// Raster width in pixels.
    pub width: u32,
    /// Raster height in pixels.
    pub height: u32,
    /// Encoded color identity.
    pub color_space: ThumbnailRasterColorSpace,
    /// Row-major RGBA8 payload.
    pub rgba: Arc<[u8]>,
}

impl ThumbnailRasterFrame {
    pub(super) fn new(
        resource_key: impl Into<String>,
        width: u32,
        height: u32,
        color_space: ThumbnailRasterColorSpace,
        rgba: impl Into<Arc<[u8]>>,
    ) -> Option<Self> {
        let rgba = rgba.into();
        let expected = width.checked_mul(height)?.checked_mul(4)? as usize;
        (width > 0 && height > 0 && rgba.len() == expected).then(|| Self {
            resource_key: resource_key.into(),
            width,
            height,
            color_space,
            rgba,
        })
    }

    fn reserved_bytes(&self) -> usize {
        self.rgba.len()
    }
}

/// Current service lifecycle state for one asset thumbnail.
#[derive(Debug, Clone)]
pub enum ThumbnailLookupState {
    /// No thumbnail is expected for this asset or color context.
    Unavailable,
    /// Work is admitted, queued, or executing.
    Loading,
    /// The exact request failed.
    Failed(ThumbnailFailure),
    /// The exact request has a resident validated raster.
    Ready(ThumbnailRasterFrame),
}

/// One bounded terminal record for Headless verification.
#[derive(Debug, Clone)]
pub struct ThumbnailTerminalRecord {
    /// Shared terminal execution classification.
    pub evidence: ExecutionTerminalEvidence,
    /// Asset whose request terminated.
    pub asset_id: AssetId,
    /// Source fingerprint used by the request.
    pub fingerprint: PreviewFileFingerprint,
    /// Wall duration after worker admission.
    pub elapsed: Duration,
    /// Domain failure category, when applicable.
    pub failure: Option<ThumbnailFailureReason>,
}

/// Immutable bounded thumbnail execution and residency evidence.
#[derive(Debug, Clone, Default)]
pub struct ThumbnailDiagnostics {
    /// Current color-context generation.
    pub generation: u64,
    /// Resident successful rasters.
    pub cached_entries: usize,
    /// Resident raster bytes.
    pub cached_bytes: usize,
    /// Configured raster byte budget.
    pub cache_byte_budget: usize,
    /// Admitted requests without a terminal result.
    pub pending_requests: usize,
    /// Admitted requests waiting for worker transport capacity.
    pub deferred_requests: usize,
    /// Retained deduplicated failures.
    pub retained_failures: usize,
    /// Service-capacity rejections.
    pub rejections: u64,
    /// Successful completions.
    pub completions: u64,
    /// Failed attempts.
    pub failures: u64,
    /// Cooperatively canceled attempts.
    pub cancellations: u64,
    /// Stale results rejected at publication.
    pub superseded: u64,
    /// LRU evictions.
    pub evictions: u64,
    /// Failure counts by stable reason.
    pub failures_by_reason: HashMap<ThumbnailFailureReason, u64>,
    /// Bounded terminal execution records.
    pub terminal_records: Vec<ThumbnailTerminalRecord>,
}

/// Production owner for asset-thumbnail execution.
pub struct AssetThumbnailService {
    state: Mutex<ThumbnailState>,
    jobs: mpsc::SyncSender<ThumbnailJob>,
    results: Mutex<mpsc::Receiver<ThumbnailResult>>,
}

impl AssetThumbnailService {
    /// Start a bounded thumbnail service and dedicated deterministic-still worker.
    pub fn new() -> Arc<Self> {
        let (job_tx, job_rx) = mpsc::sync_channel(THUMBNAIL_JOB_QUEUE_CAPACITY);
        let (result_tx, result_rx) = mpsc::sync_channel(THUMBNAIL_JOB_QUEUE_CAPACITY + 1);
        if let Err(error) = std::thread::Builder::new()
            .name("mondrian-asset-thumbnails".to_owned())
            .spawn(move || thumbnail_worker(job_rx, result_tx))
        {
            tracing::error!(%error, "failed to start asset thumbnail worker");
        }
        Arc::new(Self {
            state: Mutex::new(ThumbnailState::default()),
            jobs: job_tx,
            results: Mutex::new(result_rx),
        })
    }

    /// Rotate execution generation when the resolved sequence/project color context changes.
    pub fn set_color_context(&self, context: Option<ColorContext>) {
        let mut state = self.state.lock();
        if state.color_context == context {
            return;
        }
        for pending in state.pending.values() {
            pending.cancellation.cancel();
        }
        retire_deferred(&mut state);
        state.generation = state.generation.saturating_add(1).max(1);
        state.color_context = context;
        state.cache.clear();
        state.cache_lru.clear();
        state.cached_bytes = 0;
        state.failures.clear();
        state.failure_lru.clear();
        state.pending.clear();
        state.active.clear();
    }

    /// Resolve or admit the exact thumbnail request without waiting for media work.
    pub fn thumbnail_for_asset(&self, asset: &AssetRecord) -> ThumbnailLookupState {
        if asset.kind != AssetKind::Video {
            return ThumbnailLookupState::Unavailable;
        }
        let (generation, context) = {
            let state = self.state.lock();
            let Some(context) = state.color_context.clone() else {
                return ThumbnailLookupState::Unavailable;
            };
            (state.generation, context)
        };
        let metadata = match std::fs::metadata(&asset.path) {
            Ok(metadata) => metadata,
            Err(error) => {
                return self.retain_immediate_failure(
                    asset.id,
                    asset.path.clone(),
                    PreviewFileFingerprint {
                        len: None,
                        modified_secs: None,
                        modified_nanos: None,
                    },
                    None,
                    ThumbnailFailure::new(
                        ThumbnailFailureReason::MissingSourceFile,
                        format!("thumbnail source is unavailable: {error}"),
                    ),
                );
            }
        };
        let fingerprint = PreviewFileFingerprint::from_metadata(&metadata);
        let color = match ThumbnailColorContract::resolve(asset, &context) {
            Ok(color) => color,
            Err(failure) => {
                return self.retain_immediate_failure(
                    asset.id,
                    asset.path.clone(),
                    fingerprint,
                    None,
                    failure,
                );
            }
        };
        let key = ThumbnailRequestKey {
            asset_id: asset.id,
            path: asset.path.clone(),
            fingerprint,
            color,
        };
        {
            let mut state = self.state.lock();
            if let Some(entry) = state.cache.get(&asset.id).cloned() {
                if entry.key == key {
                    touch_asset(&mut state.cache_lru, asset.id);
                    return ThumbnailLookupState::Ready(entry.frame);
                }
            }
            if let Some(entry) = state.failures.get(&asset.id) {
                let failure_key = ThumbnailFailureKey {
                    asset_id: key.asset_id,
                    path: key.path.clone(),
                    fingerprint: key.fingerprint,
                    color: Some(key.color.clone()),
                };
                if entry.key == failure_key {
                    return ThumbnailLookupState::Failed(entry.failure.clone());
                }
            }
            if state.pending.contains_key(&key) {
                return ThumbnailLookupState::Loading;
            }
        }
        self.admit(key, generation)
    }

    /// Drain bounded completions and progressively feed the worker transport.
    pub fn poll_finished(&self) -> bool {
        self.poll_finished_with_budget(
            THUMBNAIL_MAX_COMPLETED_RESULTS_PER_POLL,
            THUMBNAIL_COMPLETED_RESULTS_POLL_BUDGET,
        )
    }

    fn poll_finished_with_budget(&self, max_results: usize, time_budget: Duration) -> bool {
        self.dispatch_deferred(THUMBNAIL_MAX_DEFERRED_DISPATCH_PER_POLL);
        let started = Instant::now();
        let results = self.results.lock();
        let mut changed = false;
        let mut drained = 0;
        let mut budget_exhausted = false;
        while drained < max_results {
            if drained > 0 && started.elapsed() >= time_budget {
                budget_exhausted = true;
                break;
            }
            let result = match results.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            drained += 1;
            changed |= self.publish(result);
        }
        drop(results);
        self.dispatch_deferred(THUMBNAIL_MAX_DEFERRED_DISPATCH_PER_POLL);
        changed || budget_exhausted || (max_results > 0 && drained == max_results)
    }

    /// Snapshot bounded execution, cache, failure, and terminal evidence.
    pub fn diagnostics(&self) -> ThumbnailDiagnostics {
        let state = self.state.lock();
        ThumbnailDiagnostics {
            generation: state.generation,
            cached_entries: state.cache.len(),
            cached_bytes: state.cached_bytes,
            cache_byte_budget: THUMBNAIL_CACHE_BYTE_BUDGET,
            pending_requests: state.pending.len(),
            deferred_requests: state.deferred.len(),
            retained_failures: state.failures.len(),
            rejections: state.counters.rejections,
            completions: state.counters.completions,
            failures: state.counters.failures,
            cancellations: state.counters.cancellations,
            superseded: state.counters.superseded,
            evictions: state.counters.evictions,
            failures_by_reason: state.counters.failures_by_reason.clone(),
            terminal_records: state.terminal_records.iter().cloned().collect(),
        }
    }

    fn admit(&self, key: ThumbnailRequestKey, generation: u64) -> ThumbnailLookupState {
        let cancellation = ExecutionCancellationToken::new();
        let job = ThumbnailJob {
            key: key.clone(),
            generation,
            cancellation: cancellation.clone(),
        };
        let mut state = self.state.lock();
        if state.generation != generation || state.color_context.is_none() {
            cancellation.cancel();
            return ThumbnailLookupState::Loading;
        }
        if state.pending.len() >= THUMBNAIL_PENDING_CAPACITY {
            state.counters.rejections = state.counters.rejections.saturating_add(1);
            push_terminal(
                &mut state,
                &key,
                generation,
                ExecutionTerminalDisposition::Rejected,
                Duration::ZERO,
                Some(ThumbnailFailureReason::AdmissionRejected),
            );
            return ThumbnailLookupState::Failed(ThumbnailFailure::new(
                ThumbnailFailureReason::AdmissionRejected,
                "thumbnail service demand capacity is exhausted",
            ));
        }
        if let Some(previous) = state.active.insert(key.asset_id, key.clone()) {
            if previous != key {
                if let Some(pending) = state.pending.get(&previous) {
                    pending.cancellation.cancel();
                }
            }
        }
        state.pending.insert(key.clone(), PendingThumbnail { generation, cancellation });
        match self.jobs.try_send(job) {
            Ok(()) => ThumbnailLookupState::Loading,
            Err(mpsc::TrySendError::Full(job)) => {
                state.deferred.push_back(job);
                ThumbnailLookupState::Loading
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                state.pending.remove(&key);
                state.active.remove(&key.asset_id);
                let failure = ThumbnailFailure::new(
                    ThumbnailFailureReason::WorkerUnavailable,
                    "thumbnail worker transport is disconnected",
                );
                retain_failure(&mut state, key.clone(), failure.clone());
                push_terminal(
                    &mut state,
                    &key,
                    generation,
                    ExecutionTerminalDisposition::Failed,
                    Duration::ZERO,
                    Some(failure.reason),
                );
                ThumbnailLookupState::Failed(failure)
            }
        }
    }

    fn dispatch_deferred(&self, max_jobs: usize) {
        for _ in 0..max_jobs {
            let mut state = self.state.lock();
            let Some(job) = state.deferred.pop_front() else {
                break;
            };
            if job.cancellation.is_canceled() {
                state.pending.remove(&job.key);
                state.counters.cancellations = state.counters.cancellations.saturating_add(1);
                push_terminal(
                    &mut state,
                    &job.key,
                    job.generation,
                    ExecutionTerminalDisposition::Canceled,
                    Duration::ZERO,
                    Some(ThumbnailFailureReason::DecodeCanceled),
                );
                continue;
            }
            match self.jobs.try_send(job) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(job)) => {
                    state.deferred.push_front(job);
                    break;
                }
                Err(mpsc::TrySendError::Disconnected(job)) => {
                    state.pending.remove(&job.key);
                    if state.active.get(&job.key.asset_id) == Some(&job.key) {
                        state.active.remove(&job.key.asset_id);
                    }
                    let failure = ThumbnailFailure::new(
                        ThumbnailFailureReason::WorkerUnavailable,
                        "thumbnail worker transport is disconnected",
                    );
                    retain_failure(&mut state, job.key.clone(), failure.clone());
                    push_terminal(
                        &mut state,
                        &job.key,
                        job.generation,
                        ExecutionTerminalDisposition::Failed,
                        Duration::ZERO,
                        Some(failure.reason),
                    );
                }
            }
        }
    }

    fn publish(&self, result: ThumbnailResult) -> bool {
        let mut state = self.state.lock();
        let owns = state
            .pending
            .get(&result.key)
            .is_some_and(|pending| pending.generation == result.generation);
        if owns {
            state.pending.remove(&result.key);
        }
        let current = owns
            && state.generation == result.generation
            && state.active.get(&result.key.asset_id) == Some(&result.key);
        if !current {
            let canceled = matches!(
                &result.result,
                Err(ThumbnailFailure { reason: ThumbnailFailureReason::DecodeCanceled, .. })
            );
            let (disposition, failure) = if canceled {
                state.counters.cancellations = state.counters.cancellations.saturating_add(1);
                (
                    ExecutionTerminalDisposition::Canceled,
                    Some(ThumbnailFailureReason::DecodeCanceled),
                )
            } else {
                state.counters.superseded = state.counters.superseded.saturating_add(1);
                (ExecutionTerminalDisposition::Superseded, None)
            };
            push_terminal(
                &mut state,
                &result.key,
                result.generation,
                disposition,
                result.elapsed,
                failure,
            );
            return false;
        }
        state.active.remove(&result.key.asset_id);
        match result.result {
            Ok(frame) => {
                state.counters.completions = state.counters.completions.saturating_add(1);
                let bytes = frame.reserved_bytes();
                if let Some(previous) = state.cache.remove(&result.key.asset_id) {
                    state.cached_bytes =
                        state.cached_bytes.saturating_sub(previous.frame.reserved_bytes());
                    state.cache_lru.retain(|asset_id| *asset_id != result.key.asset_id);
                }
                while !state.cache.is_empty()
                    && (state.cache.len() >= THUMBNAIL_CACHE_ENTRY_CAPACITY
                        || state.cached_bytes.saturating_add(bytes) > THUMBNAIL_CACHE_BYTE_BUDGET)
                {
                    let Some(asset_id) = state.cache_lru.pop_back() else {
                        break;
                    };
                    if let Some(evicted) = state.cache.remove(&asset_id) {
                        state.cached_bytes =
                            state.cached_bytes.saturating_sub(evicted.frame.reserved_bytes());
                        state.counters.evictions = state.counters.evictions.saturating_add(1);
                    }
                }
                if bytes <= THUMBNAIL_CACHE_BYTE_BUDGET {
                    state.cached_bytes = state.cached_bytes.saturating_add(bytes);
                    touch_asset(&mut state.cache_lru, result.key.asset_id);
                    state.cache.insert(
                        result.key.asset_id,
                        ThumbnailCacheEntry { key: result.key.clone(), frame },
                    );
                }
                state.failures.remove(&result.key.asset_id);
                state.failure_lru.retain(|asset_id| *asset_id != result.key.asset_id);
                push_terminal(
                    &mut state,
                    &result.key,
                    result.generation,
                    ExecutionTerminalDisposition::Completed,
                    result.elapsed,
                    None,
                );
                true
            }
            Err(failure) => {
                let canceled = failure.reason == ThumbnailFailureReason::DecodeCanceled;
                let disposition = if canceled {
                    state.counters.cancellations = state.counters.cancellations.saturating_add(1);
                    ExecutionTerminalDisposition::Canceled
                } else {
                    retain_failure(&mut state, result.key.clone(), failure.clone());
                    ExecutionTerminalDisposition::Failed
                };
                push_terminal(
                    &mut state,
                    &result.key,
                    result.generation,
                    disposition,
                    result.elapsed,
                    Some(failure.reason),
                );
                !canceled
            }
        }
    }

    fn retain_immediate_failure(
        &self,
        asset_id: AssetId,
        path: PathBuf,
        fingerprint: PreviewFileFingerprint,
        color: Option<ThumbnailColorContract>,
        failure: ThumbnailFailure,
    ) -> ThumbnailLookupState {
        let mut state = self.state.lock();
        let key = ThumbnailFailureKey { asset_id, path, fingerprint, color };
        let duplicate = state
            .failures
            .get(&asset_id)
            .is_some_and(|entry| entry.key == key && entry.failure.reason == failure.reason);
        if !duplicate {
            let generation = state.generation;
            retain_failure_entry(&mut state, key.clone(), failure.clone());
            push_terminal_identity(
                &mut state,
                asset_id,
                fingerprint,
                generation,
                ExecutionTerminalDisposition::Failed,
                Duration::ZERO,
                Some(failure.reason),
            );
        }
        ThumbnailLookupState::Failed(failure)
    }
}
