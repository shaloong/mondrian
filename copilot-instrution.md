# AI Coding 规范（Copilot 指令）

> 文件名：`copilot-instrution.md`
> 适用范围：本仓库所有代码、文档、脚本与配置变更。
> 强制级别：**MUST（必须）**。

## 1. 目标与定位

本规范用于约束 AI 与开发者协作时的行为边界与交付质量，确保：

- 代码可读、可测、可维护。
- 提交历史清晰、可追踪、可回滚。
- 架构演进可控，不引入隐性技术债。
- 文档与实现同步，避免知识断层。

---

## 2. 基本原则（必须严格遵守）

1. 正确性优先于速度。
2. 可读性优先于“炫技”。
3. 简单方案优先于过度设计（YAGNI）。
4. 单一职责与低耦合（SOLID 思想）。
5. 不破坏现有行为与公共接口，除非明确需求允许。
6. 任何改动必须可验证（编译、测试、最小运行验证）。
7. 不提交临时调试代码、注释掉的大段旧代码、无意义日志。
8. 不臆造需求；有不确定项必须显式说明假设。
9. 小步提交、渐进式重构，避免“大爆炸式改动”。
10. 每次代码改动后，**必须同步更新相关文档**。

---

## 3. 开发规范

### 3.1 变更前

- 先理解上下文：阅读相关模块、接口、调用链和约束。
- 明确影响范围：功能、性能、兼容性、数据结构、并发安全。
- 对不确定点记录假设并尽快验证。

### 3.2 编码中

- 保持函数短小、命名语义化、模块边界清晰。
- 避免重复逻辑，提炼可复用组件。
- 错误处理明确：禁止吞错；错误信息应可定位问题。
- 日志要有级别与上下文，不输出敏感信息。
- 配置优先于硬编码；常量集中管理。
- 仅在必要处添加注释，注释解释“为什么”，而不是“是什么”。

### 3.3 变更后

- 至少完成以下校验：
  - 能通过编译。
  - 受影响模块的测试通过。
  - 核心路径进行最小可运行验证。
- 更新文档：README、架构文档、模块说明、接口示例（按实际影响）。
- 若存在技术债或后续事项，写入 TODO（带上下文和处理建议）。

---

## 4. 质量门禁（MUST）

以下任一不满足，不得合并：

- 编译失败。
- 测试失败。
- 提交信息不符合 Conventional Commits。
- 代码改动与文档改动不同步。
- 引入明显未使用代码或调试残留。

---

## 5. Git 与提交规范（强制 Conventional Commits）

### 5.1 提交信息格式（必须）

使用以下格式：

`<type>(<scope>)!: <subject>`

或

`<type>: <subject>`

说明：

- `type`：必填。
- `scope`：推荐填写受影响模块，如 `app`、`renderer`、`media`。
- `!`：表示破坏性变更（BREAKING CHANGE）。
- `subject`：一句话描述，使用祈使句，<= 72 字符，末尾不加句号。

### 5.2 允许的 type

- `feat`：新增功能
- `fix`：缺陷修复
- `refactor`：重构（不改变外部行为）
- `perf`：性能优化
- `test`：测试相关
- `docs`：文档变更
- `build`：构建系统或依赖变更
- `ci`：CI/CD 变更
- `chore`：杂项维护（不影响业务逻辑）
- `revert`：回滚提交

### 5.3 提交示例

- `feat(timeline): add clip snapping with frame-accurate threshold`
- `fix(media): avoid panic when decoder returns empty frame`
- `refactor(renderer): split pipeline creation into factory`
- `docs(architecture): update media pipeline cache strategy`
- `feat(app)!: remove legacy preview toggle API`

### 5.4 BREAKING CHANGE 说明（必须）

当提交包含 `!` 或破坏性变更时，提交正文必须包含：

`BREAKING CHANGE: <具体影响与迁移方式>`

### 5.5 提交粒度与分支要求

- 一次提交只做一类逻辑变更（功能/重构/文档不要混杂）。
- 大变更拆分为可审查的小提交。
- 禁止直接在主分支堆叠大量未审查改动。

---

## 6. AI 协作行为准则（强制）

1. 不得伪造“已完成测试”或“已验证”。
2. 不得在未确认的情况下声称“无风险”。
3. 不得改动无关文件；若发现意外脏变更，先暂停并提示。
4. 优先最小改动满足需求，再考虑结构性优化。
5. 输出应包含：改了什么、为什么改、如何验证、潜在风险。
6. 涉及架构或公共接口调整时，必须同步更新架构文档。

---

## 7. 文档同步规范（强制）

代码变更后，按影响范围至少更新以下之一：

- `README.md`（使用方式、命令、配置）
- `docs/architecture/*.md`（模块设计、数据流、边界）
- 对应 crate 的模块说明或接口文档

要求：

- 文档描述必须与当前代码一致。
- 不允许“代码已变更，文档后补”。

---

## 8. 禁止事项

- 禁止提交无法编译代码（除非明确标记为草稿且不合并）。
- 禁止把敏感信息写入代码、日志或提交记录。
- 禁止未评估影响就修改公共 API。
- 禁止为了“通过检查”而删除关键测试。
- 禁止无说明的大规模格式化污染历史。

---

## 9. 推荐工作流

1. 理解需求与影响范围。
2. 设计最小可行改动方案。
3. 实施并本地验证（编译/测试/运行关键路径）。
4. 更新相关文档。
5. 使用 Conventional Commits 提交。
6. 在 PR 描述中记录风险、验证步骤与回滚方案。

---

## 10. 执行口径

本规范中的 `MUST`、`必须`、`强制` 均表示不可豁免要求。
如确需例外，必须在 PR 中明确说明原因、影响范围与补救措施，并经评审同意。

---

## 11. 仓库特定规则（Mondrian）

以下规则来自当前仓库既有文档约定，属于强制执行范围：

### 11.1 Rust 代码质量基线

- 提交前必须通过：`cargo fmt`。
- 提交前必须通过：`cargo clippy --workspace`。
- 公共 API 必须提供文档注释（`///`）。
- 生产代码禁止 `unwrap()`（测试代码可例外，但应尽量可读且可定位失败原因）。
- 错误类型优先使用 `thiserror` 进行结构化定义。

### 11.2 测试与性能校验基线

- 新功能必须附带单元测试。
- 核心算法（如关键帧插值、色彩转换）优先补充 property-based 测试。
- 性能敏感路径建议补充 benchmark（`criterion`）或性能烟雾测试。
- 受影响范围较大时，优先运行：`cargo nextest run --workspace`。

### 11.3 分支与协作约定

- `main` 仅接受 PR 合并，不直接堆叠开发提交。
- 分支命名建议：`feat/*`、`fix/*`、`perf/*`。
- PR 描述需最少包含：改动摘要、验证步骤、潜在风险、回滚思路。

### 11.4 仓库文档对齐要求

涉及以下模块时，必须同步更新对应架构文档：

- `mondrian-media`：`docs/architecture/media-pipeline.md`
- `mondrian-renderer`：`docs/architecture/renderer.md`
- `mondrian-timeline`：`docs/architecture/timeline-system.md`
- `mondrian-ai`：`docs/architecture/ai-workflow.md`
- `mondrian-assets`：`docs/architecture/asset-system.md`
- `mondrian-effects`：`docs/architecture/effects-system.md`
- `mondrian-export`：`docs/architecture/export-system.md`
