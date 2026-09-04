//! UI-independent bounded waveform analysis service.
//!
//! The service owns source revision, admission, generation cancellation,
//! windowed media decode, result publication, cache bounds, and terminal
//! evidence. Window code polls it and injects a shallow lookup handle into the
//! Timeline widget; no Widget or paint callback owns FFmpeg state.

use std::any::Any;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mondrian_assets::AssetLibrary;
use mondrian_core::{
    AssetId, AudioSourceComponentId, AudioSourceSelection, ExecutionCancellationToken,
    ExecutionTerminalDisposition, ExecutionTerminalEvidence,
};
pub use mondrian_media::MAX_WAVEFORM_WIDTH as WAVEFORM_MAX_WIDTH;
use mondrian_media::{
    AudioSourceCache, AudioSourceCacheConfig, AudioSourceCacheDiagnostics,
    AudioSourceCacheShutdownEvidence,
};
use parking_lot::{Condvar, Mutex};

use crate::app::single_worker_activity::{SingleWorkerActivity, SingleWorkerPhase};

use analysis::{slice_and_resample, waveform_worker, WaveformWorkerExit};
use state::{
    duration_to_waveform_frames, push_terminal, retain_failure_locked, rotate_waveform_generation,
    touch_key, PendingWaveform, WaveformFailure, WaveformJob, WaveformResult, WaveformSourceKey,
    WaveformState, WaveformWorkerIdentity,
};

mod analysis;
mod startup;
mod state;
pub use startup::{
    AudioWaveformStartupFailure, AudioWaveformStartupPanic, AudioWaveformStartupShutdownEvidence,
    AudioWaveformStartupStage,
};
#[cfg(test)]
mod tests;

const WAVEFORM_SAMPLE_RATE: u32 = 48_000;
const WAVEFORM_DECODE_WINDOW_SECONDS: usize = 10;
const WAVEFORM_SOURCE_CACHE_ENTRIES: usize = 512;
const WAVEFORM_SOURCE_CACHE_BYTE_BUDGET: usize = 128 * 1024 * 1024;
const WAVEFORM_FAILURE_CAPACITY: usize = 128;
const WAVEFORM_TERMINAL_EVIDENCE_CAPACITY: usize = 256;
const WAVEFORM_PENDING_CAPACITY: usize = 512;
const WAVEFORM_JOB_QUEUE_CAPACITY: usize = 16;
const WAVEFORM_MAX_DEFERRED_DISPATCH_PER_POLL: usize = 8;
const WAVEFORM_SOURCE_WINDOW_ENTRIES: usize = 4;
const WAVEFORM_PCM_CACHE_SHARE_DIVISOR: usize = 4;
const WAVEFORM_TYPICAL_STEREO_WINDOW_BYTES: usize =
    WAVEFORM_SAMPLE_RATE as usize * WAVEFORM_DECODE_WINDOW_SECONDS * 2 * std::mem::size_of::<f32>();
const WAVEFORM_MAX_COMPLETED_RESULTS_PER_POLL: usize = 8;
const WAVEFORM_COMPLETED_RESULTS_POLL_BUDGET: Duration = Duration::from_micros(2_000);
const WAVEFORM_WORKER_TERMINAL_RUNNING: u8 = 0;
const WAVEFORM_WORKER_TERMINAL_RETURNED: u8 = 1;
const WAVEFORM_WORKER_TERMINAL_PANICKED: u8 = 2;
const WAVEFORM_WORKER_TERMINAL_PANICKED_OWNER_ABANDONED: u8 = 3;
const WAVEFORM_WORKER_TERMINAL_FAILED: u8 = 4;

