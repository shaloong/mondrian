# EffectDefinition API 参考

## 概述

`EffectDefinition` 是特效的静态描述——它定义了特效的身份、参数、能力和渲染行为。每个特效（内置或插件）都由一个 `EffectDefinition` 描述。

**源码位置：** `crates/mondrian-effects/src/effect.rs`

## EffectType

```rust
pub enum EffectType {
    // 内置类型
    BasicCorrection,
    WhiteBalance,
    Lut3D,
    ColorWheel,
    Curves,
    HueSaturationLightness,
    GaussianBlur,
    Sharpen,
    Vignette,
    ChromaticAberration,
    Grain,
    ChromaKey,
    LumaKey,

    // 插件类型 —— plugin.<author>.<name>
    Plugin(String),
}
```

插件特效统一使用 `EffectType::Plugin(key)`，其中 key 遵循 `plugin.<author>.<name>` 格式。

`Plugin` 变体的辅助方法：

```rust
impl EffectType {
    pub fn key(&self) -> &str;
    pub fn display_name(&self) -> &str;
}
```

- 内置类型返回内置 key（如 `"basic_correction"`）
- `Plugin(key)` 返回 `key.as_str()`

## EffectCapabilities

```rust
pub struct EffectCapabilities {
    pub supports_render_graph: bool,
    pub supports_branching_render_graph: bool,
    pub supports_render_plan_fallback: bool,
    pub supports_custom_render_processor: bool,
    pub supports_cache_key_contract: bool,
    pub supports_legacy_parameter_evaluation: bool,
}
```

能力由 builder 方法自动设置，不需要手动填充：

| Builder 方法 | 设置的能力 |
|-------------|----------|
| `with_evaluator(...)` | `supports_legacy_parameter_evaluation` |
| `with_graph_builder(...)` | `supports_render_graph` |
| `with_branching_graph_builder(...)` | `supports_render_graph` + `supports_branching_render_graph` |
| `with_render_builder(...)` | `supports_render_plan_fallback` |
| `with_custom_render_backend(...)` | `supports_render_graph` + `supports_render_plan_fallback` + `supports_custom_render_processor` + (`supports_cache_key_contract` 如果有 cache_key_builder) |

## EffectDefinition

```rust
pub struct EffectDefinition {
    // 所有字段为私有，通过 builder 方法和访问器操作
}
```

### 构造函数

```rust
impl EffectDefinition {
    pub fn new(
        key: impl Into<String>,
        display_name: impl Into<String>,
        default_properties: PropertyBag,
    ) -> Self;
}
```

通常不直接调用此函数，而是通过 `EffectPluginDefinitionBuilder`。

### Builder 方法

```rust
impl EffectDefinition {
    /// 添加一个属性定义
    pub fn with_property(mut self, descriptor: PropertyDescriptor) -> Self;

    /// 批量添加属性定义
    pub fn with_properties(mut self, properties: PropertyBag) -> Self;

    /// 设置求值器（旧路径，不推荐新插件使用）
    pub fn with_evaluator(mut self, evaluator: EffectEvaluator) -> Self;

    /// 设置线性图构建器
    pub fn with_graph_builder(mut self, graph_builder: EffectGraphBuilder) -> Self;

    /// 设置分支图构建器
    pub fn with_branching_graph_builder(mut self, graph_builder: EffectGraphBuilder) -> Self;

    /// 设置渲染计划构建器（旧路径）
    pub fn with_render_builder(mut self, render_builder: EffectRenderBuilder) -> Self;

    /// 设置自定义渲染处理器（简化版，无显式 cache_key）
    pub fn with_custom_render_processor(
        self,
        params_builder: EffectRenderParamsBuilder,
        processor: CustomEffectRenderProcessor,
    ) -> Self;

    /// 设置自定义渲染后端（完整版）
    pub fn with_custom_render_backend(
        mut self,
        params_builder: EffectRenderParamsBuilder,
        cache_key_builder: Option<EffectCacheKeyBuilder>,
        cache_policy: EffectCachePolicy,
        processor: CustomEffectRenderProcessor,
    ) -> Self;

    /// 设置插件契约
    pub fn with_plugin_contract(mut self, plugin_contract: EffectPluginContract) -> Self;
}
```

### 访问器

```rust
impl EffectDefinition {
    /// 特效的唯一标识
    pub fn key(&self) -> &str;

    /// 面向用户的显示名称
    pub fn display_name(&self) -> &str;

    /// 能力声明
    pub fn capabilities(&self) -> EffectCapabilities;

    /// 是否支持视觉求值（渲染图可用 且 插件在特效库中可见）
    pub fn supports_visual_evaluation(&self) -> bool;

    /// 是否支持旧式参数求值
    pub fn supports_legacy_parameter_evaluation(&self) -> bool;

    /// 插件契约（仅插件特效有）
    pub fn plugin_contract(&self) -> Option<&EffectPluginContract>;
}
```

## EffectNode

`EffectNode` 是特效的实例——它持有特效类型和具体参数值。一个 `EffectDefinition` 可以被实例化为多个 `EffectNode`（不同片段上的不同参数值）。

```rust
pub struct EffectNode {
    pub id: EffectId,
    pub effect_type: EffectType,
    pub properties: PropertyBag,
    pub params: serde_json::Value,
    pub is_enabled: bool,
}
```

