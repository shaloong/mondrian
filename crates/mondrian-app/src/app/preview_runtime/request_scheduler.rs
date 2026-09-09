//! Preview prefetch, preroll, adaptive-hint, and Broker-admission Adapter.

use super::*;
use crate::app::preview_access_mode::MediaPreviewRequestIntent;
use crate::app::preview_timeline_execution::collect_preview_timeline_media_demands_with_programs;
use mondrian_media::PreviewPlaybackDirection;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

const MEDIA_PREVIEW_COLD_ACTIVATION_LOOKAHEAD_US: u64 = 2_000_000;
const MEDIA_PREVIEW_COLD_ACTIVATION_MAX_FRAMES: i64 = 120;
// Retain the complete 250 ms prefix at the canonical 24 fps qualification
// rate. Four future frames allowed a real PlaybackCursor decode admitted at
// startup to finish only after frame six had become current, forcing the Audio
// clock to skip otherwise-ready presentation coordinates. Six is the temporal
// ceiling; aggregate frame planning reduces the accepted near prefix whenever
// current plus the complete (possibly multi-layer) cold activation would
// exceed the Standard eight-unit physical grant.
const MEDIA_PREVIEW_PREROLL_MEDIA_FRAMES: usize = 6;
const FUTURE_MEDIA_WINDOW_CACHE_CAPACITY: usize =
    MEDIA_PREVIEW_COLD_ACTIVATION_MAX_FRAMES as usize + 2;
const FUTURE_MEDIA_WINDOW_MAX_REUSES_PER_FRAME: u8 = 32;

const fn opposite_media_preview_worker_lane(
    lane: MediaPreviewWorkerLane,
) -> MediaPreviewWorkerLane {
    match lane {
        MediaPreviewWorkerLane::Playback => MediaPreviewWorkerLane::NonPlayback,
        MediaPreviewWorkerLane::NonPlayback
        | MediaPreviewWorkerLane::Still
        | MediaPreviewWorkerLane::Any => MediaPreviewWorkerLane::Playback,
    }
}

/// Exhaustive result of one App media-request admission attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MediaPreviewRequestAdmission {
    /// A new Broker payload and its physical work lease were admitted.
    Scheduled,
    /// Compatible queued or in-flight work already owns the request.
    ExistingWork,
    /// The physical frame became resident before work admission.
    AlreadyResident,
    /// Decoder-session residency is changing transport family.
    DeferredResidencyTransition,
    /// Aggregate physical media ownership is temporarily full.
    DeferredAggregateCapacity,
    /// Scheduler or realtime execution pressure deferred work.
    DeferredExecutionPressure,
    /// The fixed Priming horizon ended before speculative work could own a producer.
    ExpiredPrerollDeadline,
    /// The exact current Viewer closure exceeds its machine-class grant.
    BlockedCurrentDemand,
    /// Aggregate ownership is full without a producer that can publish retry.
    BlockedAggregateCapacity,
    /// The physical media source lacks immutable reuse identity.
    InvalidMediaIdentity,
    /// The request priority/access pair violated the scheduler contract.
    InvalidScheduling,
    /// The execution generation was superseded before Broker admission.
    ObsoleteGeneration,
    /// No media worker can accept the request.
    WorkerUnavailable,
}

impl MediaPreviewRequestAdmission {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::ExistingWork => "existing_work",
            Self::AlreadyResident => "already_resident",
            Self::DeferredResidencyTransition => "deferred_residency_transition",
            Self::DeferredAggregateCapacity => "deferred_aggregate_capacity",
            Self::DeferredExecutionPressure => "deferred_execution_pressure",
            Self::ExpiredPrerollDeadline => "expired_preroll_deadline",
            Self::BlockedCurrentDemand => "blocked_current_demand",
            Self::BlockedAggregateCapacity => "blocked_aggregate_capacity",
            Self::InvalidMediaIdentity => "invalid_media_identity",
            Self::InvalidScheduling => "invalid_scheduling",
            Self::ObsoleteGeneration => "obsolete_generation",
            Self::WorkerUnavailable => "worker_unavailable",
        }
    }
}

pub(super) fn aggregate_pressure_has_observable_retry_owner(
    preemption_requested: bool,
    scheduler: MediaPreviewSchedulerDiagnostics,
    queue: MediaPreviewJobQueueDiagnostics,
) -> bool {
    preemption_requested
        || scheduler.pending_requests > 0
        || queue.queued_jobs > 0
        || queue.in_flight_jobs > 0
        || queue.in_flight_completed_jobs > 0
}

enum MediaWorkReservationOutcome {
    Reserved(mondrian_playback::MediaWorkResourceLease),
    AlreadyResident,
    DeferredAggregatePressure,
    BlockedCurrentDemand,
    BlockedAggregateCapacity,
    InvalidMediaIdentity,
}

#[derive(Clone)]
struct PlannedFutureMediaRequest {
    key: MediaPreviewKey,
    reservation: MediaPreviewResidencyReservation,
    hardware_decode_request: PreviewHardwareDecodeRequest,
    hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FutureMediaWindowIdentity {
    authoring_session_id: AuthoringSessionId,
    author_generation: u64,
    root_sequence_id: SequenceId,
    root_sequence_revision: mondrian_core::SequenceRevision,
    effect_registry_revision: u64,
    runtime_scale: mondrian_playback::PreviewResolutionScale,
    target_resolution: Resolution,
    color_context: ProgramColorContext,
    asset_library_revision: u64,
    proxy_config: mondrian_media::ProxyConfig,
    hardware_admission: Option<PlaybackHardwareDecodeAdmission>,
}

#[derive(Clone)]
enum CachedFutureMediaFrame {
    Complete(Arc<[PlannedFutureMediaRequest]>),
    Unavailable,
}

#[derive(Clone)]
struct CachedFutureMediaFrameEntry {
    frame: i64,
    plan: CachedFutureMediaFrame,
    reuse_count: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FutureMediaColdActivationMemo {
    observed_from_frame: i64,
    activation_frame: Option<i64>,
}

struct FutureMediaColdActivationResidency {
    activation_frame: i64,
    frames: Vec<MediaPreviewFrame>,
}

#[derive(Default)]
struct FutureMediaSourceObservationMemo {
    by_path: HashMap<PathBuf, mondrian_media::MediaFileFingerprint>,
}

impl FutureMediaSourceObservationMemo {
    fn observe(&mut self, path: &std::path::Path) -> mondrian_media::MediaFileFingerprint {
        *self
            .by_path
            .entry(path.to_path_buf())
            .or_insert_with(|| mondrian_media::MediaFileFingerprint::capture(path))
    }

    fn observation_count(&self) -> usize {
        self.by_path.len()
    }
}

/// Small process-local cache for immutable future-frame media contracts.
///
/// Immutable lowering entries deliberately retain neither Frame Store
/// observations nor Broker, failure-memory, resource-headroom, or
/// proxy-generation state. Those facts are sampled again for every planning
/// attempt. One separately identified cold-activation dependency closure may
/// retain Frame Store leases until playback reaches that activation or the
/// complete window identity changes. This bounded owner prevents ordinary
/// rolling prefetch from evicting a decoder session that Priming already
/// proved ready.
pub(super) struct FutureMediaWindowCache {
    identity: Option<FutureMediaWindowIdentity>,
    frames: VecDeque<CachedFutureMediaFrameEntry>,
    cold_activation: Option<FutureMediaColdActivationMemo>,
    cold_activation_residency: Option<FutureMediaColdActivationResidency>,
    playback_source_worker_affinity:
        HashMap<mondrian_media::PreviewDecodeSource, MediaPreviewWorkerLane>,
    playback_source_worker_recency: VecDeque<mondrian_media::PreviewDecodeSource>,
    next_cold_activation_lane: MediaPreviewWorkerLane,
    cache_hits: u64,
    semantic_frame_evaluations: u64,
    media_request_lowerings: u64,
    identity_invalidations: u64,
    source_fingerprint_observations: u64,
}

impl Default for FutureMediaWindowCache {
    fn default() -> Self {
        Self {
            identity: None,
            frames: VecDeque::with_capacity(FUTURE_MEDIA_WINDOW_CACHE_CAPACITY),
            cold_activation: None,
            cold_activation_residency: None,
            playback_source_worker_affinity: HashMap::new(),
            playback_source_worker_recency: VecDeque::new(),
            next_cold_activation_lane: MediaPreviewWorkerLane::NonPlayback,
            cache_hits: 0,
            semantic_frame_evaluations: 0,
            media_request_lowerings: 0,
            identity_invalidations: 0,
            source_fingerprint_observations: 0,
        }
    }
}

impl FutureMediaWindowCache {
    fn activate(&mut self, identity: FutureMediaWindowIdentity) {
        if self.identity.as_ref() == Some(&identity) {
            return;
        }
        if self.identity.is_some() {
            self.identity_invalidations = self.identity_invalidations.saturating_add(1);
        }
        self.identity = Some(identity);
        self.frames.clear();
        self.cold_activation = None;
        self.cold_activation_residency = None;
        self.playback_source_worker_affinity.clear();
        self.playback_source_worker_recency.clear();
        self.next_cold_activation_lane = MediaPreviewWorkerLane::NonPlayback;
    }

