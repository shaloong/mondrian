# 特效开发

本文详述如何开发 Mondrian 特效插件，从简单的线性特效到需要自定义渲染后端的分支效果。

## 1. 选择开发路径

在开始写代码之前，先确定你的特效属于哪一类：

| 类型 | 适用场景 | 难度 | 入口 |
|------|----------|------|------|
| 参数求值 | 非视觉参数计算、桥接旧逻辑 | 低 | `with_evaluator(...)` |
| 线性图特效 | 由内置算子组合的效果 | 低 | `with_graph(...)` |
| 分支图特效 | 需要分支/混合/蒙版的效果 | 中 | `with_branching_graph(...)` |
| 自定义渲染 | 内置算子无法表达的像素处理 | 高 | `with_custom_render_backend(...)` |

**原则：能用内置算子表达就优先用内置算子；需要分支就上 DSL；只有像素算法确实无法用内置算子组合时才走自定义渲染。**

## 2. 线性图特效

线性特效是最简单的形式——对输入依次应用一系列操作：

```rust
use mondrian_effects::{
    EffectPluginDefinitionBuilder, EffectRenderOp, EffectType,
    EffectPluginContract,
    register_effect_definition,
};
use mondrian_core::automation::{PropertyDescriptor, PropertyValue};

let plugin_type = EffectType::Plugin("plugin.example.sharpen".to_string());

let definition = EffectPluginDefinitionBuilder::new(plugin_type.key(), "Sharpen")
    .with_plugin_contract(EffectPluginContract::new("1.0.0"))
    .property(PropertyDescriptor::new(
        "plugin.example.sharpen.amount",
        "Amount",
        PropertyValue::Float(0.5),
    ))
    .with_graph(|effect, context, graph| {
        let amount = effect.evaluate_f32_by_suffix(
            "plugin.example.sharpen.amount", context.time, 0.5
        );
        graph.apply(EffectRenderOp::Sharpen { amount });
    })
    .build();

register_effect_definition(definition);
```

`graph.apply(op)` 将操作追加到当前输出链上——输入经过操作后成为新的当前输出。

## 3. 分支图特效

当你需要从当前输出分出一条支路、处理后再与当前输出融合时，使用 `with_branching_graph`。

### 3.1 Blend（混合）

"软化"类特效的典型模式：从当前输出分叉，处理后再与原输出混合：

```rust
.with_branching_graph(|effect, context, graph| {
    let radius = effect.evaluate_f32_by_suffix(
        "plugin.example.glow.radius", context.time, 6.0
    );
    let opacity = effect.evaluate_f32_by_suffix(
        "plugin.example.glow.opacity", context.time, 0.35
    );

    if radius <= 1.0e-4 || opacity <= 1.0e-4 {
        return; // 参数不足以产生可见效果时，直接退化为 identity
    }

    graph.blend_current(BlendMode::Screen, opacity, |graph, source| {
        graph.apply_to(source, EffectRenderOp::GaussianBlur { radius });
    });
})
```

`blend_current(mode, opacity, build)` 做的事情：
1. 记录当前输出作为 base
2. 在闭包中构建 overlay 支路
3. 以 `mode` 和 `opacity` 将 overlay 混合回 base

适用场景：glow、bloom、soft focus、fake diffusion 等。

### 3.2 Mask（蒙版）

当你需要用一个由子树生成的 alpha 来控制当前输出的可见区域：

```rust
graph.mask_current(false, |graph, source| {
    graph.apply_to(source, EffectRenderOp::Custom {
        key: "plugin.example.mask_shape".to_string(),
        params: serde_json::json!({"shape": "ellipse"}),
        cache_key: Some("example-mask-ellipse".to_string()),
        cache_policy: EffectCachePolicy::Deterministic,
    });
});
```

`mask_current(invert, build)` 中 `invert = true` 表示反转蒙版。

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

当内置 `EffectRenderOp` 无法满足需求时——比如需要特殊像素算法、依赖外部卷积核、或者对接自定义 GPU shader——使用自定义渲染后端。

