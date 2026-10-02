# Mondrian

English · [简体中文](README.zh-CN.md)

[![License](https://img.shields.io/badge/license-AGPL--3.0--or--later%20%2F%20Commercial-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.97.1%2B-orange)](https://rustup.rs)
[![Build](https://github.com/shaloong/mondrian/actions/workflows/ci.yml/badge.svg)](https://github.com/shaloong/mondrian/actions)

Mondrian is a native non-linear video editor built in Rust, with a multitrack timeline, GPU preview, color processing, and media export.

> [!NOTE]
> Mondrian is in active development. The core editing, preview, and export pipeline is functional. Expect continued iteration on effects, audio, and plugin APIs.

## 🏗️ Architecture overview

```text
                         mondrian-app
                  self-hosted winit / wgpu UI
                      Events / commands
                              |
            +-----------------+-----------------+
            |                 |                 |
         timeline          renderer           assets
            |                 |                 |
          media            effects       ai (experimental)
         FFmpeg          wgpu shaders     Provider contracts
            |
          export
      Encoding / queues

Shared: mondrian-core (types / errors / events)
```

## 📦 Core crates

| Crate | Responsibility | Key dependencies |
| ------------------- | ------------------------------------- | ---------------------- |
| `mondrian-core` | Shared types, errors, events and project model | serde, uuid, thiserror |
| `mondrian-media` | FFmpeg decoding, audio-source caches, physical output and proxies | ffmpeg-next, cpal |
| `mondrian-audio` | Audio authoring compilation, routing/DSP, nested output and execution sessions | mondrian-core, mondrian-timeline |
| `mondrian-timeline` | Multitrack timelines, keyframes, Bezier curves and retiming | mondrian-core |
| `mondrian-renderer` | wgpu rendering and real-time frame compositing | wgpu, bytemuck, glam |
| `mondrian-assets` | Asset libraries, roles/scenes/templates and reuse across projects | serde, sqlite |
| `mondrian-ai` | Experimental provider contracts and workflow schema | serde_yaml, tokio |
| `mondrian-effects` | Effect graphs, GPU compute, LUT grading, filters and transitions | mondrian-core |
| `mondrian-export` | Export encoding, render queues and hardware acceleration | ffmpeg-next |
| `mondrian-app` | Application entry point, self-hosted UI, state machines and panel layout | winit, wgpu |

## 🚀 Quick start

### Requirements

- Rust 1.97.1+
- FFmpeg development libraries and the `ffmpeg`/`ffprobe` tools for source builds; the Windows candidate workflow bundles a private dynamic runtime
- Vulkan / Metal / DirectX 12 drivers
- Windows 11 / macOS 13+ / Ubuntu 22.04+

Distribution qualification currently targets Windows x86_64. Linux and macOS remain source-build and CI targets until their distribution qualification is established. The AI crate provides experimental contracts and workflow models, without a production provider or editor-mutation adapter.

### Build (Windows PowerShell)

See [setup](docs/dev/setup.md) and [Windows development](docs/dev/windows-development.md) for the complete platform and native-dependency requirements.

```powershell
# Clone the repository
git clone https://github.com/shaloong/mondrian
cd mondrian

# Install the Windows media runtime used by CI
git clone --branch 2026.07.29 --depth 1 https://github.com/microsoft/vcpkg C:\vcpkg
C:\vcpkg\bootstrap-vcpkg.bat -disableMetrics
C:\vcpkg\vcpkg.exe install "ffmpeg[zlib,ffmpeg,ffprobe,gpl,x264,x265,aom,nvcodec]:x64-windows" --recurse --overlay-ports=vcpkg-overlay

# Install MSVC and LLVM as documented, then activate the current PowerShell 7 terminal
. ./scripts/enter-windows-development.ps1 -VcpkgRoot C:\vcpkg

# Debug build
cargo build --locked

# Release source build; distribution qualification is a separate step
cargo build --release --locked

# Run the application
cargo run -p mondrian-app
```

### Tests

```powershell
# Run all tests
cargo test --workspace

# Test a specific crate
cargo test -p mondrian-timeline

$perfRun = "target/perf/manual-$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())"
New-Item -ItemType Directory -Path $perfRun | Out-Null

# Project create/open/save performance smoke with machine-readable JSON
$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'project-lifecycle.jsonl'; cargo test -p mondrian-app --release -j 2 --lib perf_project_lifecycle_smoke -- --ignored --nocapture --test-threads=1

# Production media decode/cache smoke using a short FFmpeg testsrc2 fixture
$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'preview-media.jsonl'; cargo test -p mondrian-app --release -j 2 --lib preview_media_decode_cache_smoke -- --ignored --nocapture --test-threads=1

# Continuous playback smoke using a short locally generated fixture
$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'preview-playback.jsonl'; cargo test -p mondrian-app --release -j 2 --lib preview_media_continuous_playback_smoke -- --ignored --nocapture --test-threads=1

# Dense multitrack audio schedule scalar/SIMD load matrix
$env:MONDRIAN_AUDIO_LOAD_MATRIX_OUTPUT=Join-Path $perfRun 'audio-load-matrix.jsonl'; cargo test -p mondrian-audio --release -j 2 --test load_matrix dense_schedule_multitrack_load_matrix -- --ignored --nocapture --test-threads=1

# Benchmark
cargo bench -p mondrian-renderer

# GPU render profiling with a JSON report
$env:MONDRIAN_RENDER_PROFILE=1; cargo run -p mondrian-app

# Golden-image regression tests
cargo test -p mondrian-renderer golden
```

Performance smoke thresholds can be configured with environment variables:

- `MONDRIAN_PERF_CREATE_MS`: Maximum project creation time in milliseconds (default: `8000`)
- `MONDRIAN_PERF_OPEN_MS`: Maximum project open time in milliseconds (default: `6000`)
- `MONDRIAN_PERF_SAVE_MS`: Maximum project save time in milliseconds (default: `6000`)
- `MONDRIAN_PERF_OPEN_ITERS`: Number of project-open samples (default: `3`)
- `MONDRIAN_PERF_SAVE_ITERS`: Number of project-save samples (default: `5`)
- `MONDRIAN_PERF_OUTPUT`: Optional JSONL report path, with one JSON record per line. Use a fresh file for each smoke test.

The short Preview smoke tests use the production media path:

Both tests write to `MONDRIAN_PERF_OUTPUT` and generate short local fixtures
without third-party media. Real 4K HEVC Main10, long-duration A/V synchronization
and device-clock validation use
`scripts/validation/invoke-playback-reference-gates.ps1`; simulated in-memory
frames cannot substitute for those gates.

## 📋 Roadmap

See [the roadmap](docs/ROADMAP.md).

## 🗂️ Documentation

| Document | Description |
| --------------------------------------------------- | ----------------- |
| [Architecture](docs/architecture/overview.md) | System design |
| [Media pipeline](docs/architecture/media-pipeline.md) | FFmpeg + proxy caches |
| [Timeline model](docs/architecture/timeline-model.md) | Editing model |
| [Render pipeline](docs/architecture/render-pipeline.md) | GPU rendering |
| [Execution resources](docs/architecture/execution-resource-coordination.md) | Media and CPU scheduling |
| [Asset library](docs/architecture/project-model.md) | Reuse across projects |
| [Effects](docs/architecture/effect-system.md) | LUTs / filters / transitions |
| [Plugin handbook](docs/plugins/README.md) | Source-crate effect SDK |
| [Delivery](docs/architecture/professional-delivery.md) | Render queues |
| [Design system](docs/ui/design-system.md) | UI/UX rules |
| [Technology choices](docs/architecture/overview.md) | Design rationale |
| [Roadmap](docs/ROADMAP.md) | Milestones |
| [Contributing](docs/CONTRIBUTING.md) | Contributor guidance |

## Contributing

Read [the contribution guide](docs/CONTRIBUTING.md). External PRs target `develop`.

[Support](SUPPORT.md) · [Code of conduct](CODE_OF_CONDUCT.md) · [Security reports](.github/SECURITY.md) · [Governance](GOVERNANCE.md)

## 📄 License

Mondrian is available under **AGPL-3.0-or-later** or a separately negotiated
**commercial license**. Independent extensions may choose their own terms and
pricing under the [Plugin Additional Permission](LICENSES/PLUGIN-EXCEPTION.md),
including extensions that change or replace functionality. Copied or adapted
Mondrian implementation does not receive that exemption.

See [LICENSE](LICENSE) for the complete terms. Commercial licensing covers only
rights the licensor is authorized to grant and does not replace third-party
licenses. Contact **contact@shaloong.com** for commercial licensing.
