# 核心概念

本文介绍 Mondrian 插件系统的基本概念：插件是什么、有哪些能力、生命周期如何、注册与发现机制是怎样的。

## 1. 插件模型概览

本手册中的源码特效插件是一个独立的 Rust crate，由应用显式调用注册入口。
一个 crate 可注册多个 `EffectDefinition`；每个定义包含参数、执行能力和
`EffectPluginContract`，由运行时按稳定 key 发现。

统一 SDK 插件包 Manifest 和 UI/资产源扩展接口仍属规划。原生 OpenFX、CLAP/VST3
由各自的受监督子进程 Adapter 加载，支持范围见[插件架构](../architecture/plugin-system.md)；
它们不构成源码特效 crate 的动态加载 ABI。

## 2. 能力类型

### 2.1 特效能力（Effect）

特效是插件最常见的入口。一个插件可以提供一到多个 `EffectDefinition`，每个定义描述一个可应用在片段上的视觉处理。

特效能力涉及这些模块：

- `EffectDefinition` —— 特效的静态描述（标识、参数、能力声明）
- `EffectGraphDsl` —— 描述特效在渲染图中的行为
- `EffectRenderOp` —— 内置渲染算子（模糊、混合、蒙版等）
- `CustomEffectRenderProcessor` —— 自定义像素处理逻辑

详见 [特效开发](./03-effect-development.md)。

### 2.2 UI 扩展能力（规划中）

UI 扩展允许插件向应用界面注册自定义组件：

- **面板** —— 在侧边或底部区域注册独立面板
- **菜单项** —— 向主菜单或右键菜单插入功能入口
- **工具栏按钮** —— 在工具栏中增加操作按钮

详见 [UI 扩展](./07-ui-extensions.md)。

### 2.3 资产源能力（规划中）

允许插件提供自定义资产获取方式，如从云存储加载、从第三方 API 获取等。

## 3. 插件标识

每个插件使用 **反向域名格式** 的 key 作为全局唯一标识：

```
plugin.<author>.<name>
```

示例：
- `plugin.example.hello`
- `plugin.acme.color_grading`
- `plugin.ai.auto_exposure`

插件下的每个能力也需要有稳定、可预测的 key。对于特效参数，遵循 `{plugin_key}.{param_name}` 的命名惯例，例如 `plugin.ai.auto_exposure.exposure`。

## 4. 注册与发现

插件通过以下机制与运行时交互：

### 4.1 注册

插件在加载时调用注册函数，将定义注册到全局注册表中：

```rust
// Contract 是 Definition 的组成部分；非法 schema/合同必须显式处理
register_effect_definition(definition.with_plugin_contract(contract))?;
```

Definition 注册表是唯一的进程级发现 Authority。Contract 不存在可独立改写的第二个
Registry；替换 Definition 会推进同一 Registry Revision，并形成新的运行时隔离代。

### 4.2 发现

运行时通过 key 查询注册表来发现插件：

```rust
// 按 key 查找特效定义
let def = effect_definition(&EffectType::Plugin("plugin.example.hello".to_string()));

// 查询插件运行时状态
let status = effect_plugin_runtime_status("plugin.example.hello");
```

## 5. 生命周期

插件在 Mondrian 中的生命周期：

```text
  加载 crate → 调用 register() → 注册 Definition + Contract
                                    │
                    ┌───────────────┴───────────────┐
                    ▼                               ▼
            特效面板可发现                   运行时状态跟踪
                    │                               │
                    ▼                               ▼
            用户应用到片段                   执行中可能失败
                    │                               │
                    ▼                               ▼
            参与渲染管线                   报告错误/隔离本次定义
```

关键点：
- **注册发生在应用启动时**，早于任何项目加载
- **每次注册都会推进 Registry Revision**；同一 key 替换旧定义并形成新一代，缓存绑定必须据此失效
- **失败报告**：执行错误或 panic 由运行时记录并向调用方返回结构化错误，不能把失败帧当作无效果的成功帧
- **隔离绑定定义代**：`DisableDefinition` 以 `(effect_key, definition_registry_revision)` 隔离失败定义；重新注册产生独立的新代，旧代迟到的失败不会禁用新代

## 6. 核心约束

理解这几点可以避免大多数设计错误：

### 6.1 预览与导出共享语义

特效在编辑预览和最终导出中必须产生一致的视觉效果。允许调度策略和缓存策略的差异，但不允许存在两套不同的特效解释逻辑。

### 6.2 能力显式声明

不要依赖调用方“猜测”插件能做什么。Definition 必须显式声明：

- `EffectColorDomainContract`：精确输入/输出处理域；
- `EffectExecutionContract`：精确 backend/representation mode、determinism、
  state、temporal extent、ROI、resource lifetime 与最大 topology；
- 线性或分支 graph builder，以及需要时的一次性资源 preparer。

这些不是乐观元数据。每次 emitted graph 都会反向校验实际 operation requirements；
默认插件合同不准入任何 backend，也不会因为存在 graph builder 就自动变成“可用”。

### 6.3 参数路径稳定

参数 key 会被序列化到项目文件中。更改参数 key 意味着旧项目打开后该参数会丢失。除非正在进行破坏性升级，否则保持参数 key 不变。

### 6.4 缓存必须有契约

任何缓存行为必须回答四个问题：
- key 是什么
- 是否依赖 frame seed
- 是否依赖外部资源
- 是 deterministic 还是 frame-dependent

详见 [性能与缓存](./04-performance-and-caching.md)。

## 下一步

- [特效开发](./03-effect-development.md) —— 开始编写复杂特效
- [版本契约](./05-versioning-and-compatibility.md) —— 理解版本兼容与降级
