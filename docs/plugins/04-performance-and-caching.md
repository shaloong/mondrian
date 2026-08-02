# 性能与缓存

本文介绍 Mondrian 特效运行时的缓存模型，以及插件开发者如何利用缓存让特效在预览和导出中都保持高效。

## 1. 所有权与缓存层次

生产执行中的可变复用状态全部属于一个 `EffectExecutionSession`。一个 Preview
Runtime 和每个 Export attempt 各自拥有不同 Session；它们不共享像素、动态拓扑或 GPU
lowering plan 驻留。下面两层是该 Session 内的像素输出缓存，不是进程级缓存。

### 1.1 全图输出缓存（Whole-Graph Output Cache）

对整个 `CompiledEffectGraph` 的最终输出做缓存。

适用条件：
- 完全相同的完整 canonical `EffectGraphIdentity`
- 完全相同的输入帧
- 完全相同的输出尺寸与颜色域处理器身份（Float 路径）
- cache policy 要求时，完全相同的 frame seed

缓存 key 构成：
- 完整 canonical 图身份（不是 `u64` graph signature）
- 完整输入内容 SHA-256
- 输出宽高
- frame seed（仅 frame-dependent 特效）
- Float 路径的完整颜色域处理器身份

### 1.2 子树/节点输出缓存（Subtree / Node Output Cache）

对当前完整编译图中被明确标记为值得复用的昂贵 **deterministic** 或
**frame-dependent** 节点输出做缓存。

适用条件：
- 同一个完整编译图与精确 Node ID 再次求值
- 子树成本足够高，缓存收益大于哈希开销
- 节点 policy 允许跨调用复用

缓存 key 构成：
- 完整 canonical 图身份和精确 Node ID
- 完整输入内容 SHA-256
- 输出宽高
- frame seed（仅 frame-dependent）

`signature_hash()` 和 `subtree_signature` 都只是紧凑诊断值，不参与相等性判定。
当前节点缓存不会仅凭相同 subtree signature 在不同图之间复用；这样可以避免不同
Definition 绑定、颜色域合同、Custom processor 实现或图上下文被错误合并。

## 2. EffectCachePolicy

### Deterministic

相同输入、相同参数、相同外部依赖 → **输出完全一致**。

适合：
- 模糊（blur）
- 锐化（sharpen）
- 暗角（vignette）
- 3D LUT
- 确定性自定义滤波器

### FrameDependent

输出显式依赖 frame seed 或时间采样噪声。

适合：
- 噪点（grain）
- 时域噪声（temporal noise）
- 抖动、闪烁、随机化效果

**如果把 frame-dependent 效果误报为 deterministic，会导致错误的帧复用。**

### Uncacheable

无法承诺相同输入与显式参数在两次调用间产生相同输出。运行时不会为它构造可跨调用
复用的输出 key，也不会复用 Viewer Current/Pending 或最终输出缓存。若随机性可以由
显式 `frame_seed` 完整决定，应使用 `FrameDependent`；只有无法被稳定 seed 捕获的
行为才使用 `Uncacheable`。

## 3. cache_key 设计

`cache_key` 用于表达："这个自定义效果的结果依赖什么稳定外部资源"。

### 适合放入 cache_key 的内容

- 已绑定 LUT/模型内容的 canonical hash
- 不可变外部资源的稳定 ID 加精确 revision
- 预计算 kernel/config 的内容版本
- 随插件注册并预加载资源的版本化 manifest identity

### 不适合放入 cache_key 的内容

- 每帧变化的临时值
- 未标准化的调试字符串
- 带随机部分的值
- 用户本地路径或 URL 本身（位置不是内容 revision）
- 在 cache-key builder 中临时读取、探测或哈希的文件

### 示例

```rust
// 好的 cache_key：资源已经在执行路径之外绑定并验证
let prepared_lut_revision = "lut:sha256:4a...";
Some(Arc::new(move |_effect, _context| {
    Some(prepared_lut_revision.to_string())
}))

// 不好的 cache_key：位置不是内容身份
let author_path = std::path::PathBuf::from("look.cube");
Some(Arc::new(move |_effect, _context| {
    Some(format!("lut:path:{}", author_path.display()))
}))
```

参数构建器、cache-key 构建器和 pixel processor 都不得执行文件或网络 I/O。低层
`EffectGraphPreparer` 可在 Program preparation 时通过
`EffectPreparationContext::prepare_cube_lut(...)` 使用调用方拥有的 bounded cache；
它必须捕获返回的不可变 `Arc`、声明依赖，并用
`with_retained_resource_bytes(...)` 报告保守 logical charge。Preview 与每个 Export
attempt 的 cache 相互独立，每次低频 prepare 都会完整读取并哈希文件后才接受命中。
当前高层插件 SDK 尚未提供除 `.cube` 之外、作者可选外部资源的通用
preparation/revalidation Interface；在该边界落地前，这类实例必须结构化报告资源不可用，
不能用 process-global cache、path-only key、mtime/size 或每帧重读来绕过资源绑定。

