# Mondrian 技术栈选型

## 一、核心语言：Rust

| 对比项       | Rust                | C++       | Go           |
| ------------ | ------------------- | --------- | ------------ |
| 内存安全     | ✅ 编译期保证       | ❌ 需人工 | ✅ GC        |
| 零成本抽象   | ✅                  | ✅        | ❌           |
| 并发模型     | ✅ 无数据竞争       | ⚠️ 复杂   | ✅ Goroutine |
| 跨平台       | ✅                  | ✅        | ✅           |
| 生态（媒体） | 成熟（ffmpeg-next） | 最成熟    | 一般         |
| 编译速度     | 慢（可接受）        | 慢        | 快           |

**选 Rust 的核心理由：**

- 媒体处理需要零拷贝、精确内存控制
- 多线程解码/渲染需要无竞争并发模型
- 渲染引擎需要与 wgpu/Vulkan 零成本绑定
- 长期维护性优于 C++

---

## 二、媒体解码：FFmpeg（通过 ffmpeg-next）

```toml
ffmpeg-next = "8"
```

**为什么不用其他方案？**

| 方案            | 优点                       | 缺点            |
| --------------- | -------------------------- | --------------- |
| FFmpeg          | 支持所有格式 / 硬解 / 音频 | 编译复杂        |
| GStreamer       | 插件化                     | Rust 绑定不成熟 |
| MediaFoundation | Windows 原生               | 平台锁定        |

FFmpeg 是唯一覆盖 H.264/H.265/ProRes/AV1 + GPU 硬解的方案。

**GPU 硬解加速路径：**

```text
Windows : NVENC / DXVA2 / D3D11VA
macOS   : VideoToolbox
Linux   : VAAPI / NVDEC
```

---

## 三、GPU 渲染：wgpu

```toml
wgpu = "29.0"
```

**wgpu vs 直接使用 Vulkan/Metal/DX12**

| 特性      | wgpu        | Vulkan   | Metal     | DX12       |
| --------- | ----------- | -------- | --------- | ---------- |
| 跨平台    | ✅ 统一 API | ❌       | ❌        | ❌         |
| Rust 原生 | ✅          | 通过 ash | 通过 objc | 通过 d3d12 |
| 性能      | ~95% 原生   | 100%     | 100%      | 100%       |
| 开发效率  | ✅ 高       | 低       | 中        | 低         |

**wgpu 后端映射：**

```text
Windows → DirectX 12 / Vulkan
macOS   → Metal
Linux   → Vulkan
WebGPU  → 浏览器版（未来）
```

当前使用 wgpu 29.0。产品 UI 是 self-hosted winit/wgpu shell；preview 与
export compositor 均通过同一 device/queue 语义提交工作，不再保留 egui/eframe
产品路径。

---

## 四、UI 框架：self-hosted winit/wgpu

Mondrian 的产品 UI 走自研 self-hosted 栈：

```text
winit / mondrian-platform
wgpu / mondrian-ui-renderer
cosmic-text / mondrian-ui-text
mondrian-ui-core / layout / events / widgets / tooltip / theme
mondrian-editor-ui / mondrian-app::self_hosted
```

- Rust 负责窗口、事件、布局、渲染、组件、文本、主题和编辑器状态接入。
- UI token、dock/panel、overlay、shortcut、IME、tooltip、drag/drop、viewer 和
  timeline 行为在 self-hosted 栈内收敛。
- 旧 egui/eframe 原型代码已从 `mondrian-app` crate 删除；后续 UI 工作不得新增
  egui 兼容路径。

---

## 五、异步运行时：Tokio

```toml
tokio = { version = "1", features = ["full"] }
```

- AI API 调用（async HTTP）
- 后台渲染队列
- 文件 IO（proxy 生成）
- WebSocket（实时协作，v2.0）

---

## 六、数据库：SQLite（via rusqlite）

```toml
rusqlite = { version = "0.31", features = ["bundled"] }
```

用于：

- 素材库元数据索引
- 项目历史 / 撤销栈持久化
- 渲染缓存记录

---

## 七、AI API 集成层

| 能力             | Cloud API（推荐）    | Local（可选）                    |
| ---------------- | -------------------- | -------------------------------- |
| 字幕转写         | Whisper API          | faster-whisper + WhisperX        |
| 字幕翻译         | GPT-4o / Claude 3.5  | 本地 LLM（OpenAI 兼容接口）      |
| AI 配音/语音克隆 | ElevenLabs           | IndexTTS                         |
| 智能剪辑语义分析 | GPT-4o / Gemini      | 本地 LLM + 规则后处理            |
| 视频生成         | Runway Gen-3 / Kling | （预留，本地模型视性能逐步引入） |

所有 AI 能力统一通过 `mondrian-ai` Provider 抽象层调用，并支持：

- 模型可选：同一能力可切换不同模型
- 部署可选：`Cloud` / `Local` / `Hybrid`
- 用户体验可视化：普通用户通过能力卡片配置，不要求编写 JSON/YAML

---

## 八、核心库依赖汇总

```toml
# 音频
cpal        = "0.15"   # 跨平台音频输出
rubato      = "0.15"   # 重采样
symphonia   = "0.5"    # 纯 Rust 音频解码（备用）

# 图像处理
image       = "0.25"
fast_image_resize = "4"

# 数学
glam        = "0.27"   # SIMD 向量数学
nalgebra    = "0.33"   # 矩阵变换（可选）

# 文件格式
serde       = "1"
toml        = "0.8"
ron         = "0.8"    # 项目文件格式（可读性好）

# 测试
criterion   = "0.5"    # Benchmark
proptest    = "1"      # 属性测试
```
