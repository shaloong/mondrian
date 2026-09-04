//! Bounded, fingerprinted decoded-audio source windows.

mod mapping;
mod session;

pub use mapping::AudioSourceSelection;

use crate::audio::AudioBuffer;
use crate::owner_lifetime::{abandon_io_error, dispose_canonical_or_abandon_opaque_panic_payload};
use mondrian_core::{AudioChannelLayout, ExecutionCancellationToken, MondrianError, Result};
use parking_lot::{Condvar, Mutex};
use session::PersistentFfmpegAudioWindowDecoder;
use std::collections::VecDeque;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const AUDIO_SOURCE_WINDOW_SECONDS: usize = 10;
const AUDIO_SOURCE_CACHE_ENTRY_CAPACITY: usize = 16;
const AUDIO_SOURCE_CACHE_BYTE_BUDGET: usize = 64 * 1024 * 1024;
const AUDIO_SOURCE_DECODER_SESSION_CAPACITY: usize = 2;
pub(super) const AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX: usize = 64;
const AUDIO_SOURCE_FAILURE_CAPACITY: usize = 64;

/// Online-reconfigurable residency limits for one decoded-audio source cache.
///
/// These limits control retained PCM and persistent decoder residency only.
/// Lowering them never changes sample coordinates, channel mapping, or mixing
/// semantics; a non-resident window is decoded again on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSourceCacheConfig {
    /// Maximum number of retained decoded PCM windows.
    pub entry_capacity: usize,
    /// Maximum retained decoded PCM payload bytes.
    pub byte_budget: usize,
    /// Maximum persistent FFmpeg source sessions.
    pub decoder_session_capacity: usize,
}

impl AudioSourceCacheConfig {
    /// Build normalized limits. Every execution cache retains at least one
    /// admission slot, while a byte budget smaller than one decoded window is
    /// valid and makes that window non-resident after use.
    pub const fn new(
        entry_capacity: usize,
        byte_budget: usize,
        decoder_session_capacity: usize,
    ) -> Self {
        Self {
            entry_capacity: if entry_capacity == 0 {
                1
            } else {
                entry_capacity
            },
            byte_budget: if byte_budget == 0 { 1 } else { byte_budget },
            decoder_session_capacity: if decoder_session_capacity == 0 {
                1
            } else if decoder_session_capacity > AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX {
                AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX
            } else {
                decoder_session_capacity
            },
        }
    }
}

/// Shared weighted-LRU owner for native-layout decoded PCM windows at one sample rate.
pub struct AudioSourceCache {
    sample_rate: u32,
    window_frames: usize,
    configuration: Mutex<()>,
    state: Mutex<AudioSourceCacheState>,
    window_ready: Condvar,
    decoder: Arc<dyn AudioWindowDecoder>,
    decoder_shutdown_signal: Option<Arc<AudioWindowDecoderShutdownSignal>>,
    shutdown_requested: AtomicBool,
}

pub(super) struct AudioWindowDecoderShutdownSignal {
    requested: AtomicBool,
    worker: OnceLock<thread::Thread>,
}

