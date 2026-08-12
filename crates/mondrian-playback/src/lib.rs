//! Headless realtime playback state, clock, and frame-delivery policy.
//!
//! This crate owns transport semantics but deliberately knows nothing about UI,
//! codecs, GPU resources, audio devices, or concrete timeline models.

use mondrian_core::{AudioSamplePosition, AudioSampleRate, FramePosition, Rational, SequenceId};
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

    /// Add a runtime duration and fail closed if it exceeds the clock domain.
    pub fn checked_add(self, duration: Duration) -> Result<Self, PlaybackError> {
        self.0
            .checked_add(duration)
            .map(Self)
            .ok_or(PlaybackError::TransportArithmeticOverflow)
    }

    /// Add a bounded runtime duration for explicitly saturating diagnostics or budgets.
    ///
    /// Transport and presentation authority must use [`Self::checked_add`].
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
        if end_frame < 0 {
            return Err(PlaybackError::InvalidTimelineExtent);
        }
        Ok(Self {
            sequence_id,
            timeline_revision,
            time_base,
            end_frame,
        })
    }

    fn validate_position(self, position: FramePosition) -> Result<(), PlaybackError> {
        validate_time_base(position.time_base)?;
        if position.frame < 0 {
            return Err(PlaybackError::NegativeTimelinePosition);
        }
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
    /// Bounded nanoseconds after `deadline` at which a running delivery is
    /// still presented as [`FrameDeliveryKind::Degraded`].
    pub late_presentation_grace_ns: u64,
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
    late_presentation_grace_ns: u64,
}

impl FramePresentationTicket {
    /// Create the presentation authority carried by an active demand.
    pub const fn for_demand(demand: FrameDemand, quality: FramePresentationQuality) -> Self {
        Self {
            identity: demand.identity(),
            deadline: demand.deadline,
            quality,
            late_presentation_grace_ns: demand.late_presentation_grace_ns,
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

    /// Classify a prospective completion without consuming Engine authority.
    ///
    /// Presentation adapters use this pure preflight before publishing an
    /// output. The same `completed_at` must then be passed to [`Self::complete_at`]
    /// after publication succeeds.
    pub fn delivery_kind_at(self, completed_at: MonotonicTimestamp) -> FrameDeliveryKind {
        let Some(deadline) = self.deadline else {
            return match self.quality {
                FramePresentationQuality::Ready => FrameDeliveryKind::Ready,
                FramePresentationQuality::Degraded => FrameDeliveryKind::Degraded,
            };
        };
        if completed_at < deadline {
            return match self.quality {
                FramePresentationQuality::Ready => FrameDeliveryKind::Ready,
                FramePresentationQuality::Degraded => FrameDeliveryKind::Degraded,
            };
        }
        let grace_end = if self.late_presentation_grace_ns == 0 {
            deadline
        } else {
            deadline
                .checked_add(Duration::from_nanos(self.late_presentation_grace_ns))
                .unwrap_or(deadline)
        };
        if completed_at <= grace_end {
            FrameDeliveryKind::Degraded
        } else {
            FrameDeliveryKind::Late
        }
    }

    /// Classify real presentation completion against the demand deadline.
    pub fn complete_at(self, completed_at: MonotonicTimestamp) -> FrameDelivery {
        FrameDeliveryCandidate::for_demand(self.identity, self.delivery_kind_at(completed_at))
            .complete_at(completed_at)
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

/// Why the Playback Engine requires its coordinator to wake next.
///
/// Adapters may use the reason to select a platform wait primitive, but cannot
/// reinterpret or extend the accompanying duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackWakeReason {
    /// Startup preroll reached its bounded fallback deadline.
    PrimingDeadline,
    /// The current Frame Demand reaches its final useful presentation time.
    PresentationDeadline,
    /// The authoritative media phase reaches the next exact video-frame boundary.
    FrameBoundary,
    /// Bounded fallback poll for a newly published Audio Device observation.
    AudioDevicePoll,
}

/// Exact next-wake obligation emitted by the Playback Engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackWake {
    after: Duration,
    reason: PlaybackWakeReason,
}

impl PlaybackWake {
    /// Remaining monotonic duration before the required wake.
    pub const fn after(self) -> Duration {
        self.after
    }

    /// Authoritative reason for the wake.
    pub const fn reason(self) -> PlaybackWakeReason {
        self.reason
    }
}

/// Quality grade of one audio-device clock observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioClockObservationGrade {
    /// Callback-consumed samples corrected by CPAL's predicted playback delay.
    ///
    /// This is not an exact hardware playback-head position. Its uncertainty
    /// remains explicit because CPAL backends derive the prediction from
    /// different native queue and latency evidence.
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
    /// Device callback sample rate; it must equal `media_anchor.rate()`.
    pub sample_rate: u32,
    /// Cumulative output frames consumed by callbacks in this stream generation.
    pub consumed_frames: u64,
    /// Exact output-sample position queued at callback-consumption frame zero.
    pub media_anchor: AudioSamplePosition,
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

/// Timestamp-free candidate for a non-presentation terminal outcome.
///
/// Presentation Adapters normally use [`FramePresentationTicket::complete_at`].
/// Decode failure, cancellation, and policy paths first construct this value,
/// then bind the one real terminal completion timestamp with [`Self::complete_at`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDeliveryCandidate {
    identity: FrameDemandIdentity,
    kind: FrameDeliveryKind,
}

/// Media-frame lookahead observed by the preview Adapter during startup.
///
/// `preservable_media_frames` is the complete immediate future media-bearing
/// prefix that the Adapter proves can coexist within its physical resource and
/// work-admission grants. `ready_media_frames` is the prefix of those frames
/// whose complete required media closures are already resident. The Engine
/// combines this observation with actual current-frame presentation; neither
/// signal can start the clock alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoPrerollObservation {
    /// Exact current-frame demand whose future lookahead was inspected.
    pub demand: FrameDemandIdentity,
    /// Consecutive future media frames ready for presentation preparation.
    pub ready_media_frames: usize,
    /// Complete future media-bearing prefix that can be preserved concurrently.
    pub preservable_media_frames: usize,
}

impl FrameDeliveryCandidate {
    /// Build a timestamp-free terminal candidate for an exact demand identity.
    pub const fn for_demand(identity: FrameDemandIdentity, kind: FrameDeliveryKind) -> Self {
        Self { identity, kind }
    }

    /// Recover the demand identity carried through the Adapter.
    pub const fn identity(self) -> FrameDemandIdentity {
        self.identity
    }

    /// Return the proposed terminal outcome.
    pub const fn kind(self) -> FrameDeliveryKind {
        self.kind
    }

    /// Bind the unique terminal completion timestamp.
    pub const fn complete_at(self, completed_at: MonotonicTimestamp) -> FrameDelivery {
        FrameDelivery {
            identity: self.identity,
            kind: self.kind,
            completed_at,
        }
    }
}

