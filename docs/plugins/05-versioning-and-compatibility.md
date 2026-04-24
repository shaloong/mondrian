# 版本契约与兼容策略

本文说明如何为插件声明版本、处理 API 兼容性、配置运行时失败时的降级行为。

## 1. API 版本

当前运行时使用：

```
CURRENT_EFFECT_PLUGIN_API_VERSION = 1.0
```

插件通过 `EffectPluginContract` 声明自己面向的 API 版本。

### 兼容规则

- `major` 必须一致
- 插件的 `minor` 不得高于当前运行时

示例：
- 运行时 `1.0` 接受插件声明 `1.0` ✓
- 运行时 `1.5` 接受插件声明 `1.0` ✓（插件用旧 API，运行时兼容）
- 运行时 `1.0` 不接受插件声明 `2.0` ✗（major 不匹配）
- 运行时 `1.0` 不接受插件声明 `1.5` ✗（插件用比运行时更新的 minor）

## 2. EffectPluginContract

```rust
pub struct EffectPluginContract {
    pub plugin_version: String,              // 插件自身版本，如 "1.2.0"
    pub api_version: EffectPluginApiVersion,  // 面向的 API 版本
    pub failure_policy: EffectPluginFailurePolicy,
    pub degradation_policy: EffectPluginDegradationPolicy,
}
```

每个插件特效都应显式声明 contract：

```rust
.with_plugin_contract(
    EffectPluginContract::new("1.2.0")
        .with_failure_policy(EffectPluginFailurePolicy::DisablePluginDefinition)
        .with_degradation_policy(EffectPluginDegradationPolicy::HideFromEffectLibrary),
)
```

## 3. 失败策略

### BypassEffect

当前特效实例出错时，本次求值直接退化为 identity（无操作），不影响其他实例。

```rust
EffectPluginFailurePolicy::BypassEffect
```

适用：
- 开发期插件，忍受偶发错误
- 外部资源偶发不可用但不希望影响其他特效实例
- 容错优先的实验性效果

### DisablePluginDefinition

一旦该插件的任一特效发生运行时失败，运行时记录失败并禁用整个 plugin definition。

```rust
EffectPluginFailurePolicy::DisablePluginDefinition
```

适用：
- 商业发行插件，要求 fail-fast
- 不希望在一个会话里持续重复失败
- 失败意味着严重问题，需要用户注意

## 4. 降级策略

### IdentityFallback

不兼容或失败时，特效在求值时自动绕过。项目可以继续打开，该特效暂时失效（画面无变化）。

```rust
EffectPluginDegradationPolicy::IdentityFallback
```

适用：
- 迁移阶段，需要优先保障项目可打开
- 开发期，不希望因一个插件问题阻塞工作流

### HideFromEffectLibrary

当 plugin definition 不兼容或已被运行时禁用时，**新建/添加特效的面板不再暴露它**。

```rust
EffectPluginDegradationPolicy::HideFromEffectLibrary
```

适用：
- 正式交付插件，希望把不兼容版本从作者工作流里明确剔除
- 不希望用户继续在已损坏的插件上浪费时间

## 5. 推荐组合

| 阶段 | failure_policy | degradation_policy |
|------|---------------|-------------------|
| 开发期 / 内部插件 | `BypassEffect` | `IdentityFallback` |
| 测试 / 预发布 | `BypassEffect` | `HideFromEffectLibrary` |
| 稳定商业发布 | `DisablePluginDefinition` | `HideFromEffectLibrary` |

## 6. 错误隔离机制

运行时已提供以下隔离保证：

- `EffectNode` 的 evaluator / render builder / graph builder 都在 **staged state** 上执行
- 自定义渲染处理器在 **staged frame buffer** 上执行
- 只有成功返回 `Ok(())` 时才提交 staged 结果
- panic 和 `Err(...)` 都会被捕获并记录为 plugin runtime failure

这意味着：
- 插件失败**不会污染**正在运行的帧合成主链
- 自定义处理器的半成品像素**不会写回**输出帧
- 预览和导出**共享**同一套降级语义

## 7. 运行时状态查询

可以通过以下函数查询插件运行时状态：

```rust
// 获取运行时状态
let status = effect_plugin_runtime_status("plugin.example.my_effect");
// status.disabled: bool
// status.last_error: Option<String>
// status.api_compatible: bool

// 检查是否可用
let available = effect_plugin_is_runtime_available("plugin.example.my_effect", contract);

// 检查是否在特效库中可见
let visible = effect_plugin_is_library_visible("plugin.example.my_effect", contract);
```

## 下一步

- [最佳实践](./06-best-practices.md) —— 开发建议汇总
- [SDK 参考](./sdk/plugin-contract.md) —— 完整 API 签名
