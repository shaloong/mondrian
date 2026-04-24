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
| `mask(input, invert, f)` | `fn mask<F>(&mut self, input: EffectGraphValue, invert: bool, build_mask: F) -> EffectGraphValue` | 用子图为 input 生成蒙版 |
| `mask_current(invert, f)` | `fn mask_current<F>(&mut self, invert: bool, build_mask: F) -> EffectGraphValue` | 用子图为当前输出生成蒙版 |

`EffectGraphValue` 是 `EffectGraphNodeId` 的类型别名（`pub type EffectGraphValue = EffectGraphNodeId;`）。

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
    graph.apply_to(source, EffectRenderOp::GaussianBlur { radius: 12.0 });
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
    build_mask: F,
) -> EffectGraphValue
where
    F: for<'b> FnOnce(&mut EffectGraphDsl<'b>, EffectGraphValue) -> EffectGraphValue
```

用闭包构建的 alpha 蒙版遮蔽当前输出。`invert = true` 表示反转蒙版。

```rust
graph.mask_current(false, |graph, source| {
    graph.apply_to(source, EffectRenderOp::Custom {
        key: "plugin.example.vignette_mask".to_string(),
        params: serde_json::json!({"shape": "ellipse", "feather": 0.3}),
        cache_key: Some("vignette-mask-v1".to_string()),
        cache_policy: EffectCachePolicy::Deterministic,
    });
});
```

### mask

```rust
pub fn mask<F>(
    &mut self,
    input: EffectGraphValue,
    invert: bool,
    build_mask: F,
) -> EffectGraphValue
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

### 方法

```rust
impl EffectRenderGraph {
    /// 创建 identity 图（source → output，无任何处理）
    pub fn identity() -> Self;

    /// 是否为 identity 图
    pub fn is_identity(&self) -> bool;

    /// 按 ID 查找节点
    pub fn node(&self, id: EffectGraphNodeId) -> Option<&EffectGraphNode>;

    /// 计算图签名哈希（用于缓存 key）
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
    Blend { base: EffectGraphNodeId, overlay: EffectGraphNodeId, blend_mode: BlendMode, opacity: f32 },
    Mask { input: EffectGraphNodeId, mask: EffectGraphNodeId, invert: bool },
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
    /// 创建新的 builder
    pub fn new(key: impl Into<String>, display_name: impl Into<String>) -> Self;

    /// 添加单个属性
    pub fn property(mut self, descriptor: PropertyDescriptor) -> Self;

    /// 批量添加属性
    pub fn properties(mut self, properties: PropertyBag) -> Self;

    /// 设置旧式求值器
    pub fn with_evaluator(mut self, evaluator: EffectEvaluator) -> Self;

    /// 设置线性图构建器
    pub fn with_graph<F>(mut self, build: F) -> Self
    where F: for<'a> Fn(&EffectNode, EffectEvalContext, &mut EffectGraphDsl<'a>) + Send + Sync + 'static;

    /// 设置分支图构建器
    pub fn with_branching_graph<F>(mut self, build: F) -> Self
    where F: for<'a> Fn(&EffectNode, EffectEvalContext, &mut EffectGraphDsl<'a>) + Send + Sync + 'static;

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

## 相关

- [EffectDefinition API](./effect-definition.md) —— 特效定义与节点
- [Plugin Contract API](./plugin-contract.md) —— 版本与失败策略
- [运行时函数](./runtime.md) —— 图的编译与执行
