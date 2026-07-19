//! Headless realtime playback state, clock, and frame-delivery policy.
//!
//! This crate owns transport semantics but deliberately knows nothing about UI,
//! codecs, GPU resources, audio devices, or concrete timeline models.

use mondrian_core::{FramePosition, Rational, SequenceId};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

mod evidence;
pub use evidence::*;
mod cancellation_evidence;
pub use cancellation_evidence::*;
mod frame_work;
pub use frame_work::*;
mod frame_store;
pub use frame_store::*;
mod runtime_clock;
pub use runtime_clock::*;
mod work_broker;
pub use work_broker::*;

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

/// Immutable timeline facts applied atomically with a transport transition.
///
/// Keeping identity, semantic revision, evaluation grid, and content boundary
/// together prevents App Adapters from exposing a transient reset between one
/// user intent and its resulting play or seek state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackTimelineBinding {
    sequence_id: Option<SequenceId>,
    timeline_revision: u64,
    time_base: Rational,
    end_frame: i64,
}

impl PlaybackTimelineBinding {
    /// Create a validated binding for one Sequence revision.
    pub fn new(
        sequence_id: Option<SequenceId>,
        timeline_revision: u64,
        time_base: Rational,
        end_frame: i64,
    ) -> Result<Self, PlaybackError> {
        validate_time_base(time_base)?;
        Ok(Self {
            sequence_id,
            timeline_revision,
            time_base,
            end_frame: end_frame.max(0),
        })
    }

    fn validate_position(self, position: FramePosition) -> Result<(), PlaybackError> {
        validate_time_base(position.time_base)?;
        if position.time_base != self.time_base {
            return Err(PlaybackError::MismatchedTimelineTimeBase);
        }
        Ok(())
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

/// Stable identity that preview adapters carry without interpreting playback policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameDemandIdentity {
    /// Playback Session identity.
    pub epoch: PlaybackEpoch,
    /// Runtime quality-policy revision.
    pub quality_revision: u64,
    /// Demand sequence within the Playback Engine lifetime.
    pub sequence: FrameDemandSequence,
    /// Exact timeline frame requested by the demand.
    pub target_frame: i64,
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
    pub target: FramePosition,
    /// Latest useful presentation time for realtime work, or `None` for a
    /// paused current-frame presentation that remains useful until superseded.
    pub deadline: Option<MonotonicTimestamp>,
    /// Runtime-only spatial quality selected by recovery policy.
    pub preview_scale: PreviewResolutionScale,
}

impl FrameDemand {
    /// Project the request to the opaque identity carried through preview adapters.
    pub const fn identity(self) -> FrameDemandIdentity {
        FrameDemandIdentity {
            epoch: self.epoch,
            quality_revision: self.quality_revision,
            sequence: self.sequence,
            target_frame: self.target.frame,
        }
    }
}

/// Allowed on-time presentation result carried by a Frame Presentation Ticket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FramePresentationQuality {
    /// Exact requested quality was presented.
    #[default]
    Ready,
    /// An explicitly allowed degraded path was presented.
    Degraded,
}

/// Opaque authority for one Presentation Adapter to finish a Frame Demand.
///
/// The ticket keeps identity, deadline, and allowed quality together so Window,
/// CPU, and headless Adapters cannot independently reinterpret timing policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePresentationTicket {
    identity: FrameDemandIdentity,
    deadline: Option<MonotonicTimestamp>,
    quality: FramePresentationQuality,
}

impl FramePresentationTicket {
    /// Create the presentation authority carried by an active demand.
    pub const fn for_demand(demand: FrameDemand, quality: FramePresentationQuality) -> Self {
        Self {
            identity: demand.identity(),
            deadline: demand.deadline,
            quality,
        }
    }

    /// Return the exact demand identity protected by this ticket.
    pub const fn identity(self) -> FrameDemandIdentity {
        self.identity
    }

    /// Return the authoritative presentation deadline.
    pub const fn deadline(self) -> Option<MonotonicTimestamp> {
        self.deadline
    }

    /// Classify real presentation completion against the demand deadline.
    pub fn complete_at(self, completed_at: MonotonicTimestamp) -> FrameDelivery {
        let kind = if self.deadline.is_some_and(|deadline| completed_at >= deadline) {
            FrameDeliveryKind::Late
        } else {
            match self.quality {
                FramePresentationQuality::Ready => FrameDeliveryKind::Ready,
                FramePresentationQuality::Degraded => FrameDeliveryKind::Degraded,
            }
        };
        FrameDelivery::for_demand(self.identity, kind)
    }
}