impl AudioWindowDecoderShutdownSignal {
    pub(super) fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            worker: OnceLock::new(),
        }
    }

    pub(super) fn request(&self) {
        self.requested.store(true, Ordering::Release);
        self.notify_worker();
    }

    pub(super) fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    pub(super) fn register_worker(&self) {
        let _already_registered = self.worker.set(thread::current());
    }

    pub(super) fn notify_worker(&self) {
        if let Some(worker) = self.worker.get() {
            worker.unpark();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct AudioSourceIdentity {
    pub(super) path: PathBuf,
    pub(super) len: u64,
    pub(super) modified_secs: Option<u64>,
    pub(super) modified_nanos: Option<u32>,
    pub(super) selection: AudioSourceSelection,
    pub(super) channel_layout: AudioChannelLayout,
}

impl AudioSourceIdentity {
    fn capture(path: &Path, selection: AudioSourceSelection) -> Result<Self> {
        let metadata = std::fs::metadata(path).map_err(|error| MondrianError::DecodeFailed {
            asset_id: path.display().to_string(),
            reason: format!("读取音频源元数据失败: {error}"),
        })?;
        let current_fingerprint = crate::MediaFileFingerprint::capture(path);
        if !current_fingerprint.authorizes_reuse()
            || !selection.source_fingerprint().authorizes_reuse()
        {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: "audio source revision evidence is incomplete".to_owned(),
            });
        }
        if current_fingerprint != selection.source_fingerprint() {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: "audio source revision changed after stream selection".to_owned(),
            });
        }
        let channel_layout = selection.source_layout().exact_signal_layout().ok_or_else(|| {
            MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: format!(
                    "selected audio stream layout {:?} has no exact signal interpretation",
                    selection.source_layout()
                ),
            }
        })?;
        Ok(Self::from_metadata(
            path,
            &metadata,
            selection,
            channel_layout,
        ))
    }

    fn from_metadata(
        path: &Path,
        metadata: &Metadata,
        selection: AudioSourceSelection,
        channel_layout: AudioChannelLayout,
    ) -> Self {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
        Self {
            path: path.to_path_buf(),
            len: metadata.len(),
            modified_secs: modified.map(|duration| duration.as_secs()),
            modified_nanos: modified.map(|duration| duration.subsec_nanos()),
            selection,
            channel_layout,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AudioSourceWindowKey {
    source: AudioSourceIdentity,
    start_frame: i64,
}

struct AudioSourceWindowEntry {
    key: AudioSourceWindowKey,
    buffer: Arc<AudioBuffer>,
    bytes: usize,
}

struct AudioSourceFailureEntry {
    key: AudioSourceWindowKey,
    reason: String,
}

struct AudioSourceCacheState {
    config: AudioSourceCacheConfig,
    entries: VecDeque<AudioSourceWindowEntry>,
    failures: VecDeque<AudioSourceFailureEntry>,
    reserved_bytes: usize,
    hits: u64,
    misses: u64,
    decode_successes: u64,
    decode_failures: u64,
    decode_total_duration_us: u64,
    decode_max_duration_us: u64,
    evictions: u64,
    budget_reconfigurations: u64,
    budget_trim_events: u64,
    budget_trimmed_entries: u64,
    budget_trimmed_bytes: u64,
    oversize_windows: u64,
    in_flight: Vec<AudioSourceWindowKey>,
    peak_in_flight: usize,
}

impl AudioSourceCacheState {
    fn new(config: AudioSourceCacheConfig) -> Self {
        Self {
            config,
            entries: VecDeque::new(),
            failures: VecDeque::new(),
            reserved_bytes: 0,
            hits: 0,
            misses: 0,
            decode_successes: 0,
            decode_failures: 0,
            decode_total_duration_us: 0,
            decode_max_duration_us: 0,
            evictions: 0,
            budget_reconfigurations: 0,
            budget_trim_events: 0,
            budget_trimmed_entries: 0,
            budget_trimmed_bytes: 0,
            oversize_windows: 0,
            in_flight: Vec::new(),
            peak_in_flight: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct AudioWindowDecoderDiagnostics {
    pub(super) sessions: usize,
    pub(super) session_capacity: usize,
    pub(super) peak_sessions: usize,
    pub(super) session_opens: u64,
    pub(super) sequential_reuses: u64,
    pub(super) random_seek_restarts: u64,
    pub(super) session_evictions: u64,
    pub(super) capacity_reconfigurations: u64,
    pub(super) capacity_trim_evictions: u64,
    pub(super) sessions_above_capacity: usize,
    pub(super) cancellations: u64,
    pub(super) cold_window_max_duration_us: u64,
    pub(super) sequential_window_max_duration_us: u64,
    pub(super) random_seek_window_max_duration_us: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct AudioWindowDecoderShutdownEvidence {
    pub(super) sessions_before: usize,
    pub(super) sessions_remaining: usize,
    pub(super) child_processes_observed: usize,
    pub(super) child_processes_terminated: usize,
    pub(super) child_process_termination_failures: usize,
    pub(super) stdout_pump_threads_observed: usize,
    pub(super) stdout_pump_threads_joined: usize,
    pub(super) stdout_pump_threads_panicked: usize,
    pub(super) stdout_pump_thread_owner_abandonments: usize,
    pub(super) stderr_pump_threads_observed: usize,
    pub(super) stderr_pump_threads_joined: usize,
    pub(super) stderr_pump_threads_panicked: usize,
    pub(super) stderr_pump_thread_owner_abandonments: usize,
    pub(super) external_session_slot_references: usize,
    pub(super) resource_handles_remaining: usize,
    pub(super) shutdown_workers_started: u32,
    pub(super) shutdown_workers_terminated: u32,
    pub(super) shutdown_worker_start_failures: u32,
    pub(super) shutdown_worker_panics: u32,
    pub(super) shutdown_worker_publication_missing: u32,
    pub(super) shutdown_worker_owner_abandonments: u32,
}

impl AudioWindowDecoderShutdownEvidence {
    pub(super) const fn all_resources_released(self) -> bool {
        self.sessions_remaining == 0
            && self.child_processes_observed == self.child_processes_terminated
            && self.child_process_termination_failures == 0
            && self.stdout_pump_threads_observed == self.stdout_pump_threads_joined
            && self.stdout_pump_threads_panicked == 0
            && self.stdout_pump_thread_owner_abandonments == 0
            && self.stderr_pump_threads_observed == self.stderr_pump_threads_joined
            && self.stderr_pump_threads_panicked == 0
            && self.stderr_pump_thread_owner_abandonments == 0
            && self.external_session_slot_references == 0
            && self.resource_handles_remaining == 0
            && self.shutdown_workers_started == self.shutdown_workers_terminated
            && self.shutdown_worker_start_failures == 0
            && self.shutdown_worker_panics == 0
            && self.shutdown_worker_publication_missing == 0
            && self.shutdown_worker_owner_abandonments == 0
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.sessions_before = self.sessions_before.saturating_add(other.sessions_before);
        self.sessions_remaining = self.sessions_remaining.saturating_add(other.sessions_remaining);
        self.child_processes_observed =
            self.child_processes_observed.saturating_add(other.child_processes_observed);
        self.child_processes_terminated =
            self.child_processes_terminated.saturating_add(other.child_processes_terminated);
        self.child_process_termination_failures = self
            .child_process_termination_failures
            .saturating_add(other.child_process_termination_failures);
        self.stdout_pump_threads_observed = self
            .stdout_pump_threads_observed
            .saturating_add(other.stdout_pump_threads_observed);
        self.stdout_pump_threads_joined =
            self.stdout_pump_threads_joined.saturating_add(other.stdout_pump_threads_joined);
        self.stdout_pump_threads_panicked = self
            .stdout_pump_threads_panicked
            .saturating_add(other.stdout_pump_threads_panicked);
        self.stdout_pump_thread_owner_abandonments = self
            .stdout_pump_thread_owner_abandonments
            .saturating_add(other.stdout_pump_thread_owner_abandonments);
        self.stderr_pump_threads_observed = self
            .stderr_pump_threads_observed
            .saturating_add(other.stderr_pump_threads_observed);
        self.stderr_pump_threads_joined =
            self.stderr_pump_threads_joined.saturating_add(other.stderr_pump_threads_joined);
        self.stderr_pump_threads_panicked = self
            .stderr_pump_threads_panicked
            .saturating_add(other.stderr_pump_threads_panicked);
        self.stderr_pump_thread_owner_abandonments = self
            .stderr_pump_thread_owner_abandonments
            .saturating_add(other.stderr_pump_thread_owner_abandonments);
        self.external_session_slot_references = self
            .external_session_slot_references
            .saturating_add(other.external_session_slot_references);
        self.resource_handles_remaining =
            self.resource_handles_remaining.saturating_add(other.resource_handles_remaining);
        self.shutdown_workers_started =
            self.shutdown_workers_started.saturating_add(other.shutdown_workers_started);
        self.shutdown_workers_terminated = self
            .shutdown_workers_terminated
            .saturating_add(other.shutdown_workers_terminated);
        self.shutdown_worker_start_failures = self
            .shutdown_worker_start_failures
            .saturating_add(other.shutdown_worker_start_failures);
        self.shutdown_worker_panics =
            self.shutdown_worker_panics.saturating_add(other.shutdown_worker_panics);
        self.shutdown_worker_publication_missing = self
            .shutdown_worker_publication_missing
            .saturating_add(other.shutdown_worker_publication_missing);
        self.shutdown_worker_owner_abandonments = self
            .shutdown_worker_owner_abandonments
            .saturating_add(other.shutdown_worker_owner_abandonments);
    }
}

pub(super) trait AudioWindowDecoder: Send + Sync {
    fn shutdown_signal(&self) -> Option<Arc<AudioWindowDecoderShutdownSignal>> {
        None
    }

    fn decode_window(
        &self,
        source: &AudioSourceIdentity,
        start_frame: i64,
        frame_count: usize,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<AudioBuffer>;

    fn diagnostics(&self) -> AudioWindowDecoderDiagnostics {
        AudioWindowDecoderDiagnostics::default()
    }

    fn reconfigure_session_capacity(&self, _session_capacity: usize) {}

    fn shutdown_sessions(&self) -> AudioWindowDecoderShutdownEvidence {
        let diagnostics = self.diagnostics();
        AudioWindowDecoderShutdownEvidence {
            sessions_before: diagnostics.sessions,
            sessions_remaining: diagnostics.sessions,
            resource_handles_remaining: diagnostics.sessions,
            ..AudioWindowDecoderShutdownEvidence::default()
        }
    }
}

struct ShutdownAudioWindowDecoder;

impl AudioWindowDecoder for ShutdownAudioWindowDecoder {
    fn decode_window(
        &self,
        source: &AudioSourceIdentity,
        _start_frame: i64,
        _frame_count: usize,
        _sample_rate: u32,
        _channel_layout: AudioChannelLayout,
        _cancellation: &ExecutionCancellationToken,
    ) -> Result<AudioBuffer> {
        Err(canceled_audio_decode(&source.path))
    }
}

/// Stable reader for one fingerprinted media source in its exact native layout.
///
/// Readers are cheap handles. PCM ownership remains in the shared weighted LRU
/// and a file replacement creates a different identity on the next `open`.
#[derive(Clone)]
pub struct AudioSourceReader {
    cache: Arc<AudioSourceCache>,
    source: AudioSourceIdentity,
}

/// Point-in-time bounded audio-source cache evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AudioSourceCacheDiagnostics {
    /// Resident decoded PCM windows.
    pub entries: usize,
    /// Bounded terminal decode failures retained for the current source identities.
    pub failures: usize,
    /// Resident decoded PCM payload bytes.
    pub reserved_bytes: usize,
    /// Configured global PCM payload budget.
    pub byte_budget: usize,
    /// Configured global entry capacity.
    pub entry_capacity: usize,
    /// Cache hits across all source readers.
    pub hits: u64,
    /// Cache misses across all source readers.
    pub misses: u64,
    /// Successfully decoded windows.
    pub decode_successes: u64,
    /// Failed window decodes.
    pub decode_failures: u64,
    /// Total wall time spent in concrete window decode Adapters.
    pub decode_total_duration_us: u64,
    /// Slowest concrete window decode Adapter call.
    pub decode_max_duration_us: u64,
    /// LRU evictions caused by entry or byte pressure.
    pub evictions: u64,
    /// Number of online residency-limit changes.
    pub budget_reconfigurations: u64,
    /// Limit changes that immediately removed at least one resident window.
    pub budget_trim_events: u64,
    /// Resident windows removed synchronously by online limit changes.
    pub budget_trimmed_entries: u64,
    /// PCM payload bytes released synchronously by online limit changes.
    pub budget_trimmed_bytes: u64,
    /// Decoded windows too large for the configured byte budget and therefore not retained.
    pub oversize_windows: u64,
    /// Distinct source windows currently owned by decode leaders.
    pub in_flight_decodes: usize,
    /// Peak simultaneous distinct source-window decodes.
    pub peak_in_flight_decodes: usize,
    /// Resident or admitted persistent decode-session slots.
    pub decoder_sessions: usize,
    /// Configured persistent decode-session slot capacity.
    pub decoder_session_capacity: usize,
    /// Peak resident or admitted persistent decode-session slots.
    pub decoder_peak_sessions: usize,
    /// Persistent decoder process opens.
    pub decoder_session_opens: u64,
    /// Windows supplied by an already-positioned sequential session.
    pub decoder_sequential_reuses: u64,
    /// Non-contiguous requests that restarted an existing source session.
    pub decoder_random_seek_restarts: u64,
    /// Sessions evicted by bounded decoder-pool pressure.
    pub decoder_session_evictions: u64,
    /// Number of online persistent-session capacity changes.
    pub decoder_capacity_reconfigurations: u64,
    /// Idle sessions terminated synchronously or on post-use convergence after
    /// an online capacity reduction.
    pub decoder_capacity_trim_evictions: u64,
    /// Busy sessions temporarily retained above the configured capacity.
    ///
    /// This converges to zero as those sessions finish their current decode.
    pub decoder_sessions_above_capacity: usize,
    /// Decode sessions terminated by generation cancellation.
    pub decoder_cancellations: u64,
    /// Slowest first window from a newly opened decode session.
    pub decoder_cold_window_max_duration_us: u64,
    /// Slowest window from an already-positioned sequential session.
    pub decoder_sequential_window_max_duration_us: u64,
    /// Slowest first window after a random-seek session restart.
    pub decoder_random_seek_window_max_duration_us: u64,
}

/// Consuming closure evidence for one decoded-audio source cache.
///
/// A clean receipt proves that the cache was not decoding while ownership was
/// consumed, retained PCM and terminal failures were released, every
/// persistent decoder child was reaped, and both pipe-pump threads were joined.
/// Any externally retained decoder/session or PCM `Arc` is reported and makes
/// [`Self::all_resources_released`] fail closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AudioSourceCacheShutdownEvidence {
    /// Evidence schema version.
    pub schema_version: u32,
    /// Decode leaders present before consuming cleanup began.
    pub in_flight_decodes_before: usize,
    /// Resident PCM windows before cleanup.
    pub pcm_entries_before: usize,
    /// Resident PCM bytes before cleanup.
    pub pcm_bytes_before: usize,
    /// Terminal decode failures before cleanup.
    pub failure_entries_before: usize,
    /// PCM window `Arc` references retained outside the cache at cleanup time.
    pub external_pcm_buffer_references: usize,
    /// PCM windows still retained by the cache after cleanup.
    pub pcm_entries_remaining: usize,
    /// PCM bytes still retained by the cache after cleanup.
    pub pcm_bytes_remaining: usize,
    /// Terminal failures still retained by the cache after cleanup.
    pub failure_entries_remaining: usize,
    /// Decoder Session slots present before explicit decoder shutdown.
    pub decoder_sessions_before: usize,
    /// Decoder Session slots whose resources could not be reclaimed.
    pub decoder_sessions_remaining: usize,
    /// Persistent decoder child processes encountered by lifetime cleanup.
    pub child_processes_observed: usize,
    /// Persistent decoder child processes synchronously reaped.
    pub child_processes_terminated: usize,
    /// Child-process kill, status, or wait failures retained by cleanup.
    pub child_process_termination_failures: usize,
    /// Decoder stdout pump threads encountered by lifetime cleanup.
    pub stdout_pump_threads_observed: usize,
    /// Decoder stdout pump threads synchronously joined without panic.
    pub stdout_pump_threads_joined: usize,
    /// Joined decoder stdout pump threads that panicked.
    pub stdout_pump_threads_panicked: usize,
    /// Opaque stdout-pump panic payload owners deliberately abandoned.
    pub stdout_pump_thread_owner_abandonments: usize,
    /// Decoder stderr pump threads encountered by lifetime cleanup.
    pub stderr_pump_threads_observed: usize,
    /// Decoder stderr pump threads synchronously joined without panic.
    pub stderr_pump_threads_joined: usize,
    /// Joined decoder stderr pump threads that panicked.
    pub stderr_pump_threads_panicked: usize,
    /// Opaque stderr-pump panic payload owners deliberately abandoned.
    pub stderr_pump_thread_owner_abandonments: usize,
    /// Session-slot `Arc` references retained outside decoder ownership.
    pub external_decoder_session_references: usize,
    /// Decoder child/thread/resource handles not proven reclaimed.
    pub decoder_resource_handles_remaining: usize,
    /// Early decoder shutdown workers successfully created during signal phase.
    pub decoder_shutdown_workers_started: u32,
    /// Early decoder shutdown workers synchronously joined by consuming cleanup.
    pub decoder_shutdown_workers_terminated: u32,
    /// Early decoder shutdown worker creation failures.
    pub decoder_shutdown_worker_start_failures: u32,
    /// Joined early decoder shutdown workers that panicked.
    pub decoder_shutdown_worker_panics: u32,
    /// Joined shutdown workers that returned without publishing resource evidence.
    pub decoder_shutdown_worker_publication_missing: u32,
    /// Foreign startup errors or opaque worker payloads deliberately abandoned.
    pub decoder_shutdown_worker_owner_abandonments: u32,
    /// Decoder-owner `Arc` references retained outside the consumed cache.
    pub external_decoder_references: usize,
    /// Deadline-bounded shutdown coordinators successfully created.
    pub shutdown_coordinators_started: u32,
    /// Deadline-bounded shutdown coordinators whose termination was observed
    /// and joined, including a joined panic or completion after the deadline.
    pub shutdown_coordinators_terminated: u32,
    /// Shutdown coordinator creation failures.
    pub shutdown_coordinator_start_failures: u32,
    /// Shutdown supervisors or their returned JoinHandles that panicked.
    pub shutdown_coordinator_panics: u32,
    /// Shutdown coordinators whose own completion timestamp was after the
    /// deadline, including a still-running coordinator detached at that seam.
    pub shutdown_coordinator_timeouts: u32,
    /// Shutdown coordinators detached after the shared deadline.
    pub shutdown_coordinator_detachments: u32,
    /// Shutdown coordinator spawners that panicked before returning a handle.
    pub shutdown_coordinator_spawner_panics: u32,
    /// Cache/decoder owners, foreign startup errors, or opaque panic payloads
    /// deliberately abandoned by coordinator lifetime handling.
    ///
    /// Abandonment keeps a potentially blocking foreign destructor off the
    /// qualification caller, but can never be accepted as clean closure.
    pub shutdown_coordinator_owner_abandonments: u32,
    /// Whether complete cache/decoder resource facts were available at the
    /// requested shutdown deadline.
    ///
    /// A coordinator that starts but completes late can eventually return
    /// detailed resource fields, but those fields were not authoritative at
    /// the deadline and this remains false.
    pub shutdown_resource_facts_complete_at_deadline: bool,
    /// Whether cache/decoder owner lifetime was still unresolved at the
    /// requested shutdown deadline.
    pub shutdown_owner_lifetime_unresolved_at_deadline: bool,
}

impl AudioSourceCacheShutdownEvidence {
    /// Whether every cache, child-process, pump-thread, and ownership fact closed exactly.
    pub const fn all_resources_released(self) -> bool {
        self.schema_version == 5
            && self.in_flight_decodes_before == 0
            && self.external_pcm_buffer_references == 0
            && self.pcm_entries_remaining == 0
            && self.pcm_bytes_remaining == 0
            && self.failure_entries_remaining == 0
            && self.external_decoder_references == 0
            && self.shutdown_coordinators_started == self.shutdown_coordinators_terminated
            && self.shutdown_coordinator_start_failures == 0
            && self.shutdown_coordinator_panics == 0
            && self.shutdown_coordinator_timeouts == 0
            && self.shutdown_coordinator_detachments == 0
            && self.shutdown_coordinator_spawner_panics == 0
            && self.shutdown_coordinator_owner_abandonments == 0
            && self.shutdown_resource_facts_complete_at_deadline
            && !self.shutdown_owner_lifetime_unresolved_at_deadline
            && AudioWindowDecoderShutdownEvidence {
                sessions_before: self.decoder_sessions_before,
                sessions_remaining: self.decoder_sessions_remaining,
                child_processes_observed: self.child_processes_observed,
                child_processes_terminated: self.child_processes_terminated,
                child_process_termination_failures: self.child_process_termination_failures,
                stdout_pump_threads_observed: self.stdout_pump_threads_observed,
                stdout_pump_threads_joined: self.stdout_pump_threads_joined,
                stdout_pump_threads_panicked: self.stdout_pump_threads_panicked,
                stdout_pump_thread_owner_abandonments: self.stdout_pump_thread_owner_abandonments,
                stderr_pump_threads_observed: self.stderr_pump_threads_observed,
                stderr_pump_threads_joined: self.stderr_pump_threads_joined,
                stderr_pump_threads_panicked: self.stderr_pump_threads_panicked,
                stderr_pump_thread_owner_abandonments: self.stderr_pump_thread_owner_abandonments,
                external_session_slot_references: self.external_decoder_session_references,
                resource_handles_remaining: self.decoder_resource_handles_remaining,
                shutdown_workers_started: self.decoder_shutdown_workers_started,
                shutdown_workers_terminated: self.decoder_shutdown_workers_terminated,
                shutdown_worker_start_failures: self.decoder_shutdown_worker_start_failures,
                shutdown_worker_panics: self.decoder_shutdown_worker_panics,
                shutdown_worker_publication_missing: self
                    .decoder_shutdown_worker_publication_missing,
                shutdown_worker_owner_abandonments: self.decoder_shutdown_worker_owner_abandonments,
            }
            .all_resources_released()
    }
}

#[derive(Clone, Copy)]
struct AudioSourceShutdownCoordinatorPublication {
    evidence: AudioSourceCacheShutdownEvidence,
    completed_at: Instant,
    logical_panic: bool,
}

#[derive(Default)]
struct AudioSourceShutdownCoordinatorState {
    publication: Option<AudioSourceShutdownCoordinatorPublication>,
}

struct AudioSourceShutdownCoordinatorResult;

impl AudioSourceCache {
    /// Create a conservatively sized product cache.
    ///
    /// The composition root applies the current machine/pressure decision
    /// online through [`Self::reconfigure`]. These initial limits are therefore
    /// a safe startup baseline, not a fixed product entitlement.
    pub fn new(sample_rate: u32) -> Self {
        Self::with_decoder(
            sample_rate,
            AUDIO_SOURCE_WINDOW_SECONDS,
            AUDIO_SOURCE_CACHE_ENTRY_CAPACITY,
            AUDIO_SOURCE_CACHE_BYTE_BUDGET,
            AUDIO_SOURCE_DECODER_SESSION_CAPACITY,
            Arc::new(PersistentFfmpegAudioWindowDecoder::with_capacity(
                AUDIO_SOURCE_DECODER_SESSION_CAPACITY,
            )),
        )
    }

    /// Build an inert, already-closed replacement for a consumed owner slot.
    ///
    /// This constructor starts no decoder worker and admits no reads. It is
    /// intended only for structures whose consuming shutdown API operates
    /// through `&mut self` and therefore needs a resource-free tombstone.
    pub fn shutdown_placeholder(sample_rate: u32) -> Self {
        let cache = Self::with_decoder(
            sample_rate,
            1,
            1,
            1,
            1,
            Arc::new(ShutdownAudioWindowDecoder),
        );
        cache.shutdown_requested.store(true, Ordering::Release);
        cache
    }

    /// Sample rate shared by every decoded window in this cache.
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Create an independently scheduled source cache with explicit hard limits.
    ///
    /// Derived-media services such as waveform analysis use this constructor so
    /// their sequential decode windows cannot consume the realtime playback or
    /// export cache budget. Limits are normalized to at least one window, entry,
    /// and byte; callers should expose the effective values through diagnostics.
    pub fn new_bounded(
        sample_rate: u32,
        window_seconds: usize,
        entry_capacity: usize,
        byte_budget: usize,
    ) -> Self {
        Self::new_bounded_with_sessions(
            sample_rate,
            window_seconds,
            entry_capacity,
            byte_budget,
            AUDIO_SOURCE_DECODER_SESSION_CAPACITY,
        )
    }

    /// Create an independently scheduled cache with explicit PCM and
    /// persistent-decoder limits.
    pub fn new_bounded_with_sessions(
        sample_rate: u32,
        window_seconds: usize,
        entry_capacity: usize,
        byte_budget: usize,
        decoder_session_capacity: usize,
    ) -> Self {
        Self::with_decoder(
            sample_rate,
            window_seconds,
            entry_capacity,
            byte_budget,
            decoder_session_capacity,
            Arc::new(PersistentFfmpegAudioWindowDecoder::with_capacity(
                decoder_session_capacity,
            )),
        )
    }

    fn with_decoder(
        sample_rate: u32,
        window_seconds: usize,
        entry_capacity: usize,
        byte_budget: usize,
        decoder_session_capacity: usize,
        decoder: Arc<dyn AudioWindowDecoder>,
    ) -> Self {
        let sample_rate = sample_rate.max(8_000);
        let config =
            AudioSourceCacheConfig::new(entry_capacity, byte_budget, decoder_session_capacity);
        decoder.reconfigure_session_capacity(config.decoder_session_capacity);
        let decoder_shutdown_signal = decoder.shutdown_signal();
        Self {
            sample_rate,
            window_frames: (sample_rate as usize).saturating_mul(window_seconds.max(1)),
            configuration: Mutex::new(()),
            state: Mutex::new(AudioSourceCacheState::new(config)),
            window_ready: Condvar::new(),
            decoder,
            decoder_shutdown_signal,
            shutdown_requested: AtomicBool::new(false),
        }
    }

    /// Apply new hard residency limits and synchronously trim the PCM LRU.
    ///
    /// In-flight decodes are never interrupted merely to reclaim cache
    /// residency. Their result observes the latest limits before publication.
    /// The decoder pool similarly terminates idle LRU sessions immediately and
    /// converges after any busy sessions finish.
    pub fn reconfigure(&self, config: AudioSourceCacheConfig) {
        let _configuration = self.configuration.lock();
        let config = AudioSourceCacheConfig::new(
            config.entry_capacity,
            config.byte_budget,
            config.decoder_session_capacity,
        );
        {
            let mut state = self.state.lock();
            if state.config.entry_capacity != config.entry_capacity
                || state.config.byte_budget != config.byte_budget
            {
                state.config.entry_capacity = config.entry_capacity;
                state.config.byte_budget = config.byte_budget;
                state.budget_reconfigurations = state.budget_reconfigurations.saturating_add(1);
                let (entries, bytes) = trim_pcm_entries_to_config(&mut state);
                if entries > 0 {
                    state.budget_trim_events = state.budget_trim_events.saturating_add(1);
                    state.budget_trimmed_entries =
                        state.budget_trimmed_entries.saturating_add(entries as u64);
                    state.budget_trimmed_bytes =
                        state.budget_trimmed_bytes.saturating_add(bytes as u64);
                    state.evictions = state.evictions.saturating_add(entries as u64);
                }
            }
            state.config.decoder_session_capacity = config.decoder_session_capacity;
        }
        self.decoder.reconfigure_session_capacity(config.decoder_session_capacity);
    }

    /// Open one source identity without decoding its complete duration.
    pub fn open(
        self: &Arc<Self>,
        path: &Path,
        selection: AudioSourceSelection,
    ) -> Result<AudioSourceReader> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: "audio source cache is shutting down".to_owned(),
            });
        }
        // Acquire the owner before the final admission check. If shutdown
        // races metadata capture, this temporary owner is dropped here and no
        // post-signal reader escapes to the caller.
        let cache = Arc::clone(self);
        let source = AudioSourceIdentity::capture(path, selection)?;
        if cache.shutdown_requested.load(Ordering::Acquire) {
            return Err(MondrianError::DecodeFailed {
                asset_id: path.display().to_string(),
                reason: "audio source cache is shutting down".to_owned(),
            });
        }
        Ok(AudioSourceReader { cache, source })
    }

    /// Capture bounded residency and execution evidence.
    pub fn diagnostics(&self) -> AudioSourceCacheDiagnostics {
        let _configuration = self.configuration.lock();
        let state = self.state.lock();
        let decoder = self.decoder.diagnostics();
        AudioSourceCacheDiagnostics {
            entries: state.entries.len(),
            failures: state.failures.len(),
            reserved_bytes: state.reserved_bytes,
            byte_budget: state.config.byte_budget,
            entry_capacity: state.config.entry_capacity,
            hits: state.hits,
            misses: state.misses,
            decode_successes: state.decode_successes,
            decode_failures: state.decode_failures,
            decode_total_duration_us: state.decode_total_duration_us,
            decode_max_duration_us: state.decode_max_duration_us,
            evictions: state.evictions,
            budget_reconfigurations: state.budget_reconfigurations,
            budget_trim_events: state.budget_trim_events,
            budget_trimmed_entries: state.budget_trimmed_entries,
            budget_trimmed_bytes: state.budget_trimmed_bytes,
            oversize_windows: state.oversize_windows,
            in_flight_decodes: state.in_flight.len(),
            peak_in_flight_decodes: state.peak_in_flight,
            decoder_sessions: decoder.sessions,
            decoder_session_capacity: decoder.session_capacity,
            decoder_peak_sessions: decoder.peak_sessions,
            decoder_session_opens: decoder.session_opens,
            decoder_sequential_reuses: decoder.sequential_reuses,
            decoder_random_seek_restarts: decoder.random_seek_restarts,
            decoder_session_evictions: decoder.session_evictions,
            decoder_capacity_reconfigurations: decoder.capacity_reconfigurations,
            decoder_capacity_trim_evictions: decoder.capacity_trim_evictions,
            decoder_sessions_above_capacity: decoder.sessions_above_capacity,
            decoder_cancellations: decoder.cancellations,
            decoder_cold_window_max_duration_us: decoder.cold_window_max_duration_us,
            decoder_sequential_window_max_duration_us: decoder.sequential_window_max_duration_us,
            decoder_random_seek_window_max_duration_us: decoder.random_seek_window_max_duration_us,
        }
    }

    /// Consume this cache and synchronously reclaim its retained decoder resources.
    ///
    /// Callers that share the cache through an `Arc` must first prove unique
    /// ownership with `Arc::try_unwrap`. This method does not wait for an active
    /// decode leader: observing one is a shutdown contract violation recorded
    /// in the returned fail-closed evidence.
    pub fn shutdown_and_wait(self) -> AudioSourceCacheShutdownEvidence {
        self.begin_shutdown();
        let external_decoder_references = Arc::strong_count(&self.decoder).saturating_sub(1);
        let (
            in_flight_decodes_before,
            pcm_entries_before,
            pcm_bytes_before,
            failure_entries_before,
            external_pcm_buffer_references,
            pcm_entries_remaining,
            pcm_bytes_remaining,
            failure_entries_remaining,
        ) = {
            let _configuration = self.configuration.lock();
            let mut state = self.state.lock();
            let in_flight_decodes_before = state.in_flight.len();
            let pcm_entries_before = state.entries.len();
            let pcm_bytes_before = state.reserved_bytes;
            let failure_entries_before = state.failures.len();
            let external_pcm_buffer_references = state
                .entries
                .iter()
                .map(|entry| Arc::strong_count(&entry.buffer).saturating_sub(1))
                .sum();
            state.entries.clear();
            state.failures.clear();
            state.reserved_bytes = 0;
            state.in_flight.clear();
            self.window_ready.notify_all();
            (
                in_flight_decodes_before,
                pcm_entries_before,
                pcm_bytes_before,
                failure_entries_before,
                external_pcm_buffer_references,
                state.entries.len(),
                state.reserved_bytes,
                state.failures.len(),
            )
        };
        let decoder = self.decoder.shutdown_sessions();
        AudioSourceCacheShutdownEvidence {
            schema_version: 5,
            in_flight_decodes_before,
            pcm_entries_before,
            pcm_bytes_before,
            failure_entries_before,
            external_pcm_buffer_references,
            pcm_entries_remaining,
            pcm_bytes_remaining,
            failure_entries_remaining,
            decoder_sessions_before: decoder.sessions_before,
            decoder_sessions_remaining: decoder.sessions_remaining,
            child_processes_observed: decoder.child_processes_observed,
            child_processes_terminated: decoder.child_processes_terminated,
            child_process_termination_failures: decoder.child_process_termination_failures,
            stdout_pump_threads_observed: decoder.stdout_pump_threads_observed,
            stdout_pump_threads_joined: decoder.stdout_pump_threads_joined,
            stdout_pump_threads_panicked: decoder.stdout_pump_threads_panicked,
            stdout_pump_thread_owner_abandonments: decoder.stdout_pump_thread_owner_abandonments,
            stderr_pump_threads_observed: decoder.stderr_pump_threads_observed,
            stderr_pump_threads_joined: decoder.stderr_pump_threads_joined,
            stderr_pump_threads_panicked: decoder.stderr_pump_threads_panicked,
            stderr_pump_thread_owner_abandonments: decoder.stderr_pump_thread_owner_abandonments,
            external_decoder_session_references: decoder.external_session_slot_references,
            decoder_resource_handles_remaining: decoder.resource_handles_remaining,
            decoder_shutdown_workers_started: decoder.shutdown_workers_started,
            decoder_shutdown_workers_terminated: decoder.shutdown_workers_terminated,
            decoder_shutdown_worker_start_failures: decoder.shutdown_worker_start_failures,
            decoder_shutdown_worker_panics: decoder.shutdown_worker_panics,
            decoder_shutdown_worker_publication_missing: decoder
                .shutdown_worker_publication_missing,
            decoder_shutdown_worker_owner_abandonments: decoder.shutdown_worker_owner_abandonments,
            external_decoder_references,
            shutdown_coordinators_started: 0,
            shutdown_coordinators_terminated: 0,
            shutdown_coordinator_start_failures: 0,
            shutdown_coordinator_panics: 0,
            shutdown_coordinator_timeouts: 0,
            shutdown_coordinator_detachments: 0,
            shutdown_coordinator_spawner_panics: 0,
            shutdown_coordinator_owner_abandonments: 0,
            shutdown_resource_facts_complete_at_deadline: true,
            shutdown_owner_lifetime_unresolved_at_deadline: false,
        }
    }

    /// Close cache admission and wake readers without waiting for decoder teardown.
    pub fn begin_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
        self.window_ready.notify_all();
        if let Some(signal) = &self.decoder_shutdown_signal {
            signal.request();
        }
    }

    /// Consume cache/decoder ownership through one absolute qualification deadline.
    pub fn shutdown_until(self, deadline: Instant) -> AudioSourceCacheShutdownEvidence {
        self.shutdown_until_with_spawner(deadline, |work| {
            thread::Builder::new()
                .name("mondrian-audio-source-endurance-shutdown".to_owned())
                .spawn(work)
        })
    }

    fn shutdown_until_with_spawner<F>(
        self,
        deadline: Instant,
        spawn: F,
    ) -> AudioSourceCacheShutdownEvidence
    where
        F: FnOnce(
            Box<dyn FnOnce() -> AudioSourceShutdownCoordinatorResult + Send>,
        )
            -> std::io::Result<thread::JoinHandle<AudioSourceShutdownCoordinatorResult>>,
    {
        self.begin_shutdown();
        let owner = Arc::new(Mutex::new(Some(self)));
        let coordinator_owner = Arc::clone(&owner);
        let coordinator_state =
            Arc::new(Mutex::new(AudioSourceShutdownCoordinatorState::default()));
        let worker_state = Arc::clone(&coordinator_state);
        let spawn_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            spawn(Box::new(move || {
                // Release the retained-owner slot before any decoder teardown.
                // A `match coordinator_owner.lock().take()` scrutinee keeps its
                // temporary guard alive through the selected arm, which would
                // deadlock the deadline observer while a destructor blocks.
                let retained_owner = { coordinator_owner.lock().take() };
                let (evidence, logical_panic) = match retained_owner {
                    Some(owner) => {
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            owner.shutdown_and_wait()
                        })) {
                            Ok(evidence) => (evidence, false),
                            Err(payload) => {
                                let owner_abandonments = u32::from(
                                    dispose_canonical_or_abandon_opaque_panic_payload(payload),
                                );
                                (
                                    AudioSourceCache::coordinator_failure(
                                        0,
                                        0,
                                        0,
                                        0,
                                        0,
                                        0,
                                        0,
                                        owner_abandonments,
                                    ),
                                    true,
                                )
                            }
                        }
                    }
                    None => (
                        AudioSourceCache::coordinator_failure(0, 0, 0, 0, 0, 0, 0, 0),
                        true,
                    ),
                };
                let publication = AudioSourceShutdownCoordinatorPublication {
                    evidence,
                    completed_at: Instant::now(),
                    logical_panic,
                };
                worker_state.lock().publication.get_or_insert(publication);
                AudioSourceShutdownCoordinatorResult
            }))
        }));
        let coordinator = match spawn_result {
            Ok(Ok(coordinator)) => coordinator,
            Ok(Err(error)) => {
                // Preserve bounded caller behavior even when the operating system
                // cannot create the coordinator. `io::Error::other` may own a
                // foreign blocking destructor, so retain only its stable kind.
                let (_, error_owner_abandoned) = abandon_io_error(error);
                let owner_abandonments = Self::abandon_retained_coordinator_owner(&owner)
                    .saturating_add(u32::from(error_owner_abandoned));
                return Self::coordinator_failure(0, 0, 1, 0, 0, 0, 0, owner_abandonments);
            }
            Err(payload) => {
                let payload_abandonments =
                    u32::from(dispose_canonical_or_abandon_opaque_panic_payload(payload));
                let owner_abandonments = payload_abandonments
                    .saturating_add(Self::abandon_retained_coordinator_owner(&owner));
                return Self::coordinator_failure(0, 0, 1, 0, 0, 0, 1, owner_abandonments);
            }
        };

        loop {
            // Completion wins at the deadline boundary. Once `is_finished`
            // is observable, joining is non-blocking and proves that the cache
            // owner and its decoder destructor already ran on the coordinator.
            if coordinator.is_finished() {
                break;
            }
            if Instant::now() >= deadline {
                if coordinator.is_finished() {
                    continue;
                }
                drop(coordinator);
                let publication = coordinator_state.lock().publication;
                let retained_owner = owner.lock().is_some();
                if retained_owner {
                    // An injected worker may have accepted but never invoked
                    // the supplied supervisor. Keep that owner off this
                    // deadline caller while preserving the shared slot for a
                    // worker that eventually starts.
                    std::mem::forget(owner);
                }
                let mut evidence = publication.map_or_else(
                    || Self::coordinator_failure(0, 0, 0, 0, 0, 0, 0, u32::from(retained_owner)),
                    |publication| publication.evidence,
                );
                evidence.shutdown_coordinators_started =
                    evidence.shutdown_coordinators_started.saturating_add(1);
                evidence.shutdown_coordinator_timeouts =
                    evidence.shutdown_coordinator_timeouts.saturating_add(1);
                evidence.shutdown_coordinator_detachments =
                    evidence.shutdown_coordinator_detachments.saturating_add(1);
                if publication.is_some_and(|publication| publication.logical_panic) {
                    evidence.shutdown_coordinator_panics =
                        evidence.shutdown_coordinator_panics.saturating_add(1);
                }
                evidence.shutdown_resource_facts_complete_at_deadline = false;
                evidence.shutdown_owner_lifetime_unresolved_at_deadline = true;
                return evidence;
            }
            thread::sleep(Duration::from_millis(1));
        }

        let (join_panicked, join_payload_abandonments) = match coordinator.join() {
            Ok(_) => (false, 0),
            Err(payload) => (
                true,
                u32::from(dispose_canonical_or_abandon_opaque_panic_payload(payload)),
            ),
        };
        let publication = coordinator_state.lock().publication;
        let retained_owner_abandonments = if publication.is_none() {
            Self::abandon_retained_coordinator_owner(&owner)
        } else {
            0
        };
        let mut evidence = publication.map_or_else(
            || Self::coordinator_failure(0, 0, 0, 0, 0, 0, 0, 0),
            |publication| publication.evidence,
        );
        evidence.shutdown_coordinators_started =
            evidence.shutdown_coordinators_started.saturating_add(1);
        evidence.shutdown_coordinators_terminated =
            evidence.shutdown_coordinators_terminated.saturating_add(1);
        if join_panicked || publication.is_some_and(|publication| publication.logical_panic) {
            evidence.shutdown_coordinator_panics =
                evidence.shutdown_coordinator_panics.saturating_add(1);
            evidence.shutdown_resource_facts_complete_at_deadline = false;
            evidence.shutdown_owner_lifetime_unresolved_at_deadline = true;
        }
        evidence.shutdown_coordinator_owner_abandonments = evidence
            .shutdown_coordinator_owner_abandonments
            .saturating_add(join_payload_abandonments)
            .saturating_add(retained_owner_abandonments);
        let missed_deadline = publication
            .map(|publication| publication.completed_at > deadline)
            .unwrap_or(true);
        if missed_deadline {
            evidence.shutdown_coordinator_timeouts =
                evidence.shutdown_coordinator_timeouts.saturating_add(1);
            evidence.shutdown_resource_facts_complete_at_deadline = false;
            evidence.shutdown_owner_lifetime_unresolved_at_deadline = true;
        }
        evidence
    }

    fn abandon_retained_coordinator_owner(owner: &Arc<Mutex<Option<Self>>>) -> u32 {
        owner.lock().take().map_or(0, |owner| {
            std::mem::forget(owner);
            1
        })
    }

    fn coordinator_failure(
        started: u32,
        terminated: u32,
        start_failures: u32,
        panics: u32,
        timeouts: u32,
        detachments: u32,
        spawner_panics: u32,
        owner_abandonments: u32,
    ) -> AudioSourceCacheShutdownEvidence {
        AudioSourceCacheShutdownEvidence {
            schema_version: 5,
            // The coordinator owns an unobservable cache/decoder lifetime.
            // Retain a conservative resource floor instead of claiming zero.
            decoder_resource_handles_remaining: 1,
            shutdown_coordinators_started: started,
            shutdown_coordinators_terminated: terminated,
            shutdown_coordinator_start_failures: start_failures,
            shutdown_coordinator_panics: panics,
            shutdown_coordinator_timeouts: timeouts,
            shutdown_coordinator_detachments: detachments,
            shutdown_coordinator_spawner_panics: spawner_panics,
            shutdown_coordinator_owner_abandonments: owner_abandonments,
            shutdown_resource_facts_complete_at_deadline: false,
            shutdown_owner_lifetime_unresolved_at_deadline: true,
            ..AudioSourceCacheShutdownEvidence::default()
        }
    }

    fn window(
        &self,
        key: AudioSourceWindowKey,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Arc<AudioBuffer>> {
        if self.shutdown_requested.load(Ordering::Acquire) {
            return Err(MondrianError::DecodeFailed {
                asset_id: key.source.path.display().to_string(),
                reason: "audio source cache is shutting down".to_owned(),
            });
        }
        if cancellation.is_canceled() {
            return Err(canceled_audio_decode(&key.source.path));
        }
        loop {
            let mut state = self.state.lock();
            if self.shutdown_requested.load(Ordering::Acquire) {
                return Err(MondrianError::DecodeFailed {
                    asset_id: key.source.path.display().to_string(),
                    reason: "audio source cache is shutting down".to_owned(),
                });
            }
            if let Some(index) = state.entries.iter().position(|entry| entry.key == key)
                && let Some(entry) = state.entries.remove(index)
            {
                let buffer = Arc::clone(&entry.buffer);
                state.entries.push_front(entry);
                state.hits = state.hits.saturating_add(1);
                return Ok(buffer);
            }
            if let Some(failure) = state.failures.iter().find(|failure| failure.key == key) {
                return Err(MondrianError::DecodeFailed {
                    asset_id: key.source.path.display().to_string(),
                    reason: failure.reason.clone(),
                });
            }
            if state.in_flight.iter().any(|in_flight| in_flight == &key) {
                self.window_ready.wait_for(&mut state, Duration::from_millis(5));
                drop(state);
                if self.shutdown_requested.load(Ordering::Acquire) {
                    return Err(MondrianError::DecodeFailed {
                        asset_id: key.source.path.display().to_string(),
                        reason: "audio source cache is shutting down".to_owned(),
                    });
                }
                if cancellation.is_canceled() {
                    return Err(canceled_audio_decode(&key.source.path));
                }
                continue;
            }
            state.misses = state.misses.saturating_add(1);
            state.in_flight.push(key.clone());
            state.peak_in_flight = state.peak_in_flight.max(state.in_flight.len());
            break;
        }

        let decode_started = Instant::now();
        let decoded = self
            .decoder
            .decode_window(
                &key.source,
                key.start_frame,
                self.window_frames,
                self.sample_rate,
                key.source.channel_layout,
                cancellation,
            )
            .and_then(|buffer| self.validate_window(&key, buffer));
        let decode_duration_us = decode_started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        let admission_rejected =
            decoded.as_ref().is_err_and(crate::FfmpegCommandError::is_cause_of);
        if cancellation.is_canceled() && !admission_rejected {
            self.finish_in_flight(&key);
            return Err(canceled_audio_decode(&key.source.path));
        }
        match decoded {
            Ok(buffer) => {
                let buffer = Arc::new(buffer);
                let bytes = buffer.samples.len().saturating_mul(std::mem::size_of::<f32>());
                let mut state = self.state.lock();
                state.decode_successes = state.decode_successes.saturating_add(1);
                state.decode_total_duration_us =
                    state.decode_total_duration_us.saturating_add(decode_duration_us);
                state.decode_max_duration_us = state.decode_max_duration_us.max(decode_duration_us);
                state.failures.retain(|failure| failure.key != key);
                while !state.entries.is_empty()
                    && (state.entries.len() >= state.config.entry_capacity
                        || state.reserved_bytes.saturating_add(bytes) > state.config.byte_budget)
                {
                    if let Some(evicted) = state.entries.pop_back() {
                        state.reserved_bytes = state.reserved_bytes.saturating_sub(evicted.bytes);
                        state.evictions = state.evictions.saturating_add(1);
                    }
                }
                if bytes <= state.config.byte_budget {
                    state.reserved_bytes = state.reserved_bytes.saturating_add(bytes);
                    state.in_flight.retain(|in_flight| in_flight != &key);
                    state.entries.push_front(AudioSourceWindowEntry {
                        key,
                        buffer: Arc::clone(&buffer),
                        bytes,
                    });
                } else {
                    state.oversize_windows = state.oversize_windows.saturating_add(1);
                    state.in_flight.retain(|in_flight| in_flight != &key);
                }
                self.window_ready.notify_all();
                Ok(buffer)
            }
            Err(error) => {
                let reason = error.to_string();
                let mut state = self.state.lock();
                state.decode_failures = state.decode_failures.saturating_add(1);
                state.decode_total_duration_us =
                    state.decode_total_duration_us.saturating_add(decode_duration_us);
                state.decode_max_duration_us = state.decode_max_duration_us.max(decode_duration_us);
                state.failures.retain(|failure| failure.key != key);
                state.in_flight.retain(|in_flight| in_flight != &key);
                // Admission belongs to the installed toolchain, not the media
                // window. Revalidate on the next request without converting
                // its typed source into a cached, recoverable DecodeFailed.
                if !crate::FfmpegCommandError::is_cause_of(&error) {
                    state.failures.push_front(AudioSourceFailureEntry { key, reason });
                }
                while state.failures.len() > AUDIO_SOURCE_FAILURE_CAPACITY {
                    state.failures.pop_back();
                }
                self.window_ready.notify_all();
                Err(error)
            }
        }
    }

    fn finish_in_flight(&self, key: &AudioSourceWindowKey) {
        self.state.lock().in_flight.retain(|in_flight| in_flight != key);
        self.window_ready.notify_all();
    }

    fn validate_window(
        &self,
        key: &AudioSourceWindowKey,
        buffer: AudioBuffer,
    ) -> Result<AudioBuffer> {
        let channels = key.source.channel_layout.channel_count();
        if buffer.sample_rate != self.sample_rate
            || buffer.channel_layout != key.source.channel_layout
            || !buffer.samples.len().is_multiple_of(channels)
            || buffer.frame_count() > self.window_frames
        {
            return Err(MondrianError::DecodeFailed {
                asset_id: key.source.path.display().to_string(),
                reason: format!(
                    "decoded audio window violated contract: rate={}/{} layout={:?}/{:?} frames={}/{}",
                    buffer.sample_rate,
                    self.sample_rate,
                    buffer.channel_layout,
                    key.source.channel_layout,
                    buffer.frame_count(),
                    self.window_frames,
                ),
            });
        }
        Ok(buffer)
    }
}

