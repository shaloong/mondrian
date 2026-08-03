# 贡献指南

## 开发环境搭建

```bash
# 1. 安装 Rust（stable channel）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable

# 2. 安装工具链
cargo install cargo-watch
cargo install cargo-nextest    # 更快的测试运行器
cargo install cargo-audit      # 安全审计

# 3. 安装 FFmpeg（Windows，推荐与 CI 对齐）
git clone https://github.com/microsoft/vcpkg C:\vcpkg
C:\vcpkg\bootstrap-vcpkg.bat -disableMetrics
C:\vcpkg\vcpkg.exe install "ffmpeg[zlib,ffmpeg,ffprobe,gpl,x264,x265,aom]:x64-windows" --recurse
# 设置环境变量（PowerShell）
$env:VCPKG_ROOT="C:\vcpkg"
$env:VCPKGRS_TRIPLET="x64-windows"
$env:VCPKGRS_DYNAMIC="1"
$env:VCPKG_DEFAULT_TRIPLET="x64-windows"
$env:FFMPEG_DIR="C:\vcpkg\installed\x64-windows"
$env:PKG_CONFIG_PATH="C:\vcpkg\installed\x64-windows\lib\pkgconfig"
$env:PKG_CONFIG="C:\vcpkg\installed\x64-windows\tools\pkgconf\pkgconf.exe"

# 4. 安装 Vulkan SDK（Windows）
# 下载: https://vulkan.lunarg.com/sdk/home

# 5. Clone & Build
git clone https://github.com/mondrian-studio/mondrian
cd mondrian
cargo build
```

## 代码规范

- 运行 `cargo fmt` 格式化代码
- 运行 `cargo clippy --workspace --all-targets --all-features -- -D warnings` 检查代码质量（与 CI 一致）
- 所有公共 API 必须有文档注释（`///`）
- 错误处理用 `thiserror` 定义，禁止 `unwrap()`（测试代码除外）
- Tokio 只承载明确的异步 I/O；CPU/媒体工作使用领域 Module 自有的有界执行器，并显式传递资源 grant、取消与终态证据，禁止引入全局通用线程池

## 提交规范（Conventional Commits）

```text
feat(timeline): 添加贝塞尔曲线关键帧插值
fix(media): 修复 H.265 硬解码内存泄漏
perf(renderer): 优化 YUV→RGB Shader 性能
docs(ai): 补充 AI 工作流 YAML 格式文档
test(export): 添加渲染队列单元测试
refactor(core): 重构事件总线类型参数
```

## 分支策略

```text
main           正式发布（只接受 PR）
dev            开发主分支
feat/xxx       功能分支
fix/xxx        修复分支
perf/xxx       性能优化分支
```

## 发布流程（GitHub Release）

- 发布工作流文件：`.github/workflows/release.yml`
- Tag 触发规则：`v<major>.<minor>.<patch>`（例如 `v0.1.1`）
- 预发布 Tag：`v<major>.<minor>.<patch>-<channel>`（例如 `v0.2.0-rc1`）

### 发布前检查（建议）

```bash
cargo fmt
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
cargo test -p mondrian-app --test product_entrypoint_contract
cargo test -p mondrian-app app_ui
cargo test -p mondrian-ui-renderer
cargo test -p mondrian-ui-widgets component_extreme_tests
```

### 正式发布步骤

```bash
git checkout main
git pull --ff-only
git tag -a v0.1.1 -m "release: v0.1.1"
git push origin v0.1.1
```

说明：

- Release 工作流会为 Linux/macOS/Windows 构建自包含运行时并上传产物。
- Linux 构建依赖 `libasound2-dev`（用于 `alsa-sys`）。
- Windows 构建使用 vcpkg 安装完整产品 profile：链接库、`ffmpeg`/`ffprobe`、PNG/EXR decoder，以及 Export 声明的软件编码器；不能用只有 `libavcodec.pc` 的旧缓存冒充。
- Windows Release 包会同时包含 `mondrian.exe`、`ffmpeg.exe`、`ffprobe.exe` 与完整运行时 DLL closure。
- Linux Release 包会把 Mondrian、`ffmpeg`、`ffprobe` 的递归非基础系统动态库收敛到同一个私有 `lib/` 并设置相对 RPATH；macOS Release 包会把三个可执行文件的非系统 dylib 收敛到 App Bundle 的 `Contents/Frameworks` 并重写加载路径。
- 三个平台都必须在净化环境中执行 `mondrian --verify-runtime`；该命令拒绝 PATH-only 工具，并验证链接 decoder、CLI encoder/filter/muxer 和 `--enable-nonfree`。媒体或 OCIO 运行时不完整会直接阻止发布产物上传。
- `workflow_dispatch` 可用于手动 dry-run 验证构建，不会自动创建 GitHub Release。

