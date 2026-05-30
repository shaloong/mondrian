# Mondrian 系统架构总览

> **ARCHITECTURE V2 MIGRATION IN PROGRESS** (2026-05-30)
> This document describes the current (V1) architecture. The target V2 architecture
> is defined in:
>
> - [Architecture V2 Blueprint](architecture-v2-blueprint.md) — target state
> - [Architecture V2 Gap Analysis](architecture-v2-gap-analysis.md) — what's missing
> - [Architecture V2 Migration Plan](architecture-v2-migration-plan.md) — phased execution
>
> Branch: `feat/architecture-v2-dag-gpu`

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

### 4.4 应用级主题系统（Dark / Light / System）

`mondrian-app` 的主题是应用级配置，不进入项目文件。主题策略与 VS Code 一致，支持：

- `System`：跟随操作系统深浅色。
- `Dark`：强制深色。
- `Light`：强制浅色。

实现约定：

- 主题模式持久化在应用配置（`AppPreferences.theme`）。
- 每帧调用 `ui::theme::apply_theme`，将主题偏好映射到 egui 的 `ThemePreference`。
- 通过统一 token 表（色板 + 度量）驱动 `Visuals`、字体、间距、圆角等样式，禁止业务面板散落硬编码主题值。
- 支持插件覆写入口：插件可注册 token override，在不改业务面板代码的前提下覆写颜色与样式度量。

当前已经收口的 token 规范包括：

- `palette`：窗口背景、面板底色、强调色、边框、文本、预览画布基础色。
- `typography`：正文、正文小号、等宽正文、面板标题、按钮文字、列表行文字。
- `metrics`：列表最小高度、空状态高度、图标列宽、编辑最小宽度、代理标签尺寸、AI 工作流编辑区高度、日志区高度、时间线轨道高度、标尺高度、标签栏宽度、缩放范围、拖拽吸附阈值、右侧补白帧数、轨道图标/锁定/模式按钮偏移、剪辑内边距、幽灵剪辑最小宽度、播放头和选区描边宽度。

这使得后续新增品牌主题、A/B 视觉实验、插件化主题包时，仅需扩展 token 与 override 注册，不需要重写各个 UI 面板。

### 4.5 启动引导窗口（透明圆角）

`mondrian-app` 启动阶段使用独立 viewport 模式显示项目引导界面，glow渲染以确保圆角卡片外侧为真实透明而非黑底：

- NativeOptions 初始即启用透明窗口（`with_transparent(true)`），并以无系统装饰、固定尺寸启动。
- NativeOptions 初始尺寸必须与 `theme::tokens::startup_viewport_size` 对齐，避免用户看到“先短后高”的首屏尺寸跳变。
- App 在创建阶段立即同步 `startup_viewport_mode`，避免首帧清屏误用不透明底色。
- `eframe::App::clear_color` 在启动模式返回全透明 RGBA，在进入主工作区后恢复常规窗口填充色。
- 启动 UI 的 `CentralPanel` 使用透明 `Frame`，外层不再绘制兜底背景，圆角仅由启动卡片自身负责。
- 避免在启动页最外层绘制“整窗不透明底板”。若在透明窗口上绘制接近全屏的不透明矩形，即使窗口透明链路正确，也会产生“黑底仍在”的视觉结果。
- 当前启动页仅绘制左右两块内容卡片（左品牌卡、右操作卡），窗口其余区域保持透明。

打开或新建项目后，viewport 会切换回主工作区模式（可调整尺寸、带系统装饰）。

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

项目保存阶段对 `library/index.db` 采用流式写入 ZIP（`std::io::copy`），避免一次性读取整库到内存，降低大项目保存时的内存峰值与阻塞时长。
