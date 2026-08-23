//! # mondrian-ai
//!
//! AI capability contracts and fail-closed workflow schema.
//!
//! This experimental crate currently provides typed Provider traits and a
//! YAML workflow model. It does not contain a production Provider or editor
//! mutation Adapter. [`AgentOrchestrator`] therefore rejects every unbound or
//! unknown step and never reports an unexecuted workflow as complete.

pub mod orchestrator;
pub mod provider;
pub mod workflow;

pub use orchestrator::AgentOrchestrator;
pub use provider::{
    AiProvider, ImageGenerationProvider, LlmProvider, MusicGenerationProvider,
    SpeechRecognitionProvider, VideoGenerationProvider,
};
pub use workflow::{WorkflowDef, WorkflowInputs, WorkflowResult};
