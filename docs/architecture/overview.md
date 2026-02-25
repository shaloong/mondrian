# Mondrian 系统架构总览

## 1. 设计哲学

```text
高性能  =  Rust 零成本抽象  +  GPU 渲染管线  +  多线程媒体流水线
可扩展  =  Crate 模块化  +  事件驱动  +  插件 trait 抽象
AI 原生 =  Provider 抽象层  +  可视化 AI 计划  +  资产复用系统
```

---

## 2. 分层架构

```text
┌──────────────────────────────────────────────────────────────────┐
│  Layer 4: Application Layer                                       │
│  ┌─────────────────────────────────────────────────────────────┐ │
│  │  mondrian-app                                               │ │
│  │  ├─ MainWindow  (Panels: Timeline / Viewer / Library / AI)  │ │
│  │  ├─ AppState    (单向数据流，类 Redux)                       │ │
│  │  └─ CommandBus  (所有用户操作 → 可撤销 Command)              │ │
│  └─────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────┘
         │ Command / Event
┌──────────────────────────────────────────────────────────────────┐
│  Layer 3: Domain Logic Layer                                      │
│  ┌────────────────┐  ┌─────────────────┐  ┌───────────────────┐ │
│  │ mondrian-      │  │ mondrian-       │  │ mondrian-         │ │
│  │ timeline       │  │ assets          │  │ ai                │ │
│  │                │  │                 │  │                   │ │
│  │ Timeline       │  │ AssetLibrary    │  │ AgentOrchestrator │ │
│  │ Track          │  │ CharacterRepo   │  │ WorkflowEngine    │ │
│  │ Clip           │  │ SceneRepo       │  │ Providers         │ │
│  │ Keyframe       │  │ TemplateRepo    │  │                   │ │
│  └────────────────┘  └─────────────────┘  └───────────────────┘ │
└──────────────────────────────────────────────────────────────────┘
         │ FrameRequest / MediaQuery
┌──────────────────────────────────────────────────────────────────┐
│  Layer 2: Engine Layer                                            │
│  ┌──────────────────────┐   ┌────────────────────────────────┐  │
│  │ mondrian-renderer    │   │ mondrian-media                 │  │
│  │                      │   │                                │  │
│  │ FrameCompositor      │◄──│ DecoderPool    ProxyCache      │  │
│  │ RenderPipeline(wgpu) │   │ AudioMixer     MediaInfo       │  │
│  │ LayerGraph           │   │                                │  │
│  │ ShaderRegistry       │   └────────────────────────────────┘  │
│  └──────────────────────┘                                       │
│  ┌──────────────────────┐   ┌────────────────────────────────┐  │
│  │ mondrian-effects     │   │ mondrian-export                │  │
│  │                      │   │                                │  │
│  │ LutProcessor         │   │ RenderQueue                    │  │
│  │ FilterGraph          │   │ HardwareEncoder                │  │
│  │ TransitionEngine     │   │ FormatPresets                  │  │
│  └──────────────────────┘   └────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
         │ 公共类型 / 错误 / 事件
┌──────────────────────────────────────────────────────────────────┐
│  Layer 1: Core Foundation                                         │
│  mondrian-core                                                    │
│  ├─ types    (TimeCode, FrameRate, Resolution, Rect, Color...)    │
│  ├─ error    (MondrianError, Result<T>)                           │
│  ├─ events   (EventBus: publish/subscribe)                        │
│  └─ project  (Project, Sequence, Settings)                        │
└──────────────────────────────────────────────────────────────────┘
```

---

## 3. 数据流设计

### 3.1 预览渲染数据流

```text
用户拖动播放头
      │
      ▼
AppState::seek(timecode)
      │
      ▼
Timeline::get_active_clips(timecode)
  → [ClipRef { track, clip_id, local_time }]
      │
      ▼
DecoderPool::get_frame(clip_id, local_time)
  → RawFrame { yuv_data, width, height, pts }    ← 可能来自缓存
      │
      ▼
FrameCompositor::composite(clips, effects, keyframes)
  → wgpu 渲染指令
      │
      ▼
预览窗口显示
```

### 3.2 AI 生成数据流

```text
用户输入 Prompt
      │
      ▼
选择可视化 AI 计划模板（字幕 / 粗剪 / 生成镜头）
      │
      ▼
AgentOrchestrator::run_plan(ai_plan)
      │
  ┌───┴──────────────────────────────┐
  │  Step 1: transcribe_audio        │
  │  → SpeechProvider::transcribe()  │
  │  → 保存到 AssetLibrary            │
  └───┬──────────────────────────────┘
  ┌───┴──────────────────────────────┐
  │  Step 2: translate_subtitle      │
  │  → SubtitleProvider::translate() │
  │  → 生成 SRT / 双行字幕轨          │
  └───┬──────────────────────────────┘
  ┌───┴──────────────────────────────┐
  │  Step 3: place_on_timeline       │
  │  → Timeline::add_clip()          │
  └──────────────────────────────────┘
```

---

## 4. 关键设计决策

### 4.1 事件驱动 vs 直接调用

所有跨模块通信通过 `EventBus`：

```rust
// 发布
event_bus.publish(AppEvent::PlayheadMoved { timecode });

// 订阅
event_bus.subscribe::<AppEvent, _>(|event| {
    // 处理事件
});
```

优点：

- 模块解耦
- 便于测试（mock 事件）
- 支持撤销/重做（事件记录）

### 4.2 时间表示

所有时间用 `TimeCode`（基于帧数的有理数）：

```text
TimeCode = frame_number / frame_rate
```

不用浮点秒数，避免累积精度误差。

### 4.3 ID 系统

所有实体用 `Uuid v4`：

```rust
pub struct ClipId(Uuid);
pub struct TrackId(Uuid);
pub struct AssetId(Uuid);
```

新建类型模式（newtype）防止 ID 混淆。

---

## 5. 并发模型

```text
主线程          UI 渲染 / 用户输入
渲染线程        wgpu 帧提交（通过 crossbeam channel）
解码线程池      FFmpeg 解码（rayon thread pool, N = CPU 核心数 - 2）
AI 异步运行时   Tokio（HTTP 请求、文件 IO）
导出线程        后台渲染队列（独立 thread）
```

---

## 6. 项目文件格式

```text
project.mondrian (ZIP 容器)
├── project.json      项目元数据 / 序列配置
├── timeline.json     时间线数据（轨道 / clip / 关键帧）
├── assets.json       素材库引用（路径 + 元数据）
├── ai_history.json   AI 生成历史（Prompt + 结果）
└── thumbnails/       素材缩略图缓存
```
