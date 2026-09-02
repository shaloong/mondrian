//! Deep realtime Audio Playback scheduling Module.

use crate::audio::{
    RealtimeAudioOutputControlError, RealtimeAudioOutputEnqueueError,
    RealtimeAudioOutputQuiescenceToken,
};
#[cfg(feature = "validation")]
use crate::audio_output::RealtimeAudioOutputRecycleError;
use crate::audio_output::{
    RealtimeAudioOutputEvent, RealtimeAudioOutputLossReason, RealtimeAudioOutputManager,
    RealtimeAudioOutputShutdownEvidence,
};
use crate::owner_lifetime::{abandon_io_error, dispose_canonical_or_abandon_opaque_panic_payload};
use crate::{
    AudioBuffer, RealtimeAudioOutputContract, RealtimeAudioOutputDeviceEvidence,
    RealtimeAudioOutputDeviceSelection, RealtimeAudioOutputOpenFailure,
    RealtimeAudioOutputSnapshot,
};
use mondrian_core::{
    AudioChannelLayout, AudioSamplePosition, AudioSampleRate, AudioTimeError,
    ExecutionCancellationToken,
};
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::io;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering as AtomicOrdering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use thiserror::Error;

// One poll consumes at most one output lifecycle event. That event can rotate
// once; a current-generation stateful failure or a newly observed sustained
// underrun can rotate once more, but no newly scheduled work is completed in
// the same poll. Reserving two rotations therefore covers every mutation path.
const MAX_GENERATION_ROTATIONS_PER_POLL: u64 = 2;
const RENDER_WORKER_TERMINAL_UNKNOWN: u8 = 0;
const RENDER_WORKER_TERMINAL_NORMAL: u8 = 1;
const RENDER_WORKER_TERMINAL_PANICKED: u8 = 2;
const RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED: u8 = 3;
const MAX_RETIRED_RENDER_OWNERS: usize = 256;

/// Versioned scheduling policy for realtime Audio Playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlaybackConfig {
    /// Output sample rate used by render and device Adapters.
    pub sample_rate: u32,
    /// Semantic render layout lowered to the concrete device Adapter.
    pub channel_layout: AudioChannelLayout,
    /// Frames in each independently rendered PCM window.
    pub chunk_frames: usize,
    /// Queued frames required before callback consumption starts.
    pub preroll_frames: usize,
    /// Desired queued plus in-flight frames during playback.
    pub high_watermark_frames: usize,
    /// Maximum current-generation render windows admitted at once.
    pub max_in_flight: usize,
    /// Missing active-consumption frames required to enter underrun recovery.
    pub underrun_recovery_threshold_frames: u64,
    /// Consecutive stateful generation invalidations admitted before rendering is blocked.
    pub max_consecutive_render_recoveries: u32,
}

impl AudioPlaybackConfig {
    /// Product defaults: 48 kHz stereo, 80 ms chunks, 120 ms preroll, and 460 ms high watermark.
    pub const fn product_default() -> Self {
        Self {
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            chunk_frames: 3_840,
            preroll_frames: 5_760,
            high_watermark_frames: 22_080,
            max_in_flight: 8,
            underrun_recovery_threshold_frames: 960,
            max_consecutive_render_recoveries: 3,
        }
    }
}

/// Invalid Audio Playback scheduling policy.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlaybackConfigError {
    /// Sample rate, channels, frame budgets, and in-flight capacity must be positive.
    #[error("audio playback configuration values must be positive")]
    ZeroValue,
    /// Preroll must fit inside the high watermark.
    #[error("audio playback preroll must not exceed the high watermark")]
    PrerollExceedsHighWatermark,
    /// Render windows and watermark must fit atomically in the fixed device queue.
    #[error("audio playback chunk and high watermark must fit the device queue")]
    ExceedsOutputCapacity,
    /// The configured render queue must fit in the bounded retirement handoff.
    #[error("audio playback in-flight capacity exceeds render-owner retirement capacity")]
    ExceedsRetirementCapacity,
}

/// Failure to create the realtime Audio Playback execution owner.
#[derive(Debug, Error)]
pub enum AudioPlaybackCreateError {
    /// The requested scheduling policy is not executable.
    #[error(transparent)]
    InvalidConfig(#[from] AudioPlaybackConfigError),
    /// The owned PCM render worker could not be created.
    #[error("failed to spawn Audio Playback render worker: {0}")]
    RenderWorkerSpawn(#[source] io::Error),
    /// The bounded foreign-render-owner retirement worker could not be created.
    #[error("failed to spawn Audio Playback render-owner retirement worker: {0}")]
    RenderRetirementWorkerSpawn(#[source] io::Error),
}

/// Validation-only controlled-recycle request failure.
#[cfg(feature = "validation")]
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlaybackValidationError {
    /// No concrete stream exists to recycle.
    #[error("no concrete realtime output stream is available")]
    OutputUnavailable,
    /// The requested generation is no longer the current concrete stream.
    #[error("expected stream generation {expected} but current generation is {actual}")]
    StreamGenerationMismatch { expected: u64, actual: u64 },
    /// The owned device worker cannot accept the recycle command.
    #[error("realtime audio device worker command channel is unavailable")]
    WorkerUnavailable,
    /// The selected test Adapter does not expose concrete-stream recycling.
    #[error("the current Audio Playback output Adapter does not support controlled recycle")]
    UnsupportedAdapter,
}

/// Failure while synchronously reclaiming Audio Playback workers.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlaybackShutdownError {
    /// The render worker panicked before it could be joined.
    #[error("Audio Playback render worker panicked")]
    RenderWorkerPanicked,
    /// The concrete output-device lifecycle worker panicked.
    #[error("Audio Playback output-device worker panicked")]
    OutputWorkerPanicked,
    /// Shutdown was attempted from one of the workers it owns.
    #[error("Audio Playback could not synchronously join a worker from that same thread")]
    CurrentThreadDetachments,
    /// Lifetime worker accounting did not close despite no explicit panic.
    #[error("Audio Playback worker lifetime accounting did not close")]
    IncompleteWorkerClosure,
}

/// Synchronous lifetime closure evidence for Audio Playback workers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioPlaybackShutdownEvidence {
    /// Evidence schema version.
    pub schema_version: u32,
    /// PCM render workers successfully started over this owner lifetime.
    pub render_workers_started: u32,
    /// PCM render workers synchronously joined.
    pub render_workers_terminated: u32,
    /// Joined PCM render workers whose thread body panicked.
    pub render_worker_panics: u32,
    /// Opaque render-worker panic payloads deliberately abandoned.
    pub render_worker_owner_abandonments: u32,
    /// Joined render workers whose supervisor terminal stamp was absent or invalid.
    pub render_worker_terminal_evidence_missing: u32,
    /// PCM render workers detached because shutdown ran on that same thread.
    pub render_current_thread_detachments: u32,
    /// Dedicated foreign-render-owner retirement workers successfully started.
    pub render_retirement_workers_started: u32,
    /// Retirement workers synchronously joined after draining their bounded queue.
    pub render_retirement_workers_terminated: u32,
    /// Retirement workers whose supervised runtime panicked.
    pub render_retirement_worker_panics: u32,
    /// Opaque retirement-worker panic payloads deliberately abandoned.
    pub render_retirement_worker_owner_abandonments: u32,
    /// Joined retirement workers missing a valid supervisor terminal stamp.
    pub render_retirement_worker_terminal_evidence_missing: u32,
    /// Retirement workers detached because shutdown ran on that same thread.
    pub render_retirement_current_thread_detachments: u32,
    /// Concrete output-device lifecycle closure evidence.
    pub output: RealtimeAudioOutputShutdownEvidence,
    /// Foreign output/renderer owners whose shutdown or destructor panicked.
    pub foreign_owner_panics: u32,
    /// Foreign output/renderer owners or opaque payloads deliberately abandoned.
    pub foreign_owner_abandonments: u32,
    /// Deadline-bounded shutdown coordinators successfully created.
    pub shutdown_coordinators_started: u32,
    /// Deadline-bounded shutdown coordinators observed returned.
    pub shutdown_coordinators_terminated: u32,
    /// Shutdown coordinator creation failures.
    pub shutdown_coordinator_start_failures: u32,
    /// Shutdown coordinators that panicked before publishing a receipt.
    pub shutdown_coordinator_panics: u32,
    /// Shutdown coordinators still running at the shared deadline.
    pub shutdown_coordinator_timeouts: u32,
    /// Shutdown coordinators detached after the shared deadline.
    pub shutdown_coordinator_detachments: u32,
    /// Shutdown coordinator spawners that panicked before returning a handle.
    pub shutdown_coordinator_spawner_panics: u32,
    /// Audio/output owners or opaque payloads deliberately abandoned.
    pub shutdown_coordinator_owner_abandonments: u32,
    /// Whether complete render/output facts were available at the deadline.
    pub shutdown_resource_facts_complete_at_deadline: bool,
    /// Whether Audio Playback owner lifetime was unresolved at the deadline.
    pub shutdown_owner_lifetime_unresolved_at_deadline: bool,
}

impl AudioPlaybackShutdownEvidence {
    /// Current-schema clean evidence when no Audio Playback owner exists.
    pub const fn no_owner() -> Self {
        Self {
            schema_version: 3,
            render_workers_started: 0,
            render_workers_terminated: 0,
            render_worker_panics: 0,
            render_worker_owner_abandonments: 0,
            render_worker_terminal_evidence_missing: 0,
            render_current_thread_detachments: 0,
            render_retirement_workers_started: 0,
            render_retirement_workers_terminated: 0,
            render_retirement_worker_panics: 0,
            render_retirement_worker_owner_abandonments: 0,
            render_retirement_worker_terminal_evidence_missing: 0,
            render_retirement_current_thread_detachments: 0,
            output: RealtimeAudioOutputShutdownEvidence::no_worker_owner(),
            foreign_owner_panics: 0,
            foreign_owner_abandonments: 0,
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

    /// Whether both render and concrete-device workers closed exactly.
    pub const fn all_workers_terminated(self) -> bool {
        self.schema_version == 3
            && self.render_workers_started == self.render_workers_terminated
            && self.render_worker_panics == 0
            && self.render_worker_owner_abandonments == 0
            && self.render_worker_terminal_evidence_missing == 0
            && self.render_current_thread_detachments == 0
            && self.render_retirement_workers_started == self.render_retirement_workers_terminated
            && self.render_retirement_worker_panics == 0
            && self.render_retirement_worker_owner_abandonments == 0
            && self.render_retirement_worker_terminal_evidence_missing == 0
            && self.render_retirement_current_thread_detachments == 0
            && self.output.all_workers_terminated()
            && self.foreign_owner_panics == 0
            && self.foreign_owner_abandonments == 0
            && self.shutdown_coordinators_started == self.shutdown_coordinators_terminated
            && self.shutdown_coordinator_start_failures == 0
            && self.shutdown_coordinator_panics == 0
            && self.shutdown_coordinator_timeouts == 0
            && self.shutdown_coordinator_detachments == 0
            && self.shutdown_coordinator_spawner_panics == 0
            && self.shutdown_coordinator_owner_abandonments == 0
            && self.shutdown_resource_facts_complete_at_deadline
            && !self.shutdown_owner_lifetime_unresolved_at_deadline
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderWorkerJoinOutcome {
    Terminated,
    Panicked,
    TerminalEvidenceMissing,
    CurrentThreadSkipped,
}

struct AudioPlaybackShutdownCoordinatorResult {
    evidence: AudioPlaybackShutdownEvidence,
    completed_at: Instant,
}

type AudioPlaybackShutdownTask =
    Box<dyn FnOnce() -> AudioPlaybackShutdownCoordinatorResult + Send + 'static>;

/// Failure to lower or advance one realtime Audio Playback sample coordinate.
///
/// Timeline-to-sample lowering belongs to the upstream transport boundary.
/// This Module accepts only exact resolved sample positions and rejects wrong
/// rates, negative samples, or unrepresentable advances before mutation.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlaybackError {
    /// The owned PCM render worker is no longer available.
    #[error("Audio Playback render execution is unavailable")]
    ExecutionUnavailable,
    /// The supplied sample position belongs to the wrong rate or overflowed.
    #[error("invalid Audio Playback sample anchor: {0}")]
    InvalidSampleAnchor(#[from] AudioTimeError),
    /// Realtime transport does not admit a negative sample anchor.
    #[error("Audio Playback sample anchor must be non-negative")]
    NegativeSampleAnchor,
    /// A generation or sample cursor could not advance without changing identity or phase.
    #[error("Audio Playback coordinate arithmetic overflow")]
    CoordinateOverflow,
    /// The concrete stream rate differs from the validated render contract.
    #[error("realtime output sample rate {actual} does not match Audio Playback rate {expected}")]
    OutputSampleRateMismatch { expected: u32, actual: u32 },
    /// The concrete output Adapter exposes a different semantic speaker layout.
    #[error("realtime output layout {actual} does not match Audio Playback layout {expected}")]
    OutputChannelLayoutMismatch {
        expected: AudioChannelLayout,
        actual: AudioChannelLayout,
    },
    /// The low-frequency negotiation evidence and callback snapshot disagree.
    #[error("realtime output negotiation contract changed before publication")]
    OutputNegotiationEvidenceMismatch {
        selected: RealtimeAudioOutputContract,
        observed: RealtimeAudioOutputContract,
    },
    /// Callback-control or exact output trimming failed closed.
    #[error(transparent)]
    OutputControl(#[from] RealtimeAudioOutputControlError),
    /// Exact hidden-preroll catch-up cannot fit the physical output queue.
    #[error(
        "hidden preroll needs {skip_frames} catch-up plus {preroll_frames} preroll frames, exceeding output capacity {capacity_frames}"
    )]
    HiddenPrerollExceedsOutputCapacity {
        skip_frames: usize,
        preroll_frames: usize,
        capacity_frames: usize,
    },
    /// Active callback evidence existed without its exact sample anchor.
    #[error("active realtime output has no exact media sample anchor")]
    ActiveOutputMissingMediaAnchor,
    /// A concrete inactive stream was not paired with its quiescence contract.
    #[error("inactive realtime output has no callback quiescence token")]
    MissingQuiescenceToken,
}

/// Validate an already-resolved Audio Playback sample anchor without mutation.
///
/// This is a pure contract check for unavailable-execution wrappers. It never
/// lowers author time: the upstream transport remains the sole authority that
/// converts timeline coordinates into [`AudioSamplePosition`].
pub fn validate_audio_playback_anchor(
    anchor: AudioSamplePosition,
    expected_sample_rate: u32,
) -> Result<(), AudioPlaybackError> {
    let expected_rate = AudioSampleRate::new(expected_sample_rate)?;
    if anchor.rate() != expected_rate {
        return Err(AudioTimeError::RateMismatch {
            left: anchor.rate().hz(),
            right: expected_rate.hz(),
        }
        .into());
    }
    if anchor.sample() < 0 {
        return Err(AudioPlaybackError::NegativeSampleAnchor);
    }
    Ok(())
}

/// Exact PCM window requested at the render Adapter Seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPcmRenderRequest {
    /// First timeline-media sample frame in the window.
    pub start_sample: i64,
    /// Exact number of output frames required.
    pub frame_count: usize,
    /// Required output sample rate.
    pub sample_rate: u32,
    /// Required semantic output layout and interleaving order.
    pub channel_layout: AudioChannelLayout,
    /// Explicit generation entry or exact continuation selected by Playback.
    pub continuity: AudioPcmContinuity,
}

/// Playback-owned render generation identity carried through the PCM Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioPcmRenderGeneration(u64);

impl AudioPcmRenderGeneration {
    /// Construct one Playback-owned generation identity.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Return the numeric identity for cross-module evidence correlation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// State-domain operation attached to one exact PCM window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPcmContinuity {
    /// First window of a fresh render generation.
    Enter(AudioPcmRenderGeneration),
    /// Exact next window in the current render generation.
    Continue(AudioPcmRenderGeneration),
}

impl AudioPcmContinuity {
    /// Generation shared by both operations.
    pub const fn generation(self) -> AudioPcmRenderGeneration {
        match self {
            Self::Enter(generation) | Self::Continue(generation) => generation,
        }
    }
}

/// Cross-window state contract declared by a PCM renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPcmContinuityModel {
    /// Every window can be evaluated independently, so one failure may be
    /// replaced with exact-duration silence without poisoning later windows.
    IndependentWindows,
    /// Admitted windows mutate generation-owned history. Any failure requires
    /// the whole generation to be invalidated and explicitly re-entered.
    GenerationState,
}

/// Scheduler action after a stateful render generation is invalidated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRenderRecoveryDisposition {
    /// A fresh generation will enter at the authoritative Playback position.
    Reprime,
    /// The bounded recovery budget is exhausted; explicit reprime is required.
    Blocked,
}

/// Adapter Interface used by Audio Playback to render timeline PCM.
///
/// Implementations may decode and mix, but must return exactly the requested
/// rate, channels, and frame count. Independent-window errors become
/// same-duration silence; generation-state errors invalidate all work in that
/// generation. The continuity contract is installed explicitly alongside the
/// renderer, so this foreign execution trait has no second contract authority.
/// Both paths emit structured evidence and never shift media time.
pub trait AudioPcmRenderer: Send + Sync + 'static {
    /// Render one exact timeline-media window.
    fn render(
        &self,
        request: AudioPcmRenderRequest,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<AudioBuffer>;
}

/// Transport permission presented to the Audio Playback Module on each poll.
///
/// The variants deliberately separate filling PCM from allowing the output
/// callback to consume it. In particular, video/startup `Priming` maps to
/// [`Self::Preroll`], so a slow first frame cannot start audio early and then
/// force a generation-resetting phase correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlaybackMode {
    /// Do not schedule new PCM and keep device consumption disabled.
    Idle,
    /// Render and queue PCM, but do not permit device consumption yet.
    Preroll,
    /// Render PCM and permit activation once the preroll watermark is met.
    Consume,
}

impl AudioPlaybackMode {
    const fn renders_pcm(self) -> bool {
        matches!(self, Self::Preroll | Self::Consume)
    }

    const fn permits_consumption(self) -> bool {
        matches!(self, Self::Consume)
    }
}

/// Runtime Audio Playback condition exposed to callers and headless harnesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlaybackState {
    /// The PCM render execution owner could not be created; transport may use
    /// Synthetic Clock Master but no audio work is admitted.
    ExecutionUnavailable,
    /// No concrete output stream is currently available.
    DeviceUnavailable,
    /// Output exists but transport is not consuming PCM.
    Idle,
    /// Playback has no timeline PCM Adapter configured.
    WaitingForSource,
    /// Current generation is filling preroll or is ready but not yet permitted to consume.
    Prerolling,
    /// Output or state continuity failed and fresh-generation preroll is active.
    Recovering,
    /// Repeated stateful generation failures exhausted the bounded retry policy.
    RenderBlocked,
    /// Callback consumption is active for a preroll-qualified generation.
    Active,
}

/// Frozen evidence for the most recently destroyed concrete output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioOutputLossSnapshot {
    /// Typed retirement reason.
    pub reason: RealtimeAudioOutputLossReason,
    /// Callback evidence captured only after the CPAL stream was destroyed.
    pub final_output: RealtimeAudioOutputSnapshot,
    /// Exact sample anchor paired with that stream's active interval.
    pub final_media_anchor: Option<AudioSamplePosition>,
}

/// Fixed-size aggregate retained beyond transient lifecycle events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioOutputLifecycleDiagnostics {
    /// Number of concrete streams installed in this Audio Playback owner.
    pub opened_count: u64,
    /// Number of post-drop frozen loss records observed.
    pub lost_count: u64,
    /// Losses requested by the validation-only recycle seam.
    pub controlled_recycle_count: u64,
    /// Losses caused by concrete backend failure.
    pub backend_loss_count: u64,
    /// Losses whose callback deactivation token could not be allocated.
    pub deactivation_failed_count: u64,
    /// Losses caused by a successful new system-default identity observation.
    pub default_device_change_count: u64,
    /// Losses caused by a latest-wins explicit device-selection update.
    pub device_selection_change_count: u64,
    /// Most recently installed concrete stream generation.
    pub last_opened_generation: Option<u64>,
    /// Most recently destroyed concrete stream generation.
    pub last_lost_generation: Option<u64>,
    /// Frozen evidence for the most recent concrete loss.
    pub last_loss: Option<AudioOutputLossSnapshot>,
}

/// Structured lifecycle and render evidence emitted by one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioPlaybackEvent {
    /// A concrete output stream opened and the render generation was reset.
    DeviceOpened {
        stream_generation: u64,
        evidence: RealtimeAudioOutputDeviceEvidence,
    },
    /// A concrete output stream was destroyed and its callback evidence frozen.
    DeviceLost {
        reason: RealtimeAudioOutputLossReason,
        final_output: RealtimeAudioOutputSnapshot,
        final_media_anchor: Option<AudioSamplePosition>,
    },
    /// One open attempt failed and will be retried.
    DeviceOpenFailed {
        retry_after: Duration,
        failure: RealtimeAudioOutputOpenFailure,
    },
    /// The owned concrete-device lifecycle worker could not be created.
    DeviceWorkerStartFailed { reason: String },
    /// The owned concrete-device lifecycle worker exited without shutdown.
    DeviceWorkerStoppedUnexpectedly { reason: String },
    /// The owned PCM render worker exited without an explicit shutdown.
    ///
    /// Audio Playback becomes execution-unavailable and admits no further
    /// render work after publishing this event.
    RenderWorkerStoppedUnexpectedly { reason: String },
    /// A render failed or violated its PCM contract; exact-duration silence was queued.
    RenderSubstitutedWithSilence {
        generation: u64,
        start_sample: i64,
        reason: String,
    },
    /// Stateful rendering failed, so no PCM from that generation remains valid.
    RenderGenerationInvalidated {
        /// Generation whose state history was invalidated.
        failed_generation: u64,
        /// Fresh generation allocated at `restart_anchor`; admitted only when
        /// `disposition` is [`AudioRenderRecoveryDisposition::Reprime`].
        restart_generation: u64,
        /// Window at which the failure was observed.
        failed_start_sample: i64,
        /// Authoritative timeline position selected for fresh preroll.
        restart_anchor: AudioSamplePosition,
        /// Final output observation captured before deactivation, when present.
        final_output: Option<RealtimeAudioOutputSnapshot>,
        /// Media anchor paired with `final_output`, when a source was still bound.
        final_media_anchor: Option<AudioSamplePosition>,
        /// Structured renderer or PCM-contract failure.
        reason: String,
        /// Whether Playback will retry or now requires an explicit reprime.
        disposition: AudioRenderRecoveryDisposition,
    },
    /// New callback starvation was observed but may remain below recovery policy.
    UnderrunObserved {
        stream_generation: u64,
        delta_frames: u64,
        interval_total_frames: u64,
    },
    /// Missing frames reached policy; output was deactivated and reprime began.
    UnderrunRecoveryStarted {
        stream_generation: u64,
        missing_frames: u64,
        threshold_frames: u64,
        final_output: RealtimeAudioOutputSnapshot,
        final_media_anchor: AudioSamplePosition,
    },
}

