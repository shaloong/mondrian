//! Instance-owned proxy-generation execution service.
//!
//! This deep Module is the sole application owner of proxy demand admission,
//! deduplication, priority promotion, project generations, failure memory, and
//! terminal evidence. `mondrian-media` owns artifact identity and FFmpeg work;
//! callers only submit typed intent and consume structured outcomes.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;
#[cfg(any(test, feature = "validation"))]
use std::time::Instant;

use mondrian_assets::AssetRecord;
use mondrian_core::types::{AssetId, ProjectId};
use mondrian_core::{ExecutionPriority, ExecutionTerminalEvidence};
use mondrian_media::{
    resolve_decoded_video_range, MediaFileFingerprint, ProxyColorContract, ProxyConfig,
    ProxyPublicationEvidence, ProxyPublicationFailure, ProxyStatus,
};
use mondrian_timeline::sequence::{MediaInputColorContext, ResolvedInputColor};
use parking_lot::{Condvar, Mutex};

use self::backend::{MediaProxyGenerationBackend, ProxyGenerationBackend};
#[cfg(any(test, feature = "validation"))]
use self::state::clear_after_workers_terminated;
use self::state::{
    bind_project_generation, cancel_for_shutdown, diagnostics_snapshot, preflight_request,
    record_immediate_failure, request_admission, request_running_resource_yield,
    terminal_delta_snapshot, ProxyGenerationInner, ProxyGenerationKey, ProxyGenerationRequest,
    ProxyGenerationState, RunningResourceYieldScope,
};
#[cfg(any(test, feature = "validation"))]
use super::endurance_shutdown::{join_workers_until, EnduranceWorkerShutdownEvidence};
use super::AppState;

mod backend;
mod state;
#[cfg(test)]
mod tests;

const PROXY_PENDING_CAPACITY: usize = 512;
const PROXY_FAILURE_CAPACITY: usize = 256;
const PROXY_TERMINAL_CAPACITY: usize = 512;
const MAX_PROXY_GENERATION_WORKERS: usize = 8;
const PROXY_FOREGROUND_BURST: usize = 8;

/// Why proxy generation was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProxyGenerationOrigin {
    /// Explicit user request or retry.
    User,
    /// Playback pressure requested optimized media for future frames.
    PlaybackRecovery,
    /// Import policy requested background optimized media.
    Import,
}

impl ProxyGenerationOrigin {
    const fn priority(self) -> ExecutionPriority {
        match self {
            Self::User => ExecutionPriority::UserInitiated,
            Self::PlaybackRecovery | Self::Import => ExecutionPriority::Background,
        }
    }

    const fn rank(self) -> u8 {
        match self {
            Self::User => 0,
            Self::PlaybackRecovery => 1,
            Self::Import => 2,
        }
    }
}

/// Stable machine-readable proxy service failure category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProxyGenerationFailureReason {
    /// The source path disappeared or is inaccessible.
    MissingSourceFile,
    /// Source color or requested proxy encoding cannot form a valid artifact.
    InvalidProxyContract,
    /// No execution worker could be started.
    WorkerUnavailable,
    /// The bounded service demand capacity was exhausted.
    AdmissionRejected,
    /// Media generation failed after admission.
    GenerationFailed,
    /// Media or sidecar namespace publication did not return durable success.
    PublicationFailed,
}

impl ProxyGenerationFailureReason {
    /// Stable diagnostic code for logs, evidence, and support tooling.
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingSourceFile => "missing_source_file",
            Self::InvalidProxyContract => "invalid_proxy_contract",
            Self::WorkerUnavailable => "worker_unavailable",
            Self::AdmissionRejected => "admission_rejected",
            Self::GenerationFailed => "generation_failed",
            Self::PublicationFailed => "publication_failed",
        }
    }
}

/// Structured proxy service failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProxyGenerationFailure {
    pub(crate) reason: ProxyGenerationFailureReason,
    pub(crate) detail: String,
    pub(crate) publication: Option<Box<ProxyPublicationFailure>>,
}