/// Authoritative elapsed-media-time source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockMaster {
    /// Device-consumed audio sample position with explicit observation quality.
    AudioDevice,
    /// Runtime monotonic time anchored to exact timeline time.
    Synthetic,
}

/// Quality grade of one audio-device clock observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioClockObservationGrade {
    /// Cumulative frames requested by the device callback with bounded uncertainty.
    CallbackConsumptionEstimate,
}

/// Runtime state reported by an audio output Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioDeviceClockState {
    /// The stream callback is active and its cumulative frame counter is usable.
    Running,
    /// The stream exists but its latest callback sample is temporarily too
    /// stale or uncertain to advance safely.
    Uncertain,
    /// The stream failed or no longer provides a usable monotonic observation.
    Unavailable,
}

/// Versioned audio-device sample observation consumed by the Playback Engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDeviceClockObservation {
    /// Playback Session identity at the Adapter observation seam.
    pub epoch: PlaybackEpoch,
    /// Concrete output-stream generation.
    pub stream_generation: u64,
    /// Device callback sample rate.
    pub sample_rate: u32,
    /// Cumulative output frames consumed by callbacks in this stream generation.
    pub consumed_frames: u64,
    /// Exact timeline-media time queued at active callback-consumption frame zero.
    ///
    /// Its time base may be the output sample period; the Engine converts it to
    /// the sequence time base without accumulating floating-point seconds.
    pub media_anchor: FramePosition,
    /// Engine-relative monotonic observation time.
    pub observed_at: MonotonicTimestamp,
    /// Observation quality; never infer exact hardware position from this value.
    pub grade: AudioClockObservationGrade,
    /// Estimated output latency in device frames.
    pub estimated_latency_frames: u32,
    /// Upper-bound uncertainty in device frames.
    pub uncertainty_frames: u32,
    /// Cumulative callback frames rendered as silence because PCM was unavailable.
    pub underrun_frames: u64,
    /// Current output stream state.
    pub state: AudioDeviceClockState,
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
    /// Integer divisor applied to each Viewer dimension by preview Adapters.
    pub const fn dimension_divisor(self) -> u32 {
        match self {
            Self::Full => 1,
            Self::Half => 2,
            Self::Quarter => 4,
        }
    }

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

/// Media-frame lookahead observed by the preview Adapter during startup.
///
/// `available_media_frames` is the number of immediate future timeline frames
/// that require media decode and can therefore contribute to startup preroll.
/// `ready_media_frames` is the prefix of those frames whose required media
/// payloads are already resident. The Engine combines this observation with
/// actual current-frame presentation; neither signal can start the clock alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoPrerollObservation {
    /// Playback Session identity that produced this lookahead observation.
    pub epoch: PlaybackEpoch,
    /// Consecutive future media frames ready for presentation preparation.
    pub ready_media_frames: usize,
    /// Consecutive future frames that require media decode in the observed window.
    pub available_media_frames: usize,
}

impl FrameDelivery {
    /// Build a terminal observation for an exact demand identity.
    pub const fn for_demand(identity: FrameDemandIdentity, kind: FrameDeliveryKind) -> Self {
        Self {
            epoch: identity.epoch,
            quality_revision: identity.quality_revision,
            demand_sequence: identity.sequence,
            target_frame: identity.target_frame,
            kind,
        }
    }

    /// Recover the opaque demand identity carried through an Adapter.
    pub const fn identity(self) -> FrameDemandIdentity {
        FrameDemandIdentity {
            epoch: self.epoch,
            quality_revision: self.quality_revision,
            sequence: self.demand_sequence,
            target_frame: self.target_frame,
        }
    }
}

/// Versioned policy values that determine transport and recovery behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackPolicy {
    /// Maximum startup/seek wait before Synthetic Clock Master continues.
    pub priming_limit: Duration,
    /// Future media frames required before a presented current frame releases startup priming.
    pub minimum_video_preroll_frames: usize,
    /// Maximum number of recent current deliveries considered for pressure.
    pub pressure_window: usize,
    /// Late/failed deliveries in the window required to enter recovery.
    pub pressure_threshold: usize,
    /// Number of consecutive healthy deliveries required to raise one scale.
    pub healthy_deliveries_to_recover: usize,
    /// Maximum uncertainty accepted for callback-estimated Audio Device Master.
    pub max_audio_clock_uncertainty: Duration,
    /// Grace interval that preserves Audio Device Master across transient
    /// callback sampling uncertainty before Synthetic fallback.
    pub audio_clock_uncertainty_grace: Duration,
    /// Largest absolute media phase error allowed when selecting a new audio stream.
    pub max_audio_handoff_phase_error: Duration,
}

