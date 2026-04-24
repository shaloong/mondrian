# 运行时函数参考

## 概述

本文列出特效运行时的编译、执行和缓存相关函数。这些函数主要由核心渲染管线调用，插件开发者通常不需要直接调用它们，但理解它们有助于调试和性能优化。

**源码位置：**
- `crates/mondrian-effects/src/execution.rs` —— 执行与缓存
- `crates/mondrian-effects/src/graph.rs` —— 编译与调度

## 执行函数

### apply_effect_render_plan

```rust
pub fn apply_effect_render_plan(
    buffer: &mut Vec<u8>,
    width: u32,
    height: u32,
    plan: &EffectRenderPlan,
    frame_seed: Option<i64>,
) -> Result<()>
```

对帧缓冲区执行渲染计划。在 `buffer` 上做原地像素处理。

**参数：**
- `buffer` —— RGBA 像素数据，长度 = `width * height * 4`
- `width` / `height` —— 帧尺寸
- `plan` —— 要执行的渲染计划
- `frame_seed` —— 帧种子（frame-dependent 特效需要）

### apply_effect_render_plan_pass

```rust
pub fn apply_effect_render_plan_pass(
    input: &[u8],
    output: &mut Vec<u8>,
    width: u32,
    height: u32,
    plan: &EffectRenderPlan,
    frame_seed: Option<i64>,
) -> Result<()>
```

非破坏性版本：从 `input` 读取，写入 `output`。

### apply_effect_render_graph

```rust
pub fn apply_effect_render_graph(
    buffer: &mut Vec<u8>,
    width: u32,
    height: u32,
    graph: &EffectRenderGraph,
    frame_seed: Option<i64>,
) -> Result<()>
```

执行渲染图（未编译），在 `buffer` 上做原地处理。

### apply_effect_render_graph_pass

```rust
pub fn apply_effect_render_graph_pass(
    input: &[u8],
    output: &mut Vec<u8>,
    width: u32,
    height: u32,
    graph: &EffectRenderGraph,
    frame_seed: Option<i64>,
) -> Result<()>
```

非破坏性版本：从 `input` 读取，写入 `output`。

### apply_compiled_effect_graph

```rust
pub fn apply_compiled_effect_graph(
    buffer: &mut Vec<u8>,
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: Option<i64>,
) -> Result<()>
```

执行已编译的渲染图。编译图包含优化后的执行计划和缓存配置，比未编译的图执行效率更高。

### apply_compiled_effect_graph_pass

```rust
pub fn apply_compiled_effect_graph_pass(
    input: &[u8],
    output: &mut Vec<u8>,
    width: u32,
    height: u32,
    compiled: &CompiledEffectGraph,
    frame_seed: Option<i64>,
) -> Result<()>
```

非破坏性版本的编译图执行。

## 编译函数

### compile_effect_render_graph

```rust
pub fn compile_effect_render_graph(plan: &EffectRenderPlan) -> EffectRenderGraph
```

将线性渲染计划转换为渲染图结构。

### compile_scheduled_effect_graph

```rust
pub fn compile_scheduled_effect_graph(plan: &EffectRenderPlan) -> Option<CompiledEffectGraph>
```

将渲染计划编译为完整的 `CompiledEffectGraph`（包含调度、配置文件和缓存策略）。

### schedule_effect_render_graph

```rust
pub fn schedule_effect_render_graph(graph: &EffectRenderGraph) -> Option<EffectExecutionSchedule>
```

为渲染图生成拓扑执行顺序。

### get_or_compile_scheduled_effect_graph

```rust
pub fn get_or_compile_scheduled_effect_graph(
    plan: &EffectRenderPlan,
) -> Option<CompiledEffectGraph>
```

获取缓存的编译图，如果不存在则编译并缓存。

### get_or_compile_scheduled_render_graph

```rust
pub fn get_or_compile_scheduled_render_graph(
    plan: &EffectRenderPlan,
) -> (EffectRenderGraph, Option<CompiledEffectGraph>)
```

