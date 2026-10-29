//! Format-neutral native audio editor control addresses and evidence.
//!
//! These are read-only facts and intents. They neither construct an installed
//! native lease nor authorize native parameter callbacks or author transactions.

use crate::{
    AudioComponentEditId, AudioPreparedPlanId, AudioProcessingScopeId, AudioProcessorBindingId,
    AudioProcessorInstanceId, AudioRenderSessionId, MixBusId, ProgramOutputId, TrackId,
};
use std::num::NonZeroU64;

/// Maximum UTF-8 title bytes on the bounded native editor control stream.
pub const NATIVE_AUDIO_EDITOR_TITLE_BYTES: usize = 512;

/// One admitted product title with a bounded owned allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudioEditorTitle(Box<str>);

impl NativeAudioEditorTitle {
    /// Admit exact UTF-8 bytes and compact the owned allocation before queuing.
    /// Reject over-limit/NUL input; never silently truncate the product title.
    pub fn new(title: String) -> Result<Self, NativeAudioEditorControlRefusal> {
        if title.len() > NATIVE_AUDIO_EDITOR_TITLE_BYTES || title.contains('\0') {
            return Err(NativeAudioEditorControlRefusal::InvalidTitle);
        }
        Ok(Self(title.into_boxed_str()))
    }

    /// Exact admitted title displayed by the actual native parent.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact author owner of one generated native processor occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioEditorOccurrenceOwner {
    /// One placement-local materialization of a shared processing scope.
    Contribution {
        /// Actual component edit in that placement.
        edit_id: AudioComponentEditId,
        /// Actual shared processing definition.
        scope_id: AudioProcessingScopeId,
    },
    /// Actual track channel strip.
    Track(TrackId),
    /// Actual mix bus channel strip.
    Bus(MixBusId),
    /// Actual program output channel strip.
    Output(ProgramOutputId),
}

/// Exact rack insertion address, independent of caption or ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioEditorInsertion {
    /// Shared processing scope rack.
    Scope,
    /// Channel strip before its fader.
    PreFader,
    /// Channel strip after its fader.
    PostFader,
}

/// An explicit authored occurrence chosen by the product editor surface.
/// No runtime/binding/generation is invented by a UI projection. The installed
/// Runtime resolves this metadata to exactly one actual Hosted record; absent
/// or ambiguous occurrences refuse before native work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioEditorLocator {
    /// Exact author Sequence retained by that actual local Runtime.
    pub sequence: crate::SequenceId,
    /// Stable author processor in the selected occurrence.
    pub instance: AudioProcessorInstanceId,
    /// Exact occurrence, without owner-kind or Component ordinal guessing.
    pub owner: NativeAudioEditorOccurrenceOwner,
    /// Exact insertion in that authored occurrence.
    pub insertion: NativeAudioEditorInsertion,
}

impl NativeAudioEditorLocator {
    /// Compare only authored address facts, not runtime authority.
    pub fn matches_target(self, target: NativeAudioEditorTarget) -> bool {
        self.instance == target.instance
            && self.owner == target.owner
            && self.insertion == target.insertion
    }
}

/// Read-only projection of one currently published installed native record.
///
/// Public fields allow transport without an Audio dependency. Filling these
/// facts cannot mint permission: the sole Runtime must resolve and compare its
/// existing opaque lease, claim and actual child object before each SDK call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioEditorTarget {
    /// Actual root preparation owning the installed renderer.
    pub root_plan: AudioPreparedPlanId,
    /// Actual exclusive root session.
    pub root_session: AudioRenderSessionId,
    /// Actual root or nested local preparation.
    pub local_plan: AudioPreparedPlanId,
    /// Actual exclusive local session.
    pub local_session: AudioRenderSessionId,
    /// Stable author processor instance in the exact occurrence.
    pub instance: AudioProcessorInstanceId,
    /// Exact generated occurrence owner.
    pub owner: NativeAudioEditorOccurrenceOwner,
    /// Exact rack insertion.
    pub insertion: NativeAudioEditorInsertion,
    /// Exact current continuity metadata, absent before native state entry.
    pub continuity_epoch: Option<u64>,
    /// Current parent-issued same-object binding incarnation.
    pub binding: AudioProcessorBindingId,
    /// Actual published renderer-slot installation generation.
    pub installed_generation: NonZeroU64,
}