/// Largest immediate video lookahead a Playback Adapter may report for
/// bounded startup/seek priming.
pub const MAX_BOUNDED_VIDEO_PREROLL_FRAMES: usize = 16;

impl Default for PlaybackPolicy {
    fn default() -> Self {
        Self {
            priming_limit: Duration::from_millis(500),
            minimum_video_preroll_frames: MAX_BOUNDED_VIDEO_PREROLL_FRAMES,
            pressure_window: 12,
            pressure_threshold: 8,
            healthy_deliveries_to_recover: 60,
            max_audio_clock_uncertainty: Duration::from_millis(50),
            audio_clock_uncertainty_grace: Duration::from_secs(1),
            max_audio_handoff_phase_error: Duration::from_millis(20),
        }
    }
}

/// Result of the latest Synthetic-to-Audio Clock Master qualification attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioClockHandoffStatus {
    /// The stream phase was inside policy and Audio Device became authoritative.
    Accepted,
    /// The stream phase exceeded policy; Synthetic remains authoritative.
    PhaseRejected,
}

/// Structured phase evidence for a new audio stream generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioClockHandoffEvidence {
    /// Concrete stream generation being qualified.
    pub stream_generation: u64,
    /// Signed candidate-audio minus active Clock Master phase in nanoseconds.
    pub phase_error_ns: i64,
    /// Qualification result.
    pub status: AudioClockHandoffStatus,
}

/// Read-only authoritative state published to app and UI adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackSnapshot {
    /// Playback Session identity.
    pub epoch: PlaybackEpoch,
    /// Current transport condition.
    pub state: TransportState,
    /// Current exact timeline position.
    pub position: FramePosition,
    /// Current Clock Master, if a clock is running or being primed.
    pub clock_master: Option<ClockMaster>,
    /// Runtime-only Viewer resolution scale.
    pub preview_scale: PreviewResolutionScale,
    /// Revision required on Frame Demands and Deliveries.
    pub quality_revision: u64,
    /// Latest current-epoch audio observation, including quality and underrun evidence.
    pub audio_clock_observation: Option<AudioDeviceClockObservation>,
    /// Latest new-stream phase qualification evidence.
    pub audio_handoff: Option<AudioClockHandoffEvidence>,
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
    /// A transport position was expressed on a different grid than its binding.
    #[error("transport position time base does not match its timeline binding")]
    MismatchedTimelineTimeBase,
    /// A delivery targeted a different timeline frame than the current demand.
    #[error("frame delivery does not match the active target")]
    MismatchedFrameDelivery,
    /// Recovery policy cannot form a bounded pressure/health window.
    #[error("playback policy is invalid")]
    InvalidPolicy,
    /// Audio observation did not carry a usable sample rate.
    #[error("audio clock observation sample rate must be positive")]
    InvalidAudioSampleRate,
    /// Preview Adapter reported an inconsistent or unbounded lookahead window.
    #[error("video preroll must be a bounded ready prefix of the available media window")]
    InvalidVideoPrerollObservation,
}

#[derive(Debug, Clone, Copy)]
struct ClockAnchor {
    phase_ns: i128,
    monotonic: MonotonicTimestamp,
}

#[derive(Debug, Clone, Copy)]
struct AudioDeviceClockAnchor {
    stream_generation: u64,
    media_anchor: FramePosition,
    last_effective_consumed_frames: u64,
    last_observed_at: MonotonicTimestamp,
    last_media_position_ns: i128,
    last_uncertainty_frames: u32,
}

/// Deep, headless Module owning a Playback Session and its realtime invariants.
pub struct PlaybackEngine {
    policy: PlaybackPolicy,
    sequence_id: Option<SequenceId>,
    timeline_revision: u64,
    epoch: PlaybackEpoch,
    state: TransportState,
    position: FramePosition,
    end_frame: i64,
    clock_master: Option<ClockMaster>,
    clock_anchor: ClockAnchor,
    last_timestamp: MonotonicTimestamp,
    preview_scale: PreviewResolutionScale,
    quality_revision: u64,
    recent_pressure: Vec<bool>,
    consecutive_healthy: usize,
    next_demand_sequence: u64,
    active_demand: Option<FrameDemand>,
    terminal_delivery: Option<(PlaybackEpoch, u64, FrameDemandSequence)>,
    priming_current_presentable: bool,
    video_preroll_observation: Option<VideoPrerollObservation>,
    audio_device_anchor: Option<AudioDeviceClockAnchor>,
    last_audio_observation: Option<AudioDeviceClockObservation>,
    last_audio_handoff: Option<AudioClockHandoffEvidence>,
    audio_uncertain_since: Option<MonotonicTimestamp>,
}