获取或编译渲染图及其编译版本。

## 编译图结构

### CompiledEffectGraph

```rust
pub struct CompiledEffectGraph {
    pub graph: EffectRenderGraph,                           // 原始图
    pub schedule: EffectExecutionSchedule,                  // 拓扑执行顺序
    pub node_use_counts: HashMap<EffectGraphNodeId, usize>, // 节点使用计数
    pub node_profiles: HashMap<EffectGraphNodeId, CompiledEffectNodeProfile>, // 节点配置
    pub output_cache_policy: EffectCachePolicy,             // 输出缓存策略
    pub estimated_cost: u32,                                // 估算总成本
    pub output_cache_enabled: bool,                         // 是否启用输出缓存
    pub signature_hash: u64,                                // 图签名
}
```

### CompiledEffectNodeProfile

```rust
pub struct CompiledEffectNodeProfile {
    pub subtree_signature: u64,    // 子树签名（用于缓存 key）
    pub cache_policy: EffectCachePolicy,
    pub estimated_cost: u32,       // 估算成本
    pub output_cache_enabled: bool, // 是否启用此节点的输出缓存
}
```

### EffectExecutionSchedule

```rust
pub struct EffectExecutionSchedule {
    pub ordered_nodes: Vec<EffectGraphNodeId>,  // 拓扑排序后的执行顺序
}
```

## 图分析函数

```rust
/// 计算图中每个节点的使用次数
pub fn effect_graph_node_use_counts(
    graph: &EffectRenderGraph,
) -> HashMap<EffectGraphNodeId, usize>;

/// 计算图的缓存配置（policy, estimated_cost, cache_enabled）
pub fn effect_graph_cache_profile(
    graph: &EffectRenderGraph,
) -> (EffectCachePolicy, u32, bool);

/// 编译每个节点的配置
pub fn compile_effect_node_profiles(
    graph: &EffectRenderGraph,
    node_use_counts: &HashMap<EffectGraphNodeId, usize>,
) -> HashMap<EffectGraphNodeId, CompiledEffectNodeProfile>;
```

## 自定义处理器

### 类型

```rust
pub type CustomEffectRenderProcessor =
    Arc<dyn Fn(&mut Vec<u8>, u32, u32, &serde_json::Value, i64) -> Result<()> + Send + Sync>;
```

参数说明：
- `&mut Vec<u8>` —— RGBA 像素缓冲区（原地修改）
- `u32` —— 宽度
- `u32` —— 高度
- `&serde_json::Value` —— 渲染参数（由 `EffectRenderParamsBuilder` 产生）
- `i64` —— frame seed

### 注册函数

```rust
/// 注册自定义渲染处理器到全局注册表
pub fn register_custom_render_processor(
    key: impl Into<String>,
    processor: CustomEffectRenderProcessor,
);
```

通常不直接调用此函数——通过 `EffectPluginDefinitionBuilder::with_custom_render_backend(...)` 间接调用。

## 缓存结构

内部缓存结构（不直接暴露给插件）：

```rust
struct EffectOutputCacheKey {
    graph_signature: u64,
    input_signature: u64,
    width: u32,
    height: u32,
    frame_seed: Option<i64>,
}

struct EffectNodeOutputCacheKey {
    subtree_signature: u64,
    input_signature: u64,
    width: u32,
    height: u32,
    frame_seed: Option<i64>,
}
```

缓存按图签名、输入签名、尺寸和 frame seed 建立 key。frame-dependent 的缓存 key 包含 frame seed，deterministic 的不包含。

## 调用层次

从高层到底层的执行流程：

```
build_effect_render_graph()        ← 从 EffectNode[] 构建图
    │
    ▼
get_or_compile_scheduled_render_graph()  ← 编译 + 缓存
    │
    ▼
apply_compiled_effect_graph_pass() ← 执行编译图
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