/// One closed native editor control intent on the existing serial render worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeAudioEditorControlIntent {
    /// Resolve one selected author address and open its actual installed view
    /// in one operation on the existing serial worker. Locator is metadata;
    /// the returned successful target comes from the actual Hosted lease.
    ResolveAndOpen {
        /// Exact selected occurrence; cannot manufacture an installed binding.
        locator: NativeAudioEditorLocator,
        /// Admitted product title for the actual native parent.
        title: NativeAudioEditorTitle,
    },
    /// Attach the actual retained controller's view; no second instance.
    Open {
        /// Address facts, never a native or author permission grant.
        target: NativeAudioEditorTarget,
        /// Product-owned title admitted by the native transport boundary.
        title: NativeAudioEditorTitle,
    },
    /// Close the exact current attachment without an author mutation.
    Close {
        /// Exact current address, compared again at execution.
        target: NativeAudioEditorTarget,
    },
}

impl NativeAudioEditorControlIntent {
    /// Already resolved metadata for early queue refusal. ResolveAndOpen has
    /// no manufactured target; its exact target is resolved by the Runtime.
    pub const fn expected_target(&self) -> Option<NativeAudioEditorTarget> {
        match self {
            Self::Open { target, .. } | Self::Close { target } => Some(*target),
            Self::ResolveAndOpen { .. } => None,
        }
    }
}

/// Explicit editor refusal leaves valid DSP and author history unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioEditorControlRefusal {
    /// No actual native editor adapter is installed.
    UnsupportedAdapter,
    /// No currently installed native lease matches this address.
    NotInstalled,
    /// A root/local session, binding, load or continuity changed.
    OwnerChanged,
    /// The same actual controller provides no native editor.
    NoEditor,
    /// Coherent configuration or native attachment recovery is required.
    ConfigurationPending,
    /// Product title violates the bounded native request contract.
    InvalidTitle,
    /// The same work generation was cancelled before completion.
    Cancelled,
    /// The existing serial worker cannot accept the control.
    WorkerUnavailable,
    /// Native control failed; no successful editor evidence is asserted.
    ProcessorRejected,
}

/// Unfinished detach observed from the actual native attachment model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioEditorDetachStage {
    /// Removal refused; no second removed callback is legal.
    RemoveView,
    /// Removal succeeded; only clearing the same host frame may retry.
    ClearHostFrame,
}

/// Evidence from actual native work, separate from author and DSP outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioEditorControlOutcome {
    /// The same actual bound controller's view is attached to a real parent.
    Opened {
        /// Exact admitted current target.
        target: NativeAudioEditorTarget,
        /// Actual positive native width.
        width: u32,
        /// Actual positive native height.
        height: u32,
    },
    /// View and frame detached, then the actual parent was destroyed.
    Closed {
        /// Exact admitted current target.
        target: NativeAudioEditorTarget,
    },
    /// No successful native operation is asserted.
    Refused(NativeAudioEditorControlRefusal),
    /// The actual owner retains its unresolved attachment.
    ClosePending {
        /// Exact native object whose cleanup remains incomplete.
        target: NativeAudioEditorTarget,
        /// Canonical failed detach step, never a guessed close success.
        stage: NativeAudioEditorDetachStage,
    },
}

/// Queue admission facts, never native success or author permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeAudioEditorControlSubmission {
    /// The existing bounded serial worker accepted this exact intent.
    Queued,
    /// No native work was admitted.
    Refused(NativeAudioEditorControlRefusal),
}
