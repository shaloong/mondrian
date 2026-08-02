# 特效开发

本文详述如何开发 Mondrian 特效插件，从简单的线性特效到需要自定义渲染后端的分支效果。

## 1. 选择开发路径

在开始写代码之前，先确定你的特效属于哪一类：

| 类型 | 适用场景 | 难度 | 入口 |
|------|----------|------|------|
| 线性图特效 | 由内置算子组合的效果 | 低 | `with_graph(...)` |
| 分支图特效 | 需要分支/混合/蒙版的效果 | 中 | `with_branching_graph(...)` |
| 自定义 CPU RGBA8 渲染 | 内置算子无法表达的编码像素处理 | 高 | `with_custom_render_backend(...)` |

**原则：能用内置算子表达就优先用内置算子；需要分支就上 DSL；只有像素算法确实无法用内置算子组合时才走自定义渲染。**

## 2. 线性图特效

线性特效是最简单的形式——对输入依次应用一系列操作：

```rust
use mondrian_effects::{
    register_effect_definition, EffectColorDomainContract, EffectDeterminism,
    EffectExecutionContract, EffectExecutionModes, EffectGraphTopology,
    EffectPluginContract, EffectPluginDefinitionBuilder, EffectRenderOp,
    EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
    EffectTemporalInputExtent, EffectType,
};
use mondrian_core::automation::{PropertyDescriptor, PropertyValue};

let plugin_type = EffectType::Plugin("plugin.example.sharpen".to_string());
let amount_id = plugin_type
    .parameter_id("amount")
    .expect("static parameter ID");

let definition = EffectPluginDefinitionBuilder::new(
    plugin_type.key(),
    "Sharpen",
    EffectColorDomainContract::SCENE_LINEAR,
)
    .with_execution_contract(EffectExecutionContract {
        execution_modes: EffectExecutionModes::CPU_F32,
        determinism: EffectDeterminism::Deterministic,
        state_model: EffectStateModel::Stateless,
        temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
        roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
        resource_lifetime: EffectResourceLifetime::Frame,
        topology: EffectGraphTopology::LinearChain,
    })
    .with_plugin_contract(EffectPluginContract::new("1.0.0"))
    .property(PropertyDescriptor::new(
        "plugin.example.sharpen.amount",
        "Amount",
        PropertyValue::Float(0.5),
    ).with_parameter_id(amount_id.clone()))
    .with_graph(move |effect, context, graph| {
        let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 0.5);
        graph.apply(EffectRenderOp::Sharpen { amount });
    })
    .build();

register_effect_definition(definition)?;
```

`graph.apply(op)` 将操作追加到当前输出链上——输入经过操作后成为新的当前输出。

## 3. 分支图特效

当你需要从当前输出分出一条支路、处理后再与当前输出融合时，使用 `with_branching_graph`。

### 3.1 Blend（混合）

"软化"类特效的典型模式：从当前输出分叉，处理后再与原输出混合：

```rust
let radius_id = plugin_type.parameter_id("radius").expect("static parameter ID");
let opacity_id = plugin_type.parameter_id("opacity").expect("static parameter ID");

// PropertyDescriptor 分别通过 with_parameter_id(...) 绑定上述稳定身份。
let builder = builder.with_branching_graph(move |effect, context, graph| {
    let radius = effect.evaluate_f32_parameter(&radius_id, context.time, 6.0);
    let opacity = effect.evaluate_f32_parameter(&opacity_id, context.time, 0.35);

    if radius <= 1.0e-4 || opacity <= 1.0e-4 {
        return; // 参数不足以产生可见效果时，直接退化为 identity
    }

    graph.blend_current(BlendMode::Screen, opacity, |graph, source| {
        graph.apply_to(source, EffectRenderOp::GaussianBlur { radius })
    });
})
```

`blend_current(mode, opacity, build)` 做的事情：
1. 记录当前输出作为 base
2. 在闭包中构建 overlay 支路
3. 以 `mode` 和 `opacity` 将 overlay 混合回 base

适用场景：glow、bloom、soft focus、fake diffusion 等。

### 3.2 Mask（蒙版）

`mask_current(invert, mask_op, build)` 要求 mask 子树输出真实的
`EffectColorDomain::AlphaMask`，并要求显式传入
`MaskOp::{Add, Subtract, Intersect, Difference}`。`invert = true` 表示先反转 matte；
构图闭包的末尾表达式必须返回 `EffectGraphValue`。

当前高层插件 DSL 尚未公开 alpha `MaskSource` 或 RGB-to-matte producer，因此不要直接
构造 raw `EffectRenderOp::Custom` 来伪造蒙版：它既没有通过 Definition 绑定 processor，
也不能证明 AlphaMask 颜色域，编译会失败关闭。Clip 作者蒙版由引擎在编译阶段注入；
插件侧 mask producer 要等类型化入口公开后再使用此组合方法。

### 3.3 Branch（通用分支）

更灵活的分支操作：

```rust
let branch_result = graph.branch(some_input, |graph, input| {
    // 在 input 上构建独立子图
    graph.apply_to(input, EffectRenderOp::GaussianBlur { radius: 4.0 })
});
// branch_result 可用于后续 blend/mask/set_output
```

## 4. 自定义渲染后端

当内置 `EffectRenderOp` 无法满足特殊编码像素算法时，可以使用当前的自定义渲染
后端。这个 ABI 是 CPU RGBA8 staged-buffer 处理器，不是 GPU shader Interface，也
不能据此声明 GPU 或 Float32 execution mode。

### 4.1 基本结构

自定义渲染后端涉及四块配置：

