# 运行时函数参考

## 概述

本文列出特效运行时公开的编译、执行和缓存 Interface。生产执行只接受不可变
`CompiledEffectGraph`；`EffectRenderGraph` 是 Definition/插件构图阶段的编译器
输入，不存在供跨 crate 调用的 raw graph 调度或执行入口。这样图结构、拓扑调度、
颜色域、执行合同、资源生命周期与缓存证据不能被拆开或错配。

这些函数主要由核心渲染管线调用，插件开发者通常只需通过 Definition Builder 和
`EffectGraphDsl` 声明语义。

**源码位置：**
- `crates/mondrian-effects/src/execution.rs` —— 执行与缓存
- `crates/mondrian-effects/src/graph.rs` —— 编译与调度

## 执行函数

### apply_compiled_effect_graph

```rust
pub fn apply_compiled_effect_graph(
    input: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: i64,
) -> Result<Vec<u8>, EffectExecutionError>
```

执行不可变编译图。入口先准入精确 CPU/NormalizedU8 单帧模式并验证颜色域计划，随后
使用编译图自身的调度、liveness 和缓存合同。这个 free function 每次调用都会创建一个
fresh、uncached reference Session；它适用于测试、标量参照和一次性调用，不是生产逐帧
循环。生产调用方必须保留自己的 `EffectExecutionSession` 并调用
`apply_compiled_rgba8(...)`。

### apply_compiled_effect_graph_pass

```rust
pub fn apply_compiled_effect_graph_pass(
    base: &[u8],
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    opacity: f32,
    blend_mode: Option<BlendMode>,
    frame_seed: i64,
    out: &mut Vec<u8>,
) -> Result<(), EffectExecutionError>
```

执行不可变编译图并把结果合回 `base`。与上一个 free function 一样，它内部创建 fresh
uncached reference Session；生产调整层循环使用 owner-retained
`EffectExecutionSession::apply_compiled_pass_rgba8(...)`。

## 编译函数

### compile_reference_effect_graph

```rust
#[doc(hidden)]
pub fn compile_reference_effect_graph(
    plan: &EffectRenderPlan,
) -> Option<Arc<CompiledEffectGraph>>
```

将线性计划编译为一个不进入任何全局驻留的不可变参考图。对应的
`compile_reference_render_graph(...)` 与
`compile_reference_effect_graph_in_domain(...)` 也只为跨 crate
标量/集成测试和基准提供。

这是标量参考/测试 Interface，而不是生产逐帧入口。生产 Preview 与 Export
通过 `PreparedEffectProgram` 绑定 Definition、资源、零时刻拓扑和不可变编译图；
动态拓扑变体由调用者自己的 `EffectExecutionSession` 限额保留。除规范
source-only identity 图外，不存在进程级 `CompiledEffectGraph` 缓存。
这些参考构造器从生成的普通公开文档中隐藏；插件和生产调用方不得用它们绕过
Prepared Program 的 Definition、资源与执行合同绑定。

## 编译图结构

### CompiledEffectGraph

```rust
pub struct CompiledEffectGraph {
    // 所有字段私有；只能由 graph 编译器构造。
}

impl CompiledEffectGraph {
    pub const fn graph(&self) -> &EffectRenderGraph;
    pub const fn semantic_fingerprint(&self) -> [u8; 32];
    pub const fn node_use_counts(&self)
        -> &HashMap<EffectGraphNodeId, usize>;
    pub const fn node_profiles(&self)
        -> &HashMap<EffectGraphNodeId, CompiledEffectNodeProfile>;
    pub const fn output_cache_policy(&self) -> EffectCachePolicy;
    pub const fn estimated_cost(&self) -> u32;
    pub const fn output_cache_enabled(&self) -> bool;
    pub const fn signature_hash(&self) -> u64;
    pub const fn domain_plan(&self) -> &CompiledEffectDomainPlan;
    pub const fn execution_envelope(&self) -> &EffectExecutionEnvelope;

    pub fn plan_execution_demand(
        &self,
        output_time: TimelineTime,
        frame_extent: EffectFrameExtent,
        output_roi: EffectPixelRoi,
    ) -> Result<EffectExecutionDemand, EffectExecutionDemandError>;

    pub fn plan_linear_stage_placement(
        &self,
        environment: &EffectExecutionEnvironment,
    ) -> Result<EffectLinearStagePlacement, EffectLinearStagePlacementError>;
}
```

`CompiledEffectGraph` 是唯一生产编译 IR。其图、调度、use-count、节点配置、缓存合同、
颜色域计划和执行 Envelope 必须作为一个整体保持一致，因此调用方只能读取，不能通过
公开字段构造或改写其中任意部分。Effects 内部缓存相等性保留并比较完整 canonical
`EffectGraphIdentity` 字节；`semantic_fingerprint()` 是给跨 Module typed key 使用的
完整 32 字节强身份。`signature_hash()` 和节点 `subtree_signature` 都只是紧凑诊断，
不能证明图或子树相等，也不能授权缓存命中。

### CompiledEffectNodeProfile

```rust
pub struct CompiledEffectNodeProfile {
    pub subtree_signature: u64,    // 紧凑子树诊断值，不是缓存相等性身份
    pub cache_policy: EffectCachePolicy,
    pub estimated_cost: u32,       // 估算成本
    pub output_cache_enabled: bool, // 是否启用此节点的输出缓存
}
```

## EffectExecutionSession

`EffectExecutionSession` 是一个 Preview Runtime 或一个 Export attempt 独占的可变执行
状态。Prepared Program 与 `CompiledEffectGraph` 始终不可变；像素输出、Temporal
输出、动态拓扑和 GPU lowering plan 的驻留只能放在 Session 内。

