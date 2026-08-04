//! Bounded background preparation for paused Timeline audio.
//!
//! The app event loop may snapshot one current authoring/playhead demand, but
//! it never compiles or renders audio for speculative warmup. This Module owns
//! one sequential worker, a latest-demand slot, cooperative cancellation, and
//! bounded terminal evidence.

use std::collections::VecDeque;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mondrian_assets::AssetLibrary;
use mondrian_audio::AudioRuntimeResourceGrant;
use mondrian_core::{
    AudioChannelLayout, ExecutionCancellationToken, ExecutionDeadlineStatus, ExecutionPriority,
    ExecutionTerminalDisposition, ExecutionTerminalEvidence, ProjectId, SequenceId,
};
use mondrian_editor_state::AuthoringSessionId;
use mondrian_media::{
    AudioPcmContinuity, AudioPcmRenderGeneration, AudioPcmRenderRequest, AudioPcmRenderer,
    AudioSourceCache,
};
use mondrian_timeline::Sequence;
use parking_lot::{Condvar, Mutex};

use super::audio_rendering::TimelineAudioPcmRenderer;

const TERMINAL_RETENTION: usize = 32;
const FAILED_RETRY_DELAY: Duration = Duration::from_millis(900);
/// Duration of one decoded/DSP block prepared around the paused playhead.
pub(super) const AUDIO_IDLE_WARMUP_CHUNK_MILLIS: u32 = 80;

/// Stable identity of one admitted idle-audio warmup attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioIdleWarmupRequestIdentity {
    /// Module-local monotonic request number.
    pub request_id: u64,
    /// Open authoring lifetime that supplied the immutable snapshot.
    pub authoring_session_id: AuthoringSessionId,
    /// Author generation frozen by this attempt.
    pub author_generation: u64,
    /// Asset Library revision frozen by this attempt.
    pub asset_library_revision: u64,
    /// Active Sequence frozen by this attempt.
    pub sequence_id: SequenceId,
}

/// Bounded terminal record for one admitted idle-audio warmup attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioIdleWarmupTerminal {
    /// Monotonic terminal publication sequence.
    pub terminal_sequence: u64,
    /// Exact admitted attempt.
    pub identity: AudioIdleWarmupRequestIdentity,
    /// Shared execution disposition and generation evidence.
    pub evidence: ExecutionTerminalEvidence,
    /// Bounded failure detail when the background preparation failed.
    pub failure_detail: Option<String>,
}

/// Point-in-time diagnostics for the single idle-audio warmup worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioIdleWarmupDiagnostics {
    /// Whether the bounded latest-demand slot is occupied.
    pub queued: usize,
    /// Exact immutable demand waiting in the latest-demand slot.
    pub queued_identity: Option<AudioIdleWarmupRequestIdentity>,
    /// Identity physically owned by the worker, if any.
    pub running: Option<AudioIdleWarmupRequestIdentity>,
    /// Successfully completed warmup attempts.
    pub completions: u64,
    /// Canceled or superseded attempts, including replaced queued work.
    pub cancellations: u64,
    /// Attempts that ran and failed.
    pub failures: u64,
    /// Attempts rejected before admission.
    pub rejections: u64,
    /// Total terminal records published over this service lifetime.
    pub terminal_count: u64,
    /// Most recent retained terminal record.
    pub latest_terminal: Option<AudioIdleWarmupTerminal>,
    /// Whether the worker thread was created successfully.
    pub worker_available: bool,
}

/// Exact authoring lifetime accepted by the speculative worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AudioIdleWarmupAuthorBinding {
    /// Canonical Project identity.
    pub(super) project_id: ProjectId,
    /// Process-local open-session identity.
    pub(super) authoring_session_id: AuthoringSessionId,
    /// Immutable author generation.
    pub(super) author_generation: u64,
    /// Conservative revision of every media binding visible to this attempt.
    pub(super) asset_library_revision: u64,
}

/// Lightweight identity used to avoid cloning a duplicate Sequence snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AudioIdleWarmupDemandKey {
    binding: AudioIdleWarmupAuthorBinding,
    sequence_id: SequenceId,
    sequence_revision: u64,
    center_sample: i64,
    window_count: usize,
    chunk_frames: usize,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    runtime_grant: AudioRuntimeResourceGrant,
}

/// Complete immutable payload transferred from the app Adapter to the worker.
pub(super) struct AudioIdleWarmupDemand {
    key: AudioIdleWarmupDemandKey,
    sequence: Sequence,
    sequences: Vec<Sequence>,
    library: Arc<AssetLibrary>,
    source_cache: Arc<AudioSourceCache>,
}

