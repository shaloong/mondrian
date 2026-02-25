//! AI Provider 抽象 Trait 定义

use async_trait::async_trait;
use mondrian_core::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

// ─── 通用能力标记 ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    ImageGeneration,
    VideoGeneration,
    SpeechRecognition,
    MusicGeneration,
    Llm,
    Upscale,
    FrameInterpolation,
}

/// 所有 AI Provider 的基础 Trait
pub trait AiProvider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> &[Capability];
}

// ─── 图像生成 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationRequest {
    pub prompt: String,
    pub negative_prompt: Option<String>,
    pub width: u32,
    pub height: u32,
    pub quality: ImageQuality,
    pub reference_image: Option<PathBuf>,
    pub num_images: u8,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ImageQuality {
    Standard,
    Hd,
    Ultra,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImage {
    pub local_path: PathBuf,
    pub url: Option<String>,
    pub width: u32,
    pub height: u32,
    pub prompt: String,
}

#[async_trait]
pub trait ImageGenerationProvider: AiProvider {
    async fn generate_image(&self, req: ImageGenerationRequest) -> Result<Vec<GeneratedImage>>;
}

// ─── 视频生成 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoGenerationRequest {
    pub prompt: String,
    pub negative_prompt: Option<String>,
    pub duration_secs: f32, // 3 ~ 10
    pub aspect_ratio: AspectRatio,
    pub reference_image: Option<PathBuf>,
    pub end_image: Option<PathBuf>,
    pub motion_strength: Option<f32>, // 0.0 ~ 1.0
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum AspectRatio {
    Landscape, // 16:9
    Portrait,  // 9:16
    Square,    // 1:1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedVideo {
    pub local_path: PathBuf,
    pub url: Option<String>,
    pub duration: Duration,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
}

#[derive(Debug, Clone)]
pub struct GenerationProgress {
    pub progress: f32,
    pub message: String,
    pub eta_secs: Option<f32>,
}

#[async_trait]
pub trait VideoGenerationProvider: AiProvider {
    async fn generate_video(&self, req: VideoGenerationRequest) -> Result<GeneratedVideo>;

    /// 带进度回调的视频生成
    async fn generate_video_with_progress(
        &self,
        req: VideoGenerationRequest,
        _progress_cb: tokio::sync::mpsc::Sender<GenerationProgress>,
    ) -> Result<GeneratedVideo> {
        // 默认实现：调用基础方法（无进度）
        self.generate_video(req).await
    }
}

// ─── 语音识别 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub segments: Vec<TranscriptSegment>,
    pub language: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start: Duration,
    pub end: Duration,
    pub text: String,
}

#[async_trait]
pub trait SpeechRecognitionProvider: AiProvider {
    async fn transcribe(
        &self,
        audio_path: &std::path::Path,
        language: Option<&str>,
    ) -> Result<Transcript>;
}

// ─── 音乐生成 ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicGenerationRequest {
    pub prompt: String,
    pub duration_secs: f32,
    pub style: Option<String>,
    pub mood: Option<String>,
    pub bpm: Option<u32>,
    pub instrumental: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedMusic {
    pub local_path: PathBuf,
    pub duration: Duration,
    pub title: String,
    pub bpm: Option<u32>,
}

#[async_trait]
pub trait MusicGenerationProvider: AiProvider {
    async fn generate_music(&self, req: MusicGenerationRequest) -> Result<GeneratedMusic>;
}

// ─── LLM ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmOptions {
    pub model: Option<String>,
    pub temperature: f32,
    pub max_tokens: u32,
}

impl Default for LlmOptions {
    fn default() -> Self {
        Self { model: None, temperature: 0.7, max_tokens: 2048 }
    }
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    pub finish_reason: String,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[async_trait]
pub trait LlmProvider: AiProvider {
    async fn chat(&self, messages: Vec<ChatMessage>, opts: LlmOptions) -> Result<ChatResponse>;
}
