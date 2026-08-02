# EffectGraphDsl API 参考

## 概述

`EffectGraphDsl` 是插件作者构建特效渲染图的高层 API。它封装了 `EffectGraphBuilderState`，提供语义清晰的构图方法。

**源码位置：**
- `crates/mondrian-effects/src/plugin_sdk.rs` —— `EffectGraphDsl`
- `crates/mondrian-effects/src/graph.rs` —— `EffectGraphBuilderState`、`EffectRenderGraph`、`EffectGraphNodeKind`

## EffectGraphDsl

通过 `EffectPluginDefinitionBuilder::with_graph(...)` 或 `with_branching_graph(...)` 的闭包获取。

```rust
pub struct EffectGraphDsl<'a> {
    // 内部持有 &'a mut EffectGraphBuilderState
}
```

### 方法一览

| 方法 | 签名 | 说明 |
|------|------|------|
| `source()` | `fn source(&self) -> EffectGraphValue` | 获取图的原始输入 |
| `current()` | `fn current(&self) -> EffectGraphValue` | 获取当前输出节点 |
| `set_output(v)` | `fn set_output(&mut self, value: EffectGraphValue)` | 设置当前输出 |
| `apply(op)` | `fn apply(&mut self, op: EffectRenderOp) -> EffectGraphValue` | 对当前输出应用一元操作 |
| `apply_to(input, op)` | `fn apply_to(&mut self, input: EffectGraphValue, op: EffectRenderOp) -> EffectGraphValue` | 对指定输入应用一元操作 |
| `branch(input, f)` | `fn branch<F>(&mut self, input: EffectGraphValue, build: F) -> EffectGraphValue` | 在 input 上构建子图 |
| `blend(base, mode, opacity, f)` | `fn blend<F>(&mut self, base: EffectGraphValue, blend_mode: BlendMode, opacity: f32, build_overlay: F) -> EffectGraphValue` | 混合 base 和 overlay |
| `blend_current(mode, opacity, f)` | `fn blend_current<F>(&mut self, blend_mode: BlendMode, opacity: f32, build_overlay: F) -> EffectGraphValue` | 从当前输出分支并混合回来 |
| `mask(input, invert, op, f)` | `fn mask<F>(&mut self, input: EffectGraphValue, invert: bool, mask_op: MaskOp, build_mask: F) -> EffectGraphValue` | 用 alpha 子图和显式布尔操作遮蔽 input |
| `mask_current(invert, op, f)` | `fn mask_current<F>(&mut self, invert: bool, mask_op: MaskOp, build_mask: F) -> EffectGraphValue` | 用 alpha 子图和显式布尔操作遮蔽当前输出 |

`EffectGraphValue` 是 `EffectGraphNodeId` 的类型别名（`pub type EffectGraphValue = EffectGraphNodeId;`）。
`branch`、`blend`、`blend_current`、`mask` 与 `mask_current` 的构图闭包都必须把
`EffectGraphValue` 作为末尾表达式返回；在最后一次 `apply_to(...)` 后写分号会把返回值
变成 `()` 并导致编译失败。

### apply

```rust
pub fn apply(&mut self, op: EffectRenderOp) -> EffectGraphValue
```

对当前输出应用一元操作。效果等价于：`output = op(current_output)`，然后将 `output` 设为新的当前输出。

返回新节点的 ID。

```rust
// 示例：线性链
graph.apply(EffectRenderOp::GaussianBlur { radius: 4.0 });
graph.apply(EffectRenderOp::Sharpen { amount: 0.5 });
```

### apply_to

```rust
pub fn apply_to(&mut self, input: EffectGraphValue, op: EffectRenderOp) -> EffectGraphValue
```

对指定输入应用一元操作，**不改变当前输出**。

```rust
// 示例：在分支上应用操作
graph.apply_to(source, EffectRenderOp::GaussianBlur { radius: 8.0 });
```

### blend_current

```rust
pub fn blend_current<F>(
    &mut self,
    blend_mode: BlendMode,
    opacity: f32,
    build_overlay: F,
) -> EffectGraphValue
where
    F: for<'b> FnOnce(&mut EffectGraphDsl<'b>, EffectGraphValue) -> EffectGraphValue
```

从当前输出分出一条支路，在闭包中构建 overlay，然后以指定的 blend mode 和 opacity 混合回来。

