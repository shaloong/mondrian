# 快速入门

本文帮你搭建 Mondrian 插件开发环境，并在 5 分钟内写出第一个可运行的特效插件。

## 1. 前置条件

- Rust 工具链（MSRV 1.97.1），通过 [rustup](https://rustup.rs) 安装
- 已克隆 Mondrian 仓库并能在本地构建

```bash
# 确认构建正常
cargo build -p mondrian-app
```

## 2. 创建插件目录

Mondrian 插件目前以 Rust crate 形式存在。在仓库中创建你的插件目录：

```bash
mkdir -p crates/mondrian-plugin-hello/src
```

## 3. 编写 Cargo.toml

```toml
[package]
name = "mondrian-plugin-hello"
version = "0.1.0"
edition = "2021"

[dependencies]
mondrian-core = { path = "../mondrian-core" }
mondrian-effects = { path = "../mondrian-effects" }
serde_json = "1"
```

同时在仓库根 `Cargo.toml` 的 `[workspace]` 中加入：

```toml
members = [
    # ... 已有成员
    "crates/mondrian-plugin-hello",
]
```

## 4. 编写第一个特效

在 `crates/mondrian-plugin-hello/src/lib.rs` 中：

```rust
use mondrian_core::automation::{PropertyDescriptor, PropertyValue};
use mondrian_effects::effect::EffectDefinitionError;
use mondrian_effects::{
    register_effect_definition, EffectColorDomainContract, EffectDeterminism,
    EffectExecutionContract, EffectExecutionModes, EffectGraphTopology, EffectPluginContract,
    EffectPluginDefinitionBuilder, EffectRenderOp, EffectResourceLifetime,
    EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent, EffectType,
};

/// 插件入口：在库加载时注册特效定义
pub fn register() -> Result<(), EffectDefinitionError> {
    let plugin_type = EffectType::Plugin("plugin.example.hello".to_string());
    let amount_id = plugin_type
        .parameter_id("amount")
        .expect("static parameter ID");

    let definition = EffectPluginDefinitionBuilder::new(
        plugin_type.key(),
        "Hello Effect",
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
        .with_plugin_contract(EffectPluginContract::new("0.1.0"))
        .property(PropertyDescriptor::new(
            "plugin.example.hello.amount",
            "Amount",
            PropertyValue::Float(0.5),
        ).with_parameter_id(amount_id.clone()))
        .with_graph(move |effect, context, graph| {
            let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 0.5);
            graph.apply(EffectRenderOp::GaussianBlur {
                radius: amount * 10.0,
            });
        })
        .build();

    register_effect_definition(definition)
}
```

上面的合同只声明当前实现真实具备的 CPU Float32、单帧和无状态能力，并对 ROI
保持保守。声明 GPU、额外 precision 或更小 ROI 之前，必须先有对应实现并通过 emitted
graph 合同校验；缺省合同会保守地让插件保持不可执行。

## 5. 在应用中加载插件

在 `mondrian-app` 的依赖中加入你的插件 crate，然后在应用初始化时调用 `register()`：

```rust
// 在 mondrian-app 初始化代码中
mondrian_plugin_hello::register()?;
```

## 6. 验证

运行应用后，打开特效面板，你应该能在特效列表中看到 "Hello Effect"。将其拖到片段上，调整 Amount 参数，画面会产生高斯模糊效果。

## 7. 发生了什么

你刚刚用四件事创建了一个完整插件：

1. **声明身份** —— `EffectType::Plugin("plugin.example.hello")` 定义了插件的全局唯一标识
2. **声明参数** —— `PropertyDescriptor` 定义了可动画、可在 Inspector 中编辑的参数
3. **声明契约** —— `EffectPluginContract` 声明版本和失败策略
4. **描述效果** —— `with_graph(...)` 描述了效果在渲染图中的行为

## 下一步

- 阅读 [核心概念](./02-core-concepts.md) 理解插件模型的完整设计
- 阅读 [特效开发](./03-effect-development.md) 学习 Graph DSL 和分支效果