```rust
let amount_id = plugin_type.parameter_id("amount").expect("static parameter ID");

let builder = builder.with_custom_render_backend(
    // 1. 参数构建器 —— 从特效节点和上下文中提取渲染参数
    Arc::new(move |effect, context| {
        let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 1.0);
        Ok(Some(serde_json::json!({ "amount": amount })))
    }),

    // 2. 无外部资源，因此不需要额外资源 cache key
    None,

    // 3. 缓存策略
    EffectCachePolicy::Deterministic,

    // 4. 像素处理器
    Arc::new(|buffer, width, height, params, frame_seed| {
        // buffer: &mut Vec<u8> RGBA 像素数据
        // width, height: 帧尺寸
        // params: serde_json::Value 渲染参数
        // frame_seed: i64 帧种子（仅 frame-dependent 特效关心）
        //
        // 直接在 buffer 上做原地像素处理
        Ok(())
    }),
);
```

### 4.2 处理器约定

- 参数构建器返回 `Ok(Some(params))` 才创建自定义节点；`Ok(None)` 只表示定义主动判定该实例为 identity（例如强度为零），资源缺失或非法状态必须返回 `EffectGraphBuildError`
- Definition 的 execution contract 必须只声明该 ABI 真实实现的
  `EffectExecutionModes::CPU_U8`，不能声明 CPU Float32 或 GPU
- 处理器在**独立 staged buffer** 上执行，不是帧主链 buffer
- `Ok(())` 返回时，staged 结果提交到渲染管线
- `Err(...)` 或 panic 时，staged 结果被丢弃并返回结构化执行错误；插件契约只决定定义后续是否禁用和是否在效果库可见
- 不要在处理器中半途报错却仍然写回部分像素结果
- 参数构建器、cache-key 构建器和 processor 都是执行路径，禁止文件或网络 I/O

### 4.3 何时提供 cache_key

`cache_key` 只能标识已经绑定的不可变资源 revision，例如随插件注册并预加载的
固定 kernel。路径本身不是 revision；同一路径的内容可以改变。

```rust
let prepared_kernel_revision = "kernel:sha256:8f..."; // 在执行路径之外取得
Some(Arc::new(move |_effect, _context| {
    Some(prepared_kernel_revision.to_string())
}))
```

当前高层 `EffectPluginDefinitionBuilder` 尚未提供“作者路径 → 版本化不可变资源”的
preparation/revalidation Interface。依赖用户选择的 LUT、模型或配置时，在该 Interface
落地前必须返回 `EffectGraphBuildError::ResourceUnavailable`，不能逐帧读取文件、
用 path-only key 复用，或把资源缺失当 identity。不要用临时字符串、随机数、时间戳
作为 cache key。详见 [性能与缓存](./04-performance-and-caching.md)。

## 5. 参数模型设计

参数是特效与 Inspector 面板和关键帧系统的接口。好的参数设计影响编辑体验：

### 5.1 参数命名

遵循 `{plugin_key}.{param_name}` 格式：

```rust
PropertyDescriptor::new(
    "plugin.example.glow.radius", // 当前作者/UI 地址
    "Radius",                     // 显示名称
    PropertyValue::Float(6.0),    // 默认值
)
.with_parameter_id(
    plugin_type.parameter_id("radius").expect("static parameter ID"),
)
```

### 5.2 参数类型

| PropertyValue 变体 | 用途 |
|--------------------|------|
| `Bool(v)` | 开关参数 |
| `Int(i64)` | 整数参数（迭代次数等） |
| `Float(f32)` | 常规连续参数 |
| `Double(f64)` | 需要 f64 作者精度的连续参数 |
| `Vec2(glam::Vec2)` | 二维向量 |
| `Vec3(glam::Vec3)` | 三维向量 |
| `Color(mondrian_core::Color)` | RGBA 颜色 |
| `Vec4([f32; 4])` | 四分量数值 |
| `Enum(String)` | 来自 schema 选项的稳定枚举 key |
| `Resource(ParameterResourceReference)` | 可恢复的项目资产、外部文件或 URI 引用 |
| `Text(String)` | 文本；不是资源引用 |

### 5.3 参数稳定性

`ParameterId` 是序列化、自动化和执行的稳定身份；`PropertyDescriptor::path` 只是当前
作者/UI 地址。产品 Definition 必须显式 `.with_parameter_id(...)`，不能依赖
`PropertyDescriptor::new` 为原型便利派生的 ID。

## 6. Graph DSL 参考速览

`EffectGraphDsl` 提供这些核心方法：

| 方法 | 作用 |
|------|------|
| `source()` | 获取图的原始输入 |
| `current()` | 获取当前输出 |
| `set_output(v)` | 设置当前输出 |
| `apply(op)` | 对当前输出应用一元操作 |
| `apply_to(input, op)` | 对指定输入应用一元操作 |
| `branch(input, f)` | 在 input 上构建子图，返回结果 |
| `blend(base, mode, opacity, f)` | 以 blend 模式混合 base 和 overlay |
| `blend_current(mode, opacity, f)` | 从当前输出分支，处理后与当前混合 |
| `mask(input, invert, mask_op, f)` | 用 alpha 子图和显式 `MaskOp` 遮蔽 input |
| `mask_current(invert, mask_op, f)` | 用 alpha 子图和显式 `MaskOp` 遮蔽当前输出 |

完整 API 见 [EffectGraphDsl API 参考](./sdk/effect-graph-dsl.md)。

## 下一步

- [性能与缓存](./04-performance-and-caching.md) —— 让你的特效在预览和导出中都高效
- [版本契约](./05-versioning-and-compatibility.md) —— 管理版本兼容、错误传播、定义状态与效果库可见性