/// Consuming-style closure evidence for the product waveform service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AudioWaveformShutdownEvidence {
    /// Evidence schema version.
    pub schema_version: u32,
    /// Dedicated analysis workers configured for this service.
    pub workers_configured: u32,
    /// Dedicated analysis workers successfully started.
    pub workers_started: u32,
    /// Worker start attempts that failed before ownership transfer.
    pub worker_start_failures: u32,
    /// Started workers that returned and were joined.
    pub workers_terminated: u32,
    /// Joined workers whose outer supervisor observed a panic.
    pub worker_panics: u32,
    /// Workers that returned because their bounded result transport failed.
    pub worker_failures: u32,
    /// Workers still running at the supplied absolute deadline.
    pub worker_timeouts: u32,
    /// Timed-out worker handles detached for bounded caller return.
    pub worker_detachments: u32,
    /// Opaque panic or spawn-error payload owners deliberately abandoned.
    pub worker_owner_abandonments: u32,
    /// Admitted requests at the first shutdown signal.
    pub pending_requests_before: usize,
    /// Deferred requests at the first shutdown signal.
    pub deferred_requests_before: usize,
    /// Physical worker requests at the first shutdown signal.
    pub running_requests_before: usize,
    /// Completed results awaiting publication at the first shutdown signal.
    pub awaiting_publication_before: usize,
    /// Logical requests still retained after closure.
    pub pending_requests_remaining: usize,
    /// Deferred requests still retained after closure.
    pub deferred_requests_remaining: usize,
    /// Physical worker requests still retained after closure.
    pub running_requests_remaining: usize,
    /// Results still awaiting publication acknowledgement after closure.
    pub awaiting_publication_remaining: usize,
    /// Strong decoded-source cache references outside the service owner.
    pub external_source_cache_references: usize,
    /// Terminal decoded-source cache evidence.
    pub source_cache: AudioSourceCacheShutdownEvidence,
}

impl AudioWaveformShutdownEvidence {
    /// Return true only for a current, complete, panic-free owner closure.
    pub fn all_resources_released(self) -> bool {
        self.analysis_resources_released(1) && self.source_cache.all_resources_released()
    }

    fn analysis_resources_released(self, expected_workers: u32) -> bool {
        self.schema_version == 1
            && self.workers_configured == 1
            && self.workers_started == expected_workers
            && self.worker_start_failures == 0
            && self.workers_terminated == expected_workers
            && self.worker_panics == 0
            && self.worker_failures == 0
            && self.worker_timeouts == 0
            && self.worker_detachments == 0
            && self.worker_owner_abandonments == 0
            && self.pending_requests_remaining == 0
            && self.deferred_requests_remaining == 0
            && self.running_requests_remaining == 0
            && self.awaiting_publication_remaining == 0
            && self.external_source_cache_references == 0
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct WaveformShutdownBoundary {
    pending_requests: usize,
    deferred_requests: usize,
    running_requests: usize,
    awaiting_publication: usize,
}

struct WaveformShutdownControl {
    worker: Option<JoinHandle<()>>,
    boundary: Option<WaveformShutdownBoundary>,
    receipt: Option<AudioWaveformShutdownEvidence>,
    workers_started: u32,
    worker_start_failures: u32,
    worker_owner_abandonments: u32,
}

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
    /// Exact physical stream and file revision used for the request.
    pub source: AudioSourceSelection,
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
    /// Whether lookup may admit new automatic waveform work.
    pub automatic_admission_enabled: bool,
    /// Whether deferred analysis may enter the dedicated worker.
    pub dispatch_enabled: bool,
    /// Number of resident source envelopes.
    pub cached_sources: usize,
    /// Bytes retained by source envelopes.
    pub cached_source_bytes: usize,
    /// Product-configured source-envelope byte budget.
    pub source_cache_byte_budget: usize,
    /// Product-configured aggregate residency budget shared explicitly between
    /// waveform envelopes and this service's independent decoded-PCM cache.
    pub aggregate_cache_byte_budget: usize,
    /// Number of admitted analyses awaiting a terminal result.
    pub pending_sources: usize,
    /// Admitted analyses waiting before physical execution.
    pub queued_sources: usize,
    /// Admitted analyses waiting for worker-channel capacity.
    pub deferred_sources: usize,
    /// Analyses physically executing inside the waveform worker.
    pub running_sources: usize,
    /// Current physical worker phase.
    pub worker_phase: WaveformWorkerPhase,
    /// Completed worker results waiting for service publication.
    pub awaiting_publication: usize,
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

/// Physical phase of the dedicated waveform worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaveformWorkerPhase {
    /// No request currently owns the worker.
    #[default]
    Idle,
    /// A request was dequeued but has not crossed the dispatch gate.
    WaitingForDispatch,
    /// A request is executing decode/envelope analysis.
    Running,
}

/// Cloneable, shallow Timeline Adapter over the analysis service.
#[derive(Clone)]
pub struct AudioWaveformSource {
    service: Weak<AudioWaveformService>,
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
        source: &AudioSourceSelection,
        start_secs: f64,
        end_secs: f64,
        pixel_width: u32,
    ) -> Option<Vec<f32>> {
        self.service
            .upgrade()?
            .lookup(asset_id, source, start_secs, end_secs, pixel_width)
    }
}