/// Immutable Audio Playback state after one non-blocking poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPlaybackSnapshot {
    /// Current render generation.
    pub generation: u64,
    /// Current-generation windows executing or queued on the render worker.
    pub in_flight: usize,
    /// First sample frame not yet admitted to the render worker.
    pub next_start_sample: i64,
    /// Exact media time corresponding to active-consumption frame zero.
    pub media_anchor: Option<AudioSamplePosition>,
    /// Whether the current generation has met the activation preroll requirement.
    /// This does not imply that device consumption is currently permitted or active.
    pub activation_preroll_satisfied: bool,
    /// Current high-level Audio Playback condition.
    pub state: AudioPlaybackState,
    /// Concrete output callback evidence, when a device exists.
    pub output: Option<RealtimeAudioOutputSnapshot>,
    /// Current-generation render failures replaced by exact-duration silence.
    pub render_substitution_count: u64,
    /// Number of stateful render failures that invalidated and restarted a generation.
    pub render_generation_recovery_count: u64,
    /// Old-generation completions discarded before reaching the output queue.
    pub stale_completion_count: u64,
    /// Queued render windows canceled synchronously by generation invalidation.
    pub canceled_render_count: u64,
    /// Missing frames accumulated in the current active interval.
    pub active_interval_underrun_frames: u64,
    /// Number of sustained-underrun reprime cycles.
    pub underrun_recovery_count: u64,
    /// Retained concrete-stream lifecycle evidence.
    pub output_lifecycle: AudioOutputLifecycleDiagnostics,
}

impl AudioPlaybackSnapshot {
    /// Construct the explicit snapshot used when no Audio Playback execution
    /// owner could be created.
    pub const fn execution_unavailable() -> Self {
        Self {
            generation: 0,
            in_flight: 0,
            next_start_sample: 0,
            media_anchor: None,
            activation_preroll_satisfied: false,
            state: AudioPlaybackState::ExecutionUnavailable,
            output: None,
            render_substitution_count: 0,
            render_generation_recovery_count: 0,
            stale_completion_count: 0,
            canceled_render_count: 0,
            active_interval_underrun_frames: 0,
            underrun_recovery_count: 0,
            output_lifecycle: AudioOutputLifecycleDiagnostics {
                opened_count: 0,
                lost_count: 0,
                controlled_recycle_count: 0,
                backend_loss_count: 0,
                deactivation_failed_count: 0,
                default_device_change_count: 0,
                device_selection_change_count: 0,
                last_opened_generation: None,
                last_lost_generation: None,
                last_loss: None,
            },
        }
    }
}

/// Result of advancing Audio Playback without blocking the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPlaybackPoll {
    /// State after lifecycle, completion, scheduling, and activation work.
    pub snapshot: AudioPlaybackSnapshot,
    /// Ordered evidence observed during this poll.
    pub events: Vec<AudioPlaybackEvent>,
}

struct RenderWork {
    generation: u64,
    request: AudioPcmRenderRequest,
    continuity_model: AudioPcmContinuityModel,
    renderer: Arc<dyn AudioPcmRenderer>,
    cancellation: ExecutionCancellationToken,
}

struct RenderCompletion {
    generation: u64,
    request: AudioPcmRenderRequest,
    continuity_model: AudioPcmContinuityModel,
    result: RenderCompletionResult,
}

enum RenderCompletionResult {
    Rendered(AudioBuffer),
    Failed(String),
}

#[derive(Clone)]
struct PreparedAudioPcmRenderer {
    owner: Arc<dyn AudioPcmRenderer>,
    continuity_model: AudioPcmContinuityModel,
}

struct RenderQueueState {
    pending: VecDeque<RenderWork>,
    stopped: bool,
}

struct RenderWorkQueue {
    state: Mutex<RenderQueueState>,
    wake: Condvar,
    capacity: usize,
    retirement_queue: Arc<RenderOwnerRetirementQueue>,
    retirement_tracker: Arc<ForeignOwnerRetirementTracker>,
}

impl RenderWorkQueue {
    fn new(
        capacity: usize,
        retirement_queue: Arc<RenderOwnerRetirementQueue>,
        retirement_tracker: Arc<ForeignOwnerRetirementTracker>,
    ) -> Self {
        Self {
            state: Mutex::new(RenderQueueState {
                pending: VecDeque::with_capacity(capacity),
                stopped: false,
            }),
            wake: Condvar::new(),
            capacity,
            retirement_queue,
            retirement_tracker,
        }
    }

    fn push(&self, work: RenderWork) -> Result<(), RenderWork> {
        let mut state = self.state.lock();
        if state.stopped || state.pending.len() >= self.capacity {
            return Err(work);
        }
        state.pending.push_back(work);
        self.wake.notify_one();
        Ok(())
    }

    fn pop(&self) -> Option<RenderWork> {
        let mut state = self.state.lock();
        loop {
            if let Some(work) = state.pending.pop_front() {
                return Some(work);
            }
            if state.stopped {
                return None;
            }
            self.wake.wait(&mut state);
        }
    }

    fn take_pending(&self) -> VecDeque<RenderWork> {
        let mut state = self.state.lock();
        std::mem::take(&mut state.pending)
    }

    fn stop(&self) -> VecDeque<RenderWork> {
        let mut state = self.state.lock();
        state.stopped = true;
        let pending = std::mem::take(&mut state.pending);
        self.wake.notify_all();
        pending
    }
}

trait AudioOutputAdapter: Send {
    fn shutdown_signal(&self) -> Option<Arc<AtomicBool>> {
        None
    }

    fn poll(&mut self) -> Option<RealtimeAudioOutputEvent>;
    fn enqueue(&mut self, buffer: &AudioBuffer) -> Result<(), RealtimeAudioOutputEnqueueError>;
    fn clear(&self);
    fn validate_deactivation(&self) -> Result<(), RealtimeAudioOutputControlError>;
    fn deactivate(
        &self,
    ) -> Result<Option<RealtimeAudioOutputQuiescenceToken>, RealtimeAudioOutputControlError>;
    fn is_quiescent(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
    ) -> Result<bool, RealtimeAudioOutputControlError>;
    fn activate_after_discard(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
        frames: usize,
    ) -> Result<(), RealtimeAudioOutputControlError>;
    fn buffered_frames(&self) -> usize;
    fn capacity_frames(&self) -> Option<usize>;
    fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot>;

    fn shutdown_and_wait(&mut self) -> RealtimeAudioOutputShutdownEvidence {
        RealtimeAudioOutputShutdownEvidence::no_worker_owner()
    }

    fn set_device_selection(&self, _selection: RealtimeAudioOutputDeviceSelection) -> bool {
        false
    }
    #[cfg(feature = "validation")]
    fn request_controlled_recycle(
        &self,
        _expected_stream_generation: u64,
    ) -> Result<(), AudioPlaybackValidationError> {
        Err(AudioPlaybackValidationError::UnsupportedAdapter)
    }
}

struct OwnedAudioOutput {
    owner: Option<Box<dyn AudioOutputAdapter>>,
}

impl OwnedAudioOutput {
    fn new(owner: Box<dyn AudioOutputAdapter>) -> Self {
        Self { owner: Some(owner) }
    }

    fn take(&mut self) -> Option<Box<dyn AudioOutputAdapter>> {
        self.owner.take()
    }
}

impl Deref for OwnedAudioOutput {
    type Target = dyn AudioOutputAdapter;

    fn deref(&self) -> &Self::Target {
        self.owner
            .as_deref()
            .expect("Audio output owner must be present while playback is live")
    }
}

impl DerefMut for OwnedAudioOutput {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.owner
            .as_deref_mut()
            .expect("Audio output owner must be present while playback is live")
    }
}

type OrdinaryAudioOwnerDropTask = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct OrdinaryAudioOwnerHandoffEvidence {
    start_failures: u32,
    spawner_panics: u32,
    owner_abandonments: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ForeignOwnerRetirementEvidence {
    panics: u32,
    owner_abandonments: u32,
}

#[derive(Default)]
struct ForeignOwnerRetirementTracker {
    evidence: Mutex<ForeignOwnerRetirementEvidence>,
}

impl ForeignOwnerRetirementTracker {
    fn record(&self, evidence: ForeignOwnerRetirementEvidence) {
        let mut retained = self.evidence.lock();
        retained.panics = retained.panics.saturating_add(evidence.panics);
        retained.owner_abandonments =
            retained.owner_abandonments.saturating_add(evidence.owner_abandonments);
    }

    fn snapshot(&self) -> ForeignOwnerRetirementEvidence {
        *self.evidence.lock()
    }
}

#[derive(Default)]
struct RetiredRenderOwners {
    pending: VecDeque<RenderWork>,
    renderers: VecDeque<Arc<dyn AudioPcmRenderer>>,
    errors: VecDeque<mondrian_core::MondrianError>,
}

impl RetiredRenderOwners {
    fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.renderers.is_empty() && self.errors.is_empty()
    }

    fn owner_units(&self) -> usize {
        self.pending
            .len()
            .saturating_add(self.renderers.len())
            .saturating_add(self.errors.len())
    }

    fn merge(&mut self, mut other: Self) -> Result<(), Self> {
        if self.owner_units().saturating_add(other.owner_units()) > MAX_RETIRED_RENDER_OWNERS {
            return Err(other);
        }
        self.pending.append(&mut other.pending);
        self.renderers.append(&mut other.renderers);
        self.errors.append(&mut other.errors);
        Ok(())
    }
}

struct RenderOwnerRetirementQueueState {
    pending: VecDeque<RetiredRenderOwners>,
    stopped: bool,
}

struct RenderOwnerRetirementQueue {
    state: Mutex<RenderOwnerRetirementQueueState>,
    wake: Condvar,
    faulted: Arc<AtomicBool>,
}

impl RenderOwnerRetirementQueue {
    fn new(faulted: Arc<AtomicBool>) -> Self {
        Self {
            state: Mutex::new(RenderOwnerRetirementQueueState {
                pending: VecDeque::with_capacity(1),
                stopped: false,
            }),
            wake: Condvar::new(),
            faulted,
        }
    }

    fn push(&self, retired: RetiredRenderOwners) -> Result<(), RetiredRenderOwners> {
        if retired.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock();
        if state.stopped {
            self.faulted.store(true, AtomicOrdering::Release);
            return Err(retired);
        }
        let pushed = match state.pending.back_mut() {
            Some(batch) => batch.merge(retired),
            None if retired.owner_units() <= MAX_RETIRED_RENDER_OWNERS => {
                state.pending.push_back(retired);
                Ok(())
            }
            None => Err(retired),
        };
        if let Err(retired) = pushed {
            self.faulted.store(true, AtomicOrdering::Release);
            return Err(retired);
        }
        self.wake.notify_one();
        Ok(())
    }

    fn pop(&self) -> Option<RetiredRenderOwners> {
        let mut state = self.state.lock();
        loop {
            if let Some(retired) = state.pending.pop_front() {
                return Some(retired);
            }
            if state.stopped {
                return None;
            }
            self.wake.wait(&mut state);
        }
    }

    fn stop(&self) {
        let mut state = self.state.lock();
        state.stopped = true;
        self.wake.notify_all();
    }

    fn take_pending(&self) -> VecDeque<RetiredRenderOwners> {
        std::mem::take(&mut self.state.lock().pending)
    }

    fn mark_faulted(&self) {
        self.faulted.store(true, AtomicOrdering::Release);
    }
}

fn retire_render_owners(mut retired: RetiredRenderOwners) -> ForeignOwnerRetirementEvidence {
    let mut evidence = ForeignOwnerRetirementEvidence::default();
    while let Some(work) = retired.pending.pop_front() {
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(work)))
        {
            evidence.panics = evidence.panics.saturating_add(1);
            if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                evidence.owner_abandonments = evidence.owner_abandonments.saturating_add(1);
            }
        }
    }
    while let Some(renderer) = retired.renderers.pop_front() {
        if let Err(payload) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(renderer)))
        {
            evidence.panics = evidence.panics.saturating_add(1);
            if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                evidence.owner_abandonments = evidence.owner_abandonments.saturating_add(1);
            }
        }
    }
    while let Some(error) = retired.errors.pop_front() {
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(error)))
        {
            evidence.panics = evidence.panics.saturating_add(1);
            evidence.owner_abandonments = evidence.owner_abandonments.saturating_add(1);
            if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                evidence.owner_abandonments = evidence.owner_abandonments.saturating_add(1);
            }
        }
    }
    evidence
}

fn enqueue_retired_render_owners(
    queue: &RenderOwnerRetirementQueue,
    tracker: &ForeignOwnerRetirementTracker,
    retired: RetiredRenderOwners,
) {
    if let Err(retired) = queue.push(retired) {
        let owner_units = retired.owner_units().min(u32::MAX as usize) as u32;
        std::mem::forget(retired);
        tracker
            .record(ForeignOwnerRetirementEvidence { panics: 0, owner_abandonments: owner_units });
    }
}

fn run_render_owner_retirement_worker(
    queue: &RenderOwnerRetirementQueue,
    tracker: &ForeignOwnerRetirementTracker,
) {
    while let Some(retired) = queue.pop() {
        let evidence = retire_render_owners(retired);
        if evidence.panics > 0 || evidence.owner_abandonments > 0 {
            queue.mark_faulted();
        }
        tracker.record(evidence);
    }
}

fn handoff_audio_output_owner(owner: Box<dyn AudioOutputAdapter>) {
    handoff_ordinary_audio_owner(owner);
}

fn handoff_ordinary_audio_owner<T>(owner: T)
where
    T: Send + 'static,
{
    let evidence = handoff_ordinary_audio_owner_with_spawner(owner, |work| {
        thread::Builder::new().name("mondrian-audio-owner-drop".to_owned()).spawn(work)
    });
    if evidence.start_failures > 0 || evidence.spawner_panics > 0 {
        tracing::error!(
            start_failures = evidence.start_failures,
            spawner_panics = evidence.spawner_panics,
            owner_abandonments = evidence.owner_abandonments,
            "Audio Playback ordinary-drop owner handoff failed closed"
        );
    }
}

/// Move a renderer that could not enter a qualified `AudioPlayback` owner off
/// the caller thread.
///
/// This is only for construction/unavailable fallbacks where no Playback
/// receipt can exist. Live Playback rejection paths use its tracked retirement
/// worker instead.
pub fn handoff_unqualified_audio_pcm_renderer(renderer: Arc<dyn AudioPcmRenderer>) {
    handoff_ordinary_audio_owner(renderer);
}

fn handoff_ordinary_audio_owner_with_spawner<T, F>(
    owner: T,
    spawn: F,
) -> OrdinaryAudioOwnerHandoffEvidence
where
    T: Send + 'static,
    F: FnOnce(OrdinaryAudioOwnerDropTask) -> io::Result<JoinHandle<()>>,
{
    let retained_owner = Arc::new(Mutex::new(Some(owner)));
    let worker_owner = Arc::clone(&retained_owner);
    let work: OrdinaryAudioOwnerDropTask = Box::new(move || {
        let Some(owner) = worker_owner.lock().take() else {
            return;
        };
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(owner)))
        {
            if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                tracing::error!(
                    "Audio Playback ordinary-drop owner panicked with an opaque payload"
                );
            } else {
                tracing::error!("Audio Playback ordinary-drop owner panicked");
            }
        }
    });
    let spawn_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spawn(work)));
    match spawn_result {
        Ok(Ok(worker)) => {
            drop(worker);
            OrdinaryAudioOwnerHandoffEvidence::default()
        }
        Ok(Err(error)) => {
            let (_kind, error_owner_abandoned) = abandon_io_error(error);
            let owner_abandonments = abandon_retained_owner(&retained_owner);
            OrdinaryAudioOwnerHandoffEvidence {
                start_failures: 1,
                owner_abandonments: owner_abandonments
                    .saturating_add(u32::from(error_owner_abandoned)),
                ..OrdinaryAudioOwnerHandoffEvidence::default()
            }
        }
        Err(payload) => {
            let payload_abandoned =
                u32::from(dispose_canonical_or_abandon_opaque_panic_payload(payload));
            OrdinaryAudioOwnerHandoffEvidence {
                start_failures: 1,
                spawner_panics: 1,
                owner_abandonments: abandon_retained_owner(&retained_owner)
                    .saturating_add(payload_abandoned),
            }
        }
    }
}

fn abandon_retained_owner<T>(owner: &Arc<Mutex<Option<T>>>) -> u32 {
    let Some(owner) = owner.lock().take() else {
        return 0;
    };
    std::mem::forget(owner);
    1
}

fn abandon_retained_audio_playback_owner(owner: &Arc<Mutex<Option<AudioPlayback>>>) -> u32 {
    let mut retained = owner.lock();
    let Some(mut owner) = retained.take() else {
        return 0;
    };
    owner.begin_shutdown();
    owner.render_retirement_queue.stop();
    std::mem::forget(owner);
    1
}

impl AudioOutputAdapter for RealtimeAudioOutputManager {
    fn shutdown_signal(&self) -> Option<Arc<AtomicBool>> {
        Some(RealtimeAudioOutputManager::shutdown_signal(self))
    }

    fn poll(&mut self) -> Option<RealtimeAudioOutputEvent> {
        RealtimeAudioOutputManager::poll(self)
    }

    fn enqueue(&mut self, buffer: &AudioBuffer) -> Result<(), RealtimeAudioOutputEnqueueError> {
        RealtimeAudioOutputManager::enqueue(self, buffer)
    }

    fn clear(&self) {
        RealtimeAudioOutputManager::clear(self);
    }

    fn validate_deactivation(&self) -> Result<(), RealtimeAudioOutputControlError> {
        RealtimeAudioOutputManager::validate_deactivation(self)
    }

    fn deactivate(
        &self,
    ) -> Result<Option<RealtimeAudioOutputQuiescenceToken>, RealtimeAudioOutputControlError> {
        RealtimeAudioOutputManager::deactivate(self)
    }

    fn is_quiescent(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
    ) -> Result<bool, RealtimeAudioOutputControlError> {
        RealtimeAudioOutputManager::is_quiescent(self, token)
    }

    fn activate_after_discard(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
        frames: usize,
    ) -> Result<(), RealtimeAudioOutputControlError> {
        RealtimeAudioOutputManager::activate_after_discard(self, token, frames)
    }

    fn buffered_frames(&self) -> usize {
        RealtimeAudioOutputManager::buffered_frames(self)
    }

    fn capacity_frames(&self) -> Option<usize> {
        RealtimeAudioOutputManager::capacity_frames(self)
    }

    fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot> {
        RealtimeAudioOutputManager::snapshot(self)
    }

    fn shutdown_and_wait(&mut self) -> RealtimeAudioOutputShutdownEvidence {
        RealtimeAudioOutputManager::shutdown_and_wait(self)
    }

    fn set_device_selection(&self, selection: RealtimeAudioOutputDeviceSelection) -> bool {
        RealtimeAudioOutputManager::set_device_selection(self, selection)
    }

    #[cfg(feature = "validation")]
    fn request_controlled_recycle(
        &self,
        expected_stream_generation: u64,
    ) -> Result<(), AudioPlaybackValidationError> {
        RealtimeAudioOutputManager::request_controlled_recycle(self, expected_stream_generation)
            .map_err(|error| match error {
                RealtimeAudioOutputRecycleError::OutputUnavailable => {
                    AudioPlaybackValidationError::OutputUnavailable
                }
                RealtimeAudioOutputRecycleError::StreamGenerationMismatch { expected, actual } => {
                    AudioPlaybackValidationError::StreamGenerationMismatch { expected, actual }
                }
                RealtimeAudioOutputRecycleError::WorkerUnavailable => {
                    AudioPlaybackValidationError::WorkerUnavailable
                }
            })
    }
}

/// Deep Module owning realtime output, render worker, generations, watermarks, and preroll.
pub struct AudioPlayback {
    config: AudioPlaybackConfig,
    output: OwnedAudioOutput,
    output_shutdown_signal: Option<Arc<AtomicBool>>,
    render_queue: Arc<RenderWorkQueue>,
    render_retirement_queue: Arc<RenderOwnerRetirementQueue>,
    render_retirement_faulted: Arc<AtomicBool>,
    foreign_owner_retirements: Arc<ForeignOwnerRetirementTracker>,
    render_worker: Option<JoinHandle<()>>,
    render_worker_terminal: Arc<AtomicU8>,
    render_retirement_worker: Option<JoinHandle<()>>,
    render_retirement_worker_terminal: Arc<AtomicU8>,
    completion_rx: mpsc::Receiver<RenderCompletion>,
    shutdown_pending_render_work: VecDeque<RenderWork>,
    render_execution_unavailable: bool,
    shutdown_requested: bool,
    renderer: Option<PreparedAudioPcmRenderer>,
    generation: u64,
    generation_cancellation: ExecutionCancellationToken,
    generation_entry_pending: bool,
    in_flight: usize,
    next_start_sample: i64,
    generation_render_anchor: Option<AudioSamplePosition>,
    media_anchor: Option<AudioSamplePosition>,
    stream_media_anchor: Option<(u64, AudioSamplePosition)>,
    quiescence_token: Option<RealtimeAudioOutputQuiescenceToken>,
    output_generation_ready: bool,
    activation_preroll_satisfied: bool,
    render_substitution_count: u64,
    render_generation_recovery_count: u64,
    consecutive_render_generation_failures: u32,
    render_blocked: bool,
    stale_completion_count: u64,
    canceled_render_count: u64,
    underrun_baseline_frames: u64,
    last_underrun_frames: u64,
    underrun_recovery_count: u64,
    recovery_preroll: bool,
    output_lifecycle: AudioOutputLifecycleDiagnostics,
    latest_output_device_evidence: Option<RealtimeAudioOutputDeviceEvidence>,
    render_workers_started: u32,
    render_workers_terminated: u32,
    render_worker_panics: u32,
    render_worker_owner_abandonments: u32,
    render_worker_terminal_evidence_missing: u32,
    render_current_thread_detachments: u32,
    render_retirement_workers_started: u32,
    render_retirement_workers_terminated: u32,
    render_retirement_worker_panics: u32,
    render_retirement_worker_owner_abandonments: u32,
    render_retirement_worker_terminal_evidence_missing: u32,
    render_retirement_current_thread_detachments: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AudioPlaybackPollPreflight {
    authority: AudioSamplePosition,
    chunk_frames: i64,
    elapsed_skip_frames: Option<usize>,
    admission_target_frames: usize,
}

enum CompletionAdmission {
    Accepted,
    SubstitutedWithSilence(String),
    InvalidateGeneration(String),
}

impl AudioPlayback {
    /// Construct Audio Playback with the validated product policy.
    pub fn product_default() -> Result<Self, AudioPlaybackCreateError> {
        let config = AudioPlaybackConfig::product_default();
        validate_config(config)?;
        Self::with_output(
            config,
            Box::new(RealtimeAudioOutputManager::new(
                config.sample_rate,
                config.channel_layout,
            )),
        )
    }