fn trim_pcm_entries_to_config(state: &mut AudioSourceCacheState) -> (usize, usize) {
    let mut entries = 0usize;
    let mut bytes = 0usize;
    while state.entries.len() > state.config.entry_capacity
        || state.reserved_bytes > state.config.byte_budget
    {
        let Some(evicted) = state.entries.pop_back() else {
            break;
        };
        state.reserved_bytes = state.reserved_bytes.saturating_sub(evicted.bytes);
        entries = entries.saturating_add(1);
        bytes = bytes.saturating_add(evicted.bytes);
    }
    (entries, bytes)
}

impl AudioSourceReader {
    /// Exact native semantic layout produced by this reader.
    pub const fn channel_layout(&self) -> AudioChannelLayout {
        self.source.channel_layout
    }

    /// Fill one exact interleaved output block from bounded decoded windows.
    ///
    /// Negative and post-EOF coordinates remain silence. The method never
    /// retains a whole source in process memory.
    pub fn read_interleaved(
        &self,
        start_frame: i64,
        frames: usize,
        destination: &mut [f32],
    ) -> Result<()> {
        self.read_interleaved_cancellable(
            start_frame,
            frames,
            destination,
            &ExecutionCancellationToken::new(),
        )
    }

    /// Fill one exact interleaved block with generation cancellation authority.
    pub fn read_interleaved_cancellable(
        &self,
        start_frame: i64,
        frames: usize,
        destination: &mut [f32],
        cancellation: &ExecutionCancellationToken,
    ) -> Result<()> {
        if cancellation.is_canceled() {
            return Err(canceled_audio_decode(&self.source.path));
        }
        let channels = self.source.channel_layout.channel_count();
        let expected_samples = frames.checked_mul(channels).ok_or_else(|| {
            MondrianError::Other(anyhow::anyhow!("audio source block extent overflow"))
        })?;
        if destination.len() != expected_samples {
            return Err(MondrianError::Other(anyhow::anyhow!(
                "audio source block does not match the opened channel contract"
            )));
        }
        destination.fill(0.0);
        if frames == 0 {
            return Ok(());
        }

        let leading_silence = if start_frame < 0 {
            usize::try_from(start_frame.saturating_abs()).unwrap_or(usize::MAX).min(frames)
        } else {
            0
        };
        let mut destination_frame = leading_silence;
        let mut source_frame = start_frame.saturating_add(leading_silence as i64).max(0);
        let window_frames_i64 = i64::try_from(self.cache.window_frames).unwrap_or(i64::MAX);

        while destination_frame < frames {
            let window_start =
                source_frame.div_euclid(window_frames_i64).saturating_mul(window_frames_i64);
            let key = AudioSourceWindowKey {
                source: self.source.clone(),
                start_frame: window_start,
            };
            let window = self.cache.window(key, cancellation)?;
            let local_frame =
                usize::try_from(source_frame.saturating_sub(window_start)).unwrap_or(usize::MAX);
            let available_frames = window.frame_count().saturating_sub(local_frame);
            if available_frames == 0 {
                break;
            }
            let copy_frames = available_frames.min(frames - destination_frame);
            let source_sample = local_frame.saturating_mul(channels);
            let destination_sample = destination_frame.saturating_mul(channels);
            let copy_samples = copy_frames.saturating_mul(channels);
            destination[destination_sample..destination_sample + copy_samples]
                .copy_from_slice(&window.samples[source_sample..source_sample + copy_samples]);
            destination_frame += copy_frames;
            source_frame = source_frame.saturating_add(copy_frames as i64);
            if window.frame_count() < self.cache.window_frames {
                break;
            }
        }
        Ok(())
    }
}

