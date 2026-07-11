//! Headless realtime playback state, clock, and frame-delivery policy.
//!
//! This crate owns transport semantics but deliberately knows nothing about UI,
//! codecs, GPU resources, audio devices, or concrete timeline models.

use mondrian_core::{Rational, SequenceId, TimeCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

/// Monotonically increasing runtime timestamp relative to an arbitrary origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonotonicTimestamp(Duration);

impl MonotonicTimestamp {
    /// Timestamp at the chosen runtime origin.
    pub const ZERO: Self = Self(Duration::ZERO);

    /// Construct a timestamp from its duration since the runtime origin.
    pub const fn from_duration(value: Duration) -> Self {
        Self(value)
    }

    /// Return the duration since the runtime origin.
    pub const fn duration_since_origin(self) -> Duration {
        self.0
    }

    /// Add a bounded runtime duration.
    pub fn saturating_add(self, duration: Duration) -> Self {
        Self(self.0.saturating_add(duration))
    }

    fn checked_elapsed_since(self, earlier: Self) -> Result<Duration, PlaybackError> {
        self.0.checked_sub(earlier.0).ok_or(PlaybackError::NonMonotonicTimestamp)
    }
}

/// Identity of one contiguous playback run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlaybackEpoch(u64);

impl PlaybackEpoch {
    /// Return the stable numeric value used by reports and request identities.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one current-frame demand inside a Playback Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameDemandSequence(u64);

impl FrameDemandSequence {
    /// Return the stable numeric value used by reports and adapters.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Authoritative current-frame request emitted by the Playback Engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDemand {
    /// Playback Session identity.
    pub epoch: PlaybackEpoch,
    /// Runtime quality-policy revision.
    pub quality_revision: u64,
    /// Monotonic sequence unique within the Engine lifetime.
    pub sequence: FrameDemandSequence,
    /// Active sequence, when a project timeline is configured.
    pub sequence_id: Option<SequenceId>,
    /// Timeline semantic revision.
    pub timeline_revision: u64,
    /// Exact target timeline position.
    pub target: TimeCode,
    /// Latest useful presentation time for this demand.
    pub deadline: MonotonicTimestamp,
    /// Runtime-only spatial quality selected by recovery policy.
    pub preview_scale: PreviewResolutionScale,
}

/// Authoritative elapsed-media-time source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockMaster {
    /// Device-consumed audio sample position with explicit observation quality.
    AudioDevice,
    /// Runtime monotonic time anchored to exact timeline time.
    Synthetic,
}

/// User-visible transport condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportState {
    /// No clock is running and position is at the stop anchor.
    Stopped,
    /// Position is stable and exact still-frame work may run.
    Paused,
    /// Minimum audio/video readiness is being acquired with a bounded deadline.
    Priming,
    /// The Clock Master advances continuously.
    Playing,
    /// The clock continues while speculative work is suppressed and temporary
    /// preview-resolution recovery may be active.
    Recovering,
    /// The final content frame was reached.
    Ended,
    /// A correctness or required-capability condition forbids playback.
    Blocked,
}

/// Runtime-only preview resolution selected by recovery policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreviewResolutionScale {
    /// Full requested Viewer extent.
    Full,
    /// Half width and height.
    Half,
    /// Quarter width and height.
    Quarter,
}

impl PreviewResolutionScale {
    fn lower(self) -> Self {
        match self {
            Self::Full => Self::Half,
            Self::Half | Self::Quarter => Self::Quarter,
        }
    }
}

/// Stable reason a Frame Delivery did not produce an on-time current frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameDeliveryKind {
    /// Correct current frame completed before its deadline.
    Ready,
    /// Correct frame completed after its deadline.
    Late,
    /// A previous frame may remain visible but is not current readiness.
    StaleAvailable,
    /// Explicitly permitted temporary degradation was executed.
    Degraded,
    /// Correctness or capability policy forbids presentation.
    Blocked,
    /// Work was superseded or cooperatively canceled.
    Canceled,
    /// Execution failed without a policy blocker.
    Failed,
}