    /// Construct production Audio Playback with a dedicated CPAL lifecycle thread and render worker.
    pub fn new(config: AudioPlaybackConfig) -> Result<Self, AudioPlaybackCreateError> {
        validate_config(config)?;
        let output = RealtimeAudioOutputManager::new(config.sample_rate, config.channel_layout);
        Self::with_output(config, Box::new(output))
    }

    /// Construct production Audio Playback with one explicit runtime device intent.
    pub fn new_with_output_device(
        config: AudioPlaybackConfig,
        selection: RealtimeAudioOutputDeviceSelection,
    ) -> Result<Self, AudioPlaybackCreateError> {
        validate_config(config)?;
        let output = RealtimeAudioOutputManager::new_with_device_selection(
            config.sample_rate,
            config.channel_layout,
            selection,
        );
        Self::with_output(config, Box::new(output))
    }

    fn with_output(
        config: AudioPlaybackConfig,
        output: Box<dyn AudioOutputAdapter>,
    ) -> Result<Self, AudioPlaybackCreateError> {
        Self::with_output_and_spawner(config, output, spawn_render_worker)
    }

    fn with_output_and_spawner(
        config: AudioPlaybackConfig,
        output: Box<dyn AudioOutputAdapter>,
        spawner: impl FnOnce(
            Arc<RenderWorkQueue>,
            mpsc::Sender<RenderCompletion>,
            Arc<AtomicU8>,
        ) -> io::Result<JoinHandle<()>>,
    ) -> Result<Self, AudioPlaybackCreateError> {
        if let Err(error) = validate_config(config) {
            handoff_audio_output_owner(output);
            return Err(error.into());
        }
        let output_shutdown_signal = output.shutdown_signal();
        let foreign_owner_retirements = Arc::new(ForeignOwnerRetirementTracker::default());
        let render_retirement_faulted = Arc::new(AtomicBool::new(false));
        let render_retirement_queue = Arc::new(RenderOwnerRetirementQueue::new(Arc::clone(
            &render_retirement_faulted,
        )));
        let render_retirement_worker_terminal =
            Arc::new(AtomicU8::new(RENDER_WORKER_TERMINAL_UNKNOWN));
        let retirement_spawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            spawn_render_owner_retirement_worker(
                Arc::clone(&render_retirement_queue),
                Arc::clone(&foreign_owner_retirements),
                Arc::clone(&render_retirement_worker_terminal),
            )
        }));
        let render_retirement_worker = match retirement_spawn {
            Ok(Ok(worker)) => worker,
            Ok(Err(error)) => {
                let (kind, error_owner_abandoned) = abandon_io_error(error);
                if error_owner_abandoned {
                    tracing::error!(
                        "Audio Playback retirement-worker spawn error owner was abandoned"
                    );
                }
                handoff_audio_output_owner(output);
                return Err(AudioPlaybackCreateError::RenderRetirementWorkerSpawn(
                    io::Error::from(kind),
                ));
            }
            Err(payload) => {
                let opaque_payload_abandoned =
                    dispose_canonical_or_abandon_opaque_panic_payload(payload);
                handoff_audio_output_owner(output);
                if opaque_payload_abandoned {
                    tracing::error!(
                        "Audio Playback render-owner retirement spawner panicked with an opaque payload"
                    );
                }
                return Err(AudioPlaybackCreateError::RenderRetirementWorkerSpawn(
                    io::Error::other("Audio Playback render-owner retirement spawner panicked"),
                ));
            }
        };
        let render_queue = Arc::new(RenderWorkQueue::new(
            config.max_in_flight,
            Arc::clone(&render_retirement_queue),
            Arc::clone(&foreign_owner_retirements),
        ));
        let worker_queue = Arc::clone(&render_queue);
        let (completion_tx, completion_rx) = mpsc::channel::<RenderCompletion>();
        let render_worker_terminal = Arc::new(AtomicU8::new(RENDER_WORKER_TERMINAL_UNKNOWN));
        let spawn_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            spawner(
                worker_queue,
                completion_tx,
                Arc::clone(&render_worker_terminal),
            )
        }));
        let render_worker = match spawn_result {
            Ok(Ok(worker)) => worker,
            Ok(Err(error)) => {
                let (kind, error_owner_abandoned) = abandon_io_error(error);
                if error_owner_abandoned {
                    tracing::error!("Audio Playback render-worker spawn error owner was abandoned");
                }
                render_retirement_queue.stop();
                join_initial_retirement_worker(render_retirement_worker);
                handoff_audio_output_owner(output);
                return Err(AudioPlaybackCreateError::RenderWorkerSpawn(
                    io::Error::from(kind),
                ));
            }
            Err(payload) => {
                let opaque_payload_abandoned =
                    dispose_canonical_or_abandon_opaque_panic_payload(payload);
                render_retirement_queue.stop();
                join_initial_retirement_worker(render_retirement_worker);
                handoff_audio_output_owner(output);
                if opaque_payload_abandoned {
                    tracing::error!(
                        "Audio Playback render-worker spawner panicked with an opaque payload"
                    );
                }
                return Err(AudioPlaybackCreateError::RenderWorkerSpawn(
                    io::Error::other("Audio Playback render-worker spawner panicked"),
                ));
            }
        };
        Ok(Self {
            config,
            output: OwnedAudioOutput::new(output),
            output_shutdown_signal,
            render_queue,
            render_retirement_queue,
            render_retirement_faulted,
            foreign_owner_retirements,
            render_worker: Some(render_worker),
            render_worker_terminal,
            render_retirement_worker: Some(render_retirement_worker),
            render_retirement_worker_terminal,
            completion_rx,
            shutdown_pending_render_work: VecDeque::with_capacity(config.max_in_flight),
            render_execution_unavailable: false,
            shutdown_requested: false,
            renderer: None,
            generation: 1,
            generation_cancellation: ExecutionCancellationToken::new(),
            generation_entry_pending: true,
            in_flight: 0,
            next_start_sample: 0,
            generation_render_anchor: None,
            media_anchor: None,
            stream_media_anchor: None,
            quiescence_token: None,
            output_generation_ready: false,
            activation_preroll_satisfied: false,
            render_substitution_count: 0,
            render_generation_recovery_count: 0,
            consecutive_render_generation_failures: 0,
            render_blocked: false,
            stale_completion_count: 0,
            canceled_render_count: 0,
            underrun_baseline_frames: 0,
            last_underrun_frames: 0,
            underrun_recovery_count: 0,
            recovery_preroll: false,
            output_lifecycle: AudioOutputLifecycleDiagnostics::default(),
            latest_output_device_evidence: None,
            render_workers_started: 1,
            render_workers_terminated: 0,
            render_worker_panics: 0,
            render_worker_owner_abandonments: 0,
            render_worker_terminal_evidence_missing: 0,
            render_current_thread_detachments: 0,
            render_retirement_workers_started: 1,
            render_retirement_workers_terminated: 0,
            render_retirement_worker_panics: 0,
            render_retirement_worker_owner_abandonments: 0,
            render_retirement_worker_terminal_evidence_missing: 0,
            render_retirement_current_thread_detachments: 0,
        })
    }

    /// Cancel, wake, and synchronously reclaim render and output workers.
    pub fn shutdown(self) -> Result<(), AudioPlaybackShutdownError> {
        let evidence = self.shutdown_and_wait();
        if evidence.render_worker_panics > 0 || evidence.render_retirement_worker_panics > 0 {
            Err(AudioPlaybackShutdownError::RenderWorkerPanicked)
        } else if evidence.output.worker_panics > 0 {
            Err(AudioPlaybackShutdownError::OutputWorkerPanicked)
        } else if evidence.render_current_thread_detachments > 0
            || evidence.render_retirement_current_thread_detachments > 0
            || evidence.output.current_thread_detachments > 0
        {
            Err(AudioPlaybackShutdownError::CurrentThreadDetachments)
        } else if !evidence.all_workers_terminated() {
            Err(AudioPlaybackShutdownError::IncompleteWorkerClosure)
        } else {
            Ok(())
        }
    }

    /// Stop PCM production and synchronously reclaim render and device workers.
    pub fn shutdown_and_wait(mut self) -> AudioPlaybackShutdownEvidence {
        self.begin_shutdown();
        self.stop_render_worker();
        self.flush_shutdown_pending_render_work();
        if let Some(renderer) = self.renderer.take() {
            self.handoff_retired_render_owners(VecDeque::new(), Some(renderer.owner));
        }
        self.stop_render_retirement_worker();
        let retirement = self.foreign_owner_retirements.snapshot();
        let mut foreign_owner_panics = retirement.panics;
        let mut foreign_owner_abandonments = retirement.owner_abandonments;
        let mut resource_facts_complete = true;
        let mut owner_lifetime_unresolved = false;
        if retirement.panics > 0
            || retirement.owner_abandonments > 0
            || self.render_retirement_workers_started != self.render_retirement_workers_terminated
            || self.render_retirement_worker_panics > 0
            || self.render_retirement_worker_owner_abandonments > 0
            || self.render_retirement_worker_terminal_evidence_missing > 0
            || self.render_retirement_current_thread_detachments > 0
        {
            resource_facts_complete = false;
            owner_lifetime_unresolved = true;
        }
        let output = match self.output.take() {
            Some(mut output_owner) => {
                let shutdown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    output_owner.shutdown_and_wait()
                }));
                match shutdown {
                    Ok(evidence) => {
                        let drop_result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                drop(output_owner)
                            }));
                        if let Err(payload) = drop_result {
                            foreign_owner_panics = foreign_owner_panics.saturating_add(1);
                            if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                                foreign_owner_abandonments =
                                    foreign_owner_abandonments.saturating_add(1);
                            }
                            resource_facts_complete = false;
                            owner_lifetime_unresolved = true;
                        }
                        evidence
                    }
                    Err(payload) => {
                        foreign_owner_panics = foreign_owner_panics.saturating_add(1);
                        if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                            foreign_owner_abandonments =
                                foreign_owner_abandonments.saturating_add(1);
                        }
                        // The Adapter already violated its shutdown contract.
                        // Running its foreign destructor on this thread could
                        // block or panic again, so abandon it explicitly.
                        std::mem::forget(output_owner);
                        foreign_owner_abandonments = foreign_owner_abandonments.saturating_add(1);
                        resource_facts_complete = false;
                        owner_lifetime_unresolved = true;
                        RealtimeAudioOutputShutdownEvidence::default()
                    }
                }
            }
            None => {
                resource_facts_complete = false;
                owner_lifetime_unresolved = true;
                RealtimeAudioOutputShutdownEvidence::default()
            }
        };
        if self.render_workers_started != self.render_workers_terminated
            || self.render_worker_panics > 0
            || self.render_worker_owner_abandonments > 0
            || self.render_worker_terminal_evidence_missing > 0
            || self.render_current_thread_detachments > 0
            || !output.all_workers_terminated()
        {
            resource_facts_complete = false;
            owner_lifetime_unresolved = true;
        }
        AudioPlaybackShutdownEvidence {
            schema_version: 3,
            render_workers_started: self.render_workers_started,
            render_workers_terminated: self.render_workers_terminated,
            render_worker_panics: self.render_worker_panics,
            render_worker_owner_abandonments: self.render_worker_owner_abandonments,
            render_worker_terminal_evidence_missing: self.render_worker_terminal_evidence_missing,
            render_current_thread_detachments: self.render_current_thread_detachments,
            render_retirement_workers_started: self.render_retirement_workers_started,
            render_retirement_workers_terminated: self.render_retirement_workers_terminated,
            render_retirement_worker_panics: self.render_retirement_worker_panics,
            render_retirement_worker_owner_abandonments: self
                .render_retirement_worker_owner_abandonments,
            render_retirement_worker_terminal_evidence_missing: self
                .render_retirement_worker_terminal_evidence_missing,
            render_retirement_current_thread_detachments: self
                .render_retirement_current_thread_detachments,
            output,
            foreign_owner_panics,
            foreign_owner_abandonments,
            shutdown_coordinators_started: 0,
            shutdown_coordinators_terminated: 0,
            shutdown_coordinator_start_failures: 0,
            shutdown_coordinator_panics: 0,
            shutdown_coordinator_timeouts: 0,
            shutdown_coordinator_detachments: 0,
            shutdown_coordinator_spawner_panics: 0,
            shutdown_coordinator_owner_abandonments: 0,
            shutdown_resource_facts_complete_at_deadline: resource_facts_complete,
            shutdown_owner_lifetime_unresolved_at_deadline: owner_lifetime_unresolved,
        }
    }

    /// Close PCM admission and cooperatively stop Mondrian-owned render work.
    ///
    /// The concrete output's signal is captured at construction and reduced to
    /// one atomic store here. The foreign output Adapter is deliberately not
    /// called from this seam; its shutdown and destructor execute only inside
    /// a consuming closure bounded by the campaign-wide absolute deadline.
    pub fn begin_shutdown(&mut self) {
        self.shutdown_requested = true;
        self.generation_cancellation.cancel();
        let mut pending = self.render_queue.stop();
        self.shutdown_pending_render_work.append(&mut pending);
        if let Some(signal) = &self.output_shutdown_signal {
            signal.store(true, AtomicOrdering::Release);
        }
    }

    fn flush_shutdown_pending_render_work(&mut self) {
        let pending = std::mem::take(&mut self.shutdown_pending_render_work);
        self.handoff_retired_render_owners(pending, None);
    }

    fn handoff_retired_render_owners(
        &self,
        pending: VecDeque<RenderWork>,
        renderer: Option<Arc<dyn AudioPcmRenderer>>,
    ) {
        let renderers = renderer.map_or_else(VecDeque::new, |renderer| VecDeque::from([renderer]));
        enqueue_retired_render_owners(
            &self.render_retirement_queue,
            &self.foreign_owner_retirements,
            RetiredRenderOwners { pending, renderers, errors: VecDeque::new() },
        );
    }

    /// Consume Audio Playback through one absolute qualification deadline.
    ///
    /// Foreign device teardown is isolated in a tracked coordinator so a
    /// broken Adapter produces timeout/detach evidence instead of hanging the
    /// caller past the campaign-wide deadline.
    pub fn shutdown_until(self, deadline: Instant) -> AudioPlaybackShutdownEvidence {
        self.shutdown_until_with_spawner(deadline, |work| {
            thread::Builder::new()
                .name("mondrian-audio-endurance-shutdown".to_owned())
                .spawn(work)
        })
    }

    fn shutdown_until_with_spawner<F>(
        mut self,
        deadline: Instant,
        spawn: F,
    ) -> AudioPlaybackShutdownEvidence
    where
        F: FnOnce(
            AudioPlaybackShutdownTask,
        ) -> io::Result<JoinHandle<AudioPlaybackShutdownCoordinatorResult>>,
    {
        self.begin_shutdown();
        let owner = Arc::new(Mutex::new(Some(self)));
        let coordinator_owner = Arc::clone(&owner);
        let work: AudioPlaybackShutdownTask = Box::new(move || {
            let shutdown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Some(owner) = coordinator_owner.lock().take() else {
                    return AudioPlayback::coordinator_failure(0, 0, 0, 0, 0, 0, 0, 1);
                };
                owner.shutdown_and_wait()
            }));
            let evidence = match shutdown {
                Ok(evidence) => evidence,
                Err(payload) => {
                    let owner_abandonments =
                        u32::from(dispose_canonical_or_abandon_opaque_panic_payload(payload));
                    AudioPlayback::coordinator_failure(0, 0, 0, 1, 0, 0, 0, owner_abandonments)
                }
            };
            AudioPlaybackShutdownCoordinatorResult { evidence, completed_at: Instant::now() }
        });
        let coordinator = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spawn(work)));
        let coordinator = match coordinator {
            Ok(Ok(coordinator)) => coordinator,
            Ok(Err(error)) => {
                let (_kind, error_owner_abandoned) = abandon_io_error(error);
                let owner_abandonments = abandon_retained_audio_playback_owner(&owner)
                    .saturating_add(u32::from(error_owner_abandoned));
                return AudioPlayback::coordinator_failure(0, 0, 1, 0, 0, 0, 0, owner_abandonments);
            }
            Err(payload) => {
                let payload_abandoned =
                    u32::from(dispose_canonical_or_abandon_opaque_panic_payload(payload));
                let owner_abandonments =
                    abandon_retained_audio_playback_owner(&owner).saturating_add(payload_abandoned);
                return AudioPlayback::coordinator_failure(0, 0, 1, 0, 0, 0, 1, owner_abandonments);
            }
        };

        loop {
            if coordinator.is_finished() {
                break;
            }
            if Instant::now() >= deadline {
                if coordinator.is_finished() {
                    continue;
                }
                drop(coordinator);
                let retained_owner_abandonments = abandon_retained_audio_playback_owner(&owner);
                return AudioPlayback::coordinator_failure(
                    1,
                    0,
                    0,
                    0,
                    1,
                    1,
                    0,
                    retained_owner_abandonments,
                );
            }
            thread::sleep(Duration::from_millis(1));
        }

        match coordinator.join() {
            Ok(result) => {
                let mut evidence = result.evidence;
                evidence.shutdown_coordinators_started = 1;
                evidence.shutdown_coordinators_terminated = 1;
                let retained_owner_abandonments = abandon_retained_audio_playback_owner(&owner);
                evidence.shutdown_coordinator_owner_abandonments = evidence
                    .shutdown_coordinator_owner_abandonments
                    .saturating_add(retained_owner_abandonments);
                if retained_owner_abandonments > 0 {
                    evidence.shutdown_resource_facts_complete_at_deadline = false;
                    evidence.shutdown_owner_lifetime_unresolved_at_deadline = true;
                }
                if result.completed_at > deadline {
                    evidence.shutdown_coordinator_timeouts =
                        evidence.shutdown_coordinator_timeouts.saturating_add(1);
                    evidence.shutdown_resource_facts_complete_at_deadline = false;
                    evidence.shutdown_owner_lifetime_unresolved_at_deadline = true;
                }
                evidence
            }
            Err(payload) => {
                let payload_abandoned =
                    u32::from(dispose_canonical_or_abandon_opaque_panic_payload(payload));
                let owner_abandonments =
                    abandon_retained_audio_playback_owner(&owner).saturating_add(payload_abandoned);
                AudioPlayback::coordinator_failure(
                    1,
                    1,
                    0,
                    1,
                    u32::from(Instant::now() > deadline),
                    0,
                    0,
                    owner_abandonments,
                )
            }
        }
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
    ) -> AudioPlaybackShutdownEvidence {
        AudioPlaybackShutdownEvidence {
            schema_version: 3,
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
            ..AudioPlaybackShutdownEvidence::default()
        }
    }

    fn stop_render_worker(&mut self) {
        self.begin_shutdown();
        let Some(worker) = self.render_worker.take() else {
            return;
        };
        self.join_render_worker(worker);
    }

    /// Request ordinary-drop shutdown without waiting for renderer code.
    ///
    /// Returns `true` when a still-running worker was detached. In that case
    /// the started worker intentionally remains absent from the terminated
    /// count, preventing this path from looking like qualified clean closure.
    fn stop_render_worker_without_waiting(&mut self) -> bool {
        self.begin_shutdown();
        let Some(worker) = self.render_worker.take() else {
            return false;
        };
        if worker.is_finished() {
            self.join_render_worker(worker);
            false
        } else {
            drop(worker);
            true
        }
    }

    fn stop_render_retirement_worker(&mut self) {
        self.render_retirement_queue.stop();
        if let Some(worker) = self.render_retirement_worker.take() {
            self.join_render_retirement_worker(worker);
        }
        self.abandon_pending_render_retirements();
    }

    fn stop_render_retirement_worker_without_waiting(&mut self) -> bool {
        self.render_retirement_queue.stop();
        let detached = if let Some(worker) = self.render_retirement_worker.take() {
            if worker.is_finished() {
                self.join_render_retirement_worker(worker);
                false
            } else {
                drop(worker);
                true
            }
        } else {
            false
        };
        self.abandon_pending_render_retirements();
        detached
    }

    fn abandon_pending_render_retirements(&mut self) {
        let pending = self.render_retirement_queue.take_pending();
        let owner_units = pending
            .iter()
            .map(RetiredRenderOwners::owner_units)
            .fold(0_usize, usize::saturating_add);
        if owner_units == 0 {
            return;
        }
        for retired in pending {
            std::mem::forget(retired);
        }
        self.render_retirement_queue.mark_faulted();
        self.foreign_owner_retirements.record(ForeignOwnerRetirementEvidence {
            panics: 0,
            owner_abandonments: owner_units.min(u32::MAX as usize) as u32,
        });
    }

    fn join_render_retirement_worker(&mut self, worker: JoinHandle<()>) -> RenderWorkerJoinOutcome {
        let outcome = if worker.thread().id() == thread::current().id() {
            drop(worker);
            RenderWorkerJoinOutcome::CurrentThreadSkipped
        } else {
            match worker.join() {
                Ok(()) => {
                    match self.render_retirement_worker_terminal.load(AtomicOrdering::Acquire) {
                        RENDER_WORKER_TERMINAL_NORMAL => RenderWorkerJoinOutcome::Terminated,
                        RENDER_WORKER_TERMINAL_PANICKED => RenderWorkerJoinOutcome::Panicked,
                        RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED => {
                            self.render_retirement_worker_owner_abandonments =
                                self.render_retirement_worker_owner_abandonments.saturating_add(1);
                            RenderWorkerJoinOutcome::Panicked
                        }
                        _ => RenderWorkerJoinOutcome::TerminalEvidenceMissing,
                    }
                }
                Err(payload) => {
                    if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                        self.render_retirement_worker_owner_abandonments =
                            self.render_retirement_worker_owner_abandonments.saturating_add(1);
                    }
                    RenderWorkerJoinOutcome::Panicked
                }
            }
        };
        match outcome {
            RenderWorkerJoinOutcome::Terminated => {
                self.render_retirement_workers_terminated =
                    self.render_retirement_workers_terminated.saturating_add(1);
            }
            RenderWorkerJoinOutcome::Panicked => {
                self.render_retirement_workers_terminated =
                    self.render_retirement_workers_terminated.saturating_add(1);
                self.render_retirement_worker_panics =
                    self.render_retirement_worker_panics.saturating_add(1);
            }
            RenderWorkerJoinOutcome::TerminalEvidenceMissing => {
                self.render_retirement_workers_terminated =
                    self.render_retirement_workers_terminated.saturating_add(1);
                self.render_retirement_worker_terminal_evidence_missing =
                    self.render_retirement_worker_terminal_evidence_missing.saturating_add(1);
            }
            RenderWorkerJoinOutcome::CurrentThreadSkipped => {
                self.render_retirement_current_thread_detachments =
                    self.render_retirement_current_thread_detachments.saturating_add(1);
            }
        }
        outcome
    }

    fn join_render_worker(&mut self, worker: JoinHandle<()>) -> RenderWorkerJoinOutcome {
        let outcome = if worker.thread().id() == thread::current().id() {
            drop(worker);
            RenderWorkerJoinOutcome::CurrentThreadSkipped
        } else {
            match worker.join() {
                Ok(()) => match self.render_worker_terminal.load(AtomicOrdering::Acquire) {
                    RENDER_WORKER_TERMINAL_NORMAL => RenderWorkerJoinOutcome::Terminated,
                    RENDER_WORKER_TERMINAL_PANICKED => RenderWorkerJoinOutcome::Panicked,
                    RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED => {
                        self.render_worker_owner_abandonments =
                            self.render_worker_owner_abandonments.saturating_add(1);
                        RenderWorkerJoinOutcome::Panicked
                    }
                    _ => RenderWorkerJoinOutcome::TerminalEvidenceMissing,
                },
                Err(payload) => {
                    if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                        self.render_worker_owner_abandonments =
                            self.render_worker_owner_abandonments.saturating_add(1);
                    }
                    RenderWorkerJoinOutcome::Panicked
                }
            }
        };
        match outcome {
            RenderWorkerJoinOutcome::Terminated => {
                self.render_workers_terminated = self.render_workers_terminated.saturating_add(1);
            }
            RenderWorkerJoinOutcome::Panicked => {
                self.render_workers_terminated = self.render_workers_terminated.saturating_add(1);
                self.render_worker_panics = self.render_worker_panics.saturating_add(1);
            }
            RenderWorkerJoinOutcome::TerminalEvidenceMissing => {
                self.render_workers_terminated = self.render_workers_terminated.saturating_add(1);
                self.render_worker_terminal_evidence_missing =
                    self.render_worker_terminal_evidence_missing.saturating_add(1);
            }
            RenderWorkerJoinOutcome::CurrentThreadSkipped => {
                self.render_current_thread_detachments =
                    self.render_current_thread_detachments.saturating_add(1);
            }
        }
        outcome
    }

    /// Install one immutable timeline PCM Adapter and start a new generation at `anchor`.
    pub fn prepare(
        &mut self,
        anchor: AudioSamplePosition,
        renderer: Arc<dyn AudioPcmRenderer>,
        continuity_model: AudioPcmContinuityModel,
    ) -> Result<(), AudioPlaybackError> {
        if let Err(error) = self
            .validate_reprime_anchor_pure(anchor)
            .and_then(|()| self.ensure_render_execution_available())
            .and_then(|()| self.validate_reprime_output())
        {
            self.handoff_retired_render_owners(VecDeque::new(), Some(renderer));
            return Err(error);
        }
        self.reprime_prevalidated(
            anchor,
            false,
            Some(PreparedAudioPcmRenderer { owner: renderer, continuity_model }),
        )
    }

    /// Remove timeline PCM rendering and invalidate all outstanding work.
    pub fn clear_source(&mut self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        self.validate_reprime_anchor_pure(anchor)?;
        self.ensure_render_execution_available()?;
        self.validate_reprime_output()?;
        self.reprime_prevalidated(anchor, false, None)
    }

    /// Invalidate outstanding work and restart PCM scheduling at an exact timeline anchor.
    pub fn reprime(&mut self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        self.validate_reprime_anchor_pure(anchor)?;
        self.ensure_render_execution_available()?;
        self.validate_reprime_output()?;
        self.consecutive_render_generation_failures = 0;
        self.render_blocked = false;
        self.reprime_prevalidated(anchor, false, self.renderer.clone())
    }

    /// Validate an already-resolved sample anchor without changing any state.
    ///
    /// App transport can use this preflight before committing another Module's
    /// state. Timeline-to-sample lowering remains an upstream transport
    /// responsibility; this method only enforces the Media-side contract.
    pub fn validate_anchor(&self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        self.validate_reprime_anchor_pure(anchor)?;
        self.ensure_render_execution_available()?;
        self.validate_reprime_output()
    }

    /// Request destruction and normal reopen of the exact current concrete
    /// stream through the production device worker.
    #[cfg(feature = "validation")]
    pub fn request_controlled_output_recycle(
        &self,
        expected_stream_generation: u64,
    ) -> Result<(), AudioPlaybackValidationError> {
        self.output.request_controlled_recycle(expected_stream_generation)
    }

    fn ensure_render_execution_available(&self) -> Result<(), AudioPlaybackError> {
        if self.render_execution_unavailable
            || self.shutdown_requested
            || self.render_retirement_faulted.load(AtomicOrdering::Acquire)
            || self.render_worker.as_ref().is_none_or(JoinHandle::is_finished)
            || self.render_retirement_worker.as_ref().is_none_or(JoinHandle::is_finished)
        {
            Err(AudioPlaybackError::ExecutionUnavailable)
        } else {
            Ok(())
        }
    }

    fn validate_reprime_anchor_pure(
        &self,
        anchor: AudioSamplePosition,
    ) -> Result<(), AudioPlaybackError> {
        self.validate_sample_anchor(anchor)?;
        self.generation.checked_add(1).ok_or(AudioPlaybackError::CoordinateOverflow)?;
        Ok(())
    }

    fn validate_reprime_output(&self) -> Result<(), AudioPlaybackError> {
        self.validate_output_contract()?;
        self.output.validate_deactivation()?;
        Ok(())
    }

    fn validate_poll_arithmetic(
        &self,
        authority: AudioSamplePosition,
    ) -> Result<AudioPlaybackPollPreflight, AudioPlaybackError> {
        self.validate_sample_anchor(authority)?;
        self.validate_output_contract()?;
        self.output.validate_deactivation()?;
        self.generation
            .checked_add(MAX_GENERATION_ROTATIONS_PER_POLL)
            .ok_or(AudioPlaybackError::CoordinateOverflow)?;
        let chunk_frames = i64::try_from(self.config.chunk_frames)
            .map_err(|_| AudioPlaybackError::CoordinateOverflow)?;
        let maximum_admissions = i64::try_from(self.config.max_in_flight)
            .map_err(|_| AudioPlaybackError::CoordinateOverflow)?;
        let maximum_sample_span = chunk_frames
            .checked_mul(maximum_admissions)
            .ok_or(AudioPlaybackError::CoordinateOverflow)?;
        self.next_start_sample
            .checked_add(maximum_sample_span)
            .ok_or(AudioPlaybackError::CoordinateOverflow)?;
        authority
            .sample()
            .checked_add(maximum_sample_span)
            .ok_or(AudioPlaybackError::CoordinateOverflow)?;
        let output_snapshot = self.output.snapshot();
        let elapsed_skip_frames = if output_snapshot.is_some_and(|snapshot| !snapshot.active) {
            self.generation_render_anchor
                .map(|render_anchor| authority.samples_since(render_anchor))
                .transpose()?
                .and_then(|delta| usize::try_from(delta).ok())
        } else {
            None
        };
        let capacity_frames = self.output.capacity_frames().unwrap_or(
            output_capacity_frames(self.config.sample_rate)
                .ok_or(AudioPlaybackError::CoordinateOverflow)?,
        );
        if let Some(skip_frames) = elapsed_skip_frames {
            let activation_frames = skip_frames
                .checked_add(self.config.preroll_frames)
                .ok_or(AudioPlaybackError::CoordinateOverflow)?;
            if activation_frames > capacity_frames {
                return Err(AudioPlaybackError::HiddenPrerollExceedsOutputCapacity {
                    skip_frames,
                    preroll_frames: self.config.preroll_frames,
                    capacity_frames,
                });
            }
        }
        let admission_target_frames = elapsed_skip_frames
            .unwrap_or(0)
            .checked_add(self.config.high_watermark_frames)
            .ok_or(AudioPlaybackError::CoordinateOverflow)?
            .min(capacity_frames);
        Ok(AudioPlaybackPollPreflight {
            authority,
            chunk_frames,
            elapsed_skip_frames,
            admission_target_frames,
        })
    }

    fn reprime_prevalidated(
        &mut self,
        anchor: AudioSamplePosition,
        recovery_preroll: bool,
        renderer: Option<PreparedAudioPcmRenderer>,
    ) -> Result<(), AudioPlaybackError> {
        let quiescence_token = match self.output.deactivate() {
            Ok(token) => token,
            Err(error) => {
                if let Some(renderer) = renderer {
                    self.handoff_retired_render_owners(VecDeque::new(), Some(renderer.owner));
                }
                return Err(error.into());
            }
        };
        self.generation_cancellation.cancel();
        let retired_renderer = std::mem::replace(&mut self.renderer, renderer);
        self.output.clear();
        let pending = self.render_queue.take_pending();
        self.canceled_render_count =
            self.canceled_render_count.saturating_add(pending.len() as u64);
        self.handoff_retired_render_owners(
            pending,
            retired_renderer.map(|renderer| renderer.owner),
        );
        self.generation_cancellation = ExecutionCancellationToken::new();
        self.generation += 1;
        self.generation_entry_pending = true;
        self.in_flight = 0;
        self.next_start_sample = anchor.sample();
        self.generation_render_anchor = self.renderer.as_ref().map(|_| anchor);
        self.media_anchor = None;
        self.quiescence_token = quiescence_token;
        self.output_generation_ready = false;
        self.activation_preroll_satisfied = false;
        let underrun_frames = self.output.snapshot().map_or(0, |output| output.underrun_frames);
        self.underrun_baseline_frames = underrun_frames;
        self.last_underrun_frames = underrun_frames;
        self.recovery_preroll = recovery_preroll;
        Ok(())
    }

    fn validate_sample_anchor(
        &self,
        anchor: AudioSamplePosition,
    ) -> Result<(), AudioPlaybackError> {
        validate_audio_playback_anchor(anchor, self.config.sample_rate)
    }

    fn validate_output_contract(&self) -> Result<(), AudioPlaybackError> {
        let Some(snapshot) = self.output.snapshot() else {
            return Ok(());
        };
        if snapshot.contract.sample_rate != self.config.sample_rate {
            return Err(AudioPlaybackError::OutputSampleRateMismatch {
                expected: self.config.sample_rate,
                actual: snapshot.contract.sample_rate,
            });
        }
        let actual_layout = snapshot.contract.channel_layout;
        if actual_layout != self.config.channel_layout {
            return Err(AudioPlaybackError::OutputChannelLayoutMismatch {
                expected: self.config.channel_layout,
                actual: actual_layout,
            });
        }
        Ok(())
    }

    /// Poll lifecycle, completions, watermarks, and preroll without waiting on workers.
    pub fn poll(
        &mut self,
        mode: AudioPlaybackMode,
        authority: AudioSamplePosition,
    ) -> Result<AudioPlaybackPoll, AudioPlaybackError> {
        let mut events = Vec::new();
        if self.render_execution_unavailable || self.shutdown_requested {
            return Ok(AudioPlaybackPoll { snapshot: self.snapshot(mode), events });
        }
        if self.render_retirement_faulted.load(AtomicOrdering::Acquire) {
            if self.mark_render_execution_unavailable() {
                events.push(AudioPlaybackEvent::RenderWorkerStoppedUnexpectedly {
                    reason: "render-owner retirement became unavailable".to_owned(),
                });
            }
            return Ok(AudioPlaybackPoll { snapshot: self.snapshot(mode), events });
        }
        if let Some(reason) = self.finished_render_worker_reason() {
            if self.mark_render_execution_unavailable() {
                events.push(AudioPlaybackEvent::RenderWorkerStoppedUnexpectedly { reason });
            }
            return Ok(AudioPlaybackPoll { snapshot: self.snapshot(mode), events });
        }

        // This is the sole fallible part of a poll. It reserves every possible
        // generation rotation and the largest sample-cursor advance before an
        // output event, completion, queue entry, or callback state is consumed.
        let preflight = self.validate_poll_arithmetic(authority)?;
        let mut elapsed_skip_frames = preflight.elapsed_skip_frames;
        let mut admission_target_frames = preflight.admission_target_frames;
        let mut generation_rotations = 0_u64;

        let should_poll_output =
            self.output.snapshot().is_some() || (mode.renders_pcm() && self.renderer.is_some());
        if should_poll_output {
            // Lifecycle work remains bounded and leaves later events queued for
            // the next independently preflighted poll.
            if let Some(event) = self.output.poll() {
                match event {
                    RealtimeAudioOutputEvent::Opened { stream_generation, evidence } => {
                        self.validate_output_contract()?;
                        let observed = self
                            .output
                            .snapshot()
                            .ok_or(RealtimeAudioOutputControlError::OutputUnavailable)?
                            .contract;
                        if evidence.contract != observed {
                            return Err(AudioPlaybackError::OutputNegotiationEvidenceMismatch {
                                selected: evidence.contract,
                                observed,
                            });
                        }
                        self.latest_output_device_evidence = Some(evidence.clone());
                        self.output_lifecycle.opened_count =
                            self.output_lifecycle.opened_count.saturating_add(1);
                        self.output_lifecycle.last_opened_generation = Some(stream_generation);
                        self.stream_media_anchor = None;
                        self.consecutive_render_generation_failures = 0;
                        self.render_blocked = false;
                        self.reprime_prevalidated(
                            preflight.authority,
                            false,
                            self.renderer.clone(),
                        )?;
                        elapsed_skip_frames = Some(0);
                        admission_target_frames = self.config.high_watermark_frames;
                        generation_rotations += 1;
                        events
                            .push(AudioPlaybackEvent::DeviceOpened { stream_generation, evidence });
                    }
                    RealtimeAudioOutputEvent::Lost { reason, final_snapshot } => {
                        let final_media_anchor = self
                            .stream_media_anchor
                            .filter(|(stream_generation, _)| {
                                *stream_generation == final_snapshot.stream_generation
                            })
                            .map(|(_, anchor)| anchor);
                        self.stream_media_anchor = None;
                        self.output_lifecycle.lost_count =
                            self.output_lifecycle.lost_count.saturating_add(1);
                        match reason {
                            RealtimeAudioOutputLossReason::ControlledRecycle => {
                                self.output_lifecycle.controlled_recycle_count = self
                                    .output_lifecycle
                                    .controlled_recycle_count
                                    .saturating_add(1);
                            }
                            RealtimeAudioOutputLossReason::BackendFailure => {
                                self.output_lifecycle.backend_loss_count =
                                    self.output_lifecycle.backend_loss_count.saturating_add(1);
                            }
                            RealtimeAudioOutputLossReason::DeactivationFailed => {
                                self.output_lifecycle.deactivation_failed_count = self
                                    .output_lifecycle
                                    .deactivation_failed_count
                                    .saturating_add(1);
                            }
                            RealtimeAudioOutputLossReason::DefaultDeviceChanged => {
                                self.output_lifecycle.default_device_change_count = self
                                    .output_lifecycle
                                    .default_device_change_count
                                    .saturating_add(1);
                            }
                            RealtimeAudioOutputLossReason::DeviceSelectionChanged => {
                                self.output_lifecycle.device_selection_change_count = self
                                    .output_lifecycle
                                    .device_selection_change_count
                                    .saturating_add(1);
                            }
                        }
                        self.output_lifecycle.last_lost_generation =
                            Some(final_snapshot.stream_generation);
                        self.output_lifecycle.last_loss = Some(AudioOutputLossSnapshot {
                            reason,
                            final_output: final_snapshot,
                            final_media_anchor,
                        });
                        self.invalidate_generation_without_output(preflight.authority, true);
                        elapsed_skip_frames = None;
                        generation_rotations += 1;
                        events.push(AudioPlaybackEvent::DeviceLost {
                            reason,
                            final_output: final_snapshot,
                            final_media_anchor,
                        });
                    }
                    RealtimeAudioOutputEvent::OpenFailed { retry_after, failure } => {
                        events.push(AudioPlaybackEvent::DeviceOpenFailed { retry_after, failure });
                    }
                    RealtimeAudioOutputEvent::WorkerStartFailed { reason } => {
                        events.push(AudioPlaybackEvent::DeviceWorkerStartFailed { reason });
                    }
                    RealtimeAudioOutputEvent::WorkerStoppedUnexpectedly { reason } => {
                        self.stream_media_anchor = None;
                        self.invalidate_generation_without_output(preflight.authority, true);
                        elapsed_skip_frames = None;
                        generation_rotations += 1;
                        events.push(AudioPlaybackEvent::DeviceWorkerStoppedUnexpectedly { reason });
                    }
                }
            }
        }

        if let Some(token) = self.quiescence_token
            && !self.output_generation_ready
        {
            match self.output.is_quiescent(token) {
                Ok(true) => {
                    // The acknowledgement closes every callback block that could
                    // have observed the previous active revision. Clear once more
                    // before admitting any PCM for the new render generation.
                    self.output.clear();
                    self.output_generation_ready = true;
                }
                Ok(false) => {}
                Err(RealtimeAudioOutputControlError::QuiescenceRevisionMismatch {
                    token_revision,
                    current_revision,
                }) if current_revision > token_revision => {
                    // The device owner has begun a newer deactivation while
                    // retiring this exact stream. The old token cannot admit
                    // PCM or reactivate the callback, but the frozen Lost
                    // event is published only after concrete stream drop.
                    // Keep the generation closed until that lifecycle event
                    // invalidates the old output on a later bounded poll.
                }
                Err(error) => return Err(error.into()),
            }
        }

        loop {
            let completion = match self.completion_rx.try_recv() {
                Ok(completion) => completion,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if self.mark_render_execution_unavailable() {
                        events.push(AudioPlaybackEvent::RenderWorkerStoppedUnexpectedly {
                            reason: "render completion channel disconnected".to_owned(),
                        });
                    }
                    return Ok(AudioPlaybackPoll { snapshot: self.snapshot(mode), events });
                }
            };
            if completion.generation != self.generation {
                self.stale_completion_count = self.stale_completion_count.saturating_add(1);
                continue;
            }
            self.in_flight = self.in_flight.saturating_sub(1);
            let admission = if !self.output_generation_ready {
                CompletionAdmission::InvalidateGeneration(
                    "current-generation PCM completed before callback quiescence".to_owned(),
                )
            } else {
                match validate_rendered_buffer(completion.request, completion.result) {
                    Ok(buffer) => match self.output.enqueue(&buffer) {
                        Ok(()) => CompletionAdmission::Accepted,
                        Err(error) => CompletionAdmission::InvalidateGeneration(format!(
                            "device output rejected complete PCM window: {error}"
                        )),
                    },
                    Err(reason)
                        if completion.continuity_model
                            == AudioPcmContinuityModel::IndependentWindows =>
                    {
                        let silence = AudioBuffer::silent(
                            completion.request.sample_rate,
                            completion.request.channel_layout,
                            completion.request.frame_count,
                        );
                        match self.output.enqueue(&silence) {
                            Ok(()) => CompletionAdmission::SubstitutedWithSilence(reason),
                            Err(error) => CompletionAdmission::InvalidateGeneration(format!(
                            "render failed ({reason}); exact-silence output was rejected: {error}"
                        )),
                        }
                    }
                    Err(reason) => CompletionAdmission::InvalidateGeneration(reason),
                }
            };
            match admission {
                CompletionAdmission::Accepted => {}
                CompletionAdmission::SubstitutedWithSilence(reason) => {
                    self.render_substitution_count =
                        self.render_substitution_count.saturating_add(1);
                    events.push(AudioPlaybackEvent::RenderSubstitutedWithSilence {
                        generation: completion.generation,
                        start_sample: completion.request.start_sample,
                        reason,
                    });
                }
                CompletionAdmission::InvalidateGeneration(reason) => {
                    let final_output = self.output.snapshot();
                    let final_media_anchor = self.media_anchor;
                    let failed_generation = completion.generation;
                    let failed_start_sample = completion.request.start_sample;
                    self.render_generation_recovery_count =
                        self.render_generation_recovery_count.saturating_add(1);
                    self.consecutive_render_generation_failures =
                        self.consecutive_render_generation_failures.saturating_add(1);
                    let disposition = if self.consecutive_render_generation_failures
                        >= self.config.max_consecutive_render_recoveries
                    {
                        AudioRenderRecoveryDisposition::Blocked
                    } else {
                        AudioRenderRecoveryDisposition::Reprime
                    };
                    self.reprime_prevalidated(preflight.authority, true, self.renderer.clone())?;
                    elapsed_skip_frames = Some(0);
                    admission_target_frames = self.config.high_watermark_frames;
                    generation_rotations += 1;
                    if disposition == AudioRenderRecoveryDisposition::Blocked {
                        self.render_blocked = true;
                        self.recovery_preroll = false;
                    }
                    events.push(AudioPlaybackEvent::RenderGenerationInvalidated {
                        failed_generation,
                        restart_generation: self.generation,
                        failed_start_sample,
                        restart_anchor: preflight.authority,
                        final_output,
                        final_media_anchor,
                        reason,
                        disposition,
                    });
                }
            }
        }

        if !mode.permits_consumption()
            && self.output.snapshot().is_some_and(|snapshot| snapshot.active)
        {
            self.reprime_prevalidated(
                preflight.authority,
                mode != AudioPlaybackMode::Idle,
                self.renderer.clone(),
            )?;
            elapsed_skip_frames = Some(0);
            admission_target_frames = self.config.high_watermark_frames;
            generation_rotations += 1;
        }

        if mode == AudioPlaybackMode::Idle {
            if self.output_generation_ready {
                self.output.clear();
            }
            self.activation_preroll_satisfied = false;
            self.recovery_preroll = false;
            return Ok(AudioPlaybackPoll { snapshot: self.snapshot(mode), events });
        }

        let active_output = if mode.permits_consumption() {
            self.output.snapshot().filter(|output| output.active)
        } else {
            None
        };
        if let Some(output) = active_output
            && output.underrun_frames > self.last_underrun_frames
        {
            let delta_frames = output.underrun_frames - self.last_underrun_frames;
            let interval_total_frames =
                output.underrun_frames.saturating_sub(self.underrun_baseline_frames);
            self.last_underrun_frames = output.underrun_frames;
            events.push(AudioPlaybackEvent::UnderrunObserved {
                stream_generation: output.stream_generation,
                delta_frames,
                interval_total_frames,
            });
            if interval_total_frames >= self.config.underrun_recovery_threshold_frames {
                let final_media_anchor =
                    self.media_anchor.ok_or(AudioPlaybackError::ActiveOutputMissingMediaAnchor)?;
                events.push(AudioPlaybackEvent::UnderrunRecoveryStarted {
                    stream_generation: output.stream_generation,
                    missing_frames: interval_total_frames,
                    threshold_frames: self.config.underrun_recovery_threshold_frames,
                    final_output: output,
                    final_media_anchor,
                });
                self.underrun_recovery_count = self.underrun_recovery_count.saturating_add(1);
                debug_assert!(
                    generation_rotations < MAX_GENERATION_ROTATIONS_PER_POLL,
                    "poll generation-rotation bound must cover underrun recovery"
                );
                self.reprime_prevalidated(preflight.authority, true, self.renderer.clone())?;
                elapsed_skip_frames = Some(0);
                admission_target_frames = self.config.high_watermark_frames;
                generation_rotations += 1;
            }
        }

        if !self.render_blocked
            && self.output_generation_ready
            && let (Some(renderer), Some(_)) = (self.renderer.as_ref(), self.output.snapshot())
        {
            while self.has_pcm_admission_capacity(admission_target_frames) {
                // The poll preflight proved this addition for every one of
                // the at-most `max_in_flight` admissions from either the
                // pre-poll cursor or any reprime anchor.
                let next_start_sample = self.next_start_sample + preflight.chunk_frames;
                let request = AudioPcmRenderRequest {
                    start_sample: self.next_start_sample,
                    frame_count: self.config.chunk_frames,
                    sample_rate: self.config.sample_rate,
                    channel_layout: self.config.channel_layout,
                    continuity: if self.generation_entry_pending {
                        AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(self.generation))
                    } else {
                        AudioPcmContinuity::Continue(AudioPcmRenderGeneration::new(self.generation))
                    },
                };
                let work = RenderWork {
                    generation: self.generation,
                    request,
                    continuity_model: renderer.continuity_model,
                    renderer: Arc::clone(&renderer.owner),
                    cancellation: self.generation_cancellation.clone(),
                };
                if self.render_queue.push(work).is_err() {
                    break;
                }
                self.generation_entry_pending = false;
                self.in_flight += 1;
                self.next_start_sample = next_start_sample;
            }
            let activation_threshold =
                elapsed_skip_frames.and_then(|skip| skip.checked_add(self.config.preroll_frames));
            if activation_threshold
                .is_some_and(|required| self.output.buffered_frames() >= required)
            {
                self.activation_preroll_satisfied = true;
                self.consecutive_render_generation_failures = 0;
                if mode.permits_consumption()
                    && self.output.snapshot().is_some_and(|snapshot| !snapshot.active)
                {
                    let skip_frames =
                        elapsed_skip_frames.ok_or(AudioPlaybackError::CoordinateOverflow)?;
                    let token =
                        self.quiescence_token.ok_or(AudioPlaybackError::MissingQuiescenceToken)?;
                    match self.output.activate_after_discard(token, skip_frames) {
                        Ok(()) => {
                            self.media_anchor = Some(preflight.authority);
                            self.stream_media_anchor =
                                Some((token.stream_generation, preflight.authority));
                            self.recovery_preroll = false;
                        }
                        Err(RealtimeAudioOutputControlError::QuiescenceRevisionMismatch {
                            token_revision,
                            current_revision,
                        }) if current_revision > token_revision => {
                            // Device retirement won the atomic control
                            // transition. No prefix was discarded and no
                            // activation occurred; wait for frozen Lost.
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }

        debug_assert!(generation_rotations <= MAX_GENERATION_ROTATIONS_PER_POLL);
        Ok(AudioPlaybackPoll { snapshot: self.snapshot(mode), events })
    }

    fn has_pcm_admission_capacity(&self, admission_target_frames: usize) -> bool {
        if self.in_flight >= self.config.max_in_flight {
            return false;
        }
        let buffered_frames = self.output.buffered_frames();
        let Some(next_in_flight) = self.in_flight.checked_add(1) else {
            return false;
        };
        let Some(reserved_render_frames) = next_in_flight.checked_mul(self.config.chunk_frames)
        else {
            return false;
        };
        let Some(committed_frames) = buffered_frames.checked_add(reserved_render_frames) else {
            return false;
        };
        let Some(output_capacity_frames) = output_capacity_frames(self.config.sample_rate) else {
            return false;
        };
        committed_frames <= admission_target_frames && committed_frames <= output_capacity_frames
    }

    fn finished_render_worker_reason(&mut self) -> Option<String> {
        let is_finished = self.render_worker.as_ref().is_some_and(JoinHandle::is_finished);
        if !is_finished {
            return None;
        }
        let Some(worker) = self.render_worker.take() else {
            return Some("render worker ownership was lost".to_owned());
        };
        Some(match self.join_render_worker(worker) {
            RenderWorkerJoinOutcome::Terminated => {
                "render worker exited without shutdown".to_owned()
            }
            RenderWorkerJoinOutcome::Panicked => "render worker panicked".to_owned(),
            RenderWorkerJoinOutcome::TerminalEvidenceMissing => {
                "render worker exited without supervisor terminal evidence".to_owned()
            }
            RenderWorkerJoinOutcome::CurrentThreadSkipped => {
                "render worker could not join itself".to_owned()
            }
        })
    }

    fn invalidate_generation_without_output(
        &mut self,
        authority: AudioSamplePosition,
        recovery_preroll: bool,
    ) {
        self.generation_cancellation.cancel();
        let pending = self.render_queue.take_pending();
        self.canceled_render_count =
            self.canceled_render_count.saturating_add(pending.len() as u64);
        self.handoff_retired_render_owners(pending, None);
        self.generation_cancellation = ExecutionCancellationToken::new();
        self.generation += 1;
        self.generation_entry_pending = true;
        self.in_flight = 0;
        self.next_start_sample = authority.sample();
        self.generation_render_anchor = self.renderer.as_ref().map(|_| authority);
        self.media_anchor = None;
        self.quiescence_token = None;
        self.output_generation_ready = false;
        self.activation_preroll_satisfied = false;
        self.underrun_baseline_frames = 0;
        self.last_underrun_frames = 0;
        self.recovery_preroll = recovery_preroll;
    }

    fn mark_render_execution_unavailable(&mut self) -> bool {
        if self.render_execution_unavailable {
            return false;
        }
        self.render_execution_unavailable = true;
        match self.output.deactivate() {
            Ok(token) => self.quiescence_token = token,
            Err(error) => {
                tracing::error!(%error, "failed closed while deactivating unavailable audio render execution");
                self.quiescence_token = None;
            }
        }
        self.output_generation_ready = false;
        self.generation_cancellation.cancel();
        let pending = self.render_queue.stop();
        self.canceled_render_count =
            self.canceled_render_count.saturating_add(pending.len() as u64);
        let retired_renderer = self.renderer.take();
        self.handoff_retired_render_owners(
            pending,
            retired_renderer.map(|renderer| renderer.owner),
        );
        self.in_flight = 0;
        self.generation_render_anchor = None;
        self.media_anchor = None;
        self.stream_media_anchor = None;
        self.activation_preroll_satisfied = false;
        self.recovery_preroll = false;
        true
    }

    /// Publish a latest-wins runtime output-device intent.
    ///
    /// A changed intent is observed by the device lifecycle worker and lowered
    /// through the same Lost/Open generation handoff as physical device loss.
    /// It does not mutate Project or Sequence state.
    pub fn set_output_device_selection(
        &self,
        selection: RealtimeAudioOutputDeviceSelection,
    ) -> bool {
        self.output.set_device_selection(selection)
    }

    /// Most recent successful physical device/configuration negotiation.
    ///
    /// Evidence remains available after loss so diagnostics can explain which
    /// concrete contract preceded the synthetic-clock handoff.
    pub fn latest_output_device_evidence(&self) -> Option<&RealtimeAudioOutputDeviceEvidence> {
        self.latest_output_device_evidence.as_ref()
    }

    /// Return immutable state without advancing workers or lifecycle.
    pub fn snapshot(&self, mode: AudioPlaybackMode) -> AudioPlaybackSnapshot {
        let output = self.output.snapshot();
        let state = if self.render_execution_unavailable
            || self.shutdown_requested
            || self.render_retirement_faulted.load(AtomicOrdering::Acquire)
            || self.render_worker.as_ref().is_none_or(JoinHandle::is_finished)
            || self.render_retirement_worker.as_ref().is_none_or(JoinHandle::is_finished)
        {
            AudioPlaybackState::ExecutionUnavailable
        } else {
            match output {
                None => AudioPlaybackState::DeviceUnavailable,
                Some(_) if mode == AudioPlaybackMode::Idle => AudioPlaybackState::Idle,
                Some(_) if self.renderer.is_none() => AudioPlaybackState::WaitingForSource,
                Some(_) if self.render_blocked => AudioPlaybackState::RenderBlocked,
                Some(_) if self.recovery_preroll => AudioPlaybackState::Recovering,
                Some(snapshot) if snapshot.active => AudioPlaybackState::Active,
                Some(_) => AudioPlaybackState::Prerolling,
            }
        };
        AudioPlaybackSnapshot {
            generation: self.generation,
            in_flight: self.in_flight,
            next_start_sample: self.next_start_sample,
            media_anchor: self.media_anchor,
            activation_preroll_satisfied: self.activation_preroll_satisfied,
            state,
            output,
            render_substitution_count: self.render_substitution_count,
            render_generation_recovery_count: self.render_generation_recovery_count,
            stale_completion_count: self.stale_completion_count,
            canceled_render_count: self.canceled_render_count,
            active_interval_underrun_frames: output.map_or(0, |output| {
                output.underrun_frames.saturating_sub(self.underrun_baseline_frames)
            }),
            underrun_recovery_count: self.underrun_recovery_count,
            output_lifecycle: self.output_lifecycle,
        }
    }
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        let render_worker_detached = self.stop_render_worker_without_waiting();
        self.flush_shutdown_pending_render_work();
        let retirement_worker_detached = self.stop_render_retirement_worker_without_waiting();
        if let Some(output) = self.output.take() {
            handoff_ordinary_audio_owner(output);
        }
        if let Some(renderer) = self.renderer.take() {
            handoff_ordinary_audio_owner(renderer.owner);
        }
        if self.render_worker_panics > 0 {
            tracing::error!("Audio Playback render worker panicked during shutdown");
        }
        if self.render_current_thread_detachments > 0 {
            tracing::error!("Audio Playback render worker could not synchronously join itself");
        }
        if render_worker_detached {
            tracing::warn!(
                "Audio Playback render worker was still running and detached during ordinary drop"
            );
        }
        if self.render_retirement_worker_panics > 0 {
            tracing::error!("Audio Playback render-owner retirement worker panicked");
        }
        if retirement_worker_detached {
            tracing::warn!(
                "Audio Playback render-owner retirement worker was still running and detached during ordinary drop"
            );
        }
    }
}

