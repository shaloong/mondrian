//! The single audio author-to-execution lowering Module.
//!
//! `mondrian-timeline` owns persistent author intent. This crate resolves that
//! intent into one immutable semantic program, prepares a render-contract-
//! specific plan, and creates exclusive mutable render Sessions. Playback,
//! export, audition, and analysis use the same Interface and DSP mathematics.

mod built_in_processors;
mod channel_mix;
mod compiler;
mod delay;
mod dependency;
mod dsp;
mod latency;
mod lookahead_limiter;
mod meter;
mod plan;
mod processor;
mod processor_host;
mod processor_parameters;
mod render;
mod runtime;
mod schedule;

pub use compiler::{compile_audio_program, AudioCompileError};
pub use dependency::{
    compile_audio_dependency_closure, AudioDependencyClosure, AudioDependencyError,
};
pub use meter::{
    AudioChannelMeterReading, AudioMeterFrame, AudioMeterObserver, AudioMeterTarget,
    AudioMeterTargetFrame,
};
pub use plan::*;
pub use processor::*;
pub use render::{
    render_audio, AudioContinuityEpoch, AudioExecutionError, AudioPcmSource, AudioRenderCapacity,
    AudioRenderRequest, AudioRenderSession, AudioStateEntry,
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