### 4.1 基本结构

自定义渲染后端涉及四块配置：

```rust
.with_custom_render_backend(
    // 1. 参数构建器 —— 从特效节点和上下文中提取渲染参数
    Arc::new(|effect, context| {
        let path = effect.evaluate_str_by_suffix(
            "plugin.example.lut.path", context.time, ""
        );
        Ok(Some(serde_json::json!({ "lut_path": path })))
    }),

    // 2. 缓存 key 构建器 —— 声明缓存标识（可选）
    Some(Arc::new(|effect, context| {
        let path = effect.evaluate_str_by_suffix(
            "plugin.example.lut.path", context.time, ""
        );
        Some(format!("lut:{}", path))
    })),

    // 3. 缓存策略
    EffectCachePolicy::Deterministic,

    // 4. 像素处理器
    Arc::new(|buffer, width, height, params, frame_seed| {
        // buffer: &mut [u8] RGBA 像素数据
        // width, height: 帧尺寸
        // params: serde_json::Value 渲染参数
        // frame_seed: u64 帧种子（仅 frame-dependent 特效关心）
        //
        // 直接在 buffer 上做原地像素处理
        Ok(())
    }),
)
```

### 4.2 处理器约定

- 参数构建器返回 `Ok(Some(params))` 才创建自定义节点；`Ok(None)` 只表示定义主动判定该实例为 identity（例如强度为零），资源缺失或非法状态必须返回 `EffectGraphBuildError`
- 处理器在**独立 staged buffer** 上执行，不是帧主链 buffer
- `Ok(())` 返回时，staged 结果提交到渲染管线
- `Err(...)` 或 panic 时，staged 结果被丢弃并返回结构化执行错误；插件契约只决定定义后续是否禁用和是否在效果库可见
- 不要在处理器中半途报错却仍然写回部分像素结果

### 4.3 何时提供 cache_key

如果你的自定义处理器依赖外部稳定资源（文件、模型、内核配置），提供稳定 `cache_key` 可以让运行时复用计算结果：

```rust
Some(Arc::new(|effect, context| {
    let version = effect.evaluate_i32_by_suffix(
        "plugin.example.kernel.version", context.time, 1
    );
    Some(format!("kernel:v{}", version))
}))
```

不要用临时字符串、随机数、时间戳作为 cache_key。详见 [性能与缓存](./04-performance-and-caching.md)。

## 5. 参数模型设计

参数是特效与 Inspector 面板和关键帧系统的接口。好的参数设计影响编辑体验：

### 5.1 参数命名

遵循 `{plugin_key}.{param_name}` 格式：

```rust
PropertyDescriptor::new(
    "plugin.example.glow.radius",  // 全局唯一的参数 key
    "Radius",                       // 显示名称
    PropertyValue::Float(6.0),      // 默认值
)
```

### 5.2 参数类型

| PropertyValue 变体 | 用途 |
|--------------------|------|
| `Float(v)` | 滑块参数（半径、强度、阈值等） |
| `Int(v)` | 整数参数（迭代次数等） |
| `Bool(v)` | 开关参数 |
| `Color(r, g, b, a)` | 颜色参数 |
| `String(v)` | 路径、URL 等字符串参数 |

### 5.3 参数稳定性

参数 key 会被序列化到项目文件（`.mdp`）中。改名意味着旧项目丢失该参数值。请在插件设计阶段就确定参数命名，避免后期重命名。

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
| `mask(input, invert, f)` | 用子图为 input 生成 alpha 蒙版 |
| `mask_current(invert, f)` | 用子图为当前输出生成 alpha 蒙版 |

完整 API 见 [EffectGraphDsl API 参考](./sdk/effect-graph-dsl.md)。

## 下一步

- [性能与缓存](./04-performance-and-caching.md) —— 让你的特效在预览和导出中都高效
- [版本契约](./05-versioning-and-compatibility.md) —— 管理版本兼容、错误传播、定义状态与效果库可见性
