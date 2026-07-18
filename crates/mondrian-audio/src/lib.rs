//! The single audio author-to-execution lowering Module.
//!
//! `mondrian-timeline` owns persistent author intent. This crate resolves that
//! intent into one immutable semantic program, prepares a render-contract-
//! specific plan, and creates exclusive mutable render Sessions. Playback,
//! export, audition, and analysis use the same Interface and DSP mathematics.

mod compiler;
mod dsp;
mod plan;
mod render;
mod runtime;
mod schedule;

pub use compiler::{compile_audio_program, AudioCompileError};
pub use plan::*;
pub use render::{
    render_audio, AudioExecutionError, AudioPcmSource, AudioRenderRequest, AudioRenderSession,
};
pub use runtime::{
    AudioDecodedSource, AudioMediaResolver, AudioProgramRuntime, AudioRuntimeBuildError,
};
pub use schedule::{AudioKernelBackend, PreparedAudioPlan, PreparedAudioScheduleSummary};

#[cfg(test)]
mod tests;
