# 媒体处理管线设计

## 1. 架构概览

```
输入媒体文件
    │
    ▼
┌─────────────────────────────────────────┐
│  MediaInspector（媒体探针）               │
│  FFmpeg::probe → MediaInfo               │
│  { codec, resolution, fps, duration,    │
│    audio_channels, color_space, ... }    │
└──────────────────┬──────────────────────┘
                   │
         ┌─────────┴──────────┐
         │                    │
         ▼                    ▼
  ┌────────────┐      ┌──────────────┐
  │  ProxyGen  │      │  DecoderPool  │
  │  (后台)    │      │  (实时解码)   │
  │ 720p proxy │      │              │
  │ LUT cache  │      │ 复用 FFmpeg  │
  └─────┬──────┘      │ context      │
        │              └──────┬───────┘
        │                     │
        ▼                     ▼
  ┌──────────────────────────────────┐
  │           FrameCache             │
  │  LRU(key=ClipId+PTS, cap=1GB)   │
  │  RawFrame { yuv420p, pts }       │
  └──────────────────┬───────────────┘
                     │
                     ▼
             FrameCompositor (renderer 模块)
```

---

## 2. MediaInfo 数据结构

```rust
pub struct MediaInfo {
    pub path: PathBuf,
    pub duration: Duration,
    pub video_streams: Vec<VideoStreamInfo>,
    pub audio_streams: Vec<AudioStreamInfo>,
    pub file_size: u64,
    pub container_format: String,
}

pub struct VideoStreamInfo {
    pub index: u32,
    pub codec: VideoCodec,          // H264 / H265 / ProRes / AV1
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,       // 24000/1001 = 23.976...
    pub pixel_format: PixelFormat,  // YUV420P / YUV422P10LE
    pub color_space: ColorSpace,    // Rec709 / Rec2020 / sRGB
    pub bit_depth: u8,              // 8 / 10 / 12
    pub has_alpha: bool,
}

pub struct AudioStreamInfo {
    pub index: u32,
    pub codec: AudioCodec,          // AAC / PCM / FLAC
    pub sample_rate: u32,           // 44100 / 48000 / 96000
    pub channels: u8,
    pub channel_layout: ChannelLayout,
    pub bit_depth: u16,
}
```

---

## 3. 解码器池（DecoderPool）

### 设计原则

- 每个媒体文件分配一个 `DecoderContext`（FFmpeg AVFormatContext）
- 线程安全：`Arc<Mutex<DecoderContext>>`（通过 channel 异步请求）
- 最大并发解码器数 = `min(CPU核心数 - 2, 8)`
- LRU 淘汰策略：最久未使用的 context 关闭

### 关键 API

```rust
pub struct DecoderPool {
    contexts: DashMap<AssetId, Arc<DecoderContext>>,
    max_contexts: usize,
    frame_cache: Arc<FrameCache>,
}

impl DecoderPool {
    /// 获取指定时间码的视频帧（优先从缓存）
    pub async fn get_video_frame(
        &self,
        asset_id: AssetId,
        pts: TimeCode,
    ) -> Result<Arc<RawVideoFrame>>;

    /// 获取音频采样块
    pub async fn get_audio_samples(
        &self,
        asset_id: AssetId,
        time_range: TimeRange,
    ) -> Result<AudioBuffer>;

    /// 预加载（seek 前预读 N 帧）
    pub async fn prefetch(
        &self,
        asset_id: AssetId,
        pts: TimeCode,
        lookahead_frames: u32,
    ) -> Result<()>;
}
```

---

## 4. GPU 硬件解码

### 硬解码路径配置

```rust
pub enum HwAccelBackend {
    None,                   // 纯 CPU 解码
    Cuda,                   // NVIDIA NVDEC
    D3D11VA,                // Windows DirectX 11
    VideoToolbox,           // macOS/iOS
    Vaapi,                  // Linux VA-API
    Dxva2,                  // Windows DirectX 9 (legacy)
}

impl DecoderContext {
    pub fn with_hw_accel(mut self, backend: HwAccelBackend) -> Self {
        // FFmpeg hw_device_ctx 配置
        self.hw_accel = backend;
        self
    }
}
```

### 硬解帧传输