impl PlaybackEngine {
    /// Create a stopped engine at frame zero with a validated timeline time base.
    pub fn new(time_base: Rational, policy: PlaybackPolicy) -> Result<Self, PlaybackError> {
        validate_time_base(time_base)?;
        if policy.priming_limit.is_zero()
            || policy.minimum_video_preroll_frames > MAX_BOUNDED_VIDEO_PREROLL_FRAMES
            || policy.pressure_window == 0
            || policy.pressure_threshold == 0
            || policy.pressure_threshold > policy.pressure_window
            || policy.healthy_deliveries_to_recover == 0
            || policy.max_audio_clock_uncertainty.is_zero()
            || policy.audio_clock_uncertainty_grace.is_zero()
            || policy.max_audio_handoff_phase_error.is_zero()
        {
            return Err(PlaybackError::InvalidPolicy);
        }
        Ok(Self::from_validated_time_base(time_base, policy))
    }

    fn from_validated_time_base(time_base: Rational, policy: PlaybackPolicy) -> Self {
        let position = FramePosition::new(0, time_base);
        Self {
            policy,
            sequence_id: None,
            timeline_revision: 0,
            epoch: PlaybackEpoch(0),
            state: TransportState::Stopped,
            position,
            end_frame: 0,
            clock_master: None,
            clock_anchor: ClockAnchor { phase_ns: 0, monotonic: MonotonicTimestamp::ZERO },
            last_timestamp: MonotonicTimestamp::ZERO,
            preview_scale: PreviewResolutionScale::Full,
            quality_revision: 0,
            recent_pressure: Vec::with_capacity(policy.pressure_window),
            consecutive_healthy: 0,
            next_demand_sequence: 1,
            active_demand: None,
            terminal_delivery: None,
            priming_current_presentable: false,
            video_preroll_observation: None,
            audio_device_anchor: None,
            last_audio_observation: None,
            last_audio_handoff: None,
            audio_uncertain_since: None,
        }
    }

    /// Atomically bind a Sequence revision and begin bounded playback priming.
    pub fn play_timeline(
        &mut self,
        binding: PlaybackTimelineBinding,
        position: FramePosition,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        binding.validate_position(position)?;
        self.accept_timestamp(now)?;
        self.apply_timeline_binding(binding);
        self.position = nonnegative_frame(position);
        if self.position.frame > self.end_frame {
            self.position.frame = 0;
        }
        self.bump_epoch();
        self.state = TransportState::Priming;
        self.clock_master = Some(ClockMaster::Synthetic);
        self.reset_runtime_policy();
        self.refresh_frame_demand_with_duration(now, self.policy.priming_limit)?;
        self.reanchor(now);
        Ok(self.snapshot())
    }

    /// Atomically bind a Sequence revision, seek, and preserve play intent.
    ///
    /// Priming, Playing, and Recovering all count as active play intent. An
    /// active seek publishes a bounded current-frame demand in the new epoch;
    /// an inactive seek publishes an untimed demand and remains paused.
    pub fn seek_timeline(
        &mut self,
        binding: PlaybackTimelineBinding,
        position: FramePosition,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        binding.validate_position(position)?;
        self.accept_timestamp(now)?;
        let was_running = self.transport_intends_playback();
        self.apply_timeline_binding(binding);
        self.seek_after_timestamp(position, was_running, now)
    }

