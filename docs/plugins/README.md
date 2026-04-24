# Mondrian 插件开发手册

本手册面向插件开发者，从零开始介绍如何为 Mondrian 开发插件。手册按学习路径组织——如果你从未写过 Mondrian 插件，从头开始读；如果你只查 API，直接跳到 [SDK 参考](#sdk-参考)。

## 插件能做什么

一个 Mondrian 插件可以扩展以下能力：

| 能力 | 说明 | 状态 |
|------|------|------|
| 特效（Effect） | 自定义画面处理效果，参与渲染管线 | 已可用 |
| UI 面板（Panel） | 在应用界面中注册自定义面板 | 规划中 |
| 菜单项（Menu） | 向菜单栏插入自定义功能入口 | 规划中 |
| 资产源（Asset Source） | 提供自定义资产获取方式 | 规划中 |

插件通过统一的 `PluginManifest` 声明自己提供哪些能力，一个插件可以同时具备多种能力。

## 阅读导航

### 入门阶段

1. **[快速入门](./01-getting-started.md)** —— 搭建工具链，创建第一个插件项目，跑通 "Hello World" 特效。
2. **[核心概念](./02-core-concepts.md)** —— 理解插件模型：能力声明、生命周期、注册与发现。

### 开发阶段

3. **[特效开发](./03-effect-development.md)** —— 掌握 Effect Graph DSL，学会组合内置算子、编写分支效果、接入自定义渲染后端。
4. **[性能与缓存](./04-performance-and-caching.md)** —— 理解缓存策略，让你的特效在预览和导出时都高效运行。
5. **[版本契约与兼容策略](./05-versioning-and-compatibility.md)** —— 声明插件版本、处理 API 兼容性、配置失败降级行为。

### 深入阶段

6. **[最佳实践与反模式](./06-best-practices.md)** —— 来自核心开发团队的实践经验，帮你避开常见陷阱。
7. **[UI 扩展](./07-ui-extensions.md)** —— 注册自定义面板、菜单项、工具栏按钮。
8. **[打包与分发](./08-packaging.md)** —— 将插件打包为可分发的产物，管理依赖和资源。

### SDK 参考

查阅具体 API 的完整签名和用法说明：

- [EffectDefinition API](./sdk/effect-definition.md)
- [EffectGraphDsl API](./sdk/effect-graph-dsl.md)
- [Plugin Contract API](./sdk/plugin-contract.md)
- [运行时函数](./sdk/runtime.md)

## 最小示例

```rust
use mondrian_effects::{
    EffectPluginDefinitionBuilder, EffectRenderOp, EffectType,
    register_effect_definition,
    EffectPluginContract,
};
use mondrian_core::automation::{PropertyDescriptor, PropertyValue};

let plugin_type = EffectType::Plugin("plugin.example.hello".to_string());

let definition = EffectPluginDefinitionBuilder::new(plugin_type.key(), "Hello")
    .with_plugin_contract(EffectPluginContract::new("1.0.0"))
    .property(PropertyDescriptor::new(
        "plugin.example.hello.amount",
        "Amount",
        PropertyValue::Float(0.5),
    ))
    .with_graph(|effect, context, graph| {
        let amount = effect.evaluate_f32_by_suffix(
            "plugin.example.hello.amount", context.time, 0.5
        );
        graph.apply(EffectRenderOp::GaussianBlur { radius: amount * 10.0 });
    })
    .build();

register_effect_definition(definition);
```

## 相关文档

- [效果系统架构](../architecture/effects-system.md) —— 内部架构概述
- [渲染器架构](../architecture/renderer.md) —— 渲染管线设计