## 4. 缓存启用策略

运行时**不会对所有节点一律缓存**。只有满足以下条件的子树才会启用节点缓存：

- 计算成本足够高，值得缓存
- 或者中等成本节点在同一图中有 fan-out/输出复用价值
- 且 cache policy 已明确声明

这个策略避免了：
- 哈希计算成本超过渲染成本
- 小图/轻量特效过度缓存
- 内存增长失控

### 4.1 Session 预算与 LRU

`EffectExecutionSessionConfig` 的主 entry/byte grant 会被确定性地分配给 RGBA8
全图、RGBA8 节点、Float32 全图、Temporal 输出和动态图拓扑。所有分区之和不超过
调用方给出的总预算，不会把同一预算按表示形式重复计算。GPU lowering plan 与确定性
blocker 使用一组独立、较小的 entry/byte grant；瞬时 Temporal/ROI 工作集另有
`max_working_bytes` 准入上限。

每个驻留分区按 owner-local LRU 淘汰，并同时受 entry 与保守 logical-byte 上限约束。
`reconfigure(...)` 会同步收缩并淘汰超额项；单个超过分区预算的结果仍可返回给本次调用，
但不会驻留。这里没有 TTL，也没有隐藏的进程级兜底缓存。

### 4.2 Generation 与 attempt 生命周期

调用方必须在生产执行前用 `bind_generation(...)` 绑定其权威调度 generation。同一
generation 内可以复用；generation 改变时，Session 同步清空所有像素表示、Temporal
输出、动态拓扑、GPU plan 和 blocker。取消或 supersede 的旧 generation 因此不能把
缓存结果带入新 generation。

Preview Session 随 Preview Runtime 存活，但只在当前绑定 generation 内复用；Project
重开、Authoring Session 切换或调度失效会旋转 generation/owner scope。Export 为每个
已准入 attempt 创建一个独占 Session，在该 attempt 的全部帧、嵌套 Sequence 和
Transition endpoint 之间复用，并在 terminal return 或 unwind 时整体销毁。禁止逐帧
创建 Export Session，也禁止 Preview 与 Export 共享 Session。

## 5. 对你的插件的建议

### 5.1 新插件的默认选择

| 特效特征 | 推荐 Policy | 推荐提供 cache_key |
|----------|-------------|-------------------|
| 纯内置算子组合，无外部依赖 | `Deterministic` | 不需要 |
| 使用已准备且不可变的 LUT、模型 | `Deterministic` | 必须包含精确内容/revision 身份 |
| 包含随机/噪声/时间扰动 | `FrameDependent` | 视情况 |
| 自定义像素处理器 | 视算法而定 | 视情况 |

### 5.2 图组合与 Custom 的边界

能由内置算子表达的稳定步骤，应直接用 Graph DSL 分成节点，运行时才能按真实拓扑决定
复用。自定义处理器只能通过
`EffectPluginDefinitionBuilder::with_custom_render_backend(...)` 绑定；插件不得直接构造
raw `EffectRenderOp::Custom`，也不得令其 processor 为空。当前高层 builder 不支持在同一
Definition 中任意穿插多个 Custom processor 与内置节点，因此不要用未绑定 IR 绕过该
边界；需要这种组合能力时，应先扩展类型化 SDK 和编译合同。

### 5.3 核心原则

- 只要特效依赖外部资源，尽量提供稳定 `cache_key`
- 如果特效依赖 frame seed、随机噪声、时间扰动，明确标为 `FrameDependent`
- 不要为了"看起来高级"随意声明 cache key —— 错误 key 比没有 key 更危险
- 错误声明 deterministic 会导致画面错误；这比性能差更糟糕
- 不要在插件中维护第二套跨帧/跨 attempt 像素缓存；让调用方拥有的
  `EffectExecutionSession` 执行预算、LRU 和 generation 隔离

## 6. 预览 vs 导出

预览和导出共享同一套缓存语义，但可以有不同的调度策略：

- **预览**：可以使用较低的求值分辨率和较小的 owner-local 预算；generation 失效会
  立即清空旧驻留
- **导出**：按交付分辨率执行，在一个 attempt 的独占 Session 内顺序复用；缓存绝不
  跨 attempt 或跨 job 存活

插件不需要——也不应该——在代码中区分 preview 和 export。运行时负责调度、预算和
Session 生命周期；两者仍使用完整相同的 graph identity、cache policy 和输出相等性
规则。

## 下一步

- [版本契约](./05-versioning-and-compatibility.md) —— 管理版本与兼容性
- [最佳实践](./06-best-practices.md) —— 更多设计建议