impl AudioIdleWarmupDemand {
    /// Build a cheap admission key before cloning the Sequence closure.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn key(
        binding: AudioIdleWarmupAuthorBinding,
        sequence: &Sequence,
        center_sample: i64,
        window_count: usize,
        chunk_frames: usize,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        runtime_grant: AudioRuntimeResourceGrant,
    ) -> AudioIdleWarmupDemandKey {
        AudioIdleWarmupDemandKey {
            binding,
            sequence_id: sequence.id,
            sequence_revision: sequence.revision.get(),
            center_sample,
            window_count,
            chunk_frames,
            sample_rate,
            channel_layout,
            runtime_grant,
        }
    }

    /// Attach the immutable authoring and media handles after admission preflight.
    pub(super) fn from_key(
        key: AudioIdleWarmupDemandKey,
        sequence: Sequence,
        sequences: Vec<Sequence>,
        library: Arc<AssetLibrary>,
        source_cache: Arc<AudioSourceCache>,
    ) -> Self {
        debug_assert_eq!(key.sequence_id, sequence.id);
        debug_assert_eq!(key.sequence_revision, sequence.revision.get());
        Self { key, sequence, sequences, library, source_cache }
    }
}

struct AdmittedDemand {
    identity: AudioIdleWarmupRequestIdentity,
    demand: AudioIdleWarmupDemand,
    cancellation: ExecutionCancellationToken,
}

struct RunningDemand {
    identity: AudioIdleWarmupRequestIdentity,
    key: AudioIdleWarmupDemandKey,
    cancellation: ExecutionCancellationToken,
    cancellation_disposition: Option<ExecutionTerminalDisposition>,
}

struct AudioIdleWarmupState {
    shutdown: bool,
    worker_available: bool,
    dispatch_enabled: bool,
    automatic_policy_enabled: bool,
    binding: Option<AudioIdleWarmupAuthorBinding>,
    next_request_id: u64,
    pending: Option<AdmittedDemand>,
    running: Option<RunningDemand>,
    last_completed_key: Option<AudioIdleWarmupDemandKey>,
    failed_retry: Option<(AudioIdleWarmupDemandKey, Instant)>,
    completions: u64,
    cancellations: u64,
    failures: u64,
    rejections: u64,
    terminal_count: u64,
    terminals: VecDeque<AudioIdleWarmupTerminal>,
}

impl Default for AudioIdleWarmupState {
    fn default() -> Self {
        Self {
            shutdown: false,
            worker_available: true,
            dispatch_enabled: false,
            automatic_policy_enabled: true,
            binding: None,
            next_request_id: 1,
            pending: None,
            running: None,
            last_completed_key: None,
            failed_retry: None,
            completions: 0,
            cancellations: 0,
            failures: 0,
            rejections: 0,
            terminal_count: 0,
            terminals: VecDeque::with_capacity(TERMINAL_RETENTION),
        }
    }
}

struct AudioIdleWarmupShared {
    state: Mutex<AudioIdleWarmupState>,
    wake: Condvar,
}

