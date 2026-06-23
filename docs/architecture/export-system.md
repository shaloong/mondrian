# 导出系统设计

## 1. 架构

```text
用户点击「导出」
    │
    ▼
TimelineExportRequest（UI 收集预设、序列、范围、输出路径）
    │
    ▼
ExportConfig（预设 + 自定义参数）
    │
    ▼
RenderQueue（异步后台任务）
    │
    ├─ FrameRenderer（时间线逐帧渲染）
    │      ↓
    │  BatchedCompositor (wgpu 29.0, GPU compositing)
    │      ↓
    │  GPU → CPU readback → YUV帧 (或 GPU→GPU 零拷贝路径)
    │
    └─ HardwareEncoder（FFmpeg 硬件编码）
           ↓
       输出文件 (.mp4 / .mov / .gif)
```

导出 compositor 与 eframe preview 共享同一 wgpu device/queue（unified GPU），避免跨设备拷贝开销。GPU compositing 路径在导出中同样可用，包括 pass fusion 和 compute shader 效果加速。

`mondrian-app::app::exporting` 是 UI 无关的导出编排边界。egui 与自研 UI
都应提交 `TimelineExportRequest` 或对应的 `ui.export.enqueue` action，由
`AppState` 负责解析目标序列、递归收集嵌套序列素材、过滤 synthetic adjustment
asset、检查离线素材、构造 `TimelineExportInput`，最后将 `RenderJob` 放入
`RenderQueue`。面板代码不应复制这些业务规则。
同一模块也提供共享内置预设列表和 `TimelineExportDraft`，避免 egui 与自研
UI 在预设命名、默认选择、草稿状态持久化上分叉。

---

## 2. 导出格式预设

```rust
pub struct ExportPreset {
    pub name: String,
    pub container: Container,
    pub video_codec: VideoCodecConfig,
    pub audio_codec: AudioCodecConfig,
    pub resolution: Option<Resolution>,   // None = 与序列一致
    pub frame_rate: Option<Rational>,     // None = 与序列一致
}

// 内置预设
pub fn presets() -> Vec<ExportPreset> {
    vec![
        // YouTube
        ExportPreset {
            name: "YouTube 1080p".into(),
            container: Container::Mp4,
            video_codec: VideoCodecConfig::H264 {
                bitrate: Bitrate::Cbr(8_000_000),  // 8Mbps
                profile: H264Profile::High,
                level: "4.1",
            },
            audio_codec: AudioCodecConfig::Aac {
                bitrate: 192_000,
                sample_rate: 48000,
            },
            resolution: Some(Resolution { w: 1920, h: 1080 }),
            ..
        },
        // TikTok / 竖屏
        ExportPreset {
            name: "TikTok 1080x1920".into(),
            resolution: Some(Resolution { w: 1080, h: 1920 }),
            ..
        },
        // 院线 DCP
        ExportPreset {
            name: "DCP 4K".into(),
            container: Container::Mxf,
            video_codec: VideoCodecConfig::Jpeg2000 { .. },
            resolution: Some(Resolution { w: 4096, h: 2160 }),
            ..
        },
        // 代理
        ExportPreset {
            name: "Proxy 720p H.264".into(),
            video_codec: VideoCodecConfig::H264 {
                bitrate: Bitrate::Crf(23),
                ..
            },
            resolution: Some(Resolution { w: 1280, h: 720 }),
            ..
        },
        // GIF
        ExportPreset {
            name: "GIF (社交媒体)".into(),
            container: Container::Gif,
            video_codec: VideoCodecConfig::Gif { colors: 256, dither: true },
            ..
        },
    ]
}
```

---

## 3. 渲染队列

```rust
pub struct RenderQueue {
    jobs: Arc<Mutex<VecDeque<RenderJob>>>,
    active_job: Arc<Mutex<Option<RenderJob>>>,
    worker_thread: JoinHandle<()>,
    event_bus: Arc<EventBus>,
}

pub struct RenderJob {
    pub id: JobId,
    pub project_id: ProjectId,
    pub sequence_id: SequenceId,
    pub export_config: ExportConfig,
    pub output_path: PathBuf,
    pub status: JobStatus,
    pub progress: f32,      // 0.0 ~ 1.0
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobStatus {
    Pending,
    Rendering { frame: u64, total_frames: u64 },
    Encoding,
    Completed,
    Failed(String),
    Cancelled,
}

impl RenderQueue {
    pub fn enqueue(&self, job: RenderJob) -> JobId;
    pub fn cancel(&self, job_id: JobId) -> Result<()>;
    pub fn list_jobs(&self) -> Vec<RenderJob>;
    pub fn clear_completed(&self);
}
```

`clear_completed` clears every terminal queue entry, not only successful exports:
`Completed`, `Failed(_)`, and `Cancelled` jobs are removable, while `Pending`,
`Rendering`, and `Encoding` jobs remain visible and cancelable. Self-hosted UI
queue snapshots must use the same terminal definition for clear-button
availability and row state.

---

## 4. 硬件加速编码

```rust
pub enum EncoderBackend {
    // NVIDIA GPU
    NvencH264,
    NvencH265,
    NvencAv1,
    // AMD GPU
    AmfH264,
    AmfH265,
    // Intel QSV
    QsvH264,
    QsvH265,
    // Apple
    VideoToolboxH264,
    VideoToolboxH265,
    VideoToolboxProRes,
    // 纯 CPU 软件编码（兜底）
    SoftwareX264,
    SoftwareX265,
    SoftwareLibVpx,
}

impl EncoderBackend {
    /// 自动检测最优硬件编码器
    pub fn detect_best() -> Self {
        #[cfg(target_os = "windows")]
        {
            if nvidia_available() { return Self::NvencH264; }
            if amd_available()    { return Self::AmfH264; }
            if intel_qsv_available() { return Self::QsvH264; }
        }
        #[cfg(target_os = "macos")]
        {
            return Self::VideoToolboxH264;
        }
        Self::SoftwareX264
    }
}
```

---

## 5. 序列色彩与导出合法性

导出队列在进入编码前，会先基于 `SequenceSettings` 解析统一的渲染色彩上下文，并对输出组合做前置校验。

当前约束包含：

- HDR 输出不能落到 8-bit
- 保留 HDR metadata 需要 HDR 输出色彩空间
- GIF 仅允许 8-bit SDR
- ProRes 与 WebM / MP4 等容器组合需要符合预期的专业容器策略

嵌套序列会继承相同的 color context 解析规则，避免 preview/export 两条链路在 nested 场景下出现色彩解释分叉。

---

## 5. 导出性能目标

| 场景                     | 目标速度               |
| ------------------------ | ---------------------- |
| 1080p H.264 (NVENC)      | 实时 × 8 (8x realtime) |
| 4K H.265 (NVENC)         | 实时 × 2               |
| 4K ProRes (VideoToolbox) | 实时 × 4               |
| 1080p H.264 (软件 x264)  | 实时 × 3               |
| GIF 720p                 | 实时 × 1               |