impl ProxyGenerationFailure {
    fn new(reason: ProxyGenerationFailureReason, detail: impl Into<String>) -> Self {
        Self { reason, detail: detail.into(), publication: None }
    }

    fn publication(failure: ProxyPublicationFailure) -> Self {
        Self {
            reason: ProxyGenerationFailureReason::PublicationFailed,
            detail: failure.to_string(),
            publication: Some(Box::new(failure)),
        }
    }
}

/// Outcome of one nonblocking proxy request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProxyGenerationRequestOutcome {
    /// The exact proxy artifact is already reusable.
    AlreadyFresh,
    /// A new bounded attempt was admitted.
    Admitted { prior_status: ProxyStatus },
    /// An existing exact attempt owns the work; it may have been promoted.
    Deduplicated { promoted: bool },
    /// Automatic work remains suppressed after an exact retained failure.
    RetainedFailure(ProxyGenerationFailure),
    /// The request could not be admitted or validated.
    Failed(ProxyGenerationFailure),
}

/// One bounded terminal record for Headless and product diagnostics.
#[derive(Debug, Clone)]
pub struct ProxyGenerationTerminalRecord {
    /// Monotonic publication identity used by event-loop delta consumers.
    ///
    /// This is deliberately independent of `attempt_id`: concurrent attempts
    /// may finish in a different order from admission.
    pub terminal_sequence: u64,
    /// Shared priority, generation, disposition, and deadline classification.
    pub evidence: ExecutionTerminalEvidence,
    /// Module-local monotonic attempt identity.
    pub attempt_id: u64,
    /// Asset that owned the request.
    pub asset_id: AssetId,
    /// Exact source revision admitted by the attempt.
    pub source_fingerprint: MediaFileFingerprint,
    /// Product origin used by domain scheduling.
    pub origin: ProxyGenerationOrigin,
    /// Wall duration after worker dispatch.
    pub elapsed: Duration,
    /// Whether the attempt crossed the worker execution boundary.
    pub executed: bool,
    /// Structured failure category, when applicable.
    pub failure: Option<ProxyGenerationFailureReason>,
    /// Bounded human-readable diagnostic detail, when applicable.
    pub failure_detail: Option<String>,
    /// Typed media-owned publication terminal state, when applicable.
    pub publication_failure: Option<ProxyPublicationFailure>,
    /// Durable publication evidence for newly generated media and sidecar.
    pub publication: Option<ProxyPublicationEvidence>,
}

/// Immutable bounded proxy execution evidence.
#[derive(Debug, Clone, Default)]
pub struct ProxyGenerationDiagnostics {
    /// Wrapping observation token for every diagnostic field exposed here.
    ///
    /// Consumers compare this token only for equality. It is not an attempt
    /// identity, event count, or linearizable snapshot version.
    pub revision: u64,
    /// Current project execution generation.
    pub generation: u64,
    /// Whether queued attempts may cross the worker dispatch Seam.
    pub dispatch_enabled: bool,
    /// Whether automatic Import/Playback Recovery attempts may dispatch.
    pub automatic_dispatch_enabled: bool,
    /// Product-requested global running-attempt limit.
    pub dispatch_parallelism: usize,
    /// Whether lazy worker startup was attempted.
    pub worker_startup_attempted: bool,
    /// Workers configured for this service instance.
    pub requested_workers: usize,
    /// Worker threads successfully created.
    pub started_workers: usize,
    /// Whether a started worker returned before shutdown was requested.
    pub worker_unexpectedly_exited: bool,
    /// Admitted attempts waiting for a worker.
    pub queued: usize,
    /// Explicit user attempts waiting for a worker.
    pub queued_user: usize,
    /// Attempts currently executing in a worker.
    pub running: usize,
    /// Running attempts currently stopping at the resource-yield boundary.
    pub yielding: usize,
    /// Explicit user attempts currently executing in a worker.
    pub running_user: usize,
    /// Exact failures suppressing automatic retry storms.
    pub retained_failures: usize,
    /// Newly admitted attempts.
    pub admissions: u64,
    /// Exact requests joined to existing attempts.
    pub deduplications: u64,
    /// Exact Queued or Running attempts promoted by a higher-priority origin.
    pub promotions: u64,
    /// Requests satisfied by an already-fresh artifact.
    pub fresh_hits: u64,
    /// Current-generation successful attempts.
    pub completions: u64,
    /// Current-generation failed attempts.
    pub failures: u64,
    /// Cooperatively canceled attempts.
    pub cancellations: u64,
    /// Running attempts safely returned to the queue by product resource
    /// policy without losing explicit demand.
    pub resource_yields: u64,
    /// Completed work made ineligible by a newer binding.
    pub superseded: u64,
    /// Attempts rejected by bounded admission.
    pub rejections: u64,
    /// Oldest terminal publication still retained by the bounded service.
    pub oldest_terminal_sequence: Option<u64>,
    /// Latest terminal publication issued by this service lifetime.
    pub latest_terminal_sequence: u64,
    /// Bounded terminal attempt evidence.
    pub terminal_records: Vec<ProxyGenerationTerminalRecord>,
}

