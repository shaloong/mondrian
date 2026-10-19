//! The single audio author-to-execution lowering Module.
//!
//! `mondrian-timeline` owns persistent author intent. This crate resolves that
//! intent into one immutable semantic program, prepares a render-contract-
//! specific plan, and creates exclusive mutable render Sessions. Playback,
//! export, audition, and analysis use the same Interface and DSP mathematics.

mod built_in_processors;
mod channel_mix;
mod compiler;
mod continuity_evidence;
mod delay;
mod delivery;
mod dependency;
mod dsp;
mod latency;
mod lookahead_limiter;
mod loudness;
mod meter;
mod native_audio_author_observations;
mod native_audio_author_runtime;
mod native_audio_author_state_entry;
mod native_audio_editor;
mod native_audio_object;
mod native_audio_observations;
mod native_factory_storage;
mod native_host_allocation_ledger;
mod native_host_buffers;
mod native_runtime_owner;
mod native_worker_lane_resources;
mod plan;
mod processor;
mod processor_host;
mod processor_inspection;
mod processor_isolation;
mod processor_notes;
mod processor_parameters;
mod render;
mod resource_overlap;
mod runtime;
mod schedule;

pub use compiler::{compile_audio_program, AudioCompileError};
pub use continuity_evidence::{
    AudioContinuityEvidenceVisitor, AudioProcessorCursorEvidence, AudioProcessorCursorState,
    AudioSessionCursorEvidence, AudioSessionCursorIdentity, AudioSessionCursorState,
};
pub use delivery::{
    AudioDeliveryError, AudioDeliveryEvidence, AudioDeliveryMappingKind,
    AudioProgramDeliveryRuntime,
};
pub use dependency::{
    compile_audio_dependency_closure, compile_full_audio_dependency_closure,
    AudioDependencyClosure, AudioDependencyError,
};
pub use loudness::{AudioLoudnessAnalyzer, AudioLoudnessError, AudioLoudnessReport};
pub use meter::{
    AudioChannelMeterReading, AudioMeterFrame, AudioMeterObserver, AudioMeterTarget,
    AudioMeterTargetFrame,
};
pub use native_audio_author_observations::*;
pub use native_audio_author_runtime::{
    NativeAudioAuthorAdmissionError, NativeAudioAuthorExecutionHold, NativeAudioAuthorHoldReason,
};
pub use native_audio_author_state_entry::*;
pub use native_audio_editor::*;
pub use native_audio_object::*;
pub use native_audio_observations::*;
pub use native_host_buffers::{
    NativeHostBufferError, NativeHostBufferLayout, NativeHostFixedBytes,
};
pub use native_runtime_owner::*;
pub use plan::*;
pub use processor::*;
pub use processor_inspection::*;
pub use processor_isolation::*;
pub use processor_notes::*;
pub use render::{
    render_audio, AudioContinuityEpoch, AudioExecutionError, AudioPcmSource, AudioRenderCapacity,
    AudioRenderRequest, AudioRenderSession, AudioStateEntry,
};
pub use resource_overlap::{
    AudioParameterPlanResourceAdmission, AudioParameterPlanResourceCommit,
    AudioParameterPlanResourceReplacement,
};
pub use runtime::{
    AudioDecodedSource, AudioMediaResolver, AudioProgramRuntime, AudioRuntimeBuildError,
    AudioRuntimeResourceCategory, AudioRuntimeResourceFootprint, AudioRuntimeResourceGrant,
    ResolvedAudioSource,
};
pub use schedule::{
    AudioKernelBackend, AudioResourceFootprintError, AudioSessionResourceFootprint,
    PreparedAudioPlan, PreparedAudioScheduleSummary,
};

#[cfg(test)]
mod tests;
pub use channel_mix::PreparedAudioChannelMixer;

pub use native_host_allocation_ledger::{
    NativeHostAllocationBorrow, NativeHostAllocationLedger, NativeHostAllocationOwner,
    NativeHostLedgerError, NativeHostLedgerStorage, NativeHostLocalPeakSnapshot,
};

pub use native_worker_lane_resources::{
    NativeWorkerControlOverlapLayout, NativeWorkerLaneOwnedLayouts, NativeWorkerLaneResourceError,
    NativeWorkerLaneResourceProfile, NativeWorkerLaneStorageKind, SCOPED_NATIVE_DSP_STACK_BYTES,
    SCOPED_NATIVE_READER_STACK_BYTES,
};

mod native_diagnostic_projection;

// Typed same-object parameter Plan installation; actual permission remains
// the private Media Source authorization and retained native proposal.
pub use runtime::native_author::{
    NativeAudioAuthorProgram, NativeAudioParameterHandoffContinuity,
    NativeAudioParameterHandoffError, NativeAudioParameterHandoffReceipt,
    PreparedNativeAudioParameterHandoff,
};

pub use runtime::host_output_fader::{
    AudioHostFaderHandoffError, HostOutputFaderHandoffReceipt, PreparedHostOutputFaderHandoff,
};

pub use native_factory_storage::{
    AudioProcessorFactoryStorageBorrow, AudioProcessorFactoryStorageError,
    AudioProcessorFactoryStorageKind,
};

/// Bounded source order/publication owner shared by actual CLAP callback owners.
pub use processor_isolation::clap_parameter_source::{
    ClapParameterSource, ClapParameterSourceSnapshot,
};
