# Plugin Contract API 参考

Plugin Contract 声明插件版本、API 兼容性、运行时失败后的定义状态，以及不可用定义在效果库中的可见性。它不授权静默旁路失败实例。

**源码位置：** `crates/mondrian-effects/src/plugin_contract.rs`

## EffectPluginApiVersion

```rust
pub struct EffectPluginApiVersion {
    pub major: u16,
    pub minor: u16,
}

impl EffectPluginApiVersion {
    pub const fn new(major: u16, minor: u16) -> Self;
    pub fn is_compatible_with(self, runtime: Self) -> bool;
}

pub const CURRENT_EFFECT_PLUGIN_API_VERSION: EffectPluginApiVersion =
    EffectPluginApiVersion::new(1, 0);
```

`major` 必须相同，插件 `minor` 不得高于运行时。

## EffectPluginRuntimeFailurePolicy

```rust
pub enum EffectPluginRuntimeFailurePolicy {
    KeepDefinitionAvailable,
    DisableDefinition,
}
```

- `KeepDefinitionAvailable`：记录实例失败但允许修复后重试；当前求值仍返回错误。
- `DisableDefinition`：任一运行时失败后，整个定义在当前进程变为 unavailable。

## EffectPluginLibraryPolicy

```rust
pub enum EffectPluginLibraryPolicy {
    KeepVisible,
    HideWhenUnavailable,
}
```

该策略只控制不可用定义能否出现在新插入效果的产品库中。它不删除已持久化实例，也不决定渲染是否旁路。

## EffectPluginContract

```rust
pub struct EffectPluginContract {
    pub plugin_version: String,
    pub api_version: EffectPluginApiVersion,
    pub runtime_failure_policy: EffectPluginRuntimeFailurePolicy,
    pub library_policy: EffectPluginLibraryPolicy,
}

impl EffectPluginContract {
    pub fn new(plugin_version: impl Into<String>) -> Self;
    pub fn with_api_version(self, api_version: EffectPluginApiVersion) -> Self;
    pub fn with_runtime_failure_policy(
        self,
        policy: EffectPluginRuntimeFailurePolicy,
    ) -> Self;
    pub fn with_library_policy(self, policy: EffectPluginLibraryPolicy) -> Self;
    pub fn is_api_compatible(&self) -> bool;
}
```

`new` 默认使用当前 API、`DisableDefinition` 和
`HideWhenUnavailable`，确保未显式选择策略的插件也不会在失败后继续被当作可用定义。开发工具若能明确显示 unavailable/error 状态，可主动改为
`KeepDefinitionAvailable + KeepVisible`。

## EffectPluginRuntimeStatus

```rust
pub struct EffectPluginRuntimeStatus {
    pub definition_registry_revision: u64,
    pub disabled: bool,
    pub last_error: Option<String>,
    pub api_compatible: bool,
}
```

`definition_registry_revision` 指明状态属于哪一次 Definition 注册。相同持久化 key 的
替换 Definition 使用新修订；旧 Program 的迟到失败不会隔离新代。

## 绑定与查询

```rust
EffectDefinition::with_plugin_contract(contract: EffectPluginContract) -> Self;
register_effect_definition(definition: EffectDefinition)
    -> Result<(), EffectDefinitionError>;
pub fn effect_plugin_runtime_status(key: &str) -> Option<EffectPluginRuntimeStatus>;
```

Contract 只能随 Definition 注册。运行时可用性、库可见性和失败记录是 Effects
执行器的内部职责：它们都使用已冻结的 Definition Registry Revision，插件代码不能按
字符串另建或改写 Contract Authority。状态查询也不证明构图、CPU/GPU 执行或
Preview/Export 验证成功。

## 使用示例

```rust
use mondrian_effects::{
    effect_plugin_runtime_status, register_effect_definition,
    EffectPluginContract, EffectPluginLibraryPolicy,
    EffectPluginRuntimeFailurePolicy,
};

register_effect_definition(
    definition.with_plugin_contract(
        EffectPluginContract::new("1.2.0")
        .with_runtime_failure_policy(
            EffectPluginRuntimeFailurePolicy::DisableDefinition,
        )
        .with_library_policy(
            EffectPluginLibraryPolicy::HideWhenUnavailable,
        ),
    ),
)?;

if let Some(status) = effect_plugin_runtime_status("plugin.example.my_effect") {
    if status.disabled {
        eprintln!("插件已被禁用: {:?}", status.last_error);
    }
}
```

## 相关

- [EffectDefinition API](./effect-definition.md)
- [版本契约指南](../05-versioning-and-compatibility.md)
