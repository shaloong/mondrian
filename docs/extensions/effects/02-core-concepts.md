# 效果扩展核心概念

本文介绍当前源码级 Effect SDK。通用平台正式称为 **Extension**，其资源、
Web/Rust 运行容器、权限和分发规划见 [Extension 架构](../../architecture/extension-system.md)。
源码 SDK 使用 Extension 命名；持久化定义身份单独管理，注册能力不等于第三方安全沙箱。

## 1. 效果扩展模型概览

当前效果能力通过 `EffectDefinition` 注册，包含参数、图构建与执行合同，以及
`EffectExtensionContract`。实现可由源码 crate 集成；这不是通用 Extension 的包定义。
未来 Extension 的通用清单尚未实现。平台不限于 Rust，也不要求包含代码：
语言包可以只提供资源，一个包也可以组合多种贡献。

UI、通用命令、资产源、受限运行和统一分发尚未作为 Extension API 开放。
原生 OpenFX、CLAP/VST3 由各自受监督的 Adapter 加载，不构成源码效果 crate 的动态加载 ABI；
当前范围见[原生格式插件宿主](../../architecture/native-plugin-hosts.md)。

## 2. 能力类型

### 2.1 特效能力（Effect）

当前源码级效果扩展可以注册一到多个 `EffectDefinition`，每个定义描述一个可应用在片段上的视觉处理。

特效能力涉及这些模块：

- `EffectDefinition` —— 特效的静态描述（标识、参数、能力声明）
- `EffectGraphDsl` —— 描述特效在渲染图中的行为
- `EffectRenderOp` —— 内置渲染算子（模糊、混合、蒙版等）
- `CustomEffectRenderProcessor` —— 自定义像素处理逻辑

详见 [特效开发](03-effect-development.md)。

### 2.2 UI 扩展能力（规划中）

未来通用 Extension 可向应用界面贡献以下入口；当前效果 SDK 尚未开放这些接口：

- **面板** —— 在侧边或底部区域注册独立面板
- **菜单项** —— 向主菜单或右键菜单插入功能入口
- **工具栏按钮** —— 在工具栏中增加操作按钮

详见 [UI 扩展](07-ui-extensions.md)。

### 2.3 资产源能力（规划中）

未来通用 Extension 可提供素材源，如云存储或第三方服务；它与效果定义是不同的贡献类型。

## 3. 现有效果定义标识

现有效果定义使用稳定 key，以下是新定义的示例命名空间；这不是未来 Extension 包 ID 的已定格式：

```
extension.<author>.<name>
```

示例：
- `extension.example.hello`
- `extension.acme.color_grading`
- `extension.ai.auto_exposure`

每个效果定义与其参数分别拥有稳定 key。现有效果参数沿用 `{extension_key}.{param_name}` 的示例命名惯例，例如 `extension.ai.auto_exposure.exposure`。

## 4. 注册与发现

效果扩展通过以下机制与运行时交互：

### 4.1 注册

效果扩展在加载时调用注册函数，将定义注册到全局注册表中：

```rust
// Contract 是 Definition 的组成部分；非法 schema/合同必须显式处理
register_effect_definition(definition.with_extension_contract(contract))?;
```

Definition 注册表是唯一的进程级发现 Authority。Contract 不存在可独立改写的第二个
Registry；替换 Definition 会推进同一 Registry Revision，并形成新的运行时隔离代。

### 4.2 发现

运行时通过 key 查询注册表来发现效果扩展：

```rust
// 按 key 查找特效定义
let def = effect_definition(&EffectType::External("extension.example.hello".to_string()));

// 查询效果扩展运行时状态
let status = effect_extension_runtime_status("extension.example.hello");
```

## 5. 生命周期

效果扩展在 Mondrian 中的生命周期：

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
- **源码集成示例在初始化时注册**；原生格式宿主还有独立的发现与准入路径
- **同 key 替换是新注册代**，不是运行时身份不变的幂等操作
- **失败是显式结果**：不提交部分图或像素，也不把失败静默降级为 identity；
  Contract 只决定定义的后续可用性与效果库可见性，不提供 OS 安全隔离
- **隔离绑定定义代**：DisableDefinition 按效果 key 与 Registry Revision 隔离失败定义；重新注册形成新代，旧代迟到失败不会禁用新代

## 6. 核心约束

理解这几点可以避免大多数设计错误：

### 6.1 预览与导出共享语义

特效在编辑预览和最终导出中必须产生一致的视觉效果。允许调度策略和缓存策略的差异，但不允许存在两套不同的特效解释逻辑。

### 6.2 能力显式声明

不要依赖调用方“猜测”效果扩展能做什么。Definition 必须显式声明：

- `EffectColorDomainContract`：精确输入/输出处理域；
- `EffectExecutionContract`：精确 backend/representation mode、determinism、
  state、temporal extent、ROI、resource lifetime 与最大 topology；
- 线性或分支 graph builder，以及需要时的一次性资源 preparer。

这些不是乐观元数据。每次 emitted graph 都会反向校验实际 operation requirements；
默认效果扩展合同不准入任何 backend，也不会因为存在 graph builder 就自动变成“可用”。

### 6.3 参数路径稳定

参数 key 会被序列化到项目文件中。更改参数 key 意味着旧项目打开后该参数会丢失。除非正在进行破坏性升级，否则保持参数 key 不变。

### 6.4 缓存必须有契约

任何缓存行为必须回答四个问题：
- key 是什么
- 是否依赖 frame seed
- 是否依赖外部资源
- 是 deterministic 还是 frame-dependent

详见 [性能与缓存](04-performance-and-caching.md)。

## 下一步

- [特效开发](03-effect-development.md) —— 开始编写复杂特效
- [版本契约](05-versioning-and-compatibility.md) —— 理解版本兼容与降级
