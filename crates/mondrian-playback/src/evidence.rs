//! Bounded, versioned Playback Evidence aggregation.

use crate::{
    timeline_position_ns_floor, ClockMaster, FrameDeliveryApplication, FrameDeliveryKind,
    FrameDemand, FrameDemandIdentity, MonotonicTimestamp, PlaybackEpoch, PlaybackSnapshot,
    PreviewResolutionScale, TransportState,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Duration;
use thiserror::Error;

/// Current serialized Playback Evidence schema.
pub const PLAYBACK_EVIDENCE_SCHEMA_VERSION: u32 = 4;

/// Bounded retention policy for one evidence collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackEvidenceConfig {
    /// Maximum detailed events retained in memory.
    pub event_capacity: usize,
    /// Maximum deterministic reservoir samples retained per metric.
    pub sample_capacity: usize,
}

impl Default for PlaybackEvidenceConfig {
    fn default() -> Self {
        Self { event_capacity: 4_096, sample_capacity: 4_096 }
    }
}

/// Evidence Interface validation error.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackEvidenceError {
    /// Event and sample capacities must be positive.
    #[error("playback evidence capacities must be positive")]
    InvalidCapacity,
    /// Runtime observations must be monotonic.
    #[error("playback evidence timestamp moved backwards")]
    NonMonotonicTimestamp,
    /// Engine-authenticated delivery phase evidence could not be represented.
    #[error("playback delivery phase evidence is invalid or overflowed")]
    InvalidDeliveryPhaseEvidence,
}

/// User-visible seek class used by latency gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackSeekKind {
    /// Pointer-drag/latest-wins seek where a nearby usable frame is preferred.
    Warm,
    /// Settled seek requiring the exact target semantics.
    Accurate,
}

/// Detailed bounded event kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaybackEvidenceEventKind {
    /// A different Playback Session epoch became current.
    EpochChanged { previous: Option<u64>, current: u64 },
    /// Transport State changed.
    StateChanged {
        from: TransportState,
        to: TransportState,
    },
    /// Clock Master changed.
    ClockMasterChanged {
        from: Option<ClockMaster>,
        to: Option<ClockMaster>,
    },
    /// Runtime preview scale changed.
    PreviewScaleChanged {
        from: PreviewResolutionScale,
        to: PreviewResolutionScale,
    },
    /// The active Clock Master advanced across one or more media-frame boundaries.
    ClockFramesAdvanced {
        /// Previously observed authoritative timeline frame.
        from_frame: i64,
        /// Newly observed authoritative timeline frame.
        to_frame: i64,
        /// Intermediate frame targets skipped between the two observations.
        skipped_intermediate_frames: u64,
    },
    /// A distinct current Frame Demand was observed.
    DemandIssued { sequence: u64, target_frame: i64 },
    /// One terminal Frame Delivery was accepted or rejected.
    Delivery {
        sequence: u64,
        target_frame: i64,
        kind: FrameDeliveryKind,
        accepted: bool,
    },
    /// A user seek began.
    SeekStarted { kind: PlaybackSeekKind },
    /// A seek reached an accepted Ready/Degraded target.
    SeekCompleted {
        kind: PlaybackSeekKind,
        latency_us: u64,
    },
    /// Audio callback starvation was observed.
    AudioUnderrun {
        delta_frames: u64,
        recovery_started: bool,
    },
}

/// One retained evidence event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackEvidenceEvent {
    /// Monotonic sequence in this collector lifetime.
    pub sequence: u64,
    /// Runtime microseconds since the Adapter origin.
    pub observed_at_us: u64,
    /// Playback epoch current for this event.
    pub epoch: u64,
    /// Typed event payload.
    pub kind: PlaybackEvidenceEventKind,
}

/// Stable percentile summary in microseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackLatencySummary {
    /// All observations contributing to the aggregate and exact maximum.
    pub count: u64,
    /// Reservoir samples contributing to percentile estimates.
    pub sampled_count: u64,
    /// Nearest-rank 50th percentile of the deterministic reservoir.
    pub p50_us: u64,
    /// Nearest-rank 95th percentile of the deterministic reservoir.
    pub p95_us: u64,
    /// Nearest-rank 99th percentile of the deterministic reservoir.
    pub p99_us: u64,
    /// Exact maximum across all observations, including unretained samples.
    pub max_us: u64,
}

/// Point, uncertainty, and conservative proven phase-error distributions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackClockPhaseErrorSummary {
    /// Absolute exact target-to-phase distance before uncertainty.
    pub point_error: PlaybackLatencySummary,
    /// Conservative Clock-phase uncertainty upper bound.
    pub uncertainty: PlaybackLatencySummary,
    /// Point error plus the Clock observation's uncertainty upper bound.
    pub proven_error: PlaybackLatencySummary,
}

/// Per-master phase evidence for accepted presentable Frame Deliveries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackDeliveryPhaseErrorReport {
    /// Deliveries completed while Audio Device Clock Master was authoritative.
    pub audio_device: PlaybackClockPhaseErrorSummary,
    /// Deliveries completed while Synthetic Clock Master was authoritative.
    pub synthetic: PlaybackClockPhaseErrorSummary,
    /// Accepted running Ready/Degraded deliveries missing an expected Clock phase.
    pub unproven_presentable: u64,
    /// Accepted still/seek deliveries for which no running Clock phase applies.
    pub phase_not_applicable: u64,
}

/// Clock Master residency accumulated between observations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackClockResidency {
    /// Time using Audio Device Clock Master.
    pub audio_device_us: u64,
    /// Time using Synthetic Clock Master.
    pub synthetic_us: u64,
    /// Time without an active Clock Master.
    pub none_us: u64,
}

/// Transport State residency accumulated between observations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackStateResidency {
    /// Time stopped.
    pub stopped_us: u64,
    /// Time paused.
    pub paused_us: u64,
    /// Time acquiring minimum readiness.
    pub priming_us: u64,
    /// Time in normal playback.
    pub playing_us: u64,
    /// Time under recovery policy.
    pub recovering_us: u64,
    /// Time at content end.
    pub ended_us: u64,
    /// Time blocked by correctness/capability policy.
    pub blocked_us: u64,
}