fn spawn_render_worker(
    worker_queue: Arc<RenderWorkQueue>,
    completion_tx: mpsc::Sender<RenderCompletion>,
    terminal: Arc<AtomicU8>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name("mondrian-audio-render".to_owned()).spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_render_worker(&worker_queue, &completion_tx)
        }));
        let terminal_outcome = match outcome {
            Ok(terminal_outcome) => terminal_outcome,
            Err(payload) => {
                if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                    RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
                } else {
                    RENDER_WORKER_TERMINAL_PANICKED
                }
            }
        };
        terminal.store(terminal_outcome, AtomicOrdering::Release);
    })
}

fn spawn_render_owner_retirement_worker(
    queue: Arc<RenderOwnerRetirementQueue>,
    tracker: Arc<ForeignOwnerRetirementTracker>,
    terminal: Arc<AtomicU8>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("mondrian-audio-render-retirement".to_owned())
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_render_owner_retirement_worker(&queue, &tracker)
            }));
            let terminal_outcome = match outcome {
                Ok(()) => RENDER_WORKER_TERMINAL_NORMAL,
                Err(payload) => {
                    let opaque = dispose_canonical_or_abandon_opaque_panic_payload(payload);
                    queue.mark_faulted();
                    if opaque {
                        RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
                    } else {
                        RENDER_WORKER_TERMINAL_PANICKED
                    }
                }
            };
            terminal.store(terminal_outcome, AtomicOrdering::Release);
        })
}

