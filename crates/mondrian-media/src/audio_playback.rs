//! Deep realtime Audio Playback scheduling Module.

mod native_author;
mod native_child_settlement;
use native_child_settlement::ForeignOwnerRetirementTracker;
mod source_close;
mod state_entry;
pub use native_author::{
    AudioPcmNativeAuthorHandoff, AudioPcmNativeAuthorHandoffContinuity,
    AudioPcmNativeAuthorHandoffIntent, AudioPcmNativeAuthorHandoffOutcome,
    AudioPcmNativeAuthorHandoffRefusal, AudioPcmNativeAuthorHandoffStatus,
};
pub use source_close::{AudioPcmSourceCloseStatus, AudioPcmSourceCloseTicket};
use state_entry::AuthorStateEntryPhase;

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
#[cfg(target_os = "windows")]
use mondrian_audio_scheduling::RealtimeAudioThreadRole;
use mondrian_audio_scheduling::{
    AudioThreadSchedulingError, RealtimeAudioThreadEvidence, RealtimeAudioThreadExitEvidence,
    RealtimeAudioThreadLease,
};
use mondrian_core::{
    AudioChannelLayout, AudioOwnerInstallationClaim, AudioOwnerInstallationError,
    AudioOwnerInstallationSource, AudioOwnerInstallationVerifier, AudioSamplePosition,
    AudioSampleRate, AudioTimeError, ExecutionCancellationToken,
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
// One FIFO and serial worker; PCM cannot consume the reserved control frontier.
const MAX_PENDING_NATIVE_EDITOR_WORK: usize = 2;
const MAX_PENDING_NATIVE_WORK: usize = MAX_PENDING_NATIVE_EDITOR_WORK + 3;
const RENDER_WORKER_TERMINAL_SCHEDULING_UNCONFIRMED: u8 = 4;
/// Explicit producer stack reservation; native/CRT/kernel allocations are separate.
pub const AUDIO_PLAYBACK_PRODUCER_STACK_BYTES: usize = 2 * 1024 * 1024;

/// Native registration and restoration facts from the actual PCM producer.
/// These facts do not prove a render deadline, preroll or physical delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlaybackProducerScheduling {
    /// No native registration was admitted by this execution owner.
    Unregistered,
    /// This platform currently uses ordinary scheduling; no native RT admission is claimed.
    OrdinaryPlatform {
        /// Actual compile-time target OS, not a guessed native task identity.
        platform: &'static str,
    },
    /// The actual producer entered its dedicated physical scheduling task.
    Active(RealtimeAudioThreadEvidence),
    /// The exact producer registration was explicitly reverted on its owner thread.
    Restored(RealtimeAudioThreadExitEvidence),
    /// Entry or restoration failed; no qualified ordinary-state completion is claimed.
    Refused(AudioThreadSchedulingError),
}

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
    /// The actual producer refused native scheduling before any PCM render work.
    #[error("Audio Playback producer scheduling admission failed: {0}")]
    ProducerScheduling(#[source] AudioThreadSchedulingError),
    /// The bounded foreign-render-owner retirement worker could not be created.
    #[error("failed to spawn Audio Playback render-owner retirement worker: {0}")]
    RenderRetirementWorkerSpawn(#[source] io::Error),
    /// The fixed complete-PCM retention owner could not be admitted/allocated.
    #[error(transparent)]
    RetainedPcm(#[from] crate::AudioRetainedPcmError),
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// The exact retained native attachment requires control-only cleanup.
    /// A normal seek/continuity entry cannot replace its still-retryable owner.
    #[error("native editor attachment cleanup is pending")]
    NativeEditorCleanupPending,
    /// A source-close admission blocks other execution/control entry.
    #[error("Audio Playback source-close admission is pending")]
    SourceClosePending,
    /// This ticket no longer describes the actual pending source-close operation.
    #[error("Audio Playback source-close ticket is superseded")]
    SourceCloseSuperseded,
    /// The exact source-close query has not succeeded. Its original error is retained.
    #[error("Audio Playback source-close admission is not closed")]
    SourceCloseNotClosed,
    /// A metadata-only renderer installation callback panicked before the swap.
    #[error("Audio Playback owner registration callback panicked")]
    OwnerRegistrationPanicked,
    /// The exact renderer registration or installation generation was refused.
    #[error(transparent)]
    OwnerInstallation(#[from] AudioOwnerInstallationError),
    /// The supplied sample position belongs to the wrong rate or overflowed.
    #[error("invalid Audio Playback sample anchor: {0}")]
    InvalidSampleAnchor(#[from] AudioTimeError),
    /// Realtime transport does not admit a negative sample anchor.
    #[error("Audio Playback sample anchor must be non-negative")]
    NegativeSampleAnchor,
    /// This target renderer admits a fresh generation only at its own origin.
    #[error("Audio Playback fresh entry requires {required:?}, got {actual:?}")]
    FreshEntryOriginMismatch {
        required: AudioSamplePosition,
        actual: AudioSamplePosition,
    },
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
    /// Generation-owned history whose fresh Enter requires this exact origin.
    /// Contiguous Continue windows may advance beyond it within the generation.
    GenerationStateFromOrigin(AudioSamplePosition),
}

impl AudioPcmContinuityModel {
    fn validate_fresh_entry(self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        if let Self::GenerationStateFromOrigin(required) = self
            && anchor != required
        {
            return Err(AudioPlaybackError::FreshEntryOriginMismatch { required, actual: anchor });
        }
        Ok(())
    }
}

/// Scheduler action after a stateful render generation is invalidated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRenderRecoveryDisposition {
    /// A fresh generation will enter at the authoritative Playback position.
    Reprime,
    /// Entry is unavailable or the retry budget is exhausted; explicit entry is required.
    Blocked,
}

/// Complete PCM or a retained native lifecycle request, with no string authority.
#[derive(Debug)]
pub enum AudioPcmRenderOutcome {
    /// Complete exact-layout PCM accepted under its admitted contract.
    Rendered(AudioBuffer),
    /// Legal native pending request; no PCM was consumed or partially published.
    DeferredForReprepare(mondrian_core::NativeAudioReprepareNotice),
    /// No PCM was processed because this exact installed editor needs frame cleanup.
    DeferredForEditorCleanup(mondrian_core::NativeAudioEditorTarget),
    /// No DSP/sample advancement occurred while the exact retained author batch awaits Plan/Ack.
    DeferredForNativeAuthor(Box<mondrian_core::NativeAudioEditorTarget>),
}

/// Outcome of native binding work on the already-installed PCM renderer.
/// It never changes the DSP graph, author state, transport or Undo history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPcmNativeOwnerBindingStatus {
    /// Current actual Runtime published its exact same-child binding records.
    Ready,
    /// DSP remains installed, but native editor ownership is unavailable.
    Unavailable(AudioPcmNativeOwnerBindingRefusal),
}

/// Typed refusal of editor ownership without classifying valid PCM as failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPcmNativeOwnerBindingRefusal {
    /// Renderer does not implement canonical native owner binding.
    UnsupportedAdapter,
    /// The installed root or exact object changed before publication.
    Superseded,
    /// The same render-generation cancellation closed this work.
    Cancelled,
    /// Actual native binding preparation failed; no prepared record is visible.
    ProcessorRejected,
    /// The one serial worker cannot accept the control work.
    WorkerUnavailable,
}

/// Typed diagnostic-control refusal; it never invalidates an otherwise valid PCM graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioNativeObservationPollRefusal {
    /// The actual render-generation cancellation closed this work before polling.
    Cancelled,
    /// The actual format adapter rejected or failed its observation operation.
    ProcessorRejected,
    /// Current slot installation source could not provide coherent metadata.
    InstallationUnavailable,
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
    /// Observe this complete renderer's native child ownership domain.
    ///
    /// Success means every owned execution path either forwards to its actual
    /// child owner or is explicitly known to own no native child. This aggregate
    /// result is not a per-child registration or a DSP/Source admission grant.
    /// Missing forwarding makes shutdown evidence unavailable without changing
    /// the renderer's existing execution permission.
    fn bind_native_child_settlement(
        &self,
        _registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        Err(mondrian_core::NativeChildSettlementError::Unavailable)
    }

    /// Read-only native diagnostics capability captured before trait erasure.
    /// This does not perform a native poll or create editor/author permission.
    fn native_observations_supported(&self) -> bool {
        false
    }

    /// Drain one bounded actual source on the same serial render/control worker.
    /// The returned immutable completion never substitutes, resets or resumes PCM.
    fn poll_native_observations(
        &self,
        _cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<Option<mondrian_core::NativeAudioObservationBatch>> {
        Ok(None)
    }

    /// Validate the actual native author source on the original serial worker.
    /// This must not drain callbacks, acknowledge a delivery or change DSP state.
    /// Success is a current observation, not an atomic seal against later callbacks.
    fn validate_native_author_source_closed(
        &self,
        _cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<()> {
        Err(source_close::refusal(
            "renderer does not support native author source-closed validation",
        ))
    }

    /// Read only the actual currently owned Runtime's registration.
    /// Same-Arc Runtime replacement must return its fresh exact verifier; no
    /// cached previous registration or current-owner fallback is permitted.
    fn owner_installation_verifier(&self) -> Option<AudioOwnerInstallationVerifier> {
        None
    }

    /// Bind one read-only registration to this exact Runtime before slot install.
    /// This metadata seam performs no native calls or object lifecycle work.
    /// Returning false declares no native owner consumer; unrelated replacement
    /// of an already-bound Runtime's verifier must be refused.
    fn bind_owner_installation(
        &self,
        _verifier: AudioOwnerInstallationVerifier,
    ) -> Result<bool, AudioOwnerInstallationError> {
        Ok(false)
    }

    /// Prepare and publish exact native bindings on the sole serial render worker.
    /// A failed/unsupported binding leaves the valid DSP graph installed and
    /// provides no editor lease. Each record checks the actual child/object and
    /// current claim again before publication; queued metadata is not authority.
    fn install_native_owner_bindings(
        &self,
        _claim: &AudioOwnerInstallationClaim,
        _cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<AudioPcmNativeOwnerBindingStatus> {
        Ok(AudioPcmNativeOwnerBindingStatus::Unavailable(
            AudioPcmNativeOwnerBindingRefusal::UnsupportedAdapter,
        ))
    }

    /// Execute a closed editor intent on the existing serial render worker.
    /// Address facts are not authority: the actual current Runtime must compare
    /// its installed opaque binding and actual native object before SDK work.
    fn control_native_editor(
        &self,
        _intent: mondrian_core::NativeAudioEditorControlIntent,
        _cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<mondrian_core::NativeAudioEditorControlOutcome> {
        Ok(mondrian_core::NativeAudioEditorControlOutcome::Refused(
            mondrian_core::NativeAudioEditorControlRefusal::UnsupportedAdapter,
        ))
    }

    /// Render one exact timeline-media window.
    fn render(
        &self,
        request: AudioPcmRenderRequest,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<AudioPcmRenderOutcome>;
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
    /// Native child is retained pending canonical nonRT reprepare installation.
    RepreparePending,
    /// The exact installed editor needs control-only host-frame cleanup.
    EditorCleanupPending,
    /// Cleanup completed, but explicit Play/seek must start fresh continuity.
    EditorCleanupCompletePaused,
    /// Same Session/PCM generation is waiting for exact native author Plan/Ack.
    NativeAuthorPending,
    /// Fresh entry is unavailable or generation failures exhausted the retry policy.
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
    /// A legal native request deferred the next root window; no render fault occurred.
    NativeReprepareRequired {
        /// Generation admitted before the pending boundary.
        previous_generation: u64,
        /// Quiesced replacement generation, blocked pending canonical installation.
        pending_generation: u64,
        /// Exact deferred window boundary.
        start_sample: i64,
        /// Canonical anchor retained for subsequent explicit owner installation.
        restart_anchor: AudioSamplePosition,
        /// Final physical output observation before quiescence.
        final_output: Option<RealtimeAudioOutputSnapshot>,
        /// Anchor paired with the final physical output observation.
        final_media_anchor: Option<AudioSamplePosition>,
        /// Actual typed native request; diagnostic identity is not mutation authority.
        notice: mondrian_core::NativeAudioReprepareNotice,
    },
    /// The retained pending source's exact output deactivation was acknowledged.
    /// This event provides final callback evidence, never permission to resume PCM.
    NativeReprepareOutputQuiesced {
        /// Still blocked render generation whose output was retired.
        pending_generation: u64,
        /// Coherent snapshot after exact software callback quiescence, if available.
        final_output: Option<RealtimeAudioOutputSnapshot>,
        /// Old activation media anchor paired with that snapshot.
        final_media_anchor: Option<AudioSamplePosition>,
    },
    /// Non-RT native binding completed for the current installed Source.
    /// This evidence never grants author permission or resumes transport.
    NativeOwnerBindingsObserved {
        /// Current renderer materialization.
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        /// Work generation when binding was scheduled; may precede current PCM after suspension.
        generation: u64,
        /// Separate editor readiness; valid PCM has not been rolled back.
        status: AudioPcmNativeOwnerBindingStatus,
    },
    /// Actual bounded diagnostic completion for the current renderer generation.
    /// No message grants author permission or changes the DSP lifecycle.
    NativeDiagnosticsObserved {
        /// Exact current renderer materialization.
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        /// Media generation admitted on the one serial worker.
        generation: u64,
        /// Authenticated source and exact retained bytes, moved without another store.
        batch: Box<mondrian_core::NativeAudioDiagnosticBatch>,
    },
    /// Same actual view target transition, admitted on the single completion.
    /// This metadata refresh never calls SDK code or opens a rebuilt view.
    NativeEditorAttachmentObserved {
        /// Exact current renderer admitted by Media.
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        /// Actual Media work generation.
        generation: u64,
        /// Private-owner observation, not a permission capability.
        observation: Box<mondrian_core::NativeAudioEditorAttachmentObservation>,
    },
    /// Same-owner transient parameter facts from the one observation completion.
    NativeParameterObservationsObserved {
        /// Exact current renderer materialization admitted by Media.
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        /// Actual current Media work generation.
        generation: u64,
        /// Exact installed target and native incarnation; no author writes.
        observation: Box<mondrian_core::NativeAudioParameterObservation>,
    },
    /// Complete ordered author source facts on the same observation completion.
    /// This is not author permission: the actual Hosted record retains this
    /// exact shared proposal until Session/Plan installation/native Ack accept it.
    /// Next exact PCM window was not entered; existing complete PCM remains owned.
    NativeAuthorPending {
        /// Actual retained editor address at the author barrier.
        target: mondrian_core::NativeAudioEditorTarget,
        /// First window rejected before DSP, in the existing scheduler coordinate.
        start_sample: i64,
        /// Media generation retained without a new Enter.
        generation: u64,
    },
    NativeAuthorProposalObserved {
        /// Actual renderer admitted by the current private installation claim.
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        /// Actual Media work generation; no author generation is inferred.
        generation: u64,
        /// Exact retained actual-source payload, not a reconstructed callback.
        proposal: Box<mondrian_core::NativeAudioAuthorProposal>,
    },
    /// Output retirement required by an actual retained-tree state-entry policy.
    NativeAuthorStateEntryRequired {
        previous_generation: u64,
        pending_generation: u64,
        final_output: Option<RealtimeAudioOutputSnapshot>,
        final_media_anchor: Option<AudioSamplePosition>,
    },
    /// Actual output token and original serial PCM work are retired.
    NativeAuthorStateEntryOutputQuiesced {
        pending_generation: u64,
        final_output: Option<RealtimeAudioOutputSnapshot>,
        final_media_anchor: Option<AudioSamplePosition>,
    },
    /// Native observation control could not complete; PCM validity is independent.
    NativeObservationPollRefused {
        /// Current renderer materialization.
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        /// Current Media generation.
        generation: u64,
        /// Explicit cancellation or adapter refusal, never a render fault.
        reason: AudioNativeObservationPollRefusal,
    },
    /// Same-source author Plan plus native acknowledgement settled on the serial worker.
    NativeAuthorHandoffObserved {
        /// Actual source renderer materialization.
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        /// Exact Media generation admitted at completion.
        generation: u64,
        /// Shared exact retained proposal, never a reconstructed public revision.
        proposal: Box<mondrian_core::NativeAudioAuthorProposal>,
        /// Historical install/ack result and current-owner admission.
        status: AudioPcmNativeAuthorHandoffStatus,
    },
    /// Actual native editor control evidence from the same serial worker.
    /// Open requires the current install; Close also admits the retained actual
    /// attachment through Runtime. Neither authorizes callbacks or history.
    NativeEditorObserved {
        /// Actual current renderer materialization.
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        /// Same Media work generation.
        generation: u64,
        /// Exact submitted address and closed operation.
        intent: mondrian_core::NativeAudioEditorControlIntent,
        /// Actual operation evidence or an explicit refusal.
        outcome: mondrian_core::NativeAudioEditorControlOutcome,
    },
    /// Root preflight retained the exact instance before any next-window DSP.
    NativeEditorCleanupRequired {
        /// Media generation before the hold.
        previous_generation: u64,
        /// Exact retained control generation.
        pending_generation: u64,
        /// First unprocessed sample of the deferred window.
        start_sample: i64,
        /// Current exact transport anchor; no new author time is invented.
        restart_anchor: AudioSamplePosition,
        /// Last physical callback observation before output deactivation.
        final_output: Option<RealtimeAudioOutputSnapshot>,
        /// Media anchor paired with the physical observation.
        final_media_anchor: Option<AudioSamplePosition>,
        /// Read-only exact target admitted by the sole installed Runtime.
        target: mondrian_core::NativeAudioEditorTarget,
    },
    /// Original PCM work settled and the exact output token is quiescent.
    NativeEditorCleanupOutputQuiesced {
        /// Same retained generation.
        pending_generation: u64,
        /// Exact current target, not a new installed-owner grant.
        target: mondrian_core::NativeAudioEditorTarget,
        /// Final callback observation after quiescence.
        final_output: Option<RealtimeAudioOutputSnapshot>,
        /// Old activation anchor paired with that observation.
        final_media_anchor: Option<AudioSamplePosition>,
    },
    /// Same-object frame cleanup and output finalization completed as paused.
    NativeEditorCleanupReadyPaused {
        /// Current held generation, which never processed new PCM.
        pending_generation: u64,
        /// Exact target whose actual native frame was cleaned.
        target: mondrian_core::NativeAudioEditorTarget,
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

enum RenderWork {
    Pcm(PcmRenderWork),
    NativeEditor(Box<NativeEditorWork>),
    NativeObservations(NativeObservationWork),
    NativeAuthorHandoff(Box<native_author::NativeAuthorHandoffWork>),
    NativeOwnerBindings(NativeOwnerBindingWork),
}

struct NativeEditorWork {
    renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    renderer: Arc<dyn AudioPcmRenderer>,
    claim: AudioOwnerInstallationClaim,
    cancellation: ExecutionCancellationToken,
    intent: mondrian_core::NativeAudioEditorControlIntent,
}

enum NativeObservationOperation {
    Poll,
    SourceClosed(AudioPcmSourceCloseTicket),
}

struct NativeObservationWork {
    generation: u64,
    renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    renderer: Arc<dyn AudioPcmRenderer>,
    cancellation: ExecutionCancellationToken,
    installation_generation: Option<std::num::NonZeroU64>,
    operation: NativeObservationOperation,
}

struct NativeOwnerBindingWork {
    generation: u64,
    renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    renderer: Arc<dyn AudioPcmRenderer>,
    claim: AudioOwnerInstallationClaim,
    verifier: AudioOwnerInstallationVerifier,
    cancellation: ExecutionCancellationToken,
}

struct PcmRenderWork {
    generation: u64,
    renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    request: AudioPcmRenderRequest,
    // Origin stays with the prepared Source; work carries only failure policy.
    allow_window_silence: bool,
    renderer: Arc<dyn AudioPcmRenderer>,
    cancellation: ExecutionCancellationToken,
}

enum RenderCompletion {
    NativeAuthorHandoff(Box<native_author::NativeAuthorHandoffCompletion>),
    Pcm(PcmRenderCompletion),
    NativeEditor(Box<NativeEditorCompletion>),
    NativeObservations {
        generation: u64,
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        installation_generation: Option<std::num::NonZeroU64>,
        result: NativeObservationCompletion,
    },
    NativeOwnerBindings {
        generation: u64,
        renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
        installation_generation: std::num::NonZeroU64,
        status: AudioPcmNativeOwnerBindingStatus,
    },
}

struct NativeEditorCompletion {
    renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    cancellation: ExecutionCancellationToken,
    installed_generation: std::num::NonZeroU64,
    intent: mondrian_core::NativeAudioEditorControlIntent,
    outcome: mondrian_core::NativeAudioEditorControlOutcome,
}

enum NativeObservationCompletion {
    SourceClosed(mondrian_core::Result<()>),
    Observed(Option<Box<mondrian_core::NativeAudioObservationBatch>>),
    Refused(AudioNativeObservationPollRefusal),
}

struct PcmRenderCompletion {
    generation: u64,
    renderer_id: mondrian_core::AudioPcmRendererMaterializationId,
    request: AudioPcmRenderRequest,
    allow_window_silence: bool,
    result: RenderCompletionResult,
}

enum RenderCompletionResult {
    Rendered(AudioBuffer),
    DeferredForNativeAuthor(Box<mondrian_core::NativeAudioEditorTarget>),
    DeferredForReprepare(mondrian_core::NativeAudioReprepareNotice),
    DeferredForEditorCleanup(mondrian_core::NativeAudioEditorTarget),
    Failed(String),
}

#[derive(Clone)]
struct PreparedAudioPcmRenderer {
    materialization_id: mondrian_core::AudioPcmRendererMaterializationId,
    owner: Arc<dyn AudioPcmRenderer>,
    continuity_model: AudioPcmContinuityModel,
    native_owner_binding_supported: bool,
    native_observations_supported: bool,
}

struct RenderQueueState {
    pending: VecDeque<RenderWork>,
    stopped: bool,
}

struct RenderWorkQueue {
    state: Mutex<RenderQueueState>,
    #[cfg(test)]
    completion_publications: std::sync::atomic::AtomicUsize,
    scheduling: Mutex<AudioPlaybackProducerScheduling>,
    wake: Condvar,
    pcm_capacity: usize,
    retirement_queue: Arc<RenderOwnerRetirementQueue>,
    retirement_tracker: Arc<ForeignOwnerRetirementTracker>,
}

impl RenderWorkQueue {
    fn new(
        pcm_capacity: usize,
        retirement_queue: Arc<RenderOwnerRetirementQueue>,
        retirement_tracker: Arc<ForeignOwnerRetirementTracker>,
    ) -> Self {
        Self {
            state: Mutex::new(RenderQueueState {
                pending: VecDeque::with_capacity(pcm_capacity + MAX_PENDING_NATIVE_WORK),
                stopped: false,
            }),
            #[cfg(test)]
            completion_publications: std::sync::atomic::AtomicUsize::new(0),
            wake: Condvar::new(),
            scheduling: Mutex::new(AudioPlaybackProducerScheduling::Unregistered),
            pcm_capacity,
            retirement_queue,
            retirement_tracker,
        }
    }

    fn push(&self, work: RenderWork) -> Result<(), RenderWork> {
        let mut state = self.state.lock();
        let limit = match &work {
            RenderWork::Pcm(_) => self.pcm_capacity,
            RenderWork::NativeEditor(_) => MAX_PENDING_NATIVE_EDITOR_WORK,
            RenderWork::NativeObservations(_)
            | RenderWork::NativeAuthorHandoff(_)
            | RenderWork::NativeOwnerBindings(_) => 1,
        };
        let kind = std::mem::discriminant(&work);
        if state.stopped
            || state
                .pending
                .iter()
                .filter(|queued| std::mem::discriminant(*queued) == kind)
                .count()
                >= limit
        {
            return Err(work);
        }
        debug_assert!(state.pending.len() < state.pending.capacity());
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
        std::mem::replace(
            &mut state.pending,
            VecDeque::with_capacity(self.pcm_capacity + MAX_PENDING_NATIVE_WORK),
        )
    }

    fn take_pending_preserving_native_controls(
        &self,
        renderer_id: Option<mondrian_core::AudioPcmRendererMaterializationId>,
        installed_generation: Option<std::num::NonZeroU64>,
        policy: PcmRetirementPolicy,
    ) -> VecDeque<RenderWork> {
        let mut state = self.state.lock();
        let count = state.pending.len();
        // A bounded cold-control retirement owner, not a PCM sample allocation.
        // Rotate each original entry once so kept controls retain FIFO order.
        let mut retired = VecDeque::with_capacity(count);
        for _ in 0..count {
            let Some(work) = state.pending.pop_front() else {
                break;
            };
            let editor = matches!(&work,RenderWork::NativeEditor(work)
                if Some(work.renderer_id)==renderer_id
                    && Some(work.claim.generation())==installed_generation);
            // Suspension retires PCM, not the current installation's controls.
            // Existing native holds retain their original editor-only filter.
            let suspended_control = matches!(policy, PcmRetirementPolicy::Suspend)
                && match &work {
                    RenderWork::NativeOwnerBindings(work) => {
                        Some(work.renderer_id) == renderer_id
                            && Some(work.claim.generation()) == installed_generation
                    }
                    RenderWork::NativeAuthorHandoff(work) => {
                        Some(work.renderer_id) == renderer_id
                            && Some(work.expected.generation()) == installed_generation
                    }
                    _ => false,
                };
            if editor || suspended_control {
                state.pending.push_back(work);
            } else {
                retired.push_back(work);
            }
        }
        retired
    }

    fn stop(&self) -> VecDeque<RenderWork> {
        let mut state = self.state.lock();
        state.stopped = true;
        let pending = std::mem::take(&mut state.pending);
        self.wake.notify_all();
        pending
    }
}

/// Owned platform boundary for one realtime PCM output lifecycle.
///
/// Audio Playback is the sole render-generation and queue admission authority.
/// The adapter owns its concrete stream, bounded queue and callback revisions.
/// A deactivation token identifies a software callback obligation: it does not
/// prove that samples already submitted to a DAC have physically drained.
pub trait AudioOutputAdapter: Send {
    /// Optional shared cancellation flag set before owned output shutdown.
    fn shutdown_signal(&self) -> Option<Arc<AtomicBool>> {
        None
    }

    /// Poll at most one concrete lifecycle transition without blocking.
    fn poll(&mut self) -> Option<RealtimeAudioOutputEvent>;
    /// Admit one complete PCM buffer atomically, or reject without partial admission.
    fn enqueue(&mut self, buffer: &AudioBuffer) -> Result<(), RealtimeAudioOutputEnqueueError>;
    /// Discard queued PCM safely with respect to concurrent callback queue access.
    /// This does not acknowledge callbacks that already own an active output block.
    fn clear(&mut self);
    /// Check that deactivation can issue a fresh revision before changing state.
    fn validate_deactivation(&self) -> Result<(), RealtimeAudioOutputControlError>;
    /// Prevent new active callback entry and return its exact outstanding obligation.
    /// Already running callbacks may finish; absent output returns `None`.
    fn deactivate(
        &self,
    ) -> Result<Option<RealtimeAudioOutputQuiescenceToken>, RealtimeAudioOutputControlError>;
    /// Verify this exact stream/revision has no active callbacks still using old PCM.
    /// Reject foreign or superseded tokens; do not infer acknowledgement from inactivity.
    fn is_quiescent(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
    ) -> Result<bool, RealtimeAudioOutputControlError>;
    /// After exact acknowledgement, discard the checked queued prefix and activate.
    /// Failure must not partially discard or activate.
    fn activate_after_discard(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
        frames: usize,
    ) -> Result<(), RealtimeAudioOutputControlError>;
    /// Complete queued PCM frames, excluding frames already submitted to the device.
    fn buffered_frames(&self) -> usize;
    /// Fixed logical queue capacity in complete frames, when output exists.
    fn capacity_frames(&self) -> Option<usize>;
    /// Coherent current stream/callback evidence, with an explicit capture time.
    fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot>;

    /// Close every owned output worker and return actual software lifetime evidence.
    /// The default is valid only for adapters that own no worker.
    fn shutdown_and_wait(&mut self) -> RealtimeAudioOutputShutdownEvidence {
        RealtimeAudioOutputShutdownEvidence::no_worker_owner()
    }

    /// Request device intent; return whether this adapter accepts that intent.
    fn set_device_selection(&self, _selection: RealtimeAudioOutputDeviceSelection) -> bool {
        false
    }

    /// Current explicit device intent, independently of transient open failures.
    fn device_selection(&self) -> RealtimeAudioOutputDeviceSelection {
        RealtimeAudioOutputDeviceSelection::SystemDefault
    }
    /// Request validation-only retirement of the exact current stream generation.
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

    fn clear(&mut self) {
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

    fn device_selection(&self) -> RealtimeAudioOutputDeviceSelection {
        RealtimeAudioOutputManager::device_selection(self)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeControlHold {
    AuthorStateEntry {
        target: mondrian_core::NativeAudioEditorTarget,
        phase: AuthorStateEntryPhase,
    },
    AuthorPending {
        target: mondrian_core::NativeAudioEditorTarget,
        first_unrendered_sample: Option<i64>,
    },
    Reprepare(mondrian_core::NativeAudioReprepareNotice),
    EditorCleanupPending(mondrian_core::NativeAudioEditorTarget),
    EditorDetachedAwaitingQuiescence(mondrian_core::NativeAudioEditorTarget),
    EditorCleanupCompletePaused(mondrian_core::NativeAudioEditorTarget),
}

impl NativeControlHold {
    fn cleanup_target(self) -> Option<mondrian_core::NativeAudioEditorTarget> {
        match self {
            Self::Reprepare(_) | Self::AuthorPending { .. } | Self::AuthorStateEntry { .. } => None,
            Self::EditorCleanupPending(target)
            | Self::EditorDetachedAwaitingQuiescence(target)
            | Self::EditorCleanupCompletePaused(target) => Some(target),
        }
    }
}

#[derive(Clone, Copy)]
struct NativeControlOutputRetirement {
    generation: u64,
    token: Option<RealtimeAudioOutputQuiescenceToken>,
    stream_generation: Option<u64>,
    media_anchor: Option<AudioSamplePosition>,
    acknowledged: bool,
}

/// Deep Module owning realtime output, render worker, generations, watermarks, and preroll.
pub struct AudioPlayback {
    pcm_journal: crate::pcm_journal::PcmJournal,
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
    // Only this actual Media slot owner creates claims. Runtime consumers get
    // read-only registrations; candidates will use this same serial worker.
    owner_installation: Arc<AudioOwnerInstallationSource>,
    native_owner_binding_status: Option<AudioPcmNativeOwnerBindingStatus>,
    // Observed metadata admits exact same-object Close after author rebindings.
    // Runtime still validates the actual native attachment on the serial worker.
    native_editor_attachment: Option<(
        mondrian_core::AudioPcmRendererMaterializationId,
        mondrian_core::NativeAudioEditorTarget,
    )>,
    source_close_admission: Option<source_close::SourceCloseAdmission>,
    native_diagnostic_in_flight: Option<(u64, mondrian_core::AudioPcmRendererMaterializationId)>,
    native_author_handoff_in_flight:
        Option<(u64, mondrian_core::AudioPcmRendererMaterializationId)>,
    native_diagnostic_next_poll: Option<Instant>,
    native_author_state_entry_request: Option<Box<native_author::NativeAuthorHandoffWork>>,
    native_diagnostic_discarded_records: u64,
    generation: u64,
    generation_cancellation: ExecutionCancellationToken,
    // PCM scheduling can rotate while this same registered native owner is
    // retained. Actual slot/claim replacement and shutdown cancel its controls.
    native_control_cancellation: ExecutionCancellationToken,
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
    native_control_hold: Option<NativeControlHold>,
    native_control_output_retirement: Option<NativeControlOutputRetirement>,
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
    activation_authority: Option<AudioSamplePosition>,
    chunk_frames: i64,
    elapsed_skip_frames: Option<usize>,
    admission_target_frames: usize,
    external_output_retirement_pending: bool,
}

#[derive(Clone, Copy)]
enum PcmRetirementPolicy {
    FreshContinuity,
    /// Retire PCM without admitting Enter or replacing the installed owner.
    Suspend,
    BeginNativeHold,
    PreserveNativeHold,
}

enum CompletionAdmission {
    Accepted,
    SubstitutedWithSilence(String),
    InvalidateGeneration(String),
    DeferForReprepare(mondrian_core::NativeAudioReprepareNotice),
    DeferForEditorCleanup(mondrian_core::NativeAudioEditorTarget),
    DeferForNativeAuthor(mondrian_core::NativeAudioEditorTarget),
}

impl AudioPlayback {
    /// Maximum PCM frames requested by this owner's validated scheduler policy.
    pub const fn render_chunk_frames(&self) -> usize {
        self.config.chunk_frames
    }

    /// Read actual producer entry/exit facts without implying deadline qualification.
    /// This ordinary control read is outside the device and DSP callback paths.
    pub fn producer_scheduling(&self) -> AudioPlaybackProducerScheduling {
        *self.render_queue.scheduling.lock()
    }

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

    /// Construct the canonical serial render owner with an owned platform output adapter.
    ///
    /// The adapter must implement the exact queue and callback token contract in
    /// [`AudioOutputAdapter`]. This is the same constructor used by the native
    /// device constructors; renderer admission and lifetime ownership remain here.
    pub fn with_output(
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
        let pcm_journal = match crate::pcm_journal::PcmJournal::for_playback_config(config) {
            Ok(journal) => journal,
            Err(error) => {
                handoff_audio_output_owner(output);
                return Err(error.into());
            }
        };
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
                // This adapter's closed Copy enum has no foreign destructor;
                // drop its io wrapper normally rather than leaking known state.
                let scheduling = error
                    .get_ref()
                    .and_then(|source| source.downcast_ref::<AudioThreadSchedulingError>())
                    .copied();
                let (kind, error_owner_abandoned) = if scheduling.is_some() {
                    let kind = error.kind();
                    drop(error);
                    (kind, false)
                } else {
                    abandon_io_error(error)
                };
                if error_owner_abandoned {
                    tracing::error!("Audio Playback render-worker spawn error owner was abandoned");
                }
                render_retirement_queue.stop();
                join_initial_retirement_worker(render_retirement_worker);
                handoff_audio_output_owner(output);
                return Err(match scheduling {
                    Some(error) => AudioPlaybackCreateError::ProducerScheduling(error),
                    None => AudioPlaybackCreateError::RenderWorkerSpawn(io::Error::from(kind)),
                });
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
            pcm_journal,
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
            shutdown_pending_render_work: VecDeque::with_capacity(
                config.max_in_flight + MAX_PENDING_NATIVE_WORK,
            ),
            render_execution_unavailable: false,
            shutdown_requested: false,
            renderer: None,
            owner_installation: Arc::new(AudioOwnerInstallationSource::new()),
            native_owner_binding_status: None,
            native_editor_attachment: None,
            source_close_admission: None,
            native_diagnostic_in_flight: None,
            native_author_handoff_in_flight: None,
            native_diagnostic_next_poll: None,
            native_author_state_entry_request: None,
            native_diagnostic_discarded_records: 0,
            generation: 1,
            generation_cancellation: ExecutionCancellationToken::new(),
            native_control_cancellation: ExecutionCancellationToken::new(),
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
            native_control_hold: None,
            native_control_output_retirement: None,
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
        self.retire_pending_render_completions();
        self.flush_shutdown_pending_render_work();
        if let Some(renderer) = self.renderer.take() {
            self.handoff_retired_render_owners(VecDeque::new(), Some(renderer.owner));
        }
        self.stop_render_retirement_worker();
        let native_children_complete = self.foreign_owner_retirements.seal();
        let retirement = self.foreign_owner_retirements.snapshot();
        let mut foreign_owner_panics = retirement.panics;
        let mut foreign_owner_abandonments = retirement.owner_abandonments;
        let mut resource_facts_complete = true;
        let mut owner_lifetime_unresolved = false;
        if !native_children_complete
            || retirement.panics > 0
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
        self.retire_source_close_admission();
        self.shutdown_requested = true;
        if let Err(error) = self.owner_installation.revoke() {
            tracing::error!(%error, "native editor owner unavailable during audio shutdown");
        }
        self.generation_cancellation.cancel();
        self.native_control_cancellation.cancel();
        let mut pending = self.render_queue.stop();
        self.shutdown_pending_render_work.append(&mut pending);
        if let Some(work) = self.native_author_state_entry_request.take() {
            self.shutdown_pending_render_work
                .push_back(RenderWork::NativeAuthorHandoff(work));
        }
        if let Some(signal) = &self.output_shutdown_signal {
            signal.store(true, AtomicOrdering::Release);
        }
    }

    fn retire_pending_render_completions(&mut self) {
        // Stop serializes with final publication. Joined shutdown has no
        // producer; ordinary Drop admits no later successful publication.
        // Move one carrier at a time through the existing combined 256 cap.
        while let Ok(completion) = self.completion_rx.try_recv() {
            retire_render_completion(
                completion,
                &self.render_retirement_queue,
                &self.foreign_owner_retirements,
            );
        }
    }

    fn flush_shutdown_pending_render_work(&mut self) {
        let pending = std::mem::take(&mut self.shutdown_pending_render_work);
        self.handoff_retired_render_owners(pending, None);
    }

    fn handoff_retired_render_owners(
        &mut self,
        pending: VecDeque<RenderWork>,
        renderer: Option<Arc<dyn AudioPcmRenderer>>,
    ) {
        for work in &pending {
            if let RenderWork::NativeAuthorHandoff(work) = work
                && self.native_author_handoff_in_flight == Some((work.generation, work.renderer_id))
            {
                self.native_author_handoff_in_flight = None;
            }
            if let RenderWork::NativeObservations(work) = work
                && self.native_diagnostic_in_flight == Some((work.generation, work.renderer_id))
            {
                self.native_diagnostic_in_flight = None;
            }
        }
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
        self.observe_native_child_settlement(&renderer);
        if let Err(error) = self
            .validate_reprime_anchor_pure(anchor)
            .and_then(|()| self.ensure_source_close_idle())
            .and_then(|()| self.ensure_render_execution_available())
            .and_then(|()| self.validate_reprime_output())
        {
            self.handoff_retired_render_owners(VecDeque::new(), Some(renderer));
            return Err(error);
        }
        // Same-source transport changes do not replace an in-progress native
        // state entry. Its actual registered materialization and claim remain
        // authoritative until the exact installation/Ack completes.
        if self.owner_installation.current_claim()?.is_some()
            && self.renderer.as_ref().is_some_and(|current| {
                Arc::ptr_eq(&current.owner, &renderer)
                    && current.continuity_model == continuity_model
            })
        {
            return self.reprime(anchor);
        }
        if let Err(error) = continuity_model.validate_fresh_entry(anchor) {
            self.handoff_retired_render_owners(VecDeque::new(), Some(renderer));
            return Err(error);
        }
        let (supported, diagnostics_supported) =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let diagnostics_supported = renderer.native_observations_supported();
                if let Some(verifier) = renderer.owner_installation_verifier() {
                    if !self.owner_installation.owns_registration(&verifier.registration()) {
                        return Err(AudioOwnerInstallationError::WrongRegistration);
                    }
                    Ok((true, diagnostics_supported))
                } else {
                    let (_, verifier) = self.owner_installation.register()?;
                    renderer
                        .bind_owner_installation(verifier)
                        .map(|supported| (supported, diagnostics_supported))
                }
            })) {
                Ok(Ok(supported)) => supported,
                Ok(Err(error)) => {
                    self.handoff_retired_render_owners(VecDeque::new(), Some(renderer));
                    return Err(error.into());
                }
                Err(payload) => {
                    dispose_canonical_or_abandon_opaque_panic_payload(payload);
                    self.handoff_retired_render_owners(VecDeque::new(), Some(renderer));
                    return Err(AudioPlaybackError::OwnerRegistrationPanicked);
                }
            };
        self.reprime_prevalidated(
            anchor,
            false,
            Some(PreparedAudioPcmRenderer {
                materialization_id: mondrian_core::AudioPcmRendererMaterializationId::new(),
                owner: renderer,
                continuity_model,
                native_owner_binding_supported: supported,
                native_observations_supported: diagnostics_supported,
            }),
            PcmRetirementPolicy::FreshContinuity,
        )?;
        self.native_control_hold = None;
        self.native_control_output_retirement = None;
        self.render_blocked = false;
        Ok(())
    }

    /// Number of already-drained records rejected after renderer/generation supersession.
    /// This explicit delivery loss is distinct from the native producer's queue loss.
    pub const fn native_diagnostic_discarded_records(&self) -> u64 {
        self.native_diagnostic_discarded_records
    }

    fn schedule_native_diagnostics(&mut self, events: &mut Vec<AudioPlaybackEvent>) {
        if self.render_execution_unavailable
            || self.shutdown_requested
            || self.source_close_admission.is_some()
            || self.native_diagnostic_in_flight.is_some()
        {
            return;
        }
        let now = Instant::now();
        if self.native_diagnostic_next_poll.is_some_and(|next| now < next) {
            return;
        }
        let Some(renderer) =
            self.renderer.as_ref().filter(|renderer| renderer.native_observations_supported)
        else {
            return;
        };
        let identity = (self.generation, renderer.materialization_id);
        let installation_generation = match self.owner_installation.current_claim() {
            Ok(claim) => claim.map(|claim| claim.generation()),
            Err(_) => {
                self.native_diagnostic_next_poll = now.checked_add(Duration::from_millis(250));
                events.push(AudioPlaybackEvent::NativeObservationPollRefused {
                    renderer_id: identity.1,
                    generation: identity.0,
                    reason: AudioNativeObservationPollRefusal::InstallationUnavailable,
                });
                return;
            }
        };
        let work = RenderWork::NativeObservations(NativeObservationWork {
            generation: identity.0,
            renderer_id: identity.1,
            renderer: Arc::clone(&renderer.owner),
            cancellation: self.generation_cancellation.clone(),
            installation_generation,
            operation: NativeObservationOperation::Poll,
        });
        match self.render_queue.push(work) {
            Ok(()) => {
                self.native_diagnostic_in_flight = Some(identity);
                self.native_diagnostic_next_poll = now.checked_add(Duration::from_millis(250));
            }
            Err(work) => self.handoff_retired_render_owners(VecDeque::from([work]), None),
        }
    }

    /// Compare the actual installed owner, without calling the renderer or
    /// granting native/author permission. Aliases retain the same identity.
    pub fn matches_source(&self, renderer: &Arc<dyn AudioPcmRenderer>) -> bool {
        self.renderer
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.owner, renderer))
    }

    /// Pure current-slot availability; no native or author permission is issued.
    pub fn native_editor_source_available(&self) -> bool {
        self.renderer.is_some()
    }

    /// Queue one closed editor intent through the existing bounded render work.
    /// This performs no foreign Runtime getter or DSP mutex wait on the caller;
    /// current metadata only admits queuing, never native or author permission.
    pub fn request_native_audio_editor(
        &mut self,
        intent: mondrian_core::NativeAudioEditorControlIntent,
    ) -> Result<mondrian_core::NativeAudioEditorControlSubmission, AudioPlaybackError> {
        use mondrian_core::{
            NativeAudioEditorControlRefusal as Refusal,
            NativeAudioEditorControlSubmission as Submission,
        };
        self.ensure_source_close_idle()?;
        self.ensure_render_execution_available()?;
        if self.native_editor_cleanup_target().is_some()
            && matches!(
                intent,
                mondrian_core::NativeAudioEditorControlIntent::Open { .. }
            )
        {
            return Ok(Submission::Refused(Refusal::ConfigurationPending));
        }
        let Some(renderer) = self.renderer.as_ref() else {
            return Ok(Submission::Refused(Refusal::NotInstalled));
        };
        let Some(claim) = self.owner_installation.current_claim()? else {
            return Ok(Submission::Refused(Refusal::NotInstalled));
        };
        if let Some(target) = intent.expected_target()
            && claim.generation() != target.installed_generation
        {
            let retained_attachment = matches!(
                intent,
                mondrian_core::NativeAudioEditorControlIntent::Close { .. }
            ) && self.native_editor_attachment
                == Some((renderer.materialization_id, target));
            if !retained_attachment {
                return Ok(Submission::Refused(Refusal::OwnerChanged));
            }
        }
        let work = RenderWork::NativeEditor(Box::new(NativeEditorWork {
            renderer_id: renderer.materialization_id,
            renderer: Arc::clone(&renderer.owner),
            claim,
            cancellation: self.native_control_cancellation.clone(),
            intent,
        }));
        if let Err(work) = self.render_queue.push(work) {
            self.handoff_retired_render_owners(VecDeque::from([work]), None);
            return Ok(Submission::Refused(Refusal::WorkerUnavailable));
        }
        Ok(Submission::Queued)
    }

    /// Latest editor binding outcome for the current installed Source.
    /// Absence means unsupported or queued, never permission for an author edit.
    pub const fn native_owner_binding_status(&self) -> Option<AudioPcmNativeOwnerBindingStatus> {
        self.native_owner_binding_status
    }

    /// Remove timeline PCM rendering and invalidate all outstanding work.
    pub fn clear_source(&mut self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        self.ensure_source_close_idle()?;
        self.validate_reprime_anchor_pure(anchor)?;
        self.ensure_render_execution_available()?;
        self.validate_reprime_output()?;
        self.reprime_prevalidated(anchor, false, None, PcmRetirementPolicy::FreshContinuity)?;
        self.native_control_hold = None;
        self.native_control_output_retirement = None;
        self.render_blocked = false;
        Ok(())
    }

    /// Invalidate outstanding work and restart PCM scheduling at an exact timeline anchor.
    pub fn reprime(&mut self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        self.ensure_source_close_idle()?;
        self.validate_reprime_anchor_pure(anchor)?;
        self.ensure_render_execution_available()?;
        self.validate_reprime_output()?;
        if matches!(
            self.native_control_hold,
            Some(
                NativeControlHold::EditorCleanupPending(_)
                    | NativeControlHold::EditorDetachedAwaitingQuiescence(_)
            )
        ) {
            return Err(AudioPlaybackError::NativeEditorCleanupPending);
        }
        if matches!(
            self.native_control_hold,
            Some(NativeControlHold::AuthorStateEntry { .. })
        ) {
            self.next_start_sample = anchor.sample();
            self.generation_render_anchor = Some(anchor);
            return Ok(());
        }
        if let Some(renderer) = &self.renderer {
            renderer.continuity_model.validate_fresh_entry(anchor)?;
        }
        self.consecutive_render_generation_failures = 0;
        self.render_blocked = self.native_control_hold.is_some();
        self.reprime_prevalidated(
            anchor,
            false,
            self.renderer.clone(),
            PcmRetirementPolicy::FreshContinuity,
        )?;
        if matches!(
            self.native_control_hold,
            Some(NativeControlHold::EditorCleanupCompletePaused(_))
        ) {
            self.native_control_hold = None;
            self.native_control_output_retirement = None;
            self.render_blocked = false;
        }
        Ok(())
    }

    /// Close output and retire outstanding PCM without admitting a fresh Enter.
    /// The installed Source and its native controls remain owned. A later
    /// explicit prepare/reprime must still satisfy that Source's entry contract.
    pub fn suspend(&mut self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        self.ensure_source_close_idle()?;
        self.validate_reprime_anchor_pure(anchor)?;
        self.ensure_render_execution_available()?;
        self.validate_reprime_output()?;
        self.suspend_prevalidated(anchor)
    }

    /// Retire discontinuous PCM, restarting only when the current Source can
    /// enter at this actual anchor. Success may leave rendering blocked; it
    /// never keeps a failed generation active or substitutes another anchor.
    pub fn recover_at(&mut self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        self.ensure_source_close_idle()?;
        self.validate_reprime_anchor_pure(anchor)?;
        self.ensure_render_execution_available()?;
        self.validate_reprime_output()?;
        if self.recover_prevalidated(anchor, true)? == AudioRenderRecoveryDisposition::Reprime {
            self.consecutive_render_generation_failures = 0;
        }
        Ok(())
    }

    fn suspend_prevalidated(
        &mut self,
        anchor: AudioSamplePosition,
    ) -> Result<(), AudioPlaybackError> {
        self.reprime_prevalidated(
            anchor,
            false,
            self.renderer.clone(),
            PcmRetirementPolicy::Suspend,
        )
    }

    fn recover_prevalidated(
        &mut self,
        anchor: AudioSamplePosition,
        recovery_preroll: bool,
    ) -> Result<AudioRenderRecoveryDisposition, AudioPlaybackError> {
        // Eligibility is a pure decision before retirement, not a fallible
        // admission after a completion/device event has already been consumed.
        let entry_refusal = self
            .renderer
            .as_ref()
            .and_then(|renderer| renderer.continuity_model.validate_fresh_entry(anchor).err());
        if self.native_control_hold.is_some() || entry_refusal.is_some() {
            self.suspend_prevalidated(anchor)?;
            if let Some(error) = entry_refusal {
                tracing::warn!(%error, "PCM retired without admitting an unsupported fresh entry");
            }
            return Ok(AudioRenderRecoveryDisposition::Blocked);
        }
        self.reprime_prevalidated(
            anchor,
            recovery_preroll,
            self.renderer.clone(),
            PcmRetirementPolicy::FreshContinuity,
        )?;
        Ok(AudioRenderRecoveryDisposition::Reprime)
    }

    /// Validate an already-resolved sample anchor without changing any state.
    ///
    /// App transport can use this preflight before committing another Module's
    /// state. Timeline-to-sample lowering remains an upstream transport
    /// responsibility; this method only enforces the Media-side contract.
    pub fn validate_anchor(&self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        self.ensure_source_close_idle()?;
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

    fn external_output_retirement_pending(
        &self,
        output_snapshot: Option<RealtimeAudioOutputSnapshot>,
    ) -> Result<bool, AudioPlaybackError> {
        let Some(snapshot) = output_snapshot.filter(|snapshot| !snapshot.active) else {
            return Ok(false);
        };
        if !self
            .stream_media_anchor
            .is_some_and(|(stream_generation, _)| stream_generation == snapshot.stream_generation)
        {
            return Ok(false);
        }
        let token = self.quiescence_token.ok_or(AudioPlaybackError::MissingQuiescenceToken)?;
        match self.output.is_quiescent(token) {
            Err(RealtimeAudioOutputControlError::QuiescenceRevisionMismatch {
                token_revision,
                current_revision,
            }) if current_revision > token_revision => Ok(true),
            Ok(_) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn validate_poll_arithmetic(
        &self,
        mode: AudioPlaybackMode,
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
        let external_output_retirement_pending =
            self.external_output_retirement_pending(output_snapshot)?;
        // The owner trims PCM to the predicted audible activation point, not
        // the earlier wall-clock poll. The host delay names the callback tail;
        // it is also the first-frame time of the next equal-sized callback.
        let activation_authority = match output_snapshot {
            Some(snapshot)
                if mode.permits_consumption()
                    && !snapshot.active
                    && !external_output_retirement_pending =>
            {
                match (
                    snapshot.last_callback_playback_delay,
                    snapshot.last_callback_age,
                ) {
                    (Some(delay), Some(age)) if snapshot.callback_count > 0 => {
                        let lead_frames = delay
                            .saturating_sub(age)
                            .as_nanos()
                            .checked_mul(u128::from(self.config.sample_rate))
                            .ok_or(AudioPlaybackError::CoordinateOverflow)?
                            .div_ceil(1_000_000_000);
                        let lead_frames = i64::try_from(lead_frames)
                            .map_err(|_| AudioPlaybackError::CoordinateOverflow)?;
                        let sample = authority
                            .sample()
                            .checked_add(lead_frames)
                            .ok_or(AudioPlaybackError::CoordinateOverflow)?;
                        sample
                            .checked_add(maximum_sample_span)
                            .ok_or(AudioPlaybackError::CoordinateOverflow)?;
                        Some(AudioSamplePosition::new(sample, authority.rate()))
                    }
                    _ => None,
                }
            }
            _ => Some(authority),
        };
        let elapsed_skip_frames = if !external_output_retirement_pending
            && output_snapshot.is_some_and(|snapshot| !snapshot.active)
        {
            self.generation_render_anchor
                .map(|render_anchor| {
                    activation_authority.unwrap_or(authority).samples_since(render_anchor)
                })
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
            activation_authority,
            chunk_frames,
            elapsed_skip_frames,
            admission_target_frames,
            external_output_retirement_pending,
        })
    }

    fn reprime_prevalidated(
        &mut self,
        anchor: AudioSamplePosition,
        recovery_preroll: bool,
        renderer: Option<PreparedAudioPcmRenderer>,
        pcm_policy: PcmRetirementPolicy,
    ) -> Result<(), AudioPlaybackError> {
        if matches!(pcm_policy, PcmRetirementPolicy::FreshContinuity)
            && let Some(candidate) = &renderer
            && let Err(error) = candidate.continuity_model.validate_fresh_entry(anchor)
        {
            self.handoff_retired_render_owners(
                VecDeque::new(),
                renderer.map(|renderer| renderer.owner),
            );
            return Err(error);
        }
        let previous_generation = self.generation;
        let previous_renderer_id =
            self.renderer.as_ref().map(|renderer| renderer.materialization_id);
        let previous_in_flight = self.in_flight;
        let suspended_retirement = matches!(pcm_policy, PcmRetirementPolicy::Suspend)
            .then_some(self.native_control_output_retirement)
            .flatten();
        // A control hold changes scheduling/output generation, never the actual
        // installed renderer/Runtime/object. Preserve its sole claim and binding
        // so the exact frame-clear retry can still address the same object.
        let preserve_native_owner = matches!(
            pcm_policy,
            PcmRetirementPolicy::BeginNativeHold | PcmRetirementPolicy::PreserveNativeHold
        ) || (matches!(pcm_policy, PcmRetirementPolicy::Suspend)
            && self.renderer.is_some());
        if preserve_native_owner
            && !matches!((&self.renderer, &renderer),
            (Some(before), Some(after)) if before.materialization_id == after.materialization_id
                && Arc::ptr_eq(&before.owner, &after.owner))
        {
            return Err(AudioOwnerInstallationError::WrongRegistration.into());
        }
        let preserved_installation_generation = if preserve_native_owner {
            self.owner_installation.current_claim()?.map(|claim| claim.generation())
        } else {
            None
        };
        let (verifier, reservation) = if preserve_native_owner {
            (None, None)
        } else {
            // Read the actual Runtime's current verifier, never a cached previous
            // registration on this Arc. This metadata callback is outside the short
            // installation guard; a same-Arc candidate may have replaced its Runtime.
            let preflight = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let verifier = match renderer.as_ref() {
                    Some(renderer) if renderer.native_owner_binding_supported => Some(
                        renderer
                            .owner
                            .owner_installation_verifier()
                            .ok_or(AudioOwnerInstallationError::WrongRegistration)?,
                    ),
                    _ => None,
                };
                let expected = self.owner_installation.current_claim()?;
                let reservation = match &verifier {
                    Some(verifier) => self
                        .owner_installation
                        .reserve_install(&verifier.registration(), expected.as_ref())?,
                    None => self.owner_installation.reserve_clear(expected.as_ref())?,
                };
                Ok::<_, AudioOwnerInstallationError>((verifier, reservation))
            }));
            let (verifier, reservation) = match preflight {
                Ok(Ok(preflight)) => preflight,
                Ok(Err(error)) => {
                    if let Some(renderer) = renderer {
                        self.handoff_retired_render_owners(VecDeque::new(), Some(renderer.owner));
                    }
                    return Err(error.into());
                }
                Err(payload) => {
                    dispose_canonical_or_abandon_opaque_panic_payload(payload);
                    if let Some(renderer) = renderer {
                        self.handoff_retired_render_owners(VecDeque::new(), Some(renderer.owner));
                    }
                    return Err(AudioPlaybackError::OwnerRegistrationPanicked);
                }
            };
            (verifier, Some(reservation))
        };
        let quiescence_token = match self.output.deactivate() {
            Ok(token) => token,
            Err(error) => {
                if let Some(renderer) = renderer {
                    self.handoff_retired_render_owners(VecDeque::new(), Some(renderer.owner));
                }
                return Err(error.into());
            }
        };
        // All foreign preflight finished. The actual slot exchange and claim
        // publication share only this short local guard, including clear swaps.
        // No native call, callback, owner drop or queue operation occurs inside it.
        let source = Arc::clone(&self.owner_installation);
        let installation = match reservation {
            Some(reservation) => match source.begin_commit(reservation) {
                Ok(installation) => Some(installation),
                Err(error) => {
                    if let Some(renderer) = renderer {
                        self.handoff_retired_render_owners(VecDeque::new(), Some(renderer.owner));
                    }
                    return Err(error.into());
                }
            },
            None => None,
        };
        self.generation_cancellation.cancel();
        if !preserve_native_owner {
            self.native_control_cancellation.cancel();
        }
        let retired_renderer = std::mem::replace(&mut self.renderer, renderer);
        if self.renderer.as_ref().map(|renderer| renderer.materialization_id)
            != previous_renderer_id
        {
            self.native_editor_attachment = None;
        }
        let installed_claim = installation.and_then(|installation| installation.commit());
        if !preserve_native_owner {
            self.native_owner_binding_status = None;
        }
        self.output.clear();
        let pending = if preserve_native_owner {
            self.render_queue.take_pending_preserving_native_controls(
                previous_renderer_id,
                preserved_installation_generation,
                pcm_policy,
            )
        } else {
            self.render_queue.take_pending()
        };
        let canceled_current = pending.iter().filter(|work|
            matches!(work, RenderWork::Pcm(work) if work.generation == previous_generation && Some(work.renderer_id) == previous_renderer_id)).count();
        self.canceled_render_count =
            self.canceled_render_count.saturating_add(pending.len() as u64);
        self.handoff_retired_render_owners(
            pending,
            retired_renderer.map(|renderer| renderer.owner),
        );
        self.generation_cancellation = ExecutionCancellationToken::new();
        if !preserve_native_owner {
            self.native_control_cancellation = ExecutionCancellationToken::new();
            if let Some(work) = self.native_author_state_entry_request.take() {
                self.handoff_retired_render_owners(
                    VecDeque::from([RenderWork::NativeAuthorHandoff(work)]),
                    None,
                );
            }
        }
        self.generation += 1;
        if let (Some(renderer), Some(claim), Some(verifier)) =
            (&self.renderer, installed_claim, verifier)
        {
            let work = RenderWork::NativeOwnerBindings(NativeOwnerBindingWork {
                generation: self.generation,
                renderer_id: renderer.materialization_id,
                renderer: Arc::clone(&renderer.owner),
                claim,
                verifier: verifier.clone(),
                cancellation: self.native_control_cancellation.clone(),
            });
            if let Err(work) = self.render_queue.push(work) {
                self.native_owner_binding_status =
                    Some(AudioPcmNativeOwnerBindingStatus::Unavailable(
                        AudioPcmNativeOwnerBindingRefusal::WorkerUnavailable,
                    ));
                self.handoff_retired_render_owners(VecDeque::from([work]), None);
            }
        }
        match pcm_policy {
            PcmRetirementPolicy::FreshContinuity => self.pcm_journal.begin(
                self.renderer.as_ref().map(|renderer| renderer.materialization_id),
                self.generation,
            ),
            PcmRetirementPolicy::BeginNativeHold => {
                match (
                    previous_renderer_id,
                    previous_in_flight.checked_sub(canceled_current),
                ) {
                    (Some(renderer_id), Some(outstanding)) => {
                        if self
                            .pcm_journal
                            .hold(
                                renderer_id,
                                previous_generation,
                                self.generation,
                                outstanding,
                            )
                            .is_err()
                        {
                            self.pcm_journal.begin(None, self.generation);
                        }
                    }
                    _ => self.pcm_journal.begin(None, self.generation),
                }
            }
            PcmRetirementPolicy::PreserveNativeHold => match self.renderer.as_ref() {
                Some(renderer) => self
                    .pcm_journal
                    .advance_pending_generation(renderer.materialization_id, self.generation),
                None => self.pcm_journal.begin(None, self.generation),
            },
            PcmRetirementPolicy::Suspend => {
                if self.native_control_hold.is_some() || suspended_retirement.is_some() {
                    match self.renderer.as_ref() {
                        Some(renderer) => self.pcm_journal.advance_pending_generation(
                            renderer.materialization_id,
                            self.generation,
                        ),
                        None => self.pcm_journal.begin(None, self.generation),
                    }
                } else {
                    self.pcm_journal.begin(
                        self.renderer.as_ref().map(|renderer| renderer.materialization_id),
                        self.generation,
                    );
                }
            }
        }
        self.generation_entry_pending = true;
        self.in_flight = 0;
        self.next_start_sample = anchor.sample();
        self.generation_render_anchor = if matches!(pcm_policy, PcmRetirementPolicy::Suspend) {
            // No PCM generation was admitted at this transport coordinate.
            None
        } else {
            self.renderer.as_ref().map(|_| anchor)
        };
        self.media_anchor = None;
        self.quiescence_token = quiescence_token;
        if self.native_control_hold.is_some() || suspended_retirement.is_some() {
            self.native_control_output_retirement = Some(NativeControlOutputRetirement {
                generation: self.generation,
                token: quiescence_token,
                stream_generation: suspended_retirement
                    .and_then(|retirement| retirement.stream_generation)
                    .or_else(|| quiescence_token.map(|token| token.stream_generation)),
                media_anchor: suspended_retirement.and_then(|retirement| retirement.media_anchor),
                acknowledged: false,
            });
        }
        self.output_generation_ready = false;
        self.activation_preroll_satisfied = false;
        let underrun_frames = self.output.snapshot().map_or(0, |output| output.underrun_frames);
        self.underrun_baseline_frames = underrun_frames;
        self.last_underrun_frames = underrun_frames;
        self.recovery_preroll = recovery_preroll;
        if matches!(pcm_policy, PcmRetirementPolicy::Suspend) {
            self.render_blocked = self.renderer.is_some();
            self.recovery_preroll = false;
        }
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

    /// Current blocked native generation, independently of output-device availability.
    ///
    /// Fatal execution loss and explicit source replacement retire this identity.
    /// A missing physical output does not grant native DSP or clock continuation.
    pub fn native_control_hold_generation(&self) -> Option<u64> {
        (self.native_control_hold.is_some()
            && self.renderer.is_some()
            && self.snapshot(AudioPlaybackMode::Preroll).state
                != AudioPlaybackState::ExecutionUnavailable)
            .then_some(self.generation)
    }

    /// Current configuration generation, excluding editor lifecycle holds.
    pub fn native_reprepare_pending_generation(&self) -> Option<u64> {
        matches!(
            self.native_control_hold,
            Some(NativeControlHold::Reprepare(_))
        )
        .then(|| self.native_control_hold_generation())
        .flatten()
    }

    /// Current exact cleanup target from the one held control state.
    pub fn native_editor_cleanup_target(&self) -> Option<mondrian_core::NativeAudioEditorTarget> {
        self.native_control_hold.and_then(NativeControlHold::cleanup_target)
    }

    /// Visit an original complete PCM suffix retained by this exact held renderer.
    ///
    /// The visitor borrows existing original windows without cloning/re-rendering.
    /// A range outside admitted coverage, stale renderer/generation, unsettled old
    /// work or wrong grid is an explicit refusal. This does not establish a DAC
    /// anchor, DSP checkpoint, PDC migration or permission to reactivate/resume.
    pub fn visit_native_retained_pcm(
        &self,
        pending_generation: u64,
        requested_start: AudioSamplePosition,
        visitor: impl FnMut(crate::AudioRetainedPcmWindow<'_>),
    ) -> Result<crate::AudioRetainedPcmEvidence, crate::AudioRetainedPcmError> {
        if self.native_control_hold_generation() != Some(pending_generation) {
            return Err(crate::AudioRetainedPcmError::StaleOwner);
        }
        let renderer = self.renderer.as_ref().ok_or(crate::AudioRetainedPcmError::StaleOwner)?;
        self.pcm_journal.visit(
            renderer.materialization_id,
            pending_generation,
            requested_start,
            visitor,
        )
    }

    /// Finalize the held restart anchor for the exact still-pending generation.
    ///
    /// Output retirement must already be acknowledged. This changes no native
    /// contract, renderer, PCM, generation, author state or pending admission.
    pub fn finalize_native_control_anchor(
        &mut self,
        pending_generation: u64,
        anchor: AudioSamplePosition,
    ) -> Result<bool, AudioPlaybackError> {
        if self.native_control_hold_generation() != Some(pending_generation)
            || !self.native_control_output_retirement.is_some_and(|retirement| {
                retirement.generation == pending_generation && retirement.acknowledged
            })
        {
            return Ok(false);
        }
        self.validate_sample_anchor(anchor)?;
        self.native_control_output_retirement = None;
        self.next_start_sample = anchor.sample();
        if self.generation_render_anchor.is_some() {
            self.generation_render_anchor = self.renderer.as_ref().map(|_| anchor);
        }
        self.author_state_entry_output_retired(anchor);
        if let Some(NativeControlHold::EditorDetachedAwaitingQuiescence(target)) =
            self.native_control_hold
        {
            self.native_control_hold = Some(NativeControlHold::EditorCleanupCompletePaused(target));
        }
        Ok(true)
    }

    /// One cleanup admission for explicit native Close results and PCM preflight.
    /// The actual renderer/claim and original complete PCM journal are retained.
    fn begin_native_editor_cleanup_hold(
        &mut self,
        target: mondrian_core::NativeAudioEditorTarget,
        restart_anchor: AudioSamplePosition,
        start_sample: i64,
        events: &mut Vec<AudioPlaybackEvent>,
    ) -> Result<bool, AudioPlaybackError> {
        if self.native_editor_cleanup_target() == Some(target) {
            return Ok(false);
        }
        let final_output = self.output.snapshot();
        let final_media_anchor = self.media_anchor;
        let previous_generation = self.generation;
        self.reprime_prevalidated(
            restart_anchor,
            false,
            self.renderer.clone(),
            PcmRetirementPolicy::BeginNativeHold,
        )?;
        self.native_control_hold = Some(NativeControlHold::EditorCleanupPending(target));
        self.native_control_output_retirement = Some(NativeControlOutputRetirement {
            generation: self.generation,
            token: self.quiescence_token,
            stream_generation: final_output.map(|snapshot| snapshot.stream_generation),
            media_anchor: final_media_anchor,
            acknowledged: false,
        });
        self.render_blocked = true;
        self.recovery_preroll = false;
        events.push(AudioPlaybackEvent::NativeEditorCleanupRequired {
            previous_generation,
            pending_generation: self.generation,
            start_sample,
            restart_anchor,
            final_output,
            final_media_anchor,
            target,
        });
        Ok(true)
    }

    fn publish_native_control_output_quiescence(&mut self, events: &mut Vec<AudioPlaybackEvent>) {
        let Some(retirement) = self.native_control_output_retirement else {
            return;
        };
        if retirement.generation != self.generation {
            self.native_control_output_retirement = None;
            return;
        }
        if retirement.acknowledged {
            if self.native_control_hold.is_none() {
                self.native_control_output_retirement = None;
            }
            return;
        }
        if self.pcm_journal.old_work_unsettled() {
            return;
        }
        if retirement.token != self.quiescence_token
            || (retirement.token.is_some() && !self.output_generation_ready)
        {
            return;
        }
        let final_output = self
            .output
            .snapshot()
            .filter(|snapshot| Some(snapshot.stream_generation) == retirement.stream_generation);
        // A real author Ack may finish the control before callback quiescence.
        // Retain its output obligation until this actual token/old-work witness.
        self.native_control_output_retirement = self
            .native_control_hold
            .map(|_| NativeControlOutputRetirement { acknowledged: true, ..retirement });
        match self.native_control_hold {
            Some(NativeControlHold::Reprepare(_)) => {
                events.push(AudioPlaybackEvent::NativeReprepareOutputQuiesced {
                    pending_generation: retirement.generation,
                    final_output,
                    final_media_anchor: retirement.media_anchor,
                })
            }
            Some(
                NativeControlHold::EditorCleanupPending(target)
                | NativeControlHold::EditorDetachedAwaitingQuiescence(target)
                | NativeControlHold::EditorCleanupCompletePaused(target),
            ) => events.push(AudioPlaybackEvent::NativeEditorCleanupOutputQuiesced {
                pending_generation: retirement.generation,
                target,
                final_output,
                final_media_anchor: retirement.media_anchor,
            }),
            Some(NativeControlHold::AuthorStateEntry {
                phase: AuthorStateEntryPhase::WaitingForOutput,
                ..
            }) => events.push(AudioPlaybackEvent::NativeAuthorStateEntryOutputQuiesced {
                pending_generation: retirement.generation,
                final_output,
                final_media_anchor: retirement.media_anchor,
            }),
            _ => {}
        }
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
        let preflight = self.validate_poll_arithmetic(mode, authority)?;
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
                        self.render_blocked = self.native_control_hold.is_some();
                        if self.native_control_hold.is_some() {
                            self.reprime_prevalidated(
                                preflight.authority,
                                false,
                                self.renderer.clone(),
                                PcmRetirementPolicy::PreserveNativeHold,
                            )?;
                        } else {
                            self.recover_prevalidated(preflight.authority, false)?;
                        }
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

        if preflight.external_output_retirement_pending && generation_rotations == 0 {
            // The device worker has closed callback admission but has not yet
            // published the frozen Lost snapshot. Its newer deactivation
            // revision invalidates this module's activation token. Leave PCM
            // and generation state untouched until that lifecycle fact arrives.
            return Ok(AudioPlaybackPoll { snapshot: self.snapshot(mode), events });
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

        if self.drive_native_author_state_entry(
            preflight.authority,
            generation_rotations == 0,
            &mut events,
        )? {
            generation_rotations += 1;
        }

        self.publish_native_control_output_quiescence(&mut events);

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
            let completion = match completion {
                RenderCompletion::Pcm(completion) => completion,
                RenderCompletion::NativeAuthorHandoff(completion) => {
                    self.accept_native_author_handoff_completion(*completion, &mut events);
                    continue;
                }
                RenderCompletion::NativeEditor(completion) => {
                    let NativeEditorCompletion {
                        renderer_id,
                        cancellation,
                        installed_generation,
                        intent,
                        outcome,
                    } = *completion;
                    let current_install = self
                        .owner_installation
                        .current_claim()?
                        .is_some_and(|claim| claim.generation() == installed_generation);
                    let retained_close = matches!(
                        intent,
                        mondrian_core::NativeAudioEditorControlIntent::Close { .. }
                    ) && !cancellation.is_canceled();
                    if (current_install || retained_close)
                        && !cancellation.is_canceled()
                        && self
                            .renderer
                            .as_ref()
                            .is_some_and(|renderer| renderer.materialization_id == renderer_id)
                    {
                        match &outcome {
                            mondrian_core::NativeAudioEditorControlOutcome::Opened {
                                target,
                                ..
                            }
                            | mondrian_core::NativeAudioEditorControlOutcome::ClosePending {
                                target,
                                ..
                            } => self.native_editor_attachment = Some((renderer_id, *target)),
                            mondrian_core::NativeAudioEditorControlOutcome::Closed { .. } => {
                                self.native_editor_attachment = None;
                            }
                            mondrian_core::NativeAudioEditorControlOutcome::Refused(_) => {}
                        }
                        if let mondrian_core::NativeAudioEditorControlOutcome::ClosePending {
                            target,
                            stage: mondrian_core::NativeAudioEditorDetachStage::ClearHostFrame,
                        } = &outcome
                            && self.begin_native_editor_cleanup_hold(
                                *target,
                                preflight.authority,
                                preflight.authority.sample(),
                                &mut events,
                            )?
                        {
                            elapsed_skip_frames = Some(0);
                            admission_target_frames = self.config.high_watermark_frames;
                            generation_rotations += 1;
                        }
                        if let mondrian_core::NativeAudioEditorControlOutcome::Closed { target } =
                            &outcome
                        {
                            if self.native_control_hold
                                == Some(NativeControlHold::EditorCleanupPending(*target))
                            {
                                self.native_control_hold =
                                    Some(if self.native_control_output_retirement.is_none() {
                                        NativeControlHold::EditorCleanupCompletePaused(*target)
                                    } else {
                                        NativeControlHold::EditorDetachedAwaitingQuiescence(*target)
                                    });
                            }
                            if self.native_control_hold
                                == Some(NativeControlHold::EditorCleanupCompletePaused(*target))
                            {
                                events.push(AudioPlaybackEvent::NativeEditorCleanupReadyPaused {
                                    pending_generation: self.generation,
                                    target: *target,
                                });
                            }
                        }
                        events.push(AudioPlaybackEvent::NativeEditorObserved {
                            renderer_id,
                            generation: self.generation,
                            intent,
                            outcome,
                        });
                    } else {
                        self.stale_completion_count = self.stale_completion_count.saturating_add(1);
                    }
                    continue;
                }
                RenderCompletion::NativeObservations {
                    generation,
                    renderer_id,
                    installation_generation,
                    result,
                } => {
                    let result = match result {
                        NativeObservationCompletion::SourceClosed(result) => {
                            self.accept_source_close_completion(
                                generation,
                                renderer_id,
                                installation_generation,
                                result,
                            );
                            continue;
                        }
                        result => result,
                    };
                    if self.native_diagnostic_in_flight == Some((generation, renderer_id)) {
                        self.native_diagnostic_in_flight = None;
                    }
                    if generation == self.generation
                        && self
                            .renderer
                            .as_ref()
                            .is_some_and(|renderer| renderer.materialization_id == renderer_id)
                        && self.owner_installation.current_claim().is_ok_and(|claim| {
                            claim.map(|claim| claim.generation()) == installation_generation
                        })
                    {
                        match result {
                            NativeObservationCompletion::Observed(Some(batch))
                                if !batch.is_empty_complete() =>
                            {
                                let batch = *batch;
                                if let Some(diagnostics) = batch.diagnostics
                                    && !diagnostics.is_empty_complete()
                                {
                                    events.push(AudioPlaybackEvent::NativeDiagnosticsObserved {
                                        renderer_id,
                                        generation,
                                        batch: Box::new(diagnostics),
                                    });
                                }
                                // Refresh actual view target before parameter/proposal
                                // admission on this same renderer/claim completion.
                                if let Some(observation) = batch.attachment {
                                    events.push(
                                        AudioPlaybackEvent::NativeEditorAttachmentObserved {
                                            renderer_id,
                                            generation,
                                            observation: Box::new(observation),
                                        },
                                    );
                                }
                                if let Some(observation) = batch.parameters {
                                    events.push(
                                        AudioPlaybackEvent::NativeParameterObservationsObserved {
                                            renderer_id,
                                            generation,
                                            observation: Box::new(observation),
                                        },
                                    );
                                }
                                if let Some(proposal) = batch.author {
                                    events.push(AudioPlaybackEvent::NativeAuthorProposalObserved {
                                        renderer_id,
                                        generation,
                                        proposal: Box::new(proposal),
                                    });
                                }
                            }
                            NativeObservationCompletion::SourceClosed(result) => {
                                self.retire_source_close_result(result)
                            }
                            NativeObservationCompletion::Observed(_) => {}
                            NativeObservationCompletion::Refused(reason) => {
                                events.push(AudioPlaybackEvent::NativeObservationPollRefused {
                                    renderer_id,
                                    generation,
                                    reason,
                                })
                            }
                        }
                    } else {
                        if let NativeObservationCompletion::Observed(Some(batch)) = result {
                            self.native_diagnostic_discarded_records = self
                                .native_diagnostic_discarded_records
                                .saturating_add(batch.diagnostic_record_count() as u64);
                        }
                        self.stale_completion_count = self.stale_completion_count.saturating_add(1);
                    }
                    continue;
                }
                RenderCompletion::NativeOwnerBindings {
                    generation,
                    renderer_id,
                    installation_generation,
                    status,
                } => {
                    // Binding belongs to the installed Source, independently
                    // of a PCM-only suspension/generation retirement.
                    if self
                        .renderer
                        .as_ref()
                        .is_some_and(|renderer| renderer.materialization_id == renderer_id)
                        && self.owner_installation.current_claim().is_ok_and(|claim| {
                            claim.is_some_and(|claim| claim.generation() == installation_generation)
                        })
                    {
                        self.native_owner_binding_status = Some(status);
                        events.push(AudioPlaybackEvent::NativeOwnerBindingsObserved {
                            renderer_id,
                            generation,
                            status,
                        });
                    } else {
                        self.stale_completion_count = self.stale_completion_count.saturating_add(1);
                    }
                    continue;
                }
            };
            if completion.generation != self.generation
                || self
                    .renderer
                    .as_ref()
                    .is_none_or(|renderer| renderer.materialization_id != completion.renderer_id)
            {
                if self
                    .pcm_journal
                    .expects_old_completion(completion.renderer_id, completion.generation)
                {
                    match completion.result {
                        RenderCompletionResult::DeferredForReprepare(_)
                        | RenderCompletionResult::DeferredForEditorCleanup(_)
                        | RenderCompletionResult::DeferredForNativeAuthor(_) => {}
                        result => match validate_rendered_buffer(completion.request, result) {
                            Ok(buffer) => {
                                let _ = self.pcm_journal.record(
                                    completion.renderer_id,
                                    completion.generation,
                                    completion.request,
                                    buffer,
                                );
                            }
                            Err(_) => self.pcm_journal.invalidate_suffix(),
                        },
                    }
                    self.pcm_journal.settle_old_completion();
                }
                self.stale_completion_count = self.stale_completion_count.saturating_add(1);
                continue;
            }
            self.in_flight = self.in_flight.saturating_sub(1);
            let admission =
                if let RenderCompletionResult::DeferredForReprepare(notice) = &completion.result {
                    CompletionAdmission::DeferForReprepare(*notice)
                } else if let RenderCompletionResult::DeferredForEditorCleanup(target) =
                    &completion.result
                {
                    CompletionAdmission::DeferForEditorCleanup(*target)
                } else if let RenderCompletionResult::DeferredForNativeAuthor(target) =
                    &completion.result
                {
                    CompletionAdmission::DeferForNativeAuthor(**target)
                } else if !self.output_generation_ready {
                    CompletionAdmission::InvalidateGeneration(
                        "current-generation PCM completed before callback quiescence".to_owned(),
                    )
                } else {
                    match validate_rendered_buffer(completion.request, completion.result) {
                        Ok(buffer) => {
                            let enqueued = self.output.enqueue(&buffer);
                            // Keep the actual complete owner even after the queue
                            // copy; only the bounded journal owns this PCM Vec now.
                            let _ = self.pcm_journal.record(
                                completion.renderer_id,
                                completion.generation,
                                completion.request,
                                buffer,
                            );
                            match enqueued {
                                Ok(()) => CompletionAdmission::Accepted,
                                Err(error) => CompletionAdmission::InvalidateGeneration(format!(
                                    "device output rejected complete PCM window: {error}"
                                )),
                            }
                        }
                        Err(reason) if completion.allow_window_silence => {
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
                CompletionAdmission::DeferForReprepare(notice) => {
                    let final_output = self.output.snapshot();
                    let final_media_anchor = self.media_anchor;
                    let previous_generation = completion.generation;
                    self.reprime_prevalidated(
                        preflight.authority,
                        false,
                        self.renderer.clone(),
                        PcmRetirementPolicy::BeginNativeHold,
                    )?;
                    self.native_control_hold = Some(NativeControlHold::Reprepare(notice));
                    self.native_control_output_retirement = Some(NativeControlOutputRetirement {
                        generation: self.generation,
                        token: self.quiescence_token,
                        stream_generation: final_output.map(|snapshot| snapshot.stream_generation),
                        media_anchor: final_media_anchor,
                        acknowledged: false,
                    });
                    self.render_blocked = true;
                    self.recovery_preroll = false;
                    elapsed_skip_frames = Some(0);
                    admission_target_frames = self.config.high_watermark_frames;
                    generation_rotations += 1;
                    events.push(AudioPlaybackEvent::NativeReprepareRequired {
                        previous_generation,
                        pending_generation: self.generation,
                        start_sample: completion.request.start_sample,
                        restart_anchor: preflight.authority,
                        final_output,
                        final_media_anchor,
                        notice,
                    });
                }
                CompletionAdmission::DeferForNativeAuthor(target) => {
                    self.begin_native_author_hold(target, Some(completion.request.start_sample));
                    events.push(AudioPlaybackEvent::NativeAuthorPending {
                        target,
                        start_sample: completion.request.start_sample,
                        generation: self.generation,
                    });
                }
                CompletionAdmission::DeferForEditorCleanup(target) => {
                    if self.begin_native_editor_cleanup_hold(
                        target,
                        preflight.authority,
                        completion.request.start_sample,
                        &mut events,
                    )? {
                        elapsed_skip_frames = Some(0);
                        admission_target_frames = self.config.high_watermark_frames;
                        generation_rotations += 1;
                    }
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
                        self.suspend_prevalidated(preflight.authority)?;
                        AudioRenderRecoveryDisposition::Blocked
                    } else {
                        self.recover_prevalidated(preflight.authority, true)?
                    };
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

        if self.source_close_admission.is_some() {
            return Ok(AudioPlaybackPoll { snapshot: self.snapshot(mode), events });
        }

        if !mode.permits_consumption()
            && (self.output.snapshot().is_some_and(|snapshot| snapshot.active)
                || (mode == AudioPlaybackMode::Idle
                    && (!self.generation_entry_pending || self.in_flight != 0)))
        {
            if mode == AudioPlaybackMode::Idle {
                self.suspend_prevalidated(preflight.authority)?;
            } else {
                self.recover_prevalidated(preflight.authority, true)?;
            }
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
            self.schedule_native_diagnostics(&mut events);
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
            if interval_total_frames >= self.config.underrun_recovery_threshold_frames
                && !matches!(
                    self.native_control_hold,
                    Some(NativeControlHold::AuthorPending { .. })
                )
            {
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
                self.recover_prevalidated(preflight.authority, true)?;
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
                if self.generation_entry_pending
                    && renderer
                        .continuity_model
                        .validate_fresh_entry(AudioSamplePosition::new(
                            self.next_start_sample,
                            preflight.authority.rate(),
                        ))
                        .is_err()
                {
                    // Native control completion can release a hold, but it
                    // cannot grant an unsupported Enter for this actual window.
                    self.render_blocked = true;
                    self.generation_render_anchor = None;
                    self.recovery_preroll = false;
                    break;
                }
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
                let work = RenderWork::Pcm(PcmRenderWork {
                    generation: self.generation,
                    renderer_id: renderer.materialization_id,
                    request,
                    allow_window_silence: renderer.continuity_model
                        == AudioPcmContinuityModel::IndependentWindows,
                    renderer: Arc::clone(&renderer.owner),
                    cancellation: self.generation_cancellation.clone(),
                });
                if self.render_queue.push(work).is_err() {
                    break;
                }
                self.generation_entry_pending = false;
                self.in_flight += 1;
                self.next_start_sample = next_start_sample;
            }
            let activation_threshold =
                elapsed_skip_frames.and_then(|skip| skip.checked_add(self.config.preroll_frames));
            if !self.render_blocked
                && activation_threshold
                    .is_some_and(|required| self.output.buffered_frames() >= required)
            {
                self.activation_preroll_satisfied = true;
                self.consecutive_render_generation_failures = 0;
                if mode.permits_consumption()
                    && generation_rotations == 0
                    && let Some(activation_authority) = preflight.activation_authority
                    && self.output.snapshot().is_some_and(|snapshot| !snapshot.active)
                {
                    let skip_frames =
                        elapsed_skip_frames.ok_or(AudioPlaybackError::CoordinateOverflow)?;
                    let token =
                        self.quiescence_token.ok_or(AudioPlaybackError::MissingQuiescenceToken)?;
                    match self.output.activate_after_discard(token, skip_frames) {
                        Ok(()) => {
                            self.media_anchor = Some(activation_authority);
                            self.stream_media_anchor =
                                Some((token.stream_generation, activation_authority));
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

        // PCM admission above has priority; diagnostics share its one bounded work queue.
        self.schedule_native_diagnostics(&mut events);
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
        let pending = self.render_queue.take_pending_preserving_native_controls(
            self.renderer.as_ref().map(|renderer| renderer.materialization_id),
            self.owner_installation
                .current_claim()
                .ok()
                .flatten()
                .map(|claim| claim.generation()),
            PcmRetirementPolicy::PreserveNativeHold,
        );
        self.canceled_render_count =
            self.canceled_render_count.saturating_add(pending.len() as u64);
        self.handoff_retired_render_owners(pending, None);
        self.generation_cancellation = ExecutionCancellationToken::new();
        self.generation += 1;
        if self.native_control_hold.is_some() {
            match self.renderer.as_ref() {
                Some(renderer) => self
                    .pcm_journal
                    .advance_pending_generation(renderer.materialization_id, self.generation),
                None => self.pcm_journal.begin(None, self.generation),
            }
        } else {
            self.pcm_journal.begin(
                self.renderer.as_ref().map(|renderer| renderer.materialization_id),
                self.generation,
            );
        }
        self.generation_entry_pending = true;
        self.in_flight = 0;
        self.next_start_sample = authority.sample();
        self.generation_render_anchor = self.renderer.as_ref().map(|_| authority);
        self.media_anchor = None;
        self.quiescence_token = None;
        if self.native_control_hold.is_some() {
            self.native_control_output_retirement = Some(NativeControlOutputRetirement {
                generation: self.generation,
                token: None,
                stream_generation: None,
                media_anchor: None,
                acknowledged: false,
            });
        }
        self.output_generation_ready = false;
        self.activation_preroll_satisfied = false;
        self.underrun_baseline_frames = 0;
        self.last_underrun_frames = 0;
        self.recovery_preroll = recovery_preroll;
        if self.renderer.as_ref().is_some_and(|renderer| {
            renderer.continuity_model.validate_fresh_entry(authority).is_err()
        }) {
            self.render_blocked = true;
            self.generation_render_anchor = None;
            self.recovery_preroll = false;
        }
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
        self.native_control_cancellation.cancel();
        let pending = self.render_queue.stop();
        self.canceled_render_count =
            self.canceled_render_count.saturating_add(pending.len() as u64);
        if let Err(error) = self.owner_installation.revoke() {
            tracing::error!(%error, "native editor owner unavailable with closed audio execution");
        }
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

    /// Snapshot the machine-local output-device intent.
    pub fn output_device_selection(&self) -> RealtimeAudioOutputDeviceSelection {
        self.output.device_selection()
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
                Some(_) if self.renderer.is_none() => AudioPlaybackState::WaitingForSource,
                Some(_)
                    if matches!(
                        self.native_control_hold,
                        Some(NativeControlHold::Reprepare(_))
                    ) =>
                {
                    AudioPlaybackState::RepreparePending
                }
                Some(_)
                    if matches!(
                        self.native_control_hold,
                        Some(NativeControlHold::EditorCleanupCompletePaused(_))
                    ) =>
                {
                    AudioPlaybackState::EditorCleanupCompletePaused
                }
                Some(_)
                    if matches!(
                        self.native_control_hold,
                        Some(
                            NativeControlHold::AuthorPending { .. }
                                | NativeControlHold::AuthorStateEntry { .. }
                        )
                    ) =>
                {
                    AudioPlaybackState::NativeAuthorPending
                }
                Some(_) if self.native_control_hold.is_some() => {
                    AudioPlaybackState::EditorCleanupPending
                }
                Some(_) if mode == AudioPlaybackMode::Idle => AudioPlaybackState::Idle,
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
        self.retire_pending_render_completions();
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
    // Startup is ordinary control work, never a per-window/physical callback
    // allocation. The parent cannot enqueue PCM before native admission succeeds.
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), AudioThreadSchedulingError>>(1);
    let owner_queue = Arc::clone(&worker_queue);
    let worker = thread::Builder::new()
        .name("mondrian-audio-render".to_owned())
        .stack_size(AUDIO_PLAYBACK_PRODUCER_STACK_BYTES)
        .spawn(move || {
            // This slice implements Windows native admission. Other platforms
            // retain their explicitly unqualified ordinary producer policy;
            // they do not swallow a failed Windows request or claim MMCSS.
            #[cfg(target_os = "windows")]
            let scheduling =
                match RealtimeAudioThreadLease::enter(RealtimeAudioThreadRole::PlaybackProducer) {
                    Ok(lease) => Some(lease),
                    Err(error) => {
                        *worker_queue.scheduling.lock() =
                            AudioPlaybackProducerScheduling::Refused(error);
                        let _ = ready_tx.send(Err(error));
                        terminal.store(
                            RENDER_WORKER_TERMINAL_SCHEDULING_UNCONFIRMED,
                            AtomicOrdering::Release,
                        );
                        return;
                    }
                };
            #[cfg(not(target_os = "windows"))]
            let scheduling: Option<RealtimeAudioThreadLease> = None;
            *worker_queue.scheduling.lock() = match &scheduling {
                Some(lease) => AudioPlaybackProducerScheduling::Active(lease.evidence()),
                None => AudioPlaybackProducerScheduling::OrdinaryPlatform {
                    platform: std::env::consts::OS,
                },
            };
            // A lost startup owner must not leave a producer blocked forever.
            let startup_received = ready_tx.send(Ok(())).is_ok();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if startup_received {
                    run_render_worker(&worker_queue, &completion_tx)
                } else {
                    RENDER_WORKER_TERMINAL_NORMAL
                }
            }));
            let restored = scheduling.map(RealtimeAudioThreadLease::finish).transpose();
            let restoration_confirmed = restored.is_ok();
            *worker_queue.scheduling.lock() = match restored {
                Ok(Some(evidence)) => AudioPlaybackProducerScheduling::Restored(evidence),
                Ok(None) => AudioPlaybackProducerScheduling::OrdinaryPlatform {
                    platform: std::env::consts::OS,
                },
                Err(error) => AudioPlaybackProducerScheduling::Refused(error),
            };
            let terminal_outcome = match outcome {
                Ok(RENDER_WORKER_TERMINAL_NORMAL) if !restoration_confirmed => {
                    RENDER_WORKER_TERMINAL_SCHEDULING_UNCONFIRMED
                }
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
        })?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(worker),
        Ok(Err(error)) => {
            // A partial admission failure may leave the OS state unconfirmed;
            // this dedicated owner thread exits and is joined before refusal.
            owner_queue.stop();
            if let Err(payload) = worker.join() {
                let _ = dispose_canonical_or_abandon_opaque_panic_payload(payload);
            }
            Err(io::Error::other(error))
        }
        Err(_) => {
            owner_queue.stop();
            if let Err(payload) = worker.join() {
                let _ = dispose_canonical_or_abandon_opaque_panic_payload(payload);
            }
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }
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

fn retire_render_completion(
    completion: RenderCompletion,
    queue: &RenderOwnerRetirementQueue,
    tracker: &ForeignOwnerRetirementTracker,
) {
    // Exhaustively separate actual foreign Work from host-owned completion
    // data. New variants or local payload fields require this boundary review.
    // Pure PCM/DTO storage must not consume foreign retirement capacity.
    let work = match completion {
        RenderCompletion::NativeAuthorHandoff(completion) => {
            completion.into_retired_work().map(RenderWork::NativeAuthorHandoff)
        }
        RenderCompletion::Pcm(PcmRenderCompletion {
            generation,
            renderer_id,
            request,
            allow_window_silence,
            result,
        }) => {
            match result {
                RenderCompletionResult::Rendered(AudioBuffer {
                    samples,
                    sample_rate,
                    channel_layout,
                }) => drop((
                    generation,
                    renderer_id,
                    request,
                    allow_window_silence,
                    samples,
                    sample_rate,
                    channel_layout,
                )),
                RenderCompletionResult::DeferredForNativeAuthor(target) => drop((
                    generation,
                    renderer_id,
                    request,
                    allow_window_silence,
                    target,
                )),
                RenderCompletionResult::DeferredForReprepare(notice) => drop((
                    generation,
                    renderer_id,
                    request,
                    allow_window_silence,
                    RenderCompletionResult::DeferredForReprepare(notice),
                )),
                RenderCompletionResult::DeferredForEditorCleanup(target) => drop((
                    generation,
                    renderer_id,
                    request,
                    allow_window_silence,
                    RenderCompletionResult::DeferredForEditorCleanup(target),
                )),
                RenderCompletionResult::Failed(detail) => drop((
                    generation,
                    renderer_id,
                    request,
                    allow_window_silence,
                    detail,
                )),
            }
            None
        }
        RenderCompletion::NativeEditor(completion) => {
            let NativeEditorCompletion {
                renderer_id,
                cancellation,
                installed_generation,
                intent,
                outcome,
            } = *completion;
            drop((
                renderer_id,
                cancellation,
                installed_generation,
                intent,
                outcome,
            ));
            None
        }
        RenderCompletion::NativeObservations {
            generation,
            renderer_id,
            installation_generation,
            result,
        } => {
            match result {
                NativeObservationCompletion::SourceClosed(result) => {
                    if let Err(error) = result {
                        enqueue_retired_render_owners(
                            queue,
                            tracker,
                            RetiredRenderOwners {
                                pending: VecDeque::new(),
                                renderers: VecDeque::new(),
                                errors: VecDeque::from([error]),
                            },
                        );
                    }
                }
                NativeObservationCompletion::Observed(batch) => {
                    drop((generation, renderer_id, installation_generation, batch));
                }
                NativeObservationCompletion::Refused(refusal) => drop((
                    generation,
                    renderer_id,
                    installation_generation,
                    NativeObservationCompletion::Refused(refusal),
                )),
            }
            None
        }
        RenderCompletion::NativeOwnerBindings {
            generation,
            renderer_id,
            installation_generation,
            status,
        } => {
            drop(RenderCompletion::NativeOwnerBindings {
                generation,
                renderer_id,
                installation_generation,
                status,
            });
            None
        }
    };
    if let Some(work) = work {
        enqueue_retired_render_owners(
            queue,
            tracker,
            RetiredRenderOwners {
                pending: VecDeque::from([work]),
                renderers: VecDeque::new(),
                errors: VecDeque::new(),
            },
        );
    }
}

fn publish_render_completion(
    sender: &mpsc::Sender<RenderCompletion>,
    completion: RenderCompletion,
    queue: &RenderWorkQueue,
) -> bool {
    // Use stop's original lock. A successful send is entirely before stop;
    // no late producer can refill the receiver after shutdown's drain.
    // No payload is destroyed or provider code invoked under this guard.
    let retired = {
        let state = queue.state.lock();
        if state.stopped {
            Some(completion)
        } else {
            match sender.send(completion) {
                Ok(()) => {
                    #[cfg(test)]
                    queue.completion_publications.fetch_add(1, AtomicOrdering::Release);
                    None
                }
                Err(error) => Some(error.0),
            }
        }
    };
    if let Some(completion) = retired {
        retire_render_completion(
            completion,
            &queue.retirement_queue,
            &queue.retirement_tracker,
        );
        false
    } else {
        true
    }
}

fn run_render_worker(
    worker_queue: &RenderWorkQueue,
    completion_tx: &mpsc::Sender<RenderCompletion>,
) -> u8 {
    while let Some(work) = worker_queue.pop() {
        let work = match work {
            RenderWork::Pcm(work) => work,
            RenderWork::NativeAuthorHandoff(work) => {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    native_author::execute_native_author_handoff(&work)
                }));
                let completion = match result {
                    Ok(execution) => {
                        native_author::complete_native_author_handoff(work, execution, worker_queue)
                    }
                    Err(payload) => {
                        enqueue_retired_render_owners(
                            &worker_queue.retirement_queue,
                            &worker_queue.retirement_tracker,
                            RetiredRenderOwners {
                                pending: VecDeque::from([RenderWork::NativeAuthorHandoff(work)]),
                                renderers: VecDeque::new(),
                                errors: VecDeque::new(),
                            },
                        );
                        return if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                            RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
                        } else {
                            RENDER_WORKER_TERMINAL_PANICKED
                        };
                    }
                };
                if !publish_render_completion(
                    completion_tx,
                    RenderCompletion::NativeAuthorHandoff(Box::new(completion)),
                    worker_queue,
                ) {
                    break;
                }
                continue;
            }
            RenderWork::NativeEditor(work) => {
                use mondrian_core::{
                    NativeAudioEditorControlOutcome as Outcome,
                    NativeAudioEditorControlRefusal as Refusal,
                };
                let NativeEditorWork { renderer_id, renderer, claim, cancellation, intent } = *work;
                let installed_generation = claim.generation();
                let retained_close = matches!(
                    intent,
                    mondrian_core::NativeAudioEditorControlIntent::Close { .. }
                );
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if cancellation.is_canceled() {
                        return Ok(Outcome::Refused(Refusal::Cancelled));
                    }
                    let Some(verifier) = renderer.owner_installation_verifier() else {
                        return Ok(Outcome::Refused(Refusal::UnsupportedAdapter));
                    };
                    if !retained_close && verifier.validate(&claim).is_err() {
                        return Ok(Outcome::Refused(Refusal::OwnerChanged));
                    }
                    let outcome = renderer.control_native_editor(intent.clone(), &cancellation)?;
                    if cancellation.is_canceled() {
                        return Ok(Outcome::Refused(Refusal::Cancelled));
                    }
                    if !retained_close && verifier.validate(&claim).is_err() {
                        return Ok(Outcome::Refused(Refusal::OwnerChanged));
                    }
                    Ok(outcome)
                }));
                let outcome = match result {
                    Ok(Ok(outcome)) => outcome,
                    Ok(Err(error)) => {
                        enqueue_retired_render_owners(
                            &worker_queue.retirement_queue,
                            &worker_queue.retirement_tracker,
                            RetiredRenderOwners {
                                pending: VecDeque::new(),
                                renderers: VecDeque::new(),
                                errors: VecDeque::from([error]),
                            },
                        );
                        Outcome::Refused(Refusal::ProcessorRejected)
                    }
                    Err(payload) => {
                        enqueue_retired_render_owners(
                            &worker_queue.retirement_queue,
                            &worker_queue.retirement_tracker,
                            RetiredRenderOwners {
                                pending: VecDeque::new(),
                                renderers: VecDeque::from([renderer]),
                                errors: VecDeque::new(),
                            },
                        );
                        return if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                            RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
                        } else {
                            RENDER_WORKER_TERMINAL_PANICKED
                        };
                    }
                };
                enqueue_retired_render_owners(
                    &worker_queue.retirement_queue,
                    &worker_queue.retirement_tracker,
                    RetiredRenderOwners {
                        pending: VecDeque::new(),
                        renderers: VecDeque::from([renderer]),
                        errors: VecDeque::new(),
                    },
                );
                if !publish_render_completion(
                    completion_tx,
                    RenderCompletion::NativeEditor(Box::new(NativeEditorCompletion {
                        renderer_id,
                        cancellation,
                        installed_generation,
                        intent,
                        outcome,
                    })),
                    worker_queue,
                ) {
                    break;
                }
                continue;
            }
            RenderWork::NativeObservations(work) => {
                let NativeObservationWork {
                    generation,
                    renderer_id,
                    renderer,
                    cancellation,
                    installation_generation,
                    operation,
                } = work;
                let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if let NativeObservationOperation::SourceClosed(ticket) = operation {
                        return Ok(NativeObservationCompletion::SourceClosed(
                            source_close::execute(&renderer, &ticket, &cancellation),
                        ));
                    }
                    if cancellation.is_canceled() {
                        return Ok(NativeObservationCompletion::Refused(
                            AudioNativeObservationPollRefusal::Cancelled,
                        ));
                    }
                    renderer.poll_native_observations(&cancellation).map(|batch| {
                        NativeObservationCompletion::Observed(batch.and_then(|batch| {
                            (!batch.is_empty_complete()).then(|| Box::new(batch))
                        }))
                    })
                }));
                let result = match observed {
                    Ok(Ok(result)) => result,
                    Ok(Err(error)) => {
                        enqueue_retired_render_owners(
                            &worker_queue.retirement_queue,
                            &worker_queue.retirement_tracker,
                            RetiredRenderOwners {
                                pending: VecDeque::new(),
                                renderers: VecDeque::new(),
                                errors: VecDeque::from([error]),
                            },
                        );
                        NativeObservationCompletion::Refused(
                            AudioNativeObservationPollRefusal::ProcessorRejected,
                        )
                    }
                    Err(payload) => {
                        enqueue_retired_render_owners(
                            &worker_queue.retirement_queue,
                            &worker_queue.retirement_tracker,
                            RetiredRenderOwners {
                                pending: VecDeque::new(),
                                renderers: VecDeque::from([renderer]),
                                errors: VecDeque::new(),
                            },
                        );
                        return if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                            RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
                        } else {
                            RENDER_WORKER_TERMINAL_PANICKED
                        };
                    }
                };
                enqueue_retired_render_owners(
                    &worker_queue.retirement_queue,
                    &worker_queue.retirement_tracker,
                    RetiredRenderOwners {
                        pending: VecDeque::new(),
                        renderers: VecDeque::from([renderer]),
                        errors: VecDeque::new(),
                    },
                );
                if !publish_render_completion(
                    completion_tx,
                    RenderCompletion::NativeObservations {
                        generation,
                        renderer_id,
                        installation_generation,
                        result,
                    },
                    worker_queue,
                ) {
                    break;
                }
                continue;
            }
            RenderWork::NativeOwnerBindings(work) => {
                let NativeOwnerBindingWork {
                    generation,
                    renderer_id,
                    renderer,
                    claim,
                    verifier,
                    cancellation,
                } = work;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if cancellation.is_canceled() {
                        return Ok(AudioPcmNativeOwnerBindingStatus::Unavailable(
                            AudioPcmNativeOwnerBindingRefusal::Cancelled,
                        ));
                    }
                    if verifier.validate(&claim).is_err() {
                        return Ok(AudioPcmNativeOwnerBindingStatus::Unavailable(
                            AudioPcmNativeOwnerBindingRefusal::Superseded,
                        ));
                    }
                    renderer.install_native_owner_bindings(&claim, &cancellation)
                }));
                let status = match result {
                    Ok(Ok(status)) => {
                        if cancellation.is_canceled() {
                            AudioPcmNativeOwnerBindingStatus::Unavailable(
                                AudioPcmNativeOwnerBindingRefusal::Cancelled,
                            )
                        } else if verifier.validate(&claim).is_err() {
                            AudioPcmNativeOwnerBindingStatus::Unavailable(
                                AudioPcmNativeOwnerBindingRefusal::Superseded,
                            )
                        } else {
                            status
                        }
                    }
                    Ok(Err(error)) => {
                        enqueue_retired_render_owners(
                            &worker_queue.retirement_queue,
                            &worker_queue.retirement_tracker,
                            RetiredRenderOwners {
                                pending: VecDeque::new(),
                                renderers: VecDeque::new(),
                                errors: VecDeque::from([error]),
                            },
                        );
                        AudioPcmNativeOwnerBindingStatus::Unavailable(
                            AudioPcmNativeOwnerBindingRefusal::ProcessorRejected,
                        )
                    }
                    Err(payload) => {
                        enqueue_retired_render_owners(
                            &worker_queue.retirement_queue,
                            &worker_queue.retirement_tracker,
                            RetiredRenderOwners {
                                pending: VecDeque::new(),
                                renderers: VecDeque::from([renderer]),
                                errors: VecDeque::new(),
                            },
                        );
                        return if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                            RENDER_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
                        } else {
                            RENDER_WORKER_TERMINAL_PANICKED
                        };
                    }
                };
                enqueue_retired_render_owners(
                    &worker_queue.retirement_queue,
                    &worker_queue.retirement_tracker,
                    RetiredRenderOwners {
                        pending: VecDeque::new(),
                        renderers: VecDeque::from([renderer]),
                        errors: VecDeque::new(),
                    },
                );
                if !publish_render_completion(
                    completion_tx,
                    RenderCompletion::NativeOwnerBindings {
                        generation,
                        renderer_id,
                        installation_generation: claim.generation(),
                        status,
                    },
                    worker_queue,
                ) {
                    break;
                }
                continue;
            }
        };
        let PcmRenderWork {
            generation,
            renderer_id,
            request,
            allow_window_silence,
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
        if !publish_render_completion(
            completion_tx,
            RenderCompletion::Pcm(PcmRenderCompletion {
                generation,
                renderer_id,
                request,
                allow_window_silence,
                result,
            }),
            worker_queue,
        ) {
            break;
        }
    }
    RENDER_WORKER_TERMINAL_NORMAL
}