/// Terminal Frame Delivery counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackDeliveryCounts {
    /// Accepted on-time deliveries.
    pub ready: u64,
    /// Accepted late deliveries.
    pub late: u64,
    /// Accepted stale-visible deliveries.
    pub stale_available: u64,
    /// Accepted explicitly degraded deliveries.
    pub degraded: u64,
    /// Accepted correctness blockers.
    pub blocked: u64,
    /// Accepted cancellation outcomes.
    pub canceled: u64,
    /// Accepted unclassified failures.
    pub failed: u64,
    /// Stale, duplicate, or otherwise rejected terminal observations.
    pub rejected: u64,
}

/// Bounded aggregate evidence for Clock Master frame advancement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackClockFrameAdvanceCounts {
    /// Observations that advanced by exactly one media frame.
    pub single_frame_advances: u64,
    /// Observations that crossed two or more media-frame boundaries.
    pub multi_frame_advances: u64,
    /// Total media-frame boundaries crossed across all observations.
    pub advanced_frames: u64,
    /// Intermediate frame targets skipped by multi-frame observations.
    pub skipped_intermediate_frames: u64,
}

/// Versioned aggregate report shared by UI diagnostics and headless harnesses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackEvidenceReport {
    /// Serialized report schema.
    pub schema_version: u32,
    /// First epoch observed in this collector lifetime.
    pub first_epoch: Option<u64>,
    /// Most recent epoch observed.
    pub latest_epoch: Option<u64>,
    /// Total monotonic time covered by residency observations.
    pub observed_duration_us: u64,
    /// Snapshot observations accepted.
    pub snapshot_count: u64,
    /// Distinct Frame Demands observed.
    pub demand_count: u64,
    /// Active demands replaced before terminal delivery.
    pub superseded_demand_count: u64,
    /// Pending seeks replaced by a newer seek.
    pub seek_superseded_count: u64,
    /// Clock Master residency.
    pub clock_residency: PlaybackClockResidency,
    /// Transport State residency.
    pub state_residency: PlaybackStateResidency,
    /// Clock-driven single-frame and multi-frame advancement totals.
    pub clock_frame_advances: PlaybackClockFrameAdvanceCounts,
    /// Terminal delivery totals.
    pub deliveries: PlaybackDeliveryCounts,
    /// Demand issue-to-terminal latency.
    pub demand_latency: PlaybackLatencySummary,
    /// Pointer-drag seek-to-ready latency.
    pub warm_seek_latency: PlaybackLatencySummary,
    /// Settled seek-to-ready latency.
    pub accurate_seek_latency: PlaybackLatencySummary,
    /// Exact target versus completion-time Clock phase, partitioned by master.
    pub delivery_phase_error: PlaybackDeliveryPhaseErrorReport,
    /// Missing output frames observed.
    pub audio_underrun_frames: u64,
    /// Sustained-underrun recoveries observed.
    pub audio_underrun_recoveries: u64,
    /// Detailed events currently retained.
    pub retained_event_count: usize,
    /// Old detailed events intentionally evicted by the retention budget.
    pub evicted_event_count: u64,
    /// Bounded detailed event tail.
    pub events: Vec<PlaybackEvidenceEvent>,
}

#[derive(Debug, Clone, Copy)]
struct ActiveDemand {
    identity: FrameDemandIdentity,
    issued_at: MonotonicTimestamp,
}