fn join_initial_retirement_worker(worker: JoinHandle<()>) {
    if worker.thread().id() == thread::current().id() {
        drop(worker);
        return;
    }
    if let Err(payload) = worker.join() {
        let _ = dispose_canonical_or_abandon_opaque_panic_payload(payload);
    }
}

#[cfg(test)]
fn supervise_render_worker(terminal: &AtomicU8, work: impl FnOnce()) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
    let terminal_outcome = match outcome {
        Ok(()) => RENDER_WORKER_TERMINAL_NORMAL,
        Err(payload) => {
            if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
            } else {
                RENDER_WORKER_TERMINAL_PANICKED
            }
        }
    };
    terminal.store(terminal_outcome, AtomicOrdering::Release);
}

fn run_render_worker(
    worker_queue: &RenderWorkQueue,
    completion_tx: &mpsc::Sender<RenderCompletion>,
) -> u8 {
    while let Some(work) = worker_queue.pop() {
        let RenderWork {
            generation,
            request,
            continuity_model,
            renderer,
            cancellation,
        } = work;
        let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            renderer.render(request, &cancellation)
        }));
        enqueue_retired_render_owners(
            &worker_queue.retirement_queue,
            &worker_queue.retirement_tracker,
            RetiredRenderOwners {
                pending: VecDeque::new(),
                renderers: VecDeque::from([renderer]),
                errors: VecDeque::new(),
            },
        );
        let result = match rendered {
            Ok(result) => stabilize_render_result(
                result,
                &worker_queue.retirement_queue,
                &worker_queue.retirement_tracker,
            ),
            Err(payload) => {
                return if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                    RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
                } else {
                    RENDER_WORKER_TERMINAL_PANICKED
                };
            }
        };
        if completion_tx
            .send(RenderCompletion { generation, request, continuity_model, result })
            .is_err()
        {
            break;
        }
    }
    RENDER_WORKER_TERMINAL_NORMAL
}

fn stabilize_render_result(
    result: mondrian_core::Result<AudioBuffer>,
    retirement_queue: &RenderOwnerRetirementQueue,
    retirement_tracker: &ForeignOwnerRetirementTracker,
) -> RenderCompletionResult {
    match result {
        Ok(buffer) => RenderCompletionResult::Rendered(buffer),
        Err(error) => {
            enqueue_retired_render_owners(
                retirement_queue,
                retirement_tracker,
                RetiredRenderOwners {
                    pending: VecDeque::new(),
                    renderers: VecDeque::new(),
                    errors: VecDeque::from([error]),
                },
            );
            RenderCompletionResult::Failed("audio renderer returned an execution error".to_owned())
        }
    }
}

fn validate_config(config: AudioPlaybackConfig) -> Result<(), AudioPlaybackConfigError> {
    if config.sample_rate == 0
        || config.chunk_frames == 0
        || config.preroll_frames == 0
        || config.high_watermark_frames == 0
        || config.max_in_flight == 0
        || config.underrun_recovery_threshold_frames == 0
        || config.max_consecutive_render_recoveries == 0
    {
        return Err(AudioPlaybackConfigError::ZeroValue);
    }
    if config.preroll_frames > config.high_watermark_frames {
        return Err(AudioPlaybackConfigError::PrerollExceedsHighWatermark);
    }
    if config.max_in_flight >= MAX_RETIRED_RENDER_OWNERS {
        return Err(AudioPlaybackConfigError::ExceedsRetirementCapacity);
    }
    let output_capacity_frames = output_capacity_frames(config.sample_rate)
        .ok_or(AudioPlaybackConfigError::ExceedsOutputCapacity)?;
    if config.chunk_frames > output_capacity_frames
        || config.high_watermark_frames > output_capacity_frames
    {
        return Err(AudioPlaybackConfigError::ExceedsOutputCapacity);
    }
    Ok(())
}

fn output_capacity_frames(sample_rate: u32) -> Option<usize> {
    usize::try_from(sample_rate)
        .ok()
        .and_then(|sample_rate| sample_rate.checked_mul(2))
}

