# Mondrian

[![License](https://img.shields.io/badge/license-AGPL--3.0--or--later-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.92%2B-orange)](https://rustup.rs)
[![Build](https://github.com/shaloong/mondrian/actions/workflows/ci.yml/badge.svg)](https://github.com/shaloong/mondrian/actions)

> [!NOTE]
> Mondrian is in active development. The core editing, preview, and export pipeline is functional. Expect continued iteration on effects, audio, and plugin APIs.

## 🏗️ 系统架构概览

```text
┌─────────────────────────────────────────────────────────────┐
│                   mondrian-app  (UI 层)                     │
│                self-hosted winit / wgpu UI                  │
└──────────────────────────┬──────────────────────────────────┘
                           │ 事件总线 / 命令模式
     ┌─────────────────────┼────────────────────────┐
     │                     │                        │
┌────▼──────┐   ┌──────────▼───────┐   ┌────────────▼───────┐
│ timeline  │   │    renderer      │   │      assets        │
│ 时间线引擎│   │  GPU 渲染管线    │   │   素材资产系统     │
└────┬──────┘   └──────────┬───────┘   └────────────┬───────┘
     │                     │                        │
┌────▼──────┐   ┌──────────▼───────┐   ┌────────────▼───────┐
│   media   │   │     effects      │   │        ai          │
│ 媒体处理  │   │  效果/LUT/转场   │   │   AI 工作流引擎    │
│ FFmpeg    │   │  wgpu Shaders    │   │   Agent / API      │
└────┬──────┘   └──────────────────┘   └────────────────────┘
     │
┌────▼──────┐
│   export  │
│ 渲染队列  │
│ 硬件编码  │
└───────────┘
     ↑ 全局共享：mondrian-core（类型 / 错误 / 事件）
```

## 📦 Crate 结构

| Crate               | 职责                                  | 关键依赖               |
| ------------------- | ------------------------------------- | ---------------------- |
| `mondrian-core`     | 公共类型、错误、事件总线、项目模型    | serde, uuid, thiserror |
| `mondrian-media`    | FFmpeg 解码、音频源缓存、物理输出、Proxy 缓存 | ffmpeg-next, cpal      |
| `mondrian-audio`    | 音频作者模型编译、路由/DSP、嵌套输出与执行 Session | mondrian-core, mondrian-timeline |
| `mondrian-timeline` | 多轨时间线、关键帧、贝塞尔曲线、变速  | mondrian-core          |
| `mondrian-renderer` | wgpu GPU 渲染管线、实时帧合成         | wgpu, bytemuck, glam   |
| `mondrian-assets`   | 素材库、角色/场景/模板、跨项目复用    | serde, sqlite          |
| `mondrian-ai`       | AI Agent 编排、视频生成 API、自动剪辑 | reqwest, tokio         |
| `mondrian-effects`  | DAG 效果图、GPU compute 加速效果、LUT 调色、滤镜、转场 | mondrian-core          |
| `mondrian-export`   | 导出编码、渲染队列、硬件加速          | ffmpeg-next            |
| `mondrian-app`      | 主程序入口、自研 UI 壳、UI 状态机、面板布局 | winit, wgpu, legacy egui reference |

## 🚀 快速开始

### 环境要求

- Rust 1.92+
- FFmpeg 开发库与 `ffmpeg`/`ffprobe` CLI（仅源码构建需要；发行包自带私有动态运行时）
- Vulkan / Metal / DirectX 12 驱动
- Windows 11 / macOS 13+ / Ubuntu 22.04+

### 构建

```bash
# 克隆仓库
git clone https://github.com/mondrian-studio/mondrian
cd mondrian

# 安装与 CI 同构的 Windows 媒体运行时
git clone https://github.com/microsoft/vcpkg C:\vcpkg
C:\vcpkg\bootstrap-vcpkg.bat -disableMetrics
C:\vcpkg\vcpkg.exe install "ffmpeg[zlib,ffmpeg,ffprobe,gpl,x264,x265,aom]:x64-windows" --recurse

# Debug 构建
cargo build

# Release 构建（优化 + LTO）
cargo build --release

# 运行主程序
cargo run -p mondrian-app
```

### 测试

```powershell
# 运行全部测试
cargo test --workspace

# 运行特定模块
cargo test -p mondrian-timeline

$perfRun = "target/perf/manual-$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())"
New-Item -ItemType Directory -Path $perfRun | Out-Null

# 性能烟雾测试（项目创建/打开/保存，输出 AI 可解析 JSON）
$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'project-lifecycle.jsonl'; cargo test -p mondrian-app --release -j 2 --lib perf_project_lifecycle_smoke -- --ignored --nocapture --test-threads=1

# 生产媒体解码/缓存 smoke（FFmpeg 生成短 testsrc2，不是 1080p 产品门禁）
$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'preview-media.jsonl'; cargo test -p mondrian-app --release -j 2 --lib preview_media_decode_cache_smoke -- --ignored --nocapture --test-threads=1

# 生产连续播放 smoke（同样使用本地生成的短测试素材）
$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'preview-playback.jsonl'; cargo test -p mondrian-app --release -j 2 --lib preview_media_continuous_playback_smoke -- --ignored --nocapture --test-threads=1

# 稠密音频 Schedule 的 scalar/SIMD 多轨负载矩阵
$env:MONDRIAN_AUDIO_LOAD_MATRIX_OUTPUT=Join-Path $perfRun 'audio-load-matrix.jsonl'; cargo test -p mondrian-audio --release -j 2 --test load_matrix dense_schedule_multitrack_load_matrix -- --ignored --nocapture --test-threads=1

# Benchmark
cargo bench -p mondrian-renderer

# GPU 渲染性能剖析（输出 JSON 报告）
$env:MONDRIAN_RENDER_PROFILE=1; cargo run -p mondrian-app

# 金标准图像回归测试
cargo test -p mondrian-renderer golden
```

性能烟雾测试支持通过环境变量调整阈值：

- `MONDRIAN_PERF_CREATE_MS`：创建项目最大耗时（毫秒，默认 `8000`）
- `MONDRIAN_PERF_OPEN_MS`：打开项目最大耗时（毫秒，默认 `6000`）
- `MONDRIAN_PERF_SAVE_MS`：保存项目最大耗时（毫秒，默认 `6000`）
- `MONDRIAN_PERF_OPEN_ITERS`：打开项目采样次数（默认 `3`）
- `MONDRIAN_PERF_SAVE_ITERS`：保存项目采样次数（默认 `5`）
- `MONDRIAN_PERF_OUTPUT`：可选，写入 JSONL 报告路径（每行一条 JSON）。每项 smoke 必须使用独立的新文件。

短 Preview smoke 说明：

- Preview 报告使用与其他 app smoke 相同的 `MONDRIAN_PERF_OUTPUT`。
两个 Preview smoke 都通过生产媒体路径生成短时、无版权依赖的本地测试素材。
真实 4K HEVC Main10、长期 A/V 同步与设备时钟验证由
`scripts/validation/invoke-playback-reference-gates.ps1` 负责，不能用内存模拟帧替代。

## 📋 功能路线图

见 [docs/ROADMAP.md](docs/ROADMAP.md)

## 🗂️ 文档索引

| 文档                                                | 说明              |
| --------------------------------------------------- | ----------------- |
| [架构总览](docs/architecture/overview.md)           | 系统设计全貌      |
| [媒体处理管线](docs/architecture/media-pipeline.md) | FFmpeg + 代理缓存 |
| [时间线系统](docs/architecture/timeline-system.md)  | 非线编核心设计    |
| [渲染引擎](docs/architecture/renderer.md)           | GPU 渲染管线      |
| [AI 工作流](docs/architecture/ai-workflow.md)       | Agent 系统设计    |
| [素材资产系统](docs/architecture/asset-system.md)   | 跨项目复用架构    |
| [效果系统](docs/architecture/effects-system.md)     | LUT / 滤镜 / 转场 |
| [插件开发手册](docs/plugins/README.md) | 插件开发者完整手册 |
| [导出系统](docs/architecture/export-system.md)      | 渲染队列设计      |
| [设计准则](docs/DESIGN_GUIDELINES.md)               | UI/UX 设计基线    |
| [技术栈选型](docs/TECH_STACK.md)                    | 选型理由与对比    |
| [路线图](docs/ROADMAP.md)                           | 版本规划          |
| [贡献指南](docs/CONTRIBUTING.md)                    | 开发规范          |

## 🤝 贡献

请阅读 [CONTRIBUTING.md](docs/CONTRIBUTING.md)。

## 📄 许可证

本项目采用 **GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later)** 协议。