impl Default for AudioIdleWarmupShared {
    fn default() -> Self {
        Self {
            state: Mutex::new(AudioIdleWarmupState::default()),
            wake: Condvar::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioIdleWarmupExecutionOutcome {
    Completed,
    Superseded,
}

trait AudioIdleWarmupExecutor: Send + Sync + 'static {
    fn execute(
        &self,
        demand: AudioIdleWarmupDemand,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<AudioIdleWarmupExecutionOutcome, String>;
}

struct ProductionAudioIdleWarmupExecutor;

impl AudioIdleWarmupExecutor for ProductionAudioIdleWarmupExecutor {
    fn execute(
        &self,
        demand: AudioIdleWarmupDemand,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<AudioIdleWarmupExecutionOutcome, String> {
        if cancellation.is_canceled() {
            return Ok(AudioIdleWarmupExecutionOutcome::Completed);
        }
        let AudioIdleWarmupDemand { key, sequence, sequences, library, source_cache } = demand;
        if !asset_library_revision_matches(&library, key.binding.asset_library_revision)? {
            return Ok(AudioIdleWarmupExecutionOutcome::Superseded);
        }
        let Some((first_sample, window_count)) =
            causal_window_span(key.center_sample, key.window_count, key.chunk_frames)
        else {
            return Ok(AudioIdleWarmupExecutionOutcome::Completed);
        };
        let revision_probe = Arc::clone(&library);
        let renderer = TimelineAudioPcmRenderer::new(
            sequence,
            sequences,
            library,
            source_cache,
            key.runtime_grant,
            mondrian_audio::AudioAuditionOverlay::default(),
            key.sample_rate,
            key.channel_layout,
        );
        if !asset_library_revision_matches(&revision_probe, key.binding.asset_library_revision)? {
            return Ok(AudioIdleWarmupExecutionOutcome::Superseded);
        }
        let renderer = renderer.map_err(|error| error.to_string())?;
        if !renderer.execution_demand().requires_execution() {
            return Ok(AudioIdleWarmupExecutionOutcome::Completed);
        }
        if cancellation.is_canceled() {
            return Ok(AudioIdleWarmupExecutionOutcome::Completed);
        }

        // Prepare contiguous windows in causal order. The final window starts
        // at the current playhead; no arbitrary post-playhead probe is issued.
        let generation = AudioPcmRenderGeneration::new(1);
        for index in 0..window_count {
            if cancellation.is_canceled() {
                return Ok(AudioIdleWarmupExecutionOutcome::Completed);
            }
            if !asset_library_revision_matches(&revision_probe, key.binding.asset_library_revision)?
            {
                return Ok(AudioIdleWarmupExecutionOutcome::Superseded);
            }
            let offset = index.saturating_mul(key.chunk_frames);
            let start_sample =
                first_sample.saturating_add(i64::try_from(offset).unwrap_or(i64::MAX));
            let continuity = if index == 0 {
                AudioPcmContinuity::Enter(generation)
            } else {
                AudioPcmContinuity::Continue(generation)
            };
            let render = renderer.render(
                AudioPcmRenderRequest {
                    start_sample,
                    frame_count: key.chunk_frames,
                    sample_rate: key.sample_rate,
                    channel_layout: key.channel_layout,
                    continuity,
                },
                cancellation,
            );
            if !asset_library_revision_matches(&revision_probe, key.binding.asset_library_revision)?
            {
                return Ok(AudioIdleWarmupExecutionOutcome::Superseded);
            }
            render.map_err(|error| error.to_string())?;
        }
        Ok(AudioIdleWarmupExecutionOutcome::Completed)
    }
}

fn asset_library_revision_matches(
    library: &AssetLibrary,
    expected_revision: u64,
) -> Result<bool, String> {
    library
        .database_revision()
        .map(|actual_revision| actual_revision == expected_revision)
        .map_err(|error| format!("failed to verify Audio warmup Asset Library revision: {error}"))
}

/// Outcome of submitting one immutable latest warmup demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioIdleWarmupSubmitOutcome {
    /// The demand occupied the single latest-demand slot.
    Queued,
    /// The same immutable demand is already queued, running, or complete.
    Duplicate,
    /// Automatic policy or physical dispatch currently forbids admission.
    Disabled,
    /// The service is shutting down or its worker could not be created.
    Unavailable,
    /// A failed identical attempt is inside its bounded retry delay.
    RetryDeferred,
    /// The request identity counter was exhausted.
    Rejected,
}

/// Domain-owned sequential worker for speculative paused-audio preparation.
pub(super) struct AudioIdleWarmupService {
    shared: Arc<AudioIdleWarmupShared>,
    worker: Option<JoinHandle<()>>,
}

impl AudioIdleWarmupService {
    /// Start the single production worker with dispatch initially suspended.
    pub(super) fn new() -> Self {
        Self::with_executor(Arc::new(ProductionAudioIdleWarmupExecutor))
    }

    fn with_executor(executor: Arc<dyn AudioIdleWarmupExecutor>) -> Self {
        let shared = Arc::new(AudioIdleWarmupShared::default());
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("mondrian-audio-idle-warmup".to_owned())
            .spawn(move || worker_loop(worker_shared, executor));
        match worker {
            Ok(worker) => Self { shared, worker: Some(worker) },
            Err(error) => {
                let mut state = shared.state.lock();
                state.worker_available = false;
                state.shutdown = true;
                tracing::warn!(%error, "failed to create Audio idle-warmup worker");
                drop(state);
                Self { shared, worker: None }
            }
        }
    }

    /// Bind speculative work to one exact open authoring generation.
    pub(super) fn bind_authoring(&self, binding: Option<AudioIdleWarmupAuthorBinding>) {
        let mut state = self.shared.state.lock();
        if state.binding == binding {
            return;
        }
        state.binding = binding;
        state.last_completed_key = None;
        state.failed_retry = None;
        cancel_pending(&mut state, ExecutionTerminalDisposition::Canceled);
        cancel_running(&mut state, ExecutionTerminalDisposition::Canceled);
        self.shared.wake.notify_all();
    }

    /// Enable or suspend physical dispatch.
    ///
    /// A closed gate retains the single bounded pending demand so the
    /// cross-domain allocator can observe it. Already-running speculative work
    /// is canceled cooperatively and must drain before another domain opens.
    /// Realtime/critical policy uses [`Self::set_automatic_policy_enabled`] to
    /// cancel pending work as well.
    pub(super) fn set_dispatch_enabled(&self, enabled: bool) {
        let mut state = self.shared.state.lock();
        if state.dispatch_enabled == enabled {
            return;
        }
        state.dispatch_enabled = enabled;
        if !enabled {
            cancel_running(&mut state, ExecutionTerminalDisposition::Canceled);
        }
        self.shared.wake.notify_all();
    }

    /// Apply the product's automatic-work policy independently from the
    /// physical dispatch gate.
    pub(super) fn set_automatic_policy_enabled(&self, enabled: bool) {
        let mut state = self.shared.state.lock();
        if state.automatic_policy_enabled == enabled {
            return;
        }
        state.automatic_policy_enabled = enabled;
        if !enabled {
            state.last_completed_key = None;
            state.failed_retry = None;
            cancel_pending(&mut state, ExecutionTerminalDisposition::Canceled);
            cancel_running(&mut state, ExecutionTerminalDisposition::Canceled);
        }
        self.shared.wake.notify_all();
    }

    /// Submit one immutable snapshot, replacing at most one older queued
    /// demand and cooperatively canceling obsolete running work.
    pub(super) fn submit(&self, demand: AudioIdleWarmupDemand) -> AudioIdleWarmupSubmitOutcome {
        let now = Instant::now();
        let mut state = self.shared.state.lock();
        if state.shutdown || !state.worker_available {
            return AudioIdleWarmupSubmitOutcome::Unavailable;
        }
        if !state.automatic_policy_enabled {
            return AudioIdleWarmupSubmitOutcome::Disabled;
        }
        if state.binding != Some(demand.key.binding) {
            state.rejections = state.rejections.saturating_add(1);
            return AudioIdleWarmupSubmitOutcome::Rejected;
        }
        if state.pending.as_ref().is_some_and(|pending| pending.demand.key == demand.key)
            || state.running.as_ref().is_some_and(|running| running.key == demand.key)
            || state.last_completed_key == Some(demand.key)
        {
            return AudioIdleWarmupSubmitOutcome::Duplicate;
        }
        if state
            .failed_retry
            .is_some_and(|(key, deadline)| key == demand.key && now < deadline)
        {
            return AudioIdleWarmupSubmitOutcome::RetryDeferred;
        }
        let request_id = state.next_request_id;
        let Some(next_request_id) = request_id.checked_add(1) else {
            state.rejections = state.rejections.saturating_add(1);
            return AudioIdleWarmupSubmitOutcome::Rejected;
        };
        state.next_request_id = next_request_id;
        let identity = AudioIdleWarmupRequestIdentity {
            request_id,
            authoring_session_id: demand.key.binding.authoring_session_id,
            author_generation: demand.key.binding.author_generation,
            asset_library_revision: demand.key.binding.asset_library_revision,
            sequence_id: demand.key.sequence_id,
        };

        cancel_pending(&mut state, ExecutionTerminalDisposition::Superseded);
        cancel_running(&mut state, ExecutionTerminalDisposition::Superseded);
        state.pending = Some(AdmittedDemand {
            identity,
            demand,
            cancellation: ExecutionCancellationToken::new(),
        });
        self.shared.wake.notify_one();
        AudioIdleWarmupSubmitOutcome::Queued
    }

    /// Return whether constructing the full immutable Sequence snapshot could
    /// produce a new admitted demand under the current binding and policy.
    pub(super) fn wants_demand(&self, key: AudioIdleWarmupDemandKey) -> bool {
        let now = Instant::now();
        let state = self.shared.state.lock();
        !state.shutdown
            && state.worker_available
            && state.automatic_policy_enabled
            && state.binding == Some(key.binding)
            && !state.pending.as_ref().is_some_and(|pending| pending.demand.key == key)
            && !state.running.as_ref().is_some_and(|running| running.key == key)
            && state.last_completed_key != Some(key)
            && !state
                .failed_retry
                .is_some_and(|(failed_key, deadline)| failed_key == key && now < deadline)
    }

    /// Return a bounded diagnostic snapshot without changing worker state.
    pub(super) fn diagnostics(&self) -> AudioIdleWarmupDiagnostics {
        let state = self.shared.state.lock();
        AudioIdleWarmupDiagnostics {
            queued: usize::from(state.pending.is_some()),
            queued_identity: state.pending.as_ref().map(|pending| pending.identity),
            running: state.running.as_ref().map(|running| running.identity),
            completions: state.completions,
            cancellations: state.cancellations,
            failures: state.failures,
            rejections: state.rejections,
            terminal_count: state.terminal_count,
            latest_terminal: state.terminals.back().cloned(),
            worker_available: state.worker_available,
        }
    }

    pub(super) fn terminal_delta_after(&self, cursor: u64) -> AudioIdleWarmupTerminalDelta {
        let state = self.shared.state.lock();
        let first_retained = state.terminals.front().map(|terminal| terminal.terminal_sequence);
        let retention_gap = first_retained.is_some_and(|first| cursor.saturating_add(1) < first);
        let records = state
            .terminals
            .iter()
            .filter(|terminal| terminal.terminal_sequence > cursor)
            .cloned()
            .collect();
        AudioIdleWarmupTerminalDelta {
            next_cursor: state.terminal_count,
            retention_gap,
            records,
        }
    }

    fn shutdown(&mut self) {
        {
            let mut state = self.shared.state.lock();
            if !state.shutdown {
                state.shutdown = true;
                state.worker_available = false;
                cancel_pending(&mut state, ExecutionTerminalDisposition::Canceled);
                cancel_running(&mut state, ExecutionTerminalDisposition::Canceled);
            }
            self.shared.wake.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            if worker.is_finished() {
                if worker.join().is_err() {
                    tracing::warn!("Audio idle-warmup worker panicked during shutdown");
                }
                return;
            }
            // A third-party decoder or processor may fail to observe
            // cooperative cancellation. Never let AppState::drop or the UI
            // event loop wait indefinitely. A detached supervisor reaps an
            // ordinarily finishing worker; if its own spawn fails, dropping
            // the JoinHandle safely detaches the worker as the final fallback.
            if let Err(error) = std::thread::Builder::new()
                .name("mondrian-audio-idle-warmup-reaper".to_owned())
                .spawn(move || {
                    if worker.join().is_err() {
                        tracing::warn!("Audio idle-warmup worker panicked during shutdown");
                    }
                })
            {
                tracing::warn!(
                    %error,
                    "failed to create Audio idle-warmup reaper; worker detached"
                );
            }
        }
    }
}

impl Drop for AudioIdleWarmupService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Bounded terminal records published after one consumer cursor.
pub(super) struct AudioIdleWarmupTerminalDelta {
    /// Cursor after all terminal records currently known by the worker.
    pub(super) next_cursor: u64,
    /// Whether the supplied cursor predates retained evidence.
    pub(super) retention_gap: bool,
    /// Retained records newer than the supplied cursor.
    pub(super) records: Vec<AudioIdleWarmupTerminal>,
}

fn worker_loop(shared: Arc<AudioIdleWarmupShared>, executor: Arc<dyn AudioIdleWarmupExecutor>) {
    loop {
        let admitted = {
            let mut state = shared.state.lock();
            while !state.shutdown
                && (!state.dispatch_enabled
                    || !state.automatic_policy_enabled
                    || state.pending.is_none())
            {
                shared.wake.wait(&mut state);
            }
            if state.shutdown {
                return;
            }
            let Some(admitted) = state.pending.take() else {
                continue;
            };
            state.running = Some(RunningDemand {
                identity: admitted.identity,
                key: admitted.demand.key,
                cancellation: admitted.cancellation.clone(),
                cancellation_disposition: None,
            });
            admitted
        };

        let AdmittedDemand { identity, demand, cancellation } = admitted;
        let demand_key = demand.key;
        let execution =
            panic::catch_unwind(AssertUnwindSafe(|| executor.execute(demand, &cancellation)));
        let mut state = shared.state.lock();
        let cancellation_disposition = state
            .running
            .as_ref()
            .filter(|running| running.identity == identity)
            .and_then(|running| running.cancellation_disposition);
        if state.running.as_ref().is_some_and(|running| running.identity == identity) {
            state.running = None;
        }
        let (disposition, failure_detail) = if let Some(disposition) = cancellation_disposition {
            (disposition, None)
        } else {
            match execution {
                Ok(Ok(AudioIdleWarmupExecutionOutcome::Completed)) => {
                    (ExecutionTerminalDisposition::Completed, None)
                }
                Ok(Ok(AudioIdleWarmupExecutionOutcome::Superseded)) => {
                    (ExecutionTerminalDisposition::Superseded, None)
                }
                Ok(Err(error)) => (
                    ExecutionTerminalDisposition::Failed,
                    Some(bounded_failure_detail(error)),
                ),
                Err(_) => (
                    ExecutionTerminalDisposition::Failed,
                    Some("Audio idle-warmup executor panicked".to_owned()),
                ),
            }
        };
        match disposition {
            ExecutionTerminalDisposition::Completed => {
                state.completions = state.completions.saturating_add(1);
                state.last_completed_key = Some(demand_key);
                state.failed_retry = None;
            }
            ExecutionTerminalDisposition::Canceled | ExecutionTerminalDisposition::Superseded => {
                state.cancellations = state.cancellations.saturating_add(1);
                state.failed_retry = None;
            }
            ExecutionTerminalDisposition::Failed => {
                state.failures = state.failures.saturating_add(1);
                state.failed_retry = Some((demand_key, Instant::now() + FAILED_RETRY_DELAY));
            }
            ExecutionTerminalDisposition::Rejected => {
                state.rejections = state.rejections.saturating_add(1);
            }
        }
        record_terminal(&mut state, identity, disposition, failure_detail);
        shared.wake.notify_all();
    }
}

fn cancel_pending(state: &mut AudioIdleWarmupState, disposition: ExecutionTerminalDisposition) {
    let Some(pending) = state.pending.take() else {
        return;
    };
    pending.cancellation.cancel();
    state.cancellations = state.cancellations.saturating_add(1);
    record_terminal(state, pending.identity, disposition, None);
}

fn cancel_running(state: &mut AudioIdleWarmupState, disposition: ExecutionTerminalDisposition) {
    let Some(running) = state.running.as_mut() else {
        return;
    };
    if running.cancellation_disposition.is_none() {
        running.cancellation_disposition = Some(disposition);
        running.cancellation.cancel();
    }
}

fn record_terminal(
    state: &mut AudioIdleWarmupState,
    identity: AudioIdleWarmupRequestIdentity,
    disposition: ExecutionTerminalDisposition,
    failure_detail: Option<String>,
) {
    state.terminal_count = state.terminal_count.saturating_add(1);
    if state.terminals.len() == TERMINAL_RETENTION {
        state.terminals.pop_front();
    }
    state.terminals.push_back(AudioIdleWarmupTerminal {
        terminal_sequence: state.terminal_count,
        identity,
        evidence: ExecutionTerminalEvidence {
            generation: identity.author_generation,
            priority: ExecutionPriority::Maintenance,
            disposition,
            deadline: ExecutionDeadlineStatus::NotApplicable,
        },
        failure_detail,
    });
}

fn bounded_failure_detail(mut detail: String) -> String {
    const MAX_CHARS: usize = 512;
    if detail.chars().count() <= MAX_CHARS {
        return detail;
    }
    let byte_boundary =
        detail.char_indices().nth(MAX_CHARS).map_or(detail.len(), |(index, _)| index);
    detail.truncate(byte_boundary);
    detail.push('…');
    detail
}

fn causal_window_span(
    center_sample: i64,
    requested_windows: usize,
    chunk_frames: usize,
) -> Option<(i64, usize)> {
    if requested_windows == 0 || chunk_frames == 0 {
        return None;
    }
    let center_sample = center_sample.max(0);
    let requested_preceding = requested_windows.saturating_sub(1);
    let available_preceding = usize::try_from(center_sample).unwrap_or(usize::MAX) / chunk_frames;
    let preceding = requested_preceding.min(available_preceding);
    let preceding_frames = preceding.saturating_mul(chunk_frames);
    let first_sample = center_sample
        .saturating_sub(i64::try_from(preceding_frames).unwrap_or(i64::MAX))
        .max(0);
    Some((first_sample, preceding.saturating_add(1)))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use super::*;
    use crate::app::AppState;
    use mondrian_timeline::Sequence;

    struct BlockingExecutor {
        started: mpsc::Sender<i64>,
        release_first: Arc<AtomicBool>,
    }

    struct CancellationIgnoringExecutor {
        started: mpsc::Sender<()>,
        release: Arc<AtomicBool>,
    }

    struct ReleaseOnDrop(Arc<AtomicBool>);

    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    impl AudioIdleWarmupExecutor for BlockingExecutor {
        fn execute(
            &self,
            demand: AudioIdleWarmupDemand,
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioIdleWarmupExecutionOutcome, String> {
            self.started.send(demand.key.center_sample).map_err(|error| error.to_string())?;
            if demand.key.center_sample == 1 {
                while !self.release_first.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
            }
            Ok(AudioIdleWarmupExecutionOutcome::Completed)
        }
    }

    impl AudioIdleWarmupExecutor for CancellationIgnoringExecutor {
        fn execute(
            &self,
            _demand: AudioIdleWarmupDemand,
            _cancellation: &ExecutionCancellationToken,
        ) -> Result<AudioIdleWarmupExecutionOutcome, String> {
            self.started.send(()).map_err(|error| error.to_string())?;
            while !self.release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(AudioIdleWarmupExecutionOutcome::Completed)
        }
    }

    fn fixture(center_sample: i64, binding: AudioIdleWarmupAuthorBinding) -> AudioIdleWarmupDemand {
        fixture_for_sequence(center_sample, binding, &Sequence::new("warmup"))
    }

    fn fixture_for_sequence(
        center_sample: i64,
        binding: AudioIdleWarmupAuthorBinding,
        sequence: &Sequence,
    ) -> AudioIdleWarmupDemand {
        let root = AppState::test_fixture_root();
        let library = AssetLibrary::open(root.join("library")).expect("open Asset Library");
        let key = AudioIdleWarmupDemand::key(
            binding,
            sequence,
            center_sample,
            1,
            3_840,
            48_000,
            AudioChannelLayout::Stereo,
            AudioRuntimeResourceGrant::new(8, 64 * 1024 * 1024, 16 * 1024 * 1024),
        );
        AudioIdleWarmupDemand::from_key(
            key,
            sequence.clone(),
            vec![sequence.clone()],
            library,
            Arc::new(AudioSourceCache::new(48_000)),
        )
    }

    fn binding() -> (AppState, AudioIdleWarmupAuthorBinding) {
        let mut app = AppState::new();
        let sequence = Sequence::new("binding");
        app.test_set_sequence(Some(sequence));
        let binding = AudioIdleWarmupAuthorBinding {
            project_id: app.project_id().expect("project"),
            authoring_session_id: app.authoring_session_id().expect("session"),
            author_generation: app.project_author_generation(),
            asset_library_revision: app
                .asset_library()
                .expect("library")
                .database_revision()
                .expect("library revision"),
        };
        (app, binding)
    }

    #[test]
    fn latest_demand_is_bounded_and_supersedes_running_and_queued_work() {
        let (_app, binding) = binding();
        let (started_tx, started_rx) = mpsc::channel();
        let release_first = Arc::new(AtomicBool::new(false));
        let mut service = AudioIdleWarmupService::with_executor(Arc::new(BlockingExecutor {
            started: started_tx,
            release_first: Arc::clone(&release_first),
        }));
        let _release_on_drop = ReleaseOnDrop(Arc::clone(&release_first));
        service.bind_authoring(Some(binding));
        service.set_automatic_policy_enabled(true);
        service.set_dispatch_enabled(true);

        assert_eq!(
            service.submit(fixture(1, binding)),
            AudioIdleWarmupSubmitOutcome::Queued
        );
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(2)).expect("first started"),
            1
        );
        assert_eq!(
            service.submit(fixture(2, binding)),
            AudioIdleWarmupSubmitOutcome::Queued
        );
        assert_eq!(
            service.submit(fixture(3, binding)),
            AudioIdleWarmupSubmitOutcome::Queued
        );
        release_first.store(true, Ordering::Release);
        assert_eq!(
            started_rx.recv_timeout(Duration::from_secs(2)).expect("latest started"),
            3
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        while service.diagnostics().completions == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.queued, 0);
        assert!(diagnostics.running.is_none());
        assert_eq!(diagnostics.completions, 1);
        assert_eq!(diagnostics.cancellations, 2);
        assert_eq!(diagnostics.terminal_count, 3);
        assert_eq!(
            diagnostics.latest_terminal.expect("terminal").evidence.disposition,
            ExecutionTerminalDisposition::Completed
        );
        service.shutdown();
    }

    #[test]
    fn dispatch_and_author_generation_changes_cancel_speculative_work() {
        let (_app, binding) = binding();
        let (started_tx, started_rx) = mpsc::channel();
        let release_first = Arc::new(AtomicBool::new(false));
        let mut service = AudioIdleWarmupService::with_executor(Arc::new(BlockingExecutor {
            started: started_tx,
            release_first: Arc::clone(&release_first),
        }));
        let _release_on_drop = ReleaseOnDrop(Arc::clone(&release_first));
        service.bind_authoring(Some(binding));
        service.set_dispatch_enabled(false);
        assert_eq!(
            service.submit(fixture(1, binding)),
            AudioIdleWarmupSubmitOutcome::Queued
        );
        assert_eq!(service.diagnostics().queued, 1);
        assert!(
            started_rx.recv_timeout(Duration::from_millis(20)).is_err(),
            "a retained demand must not cross a closed dispatch seam"
        );
        service.set_dispatch_enabled(true);
        started_rx.recv_timeout(Duration::from_secs(2)).expect("started");
        service.bind_authoring(Some(AudioIdleWarmupAuthorBinding {
            author_generation: binding.author_generation + 1,
            ..binding
        }));
        release_first.store(true, Ordering::Release);

        let deadline = Instant::now() + Duration::from_secs(2);
        while service.diagnostics().cancellations == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let diagnostics = service.diagnostics();
        assert_eq!(diagnostics.cancellations, 1);
        assert_eq!(
            diagnostics.latest_terminal.expect("terminal").evidence.disposition,
            ExecutionTerminalDisposition::Canceled
        );
        service.shutdown();
    }

    #[test]
    fn shutdown_never_joins_an_executor_that_ignores_cancellation_on_the_owner_thread() {
        let (_app, binding) = binding();
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new(AtomicBool::new(false));
        let mut service =
            AudioIdleWarmupService::with_executor(Arc::new(CancellationIgnoringExecutor {
                started: started_tx,
                release: Arc::clone(&release),
            }));
        let _release_on_drop = ReleaseOnDrop(Arc::clone(&release));
        service.bind_authoring(Some(binding));
        service.set_dispatch_enabled(true);
        assert_eq!(
            service.submit(fixture(1, binding)),
            AudioIdleWarmupSubmitOutcome::Queued
        );
        started_rx.recv_timeout(Duration::from_secs(2)).expect("warmup started");

        let started = Instant::now();
        service.shutdown();
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "App owner shutdown must not wait for an uncooperative executor"
        );
        assert!(!service.diagnostics().worker_available);
        release.store(true, Ordering::Release);
    }

    #[test]
    fn asset_library_revision_rotates_duplicate_and_running_authority() {
        let (_app, binding) = binding();
        let sequence = Sequence::new("revision-bound warmup");
        let (started_tx, started_rx) = mpsc::channel();
        let mut service = AudioIdleWarmupService::with_executor(Arc::new(BlockingExecutor {
            started: started_tx,
            release_first: Arc::new(AtomicBool::new(true)),
        }));
        service.bind_authoring(Some(binding));
        service.set_dispatch_enabled(true);
        assert_eq!(
            service.submit(fixture_for_sequence(2, binding, &sequence)),
            AudioIdleWarmupSubmitOutcome::Queued
        );
        started_rx.recv_timeout(Duration::from_secs(2)).expect("first revision started");
        let deadline = Instant::now() + Duration::from_secs(2);
        while service.diagnostics().completions == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(
            service.submit(fixture_for_sequence(2, binding, &sequence)),
            AudioIdleWarmupSubmitOutcome::Duplicate
        );

        let next_binding = AudioIdleWarmupAuthorBinding {
            asset_library_revision: binding.asset_library_revision.saturating_add(1),
            ..binding
        };
        service.bind_authoring(Some(next_binding));
        let next_demand = fixture_for_sequence(2, next_binding, &sequence);
        assert!(service.wants_demand(next_demand.key));
        assert_eq!(
            service.submit(next_demand),
            AudioIdleWarmupSubmitOutcome::Queued
        );
        started_rx.recv_timeout(Duration::from_secs(2)).expect("new revision started");

        let deadline = Instant::now() + Duration::from_secs(2);
        while service.diagnostics().completions < 2 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let terminal = service.diagnostics().latest_terminal.expect("terminal");
        assert_eq!(
            terminal.identity.asset_library_revision,
            next_binding.asset_library_revision
        );
        service.shutdown();
    }

    #[test]
    fn production_executor_rejects_a_demand_after_its_library_revision_changes() {
        let (_app, binding) = binding();
        let mut demand = fixture(2, binding);
        demand.key.binding.asset_library_revision =
            demand.library.database_revision().expect("initial library revision");
        demand
            .library
            .create_folder("new revision", None)
            .expect("advance library revision");

        let outcome = ProductionAudioIdleWarmupExecutor
            .execute(demand, &ExecutionCancellationToken::new())
            .expect("revision check");

        assert_eq!(outcome, AudioIdleWarmupExecutionOutcome::Superseded);
    }

    #[test]
    fn causal_window_span_never_invents_negative_or_future_predecessors() {
        assert_eq!(causal_window_span(0, 3, 1_000), Some((0, 1)));
        assert_eq!(causal_window_span(1_500, 3, 1_000), Some((500, 2)));
        assert_eq!(causal_window_span(5_000, 3, 1_000), Some((3_000, 3)));
        assert_eq!(causal_window_span(5_000, 0, 1_000), None);
        assert_eq!(causal_window_span(5_000, 3, 0), None);
    }
}