    fn get(
        &mut self,
        identity: FutureMediaWindowIdentity,
        frame: i64,
    ) -> Option<CachedFutureMediaFrame> {
        self.activate(identity);
        let position = self.frames.iter().position(|entry| entry.frame == frame)?;
        let mut entry = self.frames.remove(position)?;
        if entry.reuse_count >= FUTURE_MEDIA_WINDOW_MAX_REUSES_PER_FRAME {
            return None;
        }
        entry.reuse_count = entry.reuse_count.saturating_add(1);
        let plan = entry.plan.clone();
        self.frames.push_back(entry);
        Some(plan)
    }

    fn insert(
        &mut self,
        identity: FutureMediaWindowIdentity,
        frame: i64,
        plan: CachedFutureMediaFrame,
    ) {
        self.activate(identity);
        if let Some(position) = self.frames.iter().position(|entry| entry.frame == frame) {
            self.frames.remove(position);
        }
        while self.frames.len() >= FUTURE_MEDIA_WINDOW_CACHE_CAPACITY {
            self.frames.pop_front();
        }
        self.frames
            .push_back(CachedFutureMediaFrameEntry { frame, plan, reuse_count: 0 });
    }

    fn remove(&mut self, frame: i64) {
        if let Some(position) = self.frames.iter().position(|entry| entry.frame == frame) {
            self.frames.remove(position);
        }
    }

    pub(super) fn cold_activation(
        &mut self,
        current_frame: i64,
        horizon_frame: i64,
    ) -> Option<Option<i64>> {
        let memo = self.cold_activation?;
        match memo.activation_frame {
            // Once the distinct source owns its exact residency, crossing the
            // ordinary prefetch-window edge must not turn that one activation
            // into a sliding frame-by-frame cold target. Retain it until
            // playback reaches the frame, the horizon no longer contains it,
            // or the complete semantic identity rotates.
            Some(frame) if current_frame < frame && frame <= horizon_frame => Some(Some(frame)),
            None if memo.observed_from_frame == current_frame => Some(None),
            Some(_) | None => {
                self.cold_activation = None;
                self.cold_activation_residency = None;
                None
            }
        }
    }

    fn remember_cold_activation(
        &mut self,
        observed_from_frame: i64,
        activation_frame: Option<i64>,
    ) {
        if self.cold_activation.and_then(|memo| memo.activation_frame) != activation_frame {
            self.cold_activation_residency = None;
        }
        self.cold_activation =
            Some(FutureMediaColdActivationMemo { observed_from_frame, activation_frame });
    }

    fn retain_cold_activation_residency(
        &mut self,
        activation_frame: i64,
        frames: Vec<MediaPreviewFrame>,
    ) {
        if frames.is_empty()
            || self.cold_activation.and_then(|memo| memo.activation_frame) != Some(activation_frame)
        {
            return;
        }
        self.cold_activation_residency =
            Some(FutureMediaColdActivationResidency { activation_frame, frames });
    }

    /// Bind newly discovered cold sources to the worker that will open them.
    ///
    /// Playback decoder Sessions are worker-local. The first current source
    /// owns the Playback lane; a distinct activation opens on the opposite
    /// lane and all later requests for that exact physical source retain the
    /// same affinity. Existing source ownership wins for persistent layers at
    /// an edit. The recency index bounds this ownership to the same capacity
    /// as the semantic lookahead cache.
    fn assign_cold_activation_worker_affinity(&mut self, requests: &[PlannedFutureMediaRequest]) {
        let existing_lane = requests.iter().find_map(|request| {
            let source = request.key.decode.source();
            (!source.is_still_image())
                .then(|| self.existing_playback_worker_affinity(source))
                .flatten()
        });
        let activation_lane = existing_lane
            .map(opposite_media_preview_worker_lane)
            .unwrap_or(self.next_cold_activation_lane);
        let mut assigned = false;
        for request in requests {
            let source = request.key.decode.source();
            if !source.is_still_image()
                && !self.playback_source_worker_affinity.contains_key(source)
            {
                self.remember_playback_worker_affinity(source.clone(), activation_lane);
                assigned = true;
            }
        }
        if assigned {
            self.next_cold_activation_lane = opposite_media_preview_worker_lane(activation_lane);
        }
    }

    pub(super) fn playback_worker_affinity(
        &mut self,
        source: &mondrian_media::PreviewDecodeSource,
    ) -> MediaPreviewWorkerLane {
        if source.is_still_image() {
            // Static files have no stateful playback cursor to preserve. Their
            // potentially expensive container/image probe must not replace or
            // occupy either moving-source decoder Session owner.
            return MediaPreviewWorkerLane::Still;
        }
        self.existing_playback_worker_affinity(source).unwrap_or_else(|| {
            self.remember_playback_worker_affinity(
                source.clone(),
                MediaPreviewWorkerLane::Playback,
            );
            MediaPreviewWorkerLane::Playback
        })
    }

    fn existing_playback_worker_affinity(
        &mut self,
        source: &mondrian_media::PreviewDecodeSource,
    ) -> Option<MediaPreviewWorkerLane> {
        let lane = self.playback_source_worker_affinity.get(source).copied()?;
        self.touch_playback_worker_affinity(source.clone());
        Some(lane)
    }

    pub(super) fn remember_playback_worker_affinity(
        &mut self,
        source: mondrian_media::PreviewDecodeSource,
        lane: MediaPreviewWorkerLane,
    ) {
        self.playback_source_worker_affinity.insert(source.clone(), lane);
        self.touch_playback_worker_affinity(source);
        while self.playback_source_worker_recency.len() > FUTURE_MEDIA_WINDOW_CACHE_CAPACITY {
            if let Some(expired) = self.playback_source_worker_recency.pop_front() {
                self.playback_source_worker_affinity.remove(&expired);
            }
        }
    }

    fn touch_playback_worker_affinity(&mut self, source: mondrian_media::PreviewDecodeSource) {
        if let Some(index) =
            self.playback_source_worker_recency.iter().position(|known| *known == source)
        {
            self.playback_source_worker_recency.remove(index);
        }
        self.playback_source_worker_recency.push_back(source);
    }

    /// Release only payload-owning lookahead residency.
    ///
    /// Immutable lowering plans and source-to-worker ownership are not decoded
    /// frame residency. Keeping them across an in-family resource trim prevents
    /// the same physical Playback source from reopening on another worker after
    /// its Frame Store entries are reclaimed.
    pub(super) fn release_decoded_residency(&mut self) {
        self.cold_activation_residency = None;
    }

    pub(super) fn clear(&mut self) {
        if self.identity.take().is_some()
            || !self.frames.is_empty()
            || self.cold_activation_residency.is_some()
            || !self.playback_source_worker_affinity.is_empty()
        {
            self.identity_invalidations = self.identity_invalidations.saturating_add(1);
        }
        self.frames.clear();
        self.cold_activation = None;
        self.cold_activation_residency = None;
        self.playback_source_worker_affinity.clear();
        self.playback_source_worker_recency.clear();
        self.next_cold_activation_lane = MediaPreviewWorkerLane::NonPlayback;
    }

    fn record_evaluation(&mut self, lowering_count: usize) {
        self.semantic_frame_evaluations = self.semantic_frame_evaluations.saturating_add(1);
        self.media_request_lowerings =
            self.media_request_lowerings.saturating_add(lowering_count as u64);
    }

    fn record_cache_hit(&mut self) {
        self.cache_hits = self.cache_hits.saturating_add(1);
    }

    fn record_source_fingerprint_observations(&mut self, observations: usize) {
        self.source_fingerprint_observations =
            self.source_fingerprint_observations.saturating_add(observations as u64);
    }

    pub(super) fn diagnostics(&self) -> PreviewFutureMediaWindowDiagnostics {
        PreviewFutureMediaWindowDiagnostics {
            cache_hits: self.cache_hits,
            semantic_frame_evaluations: self.semantic_frame_evaluations,
            media_request_lowerings: self.media_request_lowerings,
            identity_invalidations: self.identity_invalidations,
            source_fingerprint_observations: self.source_fingerprint_observations,
            retained_cold_activation_frame: self
                .cold_activation_residency
                .as_ref()
                .map(|residency| residency.activation_frame),
            retained_cold_activation_media_frames: self
                .cold_activation_residency
                .as_ref()
                .map_or(0, |residency| residency.frames.len()),
        }
    }
}

struct FutureMediaPrefixPlan {
    /// Physical guards that keep accepted near-term resident frames non-evictable
    /// until every planned missing request has transferred into Broker ownership.
    resident_guards: Vec<MediaPreviewFrame>,
    requests: Vec<PlannedFutureMediaRequest>,
    ready_media_frames: usize,
    preservable_media_frames: usize,
    window_proven_blank: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FutureMediaPrefetchReadiness {
    bounded_cold_activation_ready: bool,
    bounded_cold_activation_frame: Option<i64>,
}

impl FutureMediaPrefetchReadiness {
    const READY: Self = Self {
        bounded_cold_activation_ready: true,
        bounded_cold_activation_frame: None,
    };
    const PENDING: Self = Self {
        bounded_cold_activation_ready: false,
        bounded_cold_activation_frame: None,
    };
}

impl Default for FutureMediaPrefixPlan {
    fn default() -> Self {
        Self {
            resident_guards: Vec::new(),
            requests: Vec::new(),
            ready_media_frames: 0,
            preservable_media_frames: 0,
            window_proven_blank: true,
        }
    }
}

impl<O: Clone> PreviewProductionRuntime<O> {
    pub(super) fn consume_settled_aggregate_capacity_retry(&self) -> bool {
        if !self.media_aggregate_capacity_waiting.get()
            || aggregate_pressure_has_observable_retry_owner(
                false,
                self.scheduler.diagnostics(),
                self.jobs.diagnostics(),
            )
        {
            return false;
        }
        self.media_aggregate_capacity_waiting.set(false);
        true
    }