/// Terminal observation for a frame requested by the Playback Engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDelivery {
    /// Playback Session identity carried by the original demand.
    pub epoch: PlaybackEpoch,
    /// Quality-policy revision carried by the original demand.
    pub quality_revision: u64,
    /// Demand sequence carried by the original request.
    pub demand_sequence: FrameDemandSequence,
    /// Timeline frame requested by the demand.
    pub target_frame: i64,
    /// Terminal outcome.
    pub kind: FrameDeliveryKind,
}

/// Versioned policy values that determine transport and recovery behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackPolicy {
    /// Maximum startup/seek wait before Synthetic Clock Master continues.
    pub priming_limit: Duration,
    /// Maximum number of recent current deliveries considered for pressure.
    pub pressure_window: usize,
    /// Late/failed deliveries in the window required to enter recovery.
    pub pressure_threshold: usize,
    /// Number of consecutive healthy deliveries required to raise one scale.
    pub healthy_deliveries_to_recover: usize,
}

impl Default for PlaybackPolicy {
    fn default() -> Self {
        Self {
            priming_limit: Duration::from_millis(500),
            pressure_window: 12,
            pressure_threshold: 8,
            healthy_deliveries_to_recover: 60,
        }
    }
}

/// Read-only authoritative state published to app and UI adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackSnapshot {
    /// Playback Session identity.
    pub epoch: PlaybackEpoch,
    /// Current transport condition.
    pub state: TransportState,
    /// Current exact timeline position.
    pub position: TimeCode,
    /// Current Clock Master, if a clock is running or being primed.
    pub clock_master: Option<ClockMaster>,
    /// Runtime-only Viewer resolution scale.
    pub preview_scale: PreviewResolutionScale,
    /// Revision required on Frame Demands and Deliveries.
    pub quality_revision: u64,
}

/// Playback state-machine error. Invalid observations never mutate the Engine.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackError {
    /// Runtime timestamp moved backwards.
    #[error("playback timestamp must be monotonic")]
    NonMonotonicTimestamp,
    /// Timeline time base is zero or negative.
    #[error("timeline time base must be positive")]
    InvalidTimeBase,
    /// A delivery targeted a different timeline frame than the current demand.
    #[error("frame delivery does not match the active target")]
    MismatchedFrameDelivery,
    /// Recovery policy cannot form a bounded pressure/health window.
    #[error("playback policy is invalid")]
    InvalidPolicy,
}

#[derive(Debug, Clone, Copy)]
struct ClockAnchor {
    timeline: TimeCode,
    monotonic: MonotonicTimestamp,
}

/// Deep, headless Module owning a Playback Session and its realtime invariants.
pub struct PlaybackEngine {
    policy: PlaybackPolicy,
    sequence_id: Option<SequenceId>,
    timeline_revision: u64,
    epoch: PlaybackEpoch,
    state: TransportState,
    position: TimeCode,
    end_frame: i64,
    clock_master: Option<ClockMaster>,
    clock_anchor: ClockAnchor,
    last_timestamp: MonotonicTimestamp,
    preview_scale: PreviewResolutionScale,
    quality_revision: u64,
    recent_pressure: Vec<bool>,
    consecutive_healthy: usize,
    active_target_frame: Option<i64>,
    next_demand_sequence: u64,
    active_demand: Option<FrameDemand>,
    terminal_delivery: Option<(PlaybackEpoch, u64, FrameDemandSequence)>,
}

impl PlaybackEngine {
    /// Create a stopped engine at frame zero with a validated timeline time base.
    pub fn new(time_base: Rational, policy: PlaybackPolicy) -> Result<Self, PlaybackError> {
        validate_time_base(time_base)?;
        if policy.priming_limit.is_zero()
            || policy.pressure_window == 0
            || policy.pressure_threshold == 0
            || policy.pressure_threshold > policy.pressure_window
            || policy.healthy_deliveries_to_recover == 0
        {
            return Err(PlaybackError::InvalidPolicy);
        }
        Ok(Self::from_validated_time_base(time_base, policy))
    }