/// Ordered terminal publications newer than one event-loop cursor.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProxyGenerationTerminalDelta {
    /// Current Project execution generation at snapshot time.
    pub(crate) generation: u64,
    /// Cursor to persist after consuming every record in this delta.
    pub(crate) next_cursor: u64,
    /// Whether bounded retention discarded one or more publications after the
    /// supplied cursor before this poll could observe them.
    pub(crate) retention_gap: bool,
    /// Retained terminal publications in strict publication order.
    pub(crate) records: Vec<ProxyGenerationTerminalRecord>,
}

/// Lazily-started, instance-owned proxy execution service.
pub(crate) struct ProxyGenerationService {
    inner: Arc<ProxyGenerationInner>,
    requested_worker_count: usize,
    started_workers: OnceLock<usize>,
    worker_handles: Mutex<Vec<JoinHandle<()>>>,
    observed_model_revision: AtomicU64,
}

impl ProxyGenerationService {
    pub(crate) fn new() -> Self {
        Self::with_backend(
            proxy_generation_worker_count(),
            Arc::new(MediaProxyGenerationBackend),
        )
    }

    fn with_backend(
        requested_worker_count: usize,
        backend: Arc<dyn ProxyGenerationBackend>,
    ) -> Self {
        Self {
            inner: Arc::new(ProxyGenerationInner {
                state: Mutex::new(ProxyGenerationState::default()),
                available: Condvar::new(),
                backend,
                diagnostics_revision: AtomicU64::new(0),
                model_revision: AtomicU64::new(0),
                shutdown: AtomicBool::new(false),
            }),
            requested_worker_count,
            started_workers: OnceLock::new(),
            worker_handles: Mutex::new(Vec::new()),
            observed_model_revision: AtomicU64::new(0),
        }
    }

    /// Rotate the owning project generation and cancel all obsolete attempts.
    pub(crate) fn bind_project(&self, project_id: Option<ProjectId>) {
        let changed = {
            let mut state = self.inner.state.lock();
            bind_project_generation(&mut state, project_id)
        };
        if changed {
            self.inner.mark_model_changed();
            self.inner.available.notify_all();
        }
    }

