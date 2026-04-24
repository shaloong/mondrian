# Plugin Contract API 参考

## 概述

Plugin Contract 定义了插件的版本信息、API 兼容性声明、失败策略和降级策略。运行时使用这些信息来做版本检查、错误隔离和降级决策。

**源码位置：** `crates/mondrian-effects/src/plugin_contract.rs`

## EffectPluginApiVersion

```rust
pub struct EffectPluginApiVersion {
    pub major: u16,
    pub minor: u16,
}
```

### 方法

```rust
impl EffectPluginApiVersion {
    pub const fn new(major: u16, minor: u16) -> Self;

    /// 检查插件声明的 API 版本是否与运行时兼容
    /// 规则：major 必须一致，插件的 minor 不得高于运行时
    pub fn is_compatible_with(self, runtime: Self) -> bool;
}
```

### 当前版本

```rust
pub const CURRENT_EFFECT_PLUGIN_API_VERSION: EffectPluginApiVersion =
    EffectPluginApiVersion::new(1, 0);
```

兼容性示例：
- 插件 `1.0` + 运行时 `1.0` → 兼容
- 插件 `1.0` + 运行时 `1.5` → 兼容（插件用旧 API）
- 插件 `2.0` + 运行时 `1.0` → 不兼容（major 不匹配）
- 插件 `1.5` + 运行时 `1.0` → 不兼容（minor 高于运行时）

## EffectPluginFailurePolicy

```rust
pub enum EffectPluginFailurePolicy {
    /// 当前实例失败时，仅本次求值降级为 identity
    BypassEffect,

    /// 一旦任何实例失败，禁用整个 plugin definition
    DisablePluginDefinition,
}
```

## EffectPluginDegradationPolicy

```rust
pub enum EffectPluginDegradationPolicy {
    /// 不兼容或失败时，特效自动绕过（identity）
    IdentityFallback,

    /// 不兼容或被禁用时，从特效库面板中隐藏
    HideFromEffectLibrary,
}
```

## EffectPluginContract

```rust
pub struct EffectPluginContract {
    pub plugin_version: String,              // 插件自身版本，如 "1.2.0"
    pub api_version: EffectPluginApiVersion,  // 面向的 API 版本
    pub failure_policy: EffectPluginFailurePolicy,
    pub degradation_policy: EffectPluginDegradationPolicy,
}
```

### 构造函数

```rust
impl EffectPluginContract {
    /// 创建契约，默认值：
    /// - api_version: CURRENT_EFFECT_PLUGIN_API_VERSION
    /// - failure_policy: BypassEffect
    /// - degradation_policy: IdentityFallback
    pub fn new(plugin_version: impl Into<String>) -> Self;
}
```

### Builder 方法

```rust
impl EffectPluginContract {
    /// 指定 API 版本（默认使用当前版本）
    pub fn with_api_version(mut self, api_version: EffectPluginApiVersion) -> Self;

    /// 设置失败策略
    pub fn with_failure_policy(mut self, failure_policy: EffectPluginFailurePolicy) -> Self;

    /// 设置降级策略
    pub fn with_degradation_policy(
        mut self,
        degradation_policy: EffectPluginDegradationPolicy,
    ) -> Self;

    /// 检查是否与当前运行时兼容
    pub fn is_api_compatible(&self) -> bool;
}
```

## EffectPluginRuntimeStatus

```rust
pub struct EffectPluginRuntimeStatus {
    pub disabled: bool,
    pub last_error: Option<String>,
    pub api_compatible: bool,
}
```

运行时状态的只读快照。

## 注册函数

```rust
/// 注册插件契约到全局注册表
pub fn register_plugin_contract(key: impl Into<String>, contract: EffectPluginContract);

/// 按 key 查询插件契约
pub fn plugin_contract(key: &str) -> Option<EffectPluginContract>;
```

## 运行时状态查询

```rust
/// 获取插件运行时状态快照
pub fn effect_plugin_runtime_status(key: &str) -> Option<EffectPluginRuntimeStatus>;

/// 检查插件在运行时是否可用
/// 返回 true 的条件：API 兼容 且 未被禁用
pub fn effect_plugin_is_runtime_available(
    key: &str,
    contract: Option<&EffectPluginContract>,
) -> bool;

/// 检查插件是否应在特效库中可见
/// 返回 true 的条件：运行时可用 或 降级策略不是 HideFromEffectLibrary
pub fn effect_plugin_is_library_visible(
    key: &str,
    contract: Option<&EffectPluginContract>,
) -> bool;

/// 记录插件运行时失败
/// 根据 failure_policy 决定是否禁用插件
pub fn record_plugin_runtime_failure(
    key: &str,
    contract: Option<&EffectPluginContract>,
    reason: impl Into<String>,
);
```

## 使用示例

```rust
use mondrian_effects::{
    EffectPluginContract, EffectPluginFailurePolicy, EffectPluginDegradationPolicy,
    EffectPluginApiVersion,
    register_plugin_contract, effect_plugin_runtime_status,
};

// 注册契约
register_plugin_contract(
    "plugin.example.my_effect",
    EffectPluginContract::new("1.2.0")
        .with_failure_policy(EffectPluginFailurePolicy::DisablePluginDefinition)
        .with_degradation_policy(EffectPluginDegradationPolicy::HideFromEffectLibrary),
);

// 查询状态
if let Some(status) = effect_plugin_runtime_status("plugin.example.my_effect") {
    if status.disabled {
        eprintln!("插件已被禁用: {:?}", status.last_error);
    }
    if !status.api_compatible {
        eprintln!("插件 API 版本不兼容");
    }
}
```

## 推荐策略组合

| 阶段 | failure_policy | degradation_policy |
|------|---------------|-------------------|
| 开发期 | `BypassEffect` | `IdentityFallback` |
| 测试/预发布 | `BypassEffect` | `HideFromEffectLibrary` |
| 稳定发布 | `DisablePluginDefinition` | `HideFromEffectLibrary` |

## 相关

- [EffectDefinition API](./effect-definition.md) —— 在定义中设置契约
- [版本契约指南](../05-versioning-and-compatibility.md) —— 概念讲解