fn validate_rendered_buffer(
    request: AudioPcmRenderRequest,
    result: RenderCompletionResult,
) -> Result<AudioBuffer, String> {
    let buffer = match result {
        RenderCompletionResult::Rendered(buffer) => buffer,
        RenderCompletionResult::Failed(reason) => return Err(reason),
    };
    if buffer.sample_rate != request.sample_rate {
        return Err(format!(
            "rendered sample rate {} does not match requested {}",
            buffer.sample_rate, request.sample_rate
        ));
    }
    if buffer.channel_layout != request.channel_layout {
        return Err(format!(
            "rendered layout {} does not match requested {} layout",
            buffer.channel_layout, request.channel_layout,
        ));
    }
    let expected_samples = request
        .frame_count
        .checked_mul(request.channel_layout.channel_count())
        .ok_or_else(|| "requested PCM sample count overflowed".to_owned())?;
    if buffer.samples.len() != expected_samples {
        return Err(format!(
            "rendered interleaved sample count {} does not match exact requested {}",
            buffer.samples.len(),
            expected_samples,
        ));
    }
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::VecDeque;
    use std::fmt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    #[derive(Debug)]
    struct DropProbe {
        dropped: Arc<AtomicBool>,
    }

    impl fmt::Display for DropProbe {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("opaque Audio Playback test owner")
        }
    }

    impl std::error::Error for DropProbe {}

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    fn output_contract(
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
    ) -> crate::RealtimeAudioOutputContract {
        crate::RealtimeAudioOutputContract {
            sample_rate,
            channel_layout,
            sample_format: crate::RealtimeAudioSampleFormat::F32,
            channel_semantics: crate::RealtimeAudioChannelSemantics::StereoConvention,
            supported_buffer_size: crate::RealtimeAudioSupportedBufferSize::Unknown,
            candidates: crate::RealtimeAudioCandidateCounts {
                enumerated: 1,
                matching_channels: 1,
                matching_sample_rate: 1,
                executable: 1,
            },
        }
    }

    fn device_evidence(
        contract: crate::RealtimeAudioOutputContract,
    ) -> crate::RealtimeAudioOutputDeviceEvidence {
        crate::RealtimeAudioOutputDeviceEvidence {
            host_name: "test-host".to_owned(),
            device_id: crate::RealtimeAudioOutputDeviceId::new("test:test-output")
                .expect("test device identity"),
            selection: crate::RealtimeAudioOutputDeviceSelection::SystemDefault,
            was_system_default: true,
            device_name: Some("test-output".to_owned()),
            device_name_error: None,
            contract,
        }
    }

    #[derive(Default)]
    struct FakeOutputState {
        events: VecDeque<RealtimeAudioOutputEvent>,
        snapshot: Option<RealtimeAudioOutputSnapshot>,
        channel_layout: AudioChannelLayout,
        queued_frames: usize,
        reject_enqueue: bool,
        quiescence_revision: u64,
        confirmed_quiescence_revision: u64,
        auto_confirm_quiescence: bool,
        #[cfg(feature = "validation")]
        controlled_recycle_requests: Vec<u64>,
    }

    #[derive(Default)]
    struct FakeOutputControl {
        shutdown_signal: Option<Arc<AtomicBool>>,
        shutdown_started: Option<Arc<AtomicBool>>,
        shutdown_release: Option<Arc<AtomicBool>>,
        shutdown_panic_payload_dropped: Option<Arc<AtomicBool>>,
        shutdown_evidence: Option<RealtimeAudioOutputShutdownEvidence>,
        drop_started: Option<Arc<AtomicBool>>,
        drop_release: Option<Arc<AtomicBool>>,
        drop_finished: Option<Arc<AtomicBool>>,
    }

    struct FakeOutput {
        state: Arc<Mutex<FakeOutputState>>,
        control: FakeOutputControl,
    }

    impl AudioOutputAdapter for FakeOutput {
        fn shutdown_signal(&self) -> Option<Arc<AtomicBool>> {
            self.control.shutdown_signal.as_ref().map(Arc::clone)
        }

        fn poll(&mut self) -> Option<RealtimeAudioOutputEvent> {
            self.state.lock().events.pop_front()
        }

        fn enqueue(&mut self, buffer: &AudioBuffer) -> Result<(), RealtimeAudioOutputEnqueueError> {
            let mut state = self.state.lock();
            let snapshot =
                state.snapshot.ok_or(RealtimeAudioOutputEnqueueError::OutputUnavailable)?;
            if buffer.sample_rate != snapshot.contract.sample_rate {
                return Err(RealtimeAudioOutputEnqueueError::SampleRateMismatch {
                    expected: snapshot.contract.sample_rate,
                    actual: buffer.sample_rate,
                });
            }
            if buffer.channel_layout != state.channel_layout {
                return Err(RealtimeAudioOutputEnqueueError::ChannelLayoutMismatch {
                    expected: state.channel_layout,
                    actual: buffer.channel_layout,
                });
            }
            if state.reject_enqueue {
                return Err(RealtimeAudioOutputEnqueueError::InsufficientCapacity {
                    required_samples: buffer.samples.len(),
                    available_samples: 0,
                });
            }
            state.queued_frames = state.queued_frames.saturating_add(buffer.frame_count());
            let queued_frames = state.queued_frames;
            if let Some(snapshot) = state.snapshot.as_mut() {
                snapshot.buffered_frames = queued_frames;
            }
            Ok(())
        }

        fn clear(&self) {
            let mut state = self.state.lock();
            state.queued_frames = 0;
            if let Some(snapshot) = state.snapshot.as_mut() {
                snapshot.buffered_frames = 0;
            }
        }

        fn validate_deactivation(&self) -> Result<(), RealtimeAudioOutputControlError> {
            let state = self.state.lock();
            if state.snapshot.is_some_and(|snapshot| snapshot.active)
                && state.quiescence_revision == u64::MAX
            {
                return Err(
                    RealtimeAudioOutputControlError::QuiescenceRevisionExhausted {
                        stream_generation: state
                            .snapshot
                            .map_or(0, |snapshot| snapshot.stream_generation),
                    },
                );
            }
            Ok(())
        }

        fn deactivate(
            &self,
        ) -> Result<Option<RealtimeAudioOutputQuiescenceToken>, RealtimeAudioOutputControlError>
        {
            let mut state = self.state.lock();
            let Some(snapshot) = state.snapshot else {
                return Ok(None);
            };
            if snapshot.active {
                state.quiescence_revision = state.quiescence_revision.checked_add(1).ok_or(
                    RealtimeAudioOutputControlError::QuiescenceRevisionExhausted {
                        stream_generation: snapshot.stream_generation,
                    },
                )?;
                if let Some(snapshot) = state.snapshot.as_mut() {
                    snapshot.active = false;
                }
            }
            let revision = state.quiescence_revision;
            if state.auto_confirm_quiescence {
                state.confirmed_quiescence_revision = revision;
            }
            Ok(Some(RealtimeAudioOutputQuiescenceToken {
                stream_generation: snapshot.stream_generation,
                revision,
            }))
        }

        fn is_quiescent(
            &self,
            token: RealtimeAudioOutputQuiescenceToken,
        ) -> Result<bool, RealtimeAudioOutputControlError> {
            let state = self.state.lock();
            let snapshot =
                state.snapshot.ok_or(RealtimeAudioOutputControlError::OutputUnavailable)?;
            if snapshot.stream_generation != token.stream_generation {
                return Err(RealtimeAudioOutputControlError::StreamGenerationMismatch {
                    token_generation: token.stream_generation,
                    current_generation: snapshot.stream_generation,
                });
            }
            if state.quiescence_revision != token.revision {
                return Err(
                    RealtimeAudioOutputControlError::QuiescenceRevisionMismatch {
                        token_revision: token.revision,
                        current_revision: state.quiescence_revision,
                    },
                );
            }
            Ok(state.confirmed_quiescence_revision >= token.revision)
        }

        fn activate_after_discard(
            &self,
            token: RealtimeAudioOutputQuiescenceToken,
            frames: usize,
        ) -> Result<(), RealtimeAudioOutputControlError> {
            if !self.is_quiescent(token)? {
                return Err(RealtimeAudioOutputControlError::CallbackNotQuiescent {
                    revision: token.revision,
                });
            }
            let mut state = self.state.lock();
            if frames > state.queued_frames {
                return Err(
                    RealtimeAudioOutputControlError::InsufficientBufferedFrames {
                        requested_frames: frames,
                        buffered_frames: state.queued_frames,
                    },
                );
            }
            state.queued_frames -= frames;
            let queued_frames = state.queued_frames;
            if let Some(snapshot) = state.snapshot.as_mut() {
                snapshot.active = true;
                snapshot.active_callback_consumed_frames = 0;
                snapshot.buffered_frames = queued_frames;
            }
            Ok(())
        }

        fn buffered_frames(&self) -> usize {
            self.state.lock().queued_frames
        }

        fn capacity_frames(&self) -> Option<usize> {
            self.state
                .lock()
                .snapshot
                .and_then(|snapshot| usize::try_from(snapshot.contract.sample_rate).ok())
                .and_then(|sample_rate| sample_rate.checked_mul(2))
        }

        fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot> {
            self.state.lock().snapshot
        }

        fn shutdown_and_wait(&mut self) -> RealtimeAudioOutputShutdownEvidence {
            if let Some(started) = &self.control.shutdown_started {
                started.store(true, Ordering::Release);
            }
            if let Some(release) = &self.control.shutdown_release {
                while !release.load(Ordering::Acquire) {
                    thread::yield_now();
                }
            }
            if let Some(dropped) = self.control.shutdown_panic_payload_dropped.take() {
                std::panic::panic_any(DropProbe { dropped });
            }
            self.control
                .shutdown_evidence
                .take()
                .unwrap_or_else(RealtimeAudioOutputShutdownEvidence::no_worker_owner)
        }

        #[cfg(feature = "validation")]
        fn request_controlled_recycle(
            &self,
            expected_stream_generation: u64,
        ) -> Result<(), AudioPlaybackValidationError> {
            let mut state = self.state.lock();
            let current = state
                .snapshot
                .ok_or(AudioPlaybackValidationError::OutputUnavailable)?
                .stream_generation;
            if current != expected_stream_generation {
                return Err(AudioPlaybackValidationError::StreamGenerationMismatch {
                    expected: expected_stream_generation,
                    actual: current,
                });
            }
            state.controlled_recycle_requests.push(expected_stream_generation);
            Ok(())
        }
    }

    impl Drop for FakeOutput {
        fn drop(&mut self) {
            if let Some(started) = &self.control.drop_started {
                started.store(true, Ordering::Release);
            }
            if let Some(release) = &self.control.drop_release {
                while !release.load(Ordering::Acquire) {
                    thread::yield_now();
                }
            }
            if let Some(finished) = &self.control.drop_finished {
                finished.store(true, Ordering::Release);
            }
        }
    }

    struct RecordingRenderer {
        requests: Arc<Mutex<Vec<AudioPcmRenderRequest>>>,
        wrong_frame_count: bool,
    }

    struct GateRenderer {
        entered: Arc<AtomicBool>,
        released: Arc<AtomicBool>,
        canceled: Arc<AtomicBool>,
    }

    struct DetachedPanicRenderer {
        entered: Arc<AtomicBool>,
        released: Arc<AtomicBool>,
        payload_dropped: Arc<AtomicBool>,
    }

    struct FailOnceStatefulRenderer {
        requests: Arc<Mutex<Vec<AudioPcmRenderRequest>>>,
        failed: AtomicBool,
    }

    struct StatefulRecordingRenderer {
        requests: Arc<Mutex<Vec<AudioPcmRenderRequest>>>,
    }

    struct AlwaysFailStatefulRenderer {
        requests: Arc<Mutex<Vec<AudioPcmRenderRequest>>>,
    }

    struct DropTrackingRenderer {
        dropped: Arc<AtomicBool>,
    }

    #[derive(Debug)]
    struct BlockingRenderError {
        drop_started: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }

    impl fmt::Display for BlockingRenderError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("blocking render error")
        }
    }

    impl std::error::Error for BlockingRenderError {}

    impl Drop for BlockingRenderError {
        fn drop(&mut self) {
            self.drop_started.store(true, Ordering::Release);
            while !self.release.load(Ordering::Acquire) {
                thread::yield_now();
            }
        }
    }

    impl Drop for DropTrackingRenderer {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    impl AudioPcmRenderer for DropTrackingRenderer {
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioBuffer> {
            Ok(AudioBuffer::silent(
                request.sample_rate,
                request.channel_layout,
                request.frame_count,
            ))
        }
    }

    impl AudioPcmRenderer for GateRenderer {
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioBuffer> {
            self.entered.store(true, Ordering::Release);
            while !self.released.load(Ordering::Acquire) {
                if cancellation.is_canceled() {
                    self.canceled.store(true, Ordering::Release);
                    break;
                }
                thread::yield_now();
            }
            Ok(AudioBuffer::silent(
                request.sample_rate,
                request.channel_layout,
                request.frame_count,
            ))
        }
    }

    impl AudioPcmRenderer for DetachedPanicRenderer {
        fn render(
            &self,
            _request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioBuffer> {
            self.entered.store(true, Ordering::Release);
            while !self.released.load(Ordering::Acquire) {
                thread::yield_now();
            }
            std::panic::panic_any(DropProbe { dropped: Arc::clone(&self.payload_dropped) });
        }
    }

    impl AudioPcmRenderer for RecordingRenderer {
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioBuffer> {
            self.requests.lock().push(request);
            let frames = if self.wrong_frame_count {
                request.frame_count.saturating_sub(1)
            } else {
                request.frame_count
            };
            Ok(AudioBuffer::silent(
                request.sample_rate,
                request.channel_layout,
                frames,
            ))
        }
    }

    impl AudioPcmRenderer for FailOnceStatefulRenderer {
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioBuffer> {
            self.requests.lock().push(request);
            if matches!(request.continuity, AudioPcmContinuity::Continue(_))
                && !self.failed.swap(true, Ordering::AcqRel)
            {
                return Err(mondrian_core::MondrianError::DecodeFailed {
                    asset_id: "stateful-test".to_owned(),
                    reason: "injected continuation failure".to_owned(),
                });
            }
            Ok(AudioBuffer::silent(
                request.sample_rate,
                request.channel_layout,
                request.frame_count,
            ))
        }
    }

    impl AudioPcmRenderer for StatefulRecordingRenderer {
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioBuffer> {
            self.requests.lock().push(request);
            Ok(AudioBuffer::silent(
                request.sample_rate,
                request.channel_layout,
                request.frame_count,
            ))
        }
    }

    impl AudioPcmRenderer for AlwaysFailStatefulRenderer {
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioBuffer> {
            self.requests.lock().push(request);
            Err(mondrian_core::MondrianError::DecodeFailed {
                asset_id: "persistent-stateful-test".to_owned(),
                reason: "injected persistent failure".to_owned(),
            })
        }
    }

    fn test_config() -> AudioPlaybackConfig {
        AudioPlaybackConfig {
            sample_rate: 1_000,
            channel_layout: AudioChannelLayout::Stereo,
            chunk_frames: 10,
            preroll_frames: 20,
            high_watermark_frames: 30,
            max_in_flight: 3,
            underrun_recovery_threshold_frames: 10,
            max_consecutive_render_recoveries: 3,
        }
    }

    fn playback_with_buffered_frames(
        buffered_frames: usize,
    ) -> (AudioPlayback, Arc<Mutex<FakeOutputState>>) {
        let (output, state) = fake_output();
        {
            let mut output = state.lock();
            output.events.clear();
            output.queued_frames = buffered_frames;
            output.snapshot.as_mut().expect("fake output snapshot").buffered_frames =
                buffered_frames;
        }
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        (playback, state)
    }

    #[test]
    fn pcm_admission_rejects_when_less_than_one_complete_chunk_remains() {
        let (playback, _) = playback_with_buffered_frames(21);

        assert!(!playback.has_pcm_admission_capacity(playback.config.high_watermark_frames));
    }

    #[test]
    fn pcm_admission_accepts_exactly_one_remaining_chunk() {
        let (playback, _) = playback_with_buffered_frames(20);

        assert!(playback.has_pcm_admission_capacity(playback.config.high_watermark_frames));
    }

    #[test]
    fn pcm_admission_reserves_existing_in_flight_chunks() {
        let (mut playback, state) = playback_with_buffered_frames(10);
        playback.in_flight = 1;
        assert!(playback.has_pcm_admission_capacity(playback.config.high_watermark_frames));

        {
            let mut output = state.lock();
            output.queued_frames = 11;
            output.snapshot.as_mut().expect("fake output snapshot").buffered_frames = 11;
        }
        assert!(!playback.has_pcm_admission_capacity(playback.config.high_watermark_frames));
    }

    #[test]
    fn rendered_buffer_requires_the_exact_requested_semantic_layout() {
        let request = AudioPcmRenderRequest {
            start_sample: 0,
            frame_count: 10,
            sample_rate: 1_000,
            channel_layout: AudioChannelLayout::Stereo,
            continuity: AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(1)),
        };
        let result = validate_rendered_buffer(
            request,
            RenderCompletionResult::Rendered(AudioBuffer::silent(
                request.sample_rate,
                AudioChannelLayout::Mono,
                request.frame_count,
            )),
        );

        let error = result.expect_err("a different semantic layout must fail closed");
        assert!(error.contains("Mono"));
        assert!(error.contains("Stereo"));
    }

    #[test]
    fn rendered_buffer_rejects_a_partial_interleaved_frame() {
        let request = AudioPcmRenderRequest {
            start_sample: 0,
            frame_count: 10,
            sample_rate: 1_000,
            channel_layout: AudioChannelLayout::Stereo,
            continuity: AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(1)),
        };
        let result = validate_rendered_buffer(
            request,
            RenderCompletionResult::Rendered(AudioBuffer {
                samples: vec![0.0; 21],
                sample_rate: request.sample_rate,
                channel_layout: request.channel_layout,
            }),
        );

        let error = result.expect_err("partial stereo frame must fail closed");
        assert!(error.contains("21"));
        assert!(error.contains("20"));
    }

    fn fake_output() -> (Box<dyn AudioOutputAdapter>, Arc<Mutex<FakeOutputState>>) {
        fake_output_with_control(FakeOutputControl::default())
    }

    fn fake_output_with_control(
        control: FakeOutputControl,
    ) -> (Box<dyn AudioOutputAdapter>, Arc<Mutex<FakeOutputState>>) {
        let contract = output_contract(1_000, AudioChannelLayout::Stereo);
        let snapshot = RealtimeAudioOutputSnapshot {
            captured_at: std::time::Instant::now(),
            stream_generation: 4,
            contract,
            callback_consumed_frames: 0,
            active_callback_consumed_frames: 0,
            active_duration: None,
            callback_count: 0,
            underrun_frames: 0,
            last_callback_frames: 10,
            last_callback_playback_delay: Some(Duration::from_millis(10)),
            last_callback_age: None,
            buffered_frames: 0,
            stream_failed: false,
            active: false,
        };
        let state = Arc::new(Mutex::new(FakeOutputState {
            events: VecDeque::from([RealtimeAudioOutputEvent::Opened {
                stream_generation: 4,
                evidence: device_evidence(contract),
            }]),
            snapshot: Some(snapshot),
            channel_layout: AudioChannelLayout::Stereo,
            queued_frames: 0,
            reject_enqueue: false,
            quiescence_revision: 0,
            confirmed_quiescence_revision: 0,
            auto_confirm_quiescence: true,
            #[cfg(feature = "validation")]
            controlled_recycle_requests: Vec::new(),
        }));
        (
            Box::new(FakeOutput { state: Arc::clone(&state), control }),
            state,
        )
    }

    fn poll_until_settled(
        playback: &mut AudioPlayback,
        position: AudioSamplePosition,
    ) -> Vec<AudioPlaybackEvent> {
        poll_until_settled_in_mode(playback, position, AudioPlaybackMode::Consume)
    }

    fn poll_until_settled_in_mode(
        playback: &mut AudioPlayback,
        position: AudioSamplePosition,
        mode: AudioPlaybackMode,
    ) -> Vec<AudioPlaybackEvent> {
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let poll = playback.poll(mode, position).expect("valid Audio Playback poll");
            events.extend(poll.events);
            if poll.snapshot.in_flight == 0
                && poll.snapshot.output.is_some_and(|output| output.buffered_frames >= 30)
            {
                return events;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("audio render worker did not settle");
    }

    fn sample_position(sample: i64) -> AudioSamplePosition {
        AudioSamplePosition::new(
            sample,
            AudioSampleRate::new(1_000).expect("test sample rate"),
        )
    }

    #[test]
    fn headless_adapter_observes_integer_windows_watermark_and_preroll_activation() {
        let (output, _) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let requests = Arc::new(Mutex::new(Vec::new()));
        playback
            .prepare(
                sample_position(40),
                Arc::new(RecordingRenderer {
                    requests: Arc::clone(&requests),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("valid audio anchor");

        let events = poll_until_settled(&mut playback, sample_position(40));
        let snapshot = playback.snapshot(AudioPlaybackMode::Consume);

        assert!(events.iter().any(|event| matches!(
            event,
            AudioPlaybackEvent::DeviceOpened { stream_generation: 4, .. }
        )));
        let requests = requests.lock();
        assert_eq!(
            requests.iter().map(|request| request.start_sample).collect::<Vec<_>>(),
            vec![40, 50, 60]
        );
        let generation = AudioPcmRenderGeneration::new(snapshot.generation);
        assert_eq!(
            requests.iter().map(|request| request.continuity).collect::<Vec<_>>(),
            vec![
                AudioPcmContinuity::Enter(generation),
                AudioPcmContinuity::Continue(generation),
                AudioPcmContinuity::Continue(generation),
            ]
        );
        assert_eq!(snapshot.state, AudioPlaybackState::Active);
        assert!(snapshot.activation_preroll_satisfied);
        assert_eq!(snapshot.media_anchor, Some(sample_position(40)));
    }

    #[test]
    fn preroll_fills_pcm_without_consuming_or_resetting_generation() {
        let (output, state) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        playback
            .prepare(
                sample_position(0),
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("valid audio anchor");

        let events = poll_until_settled_in_mode(
            &mut playback,
            sample_position(0),
            AudioPlaybackMode::Preroll,
        );
        let primed = playback.snapshot(AudioPlaybackMode::Preroll);
        let primed_generation = primed.generation;
        let primed_frames = state.lock().queued_frames;

        assert!(events.iter().any(|event| matches!(
            event,
            AudioPlaybackEvent::DeviceOpened { stream_generation: 4, .. }
        )));
        assert_eq!(primed.state, AudioPlaybackState::Prerolling);
        assert!(primed.activation_preroll_satisfied);
        assert!(primed.output.is_some_and(|output| !output.active));
        assert_eq!(primed_frames, 30);

        let activated = playback
            .poll(AudioPlaybackMode::Consume, sample_position(0))
            .expect("valid Audio Playback poll")
            .snapshot;

        assert_eq!(activated.generation, primed_generation);
        assert_eq!(activated.state, AudioPlaybackState::Active);
        assert!(activated.output.is_some_and(|output| output.active));
        assert_eq!(state.lock().queued_frames, primed_frames);
    }

    #[test]
    fn malformed_render_keeps_media_duration_with_silence_and_evidence() {
        let (output, state) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        playback
            .prepare(
                sample_position(0),
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: true,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("valid audio anchor");

        let events = poll_until_settled(&mut playback, sample_position(0));

        assert_eq!(state.lock().queued_frames, 30);
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).render_substitution_count,
            3
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    AudioPlaybackEvent::RenderSubstitutedWithSilence { .. }
                ))
                .count(),
            3
        );
    }

    #[test]
    fn complete_output_enqueue_rejection_invalidates_the_generation() {
        let (output, state) = fake_output();
        state.lock().reject_enqueue = true;
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        playback
            .prepare(
                sample_position(0),
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("valid audio anchor");
        let initial_generation = playback.snapshot(AudioPlaybackMode::Consume).generation;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut invalidation = None;
        while Instant::now() < deadline && invalidation.is_none() {
            let poll = playback
                .poll(AudioPlaybackMode::Consume, sample_position(0))
                .expect("preflighted poll");
            invalidation = poll.events.into_iter().find(|event| {
                matches!(
                    event,
                    AudioPlaybackEvent::RenderGenerationInvalidated { .. }
                )
            });
            thread::sleep(Duration::from_millis(1));
        }

        assert!(matches!(
            invalidation,
            Some(AudioPlaybackEvent::RenderGenerationInvalidated {
                disposition: AudioRenderRecoveryDisposition::Reprime,
                ..
            })
        ));
        assert!(playback.snapshot(AudioPlaybackMode::Consume).generation > initial_generation);
        assert_eq!(state.lock().queued_frames, 0);
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).render_substitution_count,
            0
        );
    }

    #[test]
    fn stateful_render_failure_invalidates_generation_and_reenters_at_authority() {
        let (output, state) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let requests = Arc::new(Mutex::new(Vec::new()));
        playback
            .prepare(
                sample_position(0),
                Arc::new(FailOnceStatefulRenderer {
                    requests: Arc::clone(&requests),
                    failed: AtomicBool::new(false),
                }),
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("valid audio anchor");
        let authority = sample_position(5);

        let events = poll_until_settled(&mut playback, authority);
        let snapshot = playback.snapshot(AudioPlaybackMode::Consume);

        assert_eq!(state.lock().queued_frames, 30);
        assert_eq!(snapshot.render_substitution_count, 0);
        assert_eq!(snapshot.render_generation_recovery_count, 1);
        let recovery = events.iter().find_map(|event| match event {
            AudioPlaybackEvent::RenderGenerationInvalidated {
                failed_generation,
                restart_generation,
                failed_start_sample,
                restart_anchor,
                disposition,
                ..
            } => Some((
                *failed_generation,
                *restart_generation,
                *failed_start_sample,
                *restart_anchor,
                *disposition,
            )),
            _ => None,
        });
        let (
            failed_generation,
            restart_generation,
            failed_start_sample,
            restart_anchor,
            disposition,
        ) = recovery.expect("stateful recovery evidence");
        assert_ne!(failed_generation, restart_generation);
        assert_eq!(restart_generation, snapshot.generation);
        assert_eq!(failed_start_sample, 15);
        assert_eq!(restart_anchor, authority);
        assert_eq!(disposition, AudioRenderRecoveryDisposition::Reprime);

        let current = requests
            .lock()
            .iter()
            .filter(|request| request.continuity.generation().get() == restart_generation)
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            current.iter().map(|request| request.start_sample).collect::<Vec<_>>(),
            vec![5, 15, 25]
        );
        let generation = AudioPcmRenderGeneration::new(restart_generation);
        assert_eq!(
            current.iter().map(|request| request.continuity).collect::<Vec<_>>(),
            vec![
                AudioPcmContinuity::Enter(generation),
                AudioPcmContinuity::Continue(generation),
                AudioPcmContinuity::Continue(generation),
            ]
        );
    }

    #[test]
    fn persistent_stateful_failure_exhausts_bounded_generation_recovery() {
        let (output, state) = fake_output();
        let mut config = test_config();
        config.max_consecutive_render_recoveries = 2;
        let mut playback =
            AudioPlayback::with_output(config, output).expect("spawn test render worker");
        let requests = Arc::new(Mutex::new(Vec::new()));
        playback
            .prepare(
                sample_position(0),
                Arc::new(AlwaysFailStatefulRenderer { requests: Arc::clone(&requests) }),
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("valid audio anchor");
        let authority = sample_position(5);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            let poll = playback
                .poll(AudioPlaybackMode::Consume, authority)
                .expect("valid Audio Playback poll");
            events.extend(poll.events);
            if poll.snapshot.state == AudioPlaybackState::RenderBlocked {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        let blocked = playback.snapshot(AudioPlaybackMode::Consume);
        assert_eq!(blocked.state, AudioPlaybackState::RenderBlocked);
        assert_eq!(blocked.render_generation_recovery_count, 2);
        assert_eq!(blocked.render_substitution_count, 0);
        assert_eq!(state.lock().queued_frames, 0);
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    AudioPlaybackEvent::RenderGenerationInvalidated { disposition, .. } => {
                        Some(*disposition)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![
                AudioRenderRecoveryDisposition::Reprime,
                AudioRenderRecoveryDisposition::Blocked,
            ]
        );
        assert_eq!(
            requests
                .lock()
                .iter()
                .filter(|request| matches!(request.continuity, AudioPcmContinuity::Enter(_)))
                .count(),
            2
        );
    }

    #[test]
    fn invalid_policy_is_rejected_at_the_interface() {
        let mut config = test_config();
        config.preroll_frames = 31;
        assert!(matches!(
            AudioPlayback::new(config),
            Err(AudioPlaybackCreateError::InvalidConfig(
                AudioPlaybackConfigError::PrerollExceedsHighWatermark
            ))
        ));
        let mut config = test_config();
        config.max_consecutive_render_recoveries = 0;
        assert!(matches!(
            AudioPlayback::new(config),
            Err(AudioPlaybackCreateError::InvalidConfig(
                AudioPlaybackConfigError::ZeroValue
            ))
        ));
        let mut config = test_config();
        config.sample_rate = 10;
        config.high_watermark_frames = 21;
        assert!(matches!(
            AudioPlayback::new(config),
            Err(AudioPlaybackCreateError::InvalidConfig(
                AudioPlaybackConfigError::ExceedsOutputCapacity
            ))
        ));
    }

    #[test]
    fn retirement_capacity_accepts_255_in_flight_and_rejects_256() {
        let mut config = test_config();
        config.max_in_flight = MAX_RETIRED_RENDER_OWNERS - 1;
        assert_eq!(validate_config(config), Ok(()));

        config.max_in_flight = MAX_RETIRED_RENDER_OWNERS;
        assert_eq!(
            validate_config(config),
            Err(AudioPlaybackConfigError::ExceedsRetirementCapacity)
        );
    }

    #[test]
    fn stopped_retirement_queue_faults_and_never_drops_renderer_on_caller() {
        let faulted = Arc::new(AtomicBool::new(false));
        let queue = RenderOwnerRetirementQueue::new(Arc::clone(&faulted));
        let tracker = ForeignOwnerRetirementTracker::default();
        let dropped = Arc::new(AtomicBool::new(false));
        queue.stop();

        enqueue_retired_render_owners(
            &queue,
            &tracker,
            RetiredRenderOwners {
                pending: VecDeque::new(),
                renderers: VecDeque::from([Arc::new(DropTrackingRenderer {
                    dropped: Arc::clone(&dropped),
                }) as Arc<dyn AudioPcmRenderer>]),
                errors: VecDeque::new(),
            },
        );

        assert!(faulted.load(Ordering::Acquire));
        assert!(!dropped.load(Ordering::Acquire));
        assert_eq!(tracker.snapshot().owner_abandonments, 1);
    }

    #[test]
    fn render_completion_stabilizes_foreign_error_without_render_caller_drop() {
        let faulted = Arc::new(AtomicBool::new(false));
        let queue = Arc::new(RenderOwnerRetirementQueue::new(Arc::clone(&faulted)));
        let tracker = Arc::new(ForeignOwnerRetirementTracker::default());
        let terminal = Arc::new(AtomicU8::new(RENDER_WORKER_TERMINAL_UNKNOWN));
        let worker = spawn_render_owner_retirement_worker(
            Arc::clone(&queue),
            Arc::clone(&tracker),
            Arc::clone(&terminal),
        )
        .expect("retirement worker");
        let drop_started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));

        let completion = stabilize_render_result(
            Err(mondrian_core::MondrianError::Other(anyhow::Error::new(
                BlockingRenderError {
                    drop_started: Arc::clone(&drop_started),
                    release: Arc::clone(&release),
                },
            ))),
            &queue,
            &tracker,
        );

        assert!(matches!(completion, RenderCompletionResult::Failed(_)));
        let deadline = Instant::now() + Duration::from_secs(1);
        while !drop_started.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(drop_started.load(Ordering::Acquire));
        assert!(!faulted.load(Ordering::Acquire));
        release.store(true, Ordering::Release);
        queue.stop();
        worker.join().expect("retirement worker exits");
        assert_eq!(
            terminal.load(Ordering::Acquire),
            RENDER_WORKER_TERMINAL_NORMAL
        );
        assert_eq!(
            tracker.snapshot(),
            ForeignOwnerRetirementEvidence::default()
        );
    }

    #[test]
    fn retirement_fault_rejects_prepare_without_mutating_generation() {
        let (output, _) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let generation = playback.generation;
        playback.render_retirement_faulted.store(true, Ordering::Release);

        let result = playback.prepare(
            sample_position(0),
            Arc::new(RecordingRenderer {
                requests: Arc::new(Mutex::new(Vec::new())),
                wrong_frame_count: false,
            }),
            AudioPcmContinuityModel::IndependentWindows,
        );

        assert_eq!(result, Err(AudioPlaybackError::ExecutionUnavailable));
        assert_eq!(playback.generation, generation);
        assert!(playback.renderer.is_none());
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).state,
            AudioPlaybackState::ExecutionUnavailable
        );
    }

    #[test]
    fn explicit_continuity_contract_is_the_single_cached_authority() {
        let (output, _) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");

        playback
            .prepare(
                sample_position(0),
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("explicit continuity contract is sufficient");

        assert_eq!(
            playback.renderer.as_ref().map(|renderer| renderer.continuity_model),
            Some(AudioPcmContinuityModel::GenerationState)
        );
        assert!(playback.shutdown_and_wait().all_workers_terminated());
    }

    #[test]
    fn residual_retirement_owner_forces_top_level_receipt_unresolved() {
        let (output, _) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let dropped = Arc::new(AtomicBool::new(false));
        playback.renderer = Some(PreparedAudioPcmRenderer {
            owner: Arc::new(DropTrackingRenderer { dropped: Arc::clone(&dropped) }),
            continuity_model: AudioPcmContinuityModel::IndependentWindows,
        });
        playback.render_retirement_queue.stop();

        let evidence = playback.shutdown_and_wait();

        assert!(!dropped.load(Ordering::Acquire));
        assert_eq!(evidence.foreign_owner_abandonments, 1);
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn render_worker_spawn_failure_does_not_construct_audio_playback() {
        let (output, state) = fake_output();
        let result = AudioPlayback::with_output_and_spawner(test_config(), output, |_, _, _| {
            Err(io::Error::other("injected spawn failure"))
        });

        assert!(matches!(
            result,
            Err(AudioPlaybackCreateError::RenderWorkerSpawn(_))
        ));
        assert_eq!(state.lock().events.len(), 1);
    }

    #[test]
    fn render_worker_channel_disconnect_becomes_execution_unavailable_once() {
        let (output, _) = fake_output();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let mut playback = AudioPlayback::with_output_and_spawner(
            test_config(),
            output,
            move |_, completion_tx, terminal| {
                thread::Builder::new()
                    .name("mondrian-audio-render-disconnect-test".to_owned())
                    .spawn(move || {
                        supervise_render_worker(&terminal, || {
                            drop(completion_tx);
                            release_rx.recv().expect("release injected render worker");
                        });
                    })
            },
        )
        .expect("spawn injected render worker");

        let position = sample_position(0);
        let deadline = Instant::now() + Duration::from_secs(2);
        let failure = loop {
            let poll = playback
                .poll(AudioPlaybackMode::Consume, position)
                .expect("valid Audio Playback poll");
            if poll.events.iter().any(|event| {
                matches!(
                    event,
                    AudioPlaybackEvent::RenderWorkerStoppedUnexpectedly { .. }
                )
            }) {
                break poll;
            }
            assert!(
                Instant::now() < deadline,
                "render worker exit was not observed"
            );
            thread::yield_now();
        };

        assert_eq!(
            failure.snapshot.state,
            AudioPlaybackState::ExecutionUnavailable
        );
        assert_eq!(failure.snapshot.in_flight, 0);
        assert_eq!(playback.render_queue.state.lock().pending.len(), 0);
        release_tx.send(()).expect("release injected render worker");
        let next = playback
            .poll(AudioPlaybackMode::Consume, position)
            .expect("execution-unavailable poll remains valid");
        assert!(
            next.events.is_empty(),
            "worker failure is published exactly once"
        );
        assert_eq!(
            next.snapshot.state,
            AudioPlaybackState::ExecutionUnavailable
        );
    }

    #[test]
    fn render_worker_panic_fails_closed_instead_of_leaving_phantom_work() {
        let (output, _) = fake_output();
        let mut playback =
            AudioPlayback::with_output_and_spawner(test_config(), output, |_, completion_tx, _| {
                thread::Builder::new()
                    .name("mondrian-audio-render-panic-test".to_owned())
                    .spawn(move || {
                        drop(completion_tx);
                        panic!("injected render worker panic");
                    })
            })
            .expect("spawn injected render worker");

        let position = sample_position(0);
        let deadline = Instant::now() + Duration::from_secs(2);
        let failure = loop {
            let poll = playback
                .poll(AudioPlaybackMode::Consume, position)
                .expect("valid Audio Playback poll");
            if poll.events.iter().any(|event| {
                matches!(
                    event,
                    AudioPlaybackEvent::RenderWorkerStoppedUnexpectedly { .. }
                )
            }) {
                break poll;
            }
            assert!(
                Instant::now() < deadline,
                "render worker panic was not observed"
            );
            thread::yield_now();
        };

        assert_eq!(
            failure.snapshot.state,
            AudioPlaybackState::ExecutionUnavailable
        );
        assert_eq!(failure.snapshot.in_flight, 0);
        assert_eq!(playback.render_queue.state.lock().pending.len(), 0);
        let evidence = playback.shutdown_and_wait();
        assert_eq!(evidence.render_workers_started, 1);
        assert_eq!(evidence.render_workers_terminated, 1);
        assert_eq!(evidence.render_worker_panics, 1);
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn explicit_shutdown_reclaims_the_owned_render_worker() {
        let (output, _) = fake_output();
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");

        let evidence = playback.shutdown_and_wait();

        assert_eq!(evidence.schema_version, 3);
        assert_eq!(evidence.render_workers_started, 1);
        assert_eq!(evidence.render_workers_terminated, 1);
        assert_eq!(evidence.render_worker_panics, 0);
        assert_eq!(evidence.output.workers_started, 0);
        assert!(evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(!evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(evidence.all_workers_terminated());
    }

    #[test]
    fn dirty_output_receipt_propagates_to_top_level_owner_facts() {
        let (output, _) = fake_output_with_control(FakeOutputControl {
            shutdown_evidence: Some(RealtimeAudioOutputShutdownEvidence {
                worker_owner_abandonments: 1,
                ..RealtimeAudioOutputShutdownEvidence::no_worker_owner()
            }),
            ..FakeOutputControl::default()
        });
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");

        let evidence = playback.shutdown_and_wait();

        assert_eq!(evidence.output.worker_owner_abandonments, 1);
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn joined_render_worker_without_supervisor_stamp_fails_closed() {
        let (output, _) = fake_output();
        let playback = AudioPlayback::with_output_and_spawner(test_config(), output, |_, _, _| {
            thread::Builder::new().name("unstamped-render-worker".to_owned()).spawn(|| {})
        })
        .expect("spawn unstamped render worker");

        let evidence = playback.shutdown_and_wait();

        assert_eq!(evidence.render_workers_started, 1);
        assert_eq!(evidence.render_workers_terminated, 1);
        assert_eq!(evidence.render_worker_terminal_evidence_missing, 1);
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn begin_shutdown_only_sets_the_captured_output_signal() {
        let output_signal = Arc::new(AtomicBool::new(false));
        let foreign_shutdown_started = Arc::new(AtomicBool::new(false));
        let (output, _) = fake_output_with_control(FakeOutputControl {
            shutdown_signal: Some(Arc::clone(&output_signal)),
            shutdown_started: Some(Arc::clone(&foreign_shutdown_started)),
            ..FakeOutputControl::default()
        });
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");

        playback.begin_shutdown();

        assert!(output_signal.load(Ordering::Acquire));
        assert!(!foreign_shutdown_started.load(Ordering::Acquire));
        assert_eq!(
            playback.prepare(
                sample_position(0),
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            ),
            Err(AudioPlaybackError::ExecutionUnavailable)
        );
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).state,
            AudioPlaybackState::ExecutionUnavailable
        );
        let evidence = playback.shutdown_and_wait();
        assert!(foreign_shutdown_started.load(Ordering::Acquire));
        assert!(evidence.all_workers_terminated());
    }

    #[test]
    fn shutdown_evidence_rejects_default_stale_and_incomplete_receipts() {
        assert!(!AudioPlaybackShutdownEvidence::default().all_workers_terminated());
        let clean = AudioPlaybackShutdownEvidence::no_owner();
        assert!(clean.all_workers_terminated());
        assert!(
            !AudioPlaybackShutdownEvidence { schema_version: 2, ..clean }.all_workers_terminated()
        );
        assert!(!AudioPlaybackShutdownEvidence {
            shutdown_resource_facts_complete_at_deadline: false,
            ..clean
        }
        .all_workers_terminated());
        assert!(!AudioPlaybackShutdownEvidence {
            shutdown_coordinator_owner_abandonments: 1,
            ..clean
        }
        .all_workers_terminated());
    }

    #[test]
    fn deadline_shutdown_returns_exact_clean_coordinator_receipt() {
        let (output, _) = fake_output();
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");

        let evidence = playback.shutdown_until(Instant::now() + Duration::from_secs(2));

        assert_eq!(evidence.schema_version, 3);
        assert_eq!(evidence.shutdown_coordinators_started, 1);
        assert_eq!(evidence.shutdown_coordinators_terminated, 1);
        assert_eq!(evidence.shutdown_coordinator_start_failures, 0);
        assert_eq!(evidence.shutdown_coordinator_panics, 0);
        assert_eq!(evidence.shutdown_coordinator_timeouts, 0);
        assert_eq!(evidence.shutdown_coordinator_detachments, 0);
        assert_eq!(evidence.shutdown_coordinator_owner_abandonments, 0);
        assert!(evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(!evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(evidence.all_workers_terminated());
    }

    #[test]
    fn joined_completion_after_deadline_is_late_but_still_reclaimed() {
        let (output, _) = fake_output();
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let deadline = Instant::now() - Duration::from_secs(1);

        let evidence = playback.shutdown_until_with_spawner(deadline, |work| {
            let result = work();
            let carrier = thread::spawn(move || result);
            while !carrier.is_finished() {
                thread::yield_now();
            }
            Ok(carrier)
        });

        assert_eq!(evidence.shutdown_coordinators_started, 1);
        assert_eq!(evidence.shutdown_coordinators_terminated, 1);
        assert_eq!(evidence.shutdown_coordinator_timeouts, 1);
        assert_eq!(evidence.shutdown_coordinator_detachments, 0);
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn running_foreign_output_shutdown_times_out_and_detaches() {
        let shutdown_started = Arc::new(AtomicBool::new(false));
        let shutdown_release = Arc::new(AtomicBool::new(false));
        let drop_finished = Arc::new(AtomicBool::new(false));
        let (output, _) = fake_output_with_control(FakeOutputControl {
            shutdown_started: Some(Arc::clone(&shutdown_started)),
            shutdown_release: Some(Arc::clone(&shutdown_release)),
            drop_finished: Some(Arc::clone(&drop_finished)),
            ..FakeOutputControl::default()
        });
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");

        let evidence = playback.shutdown_until(Instant::now() + Duration::from_millis(20));

        let started_deadline = Instant::now() + Duration::from_secs(2);
        while !shutdown_started.load(Ordering::Acquire) && Instant::now() < started_deadline {
            thread::yield_now();
        }
        let observed_start = shutdown_started.load(Ordering::Acquire);
        shutdown_release.store(true, Ordering::Release);
        let drop_deadline = Instant::now() + Duration::from_secs(2);
        while !drop_finished.load(Ordering::Acquire) && Instant::now() < drop_deadline {
            thread::yield_now();
        }

        assert!(observed_start);
        assert_eq!(evidence.shutdown_coordinators_started, 1);
        assert_eq!(evidence.shutdown_coordinators_terminated, 0);
        assert_eq!(evidence.shutdown_coordinator_timeouts, 1);
        assert_eq!(evidence.shutdown_coordinator_detachments, 1);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_workers_terminated());
        assert!(drop_finished.load(Ordering::Acquire));
    }

    #[test]
    fn coordinator_spawn_error_abandons_foreign_owner_without_caller_drop() {
        let output_drop_started = Arc::new(AtomicBool::new(false));
        let output_drop_release = Arc::new(AtomicBool::new(false));
        let error_dropped = Arc::new(AtomicBool::new(false));
        let (output, _) = fake_output_with_control(FakeOutputControl {
            drop_started: Some(Arc::clone(&output_drop_started)),
            drop_release: Some(Arc::clone(&output_drop_release)),
            ..FakeOutputControl::default()
        });
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let retirement_terminal = Arc::clone(&playback.render_retirement_worker_terminal);
        let observed_error_drop = Arc::clone(&error_dropped);

        let evidence = playback
            .shutdown_until_with_spawner(Instant::now() + Duration::from_secs(2), move |_work| {
                Err(io::Error::other(DropProbe { dropped: observed_error_drop }))
            });

        assert_eq!(evidence.shutdown_coordinators_started, 0);
        assert_eq!(evidence.shutdown_coordinator_start_failures, 1);
        assert_eq!(evidence.shutdown_coordinator_detachments, 0);
        assert!(evidence.shutdown_coordinator_owner_abandonments >= 2);
        assert!(!error_dropped.load(Ordering::Acquire));
        assert!(!output_drop_started.load(Ordering::Acquire));
        assert!(!evidence.all_workers_terminated());
        let terminal_deadline = Instant::now() + Duration::from_secs(2);
        while retirement_terminal.load(Ordering::Acquire) == RENDER_WORKER_TERMINAL_UNKNOWN
            && Instant::now() < terminal_deadline
        {
            thread::yield_now();
        }
        assert_eq!(
            retirement_terminal.load(Ordering::Acquire),
            RENDER_WORKER_TERMINAL_NORMAL
        );
        output_drop_release.store(true, Ordering::Release);
    }

    #[test]
    fn coordinator_spawner_opaque_panic_never_drops_payload_or_foreign_owner() {
        let output_drop_started = Arc::new(AtomicBool::new(false));
        let output_drop_release = Arc::new(AtomicBool::new(false));
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let (output, _) = fake_output_with_control(FakeOutputControl {
            drop_started: Some(Arc::clone(&output_drop_started)),
            drop_release: Some(Arc::clone(&output_drop_release)),
            ..FakeOutputControl::default()
        });
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let observed_payload_drop = Arc::clone(&payload_dropped);

        let evidence = playback
            .shutdown_until_with_spawner(Instant::now() + Duration::from_secs(2), move |_work| {
                std::panic::panic_any(DropProbe { dropped: observed_payload_drop })
            });

        assert_eq!(evidence.shutdown_coordinator_start_failures, 1);
        assert_eq!(evidence.shutdown_coordinator_spawner_panics, 1);
        assert!(evidence.shutdown_coordinator_owner_abandonments >= 2);
        assert!(!payload_dropped.load(Ordering::Acquire));
        assert!(!output_drop_started.load(Ordering::Acquire));
        assert!(!evidence.all_workers_terminated());
        output_drop_release.store(true, Ordering::Release);
    }

    #[test]
    fn coordinator_join_opaque_panic_is_safe_and_retains_owner_abandonment() {
        let output_drop_started = Arc::new(AtomicBool::new(false));
        let output_drop_release = Arc::new(AtomicBool::new(false));
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let (output, _) = fake_output_with_control(FakeOutputControl {
            drop_started: Some(Arc::clone(&output_drop_started)),
            drop_release: Some(Arc::clone(&output_drop_release)),
            ..FakeOutputControl::default()
        });
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let observed_payload_drop = Arc::clone(&payload_dropped);

        let evidence = playback.shutdown_until_with_spawner(
            Instant::now() + Duration::from_secs(2),
            move |work| {
                Ok(thread::spawn(move || {
                    drop(work);
                    std::panic::panic_any(DropProbe { dropped: observed_payload_drop })
                }))
            },
        );

        assert_eq!(evidence.shutdown_coordinators_started, 1);
        assert_eq!(evidence.shutdown_coordinators_terminated, 1);
        assert_eq!(evidence.shutdown_coordinator_panics, 1);
        assert!(evidence.shutdown_coordinator_owner_abandonments >= 2);
        assert!(!payload_dropped.load(Ordering::Acquire));
        assert!(!output_drop_started.load(Ordering::Acquire));
        assert!(!evidence.all_workers_terminated());
        output_drop_release.store(true, Ordering::Release);
    }

    #[test]
    fn joined_coordinator_panic_after_deadline_is_also_timed_out() {
        let (output, _) = fake_output();
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");

        let evidence =
            playback.shutdown_until_with_spawner(Instant::now() - Duration::from_secs(1), |work| {
                let coordinator = thread::spawn(move || {
                    drop(work);
                    panic!("synthetic late coordinator panic")
                });
                while !coordinator.is_finished() {
                    thread::yield_now();
                }
                Ok(coordinator)
            });

        assert_eq!(evidence.shutdown_coordinators_started, 1);
        assert_eq!(evidence.shutdown_coordinators_terminated, 1);
        assert_eq!(evidence.shutdown_coordinator_panics, 1);
        assert_eq!(evidence.shutdown_coordinator_timeouts, 1);
        assert_eq!(evidence.shutdown_coordinator_detachments, 0);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn opaque_foreign_shutdown_panic_is_caught_and_abandoned() {
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let (output, _) = fake_output_with_control(FakeOutputControl {
            shutdown_panic_payload_dropped: Some(Arc::clone(&payload_dropped)),
            ..FakeOutputControl::default()
        });
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");

        let evidence = playback.shutdown_until(Instant::now() + Duration::from_secs(2));

        assert_eq!(evidence.foreign_owner_panics, 1);
        assert!(evidence.foreign_owner_abandonments >= 2);
        assert!(!payload_dropped.load(Ordering::Acquire));
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn opaque_render_worker_panic_payload_is_never_dropped_by_shutdown_caller() {
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let observed_payload_drop = Arc::clone(&payload_dropped);
        let (output, _) = fake_output();
        let playback =
            AudioPlayback::with_output_and_spawner(test_config(), output, move |_, _, _| {
                thread::Builder::new()
                    .name("mondrian-audio-render-opaque-panic-test".to_owned())
                    .spawn(move || {
                        std::panic::panic_any(DropProbe { dropped: observed_payload_drop });
                    })
            })
            .expect("spawn injected render worker");

        let evidence = playback.shutdown_and_wait();

        assert_eq!(evidence.render_worker_panics, 1);
        assert_eq!(evidence.render_worker_owner_abandonments, 1);
        assert!(!payload_dropped.load(Ordering::Acquire));
        assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
        assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn ordinary_drop_hands_blocking_foreign_output_to_detached_owner() {
        let drop_started = Arc::new(AtomicBool::new(false));
        let drop_release = Arc::new(AtomicBool::new(false));
        let drop_finished = Arc::new(AtomicBool::new(false));
        let (output, _) = fake_output_with_control(FakeOutputControl {
            drop_started: Some(Arc::clone(&drop_started)),
            drop_release: Some(Arc::clone(&drop_release)),
            drop_finished: Some(Arc::clone(&drop_finished)),
            ..FakeOutputControl::default()
        });
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let (drop_complete_tx, drop_complete_rx) = mpsc::channel();

        let dropper = thread::spawn(move || {
            drop(playback);
            drop_complete_tx.send(()).expect("publish ordinary drop completion");
        });
        let returned = drop_complete_rx.recv_timeout(Duration::from_secs(1)).is_ok();
        drop_release.store(true, Ordering::Release);
        dropper.join().expect("ordinary Audio Playback drop returns");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !drop_finished.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }

        assert!(
            returned,
            "ordinary Drop blocked on foreign output destruction"
        );
        assert!(drop_started.load(Ordering::Acquire));
        assert!(drop_finished.load(Ordering::Acquire));
    }

    #[test]
    fn ordinary_owner_handoff_spawn_error_abandons_owner_and_foreign_error() {
        let owner_dropped = Arc::new(AtomicBool::new(false));
        let error_dropped = Arc::new(AtomicBool::new(false));
        let observed_error_drop = Arc::clone(&error_dropped);

        let evidence = handoff_ordinary_audio_owner_with_spawner(
            DropProbe { dropped: Arc::clone(&owner_dropped) },
            move |_work| Err(io::Error::other(DropProbe { dropped: observed_error_drop })),
        );

        assert_eq!(evidence.start_failures, 1);
        assert_eq!(evidence.spawner_panics, 0);
        assert_eq!(evidence.owner_abandonments, 2);
        assert!(!owner_dropped.load(Ordering::Acquire));
        assert!(!error_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn ordinary_owner_handoff_opaque_spawner_panic_is_safe() {
        let owner_dropped = Arc::new(AtomicBool::new(false));
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let observed_payload_drop = Arc::clone(&payload_dropped);

        let evidence = handoff_ordinary_audio_owner_with_spawner(
            DropProbe { dropped: Arc::clone(&owner_dropped) },
            move |_work| std::panic::panic_any(DropProbe { dropped: observed_payload_drop }),
        );

        assert_eq!(evidence.start_failures, 1);
        assert_eq!(evidence.spawner_panics, 1);
        assert_eq!(evidence.owner_abandonments, 2);
        assert!(!owner_dropped.load(Ordering::Acquire));
        assert!(!payload_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn render_worker_spawn_error_hands_foreign_output_off_caller() {
        let drop_started = Arc::new(AtomicBool::new(false));
        let drop_release = Arc::new(AtomicBool::new(false));
        let drop_finished = Arc::new(AtomicBool::new(false));
        let (output, _) = fake_output_with_control(FakeOutputControl {
            drop_started: Some(Arc::clone(&drop_started)),
            drop_release: Some(Arc::clone(&drop_release)),
            drop_finished: Some(Arc::clone(&drop_finished)),
            ..FakeOutputControl::default()
        });

        let result = AudioPlayback::with_output_and_spawner(test_config(), output, |_, _, _| {
            Err(io::Error::other("synthetic render-worker spawn failure"))
        });

        drop_release.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !drop_finished.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }

        assert!(matches!(
            result,
            Err(AudioPlaybackCreateError::RenderWorkerSpawn(_))
        ));
        assert!(drop_started.load(Ordering::Acquire));
        assert!(drop_finished.load(Ordering::Acquire));
    }

    #[test]
    fn ordinary_drop_detaches_a_render_worker_that_has_not_finished() {
        let (output, _) = fake_output();
        let release = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let exited = Arc::new(AtomicBool::new(false));
        let worker_exited = Arc::clone(&exited);
        let playback = AudioPlayback::with_output_and_spawner(
            test_config(),
            output,
            move |render_queue, _, terminal| {
                thread::Builder::new().name("mondrian-audio-render-drop-test".to_owned()).spawn(
                    move || {
                        supervise_render_worker(&terminal, || {
                            assert!(render_queue.pop().is_none());
                            while !worker_release.load(Ordering::Acquire) {
                                thread::yield_now();
                            }
                            worker_exited.store(true, Ordering::Release);
                        });
                    },
                )
            },
        )
        .expect("spawn injected render worker");

        let (drop_complete_tx, drop_complete_rx) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(playback);
            drop_complete_tx.send(()).expect("publish drop completion");
        });
        let returned_without_worker_exit =
            drop_complete_rx.recv_timeout(Duration::from_secs(1)).is_ok();
        release.store(true, Ordering::Release);
        dropper.join().expect("ordinary Audio Playback drop must not panic");

        let deadline = Instant::now() + Duration::from_secs(2);
        while !exited.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            returned_without_worker_exit,
            "ordinary drop blocked on the render worker"
        );
        assert!(exited.load(Ordering::Acquire));
    }

    #[test]
    fn detached_production_render_worker_abandons_opaque_panic_inside_worker_runtime() {
        let (output, _) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let terminal = Arc::clone(&playback.render_worker_terminal);
        let entered = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicBool::new(false));
        let payload_dropped = Arc::new(AtomicBool::new(false));
        playback
            .prepare(
                sample_position(0),
                Arc::new(DetachedPanicRenderer {
                    entered: Arc::clone(&entered),
                    released: Arc::clone(&released),
                    payload_dropped: Arc::clone(&payload_dropped),
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("prepare detached-panic renderer");
        playback
            .poll(AudioPlaybackMode::Consume, sample_position(0))
            .expect("admit detached-panic work");

        let deadline = Instant::now() + Duration::from_secs(2);
        while !entered.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(entered.load(Ordering::Acquire));
        let (drop_complete_tx, drop_complete_rx) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(playback);
            drop_complete_tx.send(()).expect("publish playback drop completion");
        });
        let returned = drop_complete_rx.recv_timeout(Duration::from_secs(1)).is_ok();
        released.store(true, Ordering::Release);
        dropper.join().expect("ordinary Audio Playback drop returns");

        let deadline = Instant::now() + Duration::from_secs(2);
        while terminal.load(AtomicOrdering::Acquire)
            != RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        assert_eq!(
            terminal.load(AtomicOrdering::Acquire),
            RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
        );
        assert!(
            returned,
            "ordinary Audio Playback drop blocked on the render worker"
        );
        assert!(!payload_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn sample_anchor_validation_rejects_wrong_rate_and_negative_positions() {
        let (output, _) = fake_output();
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let wrong_rate = AudioSamplePosition::new(
            0,
            AudioSampleRate::new(48_000).expect("alternate sample rate"),
        );

        assert!(matches!(
            playback.validate_anchor(wrong_rate),
            Err(AudioPlaybackError::InvalidSampleAnchor(
                AudioTimeError::RateMismatch { .. }
            ))
        ));
        assert_eq!(
            playback.validate_anchor(sample_position(-1)),
            Err(AudioPlaybackError::NegativeSampleAnchor)
        );
    }

    #[test]
    fn invalid_prepare_anchor_changes_no_generation_renderer_or_output_state() {
        let (output, state) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let before = playback.snapshot(AudioPlaybackMode::Idle);

        let error = playback
            .prepare(
                AudioSamplePosition::new(
                    1,
                    AudioSampleRate::new(48_000).expect("alternate sample rate"),
                ),
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect_err("wrong sample rate must fail before commit");

        assert!(matches!(
            error,
            AudioPlaybackError::InvalidSampleAnchor(AudioTimeError::RateMismatch { .. })
        ));
        assert_eq!(playback.snapshot(AudioPlaybackMode::Idle), before);
        assert!(playback.renderer.is_none());
        assert_eq!(state.lock().queued_frames, 0);
    }

    #[test]
    fn poll_sample_cursor_overflow_is_rejected_before_consuming_output_or_queue_state() {
        let (output, state) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let position = sample_position(i64::MAX - 20);
        playback
            .prepare(
                position,
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("single anchor is representable");
        let before = playback.snapshot(AudioPlaybackMode::Consume);
        let (events_before, output_before, queued_before) = {
            let state = state.lock();
            (state.events.clone(), state.snapshot, state.queued_frames)
        };
        let pending_before = playback.render_queue.state.lock().pending.len();

        let result = playback.poll(AudioPlaybackMode::Consume, position);

        assert_eq!(result, Err(AudioPlaybackError::CoordinateOverflow));
        assert_eq!(playback.snapshot(AudioPlaybackMode::Consume), before);
        assert_eq!(
            playback.render_queue.state.lock().pending.len(),
            pending_before
        );
        let state = state.lock();
        assert_eq!(state.events, events_before);
        assert_eq!(state.snapshot, output_before);
        assert_eq!(state.queued_frames, queued_before);
    }

    #[test]
    fn poll_generation_overflow_is_rejected_before_consuming_output_or_queue_state() {
        let (output, state) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        playback.renderer = Some(PreparedAudioPcmRenderer {
            owner: Arc::new(RecordingRenderer {
                requests: Arc::new(Mutex::new(Vec::new())),
                wrong_frame_count: false,
            }),
            continuity_model: AudioPcmContinuityModel::IndependentWindows,
        });
        playback.generation = u64::MAX - 1;
        let position = sample_position(0);
        let before = playback.snapshot(AudioPlaybackMode::Consume);
        let (events_before, output_before, queued_before) = {
            let state = state.lock();
            (state.events.clone(), state.snapshot, state.queued_frames)
        };
        let pending_before = playback.render_queue.state.lock().pending.len();

        let result = playback.poll(AudioPlaybackMode::Consume, position);

        assert_eq!(result, Err(AudioPlaybackError::CoordinateOverflow));
        assert_eq!(playback.snapshot(AudioPlaybackMode::Consume), before);
        assert_eq!(
            playback.render_queue.state.lock().pending.len(),
            pending_before
        );
        let state = state.lock();
        assert_eq!(state.events, events_before);
        assert_eq!(state.snapshot, output_before);
        assert_eq!(state.queued_frames, queued_before);
    }

    #[test]
    fn isolated_underrun_preserves_master_but_sustained_missing_frames_reprime() {
        let (output, state) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        playback
            .prepare(
                sample_position(0),
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("valid audio anchor");
        poll_until_settled(&mut playback, sample_position(0));

        state.lock().snapshot.as_mut().expect("fake output").underrun_frames = 4;
        let isolated = playback
            .poll(AudioPlaybackMode::Consume, sample_position(0))
            .expect("valid Audio Playback poll");
        assert_eq!(isolated.snapshot.state, AudioPlaybackState::Active);
        assert_eq!(isolated.snapshot.underrun_recovery_count, 0);
        assert!(
            isolated.events.contains(&AudioPlaybackEvent::UnderrunObserved {
                stream_generation: 4,
                delta_frames: 4,
                interval_total_frames: 4,
            })
        );

        state.lock().snapshot.as_mut().expect("fake output").underrun_frames = 10;
        let recovering = playback
            .poll(AudioPlaybackMode::Consume, sample_position(40))
            .expect("valid Audio Playback poll");
        assert_eq!(recovering.snapshot.state, AudioPlaybackState::Recovering);
        assert_eq!(recovering.snapshot.underrun_recovery_count, 1);
        assert!(recovering.events.iter().any(|event| matches!(
            event,
            AudioPlaybackEvent::UnderrunRecoveryStarted {
                stream_generation: 4,
                missing_frames: 10,
                threshold_frames: 10,
                final_output,
                final_media_anchor,
            } if final_output.active
                && final_output.underrun_frames == 10
                && *final_media_anchor == sample_position(0)
        )));
        assert!(recovering.snapshot.output.is_some_and(|output| !output.active));

        poll_until_settled(&mut playback, sample_position(40));
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).state,
            AudioPlaybackState::Active
        );
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).active_interval_underrun_frames,
            0
        );
    }

    #[test]
    fn new_generation_waits_for_callback_ack_then_clears_racing_old_pcm() {
        let (output, state) = fake_output();
        {
            let mut output = state.lock();
            output.events.clear();
            output.auto_confirm_quiescence = false;
            output.snapshot.as_mut().expect("fake output").active = true;
        }
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        playback
            .prepare(
                sample_position(0),
                Arc::new(RecordingRenderer {
                    requests: Arc::clone(&requests),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("valid audio anchor");
        {
            let mut output = state.lock();
            assert_eq!(output.quiescence_revision, 1);
            // Models PCM made visible by an old callback/producer race after
            // the first defensive clear but before quiescence acknowledgement.
            output.queued_frames = 7;
            output.snapshot.as_mut().expect("fake output").buffered_frames = 7;
        }

        let waiting = playback
            .poll(AudioPlaybackMode::Preroll, sample_position(0))
            .expect("quiescence wait is not an error");
        assert!(!playback.output_generation_ready);
        assert_eq!(waiting.snapshot.in_flight, 0);
        assert_eq!(state.lock().queued_frames, 7);
        assert!(requests.lock().is_empty());

        state.lock().confirmed_quiescence_revision = 1;
        poll_until_settled(&mut playback, sample_position(0));

        assert!(playback.output_generation_ready);
        assert_eq!(state.lock().queued_frames, 30);
        assert_eq!(
            requests.lock().iter().map(|request| request.start_sample).collect::<Vec<_>>(),
            vec![0, 10, 20]
        );
    }

    #[test]
    fn newer_device_retirement_revision_waits_for_frozen_loss_event() {
        let (output, state) = fake_output();
        {
            let mut output = state.lock();
            output.events.clear();
            output.snapshot.as_mut().expect("fake output").active = true;
        }
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        playback
            .prepare(
                sample_position(0),
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("prepare current generation");
        assert_eq!(
            playback.quiescence_token.map(|token| token.revision),
            Some(1)
        );

        let final_snapshot = {
            let mut output = state.lock();
            // Models the device worker's deactivation becoming visible before
            // CPAL stream destruction can publish its frozen Lost event.
            output.quiescence_revision = 2;
            output.confirmed_quiescence_revision = 2;
            output.snapshot.expect("concrete output")
        };
        let waiting = playback
            .poll(AudioPlaybackMode::Preroll, sample_position(0))
            .expect("superseded quiescence waits for lifecycle evidence");
        assert!(!playback.output_generation_ready);
        assert_eq!(waiting.snapshot.in_flight, 0);

        {
            let mut output = state.lock();
            output.snapshot = None;
            output.events.push_back(RealtimeAudioOutputEvent::Lost {
                reason: RealtimeAudioOutputLossReason::ControlledRecycle,
                final_snapshot,
            });
        }
        let lost = playback
            .poll(AudioPlaybackMode::Preroll, sample_position(0))
            .expect("consume frozen device-loss evidence");

        assert!(playback.quiescence_token.is_none());
        assert!(lost.events.iter().any(|event| matches!(
            event,
            AudioPlaybackEvent::DeviceLost {
                reason: RealtimeAudioOutputLossReason::ControlledRecycle,
                ..
            }
        )));
    }

    #[test]
    fn hidden_preroll_executes_stateful_history_and_trims_exact_elapsed_prefix() {
        let (output, state) = fake_output();
        state.lock().events.clear();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        playback
            .prepare(
                sample_position(0),
                Arc::new(StatefulRecordingRenderer { requests: Arc::clone(&requests) }),
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("valid audio anchor");
        poll_until_settled_in_mode(
            &mut playback,
            sample_position(0),
            AudioPlaybackMode::Preroll,
        );

        playback.reprime(sample_position(0)).expect("restart exact hidden interval");
        requests.lock().clear();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let snapshot = playback
                .poll(AudioPlaybackMode::Consume, sample_position(5))
                .expect("exact hidden-preroll catch-up")
                .snapshot;
            if snapshot.in_flight == 0 && snapshot.output.is_some_and(|output| output.active) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        let snapshot = playback.snapshot(AudioPlaybackMode::Consume);
        assert_eq!(snapshot.media_anchor, Some(sample_position(5)));
        assert!(snapshot.output.is_some_and(|output| output.active));
        assert_eq!(state.lock().queued_frames, 25);
        let requests = requests.lock();
        assert_eq!(
            requests.iter().map(|request| request.start_sample).collect::<Vec<_>>(),
            vec![0, 10, 20]
        );
        let generation = AudioPcmRenderGeneration::new(snapshot.generation);
        assert_eq!(
            requests.iter().map(|request| request.continuity).collect::<Vec<_>>(),
            vec![
                AudioPcmContinuity::Enter(generation),
                AudioPcmContinuity::Continue(generation),
                AudioPcmContinuity::Continue(generation),
            ]
        );
    }

    #[test]
    fn hidden_preroll_catch_up_uses_physical_capacity_and_negative_delta_waits() {
        let (output, _) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        playback.generation_render_anchor = Some(sample_position(0));

        let exact_boundary = playback
            .validate_poll_arithmetic(sample_position(1_980))
            .expect("skip plus preroll exactly fits two-second queue");
        assert_eq!(exact_boundary.elapsed_skip_frames, Some(1_980));
        assert_eq!(exact_boundary.admission_target_frames, 2_000);
        assert_eq!(
            playback.validate_poll_arithmetic(sample_position(1_981)),
            Err(AudioPlaybackError::HiddenPrerollExceedsOutputCapacity {
                skip_frames: 1_981,
                preroll_frames: 20,
                capacity_frames: 2_000,
            })
        );

        playback.generation_render_anchor = Some(sample_position(10));
        let waiting = playback
            .validate_poll_arithmetic(sample_position(5))
            .expect("authority before hidden interval waits without unsigned wrap");
        assert_eq!(waiting.elapsed_skip_frames, None);
    }

    #[test]
    fn output_rate_and_layout_mismatch_fail_before_playback_mutation() {
        let (output, state) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let generation = playback.generation;

        state.lock().snapshot.as_mut().expect("fake output").contract.sample_rate = 48_000;
        assert_eq!(
            playback.prepare(
                sample_position(0),
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            ),
            Err(AudioPlaybackError::OutputSampleRateMismatch { expected: 1_000, actual: 48_000 })
        );
        assert_eq!(playback.generation, generation);
        assert!(playback.renderer.is_none());

        state.lock().snapshot.as_mut().expect("fake output").contract.sample_rate = 1_000;
        state.lock().snapshot.as_mut().expect("fake output").contract.channel_layout =
            AudioChannelLayout::Mono;
        assert_eq!(
            playback.validate_anchor(sample_position(0)),
            Err(AudioPlaybackError::OutputChannelLayoutMismatch {
                expected: AudioChannelLayout::Stereo,
                actual: AudioChannelLayout::Mono,
            })
        );
        assert_eq!(playback.generation, generation);
        assert_eq!(state.lock().queued_frames, 0);
    }

    #[test]
    fn opened_device_evidence_must_match_the_observed_stream_contract() {
        let (output, state) = fake_output();
        let observed = state.lock().snapshot.expect("fake output").contract;
        let mut selected = observed;
        selected.sample_rate = 48_000;
        state.lock().events = VecDeque::from([RealtimeAudioOutputEvent::Opened {
            stream_generation: 4,
            evidence: device_evidence(selected),
        }]);
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let generation = playback.generation;

        assert_eq!(
            playback.poll(AudioPlaybackMode::Preroll, sample_position(0)),
            Err(AudioPlaybackError::OutputNegotiationEvidenceMismatch { selected, observed })
        );
        assert_eq!(playback.generation, generation);
        assert!(playback.latest_output_device_evidence().is_none());
    }

    #[test]
    fn upstream_2997_subframe_sample_anchor_is_preserved_without_relowering() {
        let (output, state) = fake_output();
        {
            let mut output = state.lock();
            output.events.clear();
            output.snapshot.as_mut().expect("fake output").contract.sample_rate = 48_000;
        }
        let mut config = test_config();
        config.sample_rate = 48_000;
        let mut playback =
            AudioPlayback::with_output(config, output).expect("spawn test render worker");
        // 1,602 @ 48 kHz is the upstream exact/rounded sample result for a
        // 30000/1001-frame boundary; Media must preserve it verbatim.
        let anchor = AudioSamplePosition::new(
            1_602,
            AudioSampleRate::new(48_000).expect("test sample rate"),
        );
        playback
            .prepare(
                anchor,
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("exact sample anchor");

        assert_eq!(playback.generation_render_anchor, Some(anchor));
        assert_eq!(playback.next_start_sample, 1_602);
    }

    #[test]
    fn lifecycle_aggregate_retains_controlled_loss_anchor_across_async_reprime_and_reopen() {
        let (output, state) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        playback
            .prepare(
                sample_position(0),
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("valid audio anchor");
        poll_until_settled(&mut playback, sample_position(0));

        let mut final_output = state.lock().snapshot.expect("active fake output");
        final_output.active = false;
        state.lock().events.push_back(RealtimeAudioOutputEvent::Lost {
            reason: RealtimeAudioOutputLossReason::ControlledRecycle,
            final_snapshot: final_output,
        });
        playback
            .reprime(sample_position(40))
            .expect("new intent may race queued loss observation");
        state.lock().snapshot = None;

        let lost = playback
            .poll(AudioPlaybackMode::Consume, sample_position(40))
            .expect("consume frozen loss evidence");
        assert!(lost.events.iter().any(|event| matches!(
            event,
            AudioPlaybackEvent::DeviceLost {
                reason: RealtimeAudioOutputLossReason::ControlledRecycle,
                final_media_anchor: Some(anchor),
                ..
            } if *anchor == sample_position(0)
        )));
        assert_eq!(lost.snapshot.output_lifecycle.opened_count, 1);
        assert_eq!(lost.snapshot.output_lifecycle.lost_count, 1);
        assert_eq!(lost.snapshot.output_lifecycle.controlled_recycle_count, 1);
        assert_eq!(lost.snapshot.output_lifecycle.backend_loss_count, 0);
        assert_eq!(lost.snapshot.output_lifecycle.last_lost_generation, Some(4));
        assert_eq!(
            lost.snapshot.output_lifecycle.last_loss,
            Some(AudioOutputLossSnapshot {
                reason: RealtimeAudioOutputLossReason::ControlledRecycle,
                final_output,
                final_media_anchor: Some(sample_position(0)),
            })
        );

        let mut reopened = final_output;
        reopened.stream_generation = 5;
        reopened.buffered_frames = 0;
        state.lock().snapshot = Some(reopened);
        state.lock().queued_frames = 0;
        state.lock().events.push_back(RealtimeAudioOutputEvent::Opened {
            stream_generation: 5,
            evidence: device_evidence(reopened.contract),
        });
        let reopened = playback
            .poll(AudioPlaybackMode::Preroll, sample_position(40))
            .expect("install reopened output");
        assert_eq!(reopened.snapshot.output_lifecycle.opened_count, 2);
        assert_eq!(reopened.snapshot.output_lifecycle.lost_count, 1);
        assert_eq!(
            reopened.snapshot.output_lifecycle.last_opened_generation,
            Some(5)
        );
        assert_ne!(
            reopened.snapshot.output_lifecycle.last_opened_generation,
            reopened.snapshot.output_lifecycle.last_lost_generation
        );
    }

    #[cfg(feature = "validation")]
    #[test]
    fn public_controlled_recycle_seam_rejects_stale_generation_before_dispatch() {
        let (output, state) = fake_output();
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");

        assert_eq!(
            playback.request_controlled_output_recycle(3),
            Err(AudioPlaybackValidationError::StreamGenerationMismatch { expected: 3, actual: 4 })
        );
        assert!(state.lock().controlled_recycle_requests.is_empty());
        playback
            .request_controlled_output_recycle(4)
            .expect("dispatch exact current stream generation");
        assert_eq!(state.lock().controlled_recycle_requests, vec![4]);
    }

    #[test]
    fn reprime_discards_old_generation_completion_before_output() {
        let (output, state) = fake_output();
        let mut config = test_config();
        config.preroll_frames = 10;
        let mut playback =
            AudioPlayback::with_output(config, output).expect("spawn test render worker");
        let entered = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicBool::new(false));
        let canceled = Arc::new(AtomicBool::new(false));
        playback
            .prepare(
                sample_position(0),
                Arc::new(GateRenderer {
                    entered: Arc::clone(&entered),
                    released: Arc::clone(&released),
                    canceled: Arc::clone(&canceled),
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("valid audio anchor");
        playback
            .poll(AudioPlaybackMode::Consume, sample_position(0))
            .expect("valid Audio Playback poll");
        let entered_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < entered_deadline {
            if entered.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(entered.load(Ordering::Acquire));

        let current_requests = Arc::new(Mutex::new(Vec::new()));
        playback
            .prepare(
                sample_position(40),
                Arc::new(RecordingRenderer {
                    requests: Arc::clone(&current_requests),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("valid audio anchor");
        let cancellation_deadline = Instant::now() + Duration::from_millis(50);
        while Instant::now() < cancellation_deadline && !canceled.load(Ordering::Acquire) {
            thread::yield_now();
        }
        released.store(true, Ordering::Release);
        assert!(canceled.load(Ordering::Acquire));

        let settled_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < settled_deadline {
            let snapshot = playback
                .poll(AudioPlaybackMode::Consume, sample_position(40))
                .expect("valid Audio Playback poll")
                .snapshot;
            if snapshot.stale_completion_count == 1
                && snapshot.in_flight == 0
                && snapshot.output.is_some_and(|output| output.buffered_frames == 30)
            {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        let snapshot = playback.snapshot(AudioPlaybackMode::Consume);
        assert_eq!(snapshot.stale_completion_count, 1);
        assert_eq!(snapshot.canceled_render_count, 2);
        assert_eq!(state.lock().queued_frames, 30);
        let current_requests = current_requests.lock();
        assert_eq!(
            current_requests.iter().map(|request| request.start_sample).collect::<Vec<_>>(),
            vec![40, 50, 60]
        );
        let generation = AudioPcmRenderGeneration::new(snapshot.generation);
        assert_eq!(
            current_requests.iter().map(|request| request.continuity).collect::<Vec<_>>(),
            vec![
                AudioPcmContinuity::Enter(generation),
                AudioPcmContinuity::Continue(generation),
                AudioPcmContinuity::Continue(generation),
            ]
        );
    }
}