    fn from_validated_time_base(time_base: Rational, policy: PlaybackPolicy) -> Self {
        let position = TimeCode::new(0, time_base);
        Self {
            policy,
            sequence_id: None,
            timeline_revision: 0,
            epoch: PlaybackEpoch(0),
            state: TransportState::Stopped,
            position,
            end_frame: 0,
            clock_master: None,
            clock_anchor: ClockAnchor {
                timeline: position,
                monotonic: MonotonicTimestamp::ZERO,
            },
            last_timestamp: MonotonicTimestamp::ZERO,
            preview_scale: PreviewResolutionScale::Full,
            quality_revision: 0,
            recent_pressure: Vec::with_capacity(policy.pressure_window),
            consecutive_healthy: 0,
            active_target_frame: None,
            next_demand_sequence: 1,
            active_demand: None,
            terminal_delivery: None,
        }
    }

    /// Configure the timeline identity and reset transport to a stable position.
    pub fn reset_timeline(
        &mut self,
        sequence_id: Option<SequenceId>,
        timeline_revision: u64,
        position: TimeCode,
        end_frame: i64,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.accept_timestamp(now)?;
        validate_time_base(position.time_base)?;
        self.bump_epoch();
        self.sequence_id = sequence_id;
        self.timeline_revision = timeline_revision;
        self.position = nonnegative_frame(position);
        self.end_frame = end_frame.max(0);
        self.state = TransportState::Stopped;
        self.clock_master = None;
        self.reset_runtime_policy();
        self.reanchor(now);
        Ok(self.snapshot())
    }

    /// Start bounded priming at the current position.
    pub fn play(
        &mut self,
        end_frame: i64,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.accept_timestamp(now)?;
        self.end_frame = end_frame.max(0);
        if self.state == TransportState::Ended || self.position.frame > self.end_frame {
            self.position.frame = 0;
        }
        self.bump_epoch();
        self.state = TransportState::Priming;
        self.clock_master = Some(ClockMaster::Synthetic);
        self.active_target_frame = Some(self.position.frame);
        self.reset_runtime_policy();
        self.refresh_frame_demand_with_duration(now, self.policy.priming_limit)?;
        self.reanchor(now);
        Ok(self.snapshot())
    }

    /// Complete priming without changing the current timeline position.
    pub fn complete_priming(
        &mut self,
        master: ClockMaster,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.accept_timestamp(now)?;
        if self.state == TransportState::Priming {
            self.clock_master = Some(master);
            self.state = TransportState::Playing;
            self.reanchor(now);
            self.refresh_frame_demand(now)?;
        }
        Ok(self.snapshot())
    }

    /// Pause at the authoritative position observed at `now`.
    pub fn pause(&mut self, now: MonotonicTimestamp) -> Result<PlaybackSnapshot, PlaybackError> {
        self.advance_position(now)?;
        self.state = TransportState::Paused;
        self.clock_master = None;
        self.active_target_frame = Some(self.position.frame);
        self.reanchor(now);
        Ok(self.snapshot())
    }

    /// Stop and return to frame zero.
    pub fn stop(&mut self, now: MonotonicTimestamp) -> Result<PlaybackSnapshot, PlaybackError> {
        self.accept_timestamp(now)?;
        self.bump_epoch();
        self.position.frame = 0;
        self.state = TransportState::Stopped;
        self.clock_master = None;
        self.reset_runtime_policy();
        self.reanchor(now);
        Ok(self.snapshot())
    }

    /// Seek exactly and invalidate all prior epoch work.
    pub fn seek(
        &mut self,
        position: TimeCode,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.accept_timestamp(now)?;
        validate_time_base(position.time_base)?;
        let was_running = matches!(
            self.state,
            TransportState::Playing | TransportState::Recovering
        );
        self.bump_epoch();
        self.position = nonnegative_frame(position);
        self.state = if was_running {
            TransportState::Priming
        } else {
            TransportState::Paused
        };
        self.clock_master = was_running.then_some(ClockMaster::Synthetic);
        self.active_target_frame = Some(self.position.frame);
        self.reset_runtime_policy();
        self.reanchor(now);
        Ok(self.snapshot())
    }

