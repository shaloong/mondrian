//! Bounded, versioned Playback Evidence aggregation.

use crate::{
    ClockMaster, FrameDelivery, FrameDeliveryKind, FrameDemand, FrameDemandIdentity,
    MonotonicTimestamp, PlaybackEpoch, PlaybackSnapshot, PreviewResolutionScale, TransportState,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use thiserror::Error;

/// Current serialized Playback Evidence schema.
pub const PLAYBACK_EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// Bounded retention policy for one evidence collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackEvidenceConfig {
    /// Maximum detailed events retained in memory.
    pub event_capacity: usize,
    /// Maximum latency/drift samples retained per metric.
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
    /// Retained samples contributing to percentiles.
    pub count: u64,
    /// Nearest-rank 50th percentile.
    pub p50_us: u64,
    /// Nearest-rank 95th percentile.
    pub p95_us: u64,
    /// Nearest-rank 99th percentile.
    pub p99_us: u64,
    /// Maximum retained sample.
    pub max_us: u64,
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
    /// Terminal delivery totals.
    pub deliveries: PlaybackDeliveryCounts,
    /// Demand issue-to-terminal latency.
    pub demand_latency: PlaybackLatencySummary,
    /// Pointer-drag seek-to-ready latency.
    pub warm_seek_latency: PlaybackLatencySummary,
    /// Settled seek-to-ready latency.
    pub accurate_seek_latency: PlaybackLatencySummary,
    /// Absolute authoritative-clock versus accepted-delivery target distance.
    pub delivery_clock_drift: PlaybackLatencySummary,
    /// Missing output frames observed.
    pub audio_underrun_frames: u64,
    /// Sustained-underrun recoveries observed.
    pub audio_underrun_recoveries: u64,
    /// Detailed events currently retained.
    pub retained_event_count: usize,
    /// Old detailed events evicted by the memory budget.
    pub dropped_event_count: u64,
    /// Old metric samples evicted by the memory budget.
    pub dropped_sample_count: u64,
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

/// Deep Module aggregating bounded Playback Evidence from any Adapter.
pub struct PlaybackEvidenceCollector {
    config: PlaybackEvidenceConfig,
    events: VecDeque<PlaybackEvidenceEvent>,
    next_event_sequence: u64,
    dropped_event_count: u64,
    dropped_sample_count: u64,
    first_epoch: Option<PlaybackEpoch>,
    latest_epoch: Option<PlaybackEpoch>,
    last_observed_at: Option<MonotonicTimestamp>,
    last_state: Option<TransportState>,
    last_master: Option<Option<ClockMaster>>,
    last_scale: Option<PreviewResolutionScale>,
    snapshot_count: u64,
    demand_count: u64,
    superseded_demand_count: u64,
    seek_superseded_count: u64,
    clock_residency: PlaybackClockResidency,
    state_residency: PlaybackStateResidency,
    deliveries: PlaybackDeliveryCounts,
    active_demand: Option<ActiveDemand>,
    last_demand_identity: Option<FrameDemandIdentity>,
    pending_seek: Option<PendingSeek>,
    demand_latencies: VecDeque<u64>,
    warm_seek_latencies: VecDeque<u64>,
    accurate_seek_latencies: VecDeque<u64>,
    delivery_clock_drifts: VecDeque<u64>,
    audio_underrun_frames: u64,
    audio_underrun_recoveries: u64,
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
            dropped_event_count: 0,
            dropped_sample_count: 0,
            first_epoch: None,
            latest_epoch: None,
            last_observed_at: None,
            last_state: None,
            last_master: None,
            last_scale: None,
            snapshot_count: 0,
            demand_count: 0,
            superseded_demand_count: 0,
            seek_superseded_count: 0,
            clock_residency: PlaybackClockResidency::default(),
            state_residency: PlaybackStateResidency::default(),
            deliveries: PlaybackDeliveryCounts::default(),
            active_demand: None,
            last_demand_identity: None,
            pending_seek: None,
            demand_latencies: VecDeque::with_capacity(config.sample_capacity),
            warm_seek_latencies: VecDeque::with_capacity(config.sample_capacity),
            accurate_seek_latencies: VecDeque::with_capacity(config.sample_capacity),
            delivery_clock_drifts: VecDeque::with_capacity(config.sample_capacity),
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
        self.observe_demand(observed_at, epoch, demand);
        self.snapshot_count = self.snapshot_count.saturating_add(1);
        self.last_observed_at = Some(observed_at);
        self.last_state = Some(snapshot.state);
        self.last_master = Some(snapshot.clock_master);
        self.last_scale = Some(snapshot.preview_scale);
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

    /// Observe one terminal delivery and update latency/drift aggregates.
    pub fn observe_delivery(
        &mut self,
        observed_at: MonotonicTimestamp,
        snapshot: PlaybackSnapshot,
        delivery: FrameDelivery,
        accepted: bool,
    ) -> Result<(), PlaybackEvidenceError> {
        self.advance_time(observed_at)?;
        if accepted {
            increment_delivery(&mut self.deliveries, delivery.kind);
        } else {
            self.deliveries.rejected = self.deliveries.rejected.saturating_add(1);
        }
        self.push_event(
            observed_at,
            snapshot.epoch,
            PlaybackEvidenceEventKind::Delivery {
                sequence: delivery.demand_sequence.get(),
                target_frame: delivery.target_frame,
                kind: delivery.kind,
                accepted,
            },
        );
        let matching_active = self.active_demand.filter(|active| {
            active.identity.epoch == delivery.epoch
                && active.identity.quality_revision == delivery.quality_revision
                && active.identity.sequence == delivery.demand_sequence
        });
        if let Some(active) = matching_active {
            self.active_demand = None;
            let latency = elapsed_us(observed_at, active.issued_at);
            push_sample(
                &mut self.demand_latencies,
                latency,
                self.config.sample_capacity,
                &mut self.dropped_sample_count,
            );
        }
        if accepted
            && matches!(
                delivery.kind,
                FrameDeliveryKind::Ready | FrameDeliveryKind::Degraded
            )
        {
            let drift_us = frame_distance_us(
                snapshot.position.frame,
                delivery.target_frame,
                snapshot.position.time_base,
            );
            push_sample(
                &mut self.delivery_clock_drifts,
                drift_us,
                self.config.sample_capacity,
                &mut self.dropped_sample_count,
            );
            if let Some(seek) = self.pending_seek.filter(|seek| seek.epoch == delivery.epoch) {
                self.pending_seek = None;
                let latency = elapsed_us(observed_at, seek.started_at);
                let samples = match seek.kind {
                    PlaybackSeekKind::Warm => &mut self.warm_seek_latencies,
                    PlaybackSeekKind::Accurate => &mut self.accurate_seek_latencies,
                };
                push_sample(
                    samples,
                    latency,
                    self.config.sample_capacity,
                    &mut self.dropped_sample_count,
                );
                self.push_event(
                    observed_at,
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
            observed_duration_us: residency_total(self.clock_residency),
            snapshot_count: self.snapshot_count,
            demand_count: self.demand_count,
            superseded_demand_count: self.superseded_demand_count,
            seek_superseded_count: self.seek_superseded_count,
            clock_residency: self.clock_residency,
            state_residency: self.state_residency,
            deliveries: self.deliveries,
            demand_latency: summarize(&self.demand_latencies),
            warm_seek_latency: summarize(&self.warm_seek_latencies),
            accurate_seek_latency: summarize(&self.accurate_seek_latencies),
            delivery_clock_drift: summarize(&self.delivery_clock_drifts),
            audio_underrun_frames: self.audio_underrun_frames,
            audio_underrun_recoveries: self.audio_underrun_recoveries,
            retained_event_count: self.events.len(),
            dropped_event_count: self.dropped_event_count,
            dropped_sample_count: self.dropped_sample_count,
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
        let delta = elapsed_us(observed_at, last_at);
        match self.last_master.flatten() {
            Some(ClockMaster::AudioDevice) => {
                self.clock_residency.audio_device_us =
                    self.clock_residency.audio_device_us.saturating_add(delta)
            }
            Some(ClockMaster::Synthetic) => {
                self.clock_residency.synthetic_us =
                    self.clock_residency.synthetic_us.saturating_add(delta)
            }
            None => {
                self.clock_residency.none_us = self.clock_residency.none_us.saturating_add(delta)
            }
        }
        if let Some(state) = self.last_state {
            let target = match state {
                TransportState::Stopped => &mut self.state_residency.stopped_us,
                TransportState::Paused => &mut self.state_residency.paused_us,
                TransportState::Priming => &mut self.state_residency.priming_us,
                TransportState::Playing => &mut self.state_residency.playing_us,
                TransportState::Recovering => &mut self.state_residency.recovering_us,
                TransportState::Ended => &mut self.state_residency.ended_us,
                TransportState::Blocked => &mut self.state_residency.blocked_us,
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
            self.dropped_event_count = self.dropped_event_count.saturating_add(1);
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

fn push_sample(samples: &mut VecDeque<u64>, sample: u64, capacity: usize, dropped: &mut u64) {
    if samples.len() == capacity {
        samples.pop_front();
        *dropped = dropped.saturating_add(1);
    }
    samples.push_back(sample);
}

fn summarize(samples: &VecDeque<u64>) -> PlaybackLatencySummary {
    if samples.is_empty() {
        return PlaybackLatencySummary::default();
    }
    let mut sorted: Vec<_> = samples.iter().copied().collect();
    sorted.sort_unstable();
    PlaybackLatencySummary {
        count: sorted.len() as u64,
        p50_us: nearest_rank(&sorted, 50),
        p95_us: nearest_rank(&sorted, 95),
        p99_us: nearest_rank(&sorted, 99),
        max_us: sorted.last().copied().unwrap_or(0),
    }
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

fn frame_distance_us(current: i64, delivered: i64, time_base: mondrian_core::Rational) -> u64 {
    if time_base.num <= 0 || time_base.den <= 0 {
        return u64::MAX;
    }
    let frames = (current as i128).saturating_sub(delivered as i128).unsigned_abs();
    frames
        .saturating_mul(time_base.num as u128)
        .saturating_mul(1_000_000)
        .checked_div(time_base.den as u128)
        .unwrap_or(u128::MAX)
        .min(u64::MAX as u128) as u64
}

fn residency_total(value: PlaybackClockResidency) -> u64 {
    value
        .audio_device_us
        .saturating_add(value.synthetic_us)
        .saturating_add(value.none_us)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioDeviceClockObservation, FrameDemandSequence, PlaybackSnapshot};
    use mondrian_core::{FramePosition, Rational};
    use std::time::Duration;

    fn at(ms: u64) -> MonotonicTimestamp {
        MonotonicTimestamp::from_duration(Duration::from_millis(ms))
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

    #[test]
    fn aggregates_clock_delivery_seek_drift_and_underrun_evidence() {
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
        let delivery = FrameDelivery {
            epoch,
            quality_revision: 1,
            demand_sequence: FrameDemandSequence(1),
            target_frame: 0,
            kind: FrameDeliveryKind::Ready,
        };
        collector.observe_delivery(at(60), playing, delivery, true).unwrap();
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
        assert_eq!(report.delivery_clock_drift.max_us, 0);
        assert_eq!(report.audio_underrun_frames, 960);
        assert_eq!(report.audio_underrun_recoveries, 1);
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
        assert!(report.dropped_event_count >= 1);
        assert_eq!(
            collector.observe_snapshot(at(29), playing, None),
            Err(PlaybackEvidenceError::NonMonotonicTimestamp)
        );
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
            .observe_delivery(
                at(10),
                playing,
                FrameDelivery {
                    epoch,
                    quality_revision: 1,
                    demand_sequence: FrameDemandSequence(9),
                    target_frame: 4,
                    kind: FrameDeliveryKind::Ready,
                },
                true,
            )
            .unwrap();
        collector.observe_snapshot(at(20), playing, Some(current)).unwrap();

        let report = collector.report();
        assert_eq!(report.demand_count, 1);
        assert_eq!(report.demand_latency.count, 1);
        assert_eq!(report.superseded_demand_count, 0);
    }
}