/// Production owner for background waveform execution.
pub struct AudioWaveformService {
    resource_policy: Mutex<()>,
    state: Mutex<WaveformState>,
    jobs: Mutex<Option<mpsc::SyncSender<WaveformJob>>>,
    results: Mutex<mpsc::Receiver<WaveformResult>>,
    source_cache: Mutex<Option<Arc<AudioSourceCache>>>,
    dispatch_gate: Arc<WaveformDispatchGate>,
    worker_activity: Arc<SingleWorkerActivity<WaveformWorkerIdentity>>,
    worker_terminal: Arc<AtomicU8>,
    shutdown: Mutex<WaveformShutdownControl>,
}

struct WaveformDispatchGate {
    enabled: Mutex<bool>,
    changed: Condvar,
    shutdown: AtomicBool,
}

impl WaveformDispatchGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            enabled: Mutex::new(true),
            changed: Condvar::new(),
            shutdown: AtomicBool::new(false),
        })
    }

    fn set_enabled(&self, enabled: bool) {
        *self.enabled.lock() = enabled;
        self.changed.notify_all();
    }

    fn wait_until_enabled(&self, cancellation: &ExecutionCancellationToken) -> bool {
        let mut enabled = self.enabled.lock();
        while !*enabled && !self.shutdown.load(Ordering::Acquire) && !cancellation.is_canceled() {
            self.changed.wait_for(&mut enabled, Duration::from_millis(5));
        }
        *enabled && !self.shutdown.load(Ordering::Acquire) && !cancellation.is_canceled()
    }
}

impl AudioWaveformService {
    /// Create a shallow, cloneable lookup Adapter for presentation code.
    pub fn source(self: &Arc<Self>) -> AudioWaveformSource {
        AudioWaveformSource { service: Arc::downgrade(self) }
    }