    /// Advance the active Clock Master to `now` and publish the newest frame.
    pub fn tick(&mut self, now: MonotonicTimestamp) -> Result<PlaybackSnapshot, PlaybackError> {
        self.accept_timestamp(now)?;
        if self.state == TransportState::Priming {
            if let Some(deadline) = self
                .active_demand
                .map(|demand| demand.deadline)
                .filter(|deadline| now >= *deadline)
            {
                self.state = TransportState::Playing;
                self.clock_master = Some(ClockMaster::Synthetic);
                self.reanchor(deadline);
            }
        }
        self.advance_position(now)?;
        if matches!(
            self.state,
            TransportState::Playing | TransportState::Recovering
        ) {
            self.refresh_frame_demand_if_target_changed(now)?;
        }
        Ok(self.snapshot())
    }

    /// Return the exact remaining synthetic-clock duration to the next frame.
    ///
    /// Audio-device adapters may wake earlier when a new sample observation is
    /// available; this value is the bounded event-loop fallback.
    pub fn time_until_next_frame(
        &self,
        now: MonotonicTimestamp,
    ) -> Result<Option<Duration>, PlaybackError> {
        if !matches!(
            self.state,
            TransportState::Playing | TransportState::Recovering
        ) {
            return Ok(None);
        }
        let elapsed = now.checked_elapsed_since(self.clock_anchor.monotonic)?;
        let completed = elapsed_frames(elapsed, self.position.time_base)?.max(0) as u128;
        let next_frame = completed.saturating_add(1);
        let numerator = next_frame
            .saturating_mul(1_000_000_000)
            .saturating_mul(self.position.time_base.num as u128);
        let denominator = self.position.time_base.den as u128;
        let next_boundary_ns = numerator.saturating_add(denominator - 1) / denominator;
        let remaining_ns = next_boundary_ns.saturating_sub(elapsed.as_nanos());
        Ok(Some(Duration::from_nanos(
            remaining_ns.min(u64::MAX as u128) as u64,
        )))
    }

    /// Hand off from an unavailable audio device to a continuous synthetic clock.
    pub fn audio_device_lost(
        &mut self,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.advance_position(now)?;
        if matches!(
            self.state,
            TransportState::Playing | TransportState::Recovering
        ) {
            self.clock_master = Some(ClockMaster::Synthetic);
            self.reanchor(now);
        }
        Ok(self.snapshot())
    }

    /// Record a Frame Delivery and apply bounded recovery policy.
    ///
    /// Returns `Ok(false)` for a stale epoch or quality revision. Such a result
    /// is safe to diagnose but cannot mutate the current Playback Session.
    pub fn observe_frame_delivery(
        &mut self,
        delivery: FrameDelivery,
    ) -> Result<bool, PlaybackError> {
        if delivery.epoch != self.epoch || delivery.quality_revision != self.quality_revision {
            return Ok(false);
        }
        let identity = (
            delivery.epoch,
            delivery.quality_revision,
            delivery.demand_sequence,
        );
        if self.terminal_delivery == Some(identity) {
            return Ok(false);
        }
        if self
            .active_demand
            .is_none_or(|demand| demand.sequence != delivery.demand_sequence)
        {
            return Ok(false);
        }
        if self.active_target_frame.is_some_and(|target| target != delivery.target_frame) {
            return Err(PlaybackError::MismatchedFrameDelivery);
        }
        if delivery.kind == FrameDeliveryKind::Blocked {
            self.terminal_delivery = Some(identity);
            self.state = TransportState::Blocked;
            self.clock_master = None;
            return Ok(true);
        }

        let pressured = matches!(
            delivery.kind,
            FrameDeliveryKind::Late | FrameDeliveryKind::Failed
        );
        self.terminal_delivery = Some(identity);
        let healthy = matches!(
            delivery.kind,
            FrameDeliveryKind::Ready | FrameDeliveryKind::Degraded
        );
        if self.state == TransportState::Priming && healthy {
            self.state = TransportState::Playing;
            self.clock_master = Some(ClockMaster::Synthetic);
            self.reanchor(self.last_timestamp);
        }
        self.push_pressure(pressured);
        if healthy {
            self.consecutive_healthy = self.consecutive_healthy.saturating_add(1);
        } else if delivery.kind != FrameDeliveryKind::Canceled {
            self.consecutive_healthy = 0;
        }

        if self.pressure_count() >= self.policy.pressure_threshold
            && self.recent_pressure.len() == self.policy.pressure_window
            && matches!(
                self.state,
                TransportState::Playing | TransportState::Recovering
            )
        {
            self.state = TransportState::Recovering;
            let lowered = self.preview_scale.lower();
            if lowered != self.preview_scale {
                self.preview_scale = lowered;
                self.quality_revision = self.quality_revision.saturating_add(1);
                self.active_target_frame = Some(self.position.frame);
                self.refresh_frame_demand(self.last_timestamp)?;
            }
            self.recent_pressure.clear();
        } else if self.state == TransportState::Recovering
            && self.consecutive_healthy >= self.policy.healthy_deliveries_to_recover
        {
            self.preview_scale = match self.preview_scale {
                PreviewResolutionScale::Quarter => PreviewResolutionScale::Half,
                PreviewResolutionScale::Half | PreviewResolutionScale::Full => {
                    PreviewResolutionScale::Full
                }
            };
            self.quality_revision = self.quality_revision.saturating_add(1);
            self.refresh_frame_demand(self.last_timestamp)?;
            self.consecutive_healthy = 0;
            if self.preview_scale == PreviewResolutionScale::Full {
                self.state = TransportState::Playing;
            }
        }
        Ok(true)
    }

