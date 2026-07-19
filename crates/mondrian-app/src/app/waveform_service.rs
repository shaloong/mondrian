//! UI-independent bounded waveform analysis service.
//!
//! The service owns source revision, admission, generation cancellation,
//! windowed media decode, result publication, cache bounds, and terminal
//! evidence. Window code polls it and injects a shallow lookup handle into the
//! Timeline widget; no Widget or paint callback owns FFmpeg state.

use std::fmt;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use mondrian_assets::AssetLibrary;
use mondrian_core::{
    AssetId, AudioChannelLayout, ExecutionCancellationToken, ExecutionTerminalDisposition,
    ExecutionTerminalEvidence,
};
pub use mondrian_media::MAX_WAVEFORM_WIDTH as WAVEFORM_MAX_WIDTH;
use mondrian_media::{AudioSourceCache, AudioSourceCacheDiagnostics};
use parking_lot::Mutex;

use analysis::{slice_and_resample, waveform_worker};
use state::{
    duration_to_waveform_frames, push_terminal, retain_failure_locked, rotate_waveform_generation,
    touch_key, PendingWaveform, WaveformFailure, WaveformJob, WaveformResult, WaveformSourceKey,
    WaveformState,
};

mod analysis;
mod state;
#[cfg(test)]
mod tests;

const WAVEFORM_SAMPLE_RATE: u32 = 48_000;
const WAVEFORM_LAYOUT: AudioChannelLayout = AudioChannelLayout::Mono;
const WAVEFORM_DECODE_WINDOW_SECONDS: usize = 10;
const WAVEFORM_SOURCE_CACHE_ENTRIES: usize = 512;
const WAVEFORM_FAILURE_CAPACITY: usize = 128;
const WAVEFORM_TERMINAL_EVIDENCE_CAPACITY: usize = 256;
const WAVEFORM_PENDING_CAPACITY: usize = 512;
const WAVEFORM_JOB_QUEUE_CAPACITY: usize = 16;
const WAVEFORM_MAX_DEFERRED_DISPATCH_PER_POLL: usize = 8;
const WAVEFORM_SOURCE_WINDOW_ENTRIES: usize = 4;
const WAVEFORM_SOURCE_WINDOW_BYTES: usize = 16 * 1024 * 1024;
const WAVEFORM_MAX_COMPLETED_RESULTS_PER_POLL: usize = 8;
const WAVEFORM_COMPLETED_RESULTS_POLL_BUDGET: Duration = Duration::from_micros(2_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaveformFailureReason {
    /// No project asset library is currently bound.
    LibraryUnavailable,
    /// The requested asset cannot be resolved in the bound library.
    AssetUnavailable,
    /// Probe metadata does not expose a usable primary audio stream.
    AudioStreamUnavailable,
    /// Probe metadata does not provide a finite positive duration.
    DurationUnavailable,
    /// The analysis worker is no longer accepting work.
    QueueDisconnected,
    /// Media decode or sample extraction failed.
    DecodeFailed,
    /// The request generation was canceled before completion.
    Canceled,
}

/// One bounded terminal record for diagnostics and Headless verification.
#[derive(Debug, Clone)]
pub struct WaveformTerminalRecord {
    /// Cross-domain terminal execution classification.
    pub evidence: ExecutionTerminalEvidence,
    /// Asset whose analysis reached a terminal state.
    pub asset_id: AssetId,
    /// Revision of the source identity used for the request.
    pub source_revision: u64,
    /// Wall-clock execution time after admission.
    pub elapsed: Duration,
    /// Domain failure classification, when applicable.
    pub failure: Option<WaveformFailureReason>,
}

/// Immutable execution and residency evidence for waveform analysis.
#[derive(Debug, Clone, Default)]
pub struct AudioWaveformDiagnostics {
    /// Current project binding generation.
    pub generation: u64,
    /// Number of resident source envelopes.
    pub cached_sources: usize,
    /// Number of admitted analyses awaiting a terminal result.
    pub pending_sources: usize,
    /// Admitted analyses waiting for worker-channel capacity.
    pub deferred_sources: usize,
    /// Number of bounded, deduplicated failures retained for lookup.
    pub retained_failures: usize,
    /// Number of requests rejected because the bounded service demand capacity was full.
    pub queue_rejections: u64,
    /// Number of successfully completed analyses.
    pub completions: u64,
    /// Number of failed analyses, including dependency-resolution failures.
    pub failures: u64,
    /// Number of admitted analyses that observed cancellation.
    pub cancellations: u64,
    /// Number of results discarded because their generation or source revision was stale.
    pub superseded_completions: u64,
    /// Number of source envelopes removed by the bounded LRU policy.
    pub cache_evictions: u64,
    /// Bounded terminal records suitable for Headless assertions.
    pub terminal_records: Vec<WaveformTerminalRecord>,
    /// Residency and decoder-session evidence for the bounded media cache.
    pub source_cache: AudioSourceCacheDiagnostics,
}

/// Cloneable, shallow Timeline Adapter over the analysis service.
#[derive(Clone)]
pub struct AudioWaveformSource {
    service: Arc<AudioWaveformService>,
}

impl fmt::Debug for AudioWaveformSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("AudioWaveformSource").finish_non_exhaustive()
    }
}