fn stabilize_render_result(
    result: mondrian_core::Result<AudioPcmRenderOutcome>,
    retirement_queue: &RenderOwnerRetirementQueue,
    retirement_tracker: &ForeignOwnerRetirementTracker,
) -> RenderCompletionResult {
    match result {
        Ok(AudioPcmRenderOutcome::Rendered(buffer)) => RenderCompletionResult::Rendered(buffer),
        Ok(AudioPcmRenderOutcome::DeferredForReprepare(notice)) => {
            RenderCompletionResult::DeferredForReprepare(notice)
        }
        Ok(AudioPcmRenderOutcome::DeferredForNativeAuthor(target)) => {
            RenderCompletionResult::DeferredForNativeAuthor(target)
        }
        Ok(AudioPcmRenderOutcome::DeferredForEditorCleanup(target)) => {
            RenderCompletionResult::DeferredForEditorCleanup(target)
        }
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
    if config.max_in_flight >= MAX_RETIRED_RENDER_OWNERS - MAX_PENDING_NATIVE_WORK {
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
        RenderCompletionResult::DeferredForReprepare(_)
        | RenderCompletionResult::DeferredForEditorCleanup(_)
        | RenderCompletionResult::DeferredForNativeAuthor(_) => {
            return Err("native control outcome requires typed admission".to_owned());
        }
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
    mod source_close_contracts {
        include!("audio_playback/source_close_tests.rs");
    }
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
            access_policy: crate::RealtimeAudioOutputAccessPolicy::Shared,
            share_mode: crate::RealtimeAudioOutputShareMode::Shared,
            exclusive_fallback_reason: None,
            stream_container_bits: None,
            stream_valid_bits: None,
            stream_sample_rate: contract.sample_rate,
            buffer_frames: None,
            period_100ns: None,
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

        fn clear(&mut self) {
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
        fn bind_native_child_settlement(
            &self,
            registrar: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            // This portable test adapter owns only the host fields declared here.
            let _ = registrar;
            Ok(())
        }
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                request.sample_rate,
                request.channel_layout,
                request.frame_count,
            )))
        }
    }

    impl AudioPcmRenderer for GateRenderer {
        fn bind_native_child_settlement(
            &self,
            registrar: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            // This portable test adapter owns only the host fields declared here.
            let _ = registrar;
            Ok(())
        }
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            self.entered.store(true, Ordering::Release);
            while !self.released.load(Ordering::Acquire) {
                if cancellation.is_canceled() {
                    self.canceled.store(true, Ordering::Release);
                    break;
                }
                thread::yield_now();
            }
            Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                request.sample_rate,
                request.channel_layout,
                request.frame_count,
            )))
        }
    }

    impl AudioPcmRenderer for DetachedPanicRenderer {
        fn bind_native_child_settlement(
            &self,
            registrar: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            // This portable test adapter owns only the host fields declared here.
            let _ = registrar;
            Ok(())
        }
        fn render(
            &self,
            _request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            self.entered.store(true, Ordering::Release);
            while !self.released.load(Ordering::Acquire) {
                thread::yield_now();
            }
            std::panic::panic_any(DropProbe { dropped: Arc::clone(&self.payload_dropped) });
        }
    }

    impl AudioPcmRenderer for RecordingRenderer {
        fn bind_native_child_settlement(
            &self,
            registrar: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            // This portable test adapter owns only the host fields declared here.
            let _ = registrar;
            Ok(())
        }
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            self.requests.lock().push(request);
            let frames = if self.wrong_frame_count {
                request.frame_count.saturating_sub(1)
            } else {
                request.frame_count
            };
            Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                request.sample_rate,
                request.channel_layout,
                frames,
            )))
        }
    }

    impl AudioPcmRenderer for FailOnceStatefulRenderer {
        fn bind_native_child_settlement(
            &self,
            registrar: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            // This portable test adapter owns only the host fields declared here.
            let _ = registrar;
            Ok(())
        }
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            self.requests.lock().push(request);
            if matches!(request.continuity, AudioPcmContinuity::Continue(_))
                && !self.failed.swap(true, Ordering::AcqRel)
            {
                return Err(mondrian_core::MondrianError::DecodeFailed {
                    asset_id: "stateful-test".to_owned(),
                    reason: "injected continuation failure".to_owned(),
                });
            }
            Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                request.sample_rate,
                request.channel_layout,
                request.frame_count,
            )))
        }
    }

    impl AudioPcmRenderer for StatefulRecordingRenderer {
        fn bind_native_child_settlement(
            &self,
            registrar: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            // This portable test adapter owns only the host fields declared here.
            let _ = registrar;
            Ok(())
        }
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            self.requests.lock().push(request);
            Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                request.sample_rate,
                request.channel_layout,
                request.frame_count,
            )))
        }
    }

    impl AudioPcmRenderer for AlwaysFailStatefulRenderer {
        fn bind_native_child_settlement(
            &self,
            registrar: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            // This portable test adapter owns only the host fields declared here.
            let _ = registrar;
            Ok(())
        }
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            self.requests.lock().push(request);
            Err(mondrian_core::MondrianError::DecodeFailed {
                asset_id: "persistent-stateful-test".to_owned(),
                reason: "injected persistent failure".to_owned(),
            })
        }
    }

    struct DeferredTrackingRenderer {
        requests: Arc<Mutex<Vec<AudioPcmRenderRequest>>>,
        dropped: Arc<AtomicBool>,
        notice: mondrian_core::NativeAudioReprepareNotice,
    }

    impl Drop for DeferredTrackingRenderer {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    impl AudioPcmRenderer for DeferredTrackingRenderer {
        fn bind_native_child_settlement(
            &self,
            registrar: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            // This portable test adapter owns only the host fields declared here.
            let _ = registrar;
            Ok(())
        }
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            self.requests.lock().push(request);
            Ok(AudioPcmRenderOutcome::DeferredForReprepare(self.notice))
        }
    }

    #[test]
    fn native_reprepare_deferral_retains_owner_without_fault_or_silence() {
        let (output, output_state) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("render worker");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicBool::new(false));
        let notice = mondrian_core::NativeAudioReprepareNotice {
            instance_id: mondrian_core::AudioProcessorInstanceId::new(),
            request: mondrian_core::NativeAudioReprepareRequest::ClapRestart,
        };
        let renderer: Arc<dyn AudioPcmRenderer> = Arc::new(DeferredTrackingRenderer {
            requests: Arc::clone(&requests),
            dropped: Arc::clone(&dropped),
            notice,
        });
        let retained = Arc::downgrade(&renderer);
        playback
            .prepare(
                sample_position(0),
                renderer,
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("accepted owner");
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut observed = Vec::new();
        while Instant::now() < deadline {
            let poll = playback.poll(AudioPlaybackMode::Consume, sample_position(0)).expect("poll");
            observed.extend(poll.events);
            if poll.snapshot.state == AudioPlaybackState::RepreparePending {
                break;
            }
            thread::yield_now();
        }
        let pending = playback.snapshot(AudioPlaybackMode::Consume);
        assert_eq!(pending.state, AudioPlaybackState::RepreparePending);
        assert_eq!(pending.render_substitution_count, 0);
        assert_eq!(pending.render_generation_recovery_count, 0);
        assert_eq!(playback.consecutive_render_generation_failures, 0);
        assert_eq!(output_state.lock().queued_frames, 0);
        assert!(pending.output.is_none_or(|snapshot| !snapshot.active));
        assert!(retained.upgrade().is_some());
        assert!(!dropped.load(Ordering::Acquire));
        let actual = observed
            .iter()
            .filter_map(|event| match event {
                AudioPlaybackEvent::NativeReprepareRequired {
                    previous_generation,
                    pending_generation,
                    notice: actual,
                    ..
                } => Some((*previous_generation, *pending_generation, *actual)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].1, actual[0].0 + 1);
        assert_eq!(actual[0].2, notice);
        assert!(!observed.iter().any(|event| matches!(
            event,
            AudioPlaybackEvent::RenderGenerationInvalidated { .. }
                | AudioPlaybackEvent::RenderSubstitutedWithSilence { .. }
        )));
        for _ in 0..8 {
            let poll = playback
                .poll(AudioPlaybackMode::Consume, sample_position(0))
                .expect("retained poll");
            assert_eq!(poll.snapshot.generation, pending.generation);
            assert!(poll
                .events
                .iter()
                .all(|event| !matches!(event, AudioPlaybackEvent::NativeReprepareRequired { .. })));
        }
        assert!(requests
            .lock()
            .iter()
            .all(|request| request.continuity.generation().get() != pending.generation));
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).render_generation_recovery_count,
            0
        );

        // Explicitly installing a newly prepared source is the legal escape;
        // no fresh Enter is automatically sent to the old pending source.
        playback
            .prepare(
                sample_position(0),
                Arc::new(RecordingRenderer {
                    requests: Arc::new(Mutex::new(Vec::new())),
                    wrong_frame_count: false,
                }),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("new canonical owner");
        poll_until_settled(&mut playback, sample_position(0));
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).state,
            AudioPlaybackState::Active
        );
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).render_substitution_count,
            0
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "actual Windows production producer MMCSS entry/render/restore"]
    fn scheduling_actual_playback_producer_enters_before_render_and_restores_after_join() {
        struct ActualThreadRenderer(Arc<std::sync::atomic::AtomicU32>);
        impl AudioPcmRenderer for ActualThreadRenderer {
            fn bind_native_child_settlement(
                &self,
                registrar: &mondrian_core::NativeChildSettlementRegistrar,
            ) -> Result<(), mondrian_core::NativeChildSettlementError> {
                // This portable test adapter owns only the host fields declared here.
                let _ = registrar;
                Ok(())
            }
            fn render(
                &self,
                request: AudioPcmRenderRequest,
                _cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
                // SAFETY: documented current physical thread identity query.
                let current =
                    unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
                self.0.store(current, Ordering::Release);
                Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                    request.sample_rate,
                    request.channel_layout,
                    request.frame_count,
                )))
            }
        }
        let (output, _) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("actual producer admission");
        let AudioPlaybackProducerScheduling::Active(entry) = playback.producer_scheduling() else {
            panic!("actual Windows producer did not admit its scheduling role");
        };
        assert_eq!(entry.role(), RealtimeAudioThreadRole::PlaybackProducer);
        assert_eq!(
            entry.task(),
            mondrian_audio_scheduling::RealtimeAudioTask::Audio
        );
        assert_eq!(entry.relative_priority(), 0);
        // SAFETY: the UI/test owner queries its own physical identity.
        assert_ne!(entry.native_thread_id(), unsafe {
            windows_sys::Win32::System::Threading::GetCurrentThreadId()
        });
        let observed = Arc::new(std::sync::atomic::AtomicU32::new(0));
        playback
            .prepare(
                sample_position(0),
                Arc::new(ActualThreadRenderer(Arc::clone(&observed))),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("canonical source");
        poll_until_settled(&mut playback, sample_position(0));
        assert_eq!(observed.load(Ordering::Acquire), entry.native_thread_id());
        let queue = Arc::clone(&playback.render_queue);
        assert!(playback.shutdown_and_wait().all_workers_terminated());
        let AudioPlaybackProducerScheduling::Restored(exit) = *queue.scheduling.lock() else {
            panic!("successful shutdown must have native restore evidence");
        };
        assert_eq!(exit.registration(), entry);
    }

    #[test]
    fn scheduling_playback_projects_closed_native_admission_refusal_without_leaking_foreign_owner()
    {
        let (output, _) = fake_output();
        let result = AudioPlayback::with_output_and_spawner(test_config(), output, |_, _, _| {
            Err(io::Error::other(
                AudioThreadSchedulingError::RegistrationRefused { code: 1062 },
            ))
        });
        assert!(matches!(
            result,
            Err(AudioPlaybackCreateError::ProducerScheduling(
                AudioThreadSchedulingError::RegistrationRefused { code: 1062 }
            ))
        ));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn scheduling_playback_ordinary_platform_remains_usable_without_rt_admission_claim() {
        let (output, _) = fake_output();
        let playback =
            AudioPlayback::with_output(test_config(), output).expect("ordinary platform producer");
        assert_eq!(
            playback.producer_scheduling(),
            AudioPlaybackProducerScheduling::OrdinaryPlatform { platform: std::env::consts::OS }
        );
        assert!(playback.shutdown_and_wait().all_workers_terminated());
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
            callback_count: 1,
            underrun_frames: 0,
            last_callback_frames: 10,
            last_callback_playback_delay: Some(Duration::ZERO),
            last_callback_playback_delay_uncertainty: Some(Duration::ZERO),
            last_callback_age: Some(Duration::ZERO),
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
        for (continuity_model, authority) in [
            (AudioPcmContinuityModel::GenerationState, sample_position(5)),
            (
                AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
                sample_position(0),
            ),
        ] {
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
                    continuity_model,
                )
                .expect("valid audio anchor");
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
    fn retirement_capacity_includes_reserved_native_work_and_renderer_owner() {
        let mut config = test_config();
        config.max_in_flight = MAX_RETIRED_RENDER_OWNERS - MAX_PENDING_NATIVE_WORK - 1;
        assert_eq!(validate_config(config), Ok(()));

        config.max_in_flight = MAX_RETIRED_RENDER_OWNERS - MAX_PENDING_NATIVE_WORK;
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
    fn host_pcm_completions_preserve_foreign_retirement_capacity() {
        let mut config = test_config();
        config.max_in_flight = 250;
        assert_eq!(validate_config(config), Ok(()));
        assert_eq!(
            config.max_in_flight,
            MAX_RETIRED_RENDER_OWNERS - MAX_PENDING_NATIVE_WORK - 1
        );
        let faulted = Arc::new(AtomicBool::new(false));
        let queue = Arc::new(RenderOwnerRetirementQueue::new(Arc::clone(&faulted)));
        let tracker = Arc::new(ForeignOwnerRetirementTracker::default());
        let dropped = Arc::new(AtomicBool::new(false));
        let renderers = (0..config.max_in_flight)
            .map(|_| {
                Arc::new(DropTrackingRenderer { dropped: Arc::clone(&dropped) })
                    as Arc<dyn AudioPcmRenderer>
            })
            .collect();
        assert!(queue
            .push(RetiredRenderOwners {
                pending: VecDeque::new(),
                renderers,
                errors: VecDeque::new(),
            })
            .is_ok());
        for _ in 0..config.max_in_flight {
            retire_render_completion(
                RenderCompletion::Pcm(PcmRenderCompletion {
                    generation: 1,
                    renderer_id: mondrian_core::AudioPcmRendererMaterializationId::new(),
                    request: AudioPcmRenderRequest {
                        start_sample: 0,
                        frame_count: 1,
                        sample_rate: 1_000,
                        channel_layout: AudioChannelLayout::Stereo,
                        continuity: AudioPcmContinuity::Enter(AudioPcmRenderGeneration::new(1)),
                    },
                    allow_window_silence: true,
                    result: RenderCompletionResult::Rendered(AudioBuffer {
                        samples: vec![0.5, -0.5],
                        sample_rate: 1_000,
                        channel_layout: AudioChannelLayout::Stereo,
                    }),
                }),
                &queue,
                &tracker,
            );
        }
        assert_eq!(
            queue.state.lock().pending.front().expect("original batch").owner_units(),
            250,
            "host PCM completions do not duplicate already-retired renderer units"
        );
        assert!(!faulted.load(Ordering::Acquire));
        assert_eq!(
            tracker.snapshot(),
            ForeignOwnerRetirementEvidence::default()
        );
        enqueue_retired_render_owners(
            &queue,
            &tracker,
            RetiredRenderOwners {
                pending: VecDeque::new(),
                renderers: (250..MAX_RETIRED_RENDER_OWNERS)
                    .map(|_| {
                        Arc::new(DropTrackingRenderer { dropped: Arc::clone(&dropped) })
                            as Arc<dyn AudioPcmRenderer>
                    })
                    .collect(),
                errors: VecDeque::new(),
            },
        );
        assert_eq!(
            queue.state.lock().pending.front().expect("original batch").owner_units(),
            256,
            "the original foreign-owner capacity is unchanged"
        );
        assert!(!faulted.load(Ordering::Acquire));
        let rejected_dropped = Arc::new(AtomicBool::new(false));
        enqueue_retired_render_owners(
            &queue,
            &tracker,
            RetiredRenderOwners {
                pending: VecDeque::new(),
                renderers: VecDeque::from([Arc::new(DropTrackingRenderer {
                    dropped: Arc::clone(&rejected_dropped),
                }) as Arc<dyn AudioPcmRenderer>]),
                errors: VecDeque::new(),
            },
        );
        assert!(
            faulted.load(Ordering::Acquire),
            "257th foreign owner must fail closed"
        );
        assert_eq!(tracker.snapshot().owner_abandonments, 1);
        assert!(
            !dropped.load(Ordering::Acquire),
            "accepted owners remain queued"
        );
        assert!(
            !rejected_dropped.load(Ordering::Acquire),
            "rejected owner is abandoned"
        );
        let terminal = Arc::new(AtomicU8::new(RENDER_WORKER_TERMINAL_UNKNOWN));
        let worker = spawn_render_owner_retirement_worker(
            Arc::clone(&queue),
            Arc::clone(&tracker),
            Arc::clone(&terminal),
        )
        .expect("original retirement worker");
        queue.stop();
        worker.join().expect("original worker drains the accepted batch");
        assert!(dropped.load(Ordering::Acquire));
        assert!(!rejected_dropped.load(Ordering::Acquire));
        assert_eq!(
            terminal.load(Ordering::Acquire),
            RENDER_WORKER_TERMINAL_NORMAL
        );
        assert_eq!(tracker.snapshot().panics, 0);
        assert_eq!(tracker.snapshot().owner_abandonments, 1);
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

    mod entry_origin_controls {
        use super::*;

        struct GatedInstalledRenderer {
            installed: Arc<NativeInstallationProbe>,
            pcm: GateRenderer,
        }

        impl AudioPcmRenderer for GatedInstalledRenderer {
            fn bind_native_child_settlement(
                &self,
                registrar: &mondrian_core::NativeChildSettlementRegistrar,
            ) -> Result<(), mondrian_core::NativeChildSettlementError> {
                self.installed.bind_native_child_settlement(registrar)
            }
            fn owner_installation_verifier(&self) -> Option<AudioOwnerInstallationVerifier> {
                self.installed.owner_installation_verifier()
            }
            fn bind_owner_installation(
                &self,
                verifier: AudioOwnerInstallationVerifier,
            ) -> Result<bool, AudioOwnerInstallationError> {
                self.installed.bind_owner_installation(verifier)
            }
            fn install_native_owner_bindings(
                &self,
                claim: &AudioOwnerInstallationClaim,
                cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<AudioPcmNativeOwnerBindingStatus> {
                self.installed.install_native_owner_bindings(claim, cancellation)
            }
            fn render(
                &self,
                request: AudioPcmRenderRequest,
                cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
                self.pcm.render(request, cancellation)
            }
        }

        #[test]
        fn origin_refusal_preserves_installed_claim_pending_pcm_and_output() {
            let (output, state) = fake_output();
            let mut playback = AudioPlayback::with_output(test_config(), output).unwrap();
            let installed = NativeInstallationProbe::new(false, false);
            let entered = Arc::new(AtomicBool::new(false));
            let released = Arc::new(AtomicBool::new(false));
            let canceled = Arc::new(AtomicBool::new(false));
            // Release precedes Playback destruction even when an assertion fails.
            let release = NativeHoldRelease(Arc::clone(&released));
            playback
                .prepare(
                    sample_position(0),
                    Arc::new(GatedInstalledRenderer {
                        installed: Arc::clone(&installed),
                        pcm: GateRenderer {
                            entered: Arc::clone(&entered),
                            released,
                            canceled: Arc::clone(&canceled),
                        },
                    }),
                    AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
                )
                .unwrap();
            settle_native_installation(&mut playback);
            playback.poll(AudioPlaybackMode::Preroll, sample_position(0)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !entered.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(entered.load(Ordering::Acquire));
            {
                let mut output = state.lock();
                output.queued_frames = 11;
                let snapshot = output.snapshot.as_mut().unwrap();
                snapshot.active = true;
                snapshot.buffered_frames = 11;
            }
            let claim = installed.verifier().current_claim().unwrap();
            let generation = playback.generation;
            let renderer = playback.renderer.as_ref().unwrap().materialization_id;
            let pending = playback.render_queue.state.lock().pending.len();
            assert_eq!(pending, 2, "actual serial PCM admissions remain queued");
            let before = playback.snapshot(AudioPlaybackMode::Preroll);
            let quiescence = state.lock().quiescence_revision;
            let candidate = NativeInstallationProbe::new(false, false);
            let refused = AudioPlaybackError::FreshEntryOriginMismatch {
                required: sample_position(0),
                actual: sample_position(10),
            };
            assert_eq!(
                playback.prepare(
                    sample_position(10),
                    candidate.clone(),
                    AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
                ),
                Err(refused)
            );
            assert!(
                candidate.verifier.lock().is_none(),
                "refused before foreign registration"
            );
            assert_eq!(playback.reprime(sample_position(10)), Err(refused));
            // This is the actual Fresh helper used by automatic recovery. It
            // retires only the passed clone, never the still-installed owner.
            assert_eq!(
                playback.reprime_prevalidated(
                    sample_position(10),
                    true,
                    playback.renderer.clone(),
                    PcmRetirementPolicy::FreshContinuity,
                ),
                Err(refused)
            );
            assert_eq!(playback.snapshot(AudioPlaybackMode::Preroll), before);
            assert_eq!(playback.generation, generation);
            assert_eq!(
                playback.renderer.as_ref().unwrap().materialization_id,
                renderer
            );
            assert_eq!(playback.render_queue.state.lock().pending.len(), pending);
            assert_eq!(state.lock().quiescence_revision, quiescence);
            assert!(installed.verifier().validate(&claim).is_ok());
            assert!(!playback.generation_cancellation.is_canceled());
            assert!(!playback.native_control_cancellation.is_canceled());
            assert!(!canceled.load(Ordering::Acquire));
            release.0.store(true, Ordering::Release);
            assert!(playback.shutdown_and_wait().all_workers_terminated());
        }

        #[test]
        fn origin_allows_contiguous_windows_and_unconstrained_nonzero_replacement() {
            let (output, _) = fake_output();
            let mut playback = AudioPlayback::with_output(test_config(), output).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            playback
                .prepare(
                    sample_position(0),
                    Arc::new(RecordingRenderer {
                        requests: Arc::clone(&requests),
                        wrong_frame_count: false,
                    }),
                    AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
                )
                .unwrap();
            poll_until_settled(&mut playback, sample_position(0));
            let generation = AudioPcmRenderGeneration::new(playback.generation);
            assert_eq!(
                requests
                    .lock()
                    .iter()
                    .map(|request| (request.start_sample, request.continuity))
                    .collect::<Vec<_>>(),
                vec![
                    (0, AudioPcmContinuity::Enter(generation)),
                    (10, AudioPcmContinuity::Continue(generation)),
                    (20, AudioPcmContinuity::Continue(generation)),
                ]
            );
            playback.validate_anchor(sample_position(40)).unwrap();
            playback.clear_source(sample_position(40)).unwrap();
            assert!(
                playback.renderer.is_none(),
                "clear must not enforce retired source origin"
            );
            playback
                .prepare(
                    sample_position(0),
                    Arc::new(RecordingRenderer { requests, wrong_frame_count: false }),
                    AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
                )
                .unwrap();
            playback
                .prepare(
                    sample_position(50),
                    Arc::new(RecordingRenderer {
                        requests: Arc::new(Mutex::new(Vec::new())),
                        wrong_frame_count: false,
                    }),
                    AudioPcmContinuityModel::GenerationState,
                )
                .unwrap();
            assert_eq!(playback.next_start_sample, 50);
            assert_eq!(
                playback.renderer.as_ref().unwrap().continuity_model,
                AudioPcmContinuityModel::GenerationState
            );
            let generic_requests = Arc::new(Mutex::new(Vec::new()));
            let generic_renderer: Arc<dyn AudioPcmRenderer> = Arc::new(RecordingRenderer {
                requests: Arc::clone(&generic_requests),
                wrong_frame_count: false,
            });
            playback
                .prepare(
                    sample_position(70),
                    Arc::clone(&generic_renderer),
                    AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(70)),
                )
                .unwrap();
            assert_eq!(
                playback.next_start_sample, 70,
                "Media enforces the supplied generic origin"
            );
            let generic_before = playback.snapshot(AudioPlaybackMode::Preroll);
            let generic_generation = playback.generation;
            assert_eq!(
                playback.prepare(
                    sample_position(80),
                    generic_renderer,
                    AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(70)),
                ),
                Err(AudioPlaybackError::FreshEntryOriginMismatch {
                    required: sample_position(70),
                    actual: sample_position(80),
                })
            );
            assert_eq!(
                playback.snapshot(AudioPlaybackMode::Preroll),
                generic_before
            );
            assert_eq!(playback.generation, generic_generation);
            poll_until_settled(&mut playback, sample_position(70));
            let generation = AudioPcmRenderGeneration::new(generic_generation);
            assert_eq!(
                generic_requests
                    .lock()
                    .iter()
                    .map(|request| (request.start_sample, request.continuity))
                    .collect::<Vec<_>>(),
                vec![
                    (70, AudioPcmContinuity::Enter(generation)),
                    (80, AudioPcmContinuity::Continue(generation)),
                    (90, AudioPcmContinuity::Continue(generation)),
                ]
            );
            assert!(playback.shutdown_and_wait().all_workers_terminated());
        }

        #[test]
        fn origin_preserves_hold_intent_and_rejects_only_the_actual_fresh_finish() {
            let (output, state) = fake_output();
            let mut playback = AudioPlayback::with_output(test_config(), output).unwrap();
            let installed = NativeInstallationProbe::new(false, false);
            playback
                .prepare(
                    sample_position(0),
                    installed.clone(),
                    AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
                )
                .unwrap();
            settle_native_installation(&mut playback);
            let claim = installed.verifier().current_claim().unwrap();
            for (policy, sample) in [
                (PcmRetirementPolicy::BeginNativeHold, 10),
                (PcmRetirementPolicy::PreserveNativeHold, 20),
            ] {
                playback
                    .reprime_prevalidated(
                        sample_position(sample),
                        false,
                        playback.renderer.clone(),
                        policy,
                    )
                    .unwrap();
                assert!(installed.verifier().validate(&claim).is_ok());
            }
            let root_plan = mondrian_core::AudioPreparedPlanId::new();
            let root_session = mondrian_core::AudioRenderSessionId::new();
            let target = mondrian_core::NativeAudioEditorTarget {
                root_plan,
                root_session,
                local_plan: root_plan,
                local_session: root_session,
                instance: mondrian_core::AudioProcessorInstanceId::new(),
                owner: mondrian_core::NativeAudioEditorOccurrenceOwner::Output(
                    mondrian_core::ProgramOutputId::new(),
                ),
                insertion: mondrian_core::NativeAudioEditorInsertion::PreFader,
                continuity_epoch: None,
                binding: mondrian_core::AudioProcessorBindingId::new(),
                installed_generation: claim.generation(),
            };
            let hold = NativeControlHold::AuthorStateEntry {
                target,
                phase: state_entry::AuthorStateEntryPhase::InstalledReady {
                    anchor: sample_position(0),
                },
            };
            playback.native_control_hold = Some(hold);
            let generation = playback.generation;
            playback.reprime(sample_position(30)).unwrap();
            assert_eq!(
                playback.next_start_sample, 30,
                "metadata reanchor is not Enter"
            );
            assert_eq!(playback.generation_render_anchor, Some(sample_position(30)));
            // Same Arc/model delegates to the same metadata-only hold path.
            playback
                .prepare(
                    sample_position(40),
                    installed.clone(),
                    AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
                )
                .unwrap();
            assert_eq!(playback.next_start_sample, 40);
            let before = playback.snapshot(AudioPlaybackMode::Idle);
            let quiescence = state.lock().quiescence_revision;
            assert_eq!(
                playback.finish_native_author_state_entry(target, sample_position(40)),
                Err(AudioPlaybackError::FreshEntryOriginMismatch {
                    required: sample_position(0),
                    actual: sample_position(40),
                })
            );
            assert_eq!(playback.snapshot(AudioPlaybackMode::Idle), before);
            assert_eq!(playback.native_control_hold, Some(hold));
            assert_eq!(playback.generation, generation);
            assert_eq!(state.lock().quiescence_revision, quiescence);
            assert!(installed.verifier().validate(&claim).is_ok());
            assert!(playback.finish_native_author_state_entry(target, sample_position(0)).unwrap());
            assert!(playback.native_control_hold.is_none());
            assert!(playback.shutdown_and_wait().all_workers_terminated());
        }

        #[test]
        fn mandatory_retirement_at_non_origin_quiesces_failure_and_idle() {
            let mismatch = AudioPlaybackError::FreshEntryOriginMismatch {
                required: sample_position(0),
                actual: sample_position(5),
            };
            // Row 1: a real worker failure must retire poisoned execution even
            // when a new Fresh Enter at the current authority is inadmissible.
            {
                let (output, state) = fake_output();
                let mut playback = AudioPlayback::with_output(test_config(), output).unwrap();
                let requests = Arc::new(Mutex::new(Vec::new()));
                playback
                    .prepare(
                        sample_position(0),
                        Arc::new(AlwaysFailStatefulRenderer { requests: Arc::clone(&requests) }),
                        AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
                    )
                    .unwrap();
                // Consume Opened at its legal origin, independently of failure.
                playback.poll(AudioPlaybackMode::Idle, sample_position(0)).unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                playback.poll(AudioPlaybackMode::Preroll, sample_position(0)).unwrap();
                let old_generation = playback.generation;
                let old_cancellation = playback.generation_cancellation.clone();
                let quiescence = state.lock().quiescence_revision;
                {
                    let mut output = state.lock();
                    output.queued_frames = 11;
                    let snapshot = output.snapshot.as_mut().unwrap();
                    snapshot.active = true;
                    snapshot.buffered_frames = 11;
                }
                let mut origin_refusal = None;
                while Instant::now() < deadline {
                    match playback.poll(AudioPlaybackMode::Consume, sample_position(5)) {
                        Err(error) => {
                            assert_eq!(
                                error, mismatch,
                                "no unrelated poll error may satisfy this control"
                            );
                            origin_refusal = Some(error);
                            break;
                        }
                        Ok(_)
                            if old_cancellation.is_canceled()
                                || playback.generation != old_generation =>
                        {
                            break
                        }
                        Ok(_) => thread::yield_now(),
                    }
                }
                assert!(
                    playback.render_generation_recovery_count > 0,
                    "actual failed completion was consumed"
                );
                assert!(
                    old_cancellation.is_canceled() || playback.generation != old_generation,
                    "mandatory failure retirement cannot be skipped by {origin_refusal:?}"
                );
                assert!(!state.lock().snapshot.unwrap().active);
                assert!(state.lock().quiescence_revision > quiescence);
                // Fake output acknowledges its deactivation token immediately.
                assert_eq!(state.lock().queued_frames, 0);
                while Instant::now() < deadline {
                    if let Err(error) = playback.poll(AudioPlaybackMode::Idle, sample_position(5)) {
                        assert_eq!(error, mismatch);
                    }
                    assert!(!state.lock().snapshot.unwrap().active);
                    assert_eq!(state.lock().queued_frames, 0);
                    if playback.in_flight == 0 && !playback.pcm_journal.old_work_unsettled() {
                        break;
                    }
                    thread::yield_now();
                }
                assert_eq!(playback.in_flight, 0);
                assert!(!playback.pcm_journal.old_work_unsettled());
                let enters = requests
                    .lock()
                    .iter()
                    .filter(|request| matches!(request.continuity, AudioPcmContinuity::Enter(_)))
                    .copied()
                    .collect::<Vec<_>>();
                assert_eq!(
                    enters.len(),
                    1,
                    "neither Enter5 nor a hidden replay of Enter0 is legal"
                );
                assert_eq!(enters[0].start_sample, 0);
                assert_eq!(enters[0].continuity.generation().get(), old_generation);
                assert_eq!(playback.render_substitution_count, 0);
                assert!(playback.shutdown_and_wait().all_workers_terminated());
            }

            // Row 2: Idle must quiesce an active output before a running old
            // render returns successful PCM. No candidate source is introduced.
            {
                let (output, state) = fake_output();
                let mut playback = AudioPlayback::with_output(test_config(), output).unwrap();
                let installed = NativeInstallationProbe::new(false, false);
                let entered = Arc::new(AtomicBool::new(false));
                let released = Arc::new(AtomicBool::new(false));
                let canceled = Arc::new(AtomicBool::new(false));
                // Reverse local drop order releases the worker even on RED panic.
                let release = NativeHoldRelease(Arc::clone(&released));
                playback
                    .prepare(
                        sample_position(0),
                        Arc::new(GatedInstalledRenderer {
                            installed: Arc::clone(&installed),
                            pcm: GateRenderer {
                                entered: Arc::clone(&entered),
                                released,
                                canceled: Arc::clone(&canceled),
                            },
                        }),
                        AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
                    )
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                while Instant::now() < deadline {
                    playback.poll(AudioPlaybackMode::Idle, sample_position(0)).unwrap();
                    if playback.native_owner_binding_status().is_some() {
                        break;
                    }
                    thread::yield_now();
                }
                assert!(playback.native_owner_binding_status().is_some());
                let claim = installed.verifier().current_claim().unwrap();
                assert!(installed.verifier().validate(&claim).is_ok());
                let bindings = installed.calls.lock().len();
                playback.poll(AudioPlaybackMode::Preroll, sample_position(0)).unwrap();
                while !entered.load(Ordering::Acquire) && Instant::now() < deadline {
                    thread::yield_now();
                }
                assert!(entered.load(Ordering::Acquire), "actual old PCM is running");
                let old_generation = playback.generation;
                let old_cancellation = playback.generation_cancellation.clone();
                let stale_before = playback.stale_completion_count;
                let quiescence = state.lock().quiescence_revision;
                {
                    let mut output = state.lock();
                    output.queued_frames = 11;
                    let snapshot = output.snapshot.as_mut().unwrap();
                    snapshot.active = true;
                    snapshot.buffered_frames = 11;
                }
                let idle = playback.poll(AudioPlaybackMode::Idle, sample_position(5));
                if let Err(error) = idle {
                    assert_eq!(error, mismatch);
                }
                assert!(
                    old_cancellation.is_canceled() || playback.generation != old_generation,
                    "Idle must retire old work even if a new Enter5 is refused"
                );
                assert!(!state.lock().snapshot.unwrap().active);
                assert!(state.lock().quiescence_revision > quiescence);
                assert_eq!(state.lock().queued_frames, 0);
                // GateRenderer observes cancellation and still returns success:
                // this is a real late rendered buffer, never an injected packet.
                while Instant::now() < deadline {
                    if let Err(error) = playback.poll(AudioPlaybackMode::Idle, sample_position(5)) {
                        assert_eq!(error, mismatch);
                    }
                    assert!(!state.lock().snapshot.unwrap().active);
                    assert_eq!(state.lock().queued_frames, 0, "retired PCM cannot enqueue");
                    if canceled.load(Ordering::Acquire)
                        && playback.stale_completion_count > stale_before
                    {
                        break;
                    }
                    thread::yield_now();
                }
                assert!(canceled.load(Ordering::Acquire));
                assert!(playback.stale_completion_count > stale_before);
                assert_eq!(
                    installed.calls.lock().len(),
                    bindings,
                    "no duplicate native binding"
                );
                release.0.store(true, Ordering::Release);
                assert!(playback.shutdown_and_wait().all_workers_terminated());
            }
        }
    }

    #[test]
    fn residual_retirement_owner_forces_top_level_receipt_unresolved() {
        let (output, _) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        let dropped = Arc::new(AtomicBool::new(false));
        playback.renderer = Some(PreparedAudioPcmRenderer {
            native_owner_binding_supported: false,
            native_observations_supported: false,
            materialization_id: mondrian_core::AudioPcmRendererMaterializationId::new(),
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
            native_owner_binding_supported: false,
            native_observations_supported: false,
            materialization_id: mondrian_core::AudioPcmRendererMaterializationId::new(),
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
        poll_until_settled(&mut playback, sample_position(0));
        assert_eq!(
            playback.quiescence_token.map(|token| token.revision),
            Some(1)
        );
        assert_eq!(playback.stream_media_anchor, Some((4, sample_position(0))));

        let generation = playback.generation;
        let next_start_sample = playback.next_start_sample;
        let queued_frames = state.lock().queued_frames;

        let final_snapshot = {
            let mut output = state.lock();
            // Models the device worker's deactivation becoming visible before
            // CPAL stream destruction can publish its frozen Lost event.
            output.quiescence_revision = 2;
            output.confirmed_quiescence_revision = 2;
            output.snapshot.as_mut().expect("concrete output").active = false;
            output.snapshot.expect("concrete output")
        };
        let waiting = playback
            .poll(AudioPlaybackMode::Consume, sample_position(5_000))
            .expect("superseded quiescence waits for lifecycle evidence");
        assert_eq!(playback.generation, generation);
        assert_eq!(playback.next_start_sample, next_start_sample);
        assert_eq!(state.lock().queued_frames, queued_frames);
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
    fn hidden_preroll_activation_includes_the_remaining_device_delay() {
        let (output, state) = fake_output();
        {
            let mut output = state.lock();
            let snapshot = output.snapshot.as_mut().expect("output snapshot");
            snapshot.callback_count = 1;
            snapshot.last_callback_playback_delay = Some(Duration::from_millis(20));
            snapshot.last_callback_age = Some(Duration::from_millis(5));
        }
        let mut playback = AudioPlayback::with_output(test_config(), output)
            .expect("production Audio Playback owner");
        playback.generation_render_anchor = Some(sample_position(0));
        let preflight = playback
            .validate_poll_arithmetic(AudioPlaybackMode::Consume, sample_position(5))
            .expect("bounded activation preflight");
        assert_eq!(
            preflight.elapsed_skip_frames,
            Some(20),
            "the first audible sample must include the remaining 15 ms device delay"
        );
        assert_eq!(preflight.activation_authority, Some(sample_position(20)));
        let frozen = playback
            .validate_poll_arithmetic(AudioPlaybackMode::Preroll, sample_position(5))
            .expect("frozen preroll");
        assert_eq!(frozen.elapsed_skip_frames, Some(5));
        assert_eq!(
            playback
                .validate_poll_arithmetic(AudioPlaybackMode::Consume, sample_position(i64::MAX)),
            Err(AudioPlaybackError::CoordinateOverflow)
        );
        state.lock().snapshot.as_mut().expect("output snapshot").last_callback_age = None;
        assert!(playback
            .validate_poll_arithmetic(AudioPlaybackMode::Consume, sample_position(5))
            .expect("unknown timing may fill but not activate")
            .activation_authority
            .is_none());
    }

    #[test]
    fn hidden_preroll_catch_up_uses_physical_capacity_and_negative_delta_waits() {
        let (output, _) = fake_output();
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("spawn test render worker");
        playback.generation_render_anchor = Some(sample_position(0));

        let exact_boundary = playback
            .validate_poll_arithmetic(AudioPlaybackMode::Preroll, sample_position(1_980))
            .expect("skip plus preroll exactly fits two-second queue");
        assert_eq!(exact_boundary.elapsed_skip_frames, Some(1_980));
        assert_eq!(exact_boundary.admission_target_frames, 2_000);
        assert_eq!(
            playback.validate_poll_arithmetic(AudioPlaybackMode::Preroll, sample_position(1_981)),
            Err(AudioPlaybackError::HiddenPrerollExceedsOutputCapacity {
                skip_frames: 1_981,
                preroll_frames: 20,
                capacity_frames: 2_000,
            })
        );

        playback.generation_render_anchor = Some(sample_position(10));
        let waiting = playback
            .validate_poll_arithmetic(AudioPlaybackMode::Preroll, sample_position(5))
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

    struct NativeHoldDeferredRenderer {
        entered: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<AudioPcmRenderRequest>>>,
        notice: mondrian_core::NativeAudioReprepareNotice,
    }

    impl AudioPcmRenderer for NativeHoldDeferredRenderer {
        fn bind_native_child_settlement(
            &self,
            registrar: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            // This portable test adapter owns only the host fields declared here.
            let _ = registrar;
            Ok(())
        }
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            self.requests.lock().push(request);
            self.entered.store(true, Ordering::Release);
            while !self.release.load(Ordering::Acquire) && !cancellation.is_canceled() {
                thread::yield_now();
            }
            Ok(AudioPcmRenderOutcome::DeferredForReprepare(self.notice))
        }
    }

    struct NativeHoldRelease(Arc<AtomicBool>);
    impl Drop for NativeHoldRelease {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    struct NativeHoldPlaybackFixture {
        playback: AudioPlayback,
        output_state: Arc<Mutex<FakeOutputState>>,
        requests: Arc<Mutex<Vec<AudioPcmRenderRequest>>>,
        retained_renderer: std::sync::Weak<dyn AudioPcmRenderer>,
    }

    fn native_hold_pending_playback() -> NativeHoldPlaybackFixture {
        let (output, state) = fake_output();
        state.lock().events.clear();
        let entered = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let renderer: Arc<dyn AudioPcmRenderer> = Arc::new(NativeHoldDeferredRenderer {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
            requests: Arc::clone(&requests),
            notice: mondrian_core::NativeAudioReprepareNotice {
                instance_id: mondrian_core::AudioProcessorInstanceId::new(),
                request: mondrian_core::NativeAudioReprepareRequest::ClapRestart,
            },
        });
        let retained = Arc::downgrade(&renderer);
        let mut playback =
            AudioPlayback::with_output(test_config(), output).expect("serial render worker");
        // Declared after playback: panic unwinding releases the worker before
        // AudioPlayback waits for termination (reverse local drop order).
        let release_guard = NativeHoldRelease(Arc::clone(&release));
        playback
            .prepare(
                sample_position(0),
                renderer,
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("source");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !entered.load(Ordering::Acquire) && Instant::now() < deadline {
            playback
                .poll(AudioPlaybackMode::Consume, sample_position(0))
                .expect("enter worker");
            thread::yield_now();
        }
        assert!(
            entered.load(Ordering::Acquire),
            "actual render worker did not enter"
        );
        // Existing output-adapter oracle represents a currently active stream.
        // The tested token protocol is actual Media code; this is no DAC proof.
        {
            let mut output = state.lock();
            output.auto_confirm_quiescence = false;
            let snapshot = output.snapshot.as_mut().expect("output");
            snapshot.active = true;
            snapshot.active_callback_consumed_frames = 10;
            snapshot.callback_consumed_frames = 10;
            snapshot.captured_at = Instant::now();
        }
        playback.media_anchor = Some(sample_position(0));
        release_guard.0.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        while (playback.native_control_hold.is_none() || playback.pcm_journal.old_work_unsettled())
            && Instant::now() < deadline
        {
            let poll = playback
                .poll(AudioPlaybackMode::Consume, sample_position(0))
                .expect("deferred completion");
            events.extend(poll.events);
            thread::yield_now();
        }
        assert!(
            playback.native_control_hold.is_some(),
            "typed deferred result not consumed"
        );
        assert!(
            !playback.pcm_journal.old_work_unsettled(),
            "old PCM completions must settle before output-token controls"
        );
        assert!(events.iter().any(
            |event| matches!(event, AudioPlaybackEvent::NativeReprepareRequired {
            final_output: Some(output), .. } if output.active_callback_consumed_frames == 10)
        ));
        assert!(!events.iter().any(|event| matches!(
            event,
            AudioPlaybackEvent::NativeReprepareOutputQuiesced { .. }
        )));
        drop(release_guard);
        NativeHoldPlaybackFixture {
            playback,
            output_state: state,
            requests,
            retained_renderer: retained,
        }
    }

    #[test]
    fn native_reprepare_output_quiescence_waits_for_exact_token_then_captures_final_callback() {
        let NativeHoldPlaybackFixture {
            mut playback,
            output_state: state,
            requests,
            retained_renderer: retained,
        } = native_hold_pending_playback();
        let generation = playback.generation;
        assert!(!playback
            .finalize_native_control_anchor(generation, sample_position(20))
            .expect("early proof"));
        {
            let mut output = state.lock();
            let snapshot = output.snapshot.as_mut().expect("late callback");
            snapshot.active_callback_consumed_frames = 20;
            snapshot.callback_consumed_frames = 20;
            snapshot.captured_at = Instant::now();
        }
        for _ in 0..4 {
            let poll = playback
                .poll(AudioPlaybackMode::Preroll, sample_position(0))
                .expect("still waiting");
            assert_eq!(poll.snapshot.state, AudioPlaybackState::RepreparePending);
            assert!(!poll.events.iter().any(|event| matches!(
                event,
                AudioPlaybackEvent::NativeReprepareOutputQuiesced { .. }
            )));
        }
        {
            let mut output = state.lock();
            output.confirmed_quiescence_revision = output.quiescence_revision;
        }
        let poll = playback
            .poll(AudioPlaybackMode::Preroll, sample_position(0))
            .expect("exact acknowledgement");
        let finals = poll
            .events
            .iter()
            .filter_map(|event| match event {
                AudioPlaybackEvent::NativeReprepareOutputQuiesced {
                    pending_generation,
                    final_output,
                    final_media_anchor,
                } => Some((*pending_generation, *final_output, *final_media_anchor)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(finals.len(), 1);
        assert_eq!(finals[0].0, generation);
        assert_eq!(
            finals[0].1.expect("post-quiescence snapshot").active_callback_consumed_frames,
            20
        );
        assert_eq!(finals[0].2, Some(sample_position(0)));
        assert!(!playback
            .finalize_native_control_anchor(generation - 1, sample_position(20))
            .expect("stale generation"));
        assert!(playback
            .finalize_native_control_anchor(generation, sample_position(20))
            .expect("same generation held anchor"));
        assert!(!playback
            .finalize_native_control_anchor(generation, sample_position(90))
            .expect("same retirement cannot rewrite anchor twice"));
        assert_eq!(playback.next_start_sample, 20);
        assert_eq!(playback.generation_render_anchor, Some(sample_position(20)));
        for _ in 0..4 {
            let poll = playback
                .poll(AudioPlaybackMode::Preroll, sample_position(20))
                .expect("held source");
            assert_eq!(poll.snapshot.state, AudioPlaybackState::RepreparePending);
            assert_eq!(poll.snapshot.render_substitution_count, 0);
            assert_eq!(poll.snapshot.render_generation_recovery_count, 0);
            assert!(!poll.events.iter().any(|event| matches!(
                event,
                AudioPlaybackEvent::NativeReprepareOutputQuiesced { .. }
            )));
        }
        assert!(retained.upgrade().is_some());
        assert_eq!(state.lock().queued_frames, 0);
        assert!(requests
            .lock()
            .iter()
            .all(|request| request.continuity.generation().get() != generation));
        assert_eq!(playback.consecutive_render_generation_failures, 0);
    }

    #[test]
    fn native_reprepare_output_quiescence_user_reprime_retires_old_anchor_and_pending_generation() {
        let NativeHoldPlaybackFixture {
            mut playback,
            output_state: state,
            requests,
            retained_renderer: _,
        } = native_hold_pending_playback();
        let old = playback.generation;
        playback
            .reprime(sample_position(170))
            .expect("explicit user seek while pending");
        let current = playback.generation;
        assert_eq!(current, old + 1);
        {
            let mut output = state.lock();
            output.confirmed_quiescence_revision = output.quiescence_revision;
        }
        let poll = playback
            .poll(AudioPlaybackMode::Preroll, sample_position(170))
            .expect("new generation acknowledgement");
        assert!(poll.events.iter().any(
            |event| matches!(event, AudioPlaybackEvent::NativeReprepareOutputQuiesced {
            pending_generation, final_media_anchor: None, .. } if *pending_generation == current)
        ));
        assert!(!playback
            .finalize_native_control_anchor(old, sample_position(20))
            .expect("old final anchor cannot overwrite seek"));
        assert_eq!(playback.next_start_sample, 170);
        assert!(playback
            .finalize_native_control_anchor(current, sample_position(170))
            .expect("new held seek anchor"));
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Preroll).state,
            AudioPlaybackState::RepreparePending
        );
        assert!(requests
            .lock()
            .iter()
            .all(|request| request.continuity.generation().get() != current));
    }

    #[test]
    fn native_reprepare_output_quiescence_device_loss_keeps_pending_identity_and_frozen_authority()
    {
        let NativeHoldPlaybackFixture {
            mut playback,
            output_state: state,
            requests,
            retained_renderer: retained,
        } = native_hold_pending_playback();
        let old = playback.generation;
        {
            let mut output = state.lock();
            let mut final_snapshot =
                output.snapshot.take().expect("destroyed stream's final evidence");
            final_snapshot.active_callback_consumed_frames = 20;
            final_snapshot.callback_consumed_frames = 20;
            output.events.push_back(RealtimeAudioOutputEvent::Lost {
                reason: RealtimeAudioOutputLossReason::ControlledRecycle,
                final_snapshot,
            });
        }
        let poll = playback
            .poll(AudioPlaybackMode::Preroll, sample_position(170))
            .expect("actual loss consumer");
        let current = poll.snapshot.generation;
        assert_eq!(current, old + 1);
        assert_eq!(poll.snapshot.state, AudioPlaybackState::DeviceUnavailable);
        assert_eq!(playback.native_control_hold_generation(), Some(current));
        assert!(poll.events.iter().any(
            |event| matches!(event, AudioPlaybackEvent::NativeReprepareOutputQuiesced {
            pending_generation, final_output: None, final_media_anchor: None,
        } if *pending_generation == current)
        ));
        assert!(playback
            .finalize_native_control_anchor(current, sample_position(170))
            .expect("unchanged accepted authority"));
        assert_eq!(playback.next_start_sample, 170);
        assert_eq!(playback.consecutive_render_generation_failures, 0);
        assert!(retained.upgrade().is_some());
        assert!(requests
            .lock()
            .iter()
            .all(|request| request.continuity.generation().get() != current));
    }

    struct CompleteThenNativeDeferredRenderer {
        first: Mutex<Option<AudioBuffer>>,
        notice: mondrian_core::NativeAudioReprepareNotice,
    }

    impl AudioPcmRenderer for CompleteThenNativeDeferredRenderer {
        fn bind_native_child_settlement(
            &self,
            registrar: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            // This portable test adapter owns only the host fields declared here.
            let _ = registrar;
            Ok(())
        }
        fn render(
            &self,
            _request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            match self.first.lock().take() {
                Some(buffer) => Ok(AudioPcmRenderOutcome::Rendered(buffer)),
                None => Ok(AudioPcmRenderOutcome::DeferredForReprepare(self.notice)),
            }
        }
    }

    fn complete_pcm_native_hold_fixture() -> (
        AudioPlayback,
        crate::pcm_allocation_probe::BackingOwnerProbe,
    ) {
        let config = test_config();
        let samples_len = config.chunk_frames * config.channel_layout.channel_count();
        let mut samples = Vec::with_capacity(samples_len + 17);
        samples.extend((0..samples_len).map(|sample| sample as f32 + 0.25));
        let backing = crate::pcm_allocation_probe::BackingOwnerProbe::track(samples.as_ptr());
        let renderer: Arc<dyn AudioPcmRenderer> = Arc::new(CompleteThenNativeDeferredRenderer {
            first: Mutex::new(Some(AudioBuffer {
                samples,
                sample_rate: config.sample_rate,
                channel_layout: config.channel_layout,
            })),
            notice: mondrian_core::NativeAudioReprepareNotice {
                instance_id: mondrian_core::AudioProcessorInstanceId::new(),
                request: mondrian_core::NativeAudioReprepareRequest::ClapRestart,
            },
        });
        let (output, state) = fake_output();
        state.lock().events.clear();
        let mut playback =
            AudioPlayback::with_output(config, output).expect("actual serial Media owner");
        playback
            .prepare(
                sample_position(0),
                renderer,
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("actual prepare");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let poll = playback
                .poll(AudioPlaybackMode::Consume, sample_position(0))
                .expect("actual complete/deferred admission");
            if poll.snapshot.state == AudioPlaybackState::RepreparePending {
                return (playback, backing);
            }
            thread::yield_now();
        }
        panic!("actual complete PCM followed by native deferral never reached hold");
    }

    #[test]
    fn native_retained_pcm_existing_owner_survives_output_clear_until_source_retirement() {
        let (mut playback, backing) = complete_pcm_native_hold_fixture();
        // Existing API oracle: actual f32 backing must still have one owner after
        // enqueue and native output clear. No new journal/hold method is called.
        assert_eq!(
            backing.dropped(),
            0,
            "complete generated PCM was discarded while its DSP owner is retained"
        );
        playback.clear_source(sample_position(0)).expect("explicit source retirement");
        assert_eq!(
            backing.dropped(),
            1,
            "explicit retirement releases the original retained PCM backing"
        );
    }

    #[test]
    fn native_retained_pcm_existing_explicit_seek_retires_the_original_complete_owner() {
        let (mut playback, backing) = complete_pcm_native_hold_fixture();
        assert_eq!(
            backing.dropped(),
            0,
            "native hold lost its original complete PCM owner"
        );
        playback.reprime(sample_position(100)).expect("explicit fresh seek intent");
        assert_eq!(
            backing.dropped(),
            1,
            "fresh seek retires old continuous PCM rather than reusing it"
        );
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).state,
            AudioPlaybackState::RepreparePending
        );
    }

    struct NativeInstallationProbe {
        verifier: Mutex<Option<AudioOwnerInstallationVerifier>>,
        calls: Mutex<Vec<(thread::ThreadId, AudioOwnerInstallationClaim)>>,
        entered: AtomicBool,
        release: AtomicBool,
        rejected: bool,
        gated: bool,
    }

    impl NativeInstallationProbe {
        fn new(rejected: bool, gated: bool) -> Arc<Self> {
            Arc::new(Self {
                verifier: Mutex::new(None),
                calls: Mutex::new(Vec::new()),
                entered: AtomicBool::new(false),
                release: AtomicBool::new(false),
                rejected,
                gated,
            })
        }
        fn verifier(&self) -> AudioOwnerInstallationVerifier {
            self.verifier.lock().clone().expect("actual registered Runtime")
        }
    }

    impl AudioPcmRenderer for NativeInstallationProbe {
        fn bind_native_child_settlement(
            &self,
            registrar: &mondrian_core::NativeChildSettlementRegistrar,
        ) -> Result<(), mondrian_core::NativeChildSettlementError> {
            // This portable test adapter owns only the host fields declared here.
            let _ = registrar;
            Ok(())
        }
        fn owner_installation_verifier(&self) -> Option<AudioOwnerInstallationVerifier> {
            self.verifier.lock().clone()
        }
        fn bind_owner_installation(
            &self,
            verifier: AudioOwnerInstallationVerifier,
        ) -> Result<bool, AudioOwnerInstallationError> {
            assert!(
                matches!(
                    verifier.current_claim(),
                    Err(AudioOwnerInstallationError::NotInstalled
                        | AudioOwnerInstallationError::Superseded)
                ),
                "metadata registration must precede actual slot installation"
            );
            let mut current = self.verifier.lock();
            if let Some(current) = current.as_ref() {
                if !current.same_registration(&verifier) {
                    return Err(AudioOwnerInstallationError::WrongRegistration);
                }
            } else {
                *current = Some(verifier);
            }
            Ok(true)
        }
        fn install_native_owner_bindings(
            &self,
            claim: &AudioOwnerInstallationClaim,
            cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmNativeOwnerBindingStatus> {
            self.entered.store(true, Ordering::Release);
            if self.gated {
                let deadline = Instant::now() + Duration::from_secs(2);
                while !self.release.load(Ordering::Acquire)
                    && !cancellation.is_canceled()
                    && Instant::now() < deadline
                {
                    thread::yield_now();
                }
            }
            if cancellation.is_canceled() {
                return Ok(AudioPcmNativeOwnerBindingStatus::Unavailable(
                    AudioPcmNativeOwnerBindingRefusal::Cancelled,
                ));
            }
            if self.verifier().validate(claim).is_err() {
                return Ok(AudioPcmNativeOwnerBindingStatus::Unavailable(
                    AudioPcmNativeOwnerBindingRefusal::Superseded,
                ));
            }
            self.calls.lock().push((thread::current().id(), claim.clone()));
            Ok(if self.rejected {
                AudioPcmNativeOwnerBindingStatus::Unavailable(
                    AudioPcmNativeOwnerBindingRefusal::ProcessorRejected,
                )
            } else {
                AudioPcmNativeOwnerBindingStatus::Ready
            })
        }
        fn render(
            &self,
            request: AudioPcmRenderRequest,
            _cancellation: &ExecutionCancellationToken,
        ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
            Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                request.sample_rate,
                request.channel_layout,
                request.frame_count,
            )))
        }
    }

    fn settle_native_installation(playback: &mut AudioPlayback) -> Vec<AudioPlaybackEvent> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            events.extend(
                playback
                    .poll(AudioPlaybackMode::Idle, sample_position(0))
                    .expect("paused actual owner poll")
                    .events,
            );
            if playback.native_owner_binding_status().is_some() {
                return events;
            }
            thread::yield_now();
        }
        panic!("actual serial native binding work never completed");
    }

    #[test]
    fn native_installation_actual_paused_slot_runs_binding_on_sole_serial_worker() {
        let (mut playback, _) = playback_with_buffered_frames(0);
        let renderer = NativeInstallationProbe::new(false, false);
        playback
            .prepare(
                sample_position(0),
                renderer.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("actual Media slot exchange");
        let claim = renderer
            .verifier()
            .current_claim()
            .expect("actual installed slot, independent of queued readiness");
        let events = settle_native_installation(&mut playback);
        let calls = renderer.calls.lock();
        assert_eq!(calls.len(), 1);
        assert_ne!(
            calls[0].0,
            thread::current().id(),
            "native calls belong to the existing serial worker"
        );
        assert_eq!(calls[0].1.generation(), claim.generation());
        assert!(renderer.verifier().validate(&calls[0].1).is_ok());
        assert!(events.iter().any(|event| matches!(
            event,
            AudioPlaybackEvent::NativeOwnerBindingsObserved {
                status: AudioPcmNativeOwnerBindingStatus::Ready,
                ..
            }
        )));
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Idle).in_flight,
            0,
            "paused editor preparation must not manufacture PCM work"
        );
    }

    #[test]
    fn native_installation_failed_actual_swap_preserves_previous_claim_and_renderer() {
        let (mut playback, output) = playback_with_buffered_frames(0);
        let before = NativeInstallationProbe::new(false, false);
        playback
            .prepare(
                sample_position(0),
                before.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("old actual slot");
        settle_native_installation(&mut playback);
        let claim = before.verifier().current_claim().expect("old installed slot");
        let generation = playback.generation;
        let renderer_id = playback.renderer.as_ref().expect("old renderer").materialization_id;
        let after = NativeInstallationProbe::new(false, false);
        {
            let mut state = output.lock();
            state.snapshot.as_mut().expect("actual fake device").active = true;
            state.quiescence_revision = u64::MAX;
        }
        assert!(matches!(
            playback.prepare(
                sample_position(0),
                after.clone(),
                AudioPcmContinuityModel::GenerationState
            ),
            Err(AudioPlaybackError::OutputControl(
                RealtimeAudioOutputControlError::QuiescenceRevisionExhausted { .. }
            ))
        ));
        assert_eq!(playback.generation, generation);
        assert_eq!(
            playback.renderer.as_ref().expect("same old renderer").materialization_id,
            renderer_id
        );
        assert!(
            before.verifier().validate(&claim).is_ok(),
            "failed swap cannot revoke the still-current actual renderer"
        );
        assert!(
            after.verifier.lock().is_none(),
            "output preflight refuses before metadata registration"
        );
        output.lock().quiescence_revision = 0;
    }

    #[test]
    fn native_installation_wrong_media_source_cannot_rebind_an_actual_runtime() {
        let (mut first, _) = playback_with_buffered_frames(0);
        let (mut second, _) = playback_with_buffered_frames(0);
        let renderer = NativeInstallationProbe::new(false, false);
        first
            .prepare(
                sample_position(0),
                renderer.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("actual first owner");
        settle_native_installation(&mut first);
        let claim =
            renderer.verifier().current_claim().expect("bound to the first concrete source");
        assert!(matches!(
            second.prepare(
                sample_position(0),
                renderer.clone(),
                AudioPcmContinuityModel::GenerationState
            ),
            Err(AudioPlaybackError::OwnerInstallation(
                AudioOwnerInstallationError::WrongRegistration
            ))
        ));
        assert!(second.renderer.is_none());
        assert!(renderer.verifier().validate(&claim).is_ok());
    }

    #[test]
    fn native_installation_refused_binding_keeps_valid_pcm_graph_without_editor_permission() {
        let (mut playback, _) = playback_with_buffered_frames(0);
        let renderer = NativeInstallationProbe::new(true, false);
        playback
            .prepare(
                sample_position(0),
                renderer.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("valid actual DSP graph");
        settle_native_installation(&mut playback);
        assert_eq!(
            playback.native_owner_binding_status(),
            Some(AudioPcmNativeOwnerBindingStatus::Unavailable(
                AudioPcmNativeOwnerBindingRefusal::ProcessorRejected
            ))
        );
        assert!(playback.renderer.is_some());
        poll_until_settled(&mut playback, sample_position(0));
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).render_generation_recovery_count,
            0
        );
        assert_eq!(
            playback.snapshot(AudioPlaybackMode::Consume).render_substitution_count,
            0
        );
    }

    #[test]
    fn native_binding_in_flight_survives_pcm_suspend_without_rebinding() {
        struct ReleaseBinding(Arc<NativeInstallationProbe>);
        impl Drop for ReleaseBinding {
            fn drop(&mut self) {
                self.0.release.store(true, Ordering::Release);
            }
        }
        let (mut playback, output) = playback_with_buffered_frames(0);
        let renderer = NativeInstallationProbe::new(false, true);
        let release = ReleaseBinding(Arc::clone(&renderer));
        let deadline = Instant::now() + Duration::from_secs(2);
        playback
            .prepare(
                sample_position(0),
                renderer.clone(),
                AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
            )
            .unwrap();
        let verifier = renderer.verifier();
        let claim = verifier.current_claim().unwrap();
        let materialization = playback.renderer.as_ref().unwrap().materialization_id;
        let old_generation = playback.generation;
        let old_pcm = playback.generation_cancellation.clone();
        let old_native = playback.native_control_cancellation.clone();
        while !renderer.entered.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            renderer.entered.load(Ordering::Acquire),
            "real native binding callback is in flight"
        );
        {
            let mut state = output.lock();
            state.queued_frames = 11;
            let snapshot = state.snapshot.as_mut().unwrap();
            snapshot.active = true;
            snapshot.buffered_frames = 11;
        }
        let idle = playback.poll(AudioPlaybackMode::Idle, sample_position(5));
        if let Err(error) = idle {
            assert_eq!(
                error,
                AudioPlaybackError::FreshEntryOriginMismatch {
                    required: sample_position(0),
                    actual: sample_position(5),
                }
            );
        }
        assert!(old_pcm.is_canceled());
        assert_ne!(playback.generation, old_generation);
        assert!(!old_native.is_canceled());
        assert!(!playback.native_control_cancellation.is_canceled());
        assert!(verifier.validate(&claim).is_ok());
        assert_eq!(
            playback.owner_installation.current_claim().unwrap().unwrap().generation(),
            claim.generation()
        );
        assert_eq!(
            playback.renderer.as_ref().unwrap().materialization_id,
            materialization
        );
        assert_eq!(playback.generation_render_anchor, None);
        assert!(playback.render_blocked);
        assert!(!output.lock().snapshot.unwrap().active);
        assert_eq!(output.lock().queued_frames, 0);
        release.0.release.store(true, Ordering::Release);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            events
                .extend(playback.poll(AudioPlaybackMode::Idle, sample_position(5)).unwrap().events);
            if playback.native_owner_binding_status()
                == Some(AudioPcmNativeOwnerBindingStatus::Ready)
            {
                break;
            }
            thread::yield_now();
        }
        assert_eq!(
            playback.native_owner_binding_status(),
            Some(AudioPcmNativeOwnerBindingStatus::Ready)
        );
        assert!(events.iter().any(|event| matches!(event,
            AudioPlaybackEvent::NativeOwnerBindingsObserved {
                generation, status: AudioPcmNativeOwnerBindingStatus::Ready, ..
            } if *generation == old_generation
        )));
        let calls = renderer.calls.lock();
        assert_eq!(
            calls.len(),
            1,
            "same callback completes; no replacement binding is queued"
        );
        assert_eq!(calls[0].1.generation(), claim.generation());
        drop(calls);
        assert!(verifier.validate(&claim).is_ok());
        assert_eq!(
            playback.renderer.as_ref().unwrap().materialization_id,
            materialization
        );
        assert_eq!(playback.in_flight, 0);
        assert!(playback.render_blocked);
        assert_eq!(playback.generation_render_anchor, None);
        assert_eq!(output.lock().queued_frames, 0);
        assert!(!output.lock().snapshot.unwrap().active);
        // Identity witness only after the real callback has completed: cancelling
        // the captured clone must affect the retained token, not a new token.
        old_native.cancel();
        assert!(playback.native_control_cancellation.is_canceled());
        assert!(playback.shutdown_and_wait().all_workers_terminated());
    }

    #[test]
    fn native_installation_late_old_worker_completion_cannot_publish_into_replacement_slot() {
        let (mut playback, _) = playback_with_buffered_frames(0);
        let before = NativeInstallationProbe::new(false, true);
        playback
            .prepare(
                sample_position(0),
                before.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("first actual slot");
        let old_claim = before.verifier().current_claim().expect("first claim");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !before.entered.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            before.entered.load(Ordering::Acquire),
            "actual binding callback entered"
        );
        let after = NativeInstallationProbe::new(false, false);
        playback
            .prepare(
                sample_position(0),
                after.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("actual replacement while old work is queued/running");
        before.release.store(true, Ordering::Release);
        let events = settle_native_installation(&mut playback);
        assert!(
            before.calls.lock().is_empty(),
            "canceled old work may not acquire a current binding"
        );
        assert_eq!(
            before.verifier().validate(&old_claim),
            Err(AudioOwnerInstallationError::Superseded)
        );
        assert_eq!(after.calls.lock().len(), 1);
        assert!(events.iter().all(|event| !matches!(event, AudioPlaybackEvent::NativeOwnerBindingsObserved { generation, .. } if *generation < playback.generation)));
        assert!(
            playback.stale_completion_count > 0,
            "old completion is retired through the existing generation gate"
        );
    }

    #[test]
    fn native_installation_same_arc_reprime_reads_fresh_runtime_registration_and_clear_revokes() {
        let (mut playback, _) = playback_with_buffered_frames(0);
        let renderer = NativeInstallationProbe::new(false, false);
        playback
            .prepare(
                sample_position(0),
                renderer.clone(),
                AudioPcmContinuityModel::GenerationState,
            )
            .expect("actual initial slot");
        settle_native_installation(&mut playback);
        let old_verifier = renderer.verifier();
        let old_claim = old_verifier.current_claim().expect("old current Runtime");
        // This probe models a proven same-Arc Runtime replacement. Only the
        // actual Media source supplies the next registration, never a foreign
        // source or caller-authored owner ID. The public renderer getter must
        // be re-read rather than its old wrapper registration reused.
        let (_, fresh) = playback.owner_installation.register().expect("same actual slot source");
        *renderer.verifier.lock() = Some(fresh.clone());
        playback
            .reprime(sample_position(0))
            .expect("actual same-Arc slot generation swap");
        settle_native_installation(&mut playback);
        assert_eq!(
            old_verifier.validate(&old_claim),
            Err(AudioOwnerInstallationError::Superseded)
        );
        let current =
            fresh.current_claim().expect("only the actual replacement Runtime is current");
        assert_eq!(
            renderer.calls.lock().last().expect("fresh callback").1.generation(),
            current.generation()
        );
        playback.clear_source(sample_position(0)).expect("actual clear exchange");
        assert_eq!(
            fresh.validate(&current),
            Err(AudioOwnerInstallationError::NotInstalled)
        );
    }

    #[cfg(test)]
    mod native_editor_media_contracts {
        use super::*;
        use mondrian_core::{
            NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
            NativeAudioEditorControlRefusal as Refusal,
            NativeAudioEditorControlSubmission as Submission, NativeAudioEditorTarget as Target,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Explicit portable renderer adapter. Its control evidence tests the actual
        // Media queue/claim/hold boundary; it makes no native HWND/SDK/author claim.
        struct CleanupRenderer {
            installed: Arc<NativeInstallationProbe>,
            target: Mutex<Option<Target>>,
            cleanup: AtomicBool,
            closes: AtomicUsize,
            renders: AtomicUsize,
            control_threads: Mutex<Vec<thread::ThreadId>>,
            gate_pcm: AtomicBool,
            pcm_entered: AtomicBool,
            entered: AtomicBool,
            release: AtomicBool,
            gated: bool,
        }
        impl CleanupRenderer {
            fn new(gated: bool) -> Arc<Self> {
                Arc::new(Self {
                    installed: NativeInstallationProbe::new(false, false),
                    target: Mutex::new(None),
                    cleanup: AtomicBool::new(false),
                    closes: AtomicUsize::new(0),
                    renders: AtomicUsize::new(0),
                    control_threads: Mutex::new(Vec::new()),
                    gate_pcm: AtomicBool::new(false),
                    pcm_entered: AtomicBool::new(false),
                    entered: AtomicBool::new(false),
                    release: AtomicBool::new(false),
                    gated,
                })
            }
            fn publish_fixture_address(&self) -> Target {
                let claim =
                    self.installed.verifier().current_claim().expect("actual current Media slot");
                let root_plan = mondrian_core::AudioPreparedPlanId::new();
                let root_session = mondrian_core::AudioRenderSessionId::new();
                let target = Target {
                    root_plan,
                    root_session,
                    local_plan: root_plan,
                    local_session: root_session,
                    instance: mondrian_core::AudioProcessorInstanceId::new(),
                    owner: mondrian_core::NativeAudioEditorOccurrenceOwner::Output(
                        mondrian_core::ProgramOutputId::new(),
                    ),
                    insertion: mondrian_core::NativeAudioEditorInsertion::PreFader,
                    continuity_epoch: None,
                    binding: mondrian_core::AudioProcessorBindingId::new(),
                    installed_generation: claim.generation(),
                };
                *self.target.lock() = Some(target);
                target
            }
        }
        impl AudioPcmRenderer for CleanupRenderer {
            fn bind_native_child_settlement(
                &self,
                registrar: &mondrian_core::NativeChildSettlementRegistrar,
            ) -> Result<(), mondrian_core::NativeChildSettlementError> {
                self.installed.bind_native_child_settlement(registrar)
            }
            fn validate_native_author_source_closed(
                &self,
                cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<()> {
                // Portable queue model only: it has no author producer. This
                // does not qualify a native GUI/SDK SourceClosed implementation.
                if cancellation.is_canceled() {
                    Err(mondrian_core::MondrianError::Cancelled)
                } else {
                    Ok(())
                }
            }
            fn owner_installation_verifier(&self) -> Option<AudioOwnerInstallationVerifier> {
                self.installed.owner_installation_verifier()
            }
            fn bind_owner_installation(
                &self,
                verifier: AudioOwnerInstallationVerifier,
            ) -> Result<bool, AudioOwnerInstallationError> {
                self.installed.bind_owner_installation(verifier)
            }
            fn install_native_owner_bindings(
                &self,
                claim: &AudioOwnerInstallationClaim,
                cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<AudioPcmNativeOwnerBindingStatus> {
                self.installed.install_native_owner_bindings(claim, cancellation)
            }
            fn render(
                &self,
                request: AudioPcmRenderRequest,
                _cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
                if self.gate_pcm.load(Ordering::Acquire) {
                    self.pcm_entered.store(true, Ordering::Release);
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !self.release.load(Ordering::Acquire) && Instant::now() < deadline {
                        thread::yield_now();
                    }
                }
                if self.cleanup.load(Ordering::Acquire) {
                    return Ok(AudioPcmRenderOutcome::DeferredForEditorCleanup(
                        self.target.lock().expect("one exact cleanup address"),
                    ));
                }
                self.renders.fetch_add(1, Ordering::AcqRel);
                Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                    request.sample_rate,
                    request.channel_layout,
                    request.frame_count,
                )))
            }
            fn control_native_editor(
                &self,
                intent: Intent,
                cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<Outcome> {
                self.entered.store(true, Ordering::Release);
                if self.gated {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !self.release.load(Ordering::Acquire)
                        && !cancellation.is_canceled()
                        && Instant::now() < deadline
                    {
                        thread::yield_now();
                    }
                }
                if cancellation.is_canceled() {
                    return Ok(Outcome::Refused(Refusal::Cancelled));
                }
                if *self.target.lock() != intent.expected_target()
                    || self.installed.verifier().current_claim().is_err()
                {
                    return Ok(Outcome::Refused(Refusal::OwnerChanged));
                }
                let claim = self
                    .installed
                    .verifier()
                    .current_claim()
                    .expect("current registered portable source");
                if intent
                    .expected_target()
                    .is_none_or(|target| claim.generation() != target.installed_generation)
                {
                    return Ok(Outcome::Refused(Refusal::OwnerChanged));
                }
                self.control_threads.lock().push(thread::current().id());
                Ok(match intent {
                    Intent::ResolveAndOpen { .. } => Outcome::Refused(Refusal::UnsupportedAdapter),
                    Intent::Open { target, .. } => {
                        Outcome::Opened { target, width: 320, height: 160 }
                    }
                    Intent::Close { target } => {
                        if self.closes.fetch_add(1, Ordering::AcqRel) == 0 {
                            self.cleanup.store(true, Ordering::Release);
                            Outcome::ClosePending {
                                target,
                                stage: mondrian_core::NativeAudioEditorDetachStage::ClearHostFrame,
                            }
                        } else {
                            self.cleanup.store(false, Ordering::Release);
                            Outcome::Closed { target }
                        }
                    }
                })
            }
        }

        fn wait_editor(
            playback: &mut AudioPlayback,
            closed: bool,
            mode: AudioPlaybackMode,
        ) -> Vec<AudioPlaybackEvent> {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut events = Vec::new();
            while Instant::now() < deadline {
                events.extend(
                    playback
                        .poll(mode, sample_position(0))
                        .expect("actual serial completion")
                        .events,
                );
                if events.iter().any(|event| {
                    matches!(event, AudioPlaybackEvent::NativeEditorObserved { outcome, .. }
                if matches!(outcome, Outcome::Closed { .. }) == closed)
                }) {
                    return events;
                }
                thread::yield_now();
            }
            panic!("same serial editor control did not finish");
        }

        #[test]
        fn native_editor_media_paused_close_pending_preserves_claim_and_refuses_reprime_until_exact_retry(
        ) {
            let (mut playback, _) = playback_with_buffered_frames(0);
            let renderer = CleanupRenderer::new(false);
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("actual slot");
            settle_native_installation(&mut playback);
            let target = renderer.publish_fixture_address();
            let claim = renderer.installed.verifier().current_claim().expect("actual claim");
            assert_eq!(
                playback
                    .request_native_audio_editor(Intent::Close { target })
                    .expect("queue close"),
                Submission::Queued
            );
            let events = wait_editor(&mut playback, false, AudioPlaybackMode::Idle);
            assert!(events.iter().any(|event| matches!(event, AudioPlaybackEvent::NativeEditorCleanupRequired { target: actual, .. } if *actual == target)),
            "a paused close failure must hold immediately without waiting for a PCM request");
            assert_eq!(playback.native_editor_cleanup_target(), Some(target));
            assert_eq!(
                playback.snapshot(AudioPlaybackMode::Idle).state,
                AudioPlaybackState::EditorCleanupPending
            );
            assert_eq!(
                renderer
                    .installed
                    .verifier()
                    .current_claim()
                    .expect("retained same slot")
                    .generation(),
                claim.generation()
            );
            let held = playback.generation;
            assert_eq!(
                playback.reprime(sample_position(100)),
                Err(AudioPlaybackError::NativeEditorCleanupPending)
            );
            assert_eq!(
                playback.generation, held,
                "rejected continuity cannot supersede the only retryable binding"
            );
            assert_eq!(
                playback
                    .request_native_audio_editor(Intent::Open {
                        target,
                        title: mondrian_core::NativeAudioEditorTitle::new("portable source".into())
                            .expect("bounded title")
                    })
                    .expect("open preflight"),
                Submission::Refused(Refusal::ConfigurationPending)
            );
            assert_eq!(
                playback
                    .request_native_audio_editor(Intent::Close { target })
                    .expect("same binding retry"),
                Submission::Queued
            );
            wait_editor(&mut playback, true, AudioPlaybackMode::Idle);
            let deadline = Instant::now() + Duration::from_secs(2);
            while playback
                .native_control_output_retirement
                .is_some_and(|retirement| !retirement.acknowledged)
                && Instant::now() < deadline
            {
                playback
                    .poll(AudioPlaybackMode::Idle, sample_position(0))
                    .expect("exact output retirement");
                thread::yield_now();
            }
            assert!(!playback
                .finalize_native_control_anchor(held - 1, sample_position(0))
                .expect("wrong generation"));
            assert!(playback
                .finalize_native_control_anchor(held, sample_position(0))
                .expect("same acknowledged output anchor"));
            assert_eq!(
                playback.snapshot(AudioPlaybackMode::Idle).state,
                AudioPlaybackState::EditorCleanupCompletePaused
            );
            assert_eq!(
                renderer.closes.load(Ordering::Acquire),
                2,
                "one failed frame clear plus one actual retry"
            );
            assert_eq!(
                renderer.renders.load(Ordering::Acquire),
                0,
                "paused control never fabricates PCM"
            );
            assert!(renderer.control_threads.lock().iter().all(|id| *id != thread::current().id()));
            playback
                .reprime(sample_position(0))
                .expect("only explicit fresh continuity releases completed cleanup");
            settle_native_installation(&mut playback);
            assert!(playback.native_editor_cleanup_target().is_none());
            assert_ne!(
                renderer
                    .installed
                    .verifier()
                    .current_claim()
                    .expect("fresh actual slot generation")
                    .generation(),
                claim.generation()
            );
        }

        #[test]
        fn source_close_accepts_only_its_suspend_after_completed_editor_cleanup() {
            let (mut playback, _) = playback_with_buffered_frames(0);
            let renderer = CleanupRenderer::new(false);
            let erased: Arc<dyn AudioPcmRenderer> = renderer.clone();
            playback
                .prepare(
                    sample_position(0),
                    erased.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .unwrap();
            settle_native_installation(&mut playback);
            let target = renderer.publish_fixture_address();
            playback.request_native_audio_editor(Intent::Close { target }).unwrap();
            wait_editor(&mut playback, false, AudioPlaybackMode::Idle);
            let held = playback.generation;
            assert!(matches!(
                playback.request_source_close(&erased, sample_position(0)),
                Err(AudioPlaybackError::SourceClosePending)
            ));
            assert_eq!(
                playback.generation, held,
                "unfinished cleanup refuses before Suspend"
            );
            playback.request_native_audio_editor(Intent::Close { target }).unwrap();
            wait_editor(&mut playback, true, AudioPlaybackMode::Idle);
            let deadline = Instant::now() + Duration::from_secs(2);
            while playback
                .native_control_output_retirement
                .is_some_and(|retirement| !retirement.acknowledged)
            {
                assert!(Instant::now() < deadline);
                playback.poll(AudioPlaybackMode::Idle, sample_position(0)).unwrap();
                thread::yield_now();
            }
            assert!(playback.finalize_native_control_anchor(held, sample_position(0)).unwrap());
            assert_eq!(
                playback.native_control_hold,
                Some(NativeControlHold::EditorCleanupCompletePaused(target))
            );
            assert!(playback.native_control_output_retirement.is_none());
            let ticket = playback.request_source_close(&erased, sample_position(0)).unwrap();
            let own_retirement = playback
                .native_control_output_retirement
                .expect("this Suspend's new output obligation");
            assert_eq!(own_retirement.generation, playback.generation);
            assert_eq!(own_retirement.token, playback.quiescence_token);
            let deadline = Instant::now() + Duration::from_secs(2);
            while matches!(
                playback.source_close_status(&ticket).unwrap(),
                AudioPcmSourceCloseStatus::Pending
            ) {
                assert!(Instant::now() < deadline);
                playback.poll(AudioPlaybackMode::Idle, sample_position(0)).unwrap();
                thread::yield_now();
            }
            assert!(matches!(
                playback.source_close_status(&ticket).unwrap(),
                AudioPcmSourceCloseStatus::Closed
            ));
            assert_eq!(
                playback.native_control_hold,
                Some(NativeControlHold::EditorCleanupCompletePaused(target)),
                "no completed record was cleared for eligibility"
            );
            let mut foreign_retirement = playback.native_control_output_retirement.unwrap();
            foreign_retirement.generation -= 1;
            playback.native_control_output_retirement = Some(foreign_retirement);
            assert_eq!(
                playback.finish_source_close(&ticket, sample_position(0)),
                Err(AudioPlaybackError::SourceClosePending)
            );
            playback.native_control_output_retirement = Some(own_retirement);
            let current_token = playback.quiescence_token.take();
            assert!(
                current_token.is_some(),
                "actual software output issued a token"
            );
            assert_eq!(
                playback.finish_source_close(&ticket, sample_position(0)),
                Err(AudioPlaybackError::SourceClosePending),
                "a foreign token cannot reuse this close epoch"
            );
            playback.quiescence_token = current_token;
            playback.native_control_output_retirement = Some(own_retirement);
            playback.finish_source_close(&ticket, sample_position(0)).unwrap();
            assert!(playback.native_control_hold.is_none());
            assert!(playback.native_control_output_retirement.is_none());
            assert!(matches!(
                renderer.installed.verifier().current_claim(),
                Err(AudioOwnerInstallationError::NotInstalled)
            ));
            assert_eq!(renderer.closes.load(Ordering::Acquire), 2);
            assert_eq!(renderer.renders.load(Ordering::Acquire), 0);
            drop((erased, renderer));
            assert!(playback.shutdown_and_wait().all_workers_terminated());
        }

        #[test]
        fn native_editor_media_cleanup_holds_normal_pcm_without_fault_substitution_or_new_owner() {
            let (mut playback, _) = playback_with_buffered_frames(0);
            let renderer = CleanupRenderer::new(false);
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("actual slot");
            settle_native_installation(&mut playback);
            let target = renderer.publish_fixture_address();
            poll_until_settled(&mut playback, sample_position(0));
            let positive = renderer.renders.load(Ordering::Acquire);
            assert!(
                positive > 0,
                "actual ordinary PCM must run before testing cleanup hold"
            );
            assert_eq!(
                playback
                    .request_native_audio_editor(Intent::Close { target })
                    .expect("source close"),
                Submission::Queued
            );
            wait_editor(&mut playback, false, AudioPlaybackMode::Consume);
            let after_hold = renderer.renders.load(Ordering::Acquire);
            for _ in 0..8 {
                playback
                    .poll(AudioPlaybackMode::Consume, sample_position(0))
                    .expect("normal parent must retain rather than Process/retire");
            }
            assert_eq!(
                renderer.renders.load(Ordering::Acquire),
                after_hold,
                "control hold admits no new normal PCM"
            );
            assert_eq!(
                playback.snapshot(AudioPlaybackMode::Consume).render_substitution_count,
                0
            );
            assert_eq!(
                playback.snapshot(AudioPlaybackMode::Consume).render_generation_recovery_count,
                0
            );
            assert!(playback.renderer.as_ref().is_some_and(|actual| Arc::ptr_eq(
                &actual.owner,
                &(renderer.clone() as Arc<dyn AudioPcmRenderer>)
            )));
            assert_eq!(
                renderer.installed.calls.lock().len(),
                1,
                "hold must not bind a different native owner"
            );
        }

        #[test]
        fn native_editor_media_reserved_control_slots_survive_saturated_pcm_fifo() {
            let (mut playback, _) = playback_with_buffered_frames(0);
            let renderer = CleanupRenderer::new(false);
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("actual installed source");
            settle_native_installation(&mut playback);
            let target = renderer.publish_fixture_address();
            renderer.gate_pcm.store(true, Ordering::Release);
            playback
                .poll(AudioPlaybackMode::Consume, sample_position(0))
                .expect("actual saturated PCM admission");
            let deadline = Instant::now() + Duration::from_secs(2);
            while !renderer.pcm_entered.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(renderer.pcm_entered.load(Ordering::Acquire));
            assert_eq!(playback.in_flight, playback.config.max_in_flight);
            let first = playback
                .request_native_audio_editor(Intent::Close { target })
                .expect("first exact serial close");
            let second = playback
                .request_native_audio_editor(Intent::Close { target })
                .expect("one exact serial retry");
            let excess = playback
                .request_native_audio_editor(Intent::Close { target })
                .expect("bounded excess submission");
            renderer.release.store(true, Ordering::Release);
            assert_eq!(
                [first, second],
                [Submission::Queued; 2],
                "PCM admission cannot consume both reserved native control slots"
            );
            assert_eq!(
                excess,
                Submission::Refused(Refusal::WorkerUnavailable),
                "native controls retain an independent finite queue bound"
            );
            wait_editor(&mut playback, true, AudioPlaybackMode::Consume);
            assert_eq!(renderer.closes.load(Ordering::Acquire), 2);
            assert_eq!(playback.render_substitution_count, 0);
        }

        #[test]
        fn native_editor_media_pcm_hold_retains_active_and_queued_same_owner_close() {
            let (mut playback, _) = playback_with_buffered_frames(0);
            let renderer = CleanupRenderer::new(true);
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("actual installed source");
            settle_native_installation(&mut playback);
            let target = renderer.publish_fixture_address();
            let original =
                renderer.installed.verifier().current_claim().expect("actual source claim");
            let original_generation = playback.generation;
            // The actual renderer reports cleanup on an ordinary PCM request.
            // Both controls run on the SAME serial worker after that PCM call.
            renderer.cleanup.store(true, Ordering::Release);
            playback
                .poll(AudioPlaybackMode::Consume, sample_position(0))
                .expect("ordinary PCM scheduling before any parent hold");
            assert!(
                playback.in_flight > 0,
                "real PCM must be queued before native control"
            );
            for _ in 0..2 {
                assert_eq!(
                    playback
                        .request_native_audio_editor(Intent::Close { target })
                        .expect("actual control queue"),
                    Submission::Queued
                );
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while !renderer.entered.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(
                renderer.entered.load(Ordering::Acquire),
                "serial worker must enter Close AFTER producing the real PCM completion"
            );
            let mut events = Vec::new();
            while playback.native_editor_cleanup_target() != Some(target)
                && Instant::now() < deadline
            {
                events.extend(
                    playback
                        .poll(AudioPlaybackMode::Consume, sample_position(0))
                        .expect("same-owner PCM-origin hold")
                        .events,
                );
                thread::yield_now();
            }
            assert_eq!(playback.native_editor_cleanup_target(), Some(target));
            assert!(
                playback.generation > original_generation,
                "PCM scheduling generation really rotated"
            );
            assert_eq!(
                renderer
                    .installed
                    .verifier()
                    .current_claim()
                    .expect("same actual source")
                    .generation(),
                original.generation(),
                "control hold must retain actual installed owner"
            );
            renderer.release.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(2);
            while !events.iter().any(|event| {
                matches!(event,
                AudioPlaybackEvent::NativeEditorObserved{outcome:Outcome::Closed{target:actual},..}
                    if *actual==target)
            }) && Instant::now() < deadline
            {
                events.extend(
                    playback
                        .poll(AudioPlaybackMode::Consume, sample_position(0))
                        .expect("close completion after the PCM hold")
                        .events,
                );
                thread::yield_now();
            }
            assert!(
                events.iter().any(|event| matches!(event,
                AudioPlaybackEvent::NativeEditorObserved{outcome:Outcome::Closed{target:actual},..}
                    if *actual==target)),
                "same-owner PCM hold must not cancel the active or queued native Close"
            );
            assert_eq!(renderer.closes.load(Ordering::Acquire),2,
                "one failed clear followed by the one exact queued retry must both reach the same object");
            assert_eq!(
                renderer.installed.calls.lock().len(),
                1,
                "hold must not issue another binding"
            );
            assert_eq!(
                playback.snapshot(AudioPlaybackMode::Consume).render_substitution_count,
                0
            );
            assert_eq!(
                playback.snapshot(AudioPlaybackMode::Consume).render_generation_recovery_count,
                0
            );
            assert_eq!(
                renderer.renders.load(Ordering::Acquire),
                0,
                "cleanup never fabricates an accepted PCM or advances the renderer"
            );
        }

        #[test]
        fn native_editor_media_superseded_actual_slot_discards_late_control_without_publishing_success(
        ) {
            let (mut playback, _) = playback_with_buffered_frames(0);
            let renderer = CleanupRenderer::new(true);
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("old actual slot");
            settle_native_installation(&mut playback);
            let target = renderer.publish_fixture_address();
            assert_eq!(
                playback
                    .request_native_audio_editor(Intent::Close { target })
                    .expect("old queued native work"),
                Submission::Queued
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            while !renderer.entered.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(
                renderer.entered.load(Ordering::Acquire),
                "real serial worker must enter the source before replacement"
            );
            let replacement = NativeInstallationProbe::new(false, false);
            playback
                .prepare(
                    sample_position(0),
                    replacement,
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("actual current slot swap");
            renderer.release.store(true, Ordering::Release);
            let events = settle_native_installation(&mut playback);
            assert!(events
                .iter()
                .all(|event| !matches!(event, AudioPlaybackEvent::NativeEditorObserved { .. })));
            assert!(playback.native_editor_cleanup_target().is_none());
            assert_eq!(
                renderer.closes.load(Ordering::Acquire),
                0,
                "canceled stale source must not call a native cleanup"
            );
            assert!(playback.stale_completion_count > 0);
            assert_eq!(
                playback
                    .request_native_audio_editor(Intent::Close { target })
                    .expect("old target metadata"),
                Submission::Refused(Refusal::OwnerChanged)
            );
        }
    }

    #[cfg(test)]
    mod native_diagnostic_contracts {
        use super::*;
        use mondrian_core::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        fn batch() -> NativeAudioDiagnosticBatch {
            let source = NativeAudioDiagnosticSource {
                root_plan: AudioPreparedPlanId::new(),
                root_session: AudioRenderSessionId::new(),
                local_plan: AudioPreparedPlanId::new(),
                local_session: AudioRenderSessionId::new(),
                instance: AudioProcessorInstanceId::new(),
                location: NativeAudioDiagnosticLocation::Output(ProgramOutputId::new()),
                insertion: NativeAudioDiagnosticInsertion::PreFader,
                process_id: std::num::NonZeroU32::new(789).expect("owned test producer fact"),
            };
            let mut batch =
                NativeAudioDiagnosticBatch::new(source, NativeAudioDiagnosticLoss::default());
            for (raw, severity) in [
                NativeAudioDiagnosticSeverity::Debug,
                NativeAudioDiagnosticSeverity::Info,
                NativeAudioDiagnosticSeverity::Warning,
                NativeAudioDiagnosticSeverity::Error,
                NativeAudioDiagnosticSeverity::Fatal,
                NativeAudioDiagnosticSeverity::HostMisbehaving,
                NativeAudioDiagnosticSeverity::PluginMisbehaving,
            ]
            .into_iter()
            .enumerate()
            {
                batch
                    .push(
                        NativeAudioDiagnosticRecord::project(
                            raw as u64,
                            raw as i32,
                            severity,
                            NativeAudioDiagnosticMessageFacts::default(),
                            b"owned diagnostic",
                        )
                        .expect("fixed prefix"),
                    )
                    .expect("fixed batch");
            }
            batch
        }

        struct DiagnosticRenderer {
            queued: Mutex<Option<NativeAudioDiagnosticBatch>>,
            wide: Mutex<
                Option<(
                    NativeAudioEditorAttachmentObservation,
                    NativeAudioParameterObservation,
                    NativeAudioAuthorProposal,
                )>,
            >,
            calls: AtomicUsize,
            thread: Mutex<Option<thread::ThreadId>>,
            entered: AtomicBool,
            release: AtomicBool,
            gated: bool,
            rejected: bool,
        }
        impl DiagnosticRenderer {
            fn new(gated: bool, rejected: bool) -> Arc<Self> {
                Arc::new(Self {
                    queued: Mutex::new(Some(batch())),
                    wide: Mutex::new(None),
                    calls: AtomicUsize::new(0),
                    thread: Mutex::new(None),
                    entered: AtomicBool::new(false),
                    release: AtomicBool::new(false),
                    gated,
                    rejected,
                })
            }
        }
        impl AudioPcmRenderer for DiagnosticRenderer {
            fn bind_native_child_settlement(
                &self,
                registrar: &mondrian_core::NativeChildSettlementRegistrar,
            ) -> Result<(), mondrian_core::NativeChildSettlementError> {
                // This portable test adapter owns only the host fields declared here.
                let _ = registrar;
                Ok(())
            }
            fn native_observations_supported(&self) -> bool {
                true
            }
            fn poll_native_observations(
                &self,
                _cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<Option<mondrian_core::NativeAudioObservationBatch>>
            {
                self.calls.fetch_add(1, Ordering::Relaxed);
                *self.thread.lock() = Some(thread::current().id());
                // Take first: this models native records already drained before a
                // concurrent owner supersession. Cancellation cannot unconsume them.
                let value = self.queued.lock().take();
                let wide = self.wide.lock().take();
                self.entered.store(true, Ordering::Release);
                if self.gated {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !self.release.load(Ordering::Acquire) && Instant::now() < deadline {
                        thread::yield_now();
                    }
                }
                if self.rejected {
                    Err(MondrianError::Cancelled)
                } else {
                    Ok(
                        value.map(|diagnostics| mondrian_core::NativeAudioObservationBatch {
                            diagnostics: Some(diagnostics),
                            author: wide.as_ref().map(|(_, _, author)| author.clone()),
                            parameters: wide.as_ref().map(|(_, parameters, _)| parameters.clone()),
                            attachment: wide.map(|(attachment, _, _)| attachment),
                        }),
                    )
                }
            }
            fn render(
                &self,
                request: AudioPcmRenderRequest,
                _cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
                Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                    request.sample_rate,
                    request.channel_layout,
                    request.frame_count,
                )))
            }
        }
        fn wait_for(
            playback: &mut AudioPlayback,
            predicate: impl Fn(&AudioPlayback) -> bool,
        ) -> Vec<AudioPlaybackEvent> {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut events = Vec::new();
            while Instant::now() < deadline {
                events.extend(
                    playback
                        .poll(AudioPlaybackMode::Idle, sample_position(0))
                        .expect("actual serial control poll")
                        .events,
                );
                if predicate(playback) {
                    return events;
                }
                thread::yield_now();
            }
            panic!("actual diagnostic completion did not settle");
        }

        #[test]
        fn native_attachment_media_same_completion_precedes_values_and_author_without_extra_poll() {
            let (mut playback, _) = playback_with_buffered_frames(0);
            let renderer = DiagnosticRenderer::new(false, false);
            let diagnostic = renderer.queued.lock().as_ref().expect("owned source").source();
            let previous = NativeAudioEditorTarget {
                root_plan: diagnostic.root_plan,
                root_session: diagnostic.root_session,
                local_plan: diagnostic.local_plan,
                local_session: diagnostic.local_session,
                instance: diagnostic.instance,
                owner: NativeAudioEditorOccurrenceOwner::Output(match diagnostic.location {
                    NativeAudioDiagnosticLocation::Output(output) => output,
                    _ => panic!("owned output"),
                }),
                insertion: NativeAudioEditorInsertion::PreFader,
                continuity_epoch: None,
                binding: AudioProcessorBindingId::new(),
                installed_generation: std::num::NonZeroU64::new(1).expect("observed metadata"),
            };
            let mut current = previous;
            current.binding = AudioProcessorBindingId::new();
            current.continuity_epoch = Some(41);
            let attachment = NativeAudioEditorAttachmentObservation {
                previous,
                current,
                process_id: std::num::NonZeroU32::new(789).expect("owned PID"),
                object_serial: std::num::NonZeroU64::new(2).expect("owned object"),
                retained: true,
            };
            let id = ParameterId::new("owned.param").expect("id");
            let parameters = NativeAudioParameterObservation::project(
                current,
                attachment.process_id,
                attachment.object_serial,
                3,
                vec![NativeAudioParameterFact {
                    revision: 3,
                    parameter: id.clone(),
                    event_flags: 0,
                    value_bits: Some(0.75_f64.to_bits()),
                    origin: NativeAudioParameterObservationOrigin::MainQuery { rescan_flags: 0 },
                    hidden: None,
                    periodic: None,
                    name: None,
                    module: None,
                    info_flags: None,
                    display: None,
                }],
            )
            .expect("owned facts");
            let author = NativeAudioAuthorProposal::project(
                current,
                SequenceId::new(),
                3,
                NativeAudioAuthorBoundaryFacts {
                    completion: NativeAudioAuthorCompletion::ParametersOnly,
                    open_parameter_edits: 0,
                    group_depth: 0,
                },
                vec![NativeAudioAuthorCallback::Parameter {
                    parameter: id,
                    phase: NativeAudioParameterCallbackPhase::Value,
                    value_bits: Some(0.75_f64.to_bits()),
                }],
                None,
            )
            .expect("owned proposal");
            *renderer.wide.lock() = Some((attachment, parameters, author));
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("actual slot");
            let events = wait_for(&mut playback, |playback| {
                renderer.calls.load(Ordering::Relaxed) > 0
                    && playback.native_diagnostic_in_flight.is_none()
            });
            let at=events.iter().position(|event|matches!(event,AudioPlaybackEvent::NativeEditorAttachmentObserved {observation,..} if **observation==attachment)).expect("actual attachment completion");
            let values = events
                .iter()
                .position(|event| {
                    matches!(
                        event,
                        AudioPlaybackEvent::NativeParameterObservationsObserved { .. }
                    )
                })
                .expect("same facts");
            let author = events
                .iter()
                .position(|event| {
                    matches!(
                        event,
                        AudioPlaybackEvent::NativeAuthorProposalObserved { .. }
                    )
                })
                .expect("same proposal");
            assert!(
                at < values && values < author,
                "same admitted target must precede author/value surface admission"
            );
            assert_eq!(
                renderer.calls.load(Ordering::Relaxed),
                1,
                "one existing Poll work, no attachment-only queue"
            );
        }

        #[test]
        fn native_diagnostic_media_paused_serial_completion_preserves_all_levels_without_pcm_or_editor_binding(
        ) {
            let (mut playback, _) = playback_with_buffered_frames(0);
            let renderer = DiagnosticRenderer::new(false, false);
            let expected =
                renderer.queued.lock().clone().expect("independent expected observation");
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("actual slot");
            let events = wait_for(&mut playback, |playback| {
                renderer.calls.load(Ordering::Relaxed) > 0
                    && playback.native_diagnostic_in_flight.is_none()
            });
            let observed = events
                .iter()
                .find_map(|event| {
                    if let AudioPlaybackEvent::NativeDiagnosticsObserved { batch, .. } = event {
                        Some(batch)
                    } else {
                        None
                    }
                })
                .expect("accepted actual serial completion");
            assert_eq!(observed.as_ref(), &expected);
            assert_ne!(*renderer.thread.lock(), Some(thread::current().id()));
            assert_eq!(playback.snapshot(AudioPlaybackMode::Idle).in_flight, 0);
            assert_eq!(
                playback.native_owner_binding_status(),
                None,
                "diagnostics are independent of editor object capability"
            );
            for _ in 0..16 {
                assert!(playback
                    .poll(AudioPlaybackMode::Idle, sample_position(0))
                    .expect("bounded repeated UI polls")
                    .events
                    .iter()
                    .all(|event| !matches!(
                        event,
                        AudioPlaybackEvent::NativeDiagnosticsObserved { .. }
                    )));
            }
            assert_eq!(
                renderer.calls.load(Ordering::Relaxed),
                1,
                "UI poll rate cannot create duplicate control work"
            );
        }

        #[test]
        fn native_diagnostic_media_adapter_refusal_does_not_invalidate_or_substitute_valid_pcm() {
            let (mut playback, _) = playback_with_buffered_frames(0);
            let renderer = DiagnosticRenderer::new(false, true);
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("valid graph");
            let events = wait_for(&mut playback, |playback| {
                renderer.calls.load(Ordering::Relaxed) > 0
                    && playback.native_diagnostic_in_flight.is_none()
            });
            assert!(events.iter().any(|event| matches!(
                event,
                AudioPlaybackEvent::NativeObservationPollRefused {
                    reason: AudioNativeObservationPollRefusal::ProcessorRejected,
                    ..
                }
            )));
            poll_until_settled(&mut playback, sample_position(0));
            let state = playback.snapshot(AudioPlaybackMode::Consume);
            assert_eq!(state.render_substitution_count, 0);
            assert_eq!(state.render_generation_recovery_count, 0);
            assert!(playback.renderer.is_some());
        }

        #[test]
        fn native_diagnostic_media_drained_old_source_cannot_publish_after_seek_and_reports_exact_discard_count(
        ) {
            let (mut playback, _) = playback_with_buffered_frames(0);
            let renderer = DiagnosticRenderer::new(true, false);
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("old graph");
            let _ = playback
                .poll(AudioPlaybackMode::Idle, sample_position(0))
                .expect("admit one actual control");
            let deadline = Instant::now() + Duration::from_secs(2);
            while !renderer.entered.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(renderer.entered.load(Ordering::Acquire));
            playback.reprime(sample_position(100)).expect("explicit fresh seek");
            renderer.release.store(true, Ordering::Release);
            let events = wait_for(&mut playback, |playback| {
                playback.native_diagnostic_discarded_records() == 7
            });
            assert!(events.iter().all(|event| !matches!(
                event,
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. }
            )));
            assert_eq!(playback.native_diagnostic_discarded_records(), 7);
            assert_eq!(playback.render_generation_recovery_count, 0);
        }

        #[test]
        fn native_diagnostic_media_same_renderer_and_generation_still_refuse_superseded_actual_slot_claim(
        ) {
            let (mut playback, _) = playback_with_buffered_frames(0);
            let renderer = DiagnosticRenderer::new(true, false);
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("old graph");
            let generation = playback.generation;
            let renderer_id = playback.renderer.as_ref().expect("actual slot").materialization_id;
            let _ = playback
                .poll(AudioPlaybackMode::Idle, sample_position(0))
                .expect("admit control");
            let deadline = Instant::now() + Duration::from_secs(2);
            while !renderer.entered.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(renderer.entered.load(Ordering::Acquire));
            // Negative admission oracle: mint no fake claim. Change the actual Core
            // source incarnation while leaving the same renderer/generation fields.
            let (registration, _) =
                playback.owner_installation.register().expect("fresh actual registration");
            let expected =
                playback.owner_installation.current_claim().expect("actual source metadata");
            let reserved = playback
                .owner_installation
                .reserve_install(&registration, expected.as_ref())
                .expect("actual CAS preflight");
            playback
                .owner_installation
                .begin_commit(reserved)
                .expect("actual Core guard")
                .commit()
                .expect("real superseding incarnation");
            assert_eq!(playback.generation, generation);
            assert_eq!(
                playback.renderer.as_ref().expect("same Arc slot").materialization_id,
                renderer_id
            );
            renderer.release.store(true, Ordering::Release);
            let events = wait_for(&mut playback, |playback| {
                playback.native_diagnostic_discarded_records() == 7
            });
            assert!(events.iter().all(|event| !matches!(
                event,
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. }
            )));
            assert_eq!(playback.native_diagnostic_discarded_records(), 7);
        }
    }

    mod native_author_media_contracts {
        use super::*;
        use mondrian_core::{
            NativeAudioAuthorBoundaryFacts, NativeAudioAuthorCallback, NativeAudioAuthorProposal,
            NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
            NativeAudioEditorControlSubmission as Submission, NativeAudioEditorTarget as Target,
            NativeAudioParameterCallbackPhase,
        };
        use std::sync::atomic::AtomicUsize;

        // Actual Media/Core installation and queue contracts with an explicit
        // portable adapter. No native HWND, SDK or author Session claim.
        struct Renderer {
            installed: Arc<NativeInstallationProbe>,
            target: Mutex<Option<Target>>,
            attachment: Mutex<Option<Target>>,
            pending: AtomicBool,
            state_entry: AtomicBool,
            enters: AtomicUsize,
            processed: Mutex<Vec<AudioPcmRenderRequest>>,
            refused: Mutex<Vec<AudioPcmRenderRequest>>,
            installs: AtomicUsize,
            acks: AtomicUsize,
            closes: AtomicUsize,
            apply_entered: AtomicBool,
            apply_release: AtomicBool,
            gated: bool,
            policy_queries: AtomicUsize,
            policy_gated: AtomicBool,
            output_shutdown_started: Arc<AtomicBool>,
            dropped_on: Arc<Mutex<Option<(thread::ThreadId, bool)>>>,
            request_drops: Arc<Mutex<Vec<thread::ThreadId>>>,
            request_drop_panics: AtomicBool,
        }
        impl Drop for Renderer {
            fn drop(&mut self) {
                *self.dropped_on.lock() = Some((
                    thread::current().id(),
                    !self.output_shutdown_started.load(Ordering::Acquire),
                ));
            }
        }
        impl Renderer {
            fn new(gated: bool) -> Arc<Self> {
                Arc::new(Self {
                    installed: NativeInstallationProbe::new(false, false),
                    target: Mutex::new(None),
                    attachment: Mutex::new(None),
                    pending: AtomicBool::new(false),
                    state_entry: AtomicBool::new(false),
                    enters: AtomicUsize::new(0),
                    processed: Mutex::new(Vec::new()),
                    refused: Mutex::new(Vec::new()),
                    installs: AtomicUsize::new(0),
                    acks: AtomicUsize::new(0),
                    closes: AtomicUsize::new(0),
                    apply_entered: AtomicBool::new(false),
                    apply_release: AtomicBool::new(false),
                    gated,
                    policy_queries: AtomicUsize::new(0),
                    policy_gated: AtomicBool::new(false),
                    output_shutdown_started: Arc::new(AtomicBool::new(false)),
                    dropped_on: Arc::new(Mutex::new(None)),
                    request_drops: Arc::new(Mutex::new(Vec::new())),
                    request_drop_panics: AtomicBool::new(false),
                })
            }
            fn address(&self) -> Target {
                let root_plan = mondrian_core::AudioPreparedPlanId::new();
                let root_session = mondrian_core::AudioRenderSessionId::new();
                let target = Target {
                    root_plan,
                    root_session,
                    local_plan: root_plan,
                    local_session: root_session,
                    instance: mondrian_core::AudioProcessorInstanceId::new(),
                    owner: mondrian_core::NativeAudioEditorOccurrenceOwner::Output(
                        mondrian_core::ProgramOutputId::new(),
                    ),
                    insertion: mondrian_core::NativeAudioEditorInsertion::PreFader,
                    continuity_epoch: None,
                    binding: mondrian_core::AudioProcessorBindingId::new(),
                    installed_generation: self
                        .installed
                        .verifier()
                        .current_claim()
                        .expect("actual current slot")
                        .generation(),
                };
                *self.target.lock() = Some(target);
                *self.attachment.lock() = Some(target);
                target
            }
            fn request(
                self: &Arc<Self>,
                target: Target,
                revision: u64,
                retry: bool,
            ) -> Box<dyn AudioPcmNativeAuthorHandoff> {
                let proposal = NativeAudioAuthorProposal::project(
                    target,
                    mondrian_core::SequenceId::new(),
                    revision,
                    NativeAudioAuthorBoundaryFacts {
                        completion: mondrian_core::NativeAudioAuthorCompletion::Nonterminal,
                        open_parameter_edits: 1,
                        group_depth: 0,
                    },
                    vec![NativeAudioAuthorCallback::Parameter {
                        parameter: mondrian_core::ParameterId::new_static("native.gain"),
                        phase: NativeAudioParameterCallbackPhase::Value,
                        value_bits: Some(0.75f64.to_bits()),
                    }],
                    None,
                )
                .expect("bounded portable completion facts");
                Box::new(Request {
                    renderer: Arc::clone(self),
                    erased: self.clone(),
                    proposal,
                    target,
                    retry,
                })
            }
        }
        impl AudioPcmRenderer for Renderer {
            fn bind_native_child_settlement(
                &self,
                registrar: &mondrian_core::NativeChildSettlementRegistrar,
            ) -> Result<(), mondrian_core::NativeChildSettlementError> {
                self.installed.bind_native_child_settlement(registrar)
            }
            fn owner_installation_verifier(&self) -> Option<AudioOwnerInstallationVerifier> {
                self.installed.owner_installation_verifier()
            }
            fn bind_owner_installation(
                &self,
                verifier: AudioOwnerInstallationVerifier,
            ) -> Result<bool, AudioOwnerInstallationError> {
                self.installed.bind_owner_installation(verifier)
            }
            fn install_native_owner_bindings(
                &self,
                claim: &AudioOwnerInstallationClaim,
                cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<AudioPcmNativeOwnerBindingStatus> {
                self.installed.install_native_owner_bindings(claim, cancellation)
            }
            fn render(
                &self,
                request: AudioPcmRenderRequest,
                _: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
                if self.pending.load(Ordering::Acquire) {
                    self.refused.lock().push(request);
                    return Ok(AudioPcmRenderOutcome::DeferredForNativeAuthor(Box::new(
                        self.target.lock().expect("actual fixture address"),
                    )));
                }
                if matches!(request.continuity, AudioPcmContinuity::Enter(_)) {
                    self.enters.fetch_add(1, Ordering::AcqRel);
                }
                let mut processed = self.processed.lock();
                if let Some(previous) = processed.last()
                    && !(self.state_entry.load(Ordering::Acquire)
                        && matches!(request.continuity, AudioPcmContinuity::Enter(_)))
                {
                    assert_eq!(request.start_sample,previous.start_sample+i64::try_from(previous.frame_count).expect("bounded frames"),
                        "after held root windows the existing Session must continue the first unrendered coordinate");
                    assert!(
                        matches!(request.continuity, AudioPcmContinuity::Continue(_)),
                        "author handoff cannot re-enter continuity"
                    );
                }
                processed.push(request);
                Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                    request.sample_rate,
                    request.channel_layout,
                    request.frame_count,
                )))
            }
            fn control_native_editor(
                &self,
                intent: Intent,
                cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<Outcome> {
                if cancellation.is_canceled() {
                    return Ok(Outcome::Refused(
                        mondrian_core::NativeAudioEditorControlRefusal::Cancelled,
                    ));
                }
                let current = self.target.lock().expect("current same-object target");
                if let Intent::Open { target, .. } = &intent
                    && *target == current
                {
                    *self.attachment.lock() = Some(*target);
                    return Ok(Outcome::Opened { target: *target, width: 320, height: 160 });
                }
                if let Intent::Close { target } = intent
                    && *self.attachment.lock() == Some(target)
                {
                    self.installed
                        .verifier()
                        .current_claim()
                        .expect("actual current installed claim");
                    self.closes.fetch_add(1, Ordering::AcqRel);
                    *self.attachment.lock() = None;
                    return Ok(Outcome::Closed { target: current });
                }
                Ok(Outcome::Refused(
                    mondrian_core::NativeAudioEditorControlRefusal::OwnerChanged,
                ))
            }
        }
        struct Request {
            renderer: Arc<Renderer>,
            erased: Arc<dyn AudioPcmRenderer>,
            proposal: NativeAudioAuthorProposal,
            target: Target,
            retry: bool,
        }
        impl Drop for Request {
            fn drop(&mut self) {
                self.renderer.request_drops.lock().push(thread::current().id());
                assert!(
                    !self.renderer.request_drop_panics.load(Ordering::Acquire),
                    "controlled completion foreign owner Drop panic"
                );
            }
        }
        impl AudioPcmNativeAuthorHandoff for Request {
            fn continuity_policy(
                &self,
                _: &ExecutionCancellationToken,
            ) -> Result<AudioPcmNativeAuthorHandoffContinuity, AudioPcmNativeAuthorHandoffRefusal>
            {
                self.renderer.policy_queries.fetch_add(1, Ordering::AcqRel);
                if self.renderer.policy_gated.load(Ordering::Acquire) {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !self.renderer.apply_release.load(Ordering::Acquire)
                        && Instant::now() < deadline
                    {
                        thread::yield_now();
                    }
                }
                Ok(if self.renderer.state_entry.load(Ordering::Acquire) {
                    AudioPcmNativeAuthorHandoffContinuity::StateEntryRequired
                } else {
                    AudioPcmNativeAuthorHandoffContinuity::Preserved
                })
            }
            fn renderer(&self) -> &Arc<dyn AudioPcmRenderer> {
                &self.erased
            }
            fn proposal(&self) -> &NativeAudioAuthorProposal {
                &self.proposal
            }
            fn expected_target(&self) -> Target {
                self.target
            }
            fn intent(&self) -> AudioPcmNativeAuthorHandoffIntent {
                if self.retry {
                    AudioPcmNativeAuthorHandoffIntent::Acknowledge
                } else {
                    AudioPcmNativeAuthorHandoffIntent::Install
                }
            }
            fn acknowledge(
                &self,
                cancellation: &ExecutionCancellationToken,
            ) -> AudioPcmNativeAuthorHandoffOutcome {
                if cancellation.is_canceled() {
                    return AudioPcmNativeAuthorHandoffOutcome::Refused(
                        AudioPcmNativeAuthorHandoffRefusal::Cancelled,
                    );
                }
                self.renderer.acks.fetch_add(1, Ordering::AcqRel);
                self.renderer.pending.store(false, Ordering::Release);
                AudioPcmNativeAuthorHandoffOutcome::Acknowledged {
                    claim: self
                        .renderer
                        .installed
                        .verifier()
                        .current_claim()
                        .expect("actual current slot"),
                    target: Box::new(
                        self.renderer.target.lock().expect("current installed target"),
                    ),
                    continuity: if self.renderer.state_entry.load(Ordering::Acquire) {
                        AudioPcmNativeAuthorHandoffContinuity::StateEntryRequired
                    } else {
                        AudioPcmNativeAuthorHandoffContinuity::Preserved
                    },
                }
            }
            fn apply(
                &self,
                destination: AudioOwnerInstallationVerifier,
                authorization: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
                cancellation: &ExecutionCancellationToken,
            ) -> AudioPcmNativeAuthorHandoffOutcome {
                self.renderer.apply_entered.store(true, Ordering::Release);
                let deadline = Instant::now() + Duration::from_secs(2);
                while self.renderer.gated
                    && !self.renderer.apply_release.load(Ordering::Acquire)
                    && !cancellation.is_canceled()
                    && Instant::now() < deadline
                {
                    thread::yield_now();
                }
                if cancellation.is_canceled() {
                    return AudioPcmNativeAuthorHandoffOutcome::Refused(
                        AudioPcmNativeAuthorHandoffRefusal::Cancelled,
                    );
                }
                let mut verifier = self.renderer.installed.verifier.lock();
                let mut target = self.renderer.target.lock();
                let mut next = target.expect("same actual target");
                next.root_plan = mondrian_core::AudioPreparedPlanId::new();
                next.local_plan = next.root_plan;
                if self.renderer.state_entry.load(Ordering::Acquire) {
                    next.root_session = mondrian_core::AudioRenderSessionId::new();
                    next.local_session = next.root_session;
                    next.continuity_epoch = None;
                }
                next.binding = mondrian_core::AudioProcessorBindingId::new();
                let commit = authorization
                    .begin_commit(&destination)
                    .expect("actual private-source authorization");
                let claim = commit.commit();
                next.installed_generation = claim.generation();
                let retired = verifier.replace(destination);
                *target = Some(next);
                drop(verifier);
                drop(target);
                drop(retired);
                self.renderer.installs.fetch_add(1, Ordering::AcqRel);
                self.acknowledge(cancellation)
            }
        }
        fn wait(
            playback: &mut AudioPlayback,
            predicate: impl Fn(&[AudioPlaybackEvent]) -> bool,
        ) -> Vec<AudioPlaybackEvent> {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut events = Vec::new();
            while Instant::now() < deadline {
                events.extend(
                    playback
                        .poll(AudioPlaybackMode::Consume, sample_position(0))
                        .expect("actual queue poll")
                        .events,
                );
                if predicate(&events) {
                    return events;
                }
                thread::yield_now();
            }
            panic!("actual native author queue boundary did not complete");
        }
        fn setup(
            gated: bool,
        ) -> (
            AudioPlayback,
            Arc<Mutex<FakeOutputState>>,
            Arc<Renderer>,
            Target,
        ) {
            let (mut playback, output) = playback_with_buffered_frames(0);
            let renderer = Renderer::new(gated);
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("actual retained renderer");
            settle_native_installation(&mut playback);
            let target = renderer.address();
            poll_until_settled(&mut playback, sample_position(0));
            assert_eq!(renderer.enters.load(Ordering::Acquire), 1);
            (playback, output, renderer, target)
        }

        // Actual Media producer/retirement threads and Core installation with
        // the portable adapter above; no SDK/plugin or native voice qualification.
        fn completion_owner_shutdown(closed_receiver: bool, panic_on_drop: bool) {
            let renderer = Renderer::new(false);
            renderer.request_drop_panics.store(panic_on_drop, Ordering::Release);
            let (output, _) = fake_output_with_control(FakeOutputControl {
                shutdown_started: Some(Arc::clone(&renderer.output_shutdown_started)),
                ..Default::default()
            });
            let mut playback =
                AudioPlayback::with_output(test_config(), output).expect("actual Media workers");
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("actual retained renderer");
            settle_native_installation(&mut playback);
            let target = renderer.address();
            poll_until_settled(&mut playback, sample_position(0));
            let retirement_thread = playback
                .render_retirement_worker
                .as_ref()
                .expect("original retirement worker")
                .thread()
                .id();
            let caller_thread = thread::current().id();
            let dropped_on = Arc::clone(&renderer.dropped_on);
            let request_drops = Arc::clone(&renderer.request_drops);
            let weak = Arc::downgrade(&renderer);
            if closed_receiver {
                let (sender, receiver) = mpsc::channel();
                drop(sender);
                // Close the actual producer's receiver. No extra sender is
                // retained, so the existing send path must handle SendError.
                drop(std::mem::replace(&mut playback.completion_rx, receiver));
            }
            renderer.state_entry.store(true, Ordering::Release);
            renderer.pending.store(true, Ordering::Release);
            let published_before =
                playback.render_queue.completion_publications.load(Ordering::Acquire);
            assert!(playback
                .request_native_author_handoff(renderer.request(target, 19, false))
                .expect("actual request admission"));
            let deadline = Instant::now() + Duration::from_secs(2);
            while renderer.policy_queries.load(Ordering::Acquire) == 0 && Instant::now() < deadline
            {
                thread::yield_now();
            }
            assert_eq!(
                renderer.policy_queries.load(Ordering::Acquire),
                1,
                "actual producer reached the state-entry policy"
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            if closed_receiver {
                while !playback.render_worker.as_ref().expect("actual producer").is_finished()
                    && Instant::now() < deadline
                {
                    thread::yield_now();
                }
                assert!(
                    playback.render_worker.as_ref().expect("actual producer").is_finished(),
                    "SendError must finish the producer before shutdown stops publication"
                );
                assert_eq!(
                    playback.render_queue.completion_publications.load(Ordering::Acquire),
                    published_before
                );
            } else {
                while playback.render_queue.completion_publications.load(Ordering::Acquire)
                    == published_before
                    && Instant::now() < deadline
                {
                    thread::yield_now();
                }
                assert_eq!(
                    playback.render_queue.completion_publications.load(Ordering::Acquire),
                    published_before + 1,
                    "actual completion published while receiver remains unconsumed"
                );
            }
            // Join only after actual send success/SendError is established.
            // Neither branch polls or removes a queued completion.
            playback.stop_render_worker();
            assert_eq!(renderer.installs.load(Ordering::Acquire), 0);
            assert_eq!(renderer.acks.load(Ordering::Acquire), 0);
            assert!(!renderer.output_shutdown_started.load(Ordering::Acquire));
            if !closed_receiver {
                assert!(
                    request_drops.lock().is_empty(),
                    "unconsumed Work is still held"
                );
                assert!(playback.native_author_handoff_in_flight.is_some());
            }
            drop(renderer);
            let evidence = playback.shutdown_and_wait();
            if panic_on_drop {
                assert_eq!(evidence.foreign_owner_panics, 1, "{evidence:?}");
                assert_eq!(evidence.foreign_owner_abandonments, 0);
                assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
                assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
                assert!(!evidence.all_workers_terminated());
            } else {
                assert!(evidence.all_workers_terminated(), "{evidence:?}");
            }
            assert!(
                weak.upgrade().is_none(),
                "last renderer owner was actually released"
            );
            let request_threads = request_drops.lock().clone();
            assert_eq!(request_threads.len(), 1, "request retired exactly once");
            assert_eq!(
                request_threads[0],
                retirement_thread,
                "completion owner must retire on original retirement thread; closed_receiver={closed_receiver}"
            );
            let (renderer_thread, before_output_shutdown) =
                dropped_on.lock().expect("actual last renderer Drop");
            assert_eq!(renderer_thread, retirement_thread);
            assert_ne!(renderer_thread, caller_thread);
            assert!(
                before_output_shutdown,
                "completion owner must retire before output shutdown and receipt construction"
            );
        }

        #[test]
        fn unconsumed_awaiting_completion_retires_before_shutdown_receipt() {
            completion_owner_shutdown(false, false);
        }

        #[test]
        fn failed_completion_send_retires_owner_on_original_retirement_worker() {
            completion_owner_shutdown(true, false);
        }

        #[test]
        fn completion_drop_panic_is_accounted_before_shutdown_receipt() {
            completion_owner_shutdown(false, true);
        }

        fn stopped_completion_publication(retirement_stopped: bool) {
            let (mut playback, _, renderer, target) = setup(false);
            let retirement_thread = playback
                .render_retirement_worker
                .as_ref()
                .expect("original retirement worker")
                .thread()
                .id();
            let requests = Arc::clone(&renderer.request_drops);
            let weak = Arc::downgrade(&renderer);
            let published_before =
                playback.render_queue.completion_publications.load(Ordering::Acquire);
            renderer.state_entry.store(true, Ordering::Release);
            renderer.pending.store(true, Ordering::Release);
            renderer.policy_gated.store(true, Ordering::Release);
            assert!(playback
                .request_native_author_handoff(renderer.request(target, 19, false))
                .expect("actual held request"));
            let deadline = Instant::now() + Duration::from_secs(2);
            while renderer.policy_queries.load(Ordering::Acquire) == 0 && Instant::now() < deadline
            {
                thread::yield_now();
            }
            assert_eq!(renderer.policy_queries.load(Ordering::Acquire), 1);
            assert_eq!(
                playback.render_queue.completion_publications.load(Ordering::Acquire),
                published_before,
                "actual producer remains inside the policy latch"
            );
            playback.begin_shutdown();
            if retirement_stopped {
                playback.render_retirement_queue.stop();
            }
            renderer.apply_release.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(2);
            while !playback.render_worker.as_ref().expect("actual producer").is_finished()
                && Instant::now() < deadline
            {
                thread::yield_now();
            }
            assert!(playback.render_worker.as_ref().expect("actual producer").is_finished());
            playback.stop_render_worker();
            let published_after =
                playback.render_queue.completion_publications.load(Ordering::Acquire);
            drop(renderer);
            let evidence = playback.shutdown_and_wait();
            assert_eq!(
                published_after, published_before,
                "stopped producer must not publish a late completion"
            );
            if retirement_stopped {
                assert!(evidence.foreign_owner_abandonments > 0, "{evidence:?}");
                assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
                assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
                assert!(
                    requests.lock().is_empty(),
                    "stopped queue must not caller-drop Work"
                );
                assert!(
                    weak.upgrade().is_some(),
                    "abandonment retains the unresolved owner"
                );
            } else {
                assert!(evidence.all_workers_terminated(), "{evidence:?}");
                assert_eq!(*requests.lock(), [retirement_thread]);
                assert!(weak.upgrade().is_none());
            }
        }

        #[test]
        fn stopped_publication_retires_late_completion_on_original_worker() {
            stopped_completion_publication(false);
        }

        #[test]
        fn stopped_retirement_abandons_late_completion_without_caller_drop() {
            stopped_completion_publication(true);
        }

        #[test]
        fn native_scope_media_waits_real_output_token_keeps_same_arc_seek_and_enters_fresh_pcm() {
            let (mut playback, output, renderer, target) = setup(false);
            renderer.state_entry.store(true, Ordering::Release);
            renderer.pending.store(true, Ordering::Release);
            output.lock().auto_confirm_quiescence = false;
            assert!(playback
                .request_native_author_handoff(renderer.request(target, 19, false))
                .expect("one serial queue"));
            wait(&mut playback, |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        AudioPlaybackEvent::NativeAuthorStateEntryRequired { .. }
                    )
                })
            });
            let generation = playback.generation;
            let materialization =
                playback.renderer.as_ref().expect("actual retained source").materialization_id;
            let claim =
                playback.owner_installation.current_claim().expect("source").expect("installed");
            assert_eq!(
                renderer.installs.load(Ordering::Acquire),
                0,
                "no foreign state application while output is alive"
            );
            assert!(!playback
                .finalize_native_control_anchor(generation, sample_position(17))
                .expect("not quiesced"));
            playback
                .prepare(
                    sample_position(17),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("human seek keeps the captured exact Arc and pending native work");
            assert_eq!(
                playback.renderer.as_ref().expect("source").materialization_id,
                materialization
            );
            let current_claim = playback
                .owner_installation
                .current_claim()
                .expect("actual source")
                .expect("unchanged installed slot");
            assert_eq!(current_claim.generation(), claim.generation());
            let verifier = renderer.installed.verifier();
            assert!(verifier.validate(&current_claim).is_ok());
            assert!(verifier.validate(&claim).is_ok());
            assert_eq!(renderer.installs.load(Ordering::Acquire), 0);
            {
                let mut output = output.lock();
                output.confirmed_quiescence_revision = output.quiescence_revision;
            }
            wait(&mut playback, |events| {
                events.iter().any(|event|matches!(event,
                AudioPlaybackEvent::NativeAuthorStateEntryOutputQuiesced {pending_generation,..} if *pending_generation==generation))
            });
            assert!(playback
                .finalize_native_control_anchor(generation, sample_position(17))
                .expect("actual token and current user anchor"));
            wait(&mut playback, |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        AudioPlaybackEvent::NativeAuthorHandoffObserved {
                            status: AudioPcmNativeAuthorHandoffStatus::Acknowledged {
                                continuity:
                                    AudioPcmNativeAuthorHandoffContinuity::StateEntryRequired,
                                ..
                            },
                            ..
                        }
                    )
                })
            });
            let installed = renderer.target.lock().expect("actual target");
            assert_eq!(renderer.installs.load(Ordering::Acquire), 1);
            assert_eq!(renderer.acks.load(Ordering::Acquire), 1);
            assert!(playback
                .finish_native_author_state_entry(installed, sample_position(17))
                .expect("exact accepted fresh entry"));
            poll_until_settled(&mut playback, sample_position(17));
            assert_eq!(
                renderer.enters.load(Ordering::Acquire),
                2,
                "explicit discontinuity has one fresh entry"
            );
            assert!(
                renderer.processed.lock().iter().any(|request| request.start_sample == 17
                    && matches!(request.continuity, AudioPcmContinuity::Enter(_)))
            );
            assert_eq!(playback.render_substitution_count, 0);
            assert_eq!(
                playback.renderer.as_ref().expect("same source").materialization_id,
                materialization
            );
            assert!(!playback
                .finish_native_author_state_entry(installed, sample_position(17))
                .expect("one-shot completion"));
        }
        #[test]
        fn native_scope_media_actual_source_replacement_cancels_original_work_without_install() {
            let (mut playback, output, renderer, target) = setup(false);
            renderer.state_entry.store(true, Ordering::Release);
            renderer.pending.store(true, Ordering::Release);
            output.lock().auto_confirm_quiescence = false;
            assert!(playback
                .request_native_author_handoff(renderer.request(target, 19, false))
                .expect("queue"));
            wait(&mut playback, |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        AudioPlaybackEvent::NativeAuthorStateEntryRequired { .. }
                    )
                })
            });
            let replacement = Renderer::new(false);
            playback
                .prepare(
                    sample_position(0),
                    replacement,
                    AudioPcmContinuityModel::GenerationState,
                )
                .expect("actual different source replaces the retained slot");
            {
                let mut output = output.lock();
                output.confirmed_quiescence_revision = output.quiescence_revision;
            }
            settle_native_installation(&mut playback);
            for _ in 0..3 {
                playback
                    .poll(AudioPlaybackMode::Idle, sample_position(0))
                    .expect("old completion settles");
            }
            assert_eq!(renderer.installs.load(Ordering::Acquire), 0);
            assert_eq!(renderer.acks.load(Ordering::Acquire), 0);
            assert!(playback.native_author_state_entry_target().is_none());
            assert_eq!(playback.render_substitution_count, 0);
        }
        #[test]
        fn native_author_preserved_ack_does_not_resume_a_suspended_pcm_generation() {
            struct ReleaseApply(Arc<Renderer>);
            impl Drop for ReleaseApply {
                fn drop(&mut self) {
                    self.0.apply_release.store(true, Ordering::Release);
                }
            }
            let (mut playback, output) = playback_with_buffered_frames(0);
            let renderer = Renderer::new(true);
            let release = ReleaseApply(Arc::clone(&renderer));
            let deadline = Instant::now() + Duration::from_secs(2);
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
                )
                .unwrap();
            while Instant::now() < deadline {
                playback.poll(AudioPlaybackMode::Idle, sample_position(0)).unwrap();
                if playback.native_owner_binding_status().is_some() {
                    break;
                }
                thread::yield_now();
            }
            assert!(playback.native_owner_binding_status().is_some());
            let target = renderer.address();
            renderer.pending.store(true, Ordering::Release);
            while Instant::now() < deadline {
                playback.poll(AudioPlaybackMode::Consume, sample_position(0)).unwrap();
                if matches!(
                    playback.native_control_hold,
                    Some(NativeControlHold::AuthorPending { first_unrendered_sample: Some(0), .. })
                ) {
                    break;
                }
                thread::yield_now();
            }
            assert!(
                matches!(
                    playback.native_control_hold,
                    Some(NativeControlHold::AuthorPending { first_unrendered_sample: Some(0), .. })
                ),
                "actual deferred PCM establishes the first unrendered coordinate"
            );
            assert_eq!(renderer.refused.lock()[0].start_sample, 0);
            assert!(playback
                .request_native_author_handoff(renderer.request(target, 1, false))
                .unwrap());
            while !renderer.apply_entered.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(
                renderer.apply_entered.load(Ordering::Acquire),
                "real queued author apply is in flight"
            );
            let materialization = playback.renderer.as_ref().unwrap().materialization_id;
            let old_claim = renderer.installed.verifier().current_claim().unwrap();
            let old_pcm = playback.generation_cancellation.clone();
            let old_native = playback.native_control_cancellation.clone();
            let old_generation = playback.generation;
            {
                let mut state = output.lock();
                state.queued_frames = 11;
                let snapshot = state.snapshot.as_mut().unwrap();
                snapshot.active = true;
                snapshot.buffered_frames = 11;
            }
            let idle = playback.poll(AudioPlaybackMode::Idle, sample_position(5));
            if let Err(error) = idle {
                assert_eq!(
                    error,
                    AudioPlaybackError::FreshEntryOriginMismatch {
                        required: sample_position(0),
                        actual: sample_position(5),
                    }
                );
            }
            assert!(old_pcm.is_canceled());
            assert_ne!(playback.generation, old_generation);
            assert!(!old_native.is_canceled());
            assert!(renderer.installed.verifier().validate(&old_claim).is_ok());
            assert_eq!(
                playback.renderer.as_ref().unwrap().materialization_id,
                materialization
            );
            assert_eq!(playback.generation_render_anchor, None);
            assert!(playback.render_blocked);
            assert!(!output.lock().snapshot.unwrap().active);
            let suspended_next = playback.next_start_sample;
            assert_ne!(
                suspended_next, 0,
                "Suspend must not schedule the legal origin as an implicit replay"
            );
            release.0.apply_release.store(true, Ordering::Release);
            let mut events = Vec::new();
            while Instant::now() < deadline {
                events.extend(
                    playback.poll(AudioPlaybackMode::Idle, sample_position(5)).unwrap().events,
                );
                if events.iter().any(|event| {
                    matches!(
                        event,
                        AudioPlaybackEvent::NativeAuthorHandoffObserved {
                            status: AudioPcmNativeAuthorHandoffStatus::Acknowledged {
                                continuity: AudioPcmNativeAuthorHandoffContinuity::Preserved,
                                ..
                            },
                            ..
                        }
                    )
                }) {
                    break;
                }
                thread::yield_now();
            }
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    AudioPlaybackEvent::NativeAuthorHandoffObserved {
                        status: AudioPcmNativeAuthorHandoffStatus::Acknowledged {
                            continuity: AudioPcmNativeAuthorHandoffContinuity::Preserved,
                            ..
                        },
                        ..
                    }
                )),
                "actual apply/commit/Ack must finish on the original serial queue"
            );
            assert_eq!(renderer.installs.load(Ordering::Acquire), 1);
            assert_eq!(renderer.acks.load(Ordering::Acquire), 1);
            assert_eq!(renderer.enters.load(Ordering::Acquire), 0);
            assert!(renderer.processed.lock().is_empty());
            assert_eq!(
                playback.next_start_sample, suspended_next,
                "Ack is not permission to rewind or render"
            );
            assert_eq!(playback.generation_render_anchor, None);
            assert!(playback.render_blocked);
            assert_eq!(
                playback.renderer.as_ref().unwrap().materialization_id,
                materialization
            );
            let current = renderer.target.lock().unwrap();
            assert_eq!(
                renderer.installed.verifier().current_claim().unwrap().generation(),
                current.installed_generation
            );
            assert!(!old_native.is_canceled());
            assert!(!output.lock().snapshot.unwrap().active);
            assert_eq!(output.lock().queued_frames, 0);
            // Check retained token identity only after successful real Ack.
            old_native.cancel();
            assert!(playback.native_control_cancellation.is_canceled());
            assert!(playback.shutdown_and_wait().all_workers_terminated());

            // A real Preserved Ack may finish before the output callback
            // acknowledges deactivation. Repeated pause/paused-seek must keep
            // that separate retirement obligation even after AuthorPending ends.
            {
                let (mut playback, output) = playback_with_buffered_frames(0);
                output.lock().auto_confirm_quiescence = false;
                let renderer = Renderer::new(true);
                let release = ReleaseApply(Arc::clone(&renderer));
                let deadline = Instant::now() + Duration::from_secs(2);
                playback
                    .prepare(
                        sample_position(0),
                        renderer.clone(),
                        AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
                    )
                    .unwrap();
                while Instant::now() < deadline {
                    playback.poll(AudioPlaybackMode::Idle, sample_position(0)).unwrap();
                    if playback.native_owner_binding_status().is_some() {
                        break;
                    }
                    thread::yield_now();
                }
                assert_eq!(
                    playback.native_owner_binding_status(),
                    Some(AudioPcmNativeOwnerBindingStatus::Ready)
                );
                let materialization = playback.renderer.as_ref().unwrap().materialization_id;
                let native_token = playback.native_control_cancellation.clone();
                let target = renderer.address();
                renderer.pending.store(true, Ordering::Release);
                while Instant::now() < deadline {
                    playback.poll(AudioPlaybackMode::Consume, sample_position(0)).unwrap();
                    if matches!(
                        playback.native_control_hold,
                        Some(NativeControlHold::AuthorPending {
                            first_unrendered_sample: Some(0),
                            ..
                        })
                    ) {
                        break;
                    }
                    thread::yield_now();
                }
                assert!(matches!(
                    playback.native_control_hold,
                    Some(NativeControlHold::AuthorPending { first_unrendered_sample: Some(0), .. })
                ));
                assert!(playback
                    .request_native_author_handoff(renderer.request(target, 1, false))
                    .unwrap());
                while !renderer.apply_entered.load(Ordering::Acquire) && Instant::now() < deadline {
                    thread::yield_now();
                }
                assert!(renderer.apply_entered.load(Ordering::Acquire));
                {
                    let mut state = output.lock();
                    state.queued_frames = 11;
                    let snapshot = state.snapshot.as_mut().unwrap();
                    snapshot.active = true;
                    snapshot.buffered_frames = 11;
                }
                playback.suspend(sample_position(5)).unwrap();
                let first_retirement =
                    playback.native_control_output_retirement.expect("actual output obligation");
                let first_token = first_retirement.token.expect("actual deactivation token");
                assert_eq!(first_retirement.generation, playback.generation);
                assert!(!first_retirement.acknowledged);
                assert!(output.lock().confirmed_quiescence_revision < first_token.revision);
                assert!(!playback.output_generation_ready);
                assert!(playback.native_control_hold.is_some());
                release.0.apply_release.store(true, Ordering::Release);
                let mut events = Vec::new();
                while Instant::now() < deadline {
                    events.extend(
                        playback.poll(AudioPlaybackMode::Idle, sample_position(5)).unwrap().events,
                    );
                    if renderer.acks.load(Ordering::Acquire) == 1
                        && playback.native_control_hold.is_none()
                    {
                        break;
                    }
                    thread::yield_now();
                }
                assert!(
                    events.iter().any(|event| matches!(
                        event,
                        AudioPlaybackEvent::NativeAuthorHandoffObserved {
                            status: AudioPcmNativeAuthorHandoffStatus::Acknowledged {
                                continuity: AudioPcmNativeAuthorHandoffContinuity::Preserved,
                                ..
                            },
                            ..
                        }
                    )),
                    "real queued apply/commit/Ack completed before callback acknowledgement"
                );
                assert_eq!(renderer.installs.load(Ordering::Acquire), 1);
                assert_eq!(renderer.acks.load(Ordering::Acquire), 1);
                assert!(playback.native_control_hold.is_none());
                let outstanding = playback
                    .native_control_output_retirement
                    .expect("Ack must not erase the unacknowledged output token");
                assert_eq!(outstanding.token, Some(first_token));
                assert!(!outstanding.acknowledged);
                assert!(output.lock().confirmed_quiescence_revision < first_token.revision);
                let old_work_unsettled = playback.pcm_journal.old_work_unsettled();
                let previous_generation = playback.generation;
                playback.suspend(sample_position(7)).unwrap();
                assert_ne!(playback.generation, previous_generation);
                let repeated = playback
                    .native_control_output_retirement
                    .expect("repeat Suspend must carry the pending output obligation");
                let current_token = repeated.token.expect("current real deactivation token");
                assert_eq!(repeated.generation, playback.generation);
                assert_eq!(repeated.token, playback.quiescence_token);
                assert_eq!(repeated.stream_generation, outstanding.stream_generation);
                assert_eq!(repeated.media_anchor, outstanding.media_anchor);
                assert!(!repeated.acknowledged);
                assert!(output.lock().confirmed_quiescence_revision < first_token.revision);
                assert!(output.lock().confirmed_quiescence_revision < current_token.revision);
                assert_eq!(
                    playback.pcm_journal.old_work_unsettled(),
                    old_work_unsettled
                );
                assert_eq!(playback.generation_render_anchor, None);
                assert!(playback.render_blocked);
                assert_eq!(playback.next_start_sample, 7);
                assert!(!output.lock().snapshot.unwrap().active);
                playback.poll(AudioPlaybackMode::Idle, sample_position(7)).unwrap();
                let still_pending = playback
                    .native_control_output_retirement
                    .expect("poll cannot manufacture callback acknowledgement");
                assert_eq!(still_pending.generation, playback.generation);
                assert!(!still_pending.acknowledged);
                assert!(!playback.output_generation_ready);
                assert_eq!(
                    playback.pcm_journal.old_work_unsettled(),
                    old_work_unsettled
                );
                // This changes only the existing fake output's callback witness,
                // never the native author result, claim, token or journal state.
                {
                    let mut state = output.lock();
                    assert_eq!(
                        state.snapshot.unwrap().stream_generation,
                        current_token.stream_generation
                    );
                    assert_eq!(state.quiescence_revision, current_token.revision);
                    state.confirmed_quiescence_revision = current_token.revision;
                }
                while Instant::now() < deadline {
                    playback.poll(AudioPlaybackMode::Idle, sample_position(7)).unwrap();
                    if playback.native_control_output_retirement.is_none()
                        && !playback.pcm_journal.old_work_unsettled()
                    {
                        break;
                    }
                    thread::yield_now();
                }
                assert!(playback.native_control_output_retirement.is_none());
                assert!(!playback.pcm_journal.old_work_unsettled());
                assert!(playback.output_generation_ready);
                assert_eq!(playback.generation_render_anchor, None);
                assert!(playback.render_blocked);
                assert_eq!(playback.next_start_sample, 7);
                assert_eq!(
                    playback.renderer.as_ref().unwrap().materialization_id,
                    materialization
                );
                assert_eq!(renderer.enters.load(Ordering::Acquire), 0);
                assert!(renderer.processed.lock().is_empty());
                assert!(!native_token.is_canceled());
                assert!(!output.lock().snapshot.unwrap().active);
                assert_eq!(output.lock().queued_frames, 0);
                assert!(playback.shutdown_and_wait().all_workers_terminated());
            }
        }

        #[test]
        fn native_author_media_queued_deferred_windows_ack_and_continue_without_entry_or_generation_change(
        ) {
            let (mut playback, output, renderer, target) = setup(false);
            let generation = playback.generation;
            renderer.pending.store(true, Ordering::Release);
            {
                let mut output = output.lock();
                output.queued_frames = 0;
                output.snapshot.as_mut().expect("actual fake output").buffered_frames = 0;
            }
            wait(&mut playback, |_| !renderer.refused.lock().is_empty());
            assert!(playback
                .request_native_author_handoff(renderer.request(target, 1, false))
                .expect("same queue admission"));
            wait(&mut playback, |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        AudioPlaybackEvent::NativeAuthorHandoffObserved {
                            status: AudioPcmNativeAuthorHandoffStatus::Acknowledged { .. },
                            ..
                        }
                    )
                })
            });
            poll_until_settled(&mut playback, sample_position(0));
            assert_eq!(playback.generation, generation);
            assert_eq!(renderer.enters.load(Ordering::Acquire), 1);
            assert_eq!(renderer.installs.load(Ordering::Acquire), 1);
            assert_eq!(renderer.acks.load(Ordering::Acquire), 1);
            assert_eq!(playback.render_substitution_count, 0);
            assert_eq!(playback.render_generation_recovery_count, 0);
            let held = renderer.refused.lock()[0].start_sample;
            assert!(
                renderer.processed.lock().iter().any(|request| request.start_sample == held),
                "the first held coordinate must actually resume"
            );
        }
        #[test]
        fn native_author_media_observed_attachment_closes_after_same_object_claim_rebinding() {
            let (mut playback, _output, renderer, target) = setup(false);
            assert_eq!(
                playback
                    .request_native_audio_editor(Intent::Open {
                        target,
                        title: mondrian_core::NativeAudioEditorTitle::new(
                            "portable actual owner".into()
                        )
                        .expect("bounded title"),
                    })
                    .expect("actual serial Open"),
                Submission::Queued
            );
            wait(&mut playback, |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        AudioPlaybackEvent::NativeEditorObserved {
                            outcome: Outcome::Opened { .. },
                            ..
                        }
                    )
                })
            });
            renderer.pending.store(true, Ordering::Release);
            assert!(playback
                .request_native_author_handoff(renderer.request(target, 1, false))
                .expect("same object author rebind"));
            wait(&mut playback, |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        AudioPlaybackEvent::NativeAuthorHandoffObserved {
                            status: AudioPcmNativeAuthorHandoffStatus::Acknowledged { .. },
                            ..
                        }
                    )
                })
            });
            let current = renderer.target.lock().expect("current actual target");
            assert_ne!(target.installed_generation, current.installed_generation);
            assert_eq!(
                playback
                    .request_native_audio_editor(Intent::Close { target })
                    .expect("retained observed original attachment"),
                Submission::Queued
            );
            wait(&mut playback, |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        AudioPlaybackEvent::NativeEditorObserved {
                            outcome: Outcome::Closed { .. },
                            ..
                        }
                    )
                })
            });
            assert_eq!(renderer.closes.load(Ordering::Acquire), 1);
            assert!(playback.native_editor_attachment.is_none());
            assert_eq!(
                playback
                    .request_native_audio_editor(Intent::Close { target })
                    .expect("no retained attachment remains"),
                Submission::Refused(mondrian_core::NativeAudioEditorControlRefusal::OwnerChanged)
            );
        }

        #[test]
        fn native_author_media_original_queued_close_survives_same_object_plan_claim_replacement() {
            let (mut playback, _output, renderer, target) = setup(true);
            let generation = playback.generation;
            renderer.pending.store(true, Ordering::Release);
            assert!(playback
                .request_native_author_handoff(renderer.request(target, 1, false))
                .expect("handoff queue"));
            let deadline = Instant::now() + Duration::from_secs(2);
            while !renderer.apply_entered.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::yield_now();
            }
            assert!(renderer.apply_entered.load(Ordering::Acquire));
            assert_eq!(
                playback
                    .request_native_audio_editor(Intent::Close { target })
                    .expect("same attachment Close behind handoff"),
                Submission::Queued
            );
            renderer.apply_release.store(true, Ordering::Release);
            let events = wait(&mut playback, |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        AudioPlaybackEvent::NativeEditorObserved {
                            outcome: Outcome::Closed { .. },
                            ..
                        }
                    )
                })
            });
            assert!(events.iter().any(|event| matches!(
                event,
                AudioPlaybackEvent::NativeAuthorHandoffObserved {
                    status: AudioPcmNativeAuthorHandoffStatus::Acknowledged { .. },
                    ..
                }
            )));
            assert_eq!(renderer.closes.load(Ordering::Acquire), 1);
            assert_eq!(playback.generation, generation);
            assert!(renderer
                .target
                .lock()
                .is_some_and(|current| current.binding != target.binding));
            assert_eq!(renderer.enters.load(Ordering::Acquire), 1);
        }
    }
    mod native_child_settlement_contracts {
        use super::*;
        use mondrian_core::{
            NativeChildSettlementError as SettlementError,
            NativeChildSettlementRegistrar as Registrar,
        };

        // Intentionally omits aggregate forwarding, even though its inner host
        // adapter is trusted. A capability query must not turn this into NA.
        struct UnknownRenderer(RecordingRenderer);
        impl AudioPcmRenderer for UnknownRenderer {
            fn render(
                &self,
                request: AudioPcmRenderRequest,
                cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
                self.0.render(request, cancellation)
            }
        }

        // Portable ownership model only: it retains the actual counted scope,
        // never a Child, Runtime or fake native-process success statement.
        struct RetainedScopeRenderer {
            pcm: RecordingRenderer,
            registrar: Mutex<Option<Registrar>>,
        }
        impl AudioPcmRenderer for RetainedScopeRenderer {
            fn bind_native_child_settlement(
                &self,
                registrar: &Registrar,
            ) -> Result<(), SettlementError> {
                let mut current = self.registrar.lock();
                if let Some(current) = current.as_ref() {
                    if current.same_domain(registrar) {
                        return Ok(());
                    }
                    current.mark_unavailable();
                    registrar.mark_unavailable();
                    return Err(SettlementError::ForeignDomain);
                }
                *current = Some(registrar.try_fork()?);
                Ok(())
            }
            fn render(
                &self,
                request: AudioPcmRenderRequest,
                cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
                self.pcm.render(request, cancellation)
            }
        }

        fn pcm(requests: &Arc<Mutex<Vec<AudioPcmRenderRequest>>>) -> RecordingRenderer {
            RecordingRenderer {
                requests: requests.clone(),
                wrong_frame_count: false,
            }
        }

        #[test]
        fn cold_registrar_uses_original_prepare_domain_and_counts_uninstalled_owner() {
            let (output, _) = fake_output();
            let mut playback = AudioPlayback::with_output(test_config(), output).unwrap();
            let cold = playback.native_child_settlement_registrar().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let renderer = Arc::new(RetainedScopeRenderer {
                pcm: pcm(&requests),
                registrar: Mutex::new(Some(cold)),
            });
            // The actual prepare observer must use this same original tracker.
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::IndependentWindows,
                )
                .unwrap();
            let witness = playback.native_child_settlement_registrar().unwrap();
            assert!(renderer.registrar.lock().as_ref().unwrap().same_domain(&witness));
            drop(witness);
            let evidence = playback.shutdown_and_wait();
            assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
            assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
            assert_eq!(evidence.foreign_owner_panics, 0);
            assert_eq!(evidence.foreign_owner_abandonments, 0);
            drop(renderer);
            assert!(!evidence.all_workers_terminated());
        }

        #[test]
        fn cold_registrar_acquisition_failure_keeps_original_pcm_admission() {
            let (output, _) = fake_output();
            let mut playback = AudioPlayback::with_output(test_config(), output).unwrap();
            assert!(playback.foreign_owner_retirements.seal());
            assert!(matches!(
                playback.native_child_settlement_registrar(),
                Err(SettlementError::Closed)
            ));
            let requests = Arc::new(Mutex::new(Vec::new()));
            playback
                .prepare(
                    sample_position(0),
                    Arc::new(pcm(&requests)),
                    AudioPcmContinuityModel::IndependentWindows,
                )
                .unwrap();
            poll_until_settled(&mut playback, sample_position(0));
            assert!(!requests.lock().is_empty());
            let evidence = playback.shutdown_and_wait();
            assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
            assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
            assert_eq!(evidence.foreign_owner_panics, 0);
            assert_eq!(evidence.foreign_owner_abandonments, 0);
        }

        #[test]
        fn aggregate_unknown_is_unavailable_without_changing_pcm_admission() {
            for unknown in [false, true] {
                let (output, _) = fake_output();
                let mut playback = AudioPlayback::with_output(test_config(), output).unwrap();
                let requests = Arc::new(Mutex::new(Vec::new()));
                let renderer: Arc<dyn AudioPcmRenderer> = if unknown {
                    Arc::new(UnknownRenderer(pcm(&requests)))
                } else {
                    Arc::new(pcm(&requests))
                };
                playback
                    .prepare(
                        sample_position(0),
                        renderer,
                        AudioPcmContinuityModel::IndependentWindows,
                    )
                    .expect("observation does not alter Source admission");
                poll_until_settled(&mut playback, sample_position(0));
                assert!(!requests.lock().is_empty(), "same ordinary PCM execution");
                let evidence = playback.shutdown_and_wait();
                assert_eq!(evidence.foreign_owner_panics, 0);
                assert_eq!(evidence.foreign_owner_abandonments, 0);
                assert_eq!(evidence.all_workers_terminated(), !unknown);
                assert_eq!(
                    evidence.shutdown_resource_facts_complete_at_deadline,
                    !unknown
                );
                assert_eq!(
                    evidence.shutdown_owner_lifetime_unresolved_at_deadline,
                    unknown
                );
            }
        }

        #[test]
        fn external_renderer_scope_keeps_shutdown_unresolved() {
            let (output, _) = fake_output();
            let mut playback = AudioPlayback::with_output(test_config(), output).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let renderer = Arc::new(RetainedScopeRenderer {
                pcm: pcm(&requests),
                registrar: Mutex::new(None),
            });
            let tracker = playback.foreign_owner_retirements.clone();
            playback
                .prepare(
                    sample_position(0),
                    renderer.clone(),
                    AudioPcmContinuityModel::IndependentWindows,
                )
                .unwrap();
            poll_until_settled(&mut playback, sample_position(0));
            let evidence = playback.shutdown_and_wait();
            assert!(!evidence.all_workers_terminated());
            assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
            assert!(
                !tracker.seal(),
                "same external Arc still holds its prospective scope"
            );
            drop(renderer);
            assert!(
                tracker.seal(),
                "real final Arc Drop ends that scope after sealing"
            );
            assert!(
                !evidence.all_workers_terminated(),
                "receipt never changes retrospectively"
            );
        }

        #[test]
        fn same_arc_rebind_preserves_original_domain_and_sticky_failure() {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let renderer = Arc::new(RetainedScopeRenderer {
                pcm: pcm(&requests),
                registrar: Mutex::new(None),
            });
            let (output_a, _) = fake_output();
            let mut a = AudioPlayback::with_output(test_config(), output_a).unwrap();
            a.prepare(
                sample_position(0),
                renderer.clone(),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .unwrap();
            // A repeated same-source prepare reaches observation again without
            // minting a replacement domain for the retained renderer.
            a.prepare(
                sample_position(0),
                renderer.clone(),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .unwrap();
            let old = a.foreign_owner_retirements.clone();
            let old_domain = Registrar::try_new(old.clone()).expect("original domain witness");
            let (output_b, _) = fake_output();
            let mut b = AudioPlayback::with_output(test_config(), output_b).unwrap();
            let new = b.foreign_owner_retirements.clone();
            b.prepare(
                sample_position(0),
                renderer.clone(),
                AudioPcmContinuityModel::IndependentWindows,
            )
            .expect("observation rejection alone does not change existing Source policy");
            let original = renderer.registrar.lock();
            assert!(original
                .as_ref()
                .expect("original registrar remains")
                .same_domain(&old_domain));
            let raw: Arc<dyn mondrian_core::NativeChildSettlementReporter> = old.clone();
            assert!(matches!(
                Registrar::try_new(raw),
                Err(SettlementError::Unavailable)
            ));
            assert!(!new.seal());
            drop(original);
            drop(old_domain);
            drop(renderer);
            let a_receipt = a.shutdown_and_wait();
            let b_receipt = b.shutdown_and_wait();
            assert!(!a_receipt.all_workers_terminated());
            assert!(!b_receipt.all_workers_terminated());
            assert!(!old.seal(), "foreign rebind cannot clear the old failure");
        }

        #[test]
        fn unknown_binding_is_observed_before_both_prepare_retirement_paths() {
            for continuity in [
                AudioPcmContinuityModel::IndependentWindows,
                AudioPcmContinuityModel::GenerationStateFromOrigin(sample_position(0)),
            ] {
                let (output, _) = fake_output();
                let mut playback = AudioPlayback::with_output(test_config(), output).unwrap();
                let anchor = if continuity == AudioPcmContinuityModel::IndependentWindows {
                    AudioSamplePosition::new(0, AudioSampleRate::new(2_000).unwrap())
                } else {
                    sample_position(1)
                };
                let result = playback.prepare(
                    anchor,
                    Arc::new(UnknownRenderer(pcm(&Arc::new(Mutex::new(Vec::new()))))),
                    continuity,
                );
                let expected = if continuity == AudioPcmContinuityModel::IndependentWindows {
                    AudioPlaybackError::InvalidSampleAnchor(AudioTimeError::RateMismatch {
                        left: 2_000,
                        right: 1_000,
                    })
                } else {
                    AudioPlaybackError::FreshEntryOriginMismatch {
                        required: sample_position(0),
                        actual: sample_position(1),
                    }
                };
                assert_eq!(result, Err(expected), "original refusal is preserved");
                let evidence = playback.shutdown_and_wait();
                assert!(!evidence.all_workers_terminated());
                assert!(!evidence.shutdown_resource_facts_complete_at_deadline);
                assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
            }
        }

        struct PanickingAggregate {
            opaque: bool,
            payload_dropped: Arc<AtomicBool>,
        }
        impl AudioPcmRenderer for PanickingAggregate {
            fn bind_native_child_settlement(
                &self,
                _registrar: &Registrar,
            ) -> Result<(), SettlementError> {
                if self.opaque {
                    std::panic::panic_any(DropProbe { dropped: self.payload_dropped.clone() });
                }
                panic!("controlled aggregate binding panic");
            }
            fn render(
                &self,
                request: AudioPcmRenderRequest,
                _cancellation: &ExecutionCancellationToken,
            ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
                Ok(AudioPcmRenderOutcome::Rendered(AudioBuffer::silent(
                    request.sample_rate,
                    request.channel_layout,
                    request.frame_count,
                )))
            }
        }

        #[test]
        fn aggregate_binding_panic_is_caught_and_keeps_foreign_evidence() {
            for opaque in [false, true] {
                let (output, _) = fake_output();
                let mut playback = AudioPlayback::with_output(test_config(), output).unwrap();
                let payload_dropped = Arc::new(AtomicBool::new(false));
                playback
                    .prepare(
                        sample_position(0),
                        Arc::new(PanickingAggregate {
                            opaque,
                            payload_dropped: payload_dropped.clone(),
                        }),
                        AudioPcmContinuityModel::IndependentWindows,
                    )
                    .expect("observer panic alone does not revoke Source admission");
                poll_until_settled(&mut playback, sample_position(0));
                let evidence = playback.shutdown_and_wait();
                assert_eq!(evidence.foreign_owner_panics, 1);
                assert_eq!(evidence.foreign_owner_abandonments, u32::from(opaque));
                assert!(!evidence.all_workers_terminated());
                assert!(evidence.shutdown_owner_lifetime_unresolved_at_deadline);
                assert!(!payload_dropped.load(Ordering::Acquire));
            }
        }
    }
}