    /// Return the authoritative read-only snapshot.
    pub const fn snapshot(&self) -> PlaybackSnapshot {
        PlaybackSnapshot {
            epoch: self.epoch,
            state: self.state,
            position: self.position,
            clock_master: self.clock_master,
            preview_scale: self.preview_scale,
            quality_revision: self.quality_revision,
        }
    }

    /// Return the current demand that preview adapters must carry end-to-end.
    pub const fn frame_demand(&self) -> Option<FrameDemand> {
        self.active_demand
    }

    /// Timeline revision currently associated with the session.
    pub const fn timeline_revision(&self) -> u64 {
        self.timeline_revision
    }

    /// Sequence currently associated with the session.
    pub const fn sequence_id(&self) -> Option<SequenceId> {
        self.sequence_id
    }

    fn advance_position(&mut self, now: MonotonicTimestamp) -> Result<(), PlaybackError> {
        self.accept_timestamp(now)?;
        if !matches!(
            self.state,
            TransportState::Playing | TransportState::Recovering
        ) {
            return Ok(());
        }
        let elapsed = now.checked_elapsed_since(self.clock_anchor.monotonic)?;
        let advanced = elapsed_frames(elapsed, self.position.time_base)?;
        let target = self.clock_anchor.timeline.frame.saturating_add(advanced);
        self.position.frame = target.min(self.end_frame).max(0);
        if self.active_target_frame != Some(self.position.frame) {
            self.active_target_frame = Some(self.position.frame);
            self.terminal_delivery = None;
        }
        if self.position.frame >= self.end_frame {
            self.state = TransportState::Ended;
            self.clock_master = None;
        }
        Ok(())
    }

    fn accept_timestamp(&mut self, now: MonotonicTimestamp) -> Result<(), PlaybackError> {
        now.checked_elapsed_since(self.last_timestamp)?;
        self.last_timestamp = now;
        Ok(())
    }

    fn bump_epoch(&mut self) {
        self.epoch = PlaybackEpoch(self.epoch.0.saturating_add(1));
    }

    fn reanchor(&mut self, now: MonotonicTimestamp) {
        self.clock_anchor = ClockAnchor { timeline: self.position, monotonic: now };
    }

    fn reset_runtime_policy(&mut self) {
        self.preview_scale = PreviewResolutionScale::Full;
        self.quality_revision = self.quality_revision.saturating_add(1);
        self.recent_pressure.clear();
        self.consecutive_healthy = 0;
        self.active_target_frame = Some(self.position.frame);
        self.active_demand = None;
        self.terminal_delivery = None;
    }