impl AudioWaveformSource {
    /// Return a presentation-width envelope when resident, otherwise admit
    /// analysis and return `None` without blocking the caller.
    pub fn lookup(
        &self,
        asset_id: AssetId,
        source_revision: u64,
        start_secs: f64,
        end_secs: f64,
        pixel_width: u32,
    ) -> Option<Vec<f32>> {
        self.service
            .lookup(asset_id, source_revision, start_secs, end_secs, pixel_width)
    }
}

/// Production owner for background waveform execution.
pub struct AudioWaveformService {
    state: Mutex<WaveformState>,
    jobs: mpsc::SyncSender<WaveformJob>,
    results: Mutex<mpsc::Receiver<WaveformResult>>,
    source_cache: Arc<AudioSourceCache>,
}

impl AudioWaveformService {
    /// Start a bounded waveform analysis service and its dedicated worker.
    pub fn new() -> Arc<Self> {
        let source_cache = Arc::new(AudioSourceCache::new_bounded(
            WAVEFORM_SAMPLE_RATE,
            WAVEFORM_LAYOUT,
            WAVEFORM_DECODE_WINDOW_SECONDS,
            WAVEFORM_SOURCE_WINDOW_ENTRIES,
            WAVEFORM_SOURCE_WINDOW_BYTES,
        ));
        let (job_tx, job_rx) = mpsc::sync_channel(WAVEFORM_JOB_QUEUE_CAPACITY);
        let (result_tx, result_rx) = mpsc::sync_channel(WAVEFORM_JOB_QUEUE_CAPACITY + 1);
        let worker_cache = Arc::clone(&source_cache);
        if let Err(error) = std::thread::Builder::new()
            .name("mondrian-waveform-analysis".to_owned())
            .spawn(move || waveform_worker(job_rx, result_tx, worker_cache))
        {
            tracing::error!(%error, "failed to start waveform analysis worker");
        }
        Arc::new(Self {
            state: Mutex::new(WaveformState::default()),
            jobs: job_tx,
            results: Mutex::new(result_rx),
            source_cache,
        })
    }

    /// Create a shallow, cloneable lookup Adapter for presentation code.
    pub fn source(self: &Arc<Self>) -> AudioWaveformSource {
        AudioWaveformSource { service: Arc::clone(self) }
    }