    /// Consume deferred media dependencies whose physical Broker owner has
    /// settled without leaving a usable current-candidate wakeup.
    pub(super) fn consume_settled_media_retry(&self) -> bool {
        let current_generation = self.execution.borrow().generation();
        let mut settled = false;
        let mut settled_keys = Vec::new();
        {
            self.media_existing_work_waiters.borrow_mut().retain(|key, binding| {
                if binding.generation != current_generation {
                    return false;
                }
                if self.scheduler.existing_work_binding_has_owner(key, *binding) {
                    true
                } else {
                    settled = true;
                    if !settled_keys.contains(key) {
                        settled_keys.push(key.clone());
                    }
                    false
                }
            });
        }
        let realtime_pressure_active = self.playback_realtime_work_pending();
        self.media_execution_pressure_waiters.borrow_mut().retain(|key, generation| {
            if *generation != current_generation {
                return false;
            }
            if realtime_pressure_active {
                true
            } else {
                settled = true;
                if !settled_keys.contains(key) {
                    settled_keys.push(key.clone());
                }
                false
            }
        });
        // A retained Timeline evaluation records the producer dependency that
        // justified returning Loading. If that producer settles without a
        // reusable result (cancellation, expiry, or cache-only completion),
        // the candidate retry must also remove the dependency level. Merely
        // waking the candidate leaves acquire_frame_evaluation() hitting the
        // old wait forever and prevents a replacement owner from being
        // admitted at the same exact frame.
        for key in settled_keys {
            self.invalidate_evaluations_for_media_key(&key);
        }
        if settled {
            self.media_retry_pending.set(true);
        }
        self.media_retry_pending.get()
    }

    /// Wake Window or Headless consumption when a candidate-local retry owner
    /// settles after Timeline evaluation but before Loading is returned. The
    /// waiter remains authoritative until the next poll consumes it, so the
    /// wake hint cannot become completion evidence by itself.
    pub(super) fn publish_media_retry_if_actionable(&self) {
        let current_generation = self.execution.borrow().generation();
        let existing_work_actionable =
            self.media_existing_work_waiters.borrow().iter().any(|(key, binding)| {
                binding.generation == current_generation
                    && !self.scheduler.existing_work_binding_has_owner(key, *binding)
            });
        let pressure_actionable = !self.playback_realtime_work_pending()
            && self
                .media_execution_pressure_waiters
                .borrow()
                .values()
                .any(|generation| *generation == current_generation);
        let actionable = existing_work_actionable || pressure_actionable;
        if actionable {
            self.work_notifier.retry_became_actionable();
        }
    }

    fn future_media_window_identity(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        sequence: &Sequence,
        runtime_scale: mondrian_playback::PreviewResolutionScale,
        target_resolution: Resolution,
        color_context: ProgramColorContext,
    ) -> Option<FutureMediaWindowIdentity> {
        let authoring = snapshot.authoring()?;
        let asset_library_revision = match authoring.asset_library().database_revision() {
            Ok(revision) => revision,
            Err(error) => {
                tracing::debug!(
                    sequence_id = %sequence.id,
                    reason = %error,
                    "future media window bypassed because Asset Library revision was unavailable"
                );
                return None;
            }
        };
        Some(FutureMediaWindowIdentity {
            authoring_session_id: authoring.session_id(),
            author_generation: authoring.author_generation(),
            root_sequence_id: sequence.id,
            root_sequence_revision: sequence.revision,
            effect_registry_revision: mondrian_effects::effect_registry_revision(),
            runtime_scale,
            target_resolution,
            color_context,
            asset_library_revision,
            proxy_config: authoring.proxy().config().clone(),
            hardware_admission: self.hardware_decode_admission.get().observation(),
        })
    }

    fn lower_future_media_frame(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        frame: i64,
        runtime_scale: mondrian_playback::PreviewResolutionScale,
        target_resolution: Resolution,
        color_context: ProgramColorContext,
    ) -> CachedFutureMediaFrame {
        let sequences = snapshot
            .authoring()
            .map(PreviewAuthoringSnapshot::sequences)
            .unwrap_or_default();
        let author_snapshot = snapshot
            .authoring()
            .map(PreviewAuthoringSnapshot::visual_author_snapshot_identity);
        let heterogeneous_graph_budget =
            self.heterogeneous_effect_decision.get().cpu_prefix_grant().graph_execution();
        let demands = match collect_preview_timeline_media_demands_with_programs(
            sequence,
            sequences,
            frame,
            target_resolution,
            runtime_scale,
            color_context,
            &self.visual_programs,
            &self.scratch,
            author_snapshot,
            heterogeneous_graph_budget,
        ) {
            Ok(demands) => demands,
            Err(_) => {
                self.future_media_window.borrow_mut().record_evaluation(0);
                return CachedFutureMediaFrame::Unavailable;
            }
        };
        let request_capacity = demands.len();
        let mut lowering_count = 0usize;
        let mut frame_keys = HashSet::new();
        let mut requests = Vec::with_capacity(request_capacity);
        for demand in demands {
            lowering_count = lowering_count.saturating_add(1);
            let key = match self.media_preview_key_for_timeline_request(
                snapshot,
                proxy_demands,
                &demand,
                crate::app::preview_quality::preview_representation_quality(runtime_scale),
                false,
                false,
            ) {
                Ok(key) => key,
                Err(_) => {
                    self.future_media_window.borrow_mut().record_evaluation(lowering_count);
                    return CachedFutureMediaFrame::Unavailable;
                }
            };
            if !frame_keys.insert(key.clone()) {
                continue;
            }
            let hardware_decode_request =
                self.hardware_decode_request_for_key(PreviewDecodeAccessMode::PlaybackCursor, &key);
            requests.push(PlannedFutureMediaRequest {
                reservation: media_preview_residency_reservation(&key, hardware_decode_request),
                key,
                hardware_decode_request,
                hardware_decode_device_selector: self
                    .hardware_decode_device_selector_for_access_mode(
                        PreviewDecodeAccessMode::PlaybackCursor,
                    ),
            });
        }
        self.future_media_window.borrow_mut().record_evaluation(lowering_count);
        CachedFutureMediaFrame::Complete(requests.into())
    }

    fn cached_future_media_frame(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        frame: i64,
        runtime_scale: mondrian_playback::PreviewResolutionScale,
        target_resolution: Resolution,
        color_context: ProgramColorContext,
        source_observations: &mut FutureMediaSourceObservationMemo,
    ) -> CachedFutureMediaFrame {
        let Some(identity) = self.future_media_window_identity(
            snapshot,
            sequence,
            runtime_scale,
            target_resolution,
            color_context.clone(),
        ) else {
            self.future_media_window.borrow_mut().clear();
            return self.lower_future_media_frame(
                snapshot,
                proxy_demands,
                sequence,
                frame,
                runtime_scale,
                target_resolution,
                color_context,
            );
        };
        let cached_plan = { self.future_media_window.borrow_mut().get(identity.clone(), frame) };
        if let Some(plan) = cached_plan {
            let observations_before = source_observations.observation_count();
            let sources_match = future_media_frame_sources_match(&plan, source_observations);
            let new_observations =
                source_observations.observation_count().saturating_sub(observations_before);
            self.future_media_window
                .borrow_mut()
                .record_source_fingerprint_observations(new_observations);
            if sources_match {
                self.future_media_window.borrow_mut().record_cache_hit();
                return plan;
            }
            self.future_media_window.borrow_mut().remove(frame);
        }
        let plan = self.lower_future_media_frame(
            snapshot,
            proxy_demands,
            sequence,
            frame,
            runtime_scale,
            target_resolution,
            color_context,
        );
        if matches!(plan, CachedFutureMediaFrame::Complete(_)) {
            self.future_media_window.borrow_mut().insert(identity, frame, plan.clone());
        }
        plan
    }