    #[cfg(test)]
    fn play(
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

    #[cfg(test)]
    fn seek(
        &mut self,
        position: FramePosition,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.accept_timestamp(now)?;
        validate_time_base(position.time_base)?;
        let was_running = self.transport_intends_playback();
        self.seek_after_timestamp(position, was_running, now)
    }

    fn seek_after_timestamp(
        &mut self,
        position: FramePosition,
        was_running: bool,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.bump_epoch();
        self.position = nonnegative_frame(position);
        self.state = if was_running {
            TransportState::Priming
        } else {
            TransportState::Paused
        };
        self.clock_master = was_running.then_some(ClockMaster::Synthetic);
        self.reset_runtime_policy();
        self.reanchor(now);
        if was_running {
            self.refresh_frame_demand_with_duration(now, self.policy.priming_limit)?;
        } else {
            self.refresh_untimed_frame_demand()?;
        }
        Ok(self.snapshot())
    }

    /// Advance the active Clock Master to `now` and publish the newest frame.
    pub fn tick(&mut self, now: MonotonicTimestamp) -> Result<PlaybackSnapshot, PlaybackError> {
        self.accept_timestamp(now)?;
        if self.state == TransportState::Priming {
            if let Some(deadline) = self
                .active_demand
                .and_then(|demand| demand.deadline)
                .filter(|deadline| now >= *deadline)
            {
                self.state = TransportState::Playing;
                self.clock_master = Some(ClockMaster::Synthetic);
                self.reanchor(deadline);
            }
        }
        self.advance_position(now)?;
        match self.state {
            TransportState::Playing | TransportState::Recovering => {
                self.refresh_frame_demand_if_target_changed(now)?;
            }
            TransportState::Ended => {
                self.refresh_untimed_frame_demand_if_target_changed()?;
            }
            _ => {}
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
        if self.clock_master == Some(ClockMaster::AudioDevice) {
            return Ok(Some(Duration::from_millis(2)));
        }
        let phase_ns = self.synthetic_phase_ns_at(now)?;
        let current_frame = timeline_frame_at_ns(phase_ns, self.position.time_base)?.max(0);
        let next_boundary_ns = time_code_ns(FramePosition::new(
            current_frame.saturating_add(1),
            self.position.time_base,
        ))?;
        let remaining_ns = next_boundary_ns.saturating_sub(phase_ns).max(0) as u128;
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
        self.handoff_to_synthetic(now)?;
        Ok(self.snapshot())
    }

    /// Apply a qualified audio callback-consumption observation.
    ///
    /// Old epochs are ignored. Explicitly unavailable, non-monotonic, or
    /// excessively uncertain observations hand off continuously to Synthetic
    /// Clock Master. A temporarily `Uncertain` active stream retains an existing
    /// Audio Device Master only for the policy's bounded uncertainty grace.
    pub fn observe_audio_device_clock(
        &mut self,
        observation: AudioDeviceClockObservation,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        if observation.epoch != self.epoch {
            return Ok(self.snapshot());
        }
        if observation.state == AudioDeviceClockState::Running && observation.sample_rate == 0 {
            return Err(PlaybackError::InvalidAudioSampleRate);
        }
        self.advance_position(observation.observed_at)?;
        self.last_audio_observation = Some(observation);
        if self.state == TransportState::Ended {
            self.audio_device_anchor = None;
            self.refresh_untimed_frame_demand_if_target_changed()?;
            return Ok(self.snapshot());
        }
        if observation.state == AudioDeviceClockState::Unavailable {
            self.audio_uncertain_since = None;
            self.handoff_to_synthetic(observation.observed_at)?;
            return Ok(self.snapshot());
        }
        if observation.state == AudioDeviceClockState::Uncertain {
            let uncertain_since =
                *self.audio_uncertain_since.get_or_insert(observation.observed_at);
            if self.clock_master == Some(ClockMaster::AudioDevice)
                && observation
                    .observed_at
                    .duration_since_origin()
                    .saturating_sub(uncertain_since.duration_since_origin())
                    <= self.policy.audio_clock_uncertainty_grace
            {
                return Ok(self.snapshot());
            }
            self.handoff_to_synthetic(observation.observed_at)?;
            return Ok(self.snapshot());
        }
        self.audio_uncertain_since = None;
        let uncertainty = sample_frames_duration(
            observation.uncertainty_frames as u64,
            observation.sample_rate,
        );
        if uncertainty > self.policy.max_audio_clock_uncertainty {
            self.handoff_to_synthetic(observation.observed_at)?;
            return Ok(self.snapshot());
        }
        if !matches!(
            self.state,
            TransportState::Playing | TransportState::Recovering
        ) {
            return Ok(self.snapshot());
        }

        let effective_consumed = observation
            .consumed_frames
            .saturating_sub(observation.estimated_latency_frames as u64);
        if self.audio_device_anchor.is_some_and(|anchor| {
            anchor.stream_generation == observation.stream_generation
                && (effective_consumed < anchor.last_effective_consumed_frames
                    || observation.media_anchor != anchor.media_anchor)
        }) {
            self.handoff_to_synthetic(observation.observed_at)?;
            return Ok(self.snapshot());
        }
        let Some(anchor) = self
            .audio_device_anchor
            .filter(|anchor| anchor.stream_generation == observation.stream_generation)
        else {
            let candidate_ns = audio_media_position_ns(observation, effective_consumed)?;
            let reference_ns = self.clock_phase_reference_ns(observation.observed_at)?;
            let phase_error_ns = candidate_ns.saturating_sub(reference_ns);
            let phase_error_abs = phase_error_ns.unsigned_abs();
            let accepted = phase_error_abs <= self.policy.max_audio_handoff_phase_error.as_nanos();
            self.last_audio_handoff = Some(AudioClockHandoffEvidence {
                stream_generation: observation.stream_generation,
                phase_error_ns: phase_error_ns.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
                status: if accepted {
                    AudioClockHandoffStatus::Accepted
                } else {
                    AudioClockHandoffStatus::PhaseRejected
                },
            });
            if !accepted {
                self.handoff_to_synthetic(observation.observed_at)?;
                return Ok(self.snapshot());
            }
            self.audio_device_anchor = Some(AudioDeviceClockAnchor {
                stream_generation: observation.stream_generation,
                media_anchor: observation.media_anchor,
                last_effective_consumed_frames: effective_consumed,
                last_observed_at: observation.observed_at,
                last_media_position_ns: candidate_ns,
                last_uncertainty_frames: observation.uncertainty_frames,
            });
            self.clock_master = Some(ClockMaster::AudioDevice);
            self.reanchor_at_phase(observation.observed_at, candidate_ns);
            self.refresh_frame_demand_if_target_changed(observation.observed_at)?;
            return Ok(self.snapshot());
        };

        let observed_elapsed =
            observation.observed_at.checked_elapsed_since(anchor.last_observed_at)?;
        let consumed_delta =
            effective_consumed.saturating_sub(anchor.last_effective_consumed_frames);
        let allowed_elapsed = observed_elapsed.saturating_add(sample_frames_duration(
            u64::from(observation.uncertainty_frames.max(anchor.last_uncertainty_frames)),
            observation.sample_rate,
        ));
        if sample_frames_duration(consumed_delta, observation.sample_rate) > allowed_elapsed {
            self.handoff_to_synthetic(observation.observed_at)?;
            return Ok(self.snapshot());
        }

        let media_position_ns = audio_media_position_ns(observation, effective_consumed)?;
        let target = timeline_frame_at_ns(media_position_ns, self.position.time_base)?;
        self.position.frame = self.position.frame.max(target).min(self.end_frame).max(0);
        self.audio_device_anchor = Some(AudioDeviceClockAnchor {
            stream_generation: anchor.stream_generation,
            media_anchor: anchor.media_anchor,
            last_effective_consumed_frames: effective_consumed,
            last_observed_at: observation.observed_at,
            last_media_position_ns: media_position_ns,
            last_uncertainty_frames: observation.uncertainty_frames,
        });
        self.clock_master = Some(ClockMaster::AudioDevice);
        if self.position.frame >= self.end_frame {
            self.state = TransportState::Ended;
            self.clock_master = None;
            self.audio_device_anchor = None;
            self.refresh_untimed_frame_demand_if_target_changed()?;
        } else {
            self.refresh_frame_demand_if_target_changed(observation.observed_at)?;
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
        let Some(active_demand) =
            self.active_demand.filter(|demand| demand.sequence == delivery.demand_sequence)
        else {
            return Ok(false);
        };
        if active_demand.target.frame != delivery.target_frame {
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
            FrameDeliveryKind::Late | FrameDeliveryKind::Degraded | FrameDeliveryKind::Failed
        );
        self.terminal_delivery = Some(identity);
        let presentable = matches!(
            delivery.kind,
            FrameDeliveryKind::Ready | FrameDeliveryKind::Degraded
        );
        let healthy = delivery.kind == FrameDeliveryKind::Ready;
        if self.state == TransportState::Priming && presentable {
            self.priming_current_presentable = true;
            self.try_complete_observed_priming();
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

    /// Record bounded startup media lookahead from the preview Adapter.
    ///
    /// Old Playback Sessions are ignored. A valid observation can release
    /// `Priming` only after the current Frame Demand has also been presented.
    pub fn observe_video_preroll(
        &mut self,
        observation: VideoPrerollObservation,
    ) -> Result<bool, PlaybackError> {
        if observation.epoch != self.epoch {
            return Ok(false);
        }
        if observation.ready_media_frames > observation.available_media_frames
            || observation.available_media_frames > MAX_BOUNDED_VIDEO_PREROLL_FRAMES
        {
            return Err(PlaybackError::InvalidVideoPrerollObservation);
        }
        if self.state != TransportState::Priming
            || self.video_preroll_observation == Some(observation)
        {
            return Ok(false);
        }
        self.video_preroll_observation = Some(observation);
        Ok(self.try_complete_observed_priming())
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
            audio_clock_observation: self.last_audio_observation,
            audio_handoff: self.last_audio_handoff,
        }
    }

    /// Return the current demand that preview adapters must carry end-to-end.
    pub const fn frame_demand(&self) -> Option<FrameDemand> {
        self.active_demand
    }

    /// Return the active demand only while no terminal presentation/decode
    /// observation has consumed it.
    pub const fn pending_frame_demand(&self) -> Option<FrameDemand> {
        if self.terminal_delivery.is_none() {
            self.active_demand
        } else {
            None
        }
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
        let phase_ns = if self.clock_master == Some(ClockMaster::AudioDevice) {
            self.audio_phase_ns_at(now)?.unwrap_or(time_code_ns(self.position)?)
        } else {
            self.synthetic_phase_ns_at(now)?
        };
        let target = timeline_frame_at_ns(phase_ns, self.position.time_base)?;
        self.position.frame = target.min(self.end_frame).max(0);
        if self.position.frame >= self.end_frame {
            self.state = TransportState::Ended;
            self.clock_master = None;
        }
        Ok(())
    }

    fn clock_phase_reference_ns(&self, now: MonotonicTimestamp) -> Result<i128, PlaybackError> {
        if self.clock_master == Some(ClockMaster::AudioDevice) {
            return self.audio_phase_ns_at(now)?.map_or_else(|| time_code_ns(self.position), Ok);
        }
        if self.clock_master == Some(ClockMaster::Synthetic)
            && matches!(
                self.state,
                TransportState::Playing | TransportState::Recovering
            )
        {
            return self.synthetic_phase_ns_at(now);
        }
        time_code_ns(self.position)
    }

    fn audio_phase_ns_at(&self, now: MonotonicTimestamp) -> Result<Option<i128>, PlaybackError> {
        let Some(anchor) = self.audio_device_anchor else {
            return Ok(None);
        };
        let elapsed = now.checked_elapsed_since(anchor.last_observed_at)?;
        let elapsed_ns = elapsed.as_nanos().min(i128::MAX as u128) as i128;
        Ok(Some(
            anchor.last_media_position_ns.saturating_add(elapsed_ns),
        ))
    }

    fn synthetic_phase_ns_at(&self, now: MonotonicTimestamp) -> Result<i128, PlaybackError> {
        let elapsed = now.checked_elapsed_since(self.clock_anchor.monotonic)?;
        let elapsed_ns = elapsed.as_nanos().min(i128::MAX as u128) as i128;
        Ok(self.clock_anchor.phase_ns.saturating_add(elapsed_ns))
    }

    fn accept_timestamp(&mut self, now: MonotonicTimestamp) -> Result<(), PlaybackError> {
        now.checked_elapsed_since(self.last_timestamp)?;
        self.last_timestamp = now;
        Ok(())
    }

    fn apply_timeline_binding(&mut self, binding: PlaybackTimelineBinding) {
        self.sequence_id = binding.sequence_id;
        self.timeline_revision = binding.timeline_revision;
        self.end_frame = binding.end_frame;
    }

    fn transport_intends_playback(&self) -> bool {
        matches!(
            self.state,
            TransportState::Priming | TransportState::Playing | TransportState::Recovering
        )
    }

    fn bump_epoch(&mut self) {
        self.epoch = PlaybackEpoch(self.epoch.0.saturating_add(1));
    }

    fn reanchor(&mut self, now: MonotonicTimestamp) {
        let phase_ns = time_code_ns(self.position).unwrap_or(0);
        self.reanchor_at_phase(now, phase_ns);
    }

    fn reanchor_at_phase(&mut self, now: MonotonicTimestamp, phase_ns: i128) {
        self.clock_anchor = ClockAnchor { phase_ns, monotonic: now };
    }

    fn handoff_to_synthetic(&mut self, now: MonotonicTimestamp) -> Result<(), PlaybackError> {
        if matches!(
            self.state,
            TransportState::Playing | TransportState::Recovering
        ) && self.clock_master != Some(ClockMaster::Synthetic)
        {
            let phase_ns = self.audio_phase_ns_at(now)?.unwrap_or(time_code_ns(self.position)?);
            self.position.frame = timeline_frame_at_ns(phase_ns, self.position.time_base)?
                .min(self.end_frame)
                .max(0);
            self.clock_master = Some(ClockMaster::Synthetic);
            self.reanchor_at_phase(now, phase_ns);
        }
        self.audio_device_anchor = None;
        if matches!(
            self.state,
            TransportState::Playing | TransportState::Recovering
        ) {
            self.refresh_frame_demand_if_target_changed(now)?;
        }
        Ok(())
    }

    fn reset_runtime_policy(&mut self) {
        self.preview_scale = PreviewResolutionScale::Full;
        self.quality_revision = self.quality_revision.saturating_add(1);
        self.recent_pressure.clear();
        self.consecutive_healthy = 0;
        self.active_demand = None;
        self.terminal_delivery = None;
        self.priming_current_presentable = false;
        self.video_preroll_observation = None;
        self.audio_device_anchor = None;
        self.last_audio_observation = None;
        self.last_audio_handoff = None;
        self.audio_uncertain_since = None;
    }

    fn try_complete_observed_priming(&mut self) -> bool {
        if self.state != TransportState::Priming || !self.priming_current_presentable {
            return false;
        }
        let preroll_satisfied = if self.policy.minimum_video_preroll_frames == 0 {
            true
        } else {
            self.video_preroll_observation.is_some_and(|observation| {
                let required = self
                    .policy
                    .minimum_video_preroll_frames
                    .min(observation.available_media_frames);
                observation.ready_media_frames >= required
            })
        };
        if !preroll_satisfied {
            return false;
        }
        self.state = TransportState::Playing;
        self.clock_master = Some(ClockMaster::Synthetic);
        self.reanchor(self.last_timestamp);
        true
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

    fn refresh_untimed_frame_demand_if_target_changed(&mut self) -> Result<(), PlaybackError> {
        let current_matches = self.active_demand.is_some_and(|demand| {
            demand.epoch == self.epoch
                && demand.quality_revision == self.quality_revision
                && demand.target == self.position
                && demand.deadline.is_none()
        });
        if !current_matches {
            self.refresh_untimed_frame_demand()?;
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
            deadline: Some(now.saturating_add(useful_duration)),
            preview_scale: self.preview_scale,
        });
        self.terminal_delivery = None;
        Ok(())
    }

    fn refresh_untimed_frame_demand(&mut self) -> Result<(), PlaybackError> {
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
            deadline: None,
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

fn nonnegative_frame(mut value: FramePosition) -> FramePosition {
    value.frame = value.frame.max(0);
    value
}

fn sample_frames_duration(frames: u64, sample_rate: u32) -> Duration {
    if sample_rate == 0 {
        return Duration::MAX;
    }
    let nanos = (frames as u128)
        .saturating_mul(1_000_000_000)
        .checked_div(sample_rate as u128)
        .unwrap_or(u128::MAX);
    Duration::from_nanos(nanos.min(u64::MAX as u128) as u64)
}

fn audio_media_position_ns(
    observation: AudioDeviceClockObservation,
    effective_consumed_frames: u64,
) -> Result<i128, PlaybackError> {
    let anchor_ns = time_code_ns(observation.media_anchor)?;
    let consumed_ns = sample_frames_duration(effective_consumed_frames, observation.sample_rate)
        .as_nanos()
        .min(i128::MAX as u128) as i128;
    Ok(anchor_ns.saturating_add(consumed_ns))
}

fn time_code_ns(value: FramePosition) -> Result<i128, PlaybackError> {
    validate_time_base(value.time_base)?;
    let numerator = (value.frame as i128)
        .saturating_mul(value.time_base.num as i128)
        .saturating_mul(1_000_000_000);
    Ok(numerator / value.time_base.den as i128)
}

fn timeline_frame_at_ns(nanos: i128, time_base: Rational) -> Result<i64, PlaybackError> {
    validate_time_base(time_base)?;
    let denominator = (time_base.num as i128).saturating_mul(1_000_000_000);
    let frame = nanos.saturating_mul(time_base.den as i128) / denominator;
    Ok(frame.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
}

fn frame_duration(time_base: Rational) -> Result<Duration, PlaybackError> {
    validate_time_base(time_base)?;
    let numerator = 1_000_000_000_u128.saturating_mul(time_base.num as u128);
    let denominator = time_base.den as u128;
    let nanos = numerator.saturating_add(denominator - 1) / denominator;
    Ok(Duration::from_nanos(nanos.min(u64::MAX as u128) as u64))
}

#[cfg(test)]
#[path = "playback_engine/tests.rs"]
mod tests;
