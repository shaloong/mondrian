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
    pub fn key(&self) -> String;
    pub fn display_name(&self) -> &str;
}
```

- 内置类型返回带命名空间的 key（如 `"builtin.basic_correction"`）
- `Plugin(key)` 返回该持久化 key 的副本

`EffectType::WhiteBalance` 等类型可以存在于作者模型中，但当前并非每个内置类型都有
可执行 Definition。尤其是 White Balance 目前是 **modeled-only / execution
unavailable**；没有可供插件构图使用的 `WhiteBalance` 渲染算子。启用但不可执行的
效果必须返回结构化错误，不能伪装成 identity。

## EffectExecutionContract

插件不能靠若干松散布尔值声明“支持 CPU/GPU”。每个 Definition 必须提供一份完整、
可验证的 `EffectExecutionContract`，其中包括：

- 精确的 `(processing backend, sample representation)` 组合；
- determinism、连续状态与前后帧需求；
- ROI 扩张、资源生命周期和最大图拓扑。

`EffectExecutionModes::CPU_F32` 只代表 CPU Float32，不会与其他 backend 或 precision
自动组成笛卡尔积。插件默认使用
`EffectExecutionContract::CONSERVATIVE_PLUGIN_DEFAULT`：不准入任何 backend，并保守声明
unbounded temporal、stateful、full-frame 和 continuity-session 义务。插件只有显式调用
`with_execution_contract(...)` 且 emitted graph 反证该声明不比真实实现乐观后，才能进入
prepared execution。

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
        color_domain_contract: EffectColorDomainContract,
    ) -> Self;
}
```

通常不直接调用此函数，而是通过
`EffectPluginDefinitionBuilder::new(key, display_name, color_domain_contract)`。

### Builder 方法

```rust
impl EffectDefinition {
    /// 添加一个属性定义
    pub fn with_property(mut self, descriptor: PropertyDescriptor) -> Self;

    /// 批量添加属性定义
    pub fn with_properties(mut self, properties: PropertyBag) -> Self;

    /// 设置线性图构建器
    pub fn with_graph_builder(mut self, graph_builder: EffectGraphBuilder) -> Self;

    /// 设置分支图构建器
    pub fn with_branching_graph_builder(mut self, graph_builder: EffectGraphBuilder) -> Self;

    /// 准备一次不可变资源，再绑定逐帧图求值器
    pub fn with_prepared_graph_builder(
        mut self,
        graph_preparer: EffectGraphPreparer,
    ) -> Self;

    /// 声明完整的执行合同；插件默认合同不会准入任何 backend
    pub fn with_execution_contract(
        mut self,
        execution_contract: EffectExecutionContract,
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

上面是低层 Definition 能力；插件作者必须通过
`EffectPluginDefinitionBuilder::with_custom_render_backend(...)` 进入自定义处理路径。
高层插件 builder 没有简化版 custom-processor 方法，也不允许随后手工追加未绑定的
Custom op。该 staged RGBA8 ABI 只实现 `EffectExecutionModes::CPU_U8`；Definition
不能把它声明成 CPU Float32 或 GPU。

### Prepared resource Interface

需要在逐帧求值前绑定不可变资源的 Definition 使用
`with_prepared_graph_builder(...)`。Preparer 的第三个参数是借用的
`EffectPreparationContext`：

```rust
pub type EffectGraphPreparer = Arc<
    dyn for<'a> Fn(
            &EffectNode,
            WorkingColorSpace,
            EffectPreparationContext<'a>,
        ) -> Result<PreparedEffectEvaluator, EffectGraphBuildError>
        + Send
        + Sync,
>;

impl EffectPreparationContext<'_> {
    pub fn prepare_cube_lut(
        self,
        path: &Path,
    ) -> mondrian_core::Result<Arc<PreparedLut3D>>;
}
```

该 context 只在低频 Program preparation 期间有效；不能把 context 或其 cache
owner 捕获进求值器。Preparer 应捕获返回的不可变 `Arc`，并在
`PreparedEffectEvaluator` 上声明依赖和驻留量：

```rust
Ok(PreparedEffectEvaluator::new(frame_evaluator)
    .with_retained_resource_bytes(prepared_lut.retained_bytes_estimate())
    .with_dependency(EffectResourceDependency::CubeLut {
        path,
        semantic_fingerprint: *prepared_lut.semantic_fingerprint(),
    }))
```

Preview 与每个 Export attempt 使用不同的 owner-scoped bounded cache；插件不得建立
process-global LUT cache，也不得用 path、mtime、size 或短时间窗口冒充内容 revision。
`with_retained_resource_bytes(...)` 是 Program cache 的保守 logical charge，不是
allocator、GPU memory、Working Set 或 RSS 测量。当前 context 只提供 `.cube` LUT
preparation；其他外部资源在有类型化 preparation/revalidation Interface 前必须失败关闭。

### 访问器

```rust
impl EffectDefinition {
    /// 特效的唯一标识
    pub fn key(&self) -> &str;

    /// 面向用户的显示名称
    pub fn display_name(&self) -> &str;

    /// 完整执行合同
    pub fn execution_contract(&self) -> EffectExecutionContract;