闭包接收 `(dsl, source)` —— `source` 是当前输出的副本（作为 overlay 支路的输入）。

这是 glow/bloom/soft focus 类特效的核心方法。

```rust
graph.blend_current(BlendMode::Screen, 0.4, |graph, source| {
    graph.apply_to(source, EffectRenderOp::GaussianBlur { radius: 12.0 })
});
```

### blend

```rust
pub fn blend<F>(
    &mut self,
    base: EffectGraphValue,
    blend_mode: BlendMode,
    opacity: f32,
    build_overlay: F,
) -> EffectGraphValue
where
    F: for<'b> FnOnce(&mut EffectGraphDsl<'b>, EffectGraphValue) -> EffectGraphValue
```

`blend_current` 的通用版本，显式指定 base 输入。

### mask_current

```rust
pub fn mask_current<F>(
    &mut self,
    invert: bool,
    mask_op: MaskOp,
    build_mask: F,
) -> EffectGraphValue
where
    F: for<'b> FnOnce(&mut EffectGraphDsl<'b>, EffectGraphValue) -> EffectGraphValue
```

用闭包构建的 alpha 蒙版遮蔽当前输出。`invert = true` 表示先反转 matte；
`mask_op` 必须显式选择 `MaskOp::{Add, Subtract, Intersect, Difference}`。闭包最后一个
表达式必须返回 `EffectGraphValue`，不能以分号丢弃为 `()`。

传入的 mask 分支必须输出 `EffectColorDomain::AlphaMask`。当前高层插件 DSL 尚未公开
创建 `MaskSource` 或把 RGB 转成 matte 的安全入口，因此插件不能用 raw Custom 节点
伪造 mask producer；这种图会因未绑定 processor 或颜色域不合法而失败关闭。Clip
作者蒙版由引擎在编译阶段注入。

### mask

```rust
pub fn mask<F>(
    &mut self,
    input: EffectGraphValue,
    invert: bool,
    mask_op: MaskOp,
    build_mask: F,
) -> EffectGraphValue
where
    F: for<'b> FnOnce(&mut EffectGraphDsl<'b>, EffectGraphValue) -> EffectGraphValue
```

`mask_current` 的通用版本，显式指定要遮蔽的输入。

### branch

```rust
pub fn branch<F>(
    &mut self,
    input: EffectGraphValue,
    build: F,
) -> EffectGraphValue
where
    F: for<'b> FnOnce(&mut EffectGraphDsl<'b>, EffectGraphValue) -> EffectGraphValue
```

在指定 input 上构建独立子图，返回子图的最终输出。不修改当前输出。

通常用于构建需要独立处理的支路，然后手动 blend 或 mask。

### source / current / set_output

```rust
pub fn source(&self) -> EffectGraphValue;
pub fn current(&self) -> EffectGraphValue;
pub fn set_output(&mut self, value: EffectGraphValue);
```

- `source()` —— 始终返回图的原始输入节点（`EffectGraphNodeId(0)`）
- `current()` —— 返回当前的输出节点（初始为 source）
- `set_output(v)` —— 手动设置当前输出节点

大多数情况下不需要手动 `set_output` —— `apply` 和 `blend_current` 会自动更新当前输出。

## EffectRenderGraph

构图完成后产生的静态图结构：

```rust
pub struct EffectRenderGraph {
    pub nodes: Vec<EffectGraphNode>,
    pub output: Option<EffectGraphNodeId>,
}
```

`EffectRenderGraph` 只承载 Definition/插件构图结果，是编译器输入，不是可执行的
生产 IR。插件不得自行生成拓扑调度或直接执行该结构；运行时必须先通过
Mondrian 的编译入口得到不可变 `CompiledEffectGraph`，确保拓扑、颜色域、
执行合同、资源生命周期与缓存证据始终作为一个整体。

### 方法

```rust
impl EffectRenderGraph {
    /// 创建 identity 图（source → output，无任何处理）
    pub fn identity() -> Self;

    /// 是否为 identity 图
    pub fn is_identity(&self) -> bool;

    /// 按 ID 查找节点
    pub fn node(&self, id: EffectGraphNodeId) -> Option<&EffectGraphNode>;

    /// 计算紧凑诊断哈希；不作为图相等性或缓存复用证明
    pub fn signature_hash(&self) -> u64;
}
```

## EffectGraphNode

```rust
pub struct EffectGraphNode {
    pub id: EffectGraphNodeId,
    pub kind: EffectGraphNodeKind,
}
```