#[derive(Debug, Clone, Copy)]
struct PendingSeek {
    epoch: PlaybackEpoch,
    kind: PlaybackSeekKind,
    started_at: MonotonicTimestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreparedDeliveryPhaseEvidence {
    NotPresentable,
    NotApplicable,
    Unproven,
    Proven {
        master: ClockMaster,
        point_error_us: u64,
        uncertainty_us: u64,
        proven_error_us: u64,
    },
}

/// Deep Module aggregating bounded Playback Evidence from any Adapter.
pub struct PlaybackEvidenceCollector {
    config: PlaybackEvidenceConfig,
    events: VecDeque<PlaybackEvidenceEvent>,
    next_event_sequence: u64,
    evicted_event_count: u64,
    first_epoch: Option<PlaybackEpoch>,
    latest_epoch: Option<PlaybackEpoch>,
    last_observed_at: Option<MonotonicTimestamp>,
    observed_duration: Duration,
    last_state: Option<TransportState>,
    last_master: Option<Option<ClockMaster>>,
    last_scale: Option<PreviewResolutionScale>,
    last_position: Option<(PlaybackEpoch, i64)>,
    snapshot_count: u64,
    demand_count: u64,
    superseded_demand_count: u64,
    seek_superseded_count: u64,
    clock_residency: PlaybackClockResidencyDuration,
    state_residency: PlaybackStateResidencyDuration,
    clock_frame_advances: PlaybackClockFrameAdvanceCounts,
    deliveries: PlaybackDeliveryCounts,
    active_demand: Option<ActiveDemand>,
    last_demand_identity: Option<FrameDemandIdentity>,
    pending_seek: Option<PendingSeek>,
    demand_latencies: BoundedMetric,
    warm_seek_latencies: BoundedMetric,
    accurate_seek_latencies: BoundedMetric,
    audio_device_point_errors: BoundedMetric,
    audio_device_uncertainties: BoundedMetric,
    audio_device_proven_errors: BoundedMetric,
    synthetic_point_errors: BoundedMetric,
    synthetic_uncertainties: BoundedMetric,
    synthetic_proven_errors: BoundedMetric,
    unproven_presentable_deliveries: u64,
    phase_not_applicable_deliveries: u64,
    audio_underrun_frames: u64,
    audio_underrun_recoveries: u64,
}

#[derive(Default)]
struct PlaybackClockResidencyDuration {
    audio_device: Duration,
    synthetic: Duration,
    none: Duration,
}

impl PlaybackClockResidencyDuration {
    fn report(&self) -> PlaybackClockResidency {
        PlaybackClockResidency {
            audio_device_us: duration_us(self.audio_device),
            synthetic_us: duration_us(self.synthetic),
            none_us: duration_us(self.none),
        }
    }
}

#[derive(Default)]
struct PlaybackStateResidencyDuration {
    stopped: Duration,
    paused: Duration,
    priming: Duration,
    playing: Duration,
    recovering: Duration,
    ended: Duration,
    blocked: Duration,
}

impl PlaybackStateResidencyDuration {
    fn report(&self) -> PlaybackStateResidency {
        PlaybackStateResidency {
            stopped_us: duration_us(self.stopped),
            paused_us: duration_us(self.paused),
            priming_us: duration_us(self.priming),
            playing_us: duration_us(self.playing),
            recovering_us: duration_us(self.recovering),
            ended_us: duration_us(self.ended),
            blocked_us: duration_us(self.blocked),
        }
    }
}

/// Constant-memory metric accumulator with exact population count/maximum and
/// deterministic whole-run percentile sampling.
struct BoundedMetric {
    capacity: usize,
    samples: Vec<u64>,
    count: u64,
    max: u64,
}

impl BoundedMetric {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            samples: Vec::with_capacity(capacity),
            count: 0,
            max: 0,
        }
    }

    fn observe(&mut self, sample: u64) {
        self.count = self.count.saturating_add(1);
        self.max = self.max.max(sample);
        if self.samples.len() < self.capacity {
            self.samples.push(sample);
            return;
        }

        // Algorithm R with a stable SplitMix64 draw keeps the reservoir
        // representative of the complete run without a runtime RNG or growth.
        let candidate = splitmix64(self.count) % self.count;
        if candidate < self.capacity as u64 {
            self.samples[candidate as usize] = sample;
        }
    }

    fn summary(&self) -> PlaybackLatencySummary {
        if self.count == 0 {
            return PlaybackLatencySummary::default();
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        PlaybackLatencySummary {
            count: self.count,
            sampled_count: sorted.len() as u64,
            p50_us: nearest_rank(&sorted, 50),
            p95_us: nearest_rank(&sorted, 95),
            p99_us: nearest_rank(&sorted, 99),
            max_us: self.max,
        }
    }
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

impl Default for PlaybackEvidenceCollector {
    fn default() -> Self {
        Self::from_validated_config(PlaybackEvidenceConfig::default())
    }
}

impl PlaybackEvidenceCollector {
    /// Create a collector with explicit bounded retention.
    pub fn new(config: PlaybackEvidenceConfig) -> Result<Self, PlaybackEvidenceError> {
        if config.event_capacity == 0 || config.sample_capacity == 0 {
            return Err(PlaybackEvidenceError::InvalidCapacity);
        }
        Ok(Self::from_validated_config(config))
    }

    fn from_validated_config(config: PlaybackEvidenceConfig) -> Self {
        Self {
            config,
            events: VecDeque::with_capacity(config.event_capacity),
            next_event_sequence: 1,
            evicted_event_count: 0,
            first_epoch: None,
            latest_epoch: None,
            last_observed_at: None,
            observed_duration: Duration::ZERO,
            last_state: None,
            last_master: None,
            last_scale: None,
            last_position: None,
            snapshot_count: 0,
            demand_count: 0,
            superseded_demand_count: 0,
            seek_superseded_count: 0,
            clock_residency: PlaybackClockResidencyDuration::default(),
            state_residency: PlaybackStateResidencyDuration::default(),
            clock_frame_advances: PlaybackClockFrameAdvanceCounts::default(),
            deliveries: PlaybackDeliveryCounts::default(),
            active_demand: None,
            last_demand_identity: None,
            pending_seek: None,
            demand_latencies: BoundedMetric::new(config.sample_capacity),
            warm_seek_latencies: BoundedMetric::new(config.sample_capacity),
            accurate_seek_latencies: BoundedMetric::new(config.sample_capacity),
            audio_device_point_errors: BoundedMetric::new(config.sample_capacity),
            audio_device_uncertainties: BoundedMetric::new(config.sample_capacity),
            audio_device_proven_errors: BoundedMetric::new(config.sample_capacity),
            synthetic_point_errors: BoundedMetric::new(config.sample_capacity),
            synthetic_uncertainties: BoundedMetric::new(config.sample_capacity),
            synthetic_proven_errors: BoundedMetric::new(config.sample_capacity),
            unproven_presentable_deliveries: 0,
            phase_not_applicable_deliveries: 0,
            audio_underrun_frames: 0,
            audio_underrun_recoveries: 0,
        }
    }

    /// Observe authoritative state and the current demand at one monotonic time.
    pub fn observe_snapshot(
        &mut self,
        observed_at: MonotonicTimestamp,
        snapshot: PlaybackSnapshot,
        demand: Option<FrameDemand>,
    ) -> Result<(), PlaybackEvidenceError> {
        self.advance_time(observed_at)?;
        let epoch = snapshot.epoch;
        if self.latest_epoch != Some(epoch) {
            self.push_event(
                observed_at,
                epoch,
                PlaybackEvidenceEventKind::EpochChanged {
                    previous: self.latest_epoch.map(PlaybackEpoch::get),
                    current: epoch.get(),
                },
            );
            self.first_epoch.get_or_insert(epoch);
            self.latest_epoch = Some(epoch);
        }
        if let Some(from) = self.last_state.filter(|from| *from != snapshot.state) {
            self.push_event(
                observed_at,
                epoch,
                PlaybackEvidenceEventKind::StateChanged { from, to: snapshot.state },
            );
        }
        if let Some(from) = self.last_master.filter(|from| *from != snapshot.clock_master) {
            self.push_event(
                observed_at,
                epoch,
                PlaybackEvidenceEventKind::ClockMasterChanged { from, to: snapshot.clock_master },
            );
        }
        if let Some(from) = self.last_scale.filter(|from| *from != snapshot.preview_scale) {
            self.push_event(
                observed_at,
                epoch,
                PlaybackEvidenceEventKind::PreviewScaleChanged { from, to: snapshot.preview_scale },
            );
        }
        self.observe_clock_frame_advance(observed_at, snapshot);
        self.observe_demand(observed_at, epoch, demand);
        self.snapshot_count = self.snapshot_count.saturating_add(1);
        self.last_observed_at = Some(observed_at);
        self.last_state = Some(snapshot.state);
        self.last_master = Some(snapshot.clock_master);
        self.last_scale = Some(snapshot.preview_scale);
        self.last_position = Some((snapshot.epoch, snapshot.position.frame));
        Ok(())
    }

    /// Start or supersede one warm/accurate seek measurement.
    pub fn begin_seek(
        &mut self,
        observed_at: MonotonicTimestamp,
        epoch: PlaybackEpoch,
        kind: PlaybackSeekKind,
    ) -> Result<(), PlaybackEvidenceError> {
        self.advance_time(observed_at)?;
        if self
            .pending_seek
            .replace(PendingSeek { epoch, kind, started_at: observed_at })
            .is_some()
        {
            self.seek_superseded_count = self.seek_superseded_count.saturating_add(1);
        }
        self.push_event(
            observed_at,
            epoch,
            PlaybackEvidenceEventKind::SeekStarted { kind },
        );
        Ok(())
    }

    /// Consume one Engine-authenticated delivery application.
    ///
    /// All fallible timestamp and exact phase calculations complete before any
    /// aggregate, event, demand, or seek state is changed.
    pub fn observe_delivery(
        &mut self,
        application: FrameDeliveryApplication,
    ) -> Result<(), PlaybackEvidenceError> {
        let delivery = application.delivery();
        let identity = delivery.identity();
        let completed_at = delivery.completed_at();
        let snapshot = application.snapshot();
        self.validate_timestamp(completed_at)?;
        let prepared_phase_evidence = prepare_delivery_phase_evidence(&application)?;

        self.advance_time(completed_at)?;
        if application.accepted() {
            increment_delivery(&mut self.deliveries, delivery.kind());
        } else {
            self.deliveries.rejected = self.deliveries.rejected.saturating_add(1);
        }
        self.push_event(
            completed_at,
            snapshot.epoch,
            PlaybackEvidenceEventKind::Delivery {
                sequence: identity.sequence.get(),
                target_frame: identity.target_frame,
                kind: delivery.kind(),
                accepted: application.accepted(),
            },
        );
        let matching_active = if application.accepted() {
            self.active_demand.filter(|active| {
                active.identity.epoch == identity.epoch
                    && active.identity.quality_revision == identity.quality_revision
                    && active.identity.sequence == identity.sequence
            })
        } else {
            None
        };
        if let Some(active) = matching_active {
            self.active_demand = None;
            let latency = elapsed_us(completed_at, active.issued_at);
            self.demand_latencies.observe(latency);
        }
        if application.accepted()
            && matches!(
                delivery.kind(),
                FrameDeliveryKind::Ready | FrameDeliveryKind::Degraded
            )
        {
            self.observe_prepared_phase_evidence(prepared_phase_evidence);
            if let Some(seek) = self.pending_seek.filter(|seek| seek.epoch == identity.epoch) {
                self.pending_seek = None;
                let latency = elapsed_us(completed_at, seek.started_at);
                let metric = match seek.kind {
                    PlaybackSeekKind::Warm => &mut self.warm_seek_latencies,
                    PlaybackSeekKind::Accurate => &mut self.accurate_seek_latencies,
                };
                metric.observe(latency);
                self.push_event(
                    completed_at,
                    snapshot.epoch,
                    PlaybackEvidenceEventKind::SeekCompleted {
                        kind: seek.kind,
                        latency_us: latency,
                    },
                );
            }
        }
        Ok(())
    }

    fn observe_prepared_phase_evidence(&mut self, prepared: PreparedDeliveryPhaseEvidence) {
        match prepared {
            PreparedDeliveryPhaseEvidence::NotPresentable => {}
            PreparedDeliveryPhaseEvidence::NotApplicable => {
                self.phase_not_applicable_deliveries =
                    self.phase_not_applicable_deliveries.saturating_add(1);
            }
            PreparedDeliveryPhaseEvidence::Unproven => {
                self.unproven_presentable_deliveries =
                    self.unproven_presentable_deliveries.saturating_add(1);
            }
            PreparedDeliveryPhaseEvidence::Proven {
                master,
                point_error_us,
                uncertainty_us,
                proven_error_us,
            } => {
                let (point, uncertainty, proven) = match master {
                    ClockMaster::AudioDevice => (
                        &mut self.audio_device_point_errors,
                        &mut self.audio_device_uncertainties,
                        &mut self.audio_device_proven_errors,
                    ),
                    ClockMaster::Synthetic => (
                        &mut self.synthetic_point_errors,
                        &mut self.synthetic_uncertainties,
                        &mut self.synthetic_proven_errors,
                    ),
                };
                point.observe(point_error_us);
                uncertainty.observe(uncertainty_us);
                proven.observe(proven_error_us);
            }
        }
    }

    /// Add missing audio frames and optionally one sustained-underrun recovery.
    pub fn observe_audio_underrun(
        &mut self,
        observed_at: MonotonicTimestamp,
        epoch: PlaybackEpoch,
        delta_frames: u64,
        recovery_started: bool,
    ) -> Result<(), PlaybackEvidenceError> {
        self.advance_time(observed_at)?;
        self.audio_underrun_frames = self.audio_underrun_frames.saturating_add(delta_frames);
        if recovery_started {
            self.audio_underrun_recoveries = self.audio_underrun_recoveries.saturating_add(1);
        }
        self.push_event(
            observed_at,
            epoch,
            PlaybackEvidenceEventKind::AudioUnderrun { delta_frames, recovery_started },
        );
        Ok(())
    }

    /// Build a stable aggregate plus bounded detailed event tail.
    pub fn report(&self) -> PlaybackEvidenceReport {
        PlaybackEvidenceReport {
            schema_version: PLAYBACK_EVIDENCE_SCHEMA_VERSION,
            first_epoch: self.first_epoch.map(PlaybackEpoch::get),
            latest_epoch: self.latest_epoch.map(PlaybackEpoch::get),
            observed_duration_us: duration_us(self.observed_duration),
            snapshot_count: self.snapshot_count,
            demand_count: self.demand_count,
            superseded_demand_count: self.superseded_demand_count,
            seek_superseded_count: self.seek_superseded_count,
            clock_residency: self.clock_residency.report(),
            state_residency: self.state_residency.report(),
            clock_frame_advances: self.clock_frame_advances,
            deliveries: self.deliveries,
            demand_latency: self.demand_latencies.summary(),
            warm_seek_latency: self.warm_seek_latencies.summary(),
            accurate_seek_latency: self.accurate_seek_latencies.summary(),
            delivery_phase_error: PlaybackDeliveryPhaseErrorReport {
                audio_device: PlaybackClockPhaseErrorSummary {
                    point_error: self.audio_device_point_errors.summary(),
                    uncertainty: self.audio_device_uncertainties.summary(),
                    proven_error: self.audio_device_proven_errors.summary(),
                },
                synthetic: PlaybackClockPhaseErrorSummary {
                    point_error: self.synthetic_point_errors.summary(),
                    uncertainty: self.synthetic_uncertainties.summary(),
                    proven_error: self.synthetic_proven_errors.summary(),
                },
                unproven_presentable: self.unproven_presentable_deliveries,
                phase_not_applicable: self.phase_not_applicable_deliveries,
            },
            audio_underrun_frames: self.audio_underrun_frames,
            audio_underrun_recoveries: self.audio_underrun_recoveries,
            retained_event_count: self.events.len(),
            evicted_event_count: self.evicted_event_count,
            events: self.events.iter().copied().collect(),
        }
    }

    /// Remove and return the currently retained detailed events.
    pub fn drain_events(&mut self) -> Vec<PlaybackEvidenceEvent> {
        self.events.drain(..).collect()
    }

    fn observe_demand(
        &mut self,
        observed_at: MonotonicTimestamp,
        epoch: PlaybackEpoch,
        demand: Option<FrameDemand>,
    ) {
        let Some(demand) = demand else {
            return;
        };
        let identity = demand.identity();
        if self.last_demand_identity == Some(identity) {
            return;
        }
        self.last_demand_identity = Some(identity);
        if self
            .active_demand
            .replace(ActiveDemand { identity, issued_at: observed_at })
            .is_some()
        {
            self.superseded_demand_count = self.superseded_demand_count.saturating_add(1);
        }
        self.demand_count = self.demand_count.saturating_add(1);
        self.push_event(
            observed_at,
            epoch,
            PlaybackEvidenceEventKind::DemandIssued {
                sequence: identity.sequence.get(),
                target_frame: identity.target_frame,
            },
        );
    }

    fn observe_clock_frame_advance(
        &mut self,
        observed_at: MonotonicTimestamp,
        snapshot: PlaybackSnapshot,
    ) {
        let Some((previous_epoch, previous_frame)) = self.last_position else {
            return;
        };
        let previous_clock_running = self.last_state.is_some_and(|state| {
            matches!(
                state,
                TransportState::Priming | TransportState::Playing | TransportState::Recovering
            )
        });
        if previous_epoch != snapshot.epoch
            || !previous_clock_running
            || matches!(
                snapshot.state,
                TransportState::Stopped | TransportState::Blocked
            )
            || snapshot.position.frame <= previous_frame
        {
            return;
        }
        let advanced_frames = (snapshot.position.frame as i128)
            .saturating_sub(previous_frame as i128)
            .min(u64::MAX as i128) as u64;
        let skipped_intermediate_frames = advanced_frames.saturating_sub(1);
        if skipped_intermediate_frames == 0 {
            self.clock_frame_advances.single_frame_advances =
                self.clock_frame_advances.single_frame_advances.saturating_add(1);
        } else {
            self.clock_frame_advances.multi_frame_advances =
                self.clock_frame_advances.multi_frame_advances.saturating_add(1);
        }
        self.clock_frame_advances.advanced_frames =
            self.clock_frame_advances.advanced_frames.saturating_add(advanced_frames);
        self.clock_frame_advances.skipped_intermediate_frames = self
            .clock_frame_advances
            .skipped_intermediate_frames
            .saturating_add(skipped_intermediate_frames);
        self.push_event(
            observed_at,
            snapshot.epoch,
            PlaybackEvidenceEventKind::ClockFramesAdvanced {
                from_frame: previous_frame,
                to_frame: snapshot.position.frame,
                skipped_intermediate_frames,
            },
        );
    }

    fn validate_timestamp(
        &self,
        observed_at: MonotonicTimestamp,
    ) -> Result<(), PlaybackEvidenceError> {
        if self.last_observed_at.is_some_and(|last| observed_at < last) {
            return Err(PlaybackEvidenceError::NonMonotonicTimestamp);
        }
        Ok(())
    }

    fn advance_time(
        &mut self,
        observed_at: MonotonicTimestamp,
    ) -> Result<(), PlaybackEvidenceError> {
        self.validate_timestamp(observed_at)?;
        self.accumulate_residency(observed_at);
        self.last_observed_at = Some(observed_at);
        Ok(())
    }

    fn accumulate_residency(&mut self, observed_at: MonotonicTimestamp) {
        let Some(last_at) = self.last_observed_at else {
            return;
        };
        let delta = observed_at
            .duration_since_origin()
            .saturating_sub(last_at.duration_since_origin());
        self.observed_duration = self.observed_duration.saturating_add(delta);
        match self.last_master.flatten() {
            Some(ClockMaster::AudioDevice) => {
                self.clock_residency.audio_device =
                    self.clock_residency.audio_device.saturating_add(delta)
            }
            Some(ClockMaster::Synthetic) => {
                self.clock_residency.synthetic =
                    self.clock_residency.synthetic.saturating_add(delta)
            }
            None => self.clock_residency.none = self.clock_residency.none.saturating_add(delta),
        }
        if let Some(state) = self.last_state {
            let target = match state {
                TransportState::Stopped => &mut self.state_residency.stopped,
                TransportState::Paused => &mut self.state_residency.paused,
                TransportState::Priming => &mut self.state_residency.priming,
                TransportState::Playing => &mut self.state_residency.playing,
                TransportState::Recovering => &mut self.state_residency.recovering,
                TransportState::Ended => &mut self.state_residency.ended,
                TransportState::Blocked => &mut self.state_residency.blocked,
            };
            *target = target.saturating_add(delta);
        }
    }

    fn push_event(
        &mut self,
        observed_at: MonotonicTimestamp,
        epoch: PlaybackEpoch,
        kind: PlaybackEvidenceEventKind,
    ) {
        if self.events.len() == self.config.event_capacity {
            self.events.pop_front();
            self.evicted_event_count = self.evicted_event_count.saturating_add(1);
        }
        self.events.push_back(PlaybackEvidenceEvent {
            sequence: self.next_event_sequence,
            observed_at_us: duration_us(observed_at.duration_since_origin()),
            epoch: epoch.get(),
            kind,
        });
        self.next_event_sequence = self.next_event_sequence.saturating_add(1);
    }
}

fn increment_delivery(counts: &mut PlaybackDeliveryCounts, kind: FrameDeliveryKind) {
    let target = match kind {
        FrameDeliveryKind::Ready => &mut counts.ready,
        FrameDeliveryKind::Late => &mut counts.late,
        FrameDeliveryKind::StaleAvailable => &mut counts.stale_available,
        FrameDeliveryKind::Degraded => &mut counts.degraded,
        FrameDeliveryKind::Blocked => &mut counts.blocked,
        FrameDeliveryKind::Canceled => &mut counts.canceled,
        FrameDeliveryKind::Failed => &mut counts.failed,
    };
    *target = target.saturating_add(1);
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    let rank = sorted.len().saturating_mul(percentile).saturating_add(99) / 100;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn elapsed_us(later: MonotonicTimestamp, earlier: MonotonicTimestamp) -> u64 {
    duration_us(later.duration_since_origin().saturating_sub(earlier.duration_since_origin()))
}

fn duration_us(value: std::time::Duration) -> u64 {
    value.as_micros().min(u64::MAX as u128) as u64
}

fn prepare_delivery_phase_evidence(
    application: &FrameDeliveryApplication,
) -> Result<PreparedDeliveryPhaseEvidence, PlaybackEvidenceError> {
    let delivery = application.delivery();
    if !application.accepted()
        || !matches!(
            delivery.kind(),
            FrameDeliveryKind::Ready | FrameDeliveryKind::Degraded
        )
    {
        return Ok(PreparedDeliveryPhaseEvidence::NotPresentable);
    }

    let target = application
        .target()
        .ok_or(PlaybackEvidenceError::InvalidDeliveryPhaseEvidence)?;
    let identity = delivery.identity();
    if target.frame != identity.target_frame {
        return Err(PlaybackEvidenceError::InvalidDeliveryPhaseEvidence);
    }
    let Some(phase) = application.clock_phase() else {
        return if matches!(
            application.snapshot().state,
            TransportState::Playing | TransportState::Recovering
        ) {
            Ok(PreparedDeliveryPhaseEvidence::Unproven)
        } else {
            Ok(PreparedDeliveryPhaseEvidence::NotApplicable)
        };
    };
    if phase.epoch() != identity.epoch
        || phase.observed_at() != delivery.completed_at()
        || application.snapshot().clock_master != Some(phase.master())
    {
        return Err(PlaybackEvidenceError::InvalidDeliveryPhaseEvidence);
    }

    let target_ns = timeline_position_ns_floor(target)
        .map_err(|_| PlaybackEvidenceError::InvalidDeliveryPhaseEvidence)?;
    let signed_point_error = phase
        .phase_ns()
        .checked_sub(target_ns)
        .ok_or(PlaybackEvidenceError::InvalidDeliveryPhaseEvidence)?;
    let point_error_ns = signed_point_error.unsigned_abs();
    let proven_error_ns = point_error_ns
        .checked_add(phase.uncertainty_ns())
        .ok_or(PlaybackEvidenceError::InvalidDeliveryPhaseEvidence)?;
    Ok(PreparedDeliveryPhaseEvidence::Proven {
        master: phase.master(),
        point_error_us: ns_to_us_ceil(point_error_ns)?,
        uncertainty_us: ns_to_us_ceil(phase.uncertainty_ns())?,
        proven_error_us: ns_to_us_ceil(proven_error_ns)?,
    })
}

fn ns_to_us_ceil(nanos: u128) -> Result<u64, PlaybackEvidenceError> {
    let quotient = nanos / 1_000;
    let rounded = quotient
        .checked_add(u128::from(!nanos.is_multiple_of(1_000)))
        .ok_or(PlaybackEvidenceError::InvalidDeliveryPhaseEvidence)?;
    u64::try_from(rounded).map_err(|_| PlaybackEvidenceError::InvalidDeliveryPhaseEvidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AudioDeviceClockObservation, FrameDeliveryCandidate, FrameDemandSequence,
        PlaybackClockPhaseObservation, PlaybackSnapshot,
    };
    use mondrian_core::{FramePosition, Rational};
    use std::time::Duration;

    fn at(ms: u64) -> MonotonicTimestamp {
        MonotonicTimestamp::from_duration(Duration::from_millis(ms))
    }

    fn at_ns(ns: u64) -> MonotonicTimestamp {
        MonotonicTimestamp::from_duration(Duration::from_nanos(ns))
    }

    fn snapshot(
        epoch: PlaybackEpoch,
        frame: i64,
        state: TransportState,
        master: Option<ClockMaster>,
    ) -> PlaybackSnapshot {
        PlaybackSnapshot {
            epoch,
            state,
            position: FramePosition::new(frame, Rational::new(1, 25)),
            clock_master: master,
            preview_scale: PreviewResolutionScale::Full,
            quality_revision: 1,
            audio_clock_observation: None::<AudioDeviceClockObservation>,
            audio_handoff: None,
        }
    }

    fn demand(epoch: PlaybackEpoch, sequence: u64, target_frame: i64) -> FrameDemand {
        FrameDemand {
            epoch,
            quality_revision: 1,
            sequence: FrameDemandSequence(sequence),
            sequence_id: None,
            timeline_revision: 1,
            target: FramePosition::new(target_frame, Rational::new(1, 25)),
            deadline: Some(at(500)),
            preview_scale: PreviewResolutionScale::Full,
        }
    }

    fn delivery_application(
        epoch: PlaybackEpoch,
        sequence: u64,
        target: FramePosition,
        completed_at: MonotonicTimestamp,
        master_and_phase: Option<(ClockMaster, i128, u128)>,
    ) -> FrameDeliveryApplication {
        delivery_application_in_state(
            epoch,
            sequence,
            target,
            completed_at,
            TransportState::Playing,
            master_and_phase,
        )
    }

    fn delivery_application_in_state(
        epoch: PlaybackEpoch,
        sequence: u64,
        target: FramePosition,
        completed_at: MonotonicTimestamp,
        state: TransportState,
        master_and_phase: Option<(ClockMaster, i128, u128)>,
    ) -> FrameDeliveryApplication {
        let identity = FrameDemandIdentity {
            epoch,
            quality_revision: 1,
            sequence: FrameDemandSequence(sequence),
            target_frame: target.frame,
        };
        let master = master_and_phase.map(|(master, _, _)| master);
        FrameDeliveryApplication {
            delivery: FrameDeliveryCandidate::for_demand(identity, FrameDeliveryKind::Ready)
                .complete_at(completed_at),
            accepted: true,
            snapshot: PlaybackSnapshot {
                epoch,
                state,
                position: target,
                clock_master: master,
                preview_scale: PreviewResolutionScale::Full,
                quality_revision: 1,
                audio_clock_observation: None,
                audio_handoff: None,
            },
            target: Some(target),
            clock_phase: master_and_phase.map(|(master, phase_ns, uncertainty_ns)| {
                PlaybackClockPhaseObservation {
                    epoch,
                    master,
                    observed_at: completed_at,
                    phase_ns,
                    uncertainty_ns,
                }
            }),
        }
    }

    #[test]
    fn aggregates_clock_delivery_seek_phase_and_underrun_evidence() {
        let epoch = PlaybackEpoch(7);
        let mut collector = PlaybackEvidenceCollector::new(PlaybackEvidenceConfig {
            event_capacity: 16,
            sample_capacity: 8,
        })
        .expect("collector");
        let playing = snapshot(
            epoch,
            0,
            TransportState::Playing,
            Some(ClockMaster::Synthetic),
        );
        collector.observe_snapshot(at(0), playing, Some(demand(epoch, 1, 0))).unwrap();
        collector.begin_seek(at(10), epoch, PlaybackSeekKind::Accurate).unwrap();
        collector
            .observe_delivery(delivery_application(
                epoch,
                1,
                FramePosition::new(0, Rational::new(1, 25)),
                at(60),
                Some((ClockMaster::Synthetic, 0, 0)),
            ))
            .unwrap();
        let audio = snapshot(
            epoch,
            1,
            TransportState::Playing,
            Some(ClockMaster::AudioDevice),
        );
        collector.observe_snapshot(at(100), audio, Some(demand(epoch, 2, 1))).unwrap();
        collector.observe_audio_underrun(at(110), epoch, 240, false).unwrap();
        collector.observe_audio_underrun(at(120), epoch, 720, true).unwrap();

        let report = collector.report();

        assert_eq!(report.schema_version, PLAYBACK_EVIDENCE_SCHEMA_VERSION);
        assert_eq!(report.clock_residency.synthetic_us, 100_000);
        assert_eq!(report.clock_residency.audio_device_us, 20_000);
        assert_eq!(report.deliveries.ready, 1);
        assert_eq!(report.demand_latency.p95_us, 60_000);
        assert_eq!(report.accurate_seek_latency.p95_us, 50_000);
        assert_eq!(report.delivery_phase_error.synthetic.proven_error.max_us, 0);
        assert_eq!(report.delivery_phase_error.synthetic.proven_error.count, 1);
        assert_eq!(report.delivery_phase_error.unproven_presentable, 0);
        assert_eq!(report.audio_underrun_frames, 960);
        assert_eq!(report.audio_underrun_recoveries, 1);
    }

    #[test]
    fn residency_keeps_sub_microsecond_deltas_until_report_quantization() {
        let epoch = PlaybackEpoch(8);
        let mut collector = PlaybackEvidenceCollector::default();
        let playing = snapshot(
            epoch,
            0,
            TransportState::Playing,
            Some(ClockMaster::Synthetic),
        );
        collector.observe_snapshot(at_ns(0), playing, None).unwrap();
        for sample in 1..=2_000 {
            collector.observe_snapshot(at_ns(sample * 500), playing, None).unwrap();
        }

        let report = collector.report();
        assert_eq!(report.observed_duration_us, 1_000);
        assert_eq!(report.clock_residency.synthetic_us, 1_000);
        assert_eq!(report.state_residency.playing_us, 1_000);
    }

    #[test]
    fn retention_is_bounded_and_non_monotonic_observations_are_rejected() {
        let epoch = PlaybackEpoch(1);
        let mut collector = PlaybackEvidenceCollector::new(PlaybackEvidenceConfig {
            event_capacity: 2,
            sample_capacity: 1,
        })
        .expect("collector");
        let playing = snapshot(
            epoch,
            0,
            TransportState::Playing,
            Some(ClockMaster::Synthetic),
        );
        collector.observe_snapshot(at(10), playing, Some(demand(epoch, 1, 0))).unwrap();
        collector.begin_seek(at(20), epoch, PlaybackSeekKind::Warm).unwrap();
        collector.observe_audio_underrun(at(30), epoch, 1, false).unwrap();

        let report = collector.report();
        assert_eq!(report.retained_event_count, 2);
        assert!(report.evicted_event_count >= 1);
        assert_eq!(
            collector.observe_snapshot(at(29), playing, None),
            Err(PlaybackEvidenceError::NonMonotonicTimestamp)
        );
    }

    #[test]
    fn distinguishes_single_frame_advancement_from_clock_skips() {
        let epoch = PlaybackEpoch(4);
        let mut collector = PlaybackEvidenceCollector::default();
        let playing = |frame| {
            snapshot(
                epoch,
                frame,
                TransportState::Playing,
                Some(ClockMaster::Synthetic),
            )
        };

        collector.observe_snapshot(at(0), playing(0), None).unwrap();
        collector.observe_snapshot(at(40), playing(1), None).unwrap();
        collector.observe_snapshot(at(160), playing(4), None).unwrap();
        collector
            .observe_snapshot(
                at(200),
                snapshot(
                    PlaybackEpoch(5),
                    20,
                    TransportState::Playing,
                    Some(ClockMaster::Synthetic),
                ),
                None,
            )
            .unwrap();

        let report = collector.report();
        assert_eq!(
            report.clock_frame_advances,
            PlaybackClockFrameAdvanceCounts {
                single_frame_advances: 1,
                multi_frame_advances: 1,
                advanced_frames: 4,
                skipped_intermediate_frames: 2,
            }
        );
        assert!(report.events.iter().any(|event| {
            event.kind
                == PlaybackEvidenceEventKind::ClockFramesAdvanced {
                    from_frame: 0,
                    to_frame: 1,
                    skipped_intermediate_frames: 0,
                }
        }));
        assert!(report.events.iter().any(|event| {
            event.kind
                == PlaybackEvidenceEventKind::ClockFramesAdvanced {
                    from_frame: 1,
                    to_frame: 4,
                    skipped_intermediate_frames: 2,
                }
        }));
    }

    #[test]
    fn metric_reservoir_is_bounded_while_population_max_remains_exact() {
        let mut metric = BoundedMetric::new(16);
        for sample in 1..=10_000 {
            metric.observe(sample);
        }

        let summary = metric.summary();
        assert_eq!(summary.count, 10_000);
        assert_eq!(summary.sampled_count, 16);
        assert_eq!(summary.max_us, 10_000);
        assert!(summary.p50_us > 0);
        assert!(summary.p99_us <= summary.max_us);
    }

    #[test]
    fn terminal_demand_is_not_reissued_by_a_later_snapshot() {
        let epoch = PlaybackEpoch(3);
        let mut collector = PlaybackEvidenceCollector::default();
        let playing = snapshot(
            epoch,
            4,
            TransportState::Playing,
            Some(ClockMaster::Synthetic),
        );
        let current = demand(epoch, 9, 4);
        collector.observe_snapshot(at(0), playing, Some(current)).unwrap();
        collector
            .observe_delivery(delivery_application(
                epoch,
                9,
                FramePosition::new(4, Rational::new(1, 25)),
                at(10),
                Some((ClockMaster::Synthetic, 160_000_000, 0)),
            ))
            .unwrap();
        collector.observe_snapshot(at(20), playing, Some(current)).unwrap();

        let report = collector.report();
        assert_eq!(report.demand_count, 1);
        assert_eq!(report.demand_latency.count, 1);
        assert_eq!(report.superseded_demand_count, 0);
    }

    #[test]
    fn phase_error_uses_ceil_microseconds_at_the_twenty_millisecond_boundary() {
        let epoch = PlaybackEpoch(10);
        let mut collector = PlaybackEvidenceCollector::default();
        for (sequence, completed_ns, phase_ns) in
            [(1, 1, 19_999_999), (2, 2, 20_000_000), (3, 3, 20_000_001)]
        {
            collector
                .observe_delivery(delivery_application(
                    epoch,
                    sequence,
                    FramePosition::new(0, Rational::new(1, 25)),
                    at_ns(completed_ns),
                    Some((ClockMaster::Synthetic, phase_ns, 0)),
                ))
                .expect("phase evidence");
        }

        let summary = collector.report().delivery_phase_error.synthetic.proven_error;
        assert_eq!(summary.count, 3);
        assert_eq!(summary.max_us, 20_001);
        assert_eq!(summary.p50_us, 20_000);
    }

    #[test]
    fn audio_phase_error_adds_uncertainty_before_threshold_quantization() {
        let epoch = PlaybackEpoch(11);
        let mut collector = PlaybackEvidenceCollector::default();
        collector
            .observe_delivery(delivery_application(
                epoch,
                1,
                FramePosition::new(0, Rational::new(1, 25)),
                at_ns(1),
                Some((ClockMaster::AudioDevice, 19_000_000, 1_000_001)),
            ))
            .expect("audio phase evidence");

        let summary = collector.report().delivery_phase_error.audio_device;
        assert_eq!(summary.point_error.max_us, 19_000);
        assert_eq!(summary.uncertainty.max_us, 1_001);
        assert_eq!(summary.proven_error.max_us, 20_001);
    }

    #[test]
    fn fractional_target_uses_the_same_floor_nanosecond_grid_as_clock_phase() {
        let epoch = PlaybackEpoch(12);
        let mut collector = PlaybackEvidenceCollector::default();
        collector
            .observe_delivery(delivery_application(
                epoch,
                1,
                FramePosition::new(1, Rational::new(1001, 60_000)),
                at_ns(1),
                Some((ClockMaster::Synthetic, 16_683_333, 0)),
            ))
            .expect("59.94 phase evidence");

        let summary = collector.report().delivery_phase_error.synthetic;
        assert_eq!(summary.point_error.max_us, 0);
        assert_eq!(summary.proven_error.max_us, 0);
    }

    #[test]
    fn phase_preflight_failure_is_transactional() {
        let epoch = PlaybackEpoch(13);
        let mut collector = PlaybackEvidenceCollector::default();
        let before = collector.report();
        let result = collector.observe_delivery(delivery_application(
            epoch,
            1,
            FramePosition::new(i64::MAX, Rational::new(i64::from(i32::MAX), 1)),
            at_ns(1),
            Some((ClockMaster::Synthetic, i128::MIN, 0)),
        ));

        assert_eq!(
            result,
            Err(PlaybackEvidenceError::InvalidDeliveryPhaseEvidence)
        );
        assert_eq!(collector.report(), before);
    }

    #[test]
    fn playing_delivery_without_clock_phase_is_reported_unproven() {
        let epoch = PlaybackEpoch(14);
        let mut collector = PlaybackEvidenceCollector::default();
        collector
            .observe_delivery(delivery_application(
                epoch,
                1,
                FramePosition::new(0, Rational::new(1, 25)),
                at_ns(1),
                None,
            ))
            .expect("unproven delivery");

        let report = collector.report();
        assert_eq!(report.delivery_phase_error.unproven_presentable, 1);
        assert_eq!(report.delivery_phase_error.phase_not_applicable, 0);
        assert_eq!(report.delivery_phase_error.synthetic.proven_error.count, 0);
        assert_eq!(
            report.delivery_phase_error.audio_device.proven_error.count,
            0
        );
    }

    #[test]
    fn paused_seeks_after_continuous_playback_are_phase_not_applicable() {
        let epoch = PlaybackEpoch(15);
        let mut collector = PlaybackEvidenceCollector::default();
        collector
            .observe_delivery(delivery_application(
                epoch,
                1,
                FramePosition::new(0, Rational::new(1, 25)),
                at_ns(1),
                Some((ClockMaster::Synthetic, 0, 0)),
            ))
            .expect("running delivery");

        for sequence in 2..=101 {
            collector
                .observe_delivery(delivery_application_in_state(
                    epoch,
                    sequence,
                    FramePosition::new(sequence as i64, Rational::new(1, 25)),
                    at_ns(sequence),
                    TransportState::Paused,
                    None,
                ))
                .expect("paused seek delivery");
        }

        let report = collector.report();
        assert_eq!(report.delivery_phase_error.synthetic.proven_error.count, 1);
        assert_eq!(report.delivery_phase_error.unproven_presentable, 0);
        assert_eq!(report.delivery_phase_error.phase_not_applicable, 100);
    }
}