    pub(super) fn schedule_media_prefetches(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        frame: i64,
        target_width: u32,
        target_height: u32,
    ) -> FutureMediaPrefetchReadiness {
        self.schedule_media_prefetches_with_priming_current(
            snapshot,
            proxy_demands,
            sequence,
            frame,
            target_width,
            target_height,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn schedule_media_prefetches_with_priming_current(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        frame: i64,
        target_width: u32,
        target_height: u32,
        allow_resident_priming_current: bool,
    ) -> FutureMediaPrefetchReadiness {
        self.synchronize_visual_program_authoring_session(snapshot);
        let transport = snapshot.transport();
        if !transport.is_playing() {
            return FutureMediaPrefetchReadiness::READY;
        }
        if transport.is_speculative_preparation() {
            // A ticketless successor/lookahead request owns exactly one
            // prepared frame. Letting it recursively plan the ordinary future
            // window performs source revalidation and Timeline lowering on the
            // realtime coordinator without current-presentation authority.
            // The visible current/priming turn remains the sole recurring
            // prefetch owner.
            return FutureMediaPrefetchReadiness::READY;
        }
        let mut future_admission_allowed = transport.allows_future_media_admission(Instant::now());
        if self.playback_sustained_pressure_active() {
            bump(&self.metrics.playback_prefetch_skipped_sustained_pressure);
            return FutureMediaPrefetchReadiness::PENDING;
        }
        let worker_queue = self.jobs.diagnostics();
        if worker_queue.queued_current_jobs > 0 || worker_queue.in_flight_current_jobs > 0 {
            bump(&self.metrics.prefetch_skipped_current_work);
        }
        let prefetch_pressure = worker_queue
            .queued_prefetch_jobs
            .saturating_add(worker_queue.in_flight_prefetch_jobs);
        let prefetch_window_frames =
            media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate);
        self.record_playback_forward_prefetch_window(prefetch_window_frames);
        let Some(prefetch_window_frames) = prefetch_window_frames else {
            return FutureMediaPrefetchReadiness::READY;
        };
        let playback_direction = transport.playback_direction();
        let Some(authoring) = snapshot.authoring() else {
            return FutureMediaPrefetchReadiness::READY;
        };
        let author_snapshot = authoring.visual_author_snapshot_identity();
        let color_context =
            match sequence.settings.root_program_color_context(authoring.color_environment()) {
                Ok(context) => context,
                Err(error) => {
                    tracing::debug!(
                        sequence_id = %sequence.id,
                        reason = %error,
                        "skipping Preview prefetch for invalid Program color context"
                    );
                    return FutureMediaPrefetchReadiness::READY;
                }
            };
        let mut cold_activation_only = false;
        if !future_admission_allowed && transport.demand().is_some() {
            let current_base_frame = match playback_direction {
                PreviewPlaybackDirection::Forward => frame.saturating_sub(1),
                PreviewPlaybackDirection::Reverse => frame.saturating_add(1),
            };
            let current_plan = self.plan_future_media_prefix(
                snapshot,
                proxy_demands,
                sequence,
                current_base_frame,
                playback_direction,
                1,
                Some(frame),
                Resolution { width: target_width, height: target_height },
                color_context.clone(),
                0,
            );
            let current_media_closure_ready = current_plan.window_proven_blank
                || (current_plan.preservable_media_frames > 0
                    && current_plan.ready_media_frames == current_plan.preservable_media_frames);
            let current_media_closure_owned =
                current_plan.window_proven_blank || current_plan.preservable_media_frames > 0;
            if !allow_resident_priming_current || !current_media_closure_owned {
                // A pending Priming picture keeps absolute priority until its
                // complete physical media closure has reservation or residency
                // ownership. Future work cannot precede that exact closure.
                bump(&self.metrics.prefetch_skipped_current_pending);
                return FutureMediaPrefetchReadiness::PENDING;
            }
            future_admission_allowed = transport
                .priming_work_deadline()
                .is_some_and(|deadline| Instant::now() < deadline);
            cold_activation_only = !current_media_closure_ready;
        }
        // The dedicated preroll-observation seam may admit the complete
        // temporal prefix before Clock start. This recurring scheduler only
        // replenishes bounded physical work; granting it the full Priming
        // window would duplicate that authority and replace ready residency
        // with queued reservations.
        let physical_reservation_limit =
            media_preview_steady_prefetch_reservation_limit(prefetch_window_frames);
        let prefetch_slots_available = if future_admission_allowed {
            physical_reservation_limit.saturating_sub(prefetch_pressure)
        } else {
            0
        };
        if prefetch_slots_available == 0 && future_admission_allowed {
            bump(&self.metrics.prefetch_skipped_prefetch_backlog);
            return FutureMediaPrefetchReadiness::PENDING;
        }
        let preroll_deadline_at = transport.priming_work_deadline();
        let activation_horizon =
            media_preview_cold_activation_lookahead_frames(sequence.settings.frame_rate);
        let first_activation_frame = frame.saturating_add(prefetch_window_frames as i64);
        let horizon_frame = frame.saturating_add(activation_horizon);
        let cached_cold_activation = if playback_direction == PreviewPlaybackDirection::Forward {
            let identity = self.future_media_window_identity(
                snapshot,
                sequence,
                transport.runtime_scale(),
                Resolution { width: target_width, height: target_height },
                color_context.clone(),
            );
            if let Some(identity) = identity {
                let mut window = self.future_media_window.borrow_mut();
                window.activate(identity);
                window.cold_activation(frame, horizon_frame)
            } else {
                None
            }
        } else {
            Some(None)
        };
        if !future_admission_allowed && cached_cold_activation.is_none() {
            // An expired Priming grant may still observe the exact activation
            // it admitted earlier, but it cannot discover or admit new work.
            return FutureMediaPrefetchReadiness::PENDING;
        }
        // Discover the farther distinct source before planning admissions so
        // both the near window and the cold activation use one canonical
        // author snapshot. Playing nevertheless admits the immediate source
        // continuation first: a cold-source top-up must never occupy the sole
        // Playback worker ahead of the next presentation frame. Priming keeps
        // the atomic cold-activation-first rule after the exact current
        // closure is owned because its clock has not started yet.
        let distinct_activation_frame = match cached_cold_activation {
            Some(activation_frame) => activation_frame,
            None if prefetch_slots_available > 0
                && playback_direction == PreviewPlaybackDirection::Forward =>
            {
                let activation_frame = self.next_distinct_physical_source_frame(
                    snapshot,
                    proxy_demands,
                    sequence,
                    frame,
                    first_activation_frame,
                    horizon_frame,
                    Resolution { width: target_width, height: target_height },
                    color_context.clone(),
                );
                if activation_frame.is_some() {
                    self.future_media_window
                        .borrow_mut()
                        .remember_cold_activation(frame, activation_frame);
                }
                activation_frame
            }
            None => None,
        };
        let mut bounded_cold_activation_ready = true;
        let mut bounded_cold_activation_frame = distinct_activation_frame;
        let mut remaining_prefetch_jobs = prefetch_slots_available;
        let prioritize_immediate = !transport.is_priming() && !cold_activation_only;
        let mut immediate_window_proven_blank = false;
        if prioritize_immediate {
            let immediate_plan = self.plan_future_media_prefix(
                snapshot,
                proxy_demands,
                sequence,
                frame,
                playback_direction,
                1,
                (playback_direction == PreviewPlaybackDirection::Reverse).then_some(0),
                Resolution { width: target_width, height: target_height },
                color_context.clone(),
                remaining_prefetch_jobs,
            );
            immediate_window_proven_blank = immediate_plan.window_proven_blank;
            let immediate_scheduled = self.admit_future_media_prefix(
                immediate_plan,
                preroll_deadline_at,
                playback_direction,
            );
            remaining_prefetch_jobs = remaining_prefetch_jobs.saturating_sub(immediate_scheduled);
        }
        if let Some(activation_frame) = distinct_activation_frame {
            let mut activation_plan = self.plan_future_media_prefix(
                snapshot,
                proxy_demands,
                sequence,
                activation_frame.saturating_sub(1),
                PreviewPlaybackDirection::Forward,
                1,
                Some(activation_frame),
                Resolution { width: target_width, height: target_height },
                color_context.clone(),
                remaining_prefetch_jobs,
            );
            self.future_media_window
                .borrow_mut()
                .assign_cold_activation_worker_affinity(&activation_plan.requests);
            bounded_cold_activation_ready = activation_plan.preservable_media_frames > 0
                && activation_plan.ready_media_frames == activation_plan.preservable_media_frames;
            if bounded_cold_activation_ready {
                self.future_media_window.borrow_mut().retain_cold_activation_residency(
                    activation_frame,
                    std::mem::take(&mut activation_plan.resident_guards),
                );
            }
            let activation_scheduled = self.admit_future_media_prefix(
                activation_plan,
                preroll_deadline_at,
                PreviewPlaybackDirection::Forward,
            );
            remaining_prefetch_jobs = remaining_prefetch_jobs.saturating_sub(activation_scheduled);
        }

        let immediate_window_proven_blank = if cold_activation_only {
            remaining_prefetch_jobs = 0;
            false
        } else if prioritize_immediate {
            immediate_window_proven_blank
        } else {
            let immediate_plan = self.plan_future_media_prefix(
                snapshot,
                proxy_demands,
                sequence,
                frame,
                playback_direction,
                1,
                (playback_direction == PreviewPlaybackDirection::Reverse).then_some(0),
                Resolution { width: target_width, height: target_height },
                color_context.clone(),
                remaining_prefetch_jobs,
            );
            let window_proven_blank = immediate_plan.window_proven_blank;
            let immediate_scheduled = self.admit_future_media_prefix(
                immediate_plan,
                preroll_deadline_at,
                playback_direction,
            );
            remaining_prefetch_jobs = remaining_prefetch_jobs.saturating_sub(immediate_scheduled);
            window_proven_blank
        };

        // Priming has a separate preroll owner for the complete near prefix.
        // Once this scheduler has serviced the immediate frame and reserved
        // the discovered cold activation, admitting another rolling frame can
        // evict that activation before the preroll owner observes and pins its
        // completion. Playing retains the ordinary temporal-window top-up.
        let steady_admission_slots = if transport.is_priming() {
            0
        } else {
            remaining_prefetch_jobs
        };
        let steady_plan = self.plan_future_media_prefix(
            snapshot,
            proxy_demands,
            sequence,
            frame,
            playback_direction,
            prefetch_window_frames,
            (playback_direction == PreviewPlaybackDirection::Reverse).then_some(0),
            Resolution { width: target_width, height: target_height },
            color_context.clone(),
            steady_admission_slots,
        );
        let steady_window_proven_blank =
            immediate_window_proven_blank && steady_plan.window_proven_blank;
        let steady_scheduled =
            self.admit_future_media_prefix(steady_plan, preroll_deadline_at, playback_direction);
        remaining_prefetch_jobs = remaining_prefetch_jobs.saturating_sub(steady_scheduled);
        if remaining_prefetch_jobs > 0
            && distinct_activation_frame.is_none()
            && cached_cold_activation.is_none()
            && steady_window_proven_blank
            && playback_direction == PreviewPlaybackDirection::Forward
        {
            let after_frame = first_activation_frame;
            let activation_frame = {
                let mut programs = self.visual_programs.borrow_mut();
                mondrian_renderer::next_bound_prepared_visual_media_demand_frame(
                    sequence,
                    authoring.sequences(),
                    after_frame,
                    horizon_frame,
                    |candidate| {
                        programs
                            .bind_author_snapshot(author_snapshot, candidate)
                            .map_err(|error| error.to_string())
                    },
                )
            };
            match activation_frame {
                Ok(Some(activation_frame)) => {
                    bounded_cold_activation_frame = Some(activation_frame);
                    self.future_media_window
                        .borrow_mut()
                        .remember_cold_activation(frame, Some(activation_frame));
                    let mut activation_plan = self.plan_future_media_prefix(
                        snapshot,
                        proxy_demands,
                        sequence,
                        activation_frame.saturating_sub(1),
                        PreviewPlaybackDirection::Forward,
                        1,
                        Some(activation_frame),
                        Resolution { width: target_width, height: target_height },
                        color_context,
                        remaining_prefetch_jobs,
                    );
                    self.future_media_window
                        .borrow_mut()
                        .assign_cold_activation_worker_affinity(&activation_plan.requests);
                    bounded_cold_activation_ready = activation_plan.preservable_media_frames > 0
                        && activation_plan.ready_media_frames
                            == activation_plan.preservable_media_frames;
                    if bounded_cold_activation_ready {
                        self.future_media_window.borrow_mut().retain_cold_activation_residency(
                            activation_frame,
                            std::mem::take(&mut activation_plan.resident_guards),
                        );
                    }
                    self.admit_future_media_prefix(
                        activation_plan,
                        preroll_deadline_at,
                        PreviewPlaybackDirection::Forward,
                    );
                }
                Ok(None) => {
                    self.future_media_window.borrow_mut().remember_cold_activation(frame, None)
                }
                Err(error) => {
                    bounded_cold_activation_ready = false;
                    tracing::debug!(
                        sequence_id = %sequence.id,
                        after_frame,
                        horizon_frame,
                        reason = %error,
                        "canonical prepared visual range lookahead was unavailable"
                    );
                }
            }
        } else if distinct_activation_frame.is_none()
            && cached_cold_activation.is_none()
            && !steady_window_proven_blank
            && playback_direction == PreviewPlaybackDirection::Forward
        {
            self.future_media_window.borrow_mut().remember_cold_activation(frame, None);
        }
        FutureMediaPrefetchReadiness {
            bounded_cold_activation_ready,
            bounded_cold_activation_frame,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_bounded_cold_activation(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        current_frame: i64,
        target_width: u32,
        target_height: u32,
        color_context: ProgramColorContext,
    ) -> Option<FutureMediaPrefetchReadiness> {
        if snapshot.transport().playback_direction() != PreviewPlaybackDirection::Forward {
            return Some(FutureMediaPrefetchReadiness::READY);
        }
        let horizon_frame = current_frame.saturating_add(
            media_preview_cold_activation_lookahead_frames(sequence.settings.frame_rate),
        );
        let identity = self.future_media_window_identity(
            snapshot,
            sequence,
            snapshot.transport().runtime_scale(),
            Resolution { width: target_width, height: target_height },
            color_context.clone(),
        )?;
        let activation_frame = {
            let mut window = self.future_media_window.borrow_mut();
            window.activate(identity);
            window.cold_activation(current_frame, horizon_frame)?
        };
        let Some(activation_frame) = activation_frame else {
            return Some(FutureMediaPrefetchReadiness::READY);
        };
        let mut activation_plan = self.plan_future_media_prefix(
            snapshot,
            proxy_demands,
            sequence,
            activation_frame.saturating_sub(1),
            PreviewPlaybackDirection::Forward,
            1,
            Some(activation_frame),
            Resolution { width: target_width, height: target_height },
            color_context,
            0,
        );
        let bounded_cold_activation_ready = activation_plan.preservable_media_frames > 0
            && activation_plan.ready_media_frames == activation_plan.preservable_media_frames;
        if bounded_cold_activation_ready {
            self.future_media_window.borrow_mut().retain_cold_activation_residency(
                activation_frame,
                std::mem::take(&mut activation_plan.resident_guards),
            );
        }
        Some(FutureMediaPrefetchReadiness {
            bounded_cold_activation_ready,
            bounded_cold_activation_frame: Some(activation_frame),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn next_distinct_physical_source_frame(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        current_frame: i64,
        first_candidate_frame: i64,
        horizon_frame: i64,
        target_resolution: Resolution,
        color_context: ProgramColorContext,
    ) -> Option<i64> {
        if first_candidate_frame > horizon_frame {
            return None;
        }
        let authoring = snapshot.authoring()?;
        let runtime_scale = snapshot.transport().runtime_scale();
        let mut source_observations = FutureMediaSourceObservationMemo::default();
        let current_asset_ids = match self.cached_future_media_frame(
            snapshot,
            proxy_demands,
            sequence,
            current_frame,
            runtime_scale,
            target_resolution,
            color_context.clone(),
            &mut source_observations,
        ) {
            CachedFutureMediaFrame::Complete(requests) if !requests.is_empty() => {
                requests.iter().map(|request| request.key.asset_id).collect::<BTreeSet<_>>()
            }
            CachedFutureMediaFrame::Complete(_) | CachedFutureMediaFrame::Unavailable => {
                return None;
            }
        };
        let author_snapshot = authoring.visual_author_snapshot_identity();
        let result = {
            let mut programs = self.visual_programs.borrow_mut();
            mondrian_renderer::next_bound_prepared_visual_new_asset_frame(
                sequence,
                authoring.sequences(),
                first_candidate_frame.saturating_sub(1),
                horizon_frame,
                &current_asset_ids,
                |candidate| {
                    programs
                        .bind_author_snapshot(author_snapshot, candidate)
                        .map_err(|error| error.to_string())
                },
            )
        };
        match result {
            Ok(frame) => frame,
            Err(error) => {
                tracing::debug!(
                    sequence_id = %sequence.id,
                    first_candidate_frame,
                    horizon_frame,
                    reason = %error,
                    "canonical distinct-media activation query was unavailable"
                );
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_future_media_prefix(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        current_frame: i64,
        playback_direction: PreviewPlaybackDirection,
        max_future_frames: usize,
        terminal_frame: Option<i64>,
        target_resolution: Resolution,
        color_context: ProgramColorContext,
        max_new_jobs: usize,
    ) -> FutureMediaPrefixPlan {
        let mut plan = FutureMediaPrefixPlan::default();
        let mut accepted_keys = self.scheduler.pending_keys().into_iter().collect::<HashSet<_>>();
        let mut resident_keys = HashSet::new();
        let mut resident_recency_order = Vec::new();
        let mut ready_prefix = true;
        let mut remaining_jobs = max_new_jobs;
        let runtime_scale = snapshot.transport().runtime_scale();
        let mut source_observations = FutureMediaSourceObservationMemo::default();

        for offset in 1..=max_future_frames as i64 {
            let frame = match playback_direction {
                PreviewPlaybackDirection::Forward => current_frame.saturating_add(offset),
                PreviewPlaybackDirection::Reverse => current_frame.saturating_sub(offset),
            };
            let crossed_terminal = terminal_frame.is_some_and(|terminal_frame| {
                if playback_direction == PreviewPlaybackDirection::Forward {
                    frame > terminal_frame
                } else {
                    frame < terminal_frame
                }
            });
            if crossed_terminal {
                break;
            }
            let requests = match self.cached_future_media_frame(
                snapshot,
                proxy_demands,
                sequence,
                frame,
                runtime_scale,
                target_resolution,
                color_context.clone(),
                &mut source_observations,
            ) {
                CachedFutureMediaFrame::Complete(requests) => requests,
                CachedFutureMediaFrame::Unavailable => {
                    plan.window_proven_blank = false;
                    break;
                }
            };
            if requests.is_empty() {
                continue;
            }
            plan.window_proven_blank = false;

            let mut trial_resident_guards = Vec::new();
            let mut missing = Vec::new();
            let mut frame_ready = true;
            let mut frontier = false;
            for request in requests.iter() {
                let key = &request.key;
                if resident_keys.contains(key) {
                    continue;
                }
                if let Some(frame) = self.frame_store.borrow_mut().media_frame(key) {
                    accepted_keys.insert(key.clone());
                    resident_keys.insert(key.clone());
                    trial_resident_guards.push((key.clone(), frame));
                    continue;
                }
                if accepted_keys.contains(key) {
                    frame_ready = false;
                    continue;
                }
                frame_ready = false;
                if self.failed_media_key(key).is_some() {
                    frontier = true;
                    break;
                }
                missing.push(request.clone());
            }
            if frontier {
                break;
            }

            // Trial guards participate in headroom, but must not survive a
            // rejected farther frame: otherwise that frame could block a
            // previously accepted nearer missing request during admission.
            let mut headroom = self.frame_store.borrow().media_prefetch_headroom();
            let prior_requests_fit = plan.requests.iter().all(|request| {
                headroom.try_reserve_many(
                    request.reservation.entries,
                    request.reservation.cpu_bytes,
                    request.reservation.decoder_resource_units,
                )
            });
            if !prior_requests_fit {
                break;
            }
            let required_requests = missing.len();
            let mut frame_headroom = headroom;
            let complete_frame_fits = required_requests <= remaining_jobs
                && missing.iter().all(|request| {
                    frame_headroom.try_reserve_many(
                        request.reservation.entries,
                        request.reservation.cpu_bytes,
                        request.reservation.decoder_resource_units,
                    )
                });
            if !complete_frame_fits {
                // Never publish a partial physical closure. A multi-layer
                // native frame must acquire every surface before nearer work
                // can consume any of its grant.
                break;
            }
            for (key, guard) in trial_resident_guards {
                resident_recency_order.push(key);
                plan.resident_guards.push(guard);
            }
            for request in missing {
                accepted_keys.insert(request.key.clone());
                plan.requests.push(request);
            }
            remaining_jobs = remaining_jobs.saturating_sub(required_requests);
            plan.preservable_media_frames = plan.preservable_media_frames.saturating_add(1);
            ready_prefix &= frame_ready;
            if ready_prefix {
                plan.ready_media_frames = plan.ready_media_frames.saturating_add(1);
            }
        }
        // Near-to-far lookup necessarily touches the Store LRU in the wrong
        // direction. Re-touch the accepted prefix far-to-near so later
        // pressure prefers the nearest complete dependency closure.
        for key in resident_recency_order.iter().rev() {
            let _ = self.frame_store.borrow_mut().media_frame(key);
        }
        plan
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn future_media_prefix_keys_for_test(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        current_frame: i64,
        max_future_frames: usize,
        target_resolution: Resolution,
        color_context: ProgramColorContext,
    ) -> Vec<MediaPreviewKey> {
        let playback_direction = snapshot.transport().playback_direction();
        self.plan_future_media_prefix(
            snapshot,
            proxy_demands,
            sequence,
            current_frame,
            playback_direction,
            max_future_frames,
            (playback_direction == PreviewPlaybackDirection::Reverse).then_some(0),
            target_resolution,
            color_context,
            max_future_frames.saturating_mul(64).max(1),
        )
        .requests
        .into_iter()
        .map(|request| request.key)
        .collect()
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn future_media_frame_keys_at_scale_for_test(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
        sequence: &Sequence,
        frame: i64,
        runtime_scale: mondrian_playback::PreviewResolutionScale,
        target_resolution: Resolution,
        color_context: ProgramColorContext,
    ) -> Option<Vec<MediaPreviewKey>> {
        let mut source_observations = FutureMediaSourceObservationMemo::default();
        match self.cached_future_media_frame(
            snapshot,
            proxy_demands,
            sequence,
            frame,
            runtime_scale,
            target_resolution,
            color_context,
            &mut source_observations,
        ) {
            CachedFutureMediaFrame::Complete(requests) => {
                Some(requests.iter().map(|request| request.key.clone()).collect())
            }
            CachedFutureMediaFrame::Unavailable => None,
        }
    }

    #[cfg(test)]
    pub(super) fn clear_future_media_window_for_test(&self) {
        self.future_media_window.borrow_mut().clear();
    }

    fn admit_future_media_prefix(
        &self,
        plan: FutureMediaPrefixPlan,
        preroll_deadline_at: Option<Instant>,
        playback_direction: PreviewPlaybackDirection,
    ) -> usize {
        let _resident_guard_count = plan.resident_guards.len();
        if plan.requests.is_empty()
            || self.media_worker_health_failed()
            || !self.decode_residency.admits(PreviewDecodeAccessMode::PlaybackCursor)
        {
            return 0;
        }
        let batch = plan
            .requests
            .iter()
            .map(|request| (request.key.clone(), request.reservation))
            .collect::<Vec<_>>();
        let leases = match self
            .frame_store
            .borrow_mut()
            .reserve_media_prefetch_work_batch(&batch)
        {
            Ok(crate::app::preview_frame_store::MediaPrefetchBatchReservationAdmission::Reserved(
                leases,
            )) => leases,
            Ok(
                crate::app::preview_frame_store::MediaPrefetchBatchReservationAdmission::RetryPlan
                | crate::app::preview_frame_store::MediaPrefetchBatchReservationAdmission::RejectedAggregateCapacity,
            )
            | Err(_) => return 0,
        };
        let generation = self.execution.borrow().generation();
        let resource_scope = mondrian_playback::FrameWorkResourceScope::Media(
            mondrian_playback::MediaWorkReservationIntent::Prefetch,
        );
        let mut scheduled = 0usize;
        for (request, lease) in plan.requests.iter().zip(leases) {
            let worker_affinity = self
                .future_media_window
                .borrow_mut()
                .playback_worker_affinity(request.key.decode.source());
            let job = MediaPreviewJob {
                key: request.key.clone(),
                generation,
                priority: MediaPreviewRequestPriority::Prefetch,
                access_mode: PreviewDecodeAccessMode::PlaybackCursor,
                adaptive_hints: PreviewDecodeAdaptiveHints {
                    playback_direction,
                    ..PreviewDecodeAdaptiveHints::default()
                },
                hardware_decode_request: request.hardware_decode_request,
                hardware_decode_device_selector: request.hardware_decode_device_selector,
                enqueued_at: Instant::now(),
                deadline_at: preroll_deadline_at,
                demand_identity: None,
                execution_id: None,
                residency_work: Some(lease),
            };
            let submission = self.scheduler.submit_job(job, resource_scope, Some(worker_affinity));
            let admission = self.media_request_admission_from_submission(
                &request.key,
                generation,
                MediaPreviewRequestPriority::Prefetch,
                PreviewDecodeAccessMode::PlaybackCursor,
                submission,
            );
            match admission {
                MediaPreviewRequestAdmission::Scheduled => {
                    scheduled = scheduled.saturating_add(1);
                }
                MediaPreviewRequestAdmission::ExistingWork
                | MediaPreviewRequestAdmission::AlreadyResident => {}
                MediaPreviewRequestAdmission::DeferredResidencyTransition
                | MediaPreviewRequestAdmission::DeferredAggregateCapacity
                | MediaPreviewRequestAdmission::DeferredExecutionPressure
                | MediaPreviewRequestAdmission::ExpiredPrerollDeadline
                | MediaPreviewRequestAdmission::BlockedCurrentDemand
                | MediaPreviewRequestAdmission::BlockedAggregateCapacity
                | MediaPreviewRequestAdmission::InvalidMediaIdentity
                | MediaPreviewRequestAdmission::InvalidScheduling
                | MediaPreviewRequestAdmission::ObsoleteGeneration
                | MediaPreviewRequestAdmission::WorkerUnavailable => break,
            }
        }
        // `plan.resident_guards` intentionally stays alive until every
        // admission above either transferred into Broker ownership or stopped.
        scheduled
    }

    /// Maintain and inspect the bounded directional media window used by playback prefetch.
    ///
    /// This does not claim that a Viewer output is presented. During Priming it
    /// admits missing work only after the current demand is consumed. Before
    /// that point it observes already-resident future payloads without starting
    /// work ahead of the current picture or its execution generation. It reports only
    /// whether immediate future frames have media payloads and how much of the
    /// media-bearing prefix was resident at this observation; the Playback
    /// Engine separately requires current-frame presentation before releasing
    /// its clock anchor.
    pub(super) fn playback_video_preroll_readiness(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
        proxy_demands: &dyn PreviewProxyDemandSink,
    ) -> Option<PreviewVideoPreroll> {
        self.synchronize_visual_program_authoring_session(snapshot);
        let transport = snapshot.transport();
        if !transport.is_priming() {
            return None;
        }
        let authoring = snapshot.authoring()?;
        let sequence = authoring.active_sequence()?;
        let current_frame = transport.current_frame().max(0);
        let end_frame = authoring.last_content_frame()?.max(0);
        let playback_direction = transport.playback_direction();
        let at_terminal_boundary = match playback_direction {
            PreviewPlaybackDirection::Forward => current_frame >= end_frame,
            PreviewPlaybackDirection::Reverse => current_frame == 0,
        };
        if at_terminal_boundary {
            return Some(PreviewVideoPreroll {
                ready_media_frames: 0,
                preservable_media_frames: 0,
                bounded_cold_activation_ready: true,
                bounded_cold_activation_frame: None,
            });
        }
        let (width, height) = preview_dimensions_for_snapshot(snapshot, sequence);
        let color_context = sequence
            .settings
            .root_program_color_context(authoring.color_environment())
            .ok()?;
        let window = media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)?;
        let preroll_window = window.min(MEDIA_PREVIEW_PREROLL_MEDIA_FRAMES);
        let worker_queue = self.jobs.diagnostics();
        let prefetch_pressure = worker_queue
            .queued_prefetch_jobs
            .saturating_add(worker_queue.in_flight_prefetch_jobs);
        let can_admit = transport.allows_future_media_admission(Instant::now());
        // Resolve and, when already resident, retain the farther distinct
        // source before the near-prefix plan obtains its guards. Under a
        // saturated native surface grant the opposite order can let the one
        // missing near frame evict the just-completed cold activation, causing
        // both owners to chase the same last capacity unit forever.
        let observed_cold_activation = self.observe_bounded_cold_activation(
            snapshot,
            proxy_demands,
            sequence,
            current_frame,
            width,
            height,
            color_context.clone(),
        );
        let cold_activation = match observed_cold_activation {
            Some(readiness) if readiness.bounded_cold_activation_ready || !can_admit => readiness,
            // A resource trim releases the decoded payload while deliberately
            // retaining the immutable activation memo and worker affinity.
            // Reacquire Broker/Frame Store ownership for that known target
            // before the near-prefix admission can consume the bounded
            // Priming slots.
            Some(_) | None => self.schedule_media_prefetches_with_priming_current(
                snapshot,
                proxy_demands,
                sequence,
                current_frame,
                width,
                height,
                true,
            ),
        };
        let plan = self.plan_future_media_prefix(
            snapshot,
            proxy_demands,
            sequence,
            current_frame,
            playback_direction,
            preroll_window,
            Some(match playback_direction {
                PreviewPlaybackDirection::Forward => end_frame,
                PreviewPlaybackDirection::Reverse => 0,
            }),
            Resolution { width, height },
            color_context.clone(),
            if can_admit {
                preroll_window.saturating_sub(prefetch_pressure)
            } else {
                0
            },
        );
        let ready_media_frames = plan.ready_media_frames;
        let preservable_media_frames = plan.preservable_media_frames;
        let preroll_deadline_at = transport.priming_work_deadline();
        if can_admit {
            self.admit_future_media_prefix(plan, preroll_deadline_at, playback_direction);
        }
        Some(PreviewVideoPreroll {
            ready_media_frames,
            preservable_media_frames,
            bounded_cold_activation_ready: cold_activation.bounded_cold_activation_ready,
            bounded_cold_activation_frame: cold_activation.bounded_cold_activation_frame,
        })
    }

    pub(super) fn preview_decode_adaptive_hints(
        &self,
        access_mode: PreviewDecodeAccessMode,
        key: &MediaPreviewKey,
    ) -> PreviewDecodeAdaptiveHints {
        if access_mode != PreviewDecodeAccessMode::ScrubCursor {
            return PreviewDecodeAdaptiveHints::default();
        }
        let hints = self.scrub_adaptation.borrow_mut().observe_request(
            key.asset_id,
            key.source_sample().time(),
            Instant::now(),
        );
        match hints.scrub_class {
            PreviewScrubAdaptiveClass::Normal => {
                bump(&self.metrics.scrub_adaptive_normal_requests);
            }
            PreviewScrubAdaptiveClass::HotRegion => {
                bump(&self.metrics.scrub_adaptive_hot_region_requests);
            }
            PreviewScrubAdaptiveClass::SlowLatency => {
                bump(&self.metrics.scrub_adaptive_slow_latency_requests);
            }
            PreviewScrubAdaptiveClass::Recovery => {
                bump(&self.metrics.scrub_adaptive_recovery_requests);
            }
        }
        hints
    }

    pub(super) fn request_media_preview(
        &self,
        key: MediaPreviewKey,
        intent: MediaPreviewRequestIntent,
        access_mode: PreviewDecodeAccessMode,
        playback_current_deadline_at: Option<Instant>,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
        adaptive_hints: PreviewDecodeAdaptiveHints,
    ) -> MediaPreviewRequestAdmission {
        let hardware_decode_request = self.hardware_decode_request_for_key(access_mode, &key);
        let hardware_decode_device_selector =
            self.hardware_decode_device_selector_for_access_mode(access_mode);
        self.request_media_preview_with_hardware_policy(
            key,
            intent,
            access_mode,
            playback_current_deadline_at,
            demand_identity,
            adaptive_hints,
            hardware_decode_request,
            hardware_decode_device_selector,
        )
    }

    /// Requests a CPU-addressable working source for temporal Effect execution.
    ///
    /// Temporal source demands must never resolve to an opaque native surface:
    /// their complete generation-bound source set is converted to the declared
    /// working space before the Effect graph is allowed to execute.
    pub(super) fn request_cpu_working_media_preview(
        &self,
        key: MediaPreviewKey,
        intent: MediaPreviewRequestIntent,
        access_mode: PreviewDecodeAccessMode,
        playback_current_deadline_at: Option<Instant>,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
        adaptive_hints: PreviewDecodeAdaptiveHints,
    ) -> MediaPreviewRequestAdmission {
        self.request_media_preview_with_hardware_policy(
            key,
            intent,
            access_mode,
            playback_current_deadline_at,
            demand_identity,
            adaptive_hints,
            PreviewHardwareDecodeRequest::Auto,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn request_media_preview_with_hardware_policy(
        &self,
        key: MediaPreviewKey,
        intent: MediaPreviewRequestIntent,
        access_mode: PreviewDecodeAccessMode,
        playback_current_deadline_at: Option<Instant>,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
        adaptive_hints: PreviewDecodeAdaptiveHints,
        hardware_decode_request: PreviewHardwareDecodeRequest,
        hardware_decode_device_selector: Option<HwAccelDeviceSelector>,
    ) -> MediaPreviewRequestAdmission {
        if self.media_worker_health_failed() {
            return MediaPreviewRequestAdmission::WorkerUnavailable;
        }
        let priority = intent.priority();
        if priority == MediaPreviewRequestPriority::Prefetch
            && access_mode == PreviewDecodeAccessMode::PlaybackCursor
            && playback_current_deadline_at.is_some_and(|deadline| deadline <= Instant::now())
        {
            // Priming owns one immutable preparation horizon. Submitting work
            // after it expires gives the Broker an already-canceled payload
            // with no presentation authority, then invalidation can re-admit
            // the same payload forever. Terminate at admission instead.
            return MediaPreviewRequestAdmission::ExpiredPrerollDeadline;
        }
        if !self.decode_residency.admits(access_mode) {
            if priority == MediaPreviewRequestPriority::Current {
                // Register after the denied attempt, then recheck admission in
                // the poll seam. If the final acknowledgement raced this
                // store, the level check still publishes exactly one edge.
                self.decode_residency_waiting.set(Some(access_mode));
            }
            return MediaPreviewRequestAdmission::DeferredResidencyTransition;
        }
        if priority == MediaPreviewRequestPriority::Current
            && self.decode_residency_waiting.get() == Some(access_mode)
        {
            self.decode_residency_waiting.set(None);
            self.observed_decode_residency_retry_revision
                .set(self.decode_residency.actionable_retry_revision());
        }
        let generation = self.execution.borrow().generation();
        let is_current_playback = priority == MediaPreviewRequestPriority::Current
            && access_mode == PreviewDecodeAccessMode::PlaybackCursor;
        if is_current_playback {
            self.record_playback_current_deadline_budget(playback_deadline_remaining_us(
                playback_current_deadline_at,
            ));
        }
        let resource_scope =
            mondrian_playback::FrameWorkResourceScope::Media(intent.media_work_intent());
        let reservation = media_preview_residency_reservation(&key, hardware_decode_request);
        let mut job = MediaPreviewJob {
            key: key.clone(),
            generation,
            priority,
            access_mode,
            adaptive_hints,
            hardware_decode_request,
            hardware_decode_device_selector,
            enqueued_at: Instant::now(),
            deadline_at: if access_mode == PreviewDecodeAccessMode::PlaybackCursor {
                playback_current_deadline_at
            } else {
                None
            },
            demand_identity,
            execution_id: None,
            residency_work: None,
        };
        if let Some(submission) = self.scheduler.bind_existing_job(&job, resource_scope) {
            if priority == MediaPreviewRequestPriority::Current && submission.reused_existing_work()
            {
                // Register after the atomic rebind. The poll seam uses an
                // exact level predicate, so owner settlement racing this
                // store cannot lose the retry edge.
                self.media_existing_work_waiters.borrow_mut().insert(
                    key.clone(),
                    MediaPreviewExistingWorkBinding {
                        generation,
                        access_mode,
                        resource_scope,
                        demand_identity,
                    },
                );
                bump(&self.media_retry_waiter_registrations);
            }
            return self.media_request_admission_from_submission(
                &key,
                generation,
                priority,
                access_mode,
                submission,
            );
        }
        if is_current_playback
            && self.playback_sustained_pressure_active()
            && self.playback_realtime_work_pending()
            && !self.scheduler.has_pending_key(&key)
        {
            // An exact-key owner must continue below: it can either rebind
            // with the same physical scope or replace/preempt an incompatible
            // speculative scope using a Current reservation. Deferring that
            // case strands the sole Current demand behind its own prefetch,
            // with no pending Current owner or future retry edge.
            self.record_playback_current_sustained_pressure_skip();
            if self
                .media_execution_pressure_waiters
                .borrow_mut()
                .insert(key.clone(), generation)
                .is_none()
            {
                bump(&self.media_retry_waiter_registrations);
            }
            tracing::trace!(
                asset_id = %key.asset_id,
                source_sample = ?key.source_sample(),
                "viewer preview skipped new current playback decode while sustained pressure recovery has other realtime work pending"
            );
            // Close the registration/settlement race. If the last realtime
            // owner completed after the predicate above but before this store,
            // the level check publishes the retry immediately.
            self.publish_media_retry_if_actionable();
            return MediaPreviewRequestAdmission::DeferredExecutionPressure;
        }
        let residency_work = match self.reserve_media_work_for_request(&key, intent, reservation) {
            MediaWorkReservationOutcome::Reserved(lease) => lease,
            MediaWorkReservationOutcome::AlreadyResident => {
                return MediaPreviewRequestAdmission::AlreadyResident;
            }
            MediaWorkReservationOutcome::DeferredAggregatePressure => {
                if priority == MediaPreviewRequestPriority::Current {
                    self.media_aggregate_capacity_waiting.set(true);
                }
                return MediaPreviewRequestAdmission::DeferredAggregateCapacity;
            }
            MediaWorkReservationOutcome::BlockedCurrentDemand => {
                return MediaPreviewRequestAdmission::BlockedCurrentDemand;
            }
            MediaWorkReservationOutcome::BlockedAggregateCapacity => {
                return MediaPreviewRequestAdmission::BlockedAggregateCapacity;
            }
            MediaWorkReservationOutcome::InvalidMediaIdentity => {
                return MediaPreviewRequestAdmission::InvalidMediaIdentity;
            }
        };
        job.residency_work = Some(residency_work);
        let worker_affinity = (access_mode == PreviewDecodeAccessMode::PlaybackCursor).then(|| {
            self.future_media_window
                .borrow_mut()
                .playback_worker_affinity(key.decode.source())
        });
        let submission = self.scheduler.submit_job(job, resource_scope, worker_affinity);
        self.media_request_admission_from_submission(
            &key,
            generation,
            priority,
            access_mode,
            submission,
        )
    }

    fn media_request_admission_from_submission(
        &self,
        key: &MediaPreviewKey,
        generation: u64,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        submission: MediaPreviewRequestStatus,
    ) -> MediaPreviewRequestAdmission {
        match submission {
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch, evicted_still } => {
                tracing::debug!(
                    asset_id = %key.asset_id,
                    path = %key.decode.source().path().display(),
                    source_sample = ?key.source_sample(),
                    representation = ?key.decode.representation(),
                    generation, ?priority, ?access_mode,
                    "admitted Preview media request"
                );
                bump(&self.metrics.enqueued_jobs);
                if evicted_prefetch.is_some() {
                    bump(&self.metrics.queue_evicted_prefetch_jobs);
                    bump(&self.metrics.queue_canceled_jobs);
                }
                if evicted_still.is_some() {
                    bump(&self.metrics.queue_evicted_still_jobs);
                    bump(&self.metrics.queue_canceled_jobs);
                }
                MediaPreviewRequestAdmission::Scheduled
            }
            MediaPreviewRequestStatus::UpdatedQueued {
                priority_promoted,
                access_mode_changed: _,
                generation_changed: _,
            } => {
                if priority_promoted {
                    bump(&self.metrics.queue_promoted_current_jobs);
                }
                MediaPreviewRequestAdmission::ExistingWork
            }
            MediaPreviewRequestStatus::ReusedInFlight => MediaPreviewRequestAdmission::ExistingWork,
            #[cfg(test)]
            MediaPreviewRequestStatus::AlreadyPending { .. } => {
                MediaPreviewRequestAdmission::ExistingWork
            }
            MediaPreviewRequestStatus::DroppedObsoleteGeneration => {
                tracing::debug!(
                    asset_id = %key.asset_id,
                    source_sample = ?key.source_sample(),
                    generation,
                    "viewer preview request generation was already obsolete"
                );
                MediaPreviewRequestAdmission::ObsoleteGeneration
            }
            MediaPreviewRequestStatus::DroppedBackpressure => {
                bump(&self.metrics.queue_full_drops);
                tracing::trace!(
                    asset_id = %key.asset_id,
                    source_sample = ?key.source_sample(),
                    "viewer preview request dropped by backpressure"
                );
                if self.scheduler.diagnostics().pending_requests > 0 {
                    MediaPreviewRequestAdmission::DeferredExecutionPressure
                } else {
                    tracing::error!(
                        asset_id = %key.asset_id,
                        source_sample = ?key.source_sample(),
                        "Broker reported capacity pressure without a pending retry owner"
                    );
                    MediaPreviewRequestAdmission::InvalidScheduling
                }
            }
            MediaPreviewRequestStatus::DroppedInvalidAccessMode => {
                bump(&self.metrics.queue_invalid_access_mode_drops);
                tracing::warn!(
                    asset_id = %key.asset_id,
                    source_sample = ?key.source_sample(),
                    priority = ?priority,
                    access_mode = access_mode.as_str(),
                    "viewer preview request dropped because priority/access-mode pair is invalid"
                );
                MediaPreviewRequestAdmission::InvalidScheduling
            }
            MediaPreviewRequestStatus::Closed => {
                bump(&self.metrics.worker_disconnected_drops);
                tracing::debug!("viewer preview worker unavailable");
                MediaPreviewRequestAdmission::WorkerUnavailable
            }
        }
    }

    fn reserve_media_work_for_request(
        &self,
        key: &MediaPreviewKey,
        intent: MediaPreviewRequestIntent,
        reservation: MediaPreviewResidencyReservation,
    ) -> MediaWorkReservationOutcome {
        let priority = intent.priority();
        loop {
            let admission = self.frame_store.borrow_mut().reserve_media_work(
                key,
                intent.media_work_intent(),
                reservation.cpu_bytes,
                reservation.decoder_resource_units,
            );
            match admission {
                Ok(crate::app::preview_frame_store::MediaWorkReservationAdmission::Reserved(
                    lease,
                )) => {
                    return MediaWorkReservationOutcome::Reserved(lease);
                }
                Ok(crate::app::preview_frame_store::MediaWorkReservationAdmission::AlreadyResident) => {
                    return MediaWorkReservationOutcome::AlreadyResident;
                }
                Ok(
                    crate::app::preview_frame_store::MediaWorkReservationAdmission::RejectedAggregateCapacity,
                ) if priority == MediaPreviewRequestPriority::Current =>
                {
                    if let Some(preempted) = self.scheduler.cancel_one_queued_prefetch() {
                        bump(&self.metrics.queue_evicted_prefetch_jobs);
                        bump(&self.metrics.queue_canceled_jobs);
                        tracing::trace!(
                            asset_id = %preempted.asset_id,
                            source_sample = ?preempted.source_sample(),
                            "current Preview work reclaimed one queued Prefetch residency reservation"
                        );
                        continue;
                    }
                    let preemption_requested =
                        self.scheduler.request_one_in_flight_prefetch_preemption();
                    if preemption_requested {
                        tracing::trace!(
                            asset_id = %key.asset_id,
                            source_sample = ?key.source_sample(),
                            "current Preview work requested one in-flight Prefetch cooperative preemption"
                        );
                    }
                    // The in-flight payload still owns its physical lease.
                    // Completion notification will trigger the one typed retry
                    // after the worker actually releases that capacity.
                    return if aggregate_pressure_has_observable_retry_owner(
                        preemption_requested,
                        self.scheduler.diagnostics(),
                        self.jobs.diagnostics(),
                    ) {
                        MediaWorkReservationOutcome::DeferredAggregatePressure
                    } else {
                        MediaWorkReservationOutcome::BlockedAggregateCapacity
                    };
                }
                Ok(
                    crate::app::preview_frame_store::MediaWorkReservationAdmission::RejectedAggregateCapacity,
                ) => {
                    return MediaWorkReservationOutcome::DeferredAggregatePressure;
                }
                Ok(
                    crate::app::preview_frame_store::MediaWorkReservationAdmission::RejectedCurrentDemandGrant,
                ) => {
                    return MediaWorkReservationOutcome::BlockedCurrentDemand;
                }
                Err(
                    crate::app::preview_frame_store::MediaWorkReservationAdapterError::UnstableMediaIdentity,
                ) => {
                    return MediaWorkReservationOutcome::InvalidMediaIdentity;
                }
            }
        }
    }
}

fn future_media_frame_sources_match(
    plan: &CachedFutureMediaFrame,
    source_observations: &mut FutureMediaSourceObservationMemo,
) -> bool {
    match plan {
        CachedFutureMediaFrame::Complete(requests) => requests.iter().all(|request| {
            let source = request.key.decode.source();
            let observed = source_observations.observe(source.path());
            observed.authorizes_reuse() && observed == source.fingerprint()
        }),
        CachedFutureMediaFrame::Unavailable => false,
    }
}

fn media_preview_cold_activation_lookahead_frames(frame_rate: mondrian_core::Rational) -> i64 {
    let fps = frame_rate.to_f64();
    if !fps.is_finite() || fps <= 0.0 {
        return 0;
    }
    (((MEDIA_PREVIEW_COLD_ACTIVATION_LOOKAHEAD_US as f64 / 1_000_000.0) * fps).ceil() as i64)
        .clamp(1, MEDIA_PREVIEW_COLD_ACTIVATION_MAX_FRAMES)
}