    /// 输入/输出处理域合同
    pub fn color_domain_contract(&self) -> EffectColorDomainContract;

    /// 是否具有求值器、非空执行模式、无序列状态且插件健康可见
    pub fn supports_visual_evaluation(&self) -> bool;

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
    /// 创建仅含类型和空 PropertyBag 的纯作者数据
    pub fn new(effect_type: EffectType) -> Self;
}
```

需要从当前已注册 Definition 实例化规范默认参数时，应导入
`EffectNodeExt` 并调用 `EffectNode::with_defaults(effect_type)`。不要把
`EffectNode::new` 误当成 Definition factory。

### 属性访问

```rust
impl EffectNode {
    /// 按路径求值属性（返回当前时间下的值，考虑关键帧插值）
    pub fn evaluate_property(&self, path: &str, time: TimelineTime) -> Option<PropertyValue>;

    /// 按 Definition-stable 参数身份求值
    pub fn evaluate_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
    ) -> Option<PropertyValue>;

    /// 按稳定参数身份求值 f32，类型不符或不存在时返回 fallback
    pub fn evaluate_f32_parameter(
        &self,
        parameter_id: &ParameterId,
        time: TimelineTime,
        fallback: f32,
    ) -> f32;

    /// 按稳定参数身份设置静态值
    pub fn set_static_value_by_parameter(
        &mut self,
        parameter_id: &ParameterId,
        value: PropertyValue,
    ) -> mondrian_core::Result<()>;

    /// 定义属性
    pub fn define_property(&mut self, descriptor: PropertyDescriptor);

    /// 为片段实例化属性（添加命名空间前缀）
    pub fn instantiate_for_clip(&mut self, group_name: String);
}
```

执行路径不由 `EffectNode` 自己解释。调用方通过 `PreparedEffectStack` /
`PreparedEffectProgram` 绑定 Definition、资源、工作色彩空间和执行合同，再按帧求值；
插件只提供 Definition builder。

## PropertyValue

```rust
pub enum PropertyValue {
    Bool(bool),
    Int(i64),
    Float(f32),
    Double(f64),
    Vec2(glam::Vec2),
    Vec3(glam::Vec3),
    Color(mondrian_core::Color),
    Vec4([f32; 4]),
    Enum(String),
    Resource(ParameterResourceReference),
    Text(String),
}
```

资源必须使用类型化 `ParameterResourceReference`，普通文本使用 `Text`；不存在把路径、
URL 和任意文本混在一起的 String 变体。产品 Definition 必须用
`PropertyDescriptor::with_parameter_id(...)` 绑定稳定 `ParameterId`。

## EffectEvalContext

```rust
pub struct EffectEvalContext {
    pub time: TimelineTime,
    pub working_color_space: WorkingColorSpace,
}
```

求值上下文携带当前帧的精确作者时间，以及该 Sequence 的 scene-linear 工作色彩空间。
颜色算子不得隐式假定 BT.709。

## EffectRenderOp

所有内置渲染算子的枚举：

```rust
pub enum EffectRenderOp {
    ColorAdjust {
        exposure: f32,
        contrast: f32,
        saturation: f32,
        working_color_space: WorkingColorSpace,
    },
    GaussianBlur { radius: f32 },
    Sharpen { amount: f32 },
    Vignette { intensity: f32, feather: f32 },
    ChromaticAberration { amount: f32 },
    Grain { amount: f32 },
    TemporalFrameBlend {
        sample_offset: TimelineTime,
        mix: f32,
    },
    Lut3D { lut: Arc<PreparedLut3D>, intensity: f32 },
    // Custom 是编译器内部承载已绑定 processor 的公开 IR 形状；
    // 插件作者不得直接构造。
}
```

自定义 CPU RGBA8 处理必须通过
`EffectPluginDefinitionBuilder::with_custom_render_backend(...)` 绑定。直接构造 raw
`EffectRenderOp::Custom`（尤其是令 processor 为空）会在编译时失败关闭，也绕过了
Definition revision 与 processor identity。

`TemporalFrameBlend` 需要精确声明有限 past/future 窗口，并由 temporal executor
提供当前帧和 `time + sample_offset`；负值表示历史，正值表示 lookahead，零值复用
当前帧。单帧执行器不会静默降级。`Lut3D` 只接受 preparation 阶段产生的
不可变 `PreparedLut3D`，不能在逐帧构图时读取文件。
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
    Uncacheable,      // 不能承诺跨调用复现；不得建立可复用输出 key
}
```

## 注册函数

```rust
/// 校验后注册；非法参数 schema 或执行合同不会进入全局注册表
pub fn register_effect_definition(
    definition: EffectDefinition,
) -> Result<(), EffectDefinitionError>;

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
    working_color_space: WorkingColorSpace,
) -> Result<EffectRenderGraph, EffectGraphBuildError>;

/// 构图、注入 Clip Mask 并编译为可执行图
pub fn compile_clip_effect_graph(
    effects: &[EffectNode],
    masks: &[MaskComponent],
    time: TimelineTime,
    working_color_space: WorkingColorSpace,
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