    fn refresh_frame_demand_if_target_changed(
        &mut self,
        now: MonotonicTimestamp,
    ) -> Result<(), PlaybackError> {
        let current_matches = self.active_demand.is_some_and(|demand| {
            demand.epoch == self.epoch
                && demand.quality_revision == self.quality_revision
                && demand.target == self.position
        });
        if !current_matches {
            self.refresh_frame_demand(now)?;
        }
        Ok(())
    }

    fn refresh_frame_demand(&mut self, now: MonotonicTimestamp) -> Result<(), PlaybackError> {
        let frame_duration = frame_duration(self.position.time_base)?;
        self.refresh_frame_demand_with_duration(now, frame_duration)
    }

    fn refresh_frame_demand_with_duration(
        &mut self,
        now: MonotonicTimestamp,
        useful_duration: Duration,
    ) -> Result<(), PlaybackError> {
        validate_time_base(self.position.time_base)?;
        let sequence = FrameDemandSequence(self.next_demand_sequence);
        self.next_demand_sequence = self.next_demand_sequence.saturating_add(1);
        self.active_demand = Some(FrameDemand {
            epoch: self.epoch,
            quality_revision: self.quality_revision,
            sequence,
            sequence_id: self.sequence_id,
            timeline_revision: self.timeline_revision,
            target: self.position,
            deadline: now.saturating_add(useful_duration),
            preview_scale: self.preview_scale,
        });
        self.terminal_delivery = None;
        Ok(())
    }

    fn push_pressure(&mut self, pressured: bool) {
        if self.policy.pressure_window == 0 {
            return;
        }
        if self.recent_pressure.len() == self.policy.pressure_window {
            self.recent_pressure.remove(0);
        }
        self.recent_pressure.push(pressured);
    }

    fn pressure_count(&self) -> usize {
        self.recent_pressure.iter().filter(|value| **value).count()
    }
}

impl Default for PlaybackEngine {
    fn default() -> Self {
        Self::from_validated_time_base(Rational::new(1, 25), PlaybackPolicy::default())
    }
}

fn validate_time_base(value: Rational) -> Result<(), PlaybackError> {
    if value.num <= 0 || value.den <= 0 {
        Err(PlaybackError::InvalidTimeBase)
    } else {
        Ok(())
    }
}

fn nonnegative_frame(mut value: TimeCode) -> TimeCode {
    value.frame = value.frame.max(0);
    value
}

fn elapsed_frames(elapsed: Duration, time_base: Rational) -> Result<i64, PlaybackError> {
    validate_time_base(time_base)?;
    let nanos = elapsed.as_nanos();
    let numerator = nanos.saturating_mul(time_base.den as u128);
    let denominator = 1_000_000_000_u128.saturating_mul(time_base.num as u128);
    Ok((numerator / denominator).min(i64::MAX as u128) as i64)
}