### EffectGraphNodeKind

```rust
pub enum EffectGraphNodeKind {
    Source,
    UnaryEffect { input: EffectGraphNodeId, op: EffectRenderOp },
    DomainEffect {
        input: EffectGraphNodeId,
        op: EffectRenderOp,
        domain_contract: EffectColorDomainContract,
    },
    Blend { base: EffectGraphNodeId, overlay: EffectGraphNodeId, blend_mode: BlendMode, opacity: f32 },
    Mask {
        input: EffectGraphNodeId,
        mask: EffectGraphNodeId,
        invert: bool,
        mask_op: MaskOp,
    },
    MaskSource {
        shape: MaskShape,
        feather: f32,
        expansion: f32,
        opacity: f32,
    },
    MultiInput {
        inputs: Vec<EffectGraphNodeId>,
        blend_mode: BlendMode,
        opacity: f32,
    },
}
```

### EffectGraphNodeId

```rust
pub struct EffectGraphNodeId(pub u32);
```

图节点 ID，`EffectGraphValue` 是其类型别名。

## EffectPluginDefinitionBuilder（SDK 入口）

```rust
pub struct EffectPluginDefinitionBuilder {
    // 内部持有 EffectDefinition
}
```

### 方法

```rust
impl EffectPluginDefinitionBuilder {
    /// 创建新的 builder，并声明精确处理域
    pub fn new(
        key: impl Into<String>,
        display_name: impl Into<String>,
        color_domain_contract: EffectColorDomainContract,
    ) -> Self;

    /// 添加单个属性
    pub fn property(mut self, descriptor: PropertyDescriptor) -> Self;

    /// 批量添加属性
    pub fn properties(mut self, properties: PropertyBag) -> Self;

    /// 声明完整执行合同；默认插件合同不会准入任何 backend
    pub fn with_execution_contract(
        mut self,
        contract: EffectExecutionContract,
    ) -> Self;

    /// 设置线性图构建器
    pub fn with_graph<F>(mut self, build: F) -> Self
    where F: for<'a> Fn(&EffectNode, EffectEvalContext, &mut EffectGraphDsl<'a>) + Send + Sync + 'static;

    /// 设置可返回结构化构图失败的线性图构建器
    pub fn try_with_graph<F>(mut self, build: F) -> Self
    where F: for<'a> Fn(
        &EffectNode,
        EffectEvalContext,
        &mut EffectGraphDsl<'a>,
    ) -> Result<(), EffectGraphBuildError> + Send + Sync + 'static;

    /// 设置分支图构建器
    pub fn with_branching_graph<F>(mut self, build: F) -> Self
    where F: for<'a> Fn(&EffectNode, EffectEvalContext, &mut EffectGraphDsl<'a>) + Send + Sync + 'static;

    /// 设置可返回结构化构图失败的分支图构建器
    pub fn try_with_branching_graph<F>(mut self, build: F) -> Self
    where F: for<'a> Fn(
        &EffectNode,
        EffectEvalContext,
        &mut EffectGraphDsl<'a>,
    ) -> Result<(), EffectGraphBuildError> + Send + Sync + 'static;

    /// 设置自定义渲染后端
    pub fn with_custom_render_backend(
        mut self,
        params_builder: EffectRenderParamsBuilder,
        cache_key_builder: Option<EffectCacheKeyBuilder>,
        cache_policy: EffectCachePolicy,
        processor: CustomEffectRenderProcessor,
    ) -> Self;

    /// 设置插件契约
    pub fn with_plugin_contract(mut self, contract: EffectPluginContract) -> Self;

    /// 构建 EffectDefinition
    pub fn build(self) -> EffectDefinition;
}
```

`EffectPluginDefinitionBuilder` 当前没有作者资源路径到不可变
prepared-resource 的高层 SDK。逐帧 graph/cache-key/processor 闭包不得因此执行文件或
网络 I/O。需要作者可选择外部 LUT、模型或其他资源的插件，在真正的版本化
preparation/revalidation Interface 暴露前必须失败关闭；不能用 path-only cache key 或
每帧重读文件伪装成资源绑定。

## 相关

- [EffectDefinition API](./effect-definition.md) —— 特效定义与节点
- [Plugin Contract API](./plugin-contract.md) —— 版本与失败策略
- [运行时函数](./runtime.md) —— 图的编译与执行