```rust
pub struct EffectExecutionSessionConfig {
    pub max_cache_entries: usize,
    pub max_cache_bytes: usize,
    pub max_working_bytes: usize,
    pub max_gpu_plan_entries: usize,
    pub max_gpu_plan_bytes: usize,
}

impl EffectExecutionSession {
    pub fn new(config: EffectExecutionSessionConfig) -> Self;
    pub fn reconfigure(&mut self, config: EffectExecutionSessionConfig);
    pub fn bind_generation(&mut self, generation: u64);
    pub fn clear(&mut self);
    pub fn diagnostics(&self) -> EffectExecutionSessionDiagnostics;

    pub fn apply_compiled_rgba8(
        &mut self,
        input: &[u8],
        width: u32,
        height: u32,
        compiled: &CompiledEffectGraph,
        frame_seed: i64,
    ) -> Result<Vec<u8>, EffectExecutionError>;
    pub fn apply_compiled_pass_rgba8(
        &mut self,
        base: &[u8],
        width: u32,
        height: u32,
        compiled: &CompiledEffectGraph,
        opacity: f32,
        blend_mode: Option<BlendMode>,
        frame_seed: i64,
        out: &mut Vec<u8>,
    ) -> Result<(), EffectExecutionError>;
    pub fn get_or_lower_gpu_plan(
        &mut self,
        compiled: &CompiledEffectGraph,
    ) -> Result<Arc<CompiledEffectGpuPlan>, EffectGpuPlanBlocker>;
}
```

主 cache grant 被确定性地分区给 encoded whole-graph、encoded node、Float32、
Temporal 和动态图拓扑驻留，所有分区合计不超过调用方的 entry/byte 上限。GPU plan
和确定性 blocker 使用独立的小 grant；`max_working_bytes` 只准入一次
Temporal/ROI 执行的瞬时工作集。每个分区按 owner-local LRU 淘汰，
`reconfigure(...)` 同步收缩，过大的单项可完成当前调用但不会驻留。

`bind_generation(...)` 是强生命周期屏障：首次绑定建立当前 generation，换到不同
generation 时同步清空所有像素、Temporal、动态图拓扑、GPU plan 和 blocker。Preview
可以在一个当前 generation 内跨帧复用；Export 在一个已准入 attempt 的全部帧和嵌套
求值间复用，并在 attempt terminal return/unwind 时销毁整个 Session。不存在跨
Preview/Export、跨 Export attempt、跨 Project/Open lifetime 或基于 TTL 的 Effect
缓存。调用方也不得逐帧创建生产 Export Session。

## 自定义处理器

### 类型

```rust
pub type CustomEffectRenderProcessor =
    Arc<
        dyn Fn(
                &mut Vec<u8>,
                u32,
                u32,
                &serde_json::Value,
                i64,
            ) -> mondrian_core::Result<()>
            + Send
            + Sync,
    >;
```

参数说明：
- `&mut Vec<u8>` —— RGBA 像素缓冲区（原地修改）
- `u32` —— 宽度
- `u32` —— 高度
- `&serde_json::Value` —— 渲染参数（由 `EffectRenderParamsBuilder` 产生）
- `i64` —— frame seed

### 绑定边界

`EffectPluginDefinitionBuilder::with_custom_render_backend(...)` 是自定义处理器的唯一绑定
入口。Definition 求值时会把处理器及其进程内实现修订直接嵌入图节点，之后由编译图持有
该不可变绑定。不存在按字符串注册或补绑处理器的全局 Registry；任何可达且未绑定的 raw
Custom 节点都必须在编译时失败关闭。这样，同一个作者/Program 修订不会因进程内环境变化
而获得不同实现，逐帧执行也无需全局查找。

## 缓存结构

内部缓存结构（不直接暴露给插件）：

```rust
struct EffectOutputCacheKey {
    graph_identity: EffectGraphIdentity,
    input_fingerprint: [u8; 32],
    width: u32,
    height: u32,
    frame_seed: Option<i64>,
}

struct EffectNodeOutputCacheKey {
    graph_identity: EffectGraphIdentity,
    node_id: EffectGraphNodeId,
    input_fingerprint: [u8; 32],
    width: u32,
    height: u32,
    frame_seed: Option<i64>,
}
```

缓存相等性使用完整 canonical 图身份、完整输入内容 SHA-256、尺寸以及策略要求的 frame
seed；节点缓存还携带精确 Node ID。它不会只凭 subtree signature 在不同图之间复用。
Float 输出缓存另外携带完整的颜色域处理器身份。Frame-dependent 缓存包含 frame
seed，deterministic 缓存不包含，uncacheable 输出绕过跨调用缓存。所有这些 key 只在
当前 `EffectExecutionSession` 的绑定 generation 和 LRU grant 内有驻留意义。

## 调用层次

从高层到底层的执行流程：

```
PreparedEffectProgram::prepare()  ← 绑定 Definition、资源与零时刻拓扑
    │
    ▼
evaluate_with_session()           ← 绑定当帧值；动态拓扑归调用方 Session
    │
    ▼
EffectExecutionSession::apply_compiled_*() ← 生产执行编译图
    │
    ├── 检查 whole-graph output cache
    ├── 按拓扑顺序遍历节点
    │   ├── 检查 node/subtree output cache
    │   ├── 调用 apply_render_op() 或 custom processor
    │   └── 更新 node/subtree cache
    └── 更新 whole-graph cache → 输出
```

## 相关

- [EffectDefinition API](./effect-definition.md) —— 特效定义
- [EffectGraphDsl API](./effect-graph-dsl.md) —— 构图 API
- [Plugin Contract API](./plugin-contract.md) —— 版本与容错