    /// Apply product resource policy while preserving domain-owned admission,
    /// queue ordering, attempt identity, and terminal evidence.
    ///
    /// Running work cooperatively yields at the backend cancellation Seam when
    /// global dispatch closes. Closing only automatic dispatch yields
    /// Import/Playback Recovery attempts while User work remains runnable.
    pub(crate) fn set_resource_policy(
        &self,
        dispatch_enabled: bool,
        max_parallelism: usize,
        automatic_dispatch_enabled: bool,
    ) {
        let changed = {
            let mut state = self.inner.state.lock();
            let max_parallelism = max_parallelism.max(1);
            let policy_changed = !(state.dispatch_enabled == dispatch_enabled
                && state.dispatch_parallelism == max_parallelism
                && state.automatic_dispatch_enabled == automatic_dispatch_enabled);
            if policy_changed {
                state.dispatch_enabled = dispatch_enabled;
                state.dispatch_parallelism = max_parallelism;
                state.automatic_dispatch_enabled = automatic_dispatch_enabled;
            }
            let yield_scope = if !dispatch_enabled {
                RunningResourceYieldScope::All
            } else if !automatic_dispatch_enabled {
                RunningResourceYieldScope::AutomaticOnly
            } else {
                RunningResourceYieldScope::None
            };
            let running_yield_requested = request_running_resource_yield(&mut state, yield_scope);
            policy_changed || running_yield_requested
        };
        if changed {
            self.inner.mark_diagnostics_changed();
            self.inner.available.notify_all();
        }
    }

    /// Resolve freshness and admit one exact request without blocking on FFmpeg.
    pub(crate) fn request(
        &self,
        asset_id: AssetId,
        source_path: PathBuf,
        config: ProxyConfig,
        color: ProxyColorContract,
        origin: ProxyGenerationOrigin,
    ) -> ProxyGenerationRequestOutcome {
        if let Err(error) = std::fs::metadata(&source_path) {
            return self.immediate_failure(
                None,
                asset_id,
                MediaFileFingerprint::default(),
                origin,
                ProxyGenerationFailure::new(
                    ProxyGenerationFailureReason::MissingSourceFile,
                    format!("proxy source is unavailable: {error}"),
                ),
            );
        }
        let source_fingerprint = MediaFileFingerprint::capture(&source_path);
        let config = match config.freeze_cache_root() {
            Ok(config) => config,
            Err(error) => {
                return self.immediate_failure(
                    None,
                    asset_id,
                    source_fingerprint,
                    origin,
                    ProxyGenerationFailure::new(
                        ProxyGenerationFailureReason::InvalidProxyContract,
                        error.to_string(),
                    ),
                );
            }
        };
        let request = match ProxyGenerationRequest::new(
            asset_id,
            source_path,
            source_fingerprint,
            config,
            color,
        ) {
            Ok(request) => request,
            Err(failure) => {
                return self.immediate_failure(None, asset_id, source_fingerprint, origin, failure);
            }
        };
        if let Some(outcome) = {
            let mut state = self.inner.state.lock();
            preflight_request(&mut state, &request.key, origin)
        } {
            if let ProxyGenerationRequestOutcome::RetainedFailure(failure) = &outcome
                && failure.publication.is_some()
                && matches!(self.inner.backend.status(&request), Ok(ProxyStatus::Fresh))
            {
                let mut state = self.inner.state.lock();
                state.remove_failure(&request.key);
                state.counters.fresh_hits = state.counters.fresh_hits.saturating_add(1);
                drop(state);
                self.inner.mark_diagnostics_changed();
                return ProxyGenerationRequestOutcome::AlreadyFresh;
            }
            match &outcome {
                ProxyGenerationRequestOutcome::Deduplicated { promoted: true } => {
                    self.inner.mark_model_changed();
                    self.inner.available.notify_all();
                }
                ProxyGenerationRequestOutcome::Deduplicated { promoted: false } => {
                    self.inner.mark_diagnostics_changed();
                }
                _ => {}
            }
            return outcome;
        }
        let prior_status = match self.inner.backend.status(&request) {
            Ok(ProxyStatus::Fresh) => {
                let mut state = self.inner.state.lock();
                state.remove_failure(&request.key);
                state.counters.fresh_hits = state.counters.fresh_hits.saturating_add(1);
                drop(state);
                self.inner.mark_diagnostics_changed();
                return ProxyGenerationRequestOutcome::AlreadyFresh;
            }
            Ok(status) => status,
            Err(failure) => {
                return self.immediate_failure(
                    Some(request.key.clone()),
                    asset_id,
                    request.key.source_fingerprint,
                    origin,
                    failure,
                );
            }
        };
        if self.ensure_workers_started() == 0 {
            return self.immediate_failure(
                Some(request.key.clone()),
                asset_id,
                request.key.source_fingerprint,
                origin,
                ProxyGenerationFailure::new(
                    ProxyGenerationFailureReason::WorkerUnavailable,
                    "proxy generation service has no live workers",
                ),
            );
        }

        let outcome = {
            let mut state = self.inner.state.lock();
            request_admission(&mut state, request, origin, prior_status)
        };
        match &outcome {
            ProxyGenerationRequestOutcome::Admitted { .. } => {
                self.inner.mark_model_changed();
                self.inner.available.notify_one();
            }
            ProxyGenerationRequestOutcome::Deduplicated { promoted: true } => {
                self.inner.mark_model_changed();
                self.inner.available.notify_all();
            }
            ProxyGenerationRequestOutcome::Deduplicated { promoted: false } => {
                self.inner.mark_diagnostics_changed();
            }
            ProxyGenerationRequestOutcome::Failed(_) => {
                self.inner.mark_model_changed();
            }
            ProxyGenerationRequestOutcome::AlreadyFresh
            | ProxyGenerationRequestOutcome::RetainedFailure(_) => {}
        }
        outcome
    }

