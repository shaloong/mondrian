# Mondrian 效果扩展开发手册

本手册介绍当前源码级 Rust Effect SDK。通用扩展平台正式命名为 **Extension**，
其整体规划见 [Extension 文档](../README.md)。本目录是效果 Extension 的正式开发入口，
使用 EffectExtension* 与 extension_sdk/extension_contract。项目已有定义 key 和参数身份
按原值读取，不通过名称清理自动迁移。
OpenFX/VST3/CLAP 的当前支持范围见[原生格式插件宿主](../../architecture/native-plugin-hosts.md)。

如果你从未使用 Effect SDK，从头开始读；只查 API 可以直接跳到 [SDK 参考](#sdk-参考)。

## 当前与规划能力

当前效果 SDK 与未来通用 Extension 平台的状态如下：

| 能力 | 说明 | 状态 |
|------|------|------|
| 特效（Effect） | 源码集成的画面处理定义，参与渲染管线 | 已有 SDK；不代表独立扩展安装或安全沙箱 |
| UI 面板（Panel） | 在应用界面中注册自定义面板 | 规划中 |
| 菜单项（Menu） | 向菜单栏插入自定义功能入口 | 规划中 |
| 资产源（Asset Source） | 提供自定义资产获取方式 | 规划中 |

当前通过 `EffectDefinition` 及其 Contract 注册效果。未来 Extension 可同时提供多种贡献；通用清单格式尚未确定，也没有对应的公开实现。

## 阅读导航

### 入门阶段

1. **[快速入门](01-getting-started.md)** —— 搭建工具链，创建第一个源码效果示例，跑通 "Hello World" 特效。
2. **[核心概念](02-core-concepts.md)** —— 理解效果定义模型：能力声明、生命周期、注册与发现。

### 开发阶段

3. **[特效开发](03-effect-development.md)** —— 掌握 Effect Graph DSL，学会组合内置算子、编写分支效果、接入自定义渲染后端。
4. **[性能与缓存](04-performance-and-caching.md)** —— 理解缓存策略，让你的特效在预览和导出时都高效运行。
5. **[版本契约与兼容策略](05-versioning-and-compatibility.md)** —— 声明效果定义版本、处理 API 兼容性、配置失败报告与定义隔离行为。

### 深入阶段

6. **[最佳实践与反模式](06-best-practices.md)** —— 来自核心开发团队的实践经验，帮你避开常见陷阱。
7. **[UI 扩展](07-ui-extensions.md)** —— 已确定的真实 Web UI 方向与当前边界，尚未开放 API。
8. **[打包与分发](08-packaging.md)** —— 当前集成方式与未来 Extension 包的规划边界。

### SDK 参考

查阅具体 API 的完整签名和用法说明：

- [EffectDefinition API](sdk/effect-definition.md)
- [EffectGraphDsl API](sdk/effect-graph-dsl.md)
- [效果合同 API](sdk/extension-contract.md)
- [运行时函数](sdk/runtime.md)

## 最小示例

```rust
use mondrian_effects::{
    register_effect_definition, EffectColorDomainContract, EffectDeterminism,
    EffectExecutionContract, EffectExecutionModes, EffectGraphTopology,
    EffectExtensionContract, EffectExtensionDefinitionBuilder, EffectRenderOp,
    EffectResourceLifetime, EffectRoiPropagation, EffectStateModel,
    EffectTemporalInputExtent, EffectType,
};
use mondrian_core::automation::{PropertyDescriptor, PropertyValue};

let effect_type = EffectType::External("extension.example.hello".to_string());
let amount_id = effect_type
    .parameter_id("amount")
    .expect("static parameter ID");

let definition = EffectExtensionDefinitionBuilder::new(
    effect_type.key(),
    "Hello",
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
    .with_extension_contract(EffectExtensionContract::new("1.0.0"))
    .property(PropertyDescriptor::new(
        "extension.example.hello.amount",
        "Amount",
        PropertyValue::Float(0.5),
    ).with_parameter_id(amount_id.clone()))
    .with_graph(move |effect, context, graph| {
        let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 0.5);
        graph.apply(EffectRenderOp::GaussianBlur { radius: amount * 10.0 });
    })
    .build();

register_effect_definition(definition)?;
```

注册返回 `Result`；不要忽略非法参数 schema、执行合同或重复语义造成的失败。

## 相关文档

- [效果系统架构](../../architecture/effect-system.md) —— 内部架构概述
- [渲染管线架构](../../architecture/render-pipeline.md) —— 渲染管线设计
