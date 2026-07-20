# 版本契约、失败与可见性

插件版本兼容、实例执行失败、定义会话状态和效果库可见性是四个不同问题。插件失败不得静默变成 identity；项目可以保留并检查作者意图，但预览/导出必须报告失败，直到用户修复、安装兼容插件或显式禁用该实例。

## 1. API 版本

当前运行时使用 `CURRENT_EFFECT_PLUGIN_API_VERSION = 1.0`。

- `major` 必须一致。
- 插件声明的 `minor` 不得高于运行时。
- 不兼容定义不可执行；已持久化实例不会被删除或改写。

## 2. EffectPluginContract

```rust
pub struct EffectPluginContract {
    pub plugin_version: String,
    pub api_version: EffectPluginApiVersion,
    pub runtime_failure_policy: EffectPluginRuntimeFailurePolicy,
    pub library_policy: EffectPluginLibraryPolicy,
}
```

推荐的正式定义：

```rust
.with_plugin_contract(
    EffectPluginContract::new("1.2.0")
        .with_runtime_failure_policy(
            EffectPluginRuntimeFailurePolicy::DisableDefinition,
        )
        .with_library_policy(
            EffectPluginLibraryPolicy::HideWhenUnavailable,
        ),
)
```

## 3. 运行时失败策略

`KeepDefinitionAvailable` 记录当前实例错误，但保留定义，便于开发、修复资源后重试或检查其他实例。失败的求值仍返回结构化错误，不会输出无变化画面冒充成功。

`DisableDefinition` 在任一实例发生运行时失败后禁用该定义，阻止同一会话反复调用已知不可靠代码。已放置实例仍保留在项目中并报告运行时不可用。

该策略只决定一次失败后定义是否继续可调用，不决定帧是否允许旁路。

## 4. 效果库策略

`KeepVisible` 允许不可用定义继续显示在效果库中，主要用于开发和诊断。UI 必须同时显示不可用状态，且不得允许其以“可正常添加”样式出现。

`HideWhenUnavailable` 从新建/添加效果的库中隐藏 API 不兼容或已禁用定义。它不删除时间线中已有实例，也不改变保存的作者状态。

该策略只控制新插入 UI 的可见性，不是渲染降级策略。

## 5. 推荐组合

| 使用阶段 | runtime_failure_policy | library_policy |
| --- | --- | --- |
| 开发/诊断 | `KeepDefinitionAvailable` | `KeepVisible` |
| 预发布/正式交付 | `DisableDefinition` | `HideWhenUnavailable` |

## 6. 错误隔离与传播

- graph builder 在 staged graph 上执行，成功返回后才提交；`try_with_graph` / `try_with_branching_graph` 可报告资源和定义级错误。
- 自定义 RGBA8 processor 在 staged frame buffer 上执行，只有 `Ok(())` 才提交像素。
- builder/processor 的 `Err`、panic、缺失注册、API 不兼容和定义禁用均产生结构化错误。
- `DisableDefinition` 可在记录失败时旋转定义到 unavailable；另一实例不得继续执行旧状态。
- 预览与导出共享同一构图和执行错误语义。调用层可以停止、提示修复或让用户显式禁用实例，但不能把日志警告当成成功。

## 7. 运行时状态查询

```rust
let status = effect_plugin_runtime_status("plugin.example.my_effect");
// status.disabled: bool
// status.last_error: Option<String>
// status.api_compatible: bool

let available = effect_plugin_is_runtime_available(
    "plugin.example.my_effect",
    contract,
);
let visible = effect_plugin_is_library_visible(
    "plugin.example.my_effect",
    contract,
);
```

`available` 与 `visible` 不是执行成功证据；完整能力仍须经过构图、具体 backend 与 preview/export 验证。

## 下一步

- [最佳实践](./06-best-practices.md)
- [SDK 参考](./sdk/plugin-contract.md)