    /// Observe product-model changes since the previous event-loop poll.
    ///
    /// Project binding, demand admission or promotion, real execution phase
    /// changes, and terminal publication advance this cursor. Resource policy,
    /// cooperative yield, deduplication counters, and freshness-hit diagnostics
    /// do not claim a user-visible completion or model change.
    pub(crate) fn poll_finished(&self) -> bool {
        let current = self.inner.model_revision.load(Ordering::Acquire);
        let previous = self.observed_model_revision.swap(current, Ordering::AcqRel);
        current != previous
    }

    pub(crate) fn diagnostics(&self) -> ProxyGenerationDiagnostics {
        let state = self.inner.state.lock();
        let mut diagnostics = diagnostics_snapshot(&state);
        diagnostics.revision = self.inner.diagnostics_revision.load(Ordering::Acquire);
        diagnostics.worker_startup_attempted = self.started_workers.get().is_some();
        diagnostics.requested_workers = self.requested_worker_count;
        diagnostics.started_workers = self.worker_handles.lock().len();
        diagnostics.worker_unexpectedly_exited =
            self.worker_handles.lock().iter().any(JoinHandle::is_finished)
                && !self.inner.shutdown.load(Ordering::Acquire);
        diagnostics
    }

    /// Return terminal publications newer than `cursor` in publication order.
    pub(crate) fn terminal_delta_after(&self, cursor: u64) -> ProxyGenerationTerminalDelta {
        terminal_delta_snapshot(&self.inner.state.lock(), cursor)
    }

    fn immediate_failure(
        &self,
        key: Option<ProxyGenerationKey>,
        asset_id: AssetId,
        fingerprint: MediaFileFingerprint,
        origin: ProxyGenerationOrigin,
        failure: ProxyGenerationFailure,
    ) -> ProxyGenerationRequestOutcome {
        let mut state = self.inner.state.lock();
        record_immediate_failure(
            &mut state,
            key,
            asset_id,
            fingerprint,
            origin,
            failure.clone(),
        );
        drop(state);
        self.inner.mark_model_changed();
        ProxyGenerationRequestOutcome::Failed(failure)
    }