    /// Bind the current project library. A different library rotates the
    /// generation, cancels all admitted work, and clears project-local state.
    pub fn set_library(&self, library: Option<Arc<AssetLibrary>>) {
        if self.dispatch_gate.shutdown.load(Ordering::Acquire) {
            return;
        }
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

    /// Apply product resource policy without changing Waveform-owned source
    /// identity, bounded admission, cancellation, or terminal evidence.
    pub(crate) fn set_resource_policy(
        &self,
        admit_automatic: bool,
        dispatch_enabled: bool,
        aggregate_cache_byte_budget: usize,
    ) {
        if self.dispatch_gate.shutdown.load(Ordering::Acquire) {
            return;
        }
        let _resource_policy = self.resource_policy.lock();
        let aggregate_cache_byte_budget = aggregate_cache_byte_budget.max(2);
        let (envelope_cache_byte_budget, pcm_cache_byte_budget) =
            waveform_cache_partition(aggregate_cache_byte_budget);
        let should_dispatch = {
            let mut state = self.state.lock();
            if state.admit_automatic == admit_automatic
                && state.dispatch_enabled == dispatch_enabled
                && state.aggregate_cache_byte_budget == aggregate_cache_byte_budget
            {
                return;
            }
            state.admit_automatic = admit_automatic;
            state.dispatch_enabled = dispatch_enabled;
            state.aggregate_cache_byte_budget = aggregate_cache_byte_budget;
            state.source_cache_byte_budget = envelope_cache_byte_budget;
            trim_waveform_cache_to_budget(&mut state);
            dispatch_enabled
        };
        if let Some(source_cache) = self.source_cache.lock().as_ref().cloned() {
            source_cache.reconfigure(AudioSourceCacheConfig::new(
                waveform_pcm_entry_capacity(pcm_cache_byte_budget),
                pcm_cache_byte_budget,
                1,
            ));
        }
        self.dispatch_gate.set_enabled(dispatch_enabled);
        if should_dispatch {
            self.dispatch_deferred(WAVEFORM_MAX_DEFERRED_DISPATCH_PER_POLL);
        }
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
        let removed_bytes = state
            .sources
            .iter()
            .filter(|(key, _)| key.asset_id == asset_id)
            .map(|(_, source)| waveform_source_bytes(source))
            .sum::<usize>();
        state.sources.retain(|key, _| key.asset_id != asset_id);
        state.cached_source_bytes = state.cached_source_bytes.saturating_sub(removed_bytes);
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
        let _resource_policy = self.resource_policy.lock();
        let state = self.state.lock();
        let activity = self.worker_activity.snapshot();
        let running_owned = activity.current.as_ref().is_some_and(|(identity, phase)| {
            *phase == SingleWorkerPhase::Running && pending_waveform_owns_identity(&state, identity)
        });
        let awaiting_publication_owned = activity
            .awaiting_publication
            .iter()
            .filter(|identity| pending_waveform_owns_identity(&state, identity))
            .count();
        let awaiting_publication = activity.awaiting_publication.len();
        let worker_phase = match activity.current.as_ref().map(|(_, phase)| *phase) {
            None => WaveformWorkerPhase::Idle,
            Some(SingleWorkerPhase::WaitingForDispatch) => WaveformWorkerPhase::WaitingForDispatch,
            Some(SingleWorkerPhase::Running) => WaveformWorkerPhase::Running,
        };
        AudioWaveformDiagnostics {
            generation: state.generation,
            automatic_admission_enabled: state.admit_automatic,
            dispatch_enabled: state.dispatch_enabled,
            cached_sources: state.sources.len(),
            cached_source_bytes: state.cached_source_bytes,
            source_cache_byte_budget: state.source_cache_byte_budget,
            aggregate_cache_byte_budget: state.aggregate_cache_byte_budget,
            pending_sources: state.pending.len(),
            queued_sources: state
                .pending
                .len()
                .saturating_sub(usize::from(running_owned))
                .saturating_sub(awaiting_publication_owned),
            deferred_sources: state.deferred.len(),
            running_sources: usize::from(worker_phase == WaveformWorkerPhase::Running),
            worker_phase,
            awaiting_publication,
            retained_failures: state.failures.len(),
            queue_rejections: state.counters.queue_rejections,
            completions: state.counters.completions,
            failures: state.counters.failures,
            cancellations: state.counters.cancellations,
            superseded_completions: state.counters.superseded_completions,
            cache_evictions: state.counters.cache_evictions,
            terminal_records: state.terminal_records.iter().cloned().collect(),
            source_cache: self
                .source_cache
                .lock()
                .as_ref()
                .map_or_else(AudioSourceCacheDiagnostics::default, |cache| {
                    cache.diagnostics()
                }),
        }
    }

    /// Close admission and signal every Waveform-owned execution owner.
    ///
    /// This first phase is idempotent and performs no worker join or foreign
    /// process teardown.
    pub fn begin_shutdown(&self) {
        let _resource_policy = self.resource_policy.lock();
        let mut shutdown = self.shutdown.lock();
        if shutdown.boundary.is_some() {
            return;
        }

        self.dispatch_gate.shutdown.store(true, Ordering::Release);
        self.dispatch_gate.changed.notify_all();
        let activity = self.worker_activity.snapshot();
        let mut state = self.state.lock();
        let boundary = WaveformShutdownBoundary {
            pending_requests: state.pending.len(),
            deferred_requests: state.deferred.len(),
            running_requests: usize::from(activity.current.is_some()),
            awaiting_publication: activity.awaiting_publication.len(),
        };
        rotate_waveform_generation(&mut state, None);
        state.admit_automatic = false;
        state.dispatch_enabled = false;
        drop(state);
        self.jobs.lock().take();
        if let Some(source_cache) = self.source_cache.lock().as_ref().cloned() {
            source_cache.begin_shutdown();
        }
        shutdown.boundary = Some(boundary);
    }

    /// Join the analysis worker and consume its decoded-source cache before an
    /// absolute monotonic deadline.
    pub fn shutdown_until(&self, deadline: Instant) -> AudioWaveformShutdownEvidence {
        self.begin_shutdown();
        let mut shutdown = self.shutdown.lock();
        if let Some(receipt) = shutdown.receipt {
            return receipt;
        }
        let boundary = shutdown.boundary.unwrap_or_default();
        let mut workers_terminated = 0_u32;
        let mut worker_panics = 0_u32;
        let mut worker_failures = 0_u32;
        let mut worker_timeouts = 0_u32;
        let mut worker_detachments = 0_u32;
        let mut worker_owner_abandonments = shutdown.worker_owner_abandonments;

        if let Some(worker) = shutdown.worker.take() {
            if worker.thread().id() == std::thread::current().id() {
                worker_detachments = 1;
            } else {
                let worker = worker;
                loop {
                    self.drain_shutdown_results();
                    if worker.is_finished() {
                        workers_terminated = 1;
                        if let Err(payload) = worker.join() {
                            worker_panics = 1;
                            worker_owner_abandonments =
                                worker_owner_abandonments.saturating_add(u32::from(
                                    dispose_canonical_or_abandon_opaque_panic_payload(payload),
                                ));
                        } else {
                            match self.worker_terminal.load(Ordering::Acquire) {
                                WAVEFORM_WORKER_TERMINAL_RETURNED => {}
                                WAVEFORM_WORKER_TERMINAL_PANICKED => worker_panics = 1,
                                WAVEFORM_WORKER_TERMINAL_PANICKED_OWNER_ABANDONED => {
                                    worker_panics = 1;
                                    worker_owner_abandonments =
                                        worker_owner_abandonments.saturating_add(1);
                                }
                                WAVEFORM_WORKER_TERMINAL_FAILED => worker_failures = 1,
                                _ => worker_panics = 1,
                            }
                        }
                        break;
                    }
                    let now = Instant::now();
                    if now >= deadline {
                        worker_timeouts = 1;
                        worker_detachments = 1;
                        break;
                    }
                    std::thread::park_timeout(
                        deadline.saturating_duration_since(now).min(Duration::from_millis(2)),
                    );
                }
            }
        }
        self.drain_shutdown_results();

        let activity = self.worker_activity.snapshot();
        let state = self.state.lock();
        let pending_requests_remaining = state.pending.len();
        let deferred_requests_remaining = state.deferred.len();
        drop(state);

        let mut external_source_cache_references = 0_usize;
        let mut source_cache_evidence = AudioSourceCacheShutdownEvidence::default();
        if let Some(source_cache) = self.source_cache.lock().take() {
            let strong_references = Arc::strong_count(&source_cache);
            if workers_terminated == shutdown.workers_started {
                match Arc::try_unwrap(source_cache) {
                    Ok(source_cache) => {
                        source_cache_evidence = source_cache.shutdown_until(deadline);
                    }
                    Err(source_cache) => {
                        external_source_cache_references =
                            Arc::strong_count(&source_cache).saturating_sub(1);
                        drop(source_cache);
                    }
                }
            } else {
                external_source_cache_references = strong_references.saturating_sub(1);
                drop(source_cache);
            }
        }

        let receipt = AudioWaveformShutdownEvidence {
            schema_version: 1,
            workers_configured: 1,
            workers_started: shutdown.workers_started,
            worker_start_failures: shutdown.worker_start_failures,
            workers_terminated,
            worker_panics,
            worker_failures,
            worker_timeouts,
            worker_detachments,
            worker_owner_abandonments,
            pending_requests_before: boundary.pending_requests,
            deferred_requests_before: boundary.deferred_requests,
            running_requests_before: boundary.running_requests,
            awaiting_publication_before: boundary.awaiting_publication,
            pending_requests_remaining,
            deferred_requests_remaining,
            running_requests_remaining: usize::from(activity.current.is_some()),
            awaiting_publication_remaining: activity.awaiting_publication.len(),
            external_source_cache_references,
            source_cache: source_cache_evidence,
        };
        shutdown.receipt = Some(receipt);
        receipt
    }

    fn drain_shutdown_results(&self) {
        let results = self.results.lock();
        while let Ok(result) = results.try_recv() {
            self.worker_activity.acknowledge_publication(&result.worker_identity());
        }
    }

    fn lookup(
        &self,
        asset_id: AssetId,
        source: &AudioSourceSelection,
        start_secs: f64,
        end_secs: f64,
        pixel_width: u32,
    ) -> Option<Vec<f32>> {
        if self.dispatch_gate.shutdown.load(Ordering::Acquire) {
            return None;
        }
        let key = WaveformSourceKey { asset_id, selection: source.clone() };
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
            if !state.admit_automatic {
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
        let Some(media_probe) = record.media_probe() else {
            self.retain_failure(
                key,
                generation,
                WaveformFailure::new(
                    WaveformFailureReason::AudioStreamUnavailable,
                    "waveform asset has no current media probe facts",
                ),
            );
            return;
        };
        let Some(path) = record.file_path().map(std::path::Path::to_path_buf) else {
            self.retain_failure(
                key,
                generation,
                WaveformFailure::new(
                    WaveformFailureReason::AssetUnavailable,
                    "waveform asset is not file-backed",
                ),
            );
            return;
        };
        let Some(admitted_selection) =
            record.admitted_audio_source_selection(AudioSourceComponentId::primary())
        else {
            self.retain_failure(
                key,
                generation,
                WaveformFailure::new(
                    WaveformFailureReason::AudioStreamUnavailable,
                    "waveform asset has no admitted primary audio Component binding",
                ),
            );
            return;
        };
        if admitted_selection != key.selection {
            self.retain_failure(
                key,
                generation,
                WaveformFailure::new(
                    WaveformFailureReason::AudioStreamUnavailable,
                    "waveform request carries a stale physical stream or source revision",
                ),
            );
            return;
        }
        let current_fingerprint = mondrian_core::MediaFileFingerprint::capture(&path);
        if !current_fingerprint.authorizes_reuse()
            || current_fingerprint != key.selection.source_fingerprint()
        {
            self.retain_failure(
                key,
                generation,
                WaveformFailure::new(
                    WaveformFailureReason::AudioStreamUnavailable,
                    "waveform source revision cannot be revalidated at execution",
                ),
            );
            return;
        }
        let Some(audio) = media_probe
            .audio_streams
            .iter()
            .find(|stream| stream.index == key.selection.stream_index())
        else {
            self.retain_failure(
                key,
                generation,
                WaveformFailure::new(
                    WaveformFailureReason::AudioStreamUnavailable,
                    "waveform physical stream is absent from current probe evidence",
                ),
            );
            return;
        };
        let duration = audio
            .duration
            .filter(|duration| !duration.is_zero())
            .or_else(|| (!media_probe.duration.is_zero()).then_some(media_probe.duration));
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
            path,
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
        if let Some(previous) = state.active_keys.insert(key.asset_id, key.clone())
            && previous != key
            && let Some(pending) = state.pending.get(&previous)
        {
            pending.cancellation.cancel();
        }
        state.pending.insert(key.clone(), PendingWaveform { generation, cancellation });
        if !state.dispatch_enabled {
            state.deferred.push_back(job);
            return;
        }
        let sender = self.jobs.lock().as_ref().cloned();
        let send = match sender {
            Some(sender) => sender.try_send(job),
            None => Err(mpsc::TrySendError::Disconnected(job)),
        };
        match send {
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
        if self.dispatch_gate.shutdown.load(Ordering::Acquire) {
            return;
        }
        for _ in 0..max_jobs {
            let mut state = self.state.lock();
            if !state.dispatch_enabled {
                break;
            }
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
            let sender = self.jobs.lock().as_ref().cloned();
            let send = match sender {
                Some(sender) => sender.try_send(job),
                None => Err(mpsc::TrySendError::Disconnected(job)),
            };
            match send {
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
        self.worker_activity.acknowledge_publication(&result.worker_identity());
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
                let source_bytes = waveform_source_bytes(&source);
                while !state.sources.is_empty()
                    && (state.sources.len() >= WAVEFORM_SOURCE_CACHE_ENTRIES
                        || state.cached_source_bytes.saturating_add(source_bytes)
                            > state.source_cache_byte_budget)
                {
                    let Some(evicted) = state.source_lru.pop_back() else {
                        break;
                    };
                    if let Some(evicted_source) = state.sources.remove(&evicted) {
                        state.cached_source_bytes = state
                            .cached_source_bytes
                            .saturating_sub(waveform_source_bytes(&evicted_source));
                        state.counters.cache_evictions =
                            state.counters.cache_evictions.saturating_add(1);
                    }
                }
                state.failures.remove(&result.key);
                state.failure_lru.retain(|key| key != &result.key);
                if source_bytes <= state.source_cache_byte_budget {
                    touch_key(&mut state.source_lru, &result.key);
                    state.cached_source_bytes =
                        state.cached_source_bytes.saturating_add(source_bytes);
                    state.sources.insert(result.key.clone(), source);
                }
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

impl Drop for AudioWaveformService {
    fn drop(&mut self) {
        self.begin_shutdown();
        let shutdown = self.shutdown.get_mut();
        if shutdown.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            let worker = shutdown.worker.take();
            if let Some(worker) = worker
                && let Err(payload) = worker.join()
            {
                dispose_canonical_or_abandon_opaque_panic_payload(payload);
            }
        }
    }
}

fn dispose_canonical_or_abandon_opaque_panic_payload(payload: Box<dyn Any + Send>) -> bool {
    if payload.is::<&'static str>() || payload.is::<String>() {
        drop(payload);
        false
    } else {
        std::mem::forget(payload);
        true
    }
}

fn abandon_opaque_io_error(error: std::io::Error) -> bool {
    if error.get_ref().is_some() {
        std::mem::forget(error);
        true
    } else {
        drop(error);
        false
    }
}

fn pending_waveform_owns_identity(
    state: &WaveformState,
    identity: &WaveformWorkerIdentity,
) -> bool {
    state
        .pending
        .get(&identity.key)
        .is_some_and(|pending| pending.generation == identity.generation)
}

fn waveform_source_bytes(source: &state::WaveformSource) -> usize {
    source.envelope.len().saturating_mul(std::mem::size_of::<f32>())
}

fn waveform_cache_partition(aggregate_byte_budget: usize) -> (usize, usize) {
    let aggregate_byte_budget = aggregate_byte_budget.max(2);
    let pcm = (aggregate_byte_budget / WAVEFORM_PCM_CACHE_SHARE_DIVISOR).max(1);
    (aggregate_byte_budget.saturating_sub(pcm).max(1), pcm)
}

fn waveform_pcm_entry_capacity(pcm_byte_budget: usize) -> usize {
    (pcm_byte_budget / WAVEFORM_TYPICAL_STEREO_WINDOW_BYTES)
        .clamp(1, WAVEFORM_SOURCE_WINDOW_ENTRIES)
}

fn trim_waveform_cache_to_budget(state: &mut WaveformState) {
    while state.cached_source_bytes > state.source_cache_byte_budget
        || state.sources.len() > WAVEFORM_SOURCE_CACHE_ENTRIES
    {
        let Some(key) = state.source_lru.pop_back() else {
            break;
        };
        if let Some(source) = state.sources.remove(&key) {
            state.cached_source_bytes =
                state.cached_source_bytes.saturating_sub(waveform_source_bytes(&source));
            state.counters.cache_evictions = state.counters.cache_evictions.saturating_add(1);
        }
    }
}
