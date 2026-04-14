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

实现对齐说明：
- 预览解码线程数当前按 `(CPU核数 - 1).clamp(1, 8)` 计算，并允许通过 `MONDRIAN_PREVIEW_DECODE_THREADS` 覆盖。
- 预览解码后端 `PreviewDecodeBackend` 默认值为 `Auto`，通过全局原子状态切换。

### 预览合成签名与变换应用（实现对齐）

- 预览帧签名 `CompositeFrameSignature` 除 `asset_id/source_frame/opacity` 外，额外包含量化后的仿射变换键（`transform_key`），用于在位置、缩放、旋转等属性变化时正确触发重解码/重合成。
- 图层解码请求在合成前携带 `transform: [f32; 6]`（2D 仿射矩阵），合成阶段统一处理变换。
- 当所有图层均为单位变换时，优先尝试 GPU 合成路径；当存在非单位变换时，回退 CPU 仿射采样合成，保证视觉结果与时间线属性一致。
- 单图层直通优化仅在“单位变换 + 全尺寸 + 不透明度接近 1”条件下启用，避免错误绕过变换。

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

# 1080p29.97 预览模拟测试（TTFF/FPS）
$env:MONDRIAN_PREVIEW_SIM_OUTPUT='target/perf/preview-1080p2997.jsonl'
cargo test -p mondrian-app preview_1080p2997_simulated_perf -- --ignored --nocapture

# 4K60 预览模拟测试（高负载性能优化）
$env:MONDRIAN_PREVIEW_SIM_OUTPUT='target/perf/preview-4k60.jsonl'
cargo test -p mondrian-app preview_4k60_simulated_perf -- --ignored --nocapture

# 8K60 预览模拟测试（极限负载性能优化）
$env:MONDRIAN_PREVIEW_SIM_OUTPUT='target/perf/preview-8k60.jsonl'
cargo test -p mondrian-app preview_8k60_simulated_perf -- --ignored --nocapture

# 1080p29.97 导出渲染模拟测试（时间线导出前半段）
$env:MONDRIAN_EXPORT_SIM_OUTPUT='target/perf/export-1080p2997.jsonl'
cargo test -p mondrian-export export_1080p2997_simulated_perf -- --ignored --nocapture

# 4K60 导出渲染模拟测试（高负载）
$env:MONDRIAN_EXPORT_SIM_OUTPUT='target/perf/export-4k60.jsonl'
cargo test -p mondrian-export export_4k60_simulated_perf -- --ignored --nocapture

# 4K60 单层直通导出模拟测试（验证快路径）
$env:MONDRIAN_EXPORT_SIM_OUTPUT='target/perf/export-pass-through-4k60.jsonl'
cargo test -p mondrian-export export_4k60_single_layer_passthrough_simulated_perf -- --ignored --nocapture
```

这些测试都会输出 JSON，便于脚本或 AI 自动分析异常样本。

### 预览合成优化说明（2026-03）

`mondrian-app` 预览 CPU 合成路径已做多项关键优化：

- 多图层合成阶段去除 `RGBA` 数据的重复 `clone`，减少每帧内存拷贝。
- `alpha_blend` 改为整数定点计算，并对“整帧不透明图层”走快速拷贝路径。
- 合成缓冲区改为按需初始化 alpha，避免每帧无条件清屏带来的 4K 内存带宽开销。
- 大分辨率下 `alpha_blend` 按行并行执行（Rayon），提升多核 CPU 利用率。
- 单图层且全尺寸高不透明场景直接返回解码帧，减少一次额外合成开销。
- `alpha_blend` 增加 opacity LUT 与更紧凑的像素循环，减少每像素重复算术开销。
- `alpha_blend` LUT 改为全局预计算表，避免每帧重复构建。
- 并行路径增加最小分块粒度，降低任务切分和线程调度开销。
- 移除混合循环中的冗余 alpha 通道重复写入，减少 8K 场景内存写压力。
- 图层帧缓存改为 `HashMap + 队列` 结构，`get/contains` 从线性查找降为 O(1)，降低高并发预取下的缓存查询开销。
- 预取调度阶段增加图层请求结果复用，避免同一帧在覆盖率评估/就绪评估/预取提交中重复构建请求。
- 图层解码 worker 上限改为可配置（`MONDRIAN_PREVIEW_DECODE_WORKERS`，默认 6），便于按机器核心数调优高负载场景吞吐。
- 多图层解码调度改为持久 `Rayon` 线程池执行，移除每次请求创建/回收线程的开销，降低播放抖动和尾延迟。
- 解码超时预算统一为 `MONDRIAN_DECODE_TIMEOUT_BUDGET_MS`（兼容 `MONDRIAN_PREVIEW_DECODE_TIMEOUT_MS`），UI stall reset 默认使用 `budget + MONDRIAN_DECODE_STALL_GRACE_MS`，避免两端阈值错位导致重复重置。
- 超时诊断新增结构化字段日志：`MONDRIAN_DECODE_TIMEOUT_JSON=...` 与 `MONDRIAN_DECODE_STALL_JSON=...`，便于脚本/AI 直接解析根因样本。- 播放时实现专业非线编时间轴行为:音频连续前进(Audio Master),视频尽力跟随;当掉帧/解码明显落后触发 buffering 阈值时,时间轴短暂停留缓冲,音频同步重置,缓冲完成后继续播放,避免持续掉帧导致音画不同步或播放卡停体验。
该优化主要降低 1080p 预览时的 CPU 开销，并提升播放稳定帧率。