    fn ensure_workers_started(&self) -> usize {
        *self.started_workers.get_or_init(|| {
            let mut started = 0;
            let mut handles = self.worker_handles.lock();
            for index in 0..self.requested_worker_count {
                let runtime =
                    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            tracing::error!(
                                target: "mondrian::proxy",
                                worker_index = index,
                                %error,
                                "failed to build proxy generation worker runtime"
                            );
                            continue;
                        }
                    };
                let inner = Arc::clone(&self.inner);
                match std::thread::Builder::new()
                    .name(format!("mondrian-proxy-generator-{index}"))
                    .spawn(move || state::proxy_generation_worker(inner, runtime))
                {
                    Ok(handle) => {
                        handles.push(handle);
                        started += 1;
                    }
                    Err(error) => tracing::error!(
                        target: "mondrian::proxy",
                        worker_index = index,
                        %error,
                        "failed to start proxy generation worker"
                    ),
                }
            }
            started
        })
    }

    pub(crate) fn begin_endurance_shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::Release);
        cancel_for_shutdown(&mut self.inner.state.lock());
        self.inner.available.notify_all();
    }

    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn finish_endurance_shutdown(
        &mut self,
        deadline: Instant,
    ) -> EnduranceWorkerShutdownEvidence {
        let startup_attempted = self.started_workers.get().is_some();
        self.begin_endurance_shutdown();
        let join = join_workers_until(self.worker_handles.get_mut(), deadline);
        let mut state = self.inner.state.lock();
        let cumulative_failures = state.counters.failures.saturating_add(state.counters.rejections);
        if join.all_workers_returned_normally() {
            clear_after_workers_terminated(&mut state);
        }
        let diagnostics = diagnostics_snapshot(&state);
        EnduranceWorkerShutdownEvidence::from_join(
            self.requested_worker_count,
            startup_attempted,
            join,
            diagnostics.queued,
            diagnostics.running,
            diagnostics.queued.saturating_add(diagnostics.running),
            cumulative_failures,
        )
    }
}

impl Default for ProxyGenerationService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ProxyGenerationService {
    fn drop(&mut self) {
        self.begin_endurance_shutdown();
    }
}

/// Resolve the source-referred color identity used by proxy generation and lookup.
pub(crate) fn resolve_asset_proxy_color_contract(
    asset: &AssetRecord,
    input_color: &MediaInputColorContext,
) -> Result<ProxyColorContract, String> {
    let media_probe = asset
        .media_probe()
        .ok_or_else(|| "proxy generation requires media probe facts".to_owned())?;
    let detected = media_probe.primary_video().and_then(|video| video.executable_color_space());
    let resolution = input_color.missing_metadata_policy.resolve_asset_input_decision(
        None,
        asset.interpretation,
        detected,
        input_color.working_color_space,
    );
    let source_color_space = match resolution.resolved {
        ResolvedInputColor::Color(color_space) => color_space,
        ResolvedInputColor::Data => {
            return Err("proxy generation does not color-manage non-color data assets".to_owned());
        }
        ResolvedInputColor::Rejected => {
            return Err(format!(
                "proxy generation rejected source with unresolved color metadata (policy={:?})",
                input_color.missing_metadata_policy
            ));
        }
    };
    let video = media_probe
        .primary_video()
        .ok_or_else(|| "proxy generation requires a probed primary video stream".to_owned())?;
    let sampling = video.proven_sampling().ok_or_else(|| {
        "proxy generation requires proven, internally consistent video sampling facts".to_owned()
    })?;
    let source_range = resolve_decoded_video_range(asset.interpretation.range, video.color_range);
    ProxyColorContract::try_new(source_color_space, sampling.bit_depth, source_range)
        .map_err(|error| error.to_string())
}

/// Resolve a proxy contract from the active Sequence input policy and Project
/// color environment.
pub(crate) fn resolve_app_state_proxy_color_contract(
    state: &AppState,
    asset: &AssetRecord,
) -> Result<ProxyColorContract, String> {
    let sequence = state
        .active_sequence()
        .ok_or_else(|| "proxy generation requires an active sequence color context".to_owned())?;
    let input_color = sequence
        .settings
        .root_program_color_context(state.project_color_environment())
        .map_err(|error| format!("proxy generation color context is invalid: {error}"))?
        .media_input(sequence.settings.color.input.auto_tone_map_media);
    resolve_asset_proxy_color_contract(asset, &input_color)
}

fn proxy_generation_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|parallelism| proxy_generation_worker_count_for(parallelism.get()))
        .unwrap_or(1)
}

fn proxy_generation_worker_count_for(parallelism: usize) -> usize {
    parallelism.saturating_sub(2).clamp(1, MAX_PROXY_GENERATION_WORKERS)
}
