# Mondrian

<img src="/crates/mondrian-app/assets/app-ico.png" alt="Mondrian Logo" width="180" />

> **专业级非线性视频编辑器 · AI 增强生产力工具**

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange)](https://rustup.rs)
[![Build](https://github.com/mondrian-studio/mondrian/actions/workflows/ci.yml/badge.svg)](https://github.com/mondrian-studio/mondrian/actions)

---

## 🎯 产品定位

```text
专业级生产力工具  +  AI 增强层
        ↓                ↓
  非线编核心能力    AI 工作流引擎
        ↓                ↓
       资产复用系统（跨项目）
```

### 核心价值

Mondrian = 工业级非线编能力 + AI 融合生态 + 可复用资产生态

---

## 🏗️ 系统架构概览

```text
┌─────────────────────────────────────────────────────────────┐
│                   mondrian-app  (UI 层)                     │
│              egui → 迁移至 CXX-Qt (v0.3+)                   │
└──────────────────────────┬──────────────────────────────────┘
                           │ 事件总线 / 命令模式
     ┌─────────────────────┼─────────────────────────┐
     │                     │                         │
┌────▼──────┐   ┌──────────▼───────┐   ┌────────────▼───────┐
│ timeline  │   │    renderer      │   │      assets        │
│ 时间线引擎│   │  GPU 渲染管线    │   │   素材资产系统     │
└────┬──────┘   └──────────┬───────┘   └────────────┬───────┘
     │                     │                         │
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

---

## 📦 Crate 结构

| Crate               | 职责                                  | 关键依赖               |
| ------------------- | ------------------------------------- | ---------------------- |
| `mondrian-core`     | 公共类型、错误、事件总线、项目模型    | serde, uuid, thiserror |
| `mondrian-media`    | FFmpeg 解码、音频处理、Proxy 代理缓存 | ffmpeg-next, cpal      |
| `mondrian-timeline` | 多轨时间线、关键帧、贝塞尔曲线、变速  | mondrian-core          |
| `mondrian-renderer` | wgpu GPU 渲染管线、实时帧合成         | wgpu, bytemuck, glam   |
| `mondrian-assets`   | 素材库、角色/场景/模板、跨项目复用    | serde, sqlite          |
| `mondrian-ai`       | AI Agent 编排、视频生成 API、自动剪辑 | reqwest, tokio         |
| `mondrian-effects`  | LUT 调色、滤镜、转场、文字动画        | mondrian-renderer      |
| `mondrian-export`   | 导出编码、渲染队列、硬件加速          | ffmpeg-next            |
| `mondrian-app`      | 主程序入口、UI 状态机、面板布局       | egui/CXX-Qt            |

---

## 🚀 快速开始

### 环境要求

- Rust 1.75+
- FFmpeg 7.x（动态链接）
- Vulkan / Metal / DirectX 12 驱动
- Windows 11 / macOS 13+ / Ubuntu 22.04+

### 构建

```bash
# 克隆仓库
git clone https://github.com/mondrian-studio/mondrian
cd mondrian

# 安装 FFmpeg（Windows）
winget install ffmpeg

# Debug 构建
cargo build

# Release 构建（优化 + LTO）
cargo build --release

# 运行主程序
cargo run -p mondrian-app
```

### 测试

```bash
# 运行全部测试
cargo test --workspace

# 运行特定模块
cargo test -p mondrian-timeline

# 性能烟雾测试（项目创建/打开/保存，输出 AI 可解析 JSON）
$env:MONDRIAN_PERF_OUTPUT='target/perf/project-lifecycle.jsonl'; cargo test -p mondrian-app perf_project_lifecycle_smoke -- --ignored --nocapture

# 1080p29.97 预览模拟性能测试（广播常见帧率）
$env:MONDRIAN_PREVIEW_SIM_OUTPUT='target/perf/preview-1080p2997.jsonl'; cargo test -p mondrian-app preview_1080p2997_simulated_perf -- --ignored --nocapture

# 4K60 预览模拟性能测试（用于更高负载优化）
$env:MONDRIAN_PREVIEW_SIM_OUTPUT='target/perf/preview-4k60.jsonl'; cargo test -p mondrian-app preview_4k60_simulated_perf -- --ignored --nocapture

# 8K60 预览模拟性能测试（极限负载优化）
$env:MONDRIAN_PREVIEW_SIM_OUTPUT='target/perf/preview-8k60.jsonl'; cargo test -p mondrian-app preview_8k60_simulated_perf -- --ignored --nocapture

# Benchmark
cargo bench -p mondrian-renderer
```

性能烟雾测试支持通过环境变量调整阈值：

- `MONDRIAN_PERF_CREATE_MS`：创建项目最大耗时（毫秒，默认 `8000`）
- `MONDRIAN_PERF_OPEN_MS`：打开项目最大耗时（毫秒，默认 `6000`）
- `MONDRIAN_PERF_SAVE_MS`：保存项目最大耗时（毫秒，默认 `6000`）
- `MONDRIAN_PERF_OPEN_ITERS`：打开项目采样次数（默认 `3`）
- `MONDRIAN_PERF_SAVE_ITERS`：保存项目采样次数（默认 `5`）
- `MONDRIAN_PERF_OUTPUT`：可选，写入 JSONL 报告路径（每行一条 JSON）

1080p29.97 预览模拟测试关键环境变量：

- `MONDRIAN_PREVIEW_SIM_TTFF_MS`：首帧显示上限（毫秒，默认 `2000`）
- `MONDRIAN_PREVIEW_SIM_FPS_MIN`：稳定播放最低 FPS（默认 `27`）
- `MONDRIAN_PREVIEW_SIM_FPS_MAX`：稳定播放最高 FPS（默认 `30`）
- `MONDRIAN_PREVIEW_SIM_FRAMES`：模拟帧数（默认 `96`）
- `MONDRIAN_PREVIEW_SIM_LAYERS`：模拟合成图层数（默认 `2`）
- `MONDRIAN_PREVIEW_SIM_OUTPUT`：可选，写入 JSONL 报告路径

4K60 预览模拟测试关键环境变量：

- `MONDRIAN_PREVIEW_SIM_4K_WIDTH`：模拟宽度（默认 `3840`）
- `MONDRIAN_PREVIEW_SIM_4K_HEIGHT`：模拟高度（默认 `2160`）
- `MONDRIAN_PREVIEW_SIM_4K_TARGET_FPS`：目标 FPS（默认 `60`）
- `MONDRIAN_PREVIEW_SIM_4K_TTFF_MS`：首帧显示上限（默认 `3000`）
- `MONDRIAN_PREVIEW_SIM_4K_FPS_MIN`：最低 FPS 门槛（默认 `30`）
- `MONDRIAN_PREVIEW_SIM_4K_FPS_MAX`：最高 FPS 门槛（默认 `60`）
- `MONDRIAN_PREVIEW_SIM_4K_FRAMES`：模拟帧数（默认 `120`）
- `MONDRIAN_PREVIEW_SIM_4K_LAYERS`：模拟图层数（默认 `2`）

8K60 预览模拟测试关键环境变量：

- `MONDRIAN_PREVIEW_SIM_8K_WIDTH`：模拟宽度（默认 `7680`）
- `MONDRIAN_PREVIEW_SIM_8K_HEIGHT`：模拟高度（默认 `4320`）
- `MONDRIAN_PREVIEW_SIM_8K_TARGET_FPS`：目标 FPS（默认 `60`）
- `MONDRIAN_PREVIEW_SIM_8K_TTFF_MS`：首帧显示上限（默认 `4000`）
- `MONDRIAN_PREVIEW_SIM_8K_FPS_MIN`：最低 FPS 门槛（默认 `20`）
- `MONDRIAN_PREVIEW_SIM_8K_FPS_MAX`：最高 FPS 门槛（默认 `60`）
- `MONDRIAN_PREVIEW_SIM_8K_FRAMES`：模拟帧数（默认 `120`）
- `MONDRIAN_PREVIEW_SIM_8K_LAYERS`：模拟图层数（默认 `2`）

---

## 📋 功能路线图

见 [docs/ROADMAP.md](docs/ROADMAP.md)

---

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
| [导出系统](docs/architecture/export-system.md)      | 渲染队列设计      |
| [技术栈选型](docs/TECH_STACK.md)                    | 选型理由与对比    |
| [路线图](docs/ROADMAP.md)                           | 版本规划          |
| [贡献指南](docs/CONTRIBUTING.md)                    | 开发规范          |

---

## 🤝 贡献

请阅读 [CONTRIBUTING.md](docs/CONTRIBUTING.md)。

---

## 📄 许可证

本项目采用 **MIT OR Apache-2.0** 双协议。