    /// Bind the current project library. A different library rotates the
    /// generation, cancels all admitted work, and clears project-local state.
    pub fn set_library(&self, library: Option<Arc<AssetLibrary>>) {
        let mut state = self.state.lock();
        let unchanged = match (&state.library, &library) {
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        rotate_waveform_generation(&mut state, library);
    }

    /// Cancel and invalidate every artifact for one relinked or removed asset.
    pub fn evict_asset(&self, asset_id: AssetId) {
        let mut state = self.state.lock();
        for (key, pending) in &state.pending {
            if key.asset_id == asset_id {
                pending.cancellation.cancel();
            }
        }
        let canceled_deferred: Vec<_> = state
            .deferred
            .iter()
            .filter(|job| job.key.asset_id == asset_id)
            .map(|job| (job.key.clone(), job.generation))
            .collect();
        for (key, generation) in canceled_deferred {
            state.counters.cancellations = state.counters.cancellations.saturating_add(1);
            push_terminal(
                &mut state,
                &key,
                generation,
                ExecutionTerminalDisposition::Canceled,
                Duration::ZERO,
                Some(WaveformFailureReason::Canceled),
            );
        }
        state.sources.retain(|key, _| key.asset_id != asset_id);
        state.source_lru.retain(|key| key.asset_id != asset_id);
        state.active_keys.remove(&asset_id);
        state.pending.retain(|key, _| key.asset_id != asset_id);
        state.deferred.retain(|job| job.key.asset_id != asset_id);
        state.failures.retain(|key, _| key.asset_id != asset_id);
        state.failure_lru.retain(|key| key.asset_id != asset_id);
    }

    /// Drain a bounded number of completions without allowing background work
    /// to monopolize an event-loop turn.
    pub fn poll_finished(&self) -> bool {
        self.poll_finished_with_budget(
            WAVEFORM_MAX_COMPLETED_RESULTS_PER_POLL,
            WAVEFORM_COMPLETED_RESULTS_POLL_BUDGET,
        )
    }

    fn poll_finished_with_budget(&self, max_results: usize, time_budget: Duration) -> bool {
        self.dispatch_deferred(WAVEFORM_MAX_DEFERRED_DISPATCH_PER_POLL);
        let started = Instant::now();
        let mut visible_change = false;
        let mut drained = 0;
        let results = self.results.lock();
        while drained < max_results {
            if drained > 0 && started.elapsed() >= time_budget {
                break;
            }
            let result = match results.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            drained += 1;
            visible_change |= self.publish_result(result);
        }
        drop(results);
        self.dispatch_deferred(WAVEFORM_MAX_DEFERRED_DISPATCH_PER_POLL);
        visible_change || (max_results > 0 && drained == max_results)
    }

    /// Snapshot bounded execution, terminal, cache, and decoder evidence.
    pub fn diagnostics(&self) -> AudioWaveformDiagnostics {
        let state = self.state.lock();
        AudioWaveformDiagnostics {
            generation: state.generation,
            cached_sources: state.sources.len(),
            pending_sources: state.pending.len(),
            deferred_sources: state.deferred.len(),
            retained_failures: state.failures.len(),
            queue_rejections: state.counters.queue_rejections,
            completions: state.counters.completions,
            failures: state.counters.failures,
            cancellations: state.counters.cancellations,
            superseded_completions: state.counters.superseded_completions,
            cache_evictions: state.counters.cache_evictions,
            terminal_records: state.terminal_records.iter().cloned().collect(),
            source_cache: self.source_cache.diagnostics(),
        }
    }

    fn lookup(
        &self,
        asset_id: AssetId,
        source_revision: u64,
        start_secs: f64,
        end_secs: f64,
        pixel_width: u32,
    ) -> Option<Vec<f32>> {
        let key = WaveformSourceKey { asset_id, source_revision };
        let source = {
            let mut state = self.state.lock();
            let source = state.sources.get(&key).cloned();
            if source.is_some() {
                touch_key(&mut state.source_lru, &key);
            }
            source
        };
        if let Some(source) = source {
            return Some(slice_and_resample(
                &source,
                start_secs,
                end_secs,
                pixel_width,
            ));
        }
        self.request_source(key);
        None
    }

    fn request_source(&self, key: WaveformSourceKey) {
        let (generation, library) = {
            let state = self.state.lock();
            if state.sources.contains_key(&key)
                || state.pending.contains_key(&key)
                || state.failures.contains_key(&key)
            {
                return;
            }
            let Some(library) = state.library.as_ref().cloned() else {
                let generation = state.generation;
                drop(state);
                self.retain_failure(
                    key,
                    generation,
                    WaveformFailure::new(
                        WaveformFailureReason::LibraryUnavailable,
                        "waveform analysis requires an open asset library",
                    ),
                );
                return;
            };
            (state.generation, library)
        };

        let record = match library.get_asset(key.asset_id) {
            Ok(Some(record)) => record,
            Ok(None) => {
                self.retain_failure(
                    key,
                    generation,
                    WaveformFailure::new(
                        WaveformFailureReason::AssetUnavailable,
                        "waveform asset is absent from the current library",
                    ),
                );
                return;
            }
            Err(error) => {
                self.retain_failure(
                    key,
                    generation,
                    WaveformFailure::new(
                        WaveformFailureReason::AssetUnavailable,
                        error.to_string(),
                    ),
                );
                return;
            }
        };
        let Some(audio) = record.media_info.primary_audio() else {
            self.retain_failure(
                key,
                generation,
                WaveformFailure::new(
                    WaveformFailureReason::AudioStreamUnavailable,
                    "waveform asset has no probed primary audio stream",
                ),
            );
            return;
        };
        let duration = audio.duration.filter(|duration| !duration.is_zero()).or_else(|| {
            (!record.media_info.duration.is_zero()).then_some(record.media_info.duration)
        });
        let Some(total_frames) = duration.and_then(duration_to_waveform_frames) else {
            self.retain_failure(
                key,
                generation,
                WaveformFailure::new(
                    WaveformFailureReason::DurationUnavailable,
                    "waveform analysis requires a finite positive audio-stream duration",
                ),
            );
            return;
        };

        let cancellation = ExecutionCancellationToken::new();
        let job = WaveformJob {
            key: key.clone(),
            generation,
            path: record.path,
            total_frames,
            cancellation: cancellation.clone(),
        };
        let mut state = self.state.lock();
        if state.generation != generation || state.library.as_ref().is_none() {
            cancellation.cancel();
            return;
        }
        if state.pending.len() >= WAVEFORM_PENDING_CAPACITY {
            cancellation.cancel();
            state.counters.queue_rejections = state.counters.queue_rejections.saturating_add(1);
            push_terminal(
                &mut state,
                &key,
                generation,
                ExecutionTerminalDisposition::Rejected,
                Duration::ZERO,
                None,
            );
            return;
        }
        if let Some(previous) = state.active_keys.insert(key.asset_id, key.clone()) {
            if previous != key {
                if let Some(pending) = state.pending.get(&previous) {
                    pending.cancellation.cancel();
                }
            }
        }
        state.pending.insert(key.clone(), PendingWaveform { generation, cancellation });
        match self.jobs.try_send(job) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(job)) => state.deferred.push_back(job),
            Err(mpsc::TrySendError::Disconnected(_job)) => {
                state.pending.remove(&key);
                if state.active_keys.get(&key.asset_id) == Some(&key) {
                    state.active_keys.remove(&key.asset_id);
                }
                drop(state);
                self.retain_failure(
                    key,
                    generation,
                    WaveformFailure::new(
                        WaveformFailureReason::QueueDisconnected,
                        "waveform analysis worker is unavailable",
                    ),
                );
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
                    Some(WaveformFailureReason::Canceled),
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
                    if state.active_keys.get(&job.key.asset_id) == Some(&job.key) {
                        state.active_keys.remove(&job.key.asset_id);
                    }
                    let failure = WaveformFailure::new(
                        WaveformFailureReason::QueueDisconnected,
                        "waveform analysis worker is unavailable",
                    );
                    let reason = failure.reason;
                    state.counters.failures = state.counters.failures.saturating_add(1);
                    retain_failure_locked(&mut state, job.key.clone(), failure);
                    push_terminal(
                        &mut state,
                        &job.key,
                        job.generation,
                        ExecutionTerminalDisposition::Failed,
                        Duration::ZERO,
                        Some(reason),
                    );
                }
            }
        }
    }

    fn publish_result(&self, result: WaveformResult) -> bool {
        let mut state = self.state.lock();
        let owns_pending = state
            .pending
            .get(&result.key)
            .is_some_and(|pending| pending.generation == result.generation);
        if owns_pending {
            state.pending.remove(&result.key);
        }
        let current = state.generation == result.generation
            && state.active_keys.get(&result.key.asset_id) == Some(&result.key);
        if !current {
            state.counters.superseded_completions =
                state.counters.superseded_completions.saturating_add(1);
            push_terminal(
                &mut state,
                &result.key,
                result.generation,
                ExecutionTerminalDisposition::Superseded,
                result.elapsed,
                None,
            );
            return false;
        }

        match result.source {
            Ok(source) => {
                state.counters.completions = state.counters.completions.saturating_add(1);
                while state.sources.len() >= WAVEFORM_SOURCE_CACHE_ENTRIES {
                    let Some(evicted) = state.source_lru.pop_back() else {
                        break;
                    };
                    if state.sources.remove(&evicted).is_some() {
                        state.counters.cache_evictions =
                            state.counters.cache_evictions.saturating_add(1);
                    }
                }
                state.failures.remove(&result.key);
                state.failure_lru.retain(|key| key != &result.key);
                touch_key(&mut state.source_lru, &result.key);
                state.sources.insert(result.key.clone(), source);
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
                let disposition = if failure.reason == WaveformFailureReason::Canceled {
                    state.counters.cancellations = state.counters.cancellations.saturating_add(1);
                    ExecutionTerminalDisposition::Canceled
                } else {
                    state.counters.failures = state.counters.failures.saturating_add(1);
                    ExecutionTerminalDisposition::Failed
                };
                let reason = failure.reason;
                if disposition == ExecutionTerminalDisposition::Failed {
                    retain_failure_locked(&mut state, result.key.clone(), failure);
                }
                push_terminal(
                    &mut state,
                    &result.key,
                    result.generation,
                    disposition,
                    result.elapsed,
                    Some(reason),
                );
                disposition == ExecutionTerminalDisposition::Failed
            }
        }
    }

    fn retain_failure(&self, key: WaveformSourceKey, generation: u64, failure: WaveformFailure) {
        let mut state = self.state.lock();
        let reason = failure.reason;
        state.counters.failures = state.counters.failures.saturating_add(1);
        retain_failure_locked(&mut state, key.clone(), failure);
        push_terminal(
            &mut state,
            &key,
            generation,
            ExecutionTerminalDisposition::Failed,
            Duration::ZERO,
            Some(reason),
        );
    }
}