/// Terminal observation for a frame requested by the Playback Engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameDelivery {
    identity: FrameDemandIdentity,
    kind: FrameDeliveryKind,
    completed_at: MonotonicTimestamp,
}

impl FrameDelivery {
    /// Recover the demand identity carried through the Adapter.
    pub const fn identity(self) -> FrameDemandIdentity {
        self.identity
    }

    /// Return the terminal outcome classified at completion.
    pub const fn kind(self) -> FrameDeliveryKind {
        self.kind
    }

    /// Return the one timestamp at which this terminal outcome completed.
    pub const fn completed_at(self) -> MonotonicTimestamp {
        self.completed_at
    }
}

/// Exact active Clock Master phase observed at one delivery completion instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackClockPhaseObservation {
    epoch: PlaybackEpoch,
    master: ClockMaster,
    observed_at: MonotonicTimestamp,
    phase_ns: i128,
    uncertainty_ns: u128,
}

impl PlaybackClockPhaseObservation {
    /// Playback Session whose Clock Master produced this phase.
    pub const fn epoch(self) -> PlaybackEpoch {
        self.epoch
    }

    /// Clock Master that produced this phase.
    pub const fn master(self) -> ClockMaster {
        self.master
    }

    /// Exact monotonic instant shared with the terminal delivery.
    pub const fn observed_at(self) -> MonotonicTimestamp {
        self.observed_at
    }

    /// Engine-authoritative phase on its checked floor-nanosecond grid.
    pub const fn phase_ns(self) -> i128 {
        self.phase_ns
    }

    /// Conservative upper-bound phase uncertainty.
    pub const fn uncertainty_ns(self) -> u128 {
        self.uncertainty_ns
    }
}

/// Engine-authenticated result of applying one terminal Frame Delivery.
///
/// The fields are private and the value is intentionally neither `Clone` nor
/// `Copy`: only the Playback Engine can bind acceptance, post-commit state,
/// exact target, and Clock phase into one evidence-bearing application.
#[derive(Debug)]
pub struct FrameDeliveryApplication {
    delivery: FrameDelivery,
    accepted: bool,
    snapshot: PlaybackSnapshot,
    target: Option<FramePosition>,
    clock_phase: Option<PlaybackClockPhaseObservation>,
}

impl FrameDeliveryApplication {
    /// Terminal delivery submitted to the Engine.
    pub const fn delivery(&self) -> FrameDelivery {
        self.delivery
    }

    /// Whether this delivery consumed current Engine authority.
    pub const fn accepted(&self) -> bool {
        self.accepted
    }

    /// Authoritative state after the application attempt.
    pub const fn snapshot(&self) -> PlaybackSnapshot {
        self.snapshot
    }

    /// Exact accepted demand target, absent for rejected stale authority.
    pub const fn target(&self) -> Option<FramePosition> {
        self.target
    }

    /// Clock phase at the same completion instant, when a Clock Master exists.
    pub const fn clock_phase(&self) -> Option<PlaybackClockPhaseObservation> {
        self.clock_phase
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
    /// Largest proven media phase error (absolute point error plus uncertainty)
    /// allowed when selecting a new audio stream.
    pub max_audio_handoff_phase_error: Duration,
    /// Largest proven Clock-Master-to-frame-start phase error accepted for a
    /// presentable running video delivery.
    ///
    /// The Engine derives each Frame Demand deadline from this budget as well
    /// as the successor frame boundary. Adapters therefore cannot report a
    /// delivery as timely after it has already exceeded the product's A/V
    /// phase contract.
    pub max_video_presentation_phase_error: Duration,
    /// Bounded additional lateness after the Frame Demand deadline at which a
    /// running delivery is still presented, classified as [`FrameDeliveryKind::Degraded`].
    ///
    /// Decode/composite jitter that crosses the exact deadline by less than
    /// this grace is displayed rather than dropped, keeping the picture
    /// advancing through ordinary hiccups. Deliveries beyond the grace window
    /// remain [`FrameDeliveryKind::Late`] and are rejected without publication.
    pub late_presentation_grace: Duration,
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
            max_video_presentation_phase_error: Duration::from_millis(20),
            late_presentation_grace: Duration::from_millis(16),
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
    /// Conservative upper-bound uncertainty of the candidate audio phase.
    pub uncertainty_ns: u128,
    /// Absolute phase error plus uncertainty used for qualification.
    pub proven_phase_error_ns: u128,
    /// Qualification result.
    pub status: AudioClockHandoffStatus,
}

/// Atomic result of retiring one physical audio stream generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioDeviceLossApplication {
    snapshot: PlaybackSnapshot,
    final_observation_applied: bool,
}

impl AudioDeviceLossApplication {
    /// Authoritative snapshot after the Engine has selected Synthetic Clock
    /// Master (when transport is running) and retired device-clock authority.
    pub const fn snapshot(self) -> PlaybackSnapshot {
        self.snapshot
    }