### 构造函数

```rust
impl EffectNode {
    /// 创建新实例，自动从 EffectDefinition 加载默认属性
    pub fn new(effect_type: EffectType) -> Self;
}
```

### 属性访问

```rust
impl EffectNode {
    /// 按路径求值属性（返回当前时间下的值，考虑关键帧插值）
    pub fn evaluate_property(&self, path: &str, time: TimeCode) -> Option<PropertyValue>;

    /// 按后缀求值 f32 属性（便利方法）
    /// 查找 path 以 suffix 结尾的第一个属性，求值为 f32，失败时返回 fallback
    pub fn evaluate_f32_by_suffix(&self, suffix: &str, time: TimeCode, fallback: f32) -> f32;

    /// 按后缀设置静态值
    pub fn set_static_value_by_suffix(&mut self, suffix: &str, value: PropertyValue) -> Result<()>;

    /// 定义属性
    pub fn define_property(&mut self, descriptor: PropertyDescriptor);

    /// 为片段实例化属性（添加命名空间前缀）
    pub fn instantiate_for_clip(&mut self, group_name: String);
}
```

### 渲染求值

```rust
impl EffectNode {
    /// 执行参数求值，结果写入 output
    pub fn evaluate_into(&self, context: EffectEvalContext, output: &mut EffectStackEvaluation);

    /// 执行渲染计划构建，结果追加到 plan
    pub fn evaluate_render_into(&self, context: EffectEvalContext, plan: &mut EffectRenderPlan);

    /// 执行图构建，结果写入 graph
    pub fn evaluate_graph_into(
        &self,
        context: EffectEvalContext,
        graph: &mut EffectGraphBuilderState,
    );
}
```

## EffectEvalContext

```rust
pub struct EffectEvalContext {
    pub time: TimeCode,
}
```

求值上下文，携带当前帧时间。在 graph builder 和 evaluator 中使用。

## EffectRenderOp

所有内置渲染算子的枚举：

```rust
pub enum EffectRenderOp {
    ColorAdjust { exposure: f32, contrast: f32, saturation: f32 },
    WhiteBalance { temperature: f32, tint: f32 },
    GaussianBlur { radius: f32 },
    Sharpen { amount: f32 },
    Vignette { intensity: f32, feather: f32 },
    ChromaticAberration { amount: f32 },
    Grain { amount: f32 },
    Custom {
        key: String,
        params: serde_json::Value,
        cache_key: Option<String>,
        cache_policy: EffectCachePolicy,
    },
}
```

### 方法

```rust
impl EffectRenderOp {
    /// 计算签名字节（用于缓存 key 和去重）
    pub fn hash_signature<H: std::hash::Hasher>(&self, state: &mut H);

    /// 获取此算子的缓存策略
    pub fn cache_policy(&self) -> EffectCachePolicy;

    /// 估算计算成本（1-5，5 最贵）
    pub fn estimated_cost(&self) -> u32;
}
```

`cache_policy()` 返回值：
- `Grain { .. }` → `FrameDependent`
- `Custom { cache_policy, .. }` → 使用自定义的策略
- 其余 → `Deterministic`

## EffectCachePolicy

```rust
pub enum EffectCachePolicy {
    Deterministic,    // 相同输入 → 相同输出
    FrameDependent,   // 依赖 frame seed 或时间噪声
}
```

## 注册函数

```rust
/// 注册特效定义到全局注册表
pub fn register_effect_definition(definition: EffectDefinition);

/// 按类型查找特效定义
pub fn effect_definition(effect_type: &EffectType) -> Option<Arc<EffectDefinition>>;

/// 获取所有已注册的特效类型列表（用于特效库面板）
pub fn effect_library_types() -> Vec<EffectType>;
```

## 构建与求值函数

```rust
/// 为一组特效节点构建渲染图
pub fn build_effect_render_graph(
    effects: &[EffectNode],
    time: TimelineTime,
) -> Result<EffectRenderGraph, EffectGraphBuildError>;

/// 构图、注入 Clip Mask 并编译为可执行图
pub fn compile_clip_effect_graph(
    effects: &[EffectNode],
    masks: &[MaskComponent],
    time: TimelineTime,
) -> Result<Arc<CompiledEffectGraph>, EffectGraphBuildError>;
```

## 类型别名

```rust
pub type EffectGraphBuilder =
    Arc<dyn Fn(
        &EffectNode,
        EffectEvalContext,
        &mut EffectGraphBuilderState,
    ) -> Result<(), EffectGraphBuildError> + Send + Sync>;
pub type EffectRenderParamsBuilder =
    Arc<dyn Fn(
        &EffectNode,
        EffectEvalContext,
    ) -> Result<Option<serde_json::Value>, EffectGraphBuildError> + Send + Sync>;
pub type EffectCacheKeyBuilder =
    Arc<dyn Fn(&EffectNode, EffectEvalContext) -> Option<String> + Send + Sync>;
```

`Ok(None)` 仅表示定义主动选择 identity（例如零强度）；缺失资源或非法作者状态必须返回 `Err`。

## 相关

- [EffectGraphDsl API](./effect-graph-dsl.md) —— DSL 方法参考
- [Plugin Contract API](./plugin-contract.md) —— 契约与版本
- [运行时函数](./runtime.md) —— 执行与缓存
