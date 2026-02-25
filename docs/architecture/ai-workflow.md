# AI 工作流引擎设计

## 1. 核心设计理念

```text
用户意图（自然语言）
    │
    ▼
可视化 AI 计划（节点式点击配置）
    │
    ▼
AgentOrchestrator
    │
    ├─ Step 解析 → 调度到对应 Provider
    ├─ 上下文传递（前一步输出 → 下一步输入）
    ├─ 错误重试 / 降级策略
    └─ 结果写入 AssetLibrary + 放置到 Timeline
```

---

## 2. Provider 抽象层

````rust
/// 所有 AI 能力的统一抽象
#[async_trait]
pub trait AiProvider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> &[Capability];
}

/// 文生图
#[async_trait]
pub trait ImageGenerationProvider: AiProvider {
    async fn generate_image(
        &self,
        request: ImageGenerationRequest,
    ) -> Result<GeneratedImage>;
}

/// 文生视频
#[async_trait]
pub trait VideoGenerationProvider: AiProvider {
    async fn generate_video(
        &self,
        request: VideoGenerationRequest,
    ) -> Result<GeneratedVideo>;

    /// 流式进度回调
    async fn generate_video_with_progress(
        &self,
        request: VideoGenerationRequest,
        progress_cb: impl Fn(GenerationProgress) + Send,
    ) -> Result<GeneratedVideo>;
}

/// 语音识别（自动字幕）
#[async_trait]
pub trait SpeechRecognitionProvider: AiProvider {
    async fn transcribe(
        &self,
        audio: AudioBuffer,
        language: Option<&str>,
    ) -> Result<Transcript>;
}

  /// 字幕翻译（多语言 / 双行字幕）
  #[async_trait]
  pub trait SubtitleTranslationProvider: AiProvider {
    async fn translate_subtitle(
      &self,
      request: SubtitleTranslationRequest,
    ) -> Result<TranslatedSubtitle>;
  }

  /// AI 配音 / 语音克隆
  #[async_trait]
  pub trait VoiceSynthesisProvider: AiProvider {
    async fn synthesize_voice(
      &self,
      request: VoiceSynthesisRequest,
    ) -> Result<GeneratedVoiceTrack>;
  }

/// 自动配乐
#[async_trait]
pub trait MusicGenerationProvider: AiProvider {
    async fn generate_music(
        &self,
        request: MusicGenerationRequest,
    ) -> Result<GeneratedMusic>;
}

/// LLM（剪辑建议 / Prompt 优化）
#[async_trait]
pub trait LlmProvider: AiProvider {
    async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        options: LlmOptions,
    ) -> Result<ChatResponse>;
}
```text

---

## 3. 内置 Provider 实现

````

providers/
├── video/
│ ├── runway_gen3.rs Runway Gen-3 Alpha
│ ├── kling.rs 可灵 AI v1.6
│ ├── sora.rs OpenAI Sora (reserved)
│ └── minimax_video.rs MiniMax Video
├── speech/
│ ├── faster_whisper.rs faster-whisper（本地）
│ ├── whisperx.rs WhisperX（词级对齐）
│ └── openai_whisper.rs Whisper API（云端）
├── subtitle/
│ ├── llm_translate.rs LLM 翻译字幕（多语言）
│ └── bilingual_layout.rs 双行字幕布局
├── voice/
│ ├── elevenlabs.rs ElevenLabs API
│ └── indextts.rs IndexTTS（本地）
└── llm/
├── openai_gpt4o.rs GPT-4o
├── anthropic_claude.rs Claude 3.5 Sonnet
└── gemini.rs Google Gemini Pro

````text

---

## 4. 可视化 AI 计划（面向普通用户）

默认交互是“点击式能力卡片 + 参数面板 + 一键执行”，不要求普通用户编写 YAML/JSON/脚本。

### 4.1 用户可见流程

```text
选择目标（字幕 / 粗剪 / 生成镜头）
    │
    ▼
选择模型来源（Cloud API / Local）
    │
    ▼
勾选能力卡片（如：转写 → 翻译 → 配音）
    │
    ▼
AgentOrchestrator 生成执行计划 + 实时进度
    │
    ▼
结果自动写入 AssetLibrary 并放置到 Timeline
````

### 4.2 内置计划模板（MVP）

- 字幕助手：`转写（faster-whisper/WhisperX）→ 翻译（可多语言）→ 导出 SRT 或双行字幕`
- 配音助手：`翻译稿 → TTS/语音克隆（ElevenLabs 或 IndexTTS）→ 对齐时间线`
- 智能粗剪：`去静音/去无用镜头 → 语义分段 → 自动粗剪到新序列`
- 生成镜头助手：`文本生成 B-roll / 基于首尾帧补镜 / 视频延长`

### 4.3 插件扩展策略（保留接口）

