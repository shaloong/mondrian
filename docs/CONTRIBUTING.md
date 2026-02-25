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

# 3. 安装 FFmpeg（Windows）
winget install ffmpeg
# 设置环境变量
setx FFMPEG_DIR "C:\ProgramData\chocolatey\lib\ffmpeg\tools\ffmpeg"

# 4. 安装 Vulkan SDK（Windows）
# 下载: https://vulkan.lunarg.com/sdk/home

# 5. Clone & Build
git clone https://github.com/mondrian-studio/mondrian
cd mondrian
cargo build
```text

---

## 代码规范

- 运行 `cargo fmt` 格式化代码
- 运行 `cargo clippy --workspace` 检查代码质量
- 所有公共 API 必须有文档注释（`///`）
- 错误处理用 `thiserror` 定义，禁止 `unwrap()`（测试代码除外）
- 异步函数用 Tokio，同步 CPU 密集用 `rayon`

---

## 提交规范（Conventional Commits）

```
feat(timeline): 添加贝塞尔曲线关键帧插值
fix(media): 修复 H.265 硬解码内存泄漏
perf(renderer): 优化 YUV→RGB Shader 性能
docs(ai): 补充 AI 工作流 YAML 格式文档
test(export): 添加渲染队列单元测试
refactor(core): 重构事件总线类型参数
```text

---

## 分支策略

```
main           正式发布（只接受 PR）
dev            开发主分支
feat/xxx       功能分支
fix/xxx        修复分支
perf/xxx       性能优化分支
```text

---

## 测试要求

- 新功能必须附带单元测试
- 核心算法（关键帧插值、色彩转换）必须有 property-based 测试
- 性能敏感路径必须有 benchmark（criterion）
- 运行测试：`cargo nextest run --workspace`
