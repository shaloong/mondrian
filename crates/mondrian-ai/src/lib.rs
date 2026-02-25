//! # mondrian-ai
//!
//! AI 工作流引擎。
//!
//! 提供：
//! - AI Provider 抽象层（图像/视频/语音/音乐/LLM）
//! - 内置 Provider 实现（OpenAI / Runway / Kling / Suno / Whisper）
//! - Agent 工作流引擎（YAML DSL 驱动）

pub mod orchestrator;
pub mod provider;
pub mod workflow;

pub use orchestrator::AgentOrchestrator;
pub use provider::{
    AiProvider, ImageGenerationProvider, LlmProvider, MusicGenerationProvider,
    SpeechRecognitionProvider, VideoGenerationProvider,
};
pub use workflow::{WorkflowDef, WorkflowInputs, WorkflowResult};