## 测试要求

- 新功能必须附带单元测试
- 核心算法（关键帧插值、色彩转换）必须有 property-based 测试
- 性能敏感路径必须有 benchmark（criterion）
- 运行测试：`cargo nextest run --workspace`
- 自研 UI 相关变更还必须跑 app UI 产品入口、`mondrian-app app_ui`
  过滤测试、`mondrian-ui-renderer` primitive/draw-command 测试，以及
  `mondrian-ui-widgets component_extreme_tests`。

### 性能回归门禁（推荐）

对容易卡顿的路径（项目加载、预览、渲染前准备）建议至少配置一个 smoke 级性能测试，并输出机器可读 JSON，便于 AI 自动定位回退。

```powershell
$perfRun = "target/perf/manual-$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())"
New-Item -ItemType Directory -Path $perfRun | Out-Null
$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'project-lifecycle.jsonl'
$env:MONDRIAN_PERF_OPEN_MS='3500'
$env:MONDRIAN_PERF_SAVE_MS='2500'
cargo test -p mondrian-app --release -j 2 --lib perf_project_lifecycle_smoke -- --ignored --nocapture --test-threads=1

$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'preview-media.jsonl'
cargo test -p mondrian-app --release -j 2 --lib preview_media_decode_cache_smoke -- --ignored --nocapture --test-threads=1

$env:MONDRIAN_EXPORT_SIM_OUTPUT=Join-Path $perfRun 'export-1080p2997.jsonl'
cargo test -p mondrian-export --release -j 2 --lib export_1080p2997_simulated_perf -- --ignored --nocapture --test-threads=1

$env:MONDRIAN_AUDIO_LOAD_MATRIX_OUTPUT=Join-Path $perfRun 'audio-load-matrix.jsonl'
cargo test -p mondrian-audio --release -j 2 --test load_matrix dense_schedule_multitrack_load_matrix -- --ignored --nocapture --test-threads=1
```

- 失败时测试会直接报错并附带 JSON 报告。
- 成功时会打印对应的结构化报告；每个 JSONL smoke 必须使用独立的新文件。
- 进程返回成功但明确报告 `skipped` 的运行不构成性能证据。
- 本地开发可放宽阈值，CI 建议使用更严格阈值并固定机器规格。

### 前后对比流程（性能优化后建议实施）

每次性能优化都要保留 baseline，并做 before/after 对比，避免“主观感觉变快”：

```powershell
# 1) 在优化前分支跑一轮，保存 baseline
powershell -File scripts/perf/run-perf-suite.ps1 -OutputDir target/perf/baseline

# 2) 在优化后分支跑一轮，保存 current
powershell -File scripts/perf/run-perf-suite.ps1 -OutputDir target/perf/current

# 3) 严格核对 workload/case 集并以 5% 容差拒绝 project/export 回退
powershell -File scripts/perf/compare-perf.ps1 -BeforeDir target/perf/baseline -AfterDir target/perf/current -RegressionTolerancePct 5 -FailOnRegression
```

### 样片与 golden fixtures 约定

可再分发且许可证清晰的小型 golden fixture 才能进入仓库的
`tests/fixtures/`。下载的专业样片、本机素材或来源不明的媒体只能放在
gitignored 的外部验证目录，并通过环境变量或 reference-validation manifest
引用，禁止提交到 Git 历史。

仓库内可再分发 fixture 按用途分目录：

- `tests/fixtures/color/`：色彩 golden samples、参考帧、HDR/SDR 对照样片
- `tests/fixtures/lut/`：`.cube` LUT 文件、缓存命中/失效样片
- `tests/fixtures/export/`：导出编码合法性、metadata、range/bit-depth 组合样片
- `tests/fixtures/sequence/`：嵌套序列、PAR、场序、帧率覆盖样片

建议命名规则：

- `*_src.*`：输入样片
- `*_golden.*`：参考输出
- `*_hdr.*` / `*_sdr.*`：动态范围变体
- `*_legal.*` / `*_full.*`：range 变体
- `*_rec709.*` / `*_rec2020.*` / `*_hlg.*` / `*_pq.*` / `*_log.*`：色彩空间变体

如果样片体积很大，不建议直接塞进主线历史；优先放小尺寸裁剪样片，或配套 manifest + 下载脚本。