    /// Whether a valid final callback observation from the exact authoritative
    /// stream generation contributed to the handoff phase.
    pub const fn final_observation_applied(self) -> bool {
        self.final_observation_applied
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
    /// Realtime transport positions are non-negative within the bound Sequence.
    #[error("playback timeline position must be non-negative")]
    NegativeTimelinePosition,
    /// A Timeline Binding cannot have a negative inclusive content boundary.
    #[error("playback timeline extent must be non-negative")]
    InvalidTimelineExtent,
    /// Exact transport time, clock phase, or identity arithmetic overflowed.
    #[error("playback transport arithmetic overflow")]
    TransportArithmeticOverflow,
    /// An audio observation carried internally inconsistent position evidence.
    #[error("audio clock observation position evidence is invalid")]
    InvalidAudioClockPosition,
    /// A request named a Playback Session other than the current epoch.
    #[error("playback epoch does not match the current session")]
    MismatchedPlaybackEpoch,
    /// Audio sample coordinates from different rates cannot be combined.
    #[error("audio sample rate does not match the active output anchor")]
    MismatchedAudioSampleRate,
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
    media_anchor: AudioSamplePosition,
    last_effective_consumed_frames: u64,
    last_observed_at: MonotonicTimestamp,
    last_media_position_ns: i128,
    last_uncertainty_frames: u32,
}

/// Deep, headless Module owning a Playback Session and its realtime invariants.
#[derive(Clone)]
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
            || policy.max_video_presentation_phase_error.is_zero()
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
        self.commit_candidate(move |candidate| {
            candidate.play_timeline_in_place(binding, position, now)
        })
    }

    fn play_timeline_in_place(
        &mut self,
        binding: PlaybackTimelineBinding,
        position: FramePosition,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        binding.validate_position(position)?;
        self.accept_timestamp(now)?;
        self.apply_timeline_binding(binding);
        self.position = position;
        if self.position.frame > self.end_frame {
            self.position.frame = 0;
        }
        self.bump_epoch()?;
        self.state = TransportState::Priming;
        self.clock_master = Some(ClockMaster::Synthetic);
        self.reset_runtime_policy()?;
        self.refresh_frame_demand_with_duration(now, self.policy.priming_limit)?;
        self.reanchor(now)?;
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
        self.commit_candidate(move |candidate| {
            candidate.seek_timeline_in_place(binding, position, now)
        })
    }

    fn seek_timeline_in_place(
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
        if end_frame < 0 {
            return Err(PlaybackError::InvalidTimelineExtent);
        }
        self.end_frame = end_frame;
        if self.state == TransportState::Ended || self.position.frame > self.end_frame {
            self.position.frame = 0;
        }
        self.bump_epoch()?;
        self.state = TransportState::Priming;
        self.clock_master = Some(ClockMaster::Synthetic);
        self.reset_runtime_policy()?;
        self.refresh_frame_demand_with_duration(now, self.policy.priming_limit)?;
        self.reanchor(now)?;
        Ok(self.snapshot())
    }

    /// Complete priming without changing the current timeline position.
    pub fn complete_priming(
        &mut self,
        master: ClockMaster,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.commit_candidate(move |candidate| candidate.complete_priming_in_place(master, now))
    }

    fn complete_priming_in_place(
        &mut self,
        master: ClockMaster,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.accept_timestamp(now)?;
        if self.state == TransportState::Priming {
            if master == ClockMaster::AudioDevice && self.audio_device_anchor.is_none() {
                return Err(PlaybackError::InvalidAudioClockPosition);
            }
            self.clock_master = Some(master);
            self.state = TransportState::Playing;
            self.reanchor(now)?;
            self.refresh_frame_demand(now)?;
        }
        Ok(self.snapshot())
    }

    /// Pause at the authoritative position observed at `now`.
    pub fn pause(&mut self, now: MonotonicTimestamp) -> Result<PlaybackSnapshot, PlaybackError> {
        self.commit_candidate(move |candidate| candidate.pause_in_place(now))
    }

    fn pause_in_place(
        &mut self,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        if !matches!(
            self.state,
            TransportState::Priming | TransportState::Playing | TransportState::Recovering
        ) {
            // Pause is an idempotent transport command, not an escape hatch
            // from a correctness blocker or natural end. Still accept the
            // caller's monotonic sample so a later command cannot move time
            // backwards. The command also establishes a fresh stable-output
            // obligation when the previous one already terminated without a
            // presentable frame.
            self.accept_timestamp(now)?;
            self.ensure_pending_untimed_frame_demand()?;
            return Ok(self.snapshot());
        }
        self.advance_position(now)?;
        if self.state != TransportState::Ended {
            self.state = TransportState::Paused;
            self.clock_master = None;
            self.reanchor(now)?;
        }
        self.ensure_pending_untimed_frame_demand()?;
        Ok(self.snapshot())
    }

    /// Stop and return to frame zero.
    pub fn stop(&mut self, now: MonotonicTimestamp) -> Result<PlaybackSnapshot, PlaybackError> {
        self.commit_candidate(move |candidate| candidate.stop_in_place(now))
    }

    fn stop_in_place(
        &mut self,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.accept_timestamp(now)?;
        self.bump_epoch()?;
        self.position.frame = 0;
        self.state = TransportState::Stopped;
        self.clock_master = None;
        self.reset_runtime_policy()?;
        self.reanchor(now)?;
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
        if position.frame < 0 {
            return Err(PlaybackError::NegativeTimelinePosition);
        }
        let was_running = self.transport_intends_playback();
        self.seek_after_timestamp(position, was_running, now)
    }

    fn seek_after_timestamp(
        &mut self,
        position: FramePosition,
        was_running: bool,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.bump_epoch()?;
        self.position = position;
        self.state = if was_running {
            TransportState::Priming
        } else {
            TransportState::Paused
        };
        self.clock_master = was_running.then_some(ClockMaster::Synthetic);
        self.reset_runtime_policy()?;
        self.reanchor(now)?;
        if was_running {
            self.refresh_frame_demand_with_duration(now, self.policy.priming_limit)?;
        } else {
            self.refresh_untimed_frame_demand()?;
        }
        Ok(self.snapshot())
    }

    /// Advance the active Clock Master to `now` and publish the newest frame.
    pub fn tick(&mut self, now: MonotonicTimestamp) -> Result<PlaybackSnapshot, PlaybackError> {
        self.commit_candidate(move |candidate| candidate.tick_in_place(now))
    }

    fn tick_in_place(
        &mut self,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.accept_timestamp(now)?;
        if self.state == TransportState::Priming
            && let Some(deadline) = self
                .active_demand
                .and_then(|demand| demand.deadline)
                .filter(|deadline| now >= *deadline)
        {
            self.state = TransportState::Playing;
            self.clock_master = Some(ClockMaster::Synthetic);
            self.reanchor(deadline)?;
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

    /// Return the exact remaining duration to the next required Engine wake.
    ///
    /// Priming exposes its bounded fallback deadline even after the current
    /// Frame Demand has received a terminal non-presentable delivery. Running
    /// playback exposes the earlier of the pending presentation phase
    /// deadline and its normal Clock wake. Audio Device playback retains a
    /// bounded polling fallback because a callback sample may arrive earlier.
    pub fn time_until_next_wake(
        &self,
        now: MonotonicTimestamp,
    ) -> Result<Option<Duration>, PlaybackError> {
        Ok(self.next_wake(now)?.map(PlaybackWake::after))
    }

    /// Return the exact next-wake duration together with its scheduling reason.
    pub fn next_wake(
        &self,
        now: MonotonicTimestamp,
    ) -> Result<Option<PlaybackWake>, PlaybackError> {
        if self.state == TransportState::Priming {
            let Some(deadline) = self.active_demand.and_then(|demand| demand.deadline) else {
                return Ok(None);
            };
            if now >= deadline {
                return Ok(Some(PlaybackWake {
                    after: Duration::ZERO,
                    reason: PlaybackWakeReason::PrimingDeadline,
                }));
            }
            return deadline
                .duration_since_origin()
                .checked_sub(now.duration_since_origin())
                .map(|after| {
                    Some(PlaybackWake { after, reason: PlaybackWakeReason::PrimingDeadline })
                })
                .ok_or(PlaybackError::NonMonotonicTimestamp);
        }
        if !matches!(
            self.state,
            TransportState::Playing | TransportState::Recovering
        ) {
            return Ok(None);
        }
        let presentation_wake =
            self.pending_frame_demand().and_then(|demand| demand.deadline).map(|deadline| {
                PlaybackWake {
                    after: deadline
                        .duration_since_origin()
                        .checked_sub(now.duration_since_origin())
                        .unwrap_or(Duration::ZERO),
                    reason: PlaybackWakeReason::PresentationDeadline,
                }
            });
        let phase_ns = self.clock_phase_reference_ns(now)?;
        let current_frame = timeline_frame_at_ns(phase_ns, self.position.time_base)?;
        let successor =
            current_frame.checked_add(1).ok_or(PlaybackError::TransportArithmeticOverflow)?;
        let next_boundary_ns =
            timeline_frame_boundary_ns(FramePosition::new(successor, self.position.time_base))?;
        let remaining_ns = next_boundary_ns
            .checked_sub(phase_ns)
            .ok_or(PlaybackError::TransportArithmeticOverflow)?;
        let frame_boundary_wake = nonnegative_ns_duration(remaining_ns)?;
        let clock_wake = if self.clock_master == Some(ClockMaster::AudioDevice) {
            // Callback publication can move the physical Audio Device Clock
            // earlier than its extrapolated position, so retain a bounded poll.
            // The projected video boundary is independently authoritative:
            // sleeping the full poll when that boundary is closer would mint
            // the successor Frame Demand late and needlessly spend its phase
            // budget before any Presentation Adapter could act.
            if frame_boundary_wake <= Duration::from_millis(2) {
                PlaybackWake {
                    after: frame_boundary_wake,
                    reason: PlaybackWakeReason::FrameBoundary,
                }
            } else {
                PlaybackWake {
                    after: Duration::from_millis(2),
                    reason: PlaybackWakeReason::AudioDevicePoll,
                }
            }
        } else {
            PlaybackWake {
                after: frame_boundary_wake,
                reason: PlaybackWakeReason::FrameBoundary,
            }
        };
        Ok(Some(presentation_wake.map_or(clock_wake, |wake| {
            if wake.after <= clock_wake.after {
                wake
            } else {
                clock_wake
            }
        })))
    }

    /// Hand off from an unavailable audio device to a continuous synthetic clock.
    pub fn audio_device_lost(
        &mut self,
        now: MonotonicTimestamp,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        self.commit_candidate(move |candidate| candidate.audio_device_lost_in_place(now))
    }

    /// Retire one exact physical stream generation and optionally apply its
    /// frozen final callback observation before the continuous handoff.
    ///
    /// The optional observation is advisory continuity evidence: it is applied
    /// only when it belongs to the current epoch and the exact stream that is
    /// presently Audio Device Clock Master. Invalid, stale, or unrelated final
    /// evidence is ignored, but confirmed physical loss still atomically
    /// selects Synthetic Clock Master. The method fails only if the mandatory
    /// loss handoff itself cannot be represented.
    pub fn audio_device_lost_with_final_observation(
        &mut self,
        stream_generation: u64,
        final_observation: Option<AudioDeviceClockObservation>,
        observed_at: MonotonicTimestamp,
    ) -> Result<AudioDeviceLossApplication, PlaybackError> {
        self.commit_candidate(move |candidate| {
            candidate.audio_device_lost_with_final_observation_in_place(
                stream_generation,
                final_observation,
                observed_at,
            )
        })
    }

    fn audio_device_lost_with_final_observation_in_place(
        &mut self,
        stream_generation: u64,
        final_observation: Option<AudioDeviceClockObservation>,
        observed_at: MonotonicTimestamp,
    ) -> Result<AudioDeviceLossApplication, PlaybackError> {
        observed_at.checked_elapsed_since(self.last_timestamp)?;
        let authoritative_generation = self
            .audio_device_anchor
            .filter(|_| self.clock_master == Some(ClockMaster::AudioDevice))
            .map(|anchor| anchor.stream_generation);
        let mut final_observation_applied = false;
        if authoritative_generation == Some(stream_generation)
            && let Some(observation) = final_observation.filter(|observation| {
                observation.epoch == self.epoch
                    && observation.stream_generation == stream_generation
                    && observation.observed_at >= self.last_timestamp
                    && observation.observed_at <= observed_at
            })
        {
            let mut observed_candidate = self.clone();
            if observed_candidate.observe_audio_device_clock_in_place(observation).is_ok() {
                *self = observed_candidate;
                final_observation_applied = true;
            }
        }
        let snapshot = self.audio_device_lost_in_place(observed_at)?;
        Ok(AudioDeviceLossApplication { snapshot, final_observation_applied })
    }

    fn audio_device_lost_in_place(
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
        self.commit_candidate(move |candidate| {
            candidate.observe_audio_device_clock_in_place(observation)
        })
    }

    fn observe_audio_device_clock_in_place(
        &mut self,
        observation: AudioDeviceClockObservation,
    ) -> Result<PlaybackSnapshot, PlaybackError> {
        if observation.epoch != self.epoch {
            return Ok(self.snapshot());
        }
        let observation_rate = AudioSampleRate::new(observation.sample_rate)
            .map_err(|_| PlaybackError::InvalidAudioSampleRate)?;
        if observation.media_anchor.rate() != observation_rate {
            return Err(PlaybackError::MismatchedAudioSampleRate);
        }
        if observation.media_anchor.sample() < 0 {
            return Err(PlaybackError::InvalidAudioClockPosition);
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
            let uncertain_elapsed = observation
                .observed_at
                .duration_since_origin()
                .checked_sub(uncertain_since.duration_since_origin())
                .ok_or(PlaybackError::NonMonotonicTimestamp)?;
            if self.clock_master == Some(ClockMaster::AudioDevice)
                && uncertain_elapsed <= self.policy.audio_clock_uncertainty_grace
            {
                return Ok(self.snapshot());
            }
            self.handoff_to_synthetic(observation.observed_at)?;
            return Ok(self.snapshot());
        }
        self.audio_uncertain_since = None;
        let uncertainty_ns =
            sample_frames_ns_ceil(u64::from(observation.uncertainty_frames), observation_rate)?;
        if uncertainty_ns > self.policy.max_audio_clock_uncertainty.as_nanos() {
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
            .checked_sub(observation.estimated_latency_frames as u64)
            .ok_or(PlaybackError::InvalidAudioClockPosition)?;
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
            let phase_error_ns = candidate_ns
                .checked_sub(reference_ns)
                .ok_or(PlaybackError::TransportArithmeticOverflow)?;
            let phase_error_abs = phase_error_ns.unsigned_abs();
            let proven_phase_error_ns = phase_error_abs
                .checked_add(uncertainty_ns)
                .ok_or(PlaybackError::TransportArithmeticOverflow)?;
            let accepted =
                proven_phase_error_ns <= self.policy.max_audio_handoff_phase_error.as_nanos();
            self.last_audio_handoff = Some(AudioClockHandoffEvidence {
                stream_generation: observation.stream_generation,
                phase_error_ns: i64::try_from(phase_error_ns)
                    .map_err(|_| PlaybackError::TransportArithmeticOverflow)?,
                uncertainty_ns,
                proven_phase_error_ns,
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
            self.refresh_frame_demand_for_clock_handoff(observation.observed_at)?;
            return Ok(self.snapshot());
        };

        let observed_elapsed =
            observation.observed_at.checked_elapsed_since(anchor.last_observed_at)?;
        let consumed_delta = effective_consumed
            .checked_sub(anchor.last_effective_consumed_frames)
            .ok_or(PlaybackError::InvalidAudioClockPosition)?;
        let allowed_elapsed_ns = observed_elapsed
            .as_nanos()
            .checked_add(sample_frames_ns_ceil(
                u64::from(observation.uncertainty_frames.max(anchor.last_uncertainty_frames)),
                observation_rate,
            )?)
            .ok_or(PlaybackError::TransportArithmeticOverflow)?;
        if sample_frames_exceed_duration_ns(consumed_delta, observation_rate, allowed_elapsed_ns)? {
            self.handoff_to_synthetic(observation.observed_at)?;
            return Ok(self.snapshot());
        }

        let media_position_ns = audio_media_position_ns(observation, effective_consumed)?;
        let target = timeline_frame_at_ns(media_position_ns, self.position.time_base)?;
        self.position.frame = self.position.frame.max(target).min(self.end_frame);
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

    /// Record a timestamp-bound Frame Delivery and apply bounded recovery policy.
    ///
    /// Rejected stale authority is returned as an authenticated application but
    /// cannot mutate the current Playback Session or advance its monotonic
    /// timestamp. Accepted applications bind the exact target and active Clock
    /// phase at the delivery's own completion timestamp.
    pub fn observe_frame_delivery(
        &mut self,
        delivery: FrameDelivery,
    ) -> Result<FrameDeliveryApplication, PlaybackError> {
        self.commit_candidate(move |candidate| candidate.observe_frame_delivery_in_place(delivery))
    }

    fn observe_frame_delivery_in_place(
        &mut self,
        delivery: FrameDelivery,
    ) -> Result<FrameDeliveryApplication, PlaybackError> {
        let identity = delivery.identity();
        if identity.epoch != self.epoch || identity.quality_revision != self.quality_revision {
            return Ok(FrameDeliveryApplication {
                delivery,
                accepted: false,
                snapshot: self.snapshot(),
                target: None,
                clock_phase: None,
            });
        }
        let terminal_identity = (identity.epoch, identity.quality_revision, identity.sequence);
        if self.terminal_delivery == Some(terminal_identity) {
            return Ok(FrameDeliveryApplication {
                delivery,
                accepted: false,
                snapshot: self.snapshot(),
                target: None,
                clock_phase: None,
            });
        }
        let Some(active_demand) =
            self.active_demand.filter(|demand| demand.sequence == identity.sequence)
        else {
            return Ok(FrameDeliveryApplication {
                delivery,
                accepted: false,
                snapshot: self.snapshot(),
                target: None,
                clock_phase: None,
            });
        };
        if active_demand.target.frame != identity.target_frame {
            return Err(PlaybackError::MismatchedFrameDelivery);
        }
        let completed_at = delivery.completed_at();
        self.accept_timestamp(completed_at)?;
        if delivery.kind() == FrameDeliveryKind::Blocked {
            self.terminal_delivery = Some(terminal_identity);
            self.state = TransportState::Blocked;
            self.clock_master = None;
            return Ok(FrameDeliveryApplication {
                delivery,
                accepted: true,
                snapshot: self.snapshot(),
                target: Some(active_demand.target),
                clock_phase: None,
            });
        }

        let pressured = matches!(
            delivery.kind(),
            FrameDeliveryKind::Late | FrameDeliveryKind::Degraded | FrameDeliveryKind::Failed
        );
        self.terminal_delivery = Some(terminal_identity);
        let presentable = matches!(
            delivery.kind(),
            FrameDeliveryKind::Ready | FrameDeliveryKind::Degraded
        );
        let healthy = delivery.kind() == FrameDeliveryKind::Ready;
        if self.state == TransportState::Priming && presentable {
            self.priming_current_presentable = true;
            self.try_complete_observed_priming(completed_at)?;
        }
        self.push_pressure(pressured);
        if healthy {
            self.consecutive_healthy = self
                .consecutive_healthy
                .checked_add(1)
                .ok_or(PlaybackError::TransportArithmeticOverflow)?;
        } else if delivery.kind() != FrameDeliveryKind::Canceled {
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
                self.quality_revision = self
                    .quality_revision
                    .checked_add(1)
                    .ok_or(PlaybackError::TransportArithmeticOverflow)?;
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
            self.quality_revision = self
                .quality_revision
                .checked_add(1)
                .ok_or(PlaybackError::TransportArithmeticOverflow)?;
            self.refresh_frame_demand(self.last_timestamp)?;
            self.consecutive_healthy = 0;
            if self.preview_scale == PreviewResolutionScale::Full {
                self.state = TransportState::Playing;
            }
        }
        let clock_phase = self.playback_clock_phase_observation_at(completed_at)?;
        Ok(FrameDeliveryApplication {
            delivery,
            accepted: true,
            snapshot: self.snapshot(),
            target: Some(active_demand.target),
            clock_phase,
        })
    }

    /// Record bounded startup media lookahead from the preview Adapter.
    ///
    /// Old Playback Sessions are ignored. A valid observation can release
    /// `Priming` only after the current Frame Demand has also been presented.
    /// `observed_at` is the instant at which the Adapter finished deriving the
    /// readiness fact; when this is the second condition, it becomes the
    /// Synthetic Clock anchor.
    pub fn observe_video_preroll(
        &mut self,
        observation: VideoPrerollObservation,
        observed_at: MonotonicTimestamp,
    ) -> Result<bool, PlaybackError> {
        self.commit_candidate(move |candidate| {
            candidate.observe_video_preroll_in_place(observation, observed_at)
        })
    }

    fn observe_video_preroll_in_place(
        &mut self,
        observation: VideoPrerollObservation,
        observed_at: MonotonicTimestamp,
    ) -> Result<bool, PlaybackError> {
        if self.active_demand.map(FrameDemand::identity) != Some(observation.demand) {
            return Ok(false);
        }
        if observation.ready_media_frames > observation.preservable_media_frames
            || observation.preservable_media_frames > MAX_BOUNDED_VIDEO_PREROLL_FRAMES
        {
            return Err(PlaybackError::InvalidVideoPrerollObservation);
        }
        if self.state != TransportState::Priming
            || self.video_preroll_observation == Some(observation)
        {
            return Ok(false);
        }
        self.accept_timestamp(observed_at)?;
        self.video_preroll_observation = Some(observation);
        self.try_complete_observed_priming(observed_at)
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

    /// Latest monotonic timestamp accepted by authoritative Engine state.
    ///
    /// Adapters use this as their sole execution high-water mark. Evidence
    /// collectors observe it but never advance it.
    pub const fn monotonic_high_water(&self) -> MonotonicTimestamp {
        self.last_timestamp
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

    /// Lower the authoritative phase at one exact monotonic instant to an
    /// output-sample coordinate.
    ///
    /// The caller must bind the current Playback Epoch explicitly. While Audio
    /// Device is Clock Master, the requested rate must also match the active
    /// stream anchor. Conversion uses the Engine's checked floor-nanosecond
    /// phase grid followed by nearest-sample rounding; no video-frame
    /// quantization participates in this boundary.
    pub fn authoritative_audio_sample_position_at(
        &self,
        epoch: PlaybackEpoch,
        observed_at: MonotonicTimestamp,
        sample_rate: AudioSampleRate,
    ) -> Result<AudioSamplePosition, PlaybackError> {
        if epoch != self.epoch {
            return Err(PlaybackError::MismatchedPlaybackEpoch);
        }
        observed_at.checked_elapsed_since(self.last_timestamp)?;
        if self.clock_master == Some(ClockMaster::AudioDevice) {
            let anchor =
                self.audio_device_anchor.ok_or(PlaybackError::InvalidAudioClockPosition)?;
            if anchor.media_anchor.rate() != sample_rate {
                return Err(PlaybackError::MismatchedAudioSampleRate);
            }
        }
        let phase_ns = self.clock_phase_reference_ns(observed_at)?;
        audio_sample_position_at_phase_ns(phase_ns, sample_rate)
    }

    /// Timeline revision currently associated with the session.
    pub const fn timeline_revision(&self) -> u64 {
        self.timeline_revision
    }

    /// Sequence currently associated with the session.
    pub const fn sequence_id(&self) -> Option<SequenceId> {
        self.sequence_id
    }

    fn commit_candidate<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T, PlaybackError>,
    ) -> Result<T, PlaybackError> {
        let mut candidate = self.clone();
        let result = operation(&mut candidate)?;
        *self = candidate;
        Ok(result)
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
            self.audio_phase_ns_at(now)?.ok_or(PlaybackError::InvalidAudioClockPosition)?
        } else {
            self.synthetic_phase_ns_at(now)?
        };
        let target = timeline_frame_at_ns(phase_ns, self.position.time_base)?;
        self.position.frame = target.min(self.end_frame);
        if self.position.frame >= self.end_frame {
            self.state = TransportState::Ended;
            self.clock_master = None;
        }
        Ok(())
    }

    fn clock_phase_reference_ns(&self, now: MonotonicTimestamp) -> Result<i128, PlaybackError> {
        if self.clock_master == Some(ClockMaster::AudioDevice) {
            return self.audio_phase_ns_at(now)?.ok_or(PlaybackError::InvalidAudioClockPosition);
        }
        if self.clock_master == Some(ClockMaster::Synthetic)
            && matches!(
                self.state,
                TransportState::Playing | TransportState::Recovering
            )
        {
            return self.synthetic_phase_ns_at(now);
        }
        timeline_position_ns_floor(self.position)
    }

    fn playback_clock_phase_observation_at(
        &self,
        observed_at: MonotonicTimestamp,
    ) -> Result<Option<PlaybackClockPhaseObservation>, PlaybackError> {
        let Some(master) = self.clock_master else {
            return Ok(None);
        };
        let phase_ns = self.clock_phase_reference_ns(observed_at)?;
        let uncertainty_ns = match master {
            ClockMaster::Synthetic => 0,
            ClockMaster::AudioDevice => self.audio_clock_uncertainty_ns_at(observed_at)?,
        };
        Ok(Some(PlaybackClockPhaseObservation {
            epoch: self.epoch,
            master,
            observed_at,
            phase_ns,
            uncertainty_ns,
        }))
    }

    fn audio_phase_ns_at(&self, now: MonotonicTimestamp) -> Result<Option<i128>, PlaybackError> {
        let Some(anchor) = self.audio_device_anchor else {
            return Ok(None);
        };
        let elapsed = now.checked_elapsed_since(anchor.last_observed_at)?;
        let elapsed_ns = i128::try_from(elapsed.as_nanos())
            .map_err(|_| PlaybackError::TransportArithmeticOverflow)?;
        Ok(Some(
            anchor
                .last_media_position_ns
                .checked_add(elapsed_ns)
                .ok_or(PlaybackError::TransportArithmeticOverflow)?,
        ))
    }

    fn audio_clock_uncertainty_ns_at(
        &self,
        now: MonotonicTimestamp,
    ) -> Result<u128, PlaybackError> {
        let anchor = self.audio_device_anchor.ok_or(PlaybackError::InvalidAudioClockPosition)?;
        let callback_age_ns = now.checked_elapsed_since(anchor.last_observed_at)?.as_nanos();
        sample_frames_ns_ceil(
            u64::from(anchor.last_uncertainty_frames),
            anchor.media_anchor.rate(),
        )?
        .checked_add(callback_age_ns)
        .ok_or(PlaybackError::TransportArithmeticOverflow)
    }

    fn synthetic_phase_ns_at(&self, now: MonotonicTimestamp) -> Result<i128, PlaybackError> {
        let elapsed = now.checked_elapsed_since(self.clock_anchor.monotonic)?;
        let elapsed_ns = i128::try_from(elapsed.as_nanos())
            .map_err(|_| PlaybackError::TransportArithmeticOverflow)?;
        self.clock_anchor
            .phase_ns
            .checked_add(elapsed_ns)
            .ok_or(PlaybackError::TransportArithmeticOverflow)
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

    fn bump_epoch(&mut self) -> Result<(), PlaybackError> {
        self.epoch = PlaybackEpoch(
            self.epoch.0.checked_add(1).ok_or(PlaybackError::TransportArithmeticOverflow)?,
        );
        Ok(())
    }

    fn reanchor(&mut self, now: MonotonicTimestamp) -> Result<(), PlaybackError> {
        let phase_ns = timeline_position_ns_floor(self.position)?;
        self.reanchor_at_phase(now, phase_ns);
        Ok(())
    }

    fn reanchor_at_phase(&mut self, now: MonotonicTimestamp, phase_ns: i128) {
        self.clock_anchor = ClockAnchor { phase_ns, monotonic: now };
    }

    fn handoff_to_synthetic(&mut self, now: MonotonicTimestamp) -> Result<(), PlaybackError> {
        let handed_off = matches!(
            self.state,
            TransportState::Playing | TransportState::Recovering
        ) && self.clock_master != Some(ClockMaster::Synthetic);
        if handed_off {
            let phase_ns = if self.clock_master == Some(ClockMaster::AudioDevice) {
                self.audio_phase_ns_at(now)?.ok_or(PlaybackError::InvalidAudioClockPosition)?
            } else {
                timeline_position_ns_floor(self.position)?
            };
            self.position.frame =
                timeline_frame_at_ns(phase_ns, self.position.time_base)?.min(self.end_frame);
            self.clock_master = Some(ClockMaster::Synthetic);
            self.reanchor_at_phase(now, phase_ns);
        }
        self.audio_device_anchor = None;
        if matches!(
            self.state,
            TransportState::Playing | TransportState::Recovering
        ) {
            if handed_off {
                self.refresh_frame_demand_for_clock_handoff(now)?;
            } else {
                self.refresh_frame_demand_if_target_changed(now)?;
            }
        }
        Ok(())
    }

    fn reset_runtime_policy(&mut self) -> Result<(), PlaybackError> {
        self.preview_scale = PreviewResolutionScale::Full;
        self.quality_revision = self
            .quality_revision
            .checked_add(1)
            .ok_or(PlaybackError::TransportArithmeticOverflow)?;
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
        Ok(())
    }

    fn try_complete_observed_priming(
        &mut self,
        observed_at: MonotonicTimestamp,
    ) -> Result<bool, PlaybackError> {
        if self.state != TransportState::Priming || !self.priming_current_presentable {
            return Ok(false);
        }
        let preroll_satisfied = if self.policy.minimum_video_preroll_frames == 0 {
            true
        } else {
            self.video_preroll_observation.is_some_and(|observation| {
                let required = self
                    .policy
                    .minimum_video_preroll_frames
                    .min(observation.preservable_media_frames);
                observation.ready_media_frames >= required
            })
        };
        if !preroll_satisfied {
            return Ok(false);
        }
        self.state = TransportState::Playing;
        self.clock_master = Some(ClockMaster::Synthetic);
        self.reanchor(observed_at)?;
        Ok(true)
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

    fn refresh_frame_demand_for_clock_handoff(
        &mut self,
        now: MonotonicTimestamp,
    ) -> Result<(), PlaybackError> {
        if self.pending_frame_demand().is_some() {
            self.refresh_frame_demand(now)
        } else {
            self.refresh_frame_demand_if_target_changed(now)
        }
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

    fn ensure_pending_untimed_frame_demand(&mut self) -> Result<(), PlaybackError> {
        let pending_matches = self.pending_frame_demand().is_some_and(|demand| {
            demand.epoch == self.epoch
                && demand.quality_revision == self.quality_revision
                && demand.target == self.position
                && demand.deadline.is_none()
        });
        if !pending_matches {
            self.refresh_untimed_frame_demand()?;
        }
        Ok(())
    }

    fn refresh_frame_demand(&mut self, now: MonotonicTimestamp) -> Result<(), PlaybackError> {
        let computed_deadline = self.computed_frame_demand_deadline(now)?;
        let deadline = self
            .active_demand
            .filter(|demand| {
                demand.epoch == self.epoch
                    && demand.target == self.position
                    && demand.deadline.is_some()
            })
            .and_then(|demand| demand.deadline)
            .map_or(computed_deadline, |existing| {
                existing.min(computed_deadline)
            });
        self.refresh_frame_demand_with_deadline(deadline)
    }

    fn computed_frame_demand_deadline(
        &self,
        now: MonotonicTimestamp,
    ) -> Result<MonotonicTimestamp, PlaybackError> {
        let phase_ns = self.clock_phase_reference_ns(now)?;
        let successor = self
            .position
            .frame
            .checked_add(1)
            .ok_or(PlaybackError::TransportArithmeticOverflow)?;
        let boundary_ns =
            timeline_frame_boundary_ns(FramePosition::new(successor, self.position.time_base))?;
        let remaining_ns = boundary_ns
            .checked_sub(phase_ns)
            .ok_or(PlaybackError::TransportArithmeticOverflow)?;
        let frame_boundary_deadline =
            checked_timestamp_add(now, nonnegative_ns_duration(remaining_ns)?)?;
        let phase_deadline = self.video_presentation_phase_deadline(now, phase_ns)?;
        Ok(frame_boundary_deadline.min(phase_deadline))
    }

    fn video_presentation_phase_deadline(
        &self,
        now: MonotonicTimestamp,
        phase_ns: i128,
    ) -> Result<MonotonicTimestamp, PlaybackError> {
        let target_ns = timeline_position_ns_floor(self.position)?;
        let point_error_ns = phase_ns
            .checked_sub(target_ns)
            .ok_or(PlaybackError::TransportArithmeticOverflow)?
            .unsigned_abs();
        let uncertainty_ns = match self.clock_master {
            Some(ClockMaster::AudioDevice) => self.audio_clock_uncertainty_ns_at(now)?,
            _ => 0,
        };
        let proven_error_ns = point_error_ns
            .checked_add(uncertainty_ns)
            .ok_or(PlaybackError::TransportArithmeticOverflow)?;
        let remaining_budget_ns = self
            .policy
            .max_video_presentation_phase_error
            .as_nanos()
            .saturating_sub(proven_error_ns);

        // Between callback observations, the extrapolated audio point phase
        // advances once while callback-age uncertainty advances once more.
        // Synthetic Clock has no such uncertainty growth. An earlier retained
        // deadline is intentionally never extended by a later observation.
        let allowable_delay_ns = if self.clock_master == Some(ClockMaster::AudioDevice) {
            remaining_budget_ns / 2
        } else {
            remaining_budget_ns
        };
        let allowable_delay_ns = i128::try_from(allowable_delay_ns)
            .map_err(|_| PlaybackError::TransportArithmeticOverflow)?;
        checked_timestamp_add(now, nonnegative_ns_duration(allowable_delay_ns)?)
    }

    fn refresh_frame_demand_with_duration(
        &mut self,
        now: MonotonicTimestamp,
        useful_duration: Duration,
    ) -> Result<(), PlaybackError> {
        self.refresh_frame_demand_with_deadline(checked_timestamp_add(now, useful_duration)?)
    }

    fn refresh_frame_demand_with_deadline(
        &mut self,
        deadline: MonotonicTimestamp,
    ) -> Result<(), PlaybackError> {
        validate_time_base(self.position.time_base)?;
        let sequence = FrameDemandSequence(self.next_demand_sequence);
        self.next_demand_sequence = self
            .next_demand_sequence
            .checked_add(1)
            .ok_or(PlaybackError::TransportArithmeticOverflow)?;
        self.active_demand = Some(FrameDemand {
            epoch: self.epoch,
            quality_revision: self.quality_revision,
            sequence,
            sequence_id: self.sequence_id,
            timeline_revision: self.timeline_revision,
            target: self.position,
            deadline: Some(deadline),
            late_presentation_grace_ns: self.late_presentation_grace_ns()?,
            preview_scale: self.preview_scale,
        });
        self.terminal_delivery = None;
        Ok(())
    }

    fn refresh_untimed_frame_demand(&mut self) -> Result<(), PlaybackError> {
        validate_time_base(self.position.time_base)?;
        let sequence = FrameDemandSequence(self.next_demand_sequence);
        self.next_demand_sequence = self
            .next_demand_sequence
            .checked_add(1)
            .ok_or(PlaybackError::TransportArithmeticOverflow)?;
        self.active_demand = Some(FrameDemand {
            epoch: self.epoch,
            quality_revision: self.quality_revision,
            sequence,
            sequence_id: self.sequence_id,
            timeline_revision: self.timeline_revision,
            target: self.position,
            deadline: None,
            late_presentation_grace_ns: 0,
            preview_scale: self.preview_scale,
        });
        self.terminal_delivery = None;
        Ok(())
    }

    /// Bound the late-presentation grace so a degraded delivery is never
    /// published more than one half frame interval after the last useful
    /// presentation time. The phase budget already constrains the deadline;
    /// this clamps only the jitter absorption window.
    fn late_presentation_grace_ns(&self) -> Result<u64, PlaybackError> {
        let policy_grace_ns =
            u64::try_from(self.policy.late_presentation_grace.as_nanos()).unwrap_or(u64::MAX);
        if policy_grace_ns == 0 {
            return Ok(0);
        }
        let frame_boundary_ns =
            timeline_frame_boundary_ns(FramePosition::new(1, self.position.time_base))?;
        let half_frame_ns = frame_boundary_ns.checked_div(2).unwrap_or(i128::MAX);
        let half_frame_ns = u64::try_from(half_frame_ns).unwrap_or(u64::MAX);
        Ok(policy_grace_ns.min(half_frame_ns))
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

fn checked_timestamp_add(
    timestamp: MonotonicTimestamp,
    duration: Duration,
) -> Result<MonotonicTimestamp, PlaybackError> {
    timestamp
        .duration_since_origin()
        .checked_add(duration)
        .map(MonotonicTimestamp::from_duration)
        .ok_or(PlaybackError::TransportArithmeticOverflow)
}

fn nonnegative_ns_duration(nanos: i128) -> Result<Duration, PlaybackError> {
    let nanos = u128::try_from(nanos).map_err(|_| PlaybackError::TransportArithmeticOverflow)?;
    let seconds = nanos / 1_000_000_000;
    let subsecond_nanos = nanos % 1_000_000_000;
    Ok(Duration::new(
        u64::try_from(seconds).map_err(|_| PlaybackError::TransportArithmeticOverflow)?,
        u32::try_from(subsecond_nanos).map_err(|_| PlaybackError::TransportArithmeticOverflow)?,
    ))
}

fn sample_frames_ns_ceil(frames: u64, sample_rate: AudioSampleRate) -> Result<u128, PlaybackError> {
    let numerator = u128::from(frames)
        .checked_mul(1_000_000_000)
        .ok_or(PlaybackError::TransportArithmeticOverflow)?;
    let denominator = u128::from(sample_rate.hz());
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    quotient
        .checked_add(u128::from(remainder != 0))
        .ok_or(PlaybackError::TransportArithmeticOverflow)
}

fn sample_frames_exceed_duration_ns(
    frames: u64,
    sample_rate: AudioSampleRate,
    duration_ns: u128,
) -> Result<bool, PlaybackError> {
    let frame_time_numerator = u128::from(frames)
        .checked_mul(1_000_000_000)
        .ok_or(PlaybackError::TransportArithmeticOverflow)?;
    let duration_at_rate = duration_ns
        .checked_mul(u128::from(sample_rate.hz()))
        .ok_or(PlaybackError::TransportArithmeticOverflow)?;
    Ok(frame_time_numerator > duration_at_rate)
}

fn audio_media_position_ns(
    observation: AudioDeviceClockObservation,
    effective_consumed_frames: u64,
) -> Result<i128, PlaybackError> {
    let rate = AudioSampleRate::new(observation.sample_rate)
        .map_err(|_| PlaybackError::InvalidAudioSampleRate)?;
    if observation.media_anchor.rate() != rate {
        return Err(PlaybackError::MismatchedAudioSampleRate);
    }
    let consumed = i64::try_from(effective_consumed_frames)
        .map_err(|_| PlaybackError::TransportArithmeticOverflow)?;
    let sample = observation
        .media_anchor
        .sample()
        .checked_add(consumed)
        .ok_or(PlaybackError::TransportArithmeticOverflow)?;
    if sample < 0 {
        return Err(PlaybackError::InvalidAudioClockPosition);
    }
    audio_sample_position_ns_floor(AudioSamplePosition::new(sample, rate))
}

fn audio_sample_position_ns_floor(position: AudioSamplePosition) -> Result<i128, PlaybackError> {
    if position.sample() < 0 {
        return Err(PlaybackError::InvalidAudioClockPosition);
    }
    i128::from(position.sample())
        .checked_mul(1_000_000_000)
        .ok_or(PlaybackError::TransportArithmeticOverflow)
        .map(|numerator| numerator.div_euclid(i128::from(position.rate().hz())))
}

fn audio_sample_position_at_phase_ns(
    phase_ns: i128,
    sample_rate: AudioSampleRate,
) -> Result<AudioSamplePosition, PlaybackError> {
    if phase_ns < 0 {
        return Err(PlaybackError::InvalidAudioClockPosition);
    }
    let numerator = phase_ns
        .checked_mul(i128::from(sample_rate.hz()))
        .ok_or(PlaybackError::TransportArithmeticOverflow)?;
    let denominator = 1_000_000_000_i128;
    let quotient = numerator.div_euclid(denominator);
    let remainder = numerator.rem_euclid(denominator);
    let rounded = quotient
        .checked_add(i128::from(
            remainder.checked_mul(2).ok_or(PlaybackError::TransportArithmeticOverflow)?
                >= denominator,
        ))
        .ok_or(PlaybackError::TransportArithmeticOverflow)?;
    let sample = i64::try_from(rounded).map_err(|_| PlaybackError::TransportArithmeticOverflow)?;
    Ok(AudioSamplePosition::new(sample, sample_rate))
}

pub(crate) fn timeline_position_ns_floor(value: FramePosition) -> Result<i128, PlaybackError> {
    validate_time_base(value.time_base)?;
    let numerator = i128::from(value.frame)
        .checked_mul(i128::from(value.time_base.num))
        .and_then(|value| value.checked_mul(1_000_000_000))
        .ok_or(PlaybackError::TransportArithmeticOverflow)?;
    Ok(numerator.div_euclid(i128::from(value.time_base.den)))
}

fn timeline_frame_boundary_ns(value: FramePosition) -> Result<i128, PlaybackError> {
    validate_time_base(value.time_base)?;
    if value.frame < 0 {
        return Err(PlaybackError::NegativeTimelinePosition);
    }
    let numerator = i128::from(value.frame)
        .checked_mul(i128::from(value.time_base.num))
        .and_then(|value| value.checked_mul(1_000_000_000))
        .ok_or(PlaybackError::TransportArithmeticOverflow)?;
    let denominator = value.time_base.den as i128;
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    quotient
        .checked_add(i128::from(remainder != 0))
        .ok_or(PlaybackError::TransportArithmeticOverflow)
}

fn timeline_frame_at_ns(nanos: i128, time_base: Rational) -> Result<i64, PlaybackError> {
    validate_time_base(time_base)?;
    if nanos < 0 {
        return Err(PlaybackError::NegativeTimelinePosition);
    }
    let denominator = i128::from(time_base.num)
        .checked_mul(1_000_000_000)
        .ok_or(PlaybackError::TransportArithmeticOverflow)?;
    let frame = nanos
        .checked_mul(i128::from(time_base.den))
        .ok_or(PlaybackError::TransportArithmeticOverflow)?
        / denominator;
    i64::try_from(frame).map_err(|_| PlaybackError::TransportArithmeticOverflow)
}

#[cfg(test)]
#[path = "playback_engine/tests.rs"]
mod tests;