硬解后帧数据在 GPU 显存中：

```
GPU 显存 (NV12 surface)
    │
    ▼  FFmpeg hwdownload filter（按需）
CPU 内存 (YUV420P)
    │
    ▼  wgpu texture upload
GPU 显存 (wgpu Texture, RGBA)
```text

**优化目标：** 4K H.265 在 GPU 完成解码 + 渲染，避免 CPU ↔ GPU 搬运。

---

## 5. 代理文件系统（Proxy）

### 工作原理

```
原始文件: 4K ProRes 422 HQ (大文件，高 CPU 解码压力)
    │
    │ 后台生成（cargo run --bin proxy-gen）
    ▼
代理文件: 720p H.264 (小文件，低延迟解码)
    │
    ▼ 编辑时使用代理文件预览

导出时: 自动切回原始文件
```text

### 代理生成配置

```rust
pub struct ProxyConfig {
    pub resolution: ProxyResolution,  // P720 / P1080 / P360
    pub codec: ProxyCodec,            // H264 / DNxHD
    pub quality: u8,                  // CRF 0-51
    pub concurrent_jobs: u8,          // 并行转码数
}
```

### 代理文件命名规则

```text
原始: /media/project/scene01.mp4
代理: ~/.mondrian/proxy/{SHA256(原始路径)}_720p.mp4
```

---

## 6. 音频处理

### 音频时间线混合

```rust
pub struct AudioMixer {
    sample_rate: u32,          // 输出采样率（默认 48000）
    channels: u8,              // 输出声道数（默认立体声）
    buffer_size: usize,        // 每次处理采样数（512 / 1024）
}

impl AudioMixer {
    /// 混合多轨音频，输出 interleaved PCM f32
    pub fn mix_tracks(
        &self,
        tracks: &[AudioTrackData],
        timecode: TimeCode,
        duration: Duration,
    ) -> AudioBuffer;
}
```text

### 音频同步策略

- 视频帧率驱动（Video Master Clock）
- 音频每帧跟随 PTS 对齐
- 允许最大 ±1 帧误差（约 40ms @ 25fps）

---

## 7. 性能指标目标

| 场景             | 目标                         |
| ---------------- | ---------------------------- |
| 1080p H.264 播放 | CPU 解码 < 30%（i7-12代）    |
| 4K H.265 播放    | GPU 硬解，CPU < 20%          |
| 时间线响应延迟   | Seek 后首帧 < 100ms          |
| 代理文件生成     | 1小时 4K → 720p 代理 < 5分钟 |
| 帧缓存命中率     | 顺序播放 > 95%               |

### 开发态性能验证（开发专用）

为避免影响常规 `cargo test`，性能测试默认标记为 `#[ignore]`，仅在需要性能回归排查时手动执行：

```powershell
# 项目创建/打开/保存性能烟雾测试
$env:MONDRIAN_PERF_OUTPUT='target/perf/project-lifecycle.jsonl'
cargo test -p mondrian-app perf_project_lifecycle_smoke -- --ignored --nocapture

# 1080p24 预览模拟测试（TTFF/FPS）
$env:MONDRIAN_PREVIEW_SIM_OUTPUT='target/perf/preview-1080p24.jsonl'
cargo test -p mondrian-app preview_1080p24_simulated_perf -- --ignored --nocapture

# 4K60 预览模拟测试（高负载性能优化）
$env:MONDRIAN_PREVIEW_SIM_OUTPUT='target/perf/preview-4k60.jsonl'
cargo test -p mondrian-app preview_4k60_simulated_perf -- --ignored --nocapture
```

两项测试都会输出 JSON，便于脚本或 AI 自动分析异常样本。

### 预览合成优化说明（2026-03）

`mondrian-app` 预览 CPU 合成路径已做两项关键优化：

- 多图层合成阶段去除 `RGBA` 数据的重复 `clone`，减少每帧内存拷贝。
- `alpha_blend` 改为整数定点计算，并对“整帧不透明图层”走快速拷贝路径。
- 合成缓冲区改为按需初始化 alpha，避免每帧无条件清屏带来的 4K 内存带宽开销。

该优化主要降低 1080p 预览时的 CPU 开销，并提升播放稳定帧率。