pub(super) fn canceled_audio_decode(path: &Path) -> MondrianError {
    MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: "audio render generation was canceled".to_owned(),
    }
}

pub(super) fn audio_frame_timestamp(frame: i64, sample_rate: u32) -> String {
    let frame = frame.max(0) as u128;
    let sample_rate = u128::from(sample_rate.max(1));
    let seconds = frame / sample_rate;
    let fractional_nanos = (frame % sample_rate).saturating_mul(1_000_000_000) / sample_rate;
    format!("{seconds}.{fractional_nanos:09}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "validation")]
    #[test]
    fn command_admission_failure_is_not_cached_as_a_recoverable_decode_error() {
        struct RejectedDecoder {
            calls: AtomicU64,
            cancel_before_reject: bool,
        }
        impl AudioWindowDecoder for RejectedDecoder {
            fn decode_window(
                &self,
                _source: &AudioSourceIdentity,
                _start_frame: i64,
                _frame_count: usize,
                _sample_rate: u32,
                _channel_layout: AudioChannelLayout,
                cancellation: &ExecutionCancellationToken,
            ) -> Result<AudioBuffer> {
                self.calls.fetch_add(1, Ordering::Relaxed);
                if self.cancel_before_reject {
                    cancellation.cancel();
                }
                Err(crate::FfmpegCommandError::from(
                    crate::QualifiedFfmpegToolchainError::CapsuleNamespaceChanged,
                )
                .into())
            }
        }
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        for cancel_before_reject in [false, true] {
            let decoder =
                Arc::new(RejectedDecoder { calls: AtomicU64::new(0), cancel_before_reject });
            let cache = Arc::new(AudioSourceCache::with_decoder(
                8_000,
                1,
                2,
                128 * 1024,
                1,
                decoder.clone(),
            ));
            let reader =
                cache.open(file.path(), stereo_selection(file.path(), 0)).expect("open source");
            for _ in 0..2 {
                let error = reader.read_interleaved(0, 2, &mut [0.0; 4]).expect_err("deny decode");
                assert!(crate::FfmpegCommandError::is_cause_of(&error));
            }
            assert_eq!(
                decoder.calls.load(Ordering::Relaxed),
                2,
                "each request revalidates authority"
            );
            let diagnostics = cache.diagnostics();
            assert_eq!(diagnostics.entries, 0);
            assert_eq!(
                diagnostics.failures, 0,
                "no string-only failure cache entry"
            );
            assert_eq!(diagnostics.decode_failures, 2);
            assert!(cache.state.lock().in_flight.is_empty());
        }
    }
    use crate::audio::decode_audio_file_with_ffmpeg_cli;
    use crate::info::ChannelLayout;
    use crate::MediaFileFingerprint;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    fn stereo_selection(path: &Path, stream_index: u32) -> AudioSourceSelection {
        AudioSourceSelection::new(
            stream_index,
            ChannelLayout::Exact(AudioChannelLayout::Stereo),
            MediaFileFingerprint::capture(path),
        )
    }

    struct RampWindowDecoder {
        calls: AtomicU64,
    }

    impl RampWindowDecoder {
        fn new() -> Self {
            Self { calls: AtomicU64::new(0) }
        }
    }

    impl AudioWindowDecoder for RampWindowDecoder {
        fn decode_window(
            &self,
            source: &AudioSourceIdentity,
            start_frame: i64,
            frame_count: usize,
            sample_rate: u32,
            channel_layout: AudioChannelLayout,
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioBuffer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let channels_usize = channel_layout.channel_count();
            let stream_offset = source.selection.stream_index() as f32 * 1_000_000.0;
            let mut samples = vec![0.0; frame_count * channels_usize];
            for frame in 0..frame_count {
                for channel in 0..channels_usize {
                    samples[frame * channels_usize + channel] =
                        stream_offset + (start_frame + frame as i64) as f32 * 10.0 + channel as f32;
                }
            }
            Ok(AudioBuffer { samples, sample_rate, channel_layout })
        }
    }

    struct MalformedWindowDecoder {
        calls: AtomicU64,
    }

    struct BlockingWindowDecoder {
        entered: AtomicBool,
    }

    struct SingleFlightWindowDecoder {
        calls: AtomicU64,
        entered: AtomicBool,
        release: AtomicBool,
    }

    struct ResidualWindowDecoder;

    struct BlockingDropWindowDecoder {
        drop_started: Arc<AtomicBool>,
        drop_finished: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }

    #[derive(Debug)]
    struct DropTrackingPayload {
        dropped: Arc<AtomicBool>,
    }

    impl std::fmt::Display for DropTrackingPayload {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("drop-tracking payload")
        }
    }

    impl std::error::Error for DropTrackingPayload {}

    impl Drop for DropTrackingPayload {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    struct AbandonedPumpPayloadWindowDecoder;

    impl AudioWindowDecoder for MalformedWindowDecoder {
        fn decode_window(
            &self,
            _source: &AudioSourceIdentity,
            _start_frame: i64,
            frame_count: usize,
            sample_rate: u32,
            _channel_layout: AudioChannelLayout,
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioBuffer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(AudioBuffer {
                samples: vec![0.0; frame_count],
                sample_rate,
                channel_layout: AudioChannelLayout::Mono,
            })
        }
    }

    impl AudioWindowDecoder for BlockingWindowDecoder {
        fn decode_window(
            &self,
            source: &AudioSourceIdentity,
            _start_frame: i64,
            _frame_count: usize,
            _sample_rate: u32,
            _channel_layout: AudioChannelLayout,
            cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioBuffer> {
            self.entered.store(true, Ordering::Release);
            while !cancellation.is_canceled() {
                std::thread::yield_now();
            }
            Err(canceled_audio_decode(&source.path))
        }
    }

    impl AudioWindowDecoder for SingleFlightWindowDecoder {
        fn decode_window(
            &self,
            _source: &AudioSourceIdentity,
            _start_frame: i64,
            frame_count: usize,
            sample_rate: u32,
            channel_layout: AudioChannelLayout,
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioBuffer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.entered.store(true, Ordering::Release);
            while !self.release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            Ok(AudioBuffer {
                samples: vec![0.0; frame_count * channel_layout.channel_count()],
                sample_rate,
                channel_layout,
            })
        }
    }

    impl AudioWindowDecoder for ResidualWindowDecoder {
        fn decode_window(
            &self,
            _source: &AudioSourceIdentity,
            _start_frame: i64,
            _frame_count: usize,
            _sample_rate: u32,
            _channel_layout: AudioChannelLayout,
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioBuffer> {
            Err(MondrianError::Other(anyhow::anyhow!(
                "residual decoder must not decode"
            )))
        }

        fn diagnostics(&self) -> AudioWindowDecoderDiagnostics {
            AudioWindowDecoderDiagnostics {
                sessions: 1,
                ..AudioWindowDecoderDiagnostics::default()
            }
        }
    }

    impl AudioWindowDecoder for BlockingDropWindowDecoder {
        fn decode_window(
            &self,
            _source: &AudioSourceIdentity,
            _start_frame: i64,
            _frame_count: usize,
            _sample_rate: u32,
            _channel_layout: AudioChannelLayout,
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioBuffer> {
            Err(MondrianError::Other(anyhow::anyhow!(
                "blocking-drop decoder must not decode"
            )))
        }
    }

    impl AudioWindowDecoder for AbandonedPumpPayloadWindowDecoder {
        fn decode_window(
            &self,
            _source: &AudioSourceIdentity,
            _start_frame: i64,
            _frame_count: usize,
            _sample_rate: u32,
            _channel_layout: AudioChannelLayout,
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioBuffer> {
            Err(MondrianError::Other(anyhow::anyhow!(
                "pump-payload decoder must not decode"
            )))
        }

        fn shutdown_sessions(&self) -> AudioWindowDecoderShutdownEvidence {
            AudioWindowDecoderShutdownEvidence {
                stdout_pump_threads_observed: 1,
                stdout_pump_threads_panicked: 1,
                stdout_pump_thread_owner_abandonments: 1,
                ..AudioWindowDecoderShutdownEvidence::default()
            }
        }
    }

    impl Drop for BlockingDropWindowDecoder {
        fn drop(&mut self) {
            self.drop_started.store(true, Ordering::Release);
            while !self.release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            self.drop_finished.store(true, Ordering::Release);
        }
    }

    fn test_audio_source(
        decoder: Arc<RampWindowDecoder>,
        entry_capacity: usize,
        byte_budget: usize,
    ) -> (
        tempfile::NamedTempFile,
        Arc<AudioSourceCache>,
        AudioSourceReader,
    ) {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        let cache = Arc::new(AudioSourceCache::with_decoder(
            8_000,
            1,
            entry_capacity,
            byte_budget,
            1,
            decoder,
        ));
        let reader =
            cache.open(file.path(), stereo_selection(file.path(), 0)).expect("open source");
        (file, cache, reader)
    }

    #[test]
    fn consuming_shutdown_of_never_opened_cache_proves_complete_release() {
        assert!(!AudioSourceCacheShutdownEvidence::default().all_resources_released());
        let evidence = AudioSourceCache::new(48_000).shutdown_and_wait();

        assert_eq!(evidence.schema_version, 5);
        assert_eq!(
            evidence,
            AudioSourceCacheShutdownEvidence {
                schema_version: 5,
                decoder_shutdown_workers_started: 1,
                decoder_shutdown_workers_terminated: 1,
                shutdown_resource_facts_complete_at_deadline: true,
                shutdown_owner_lifetime_unresolved_at_deadline: false,
                ..AudioSourceCacheShutdownEvidence::default()
            }
        );
        assert!(evidence.all_resources_released());

        let stale = AudioSourceCacheShutdownEvidence { schema_version: 4, ..evidence };
        assert!(!stale.all_resources_released());
    }

    #[test]
    fn shutdown_placeholder_owns_no_decoder_worker() {
        let evidence = AudioSourceCache::shutdown_placeholder(48_000).shutdown_and_wait();

        assert_eq!(evidence.schema_version, 5);
        assert_eq!(evidence.decoder_shutdown_workers_started, 0);
        assert_eq!(evidence.decoder_shutdown_workers_terminated, 0);
        assert!(evidence.all_resources_released());
    }

    #[test]
    fn decoder_session_capacity_is_normalized_to_the_physical_owner_limit() {
        assert_eq!(
            AudioSourceCacheConfig::new(1, 1, 0).decoder_session_capacity,
            1
        );
        assert_eq!(
            AudioSourceCacheConfig::new(1, 1, AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX + 1,)
                .decoder_session_capacity,
            AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX
        );
    }

    #[test]
    fn begin_shutdown_rejects_open_before_filesystem_admission() {
        let cache = Arc::new(AudioSourceCache::new(48_000));
        cache.begin_shutdown();
        let owners_before = Arc::strong_count(&cache);
        let missing = std::path::Path::new("definitely-missing-after-audio-source-shutdown.wav");
        let selection = AudioSourceSelection::new(
            0,
            ChannelLayout::Exact(AudioChannelLayout::Stereo),
            MediaFileFingerprint::default(),
        );

        let error = cache.open(missing, selection).err().expect("closed admission");

        assert!(error.to_string().contains("shutting down"));
        assert_eq!(Arc::strong_count(&cache), owners_before);
    }

    #[test]
    fn consuming_shutdown_propagates_opaque_pump_payload_abandonment() {
        let cache = AudioSourceCache::with_decoder(
            48_000,
            1,
            1,
            1,
            1,
            Arc::new(AbandonedPumpPayloadWindowDecoder),
        );

        let evidence = cache.shutdown_and_wait();

        assert_eq!(evidence.stdout_pump_threads_observed, 1);
        assert_eq!(evidence.stdout_pump_threads_panicked, 1);
        assert_eq!(evidence.stdout_pump_thread_owner_abandonments, 1);
        assert_eq!(evidence.stderr_pump_thread_owner_abandonments, 0);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn deadline_consuming_shutdown_of_clean_cache_returns_exact_receipt() {
        let evidence =
            AudioSourceCache::new(48_000).shutdown_until(Instant::now() + Duration::from_secs(2));

        assert_eq!(evidence.schema_version, 5);
        assert_eq!(evidence.shutdown_coordinators_started, 1);
        assert_eq!(evidence.shutdown_coordinators_terminated, 1);
        assert_eq!(evidence.shutdown_coordinator_start_failures, 0);
        assert_eq!(evidence.shutdown_coordinator_panics, 0);
        assert_eq!(evidence.shutdown_coordinator_timeouts, 0);
        assert_eq!(evidence.shutdown_coordinator_detachments, 0);
        assert_eq!(evidence.shutdown_coordinator_owner_abandonments, 0);
        assert!(evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(!evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(evidence.all_resources_released());
    }

    #[test]
    fn joined_completion_after_absolute_deadline_fails_closed_as_late() {
        let deadline = Instant::now() - Duration::from_secs(1);
        let evidence =
            AudioSourceCache::new(48_000).shutdown_until_with_spawner(deadline, |work| {
                // Run the clean owner consumption before returning the handle,
                // then wait until the result carrier is observably complete.
                // An implementation that only checks `is_finished` would
                // incorrectly turn this deliberately late completion clean.
                let result = work();
                let carrier = std::thread::spawn(move || result);
                while !carrier.is_finished() {
                    std::thread::yield_now();
                }
                Ok(carrier)
            });

        assert_eq!(evidence.shutdown_coordinators_started, 1);
        assert_eq!(evidence.shutdown_coordinators_terminated, 1);
        assert_eq!(evidence.shutdown_coordinator_timeouts, 1);
        assert_eq!(evidence.shutdown_coordinator_detachments, 0);
        assert_eq!(evidence.shutdown_coordinator_owner_abandonments, 0);
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn joined_late_coordinator_panic_records_timeout_and_termination() {
        let evidence = AudioSourceCache::new(48_000).shutdown_until_with_spawner(
            Instant::now() - Duration::from_secs(1),
            |work| {
                let carrier =
                    std::thread::spawn(move || -> AudioSourceShutdownCoordinatorResult {
                        let _completed = work();
                        panic!("synthetic audio-source coordinator panic after owner consumption");
                    });
                while !carrier.is_finished() {
                    std::thread::yield_now();
                }
                Ok(carrier)
            },
        );

        assert_eq!(evidence.shutdown_coordinators_started, 1);
        assert_eq!(evidence.shutdown_coordinators_terminated, 1);
        assert_eq!(evidence.shutdown_coordinator_panics, 1);
        assert_eq!(evidence.shutdown_coordinator_timeouts, 1);
        assert_eq!(evidence.shutdown_coordinator_detachments, 0);
        assert_eq!(evidence.shutdown_coordinator_owner_abandonments, 0);
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn joined_opaque_coordinator_panic_abandons_payload_owner() {
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let worker_payload_dropped = Arc::clone(&payload_dropped);
        let evidence = AudioSourceCache::new(48_000).shutdown_until_with_spawner(
            Instant::now() + Duration::from_secs(2),
            |work| {
                let carrier =
                    std::thread::spawn(move || -> AudioSourceShutdownCoordinatorResult {
                        let _completed = work();
                        std::panic::panic_any(DropTrackingPayload {
                            dropped: worker_payload_dropped,
                        });
                    });
                while !carrier.is_finished() {
                    std::thread::yield_now();
                }
                Ok(carrier)
            },
        );

        assert_eq!(evidence.shutdown_coordinators_started, 1);
        assert_eq!(evidence.shutdown_coordinators_terminated, 1);
        assert_eq!(evidence.shutdown_coordinator_panics, 1);
        assert_eq!(evidence.shutdown_coordinator_timeouts, 0);
        assert_eq!(evidence.shutdown_coordinator_owner_abandonments, 1);
        assert!(!payload_dropped.load(Ordering::Acquire));
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn injected_join_handle_without_supervisor_stamp_fails_closed() {
        let evidence = AudioSourceCache::new(48_000).shutdown_until_with_spawner(
            Instant::now() + Duration::from_secs(2),
            |_work| {
                let carrier = std::thread::spawn(|| AudioSourceShutdownCoordinatorResult);
                while !carrier.is_finished() {
                    std::thread::yield_now();
                }
                Ok(carrier)
            },
        );

        assert_eq!(evidence.shutdown_coordinators_started, 1);
        assert_eq!(evidence.shutdown_coordinators_terminated, 1);
        assert_eq!(evidence.shutdown_coordinator_panics, 0);
        assert_eq!(evidence.shutdown_coordinator_timeouts, 1);
        assert_eq!(evidence.shutdown_coordinator_detachments, 0);
        assert_eq!(evidence.shutdown_coordinator_owner_abandonments, 1);
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn canonical_spawner_panic_abandons_only_retained_cache_owner() {
        let evidence = AudioSourceCache::new(48_000).shutdown_until_with_spawner(
            Instant::now() + Duration::from_secs(2),
            |_work| -> std::io::Result<thread::JoinHandle<AudioSourceShutdownCoordinatorResult>> {
                panic!("synthetic canonical audio-source spawner panic")
            },
        );

        assert_eq!(evidence.shutdown_coordinators_started, 0);
        assert_eq!(evidence.shutdown_coordinator_start_failures, 1);
        assert_eq!(evidence.shutdown_coordinator_spawner_panics, 1);
        assert_eq!(evidence.shutdown_coordinator_owner_abandonments, 1);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn opaque_spawner_panic_abandons_payload_and_retained_cache_owners() {
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let panic_payload_dropped = Arc::clone(&payload_dropped);
        let evidence =
            AudioSourceCache::new(48_000).shutdown_until_with_spawner(
                Instant::now() + Duration::from_secs(2),
                move |_work| -> std::io::Result<
                    thread::JoinHandle<AudioSourceShutdownCoordinatorResult>,
                > {
                    std::panic::panic_any(DropTrackingPayload { dropped: panic_payload_dropped })
                },
            );

        assert_eq!(evidence.shutdown_coordinators_started, 0);
        assert_eq!(evidence.shutdown_coordinator_start_failures, 1);
        assert_eq!(evidence.shutdown_coordinator_spawner_panics, 1);
        assert_eq!(evidence.shutdown_coordinator_owner_abandonments, 2);
        assert!(!payload_dropped.load(Ordering::Acquire));
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn deadline_consuming_shutdown_classifies_timeout_and_detaches_owner() {
        let drop_started = Arc::new(AtomicBool::new(false));
        let drop_finished = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let cache = AudioSourceCache::with_decoder(
            48_000,
            1,
            1,
            1,
            1,
            Arc::new(BlockingDropWindowDecoder {
                drop_started: Arc::clone(&drop_started),
                drop_finished: Arc::clone(&drop_finished),
                release: Arc::clone(&release),
            }),
        );

        let observed_drop_started = Arc::clone(&drop_started);
        let evidence = cache.shutdown_until_with_spawner(
            Instant::now() + Duration::from_millis(20),
            move |work| {
                let coordinator = std::thread::spawn(work);
                let started_deadline = Instant::now() + Duration::from_secs(2);
                while !observed_drop_started.load(Ordering::Acquire)
                    && Instant::now() < started_deadline
                {
                    std::thread::yield_now();
                }
                Ok(coordinator)
            },
        );

        let observed_start = drop_started.load(Ordering::Acquire);
        let completed_before_release = drop_finished.load(Ordering::Acquire);
        release.store(true, Ordering::Release);
        let finished_deadline = Instant::now() + Duration::from_secs(2);
        while !drop_finished.load(Ordering::Acquire) && Instant::now() < finished_deadline {
            std::thread::yield_now();
        }

        assert!(observed_start);
        assert!(!completed_before_release);
        assert!(drop_finished.load(Ordering::Acquire));
        assert_eq!(evidence.shutdown_coordinators_started, 1);
        assert_eq!(evidence.shutdown_coordinators_terminated, 0);
        assert_eq!(evidence.shutdown_coordinator_timeouts, 1);
        assert_eq!(evidence.shutdown_coordinator_detachments, 1);
        assert_eq!(evidence.shutdown_coordinator_owner_abandonments, 0);
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn coordinator_spawn_failure_abandons_owner_without_running_drop_on_caller() {
        let drop_started = Arc::new(AtomicBool::new(false));
        let drop_finished = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let error_payload_dropped = Arc::new(AtomicBool::new(false));
        let spawn_error_payload_dropped = Arc::clone(&error_payload_dropped);
        let cache = AudioSourceCache::with_decoder(
            48_000,
            1,
            1,
            1,
            1,
            Arc::new(BlockingDropWindowDecoder {
                drop_started: Arc::clone(&drop_started),
                drop_finished: Arc::clone(&drop_finished),
                release: Arc::clone(&release),
            }),
        );

        let evidence =
            cache.shutdown_until_with_spawner(Instant::now() + Duration::from_secs(2), |_work| {
                Err(std::io::Error::other(DropTrackingPayload {
                    dropped: spawn_error_payload_dropped,
                }))
            });

        assert_eq!(evidence.shutdown_coordinators_started, 0);
        assert_eq!(evidence.shutdown_coordinators_terminated, 0);
        assert_eq!(evidence.shutdown_coordinator_start_failures, 1);
        assert_eq!(evidence.shutdown_coordinator_timeouts, 0);
        assert_eq!(evidence.shutdown_coordinator_detachments, 0);
        assert_eq!(evidence.shutdown_coordinator_owner_abandonments, 2);
        assert_eq!(evidence.decoder_resource_handles_remaining, 1);
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!drop_started.load(Ordering::Acquire));
        assert!(!drop_finished.load(Ordering::Acquire));
        assert!(!error_payload_dropped.load(Ordering::Acquire));
        assert!(!evidence.all_resources_released());
        release.store(true, Ordering::Release);
    }

    #[test]
    fn consuming_shutdown_clears_pcm_residency() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        let cache = Arc::new(AudioSourceCache::with_decoder(
            8_000,
            1,
            2,
            128 * 1024,
            1,
            Arc::new(RampWindowDecoder::new()),
        ));
        let reader =
            cache.open(file.path(), stereo_selection(file.path(), 0)).expect("open source");
        let mut destination = [0.0; 2];
        reader.read_interleaved(0, 1, &mut destination).expect("decode resident window");
        drop(reader);
        let cache = Arc::try_unwrap(cache).ok().expect("unique cache owner");

        let evidence = cache.shutdown_and_wait();

        assert_eq!(evidence.pcm_entries_before, 1);
        assert!(evidence.pcm_bytes_before > 0);
        assert_eq!(evidence.pcm_entries_remaining, 0);
        assert_eq!(evidence.pcm_bytes_remaining, 0);
        assert!(evidence.all_resources_released());
    }

    #[test]
    fn consuming_shutdown_clears_terminal_failure_residency() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        let cache = Arc::new(AudioSourceCache::with_decoder(
            8_000,
            1,
            2,
            128 * 1024,
            1,
            Arc::new(MalformedWindowDecoder { calls: AtomicU64::new(0) }),
        ));
        let reader =
            cache.open(file.path(), stereo_selection(file.path(), 0)).expect("open source");
        let mut destination = [0.0; 4];
        reader
            .read_interleaved(0, 2, &mut destination)
            .expect_err("malformed decoder result fails closed");
        drop(reader);
        let cache = Arc::try_unwrap(cache).ok().expect("unique cache owner");

        let evidence = cache.shutdown_and_wait();

        assert_eq!(evidence.failure_entries_before, 1);
        assert_eq!(evidence.failure_entries_remaining, 0);
        assert!(evidence.all_resources_released());
    }

    #[test]
    fn default_decoder_shutdown_cannot_claim_unreleased_sessions() {
        let cache =
            AudioSourceCache::with_decoder(48_000, 1, 1, 1, 1, Arc::new(ResidualWindowDecoder));

        let evidence = cache.shutdown_and_wait();

        assert_eq!(evidence.decoder_sessions_before, 1);
        assert_eq!(evidence.decoder_sessions_remaining, 1);
        assert_eq!(evidence.decoder_resource_handles_remaining, 1);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn consuming_shutdown_rejects_an_externally_retained_decoder_owner() {
        let decoder = Arc::new(RampWindowDecoder::new());
        let cache = AudioSourceCache::with_decoder(48_000, 1, 1, 1, 1, decoder.clone());

        let evidence = cache.shutdown_and_wait();

        assert_eq!(evidence.external_decoder_references, 1);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn consuming_shutdown_records_in_flight_decode_state_fail_closed() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        let cache = Arc::new(AudioSourceCache::with_decoder(
            48_000,
            1,
            1,
            1,
            1,
            Arc::new(RampWindowDecoder::new()),
        ));
        let reader =
            cache.open(file.path(), stereo_selection(file.path(), 0)).expect("open source");
        cache
            .state
            .lock()
            .in_flight
            .push(AudioSourceWindowKey { source: reader.source.clone(), start_frame: 0 });
        drop(reader);
        let cache = Arc::try_unwrap(cache).ok().expect("unique cache owner");

        let evidence = cache.shutdown_and_wait();

        assert_eq!(evidence.in_flight_decodes_before, 1);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn bounded_audio_source_reads_exactly_across_aligned_windows() {
        let decoder = Arc::new(RampWindowDecoder::new());
        let (_file, cache, reader) = test_audio_source(
            Arc::clone(&decoder),
            4,
            4 * 8_000 * 2 * std::mem::size_of::<f32>(),
        );
        let mut destination = vec![0.0; 8];

        reader.read_interleaved(7_998, 4, &mut destination).expect("cross-window read");

        assert_eq!(
            destination,
            vec![79_980.0, 79_981.0, 79_990.0, 79_991.0, 80_000.0, 80_001.0, 80_010.0, 80_011.0]
        );
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.diagnostics().entries, 2);
    }

    #[test]
    fn physical_stream_selection_is_part_of_cache_identity() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        let decoder = Arc::new(RampWindowDecoder::new());
        let cache = Arc::new(AudioSourceCache::with_decoder(
            8_000,
            1,
            4,
            4 * 8_000 * 2 * std::mem::size_of::<f32>(),
            1,
            decoder.clone(),
        ));
        let first = cache.open(file.path(), stereo_selection(file.path(), 1)).expect("stream one");
        let second =
            cache.open(file.path(), stereo_selection(file.path(), 3)).expect("stream three");
        let mut first_samples = [0.0; 2];
        let mut second_samples = [0.0; 2];

        first.read_interleaved(0, 1, &mut first_samples).expect("first stream read");
        second.read_interleaved(0, 1, &mut second_samples).expect("second stream read");

        assert_eq!(first_samples, [1_000_000.0, 1_000_001.0]);
        assert_eq!(second_samples, [3_000_000.0, 3_000_001.0]);
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.diagnostics().entries, 2);
    }

    #[test]
    fn source_window_propagates_generation_cancellation_without_cache_admission() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source bytes");
        file.flush().expect("flush source");
        let decoder = Arc::new(BlockingWindowDecoder { entered: AtomicBool::new(false) });
        let cache = Arc::new(AudioSourceCache::with_decoder(
            48_000,
            1,
            2,
            1_000_000,
            1,
            decoder.clone(),
        ));
        let reader =
            cache.open(file.path(), stereo_selection(file.path(), 0)).expect("open source");
        let cancellation = ExecutionCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker = std::thread::spawn(move || {
            let mut destination = vec![0.0; 2_048 * 2];
            reader.read_interleaved_cancellable(0, 2_048, &mut destination, &worker_cancellation)
        });
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while Instant::now() < deadline && !decoder.entered.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(decoder.entered.load(Ordering::Acquire));
        cancellation.cancel();
        let error = worker.join().expect("worker returns").expect_err("canceled source fails");
        assert!(error.to_string().contains("canceled"));
        assert_eq!(
            cache.diagnostics(),
            AudioSourceCacheDiagnostics {
                byte_budget: 1_000_000,
                entry_capacity: 2,
                misses: 1,
                peak_in_flight_decodes: 1,
                ..AudioSourceCacheDiagnostics::default()
            }
        );
    }

    #[test]
    fn concurrent_same_window_miss_has_one_decode_leader() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source bytes");
        file.flush().expect("flush source");
        let decoder = Arc::new(SingleFlightWindowDecoder {
            calls: AtomicU64::new(0),
            entered: AtomicBool::new(false),
            release: AtomicBool::new(false),
        });
        let cache = Arc::new(AudioSourceCache::with_decoder(
            48_000,
            1,
            2,
            1_000_000,
            1,
            decoder.clone(),
        ));
        let first_reader = cache
            .open(file.path(), stereo_selection(file.path(), 0))
            .expect("open first reader");
        let second_reader = first_reader.clone();
        let first = std::thread::spawn(move || {
            let mut destination = vec![0.0; 2_048 * 2];
            first_reader.read_interleaved(0, 2_048, &mut destination)
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !decoder.entered.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(decoder.entered.load(Ordering::Acquire));
        let second = std::thread::spawn(move || {
            let mut destination = vec![0.0; 2_048 * 2];
            second_reader.read_interleaved(0, 2_048, &mut destination)
        });
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 1);
        decoder.release.store(true, Ordering::Release);
        first.join().expect("first reader returns").expect("first read");
        second.join().expect("second reader returns").expect("second read");

        let diagnostics = cache.diagnostics();
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 1);
        assert_eq!(diagnostics.misses, 1);
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.in_flight_decodes, 0);
        assert_eq!(diagnostics.peak_in_flight_decodes, 1);
    }

    #[test]
    fn bounded_audio_source_preserves_negative_silence_and_reuses_seek_window() {
        let decoder = Arc::new(RampWindowDecoder::new());
        let (_file, cache, reader) = test_audio_source(
            Arc::clone(&decoder),
            2,
            2 * 8_000 * 2 * std::mem::size_of::<f32>(),
        );
        let mut destination = vec![1.0; 8];

        reader.read_interleaved(-2, 4, &mut destination).expect("negative source read");
        reader
            .read_interleaved(128, 2, &mut destination[..4])
            .expect("same-window seek");

        assert_eq!(&destination[4..], &[0.0, 1.0, 10.0, 11.0]);
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 1);
        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.hits, 1);
        assert_eq!(diagnostics.misses, 1);
    }

    #[test]
    fn bounded_audio_source_evicts_by_global_pcm_byte_budget() {
        let decoder = Arc::new(RampWindowDecoder::new());
        let window_bytes = 8_000 * 2 * std::mem::size_of::<f32>();
        let (_file, cache, reader) = test_audio_source(Arc::clone(&decoder), 8, window_bytes);
        let mut destination = vec![0.0; 2];

        reader.read_interleaved(0, 1, &mut destination).expect("first window");
        reader.read_interleaved(8_000, 1, &mut destination).expect("second window");
        reader.read_interleaved(0, 1, &mut destination).expect("evicted window reload");

        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert_eq!(diagnostics.reserved_bytes, window_bytes);
        assert_eq!(diagnostics.evictions, 2);
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn online_budget_reduction_synchronously_trims_the_true_pcm_lru() {
        let decoder = Arc::new(RampWindowDecoder::new());
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        let window_frames = 48_000 * 10;
        let window_bytes = window_frames * 2 * std::mem::size_of::<f32>();
        let cache = Arc::new(AudioSourceCache::with_decoder(
            48_000,
            10,
            8,
            4 * window_bytes,
            1,
            decoder.clone(),
        ));
        let reader = cache
            .open(file.path(), stereo_selection(file.path(), 0))
            .expect("open large-window source");
        let mut destination = vec![0.0; 2];

        for start in [0, window_frames as i64, (2 * window_frames) as i64] {
            reader
                .read_interleaved(start, 1, &mut destination)
                .expect("populate large PCM window");
        }
        reader
            .read_interleaved(window_frames as i64, 1, &mut destination)
            .expect("make middle window most recent");
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 3);

        cache.reconfigure(AudioSourceCacheConfig::new(2, 2 * window_bytes, 1));
        let reduced = cache.diagnostics();
        assert_eq!(reduced.entries, 2);
        assert_eq!(reduced.reserved_bytes, 2 * window_bytes);
        assert_eq!(reduced.entry_capacity, 2);
        assert_eq!(reduced.byte_budget, 2 * window_bytes);
        assert_eq!(reduced.budget_reconfigurations, 1);
        assert_eq!(reduced.budget_trim_events, 1);
        assert_eq!(reduced.budget_trimmed_entries, 1);
        assert_eq!(reduced.budget_trimmed_bytes, window_bytes as u64);

        reader
            .read_interleaved((2 * window_frames) as i64, 1, &mut destination)
            .expect("newer window survived trim");
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 3);
        reader
            .read_interleaved(0, 1, &mut destination)
            .expect("oldest window reloads after trim");
        assert_eq!(decoder.calls.load(Ordering::Relaxed), 4);

        cache.reconfigure(AudioSourceCacheConfig::new(8, window_bytes, 1));
        let byte_reduced = cache.diagnostics();
        assert_eq!(byte_reduced.entries, 1);
        assert_eq!(byte_reduced.reserved_bytes, window_bytes);
        assert_eq!(byte_reduced.budget_reconfigurations, 2);
        assert_eq!(byte_reduced.budget_trim_events, 2);
        assert_eq!(byte_reduced.budget_trimmed_entries, 2);
        assert_eq!(byte_reduced.budget_trimmed_bytes, (2 * window_bytes) as u64);
    }

    #[test]
    fn independently_bounded_cache_reports_effective_hard_limits() {
        let cache = AudioSourceCache::new_bounded(48_000, 10, 4, 16 * 1024 * 1024);
        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entry_capacity, 4);
        assert_eq!(diagnostics.byte_budget, 16 * 1024 * 1024);
        assert_eq!(
            diagnostics.decoder_session_capacity,
            AUDIO_SOURCE_DECODER_SESSION_CAPACITY
        );
        assert_eq!(diagnostics.entries, 0);
        assert_eq!(diagnostics.reserved_bytes, 0);
    }

    #[test]
    fn reopening_replaced_audio_source_uses_a_new_fingerprint() {
        let decoder = Arc::new(RampWindowDecoder::new());
        let (mut file, cache, first_reader) = test_audio_source(
            Arc::clone(&decoder),
            4,
            4 * 8_000 * 2 * std::mem::size_of::<f32>(),
        );
        let mut destination = vec![0.0; 2];
        first_reader.read_interleaved(0, 1, &mut destination).expect("first identity");
        file.write_all(b"-replacement").expect("replace source identity");
        file.flush().expect("flush replacement");

        let second_reader = cache
            .open(file.path(), stereo_selection(file.path(), 0))
            .expect("reopen replaced source");
        second_reader.read_interleaved(0, 1, &mut destination).expect("second identity");

        assert_eq!(decoder.calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.diagnostics().entries, 2);
    }

    #[test]
    fn open_rejects_a_selection_from_an_obsolete_file_revision() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        file.flush().expect("flush source");
        let selection = stereo_selection(file.path(), 0);
        file.write_all(b"-replacement").expect("replace source identity");
        file.flush().expect("flush replacement");
        let cache = Arc::new(AudioSourceCache::new(48_000));

        let error = cache
            .open(file.path(), selection)
            .err()
            .expect("obsolete stream selection must fail");

        assert!(error.to_string().contains("revision changed"));
    }

    #[test]
    fn open_rejects_partial_fingerprint_evidence_instead_of_using_path_metadata() {
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        file.flush().expect("flush source");
        let partial = AudioSourceSelection::new(
            0,
            ChannelLayout::Exact(AudioChannelLayout::Stereo),
            MediaFileFingerprint {
                len: Some(6),
                modified_secs: Some(1),
                modified_nanos: Some(0),
                object_identity: None,
                change_stamp: None,
            },
        );
        let cache = Arc::new(AudioSourceCache::new(48_000));

        let error = cache
            .open(file.path(), partial)
            .err()
            .expect("partial source identity must fail closed");

        assert!(error.to_string().contains("incomplete"));
    }

    #[test]
    fn malformed_window_fails_closed_and_uses_bounded_failure_memory() {
        let decoder = Arc::new(MalformedWindowDecoder { calls: AtomicU64::new(0) });
        let mut file = tempfile::NamedTempFile::new().expect("temporary source");
        file.write_all(b"source").expect("source identity");
        let cache = Arc::new(AudioSourceCache::with_decoder(
            8_000,
            1,
            2,
            128 * 1024,
            1,
            decoder.clone(),
        ));
        let reader =
            cache.open(file.path(), stereo_selection(file.path(), 0)).expect("open source");
        let mut destination = vec![0.0; 4];

        for _ in 0..2 {
            reader
                .read_interleaved(0, 2, &mut destination)
                .expect_err("malformed channel contract must fail");
        }

        assert_eq!(decoder.calls.load(Ordering::Relaxed), 1);
        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 0);
        assert_eq!(diagnostics.failures, 1);
        assert_eq!(diagnostics.decode_failures, 1);
    }

    #[test]
    #[ignore = "manual real-media parity gate; requires MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH"]
    fn external_audio_windows_match_sequential_decode_at_seek_positions() {
        let Some(path) = std::env::var_os("MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH").map(PathBuf::from)
        else {
            eprintln!("skipped: MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH not set");
            return;
        };
        let sample_rate = 48_000;
        let channel_layout = AudioChannelLayout::Stereo;
        let channels = channel_layout.channel_count_u8();
        let full = decode_audio_file_with_ffmpeg_cli(&path, sample_rate, channel_layout)
            .expect("sequential reference decode");
        assert!(
            full.frame_count() >= sample_rate as usize * 2 + 2_048,
            "manual parity source must contain at least two seconds of audio"
        );
        let cache = Arc::new(AudioSourceCache::with_decoder(
            sample_rate,
            1,
            1,
            sample_rate as usize * usize::from(channels) * std::mem::size_of::<f32>(),
            1,
            Arc::new(PersistentFfmpegAudioWindowDecoder::default()),
        ));
        let stream = crate::probe_media_info(&path)
            .expect("probe external source")
            .primary_audio()
            .expect("primary audio stream")
            .clone();
        let reader = cache
            .open(
                &path,
                AudioSourceSelection::from_stream(&stream, MediaFileFingerprint::capture(&path)),
            )
            .expect("open bounded source");
        let frames = 2_048usize;
        for start in [0usize, sample_rate as usize, 0] {
            if start.saturating_add(frames) > full.frame_count() {
                continue;
            }
            let mut actual = vec![0.0; frames * usize::from(channels)];
            reader
                .read_interleaved(start as i64, frames, &mut actual)
                .expect("window decode");
            let expected_start = start * usize::from(channels);
            let expected = &full.samples[expected_start..expected_start + actual.len()];
            let max_error = actual
                .iter()
                .zip(expected)
                .map(|(actual, expected)| (actual - expected).abs())
                .fold(0.0_f32, f32::max);
            assert!(
                max_error <= 1.0e-4,
                "window at frame {start} differs from sequential decode: max_error={max_error}"
            );
        }
        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 1);
        assert!(diagnostics.reserved_bytes <= diagnostics.byte_budget);
        assert_eq!(diagnostics.decoder_session_opens, 2);
        assert_eq!(diagnostics.decoder_sequential_reuses, 1);
        assert_eq!(diagnostics.decoder_random_seek_restarts, 1);
        assert_eq!(diagnostics.decoder_sessions, 1);
        assert!(diagnostics.decoder_peak_sessions <= diagnostics.decoder_session_capacity);
    }

    #[test]
    #[ignore = "manual real-media cancellation gate; requires MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH"]
    fn external_persistent_audio_session_observes_cancellation() {
        let Some(path) = std::env::var_os("MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH").map(PathBuf::from)
        else {
            eprintln!("skipped: MONDRIAN_AUDIO_EXTERNAL_MEDIA_PATH not set");
            return;
        };
        let cache = Arc::new(AudioSourceCache::new(48_000));
        let stream = crate::probe_media_info(&path)
            .expect("probe external source")
            .primary_audio()
            .expect("primary audio stream")
            .clone();
        let reader = cache
            .open(
                &path,
                AudioSourceSelection::from_stream(&stream, MediaFileFingerprint::capture(&path)),
            )
            .expect("open bounded source");
        let cancellation = ExecutionCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker = std::thread::spawn(move || {
            let mut destination = vec![0.0; 2_048 * 2];
            reader.read_interleaved_cancellable(0, 2_048, &mut destination, &worker_cancellation)
        });
        let admission_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < admission_deadline && cache.diagnostics().decoder_sessions == 0 {
            std::thread::yield_now();
        }
        assert_eq!(cache.diagnostics().decoder_sessions, 1);
        let canceled_at = Instant::now();
        cancellation.cancel();
        let error = worker.join().expect("decode worker returns").expect_err("decode cancels");
        assert!(
            canceled_at.elapsed() <= Duration::from_millis(50),
            "persistent decode cancellation exceeded 50 ms: {:?}",
            canceled_at.elapsed()
        );
        assert!(error.to_string().contains("canceled"));
        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.entries, 0);
        assert_eq!(diagnostics.failures, 0);
        assert_eq!(diagnostics.in_flight_decodes, 0);
        assert_eq!(diagnostics.decoder_sessions, 0);
        assert_eq!(diagnostics.decoder_cancellations, 1);
    }
}