fn frame_duration(time_base: Rational) -> Result<Duration, PlaybackError> {
    validate_time_base(time_base)?;
    let numerator = 1_000_000_000_u128.saturating_mul(time_base.num as u128);
    let denominator = time_base.den as u128;
    let nanos = numerator.saturating_add(denominator - 1) / denominator;
    Ok(Duration::from_nanos(nanos.min(u64::MAX as u128) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(ms: u64) -> MonotonicTimestamp {
        MonotonicTimestamp::from_duration(Duration::from_millis(ms))
    }

    fn engine() -> PlaybackEngine {
        PlaybackEngine::new(Rational::new(1, 25), PlaybackPolicy::default()).unwrap()
    }

    fn current_delivery(engine: &PlaybackEngine, kind: FrameDeliveryKind) -> FrameDelivery {
        let demand = engine.frame_demand().expect("active frame demand");
        FrameDelivery {
            epoch: demand.epoch,
            quality_revision: demand.quality_revision,
            demand_sequence: demand.sequence,
            target_frame: demand.target.frame,
            kind,
        }
    }

    #[test]
    fn priming_holds_then_synthetic_clock_advances_exact_frames() {
        let mut engine = engine();
        engine.play(100, ts(0)).unwrap();
        assert_eq!(engine.tick(ts(400)).unwrap().position.frame, 0);

        engine.complete_priming(ClockMaster::Synthetic, ts(400)).unwrap();
        assert_eq!(engine.tick(ts(440)).unwrap().position.frame, 1);
        assert_eq!(engine.tick(ts(800)).unwrap().position.frame, 10);
    }

    #[test]
    fn audio_loss_handoff_is_continuous_and_never_uses_video_as_master() {
        let mut engine = engine();
        engine.play(100, ts(0)).unwrap();
        engine.complete_priming(ClockMaster::AudioDevice, ts(0)).unwrap();
        let before = engine.tick(ts(400)).unwrap();
        let handoff = engine.audio_device_lost(ts(400)).unwrap();
        let after = engine.tick(ts(440)).unwrap();

        assert_eq!(before.position, handoff.position);
        assert_eq!(handoff.clock_master, Some(ClockMaster::Synthetic));
        assert_eq!(after.position.frame, before.position.frame + 1);
    }

    #[test]
    fn seek_invalidates_old_epoch_delivery() {
        let mut engine = engine();
        let first = engine.play(100, ts(0)).unwrap();
        engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
        let current = engine.seek(TimeCode::new(50, Rational::new(1, 25)), ts(10)).unwrap();

        let accepted = engine
            .observe_frame_delivery(FrameDelivery {
                epoch: first.epoch,
                quality_revision: first.quality_revision,
                demand_sequence: FrameDemandSequence(0),
                target_frame: 0,
                kind: FrameDeliveryKind::Ready,
            })
            .unwrap();
        assert!(!accepted);
        assert_eq!(engine.snapshot(), current);
    }

    #[test]
    fn sustained_pressure_recovers_by_resolution_without_proxy_semantics() {
        let policy = PlaybackPolicy {
            priming_limit: Duration::from_millis(500),
            pressure_window: 4,
            pressure_threshold: 3,
            healthy_deliveries_to_recover: 2,
        };
        let mut engine = PlaybackEngine::new(Rational::new(1, 25), policy).unwrap();
        engine.play(100, ts(0)).unwrap();
        let playing = engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();

        for (index, kind) in [
            FrameDeliveryKind::Late,
            FrameDeliveryKind::Late,
            FrameDeliveryKind::Ready,
            FrameDeliveryKind::Failed,
        ]
        .into_iter()
        .enumerate()
        {
            engine.tick(ts((index as u64 + 1) * 40)).unwrap();
            let delivery = current_delivery(&engine, kind);
            engine.observe_frame_delivery(delivery).unwrap();
        }
        let recovering = engine.snapshot();
        assert_eq!(recovering.state, TransportState::Recovering);
        assert_eq!(recovering.preview_scale, PreviewResolutionScale::Half);
        assert!(recovering.quality_revision > playing.quality_revision);

        for index in 0..2 {
            engine.tick(ts(200 + index * 40)).unwrap();
            let delivery = current_delivery(&engine, FrameDeliveryKind::Ready);
            engine.observe_frame_delivery(delivery).unwrap();
        }
        assert_eq!(engine.snapshot().state, TransportState::Playing);
        assert_eq!(
            engine.snapshot().preview_scale,
            PreviewResolutionScale::Full
        );
    }

    #[test]
    fn blocked_delivery_is_not_reclassified_as_buffering_or_pressure() {
        let mut engine = engine();
        engine.play(100, ts(0)).unwrap();
        engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
        let delivery = current_delivery(&engine, FrameDeliveryKind::Blocked);
        engine.observe_frame_delivery(delivery).unwrap();
        assert_eq!(engine.snapshot().state, TransportState::Blocked);
        assert_eq!(engine.snapshot().clock_master, None);
    }

    #[test]
    fn non_monotonic_time_is_rejected_without_moving_position() {
        let mut engine = engine();
        engine.play(100, ts(100)).unwrap();
        engine.complete_priming(ClockMaster::Synthetic, ts(100)).unwrap();
        let before = engine.snapshot();
        assert_eq!(
            engine.tick(ts(99)),
            Err(PlaybackError::NonMonotonicTimestamp)
        );
        assert_eq!(engine.snapshot(), before);
    }

    #[test]
    fn fractional_rates_do_not_accumulate_float_drift() {
        let mut engine =
            PlaybackEngine::new(Rational::new(1001, 30000), PlaybackPolicy::default()).unwrap();
        engine.play(100_000, ts(0)).unwrap();
        engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
        let snapshot = engine
            .tick(MonotonicTimestamp::from_duration(Duration::from_secs(1001)))
            .unwrap();
        assert_eq!(snapshot.position.frame, 30_000);
    }

    #[test]
    fn next_frame_delay_uses_remaining_subframe_time() {
        let mut engine = engine();
        engine.play(100, ts(0)).unwrap();
        engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
        assert_eq!(
            engine.time_until_next_frame(ts(10)).unwrap(),
            Some(Duration::from_millis(30))
        );
    }

    #[test]
    fn invalid_recovery_policy_is_rejected_at_the_interface() {
        let result = PlaybackEngine::new(
            Rational::new(1, 25),
            PlaybackPolicy {
                priming_limit: Duration::from_millis(500),
                pressure_window: 4,
                pressure_threshold: 5,
                healthy_deliveries_to_recover: 1,
            },
        );
        assert!(matches!(result, Err(PlaybackError::InvalidPolicy)));
    }

    #[test]
    fn paused_playhead_may_seek_beyond_current_content_end() {
        let mut engine = engine();
        engine
            .reset_timeline(None, 1, TimeCode::new(120, Rational::new(1, 25)), 20, ts(0))
            .unwrap();
        let snapshot = engine.seek(TimeCode::new(200, Rational::new(1, 25)), ts(0)).unwrap();
        assert_eq!(snapshot.state, TransportState::Paused);
        assert_eq!(snapshot.position.frame, 200);
    }

    #[test]
    fn duplicate_terminal_delivery_cannot_mutate_pressure_twice() {
        let mut engine = engine();
        engine.play(100, ts(0)).unwrap();
        engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
        let delivery = current_delivery(&engine, FrameDeliveryKind::Late);
        assert!(engine.observe_frame_delivery(delivery).unwrap());
        assert!(!engine.observe_frame_delivery(delivery).unwrap());
    }

    #[test]
    fn frame_demand_identity_and_deadline_advance_once_per_target() {
        let mut engine = engine();
        engine.play(100, ts(0)).unwrap();
        engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
        let first = engine.frame_demand().expect("initial demand");
        assert_eq!(first.target.frame, 0);
        assert_eq!(first.deadline, ts(40));

        engine.tick(ts(10)).unwrap();
        assert_eq!(engine.frame_demand(), Some(first));

        engine.tick(ts(40)).unwrap();
        let second = engine.frame_demand().expect("next demand");
        assert_eq!(second.target.frame, 1);
        assert!(second.sequence.get() > first.sequence.get());
        assert_eq!(second.deadline, ts(80));
    }

    #[test]
    fn delivery_for_superseded_demand_is_rejected_even_on_same_epoch() {
        let mut engine = engine();
        engine.play(100, ts(0)).unwrap();
        engine.complete_priming(ClockMaster::Synthetic, ts(0)).unwrap();
        let old = current_delivery(&engine, FrameDeliveryKind::Ready);
        engine.tick(ts(40)).unwrap();

        assert!(!engine.observe_frame_delivery(old).unwrap());
        assert_eq!(engine.snapshot().state, TransportState::Playing);
    }

    #[test]
    fn priming_timeout_starts_at_deadline_and_catches_up_without_extra_drift() {
        let mut engine = engine();
        engine.play(100, ts(0)).unwrap();

        let snapshot = engine.tick(ts(750)).unwrap();

        assert_eq!(snapshot.state, TransportState::Playing);
        assert_eq!(snapshot.clock_master, Some(ClockMaster::Synthetic));
        assert_eq!(snapshot.position.frame, 6);
    }
}