- 普通用户界面不暴露 DSL 编辑器。
- 为插件开发者保留 `PlanNode` / `Provider` 注册接口。
- 插件可新增能力节点、参数面板和后处理器，但仍通过统一执行引擎与权限模型。

---

## 5. AgentOrchestrator

````rust
pub struct AgentOrchestrator {
    providers: ProviderRegistry,
    asset_library: Arc<AssetLibrary>,
    timeline: Arc<RwLock<Sequence>>,
    event_bus: Arc<EventBus>,
}

impl AgentOrchestrator {
    /// 执行工作流（异步，实时进度事件）
    pub async fn run_workflow(
        &self,
        workflow: WorkflowDef,
        inputs: WorkflowInputs,
    ) -> Result<WorkflowResult> {
        let mut ctx = WorkflowContext::new(inputs);

        for step in &workflow.steps {
            // 条件检查
            if !self.evaluate_condition(&ctx, &step.condition)? {
                continue;
            }

            // 发送进度事件到 UI
            self.event_bus.publish(AppEvent::WorkflowStepStarted {
                step_id: step.id.clone(),
                step_name: step.name.clone(),
            });

            // 执行 step
            let output = self.execute_step(&ctx, step).await
                .map_err(|e| self.handle_step_error(step, e))?;

            ctx.set_output(&step.id, output);

            self.event_bus.publish(AppEvent::WorkflowStepCompleted {
                step_id: step.id.clone(),
            });
        }

        Ok(WorkflowResult { context: ctx })
    }
}
```text

---

## 6. 自动字幕系统

```rust
/// Whisper 输出 → 时间线字幕轨
pub struct Transcript {
    pub segments: Vec<TranscriptSegment>,
    pub language: String,
    pub confidence: f32,
}

pub struct TranscriptSegment {
    pub start: Duration,
    pub end: Duration,
    pub text: String,
    pub words: Vec<WordTimestamp>,  // 词级时间戳（用于卡拍字幕）
}

impl Transcript {
    /// 转换为 ASS 字幕格式
    pub fn to_ass(&self, style: &SubtitleStyle) -> String;

    /// 转换为 SRT 格式
    pub fn to_srt(&self) -> String;

    /// 放置到时间线字幕轨
    pub fn place_on_timeline(
        &self,
        timeline: &mut Sequence,
        track_index: usize,
        offset: TimeCode,
    ) -> Result<Vec<ClipId>>;
}
````

---

## 7. AI 素材管理

所有 AI 生成的内容自动进入 `AssetLibrary`，支持：

```text
AssetLibrary/
├── ai_generated/
│   ├── images/          DALL·E / Flux 生成图
│   ├── videos/          Runway / Kling 生成视频
│   ├── music/           Suno 生成音乐
│   └── prompts/         历史 Prompt（复用）
└── characters/
    └── {char_name}/
        ├── reference_images/   角色参考图
        ├── lora_config.json    LoRA 配置
        └── prompt_template.txt 该角色的提示词模板
```

---

## 8. API 配置管理

````toml
# config/ai_providers.toml
[runtime]
# cloud | local | hybrid
mode = "hybrid"

[providers.openai]
api_key      = "${OPENAI_API_KEY}"
base_url     = "https://api.openai.com/v1"
default_model = "gpt-4o"

[providers.runway]
api_key      = "${RUNWAY_API_KEY}"
base_url     = "https://api.dev.runwayml.com/v1"

[providers.kling]
api_key      = "${KLING_API_KEY}"
base_url     = "https://api.klingai.com"

[providers.suno]
api_key      = "${SUNO_API_KEY}"

[providers.elevenlabs]
api_key      = "${ELEVENLABS_API_KEY}"

[providers.local]
# 由本地推理守护进程托管模型（如 faster-whisper / whisperx / indextts）
endpoint = "http://127.0.0.1:32100"

[model_selection]
subtitle_transcribe = ["faster-whisper", "whisperx", "openai-whisper"]
subtitle_translate  = ["gpt-4o", "claude-3.5-sonnet", "local-llm"]
voice_synthesis     = ["elevenlabs", "indextts"]
video_generation    = ["runway-gen3", "kling-v1.6", "minimax-video"]
```text

---

## 9. 功能优先级（AI 计划）

### P0 / P1（优先实现）

- 字幕转写：faster-whisper + WhisperX（词级时间戳）
- 多语言翻译字幕：支持导出 SRT 或时间线双行显示
- AI 配音 / 语音克隆：ElevenLabs API 或 IndexTTS 本地模型
- 智能剪辑：去无用镜头与静音、自动粗剪、语义排布
- 视频生成：文本生成 B-roll、基于首/尾帧的延长与补镜

### P3（最低优先级，短期不实现）

- 片段自动搜索：任务识别、人物/地点/动作标签、自然语言搜索、自动镜头分类
````
