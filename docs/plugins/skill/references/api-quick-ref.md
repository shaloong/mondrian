# API Quick Reference

Condensed API reference for the mondrian-effects crate. For complete docs, see `docs/plugins/sdk/` in the Mondrian repo.

## EffectPluginDefinitionBuilder

```rust
EffectPluginDefinitionBuilder::new(
    key: impl Into<String>,
    display_name: impl Into<String>,
    color_domain_contract: EffectColorDomainContract,
) -> Self

// Properties
.property(descriptor: PropertyDescriptor) -> Self
.properties(properties: PropertyBag) -> Self

// Graph builders (choose ONE)
.with_graph(build: F) -> Self              // Linear chain of ops
.with_branching_graph(build: F) -> Self    // Branch/blend/mask support
.try_with_graph(build: F) -> Self          // Fallible linear builder
.try_with_branching_graph(build: F) -> Self // Fallible branching builder
// where F: for<'a> Fn(&EffectNode, EffectEvalContext, &mut EffectGraphDsl<'a>) + Send + Sync + 'static
// try_* builders return Result<(), EffectGraphBuildError>

// Custom render (advanced)
.with_custom_render_backend(params_builder, cache_key_builder, cache_policy, processor) -> Self
.with_custom_render_processor(params_builder, processor) -> Self  // Simplified, no explicit cache_key
// params_builder returns Result<Option<serde_json::Value>, EffectGraphBuildError>
// Ok(None) is intentional identity; missing/invalid state returns Err

// Contract
.with_plugin_contract(contract: EffectPluginContract) -> Self

// Finalize
.build() -> EffectDefinition
```

## EffectGraphDsl

```rust
// Input/output
fn source(&self) -> EffectGraphValue
fn current(&self) -> EffectGraphValue
fn set_output(&mut self, value: EffectGraphValue)

// Unary ops
fn apply(&mut self, op: EffectRenderOp) -> EffectGraphValue
fn apply_to(&mut self, input: EffectGraphValue, op: EffectRenderOp) -> EffectGraphValue

// Branching
fn branch<F>(&mut self, input: EffectGraphValue, build: F) -> EffectGraphValue
fn blend<F>(&mut self, base: EffectGraphValue, blend_mode: BlendMode, opacity: f32, build: F) -> EffectGraphValue
fn blend_current<F>(&mut self, blend_mode: BlendMode, opacity: f32, build: F) -> EffectGraphValue
fn mask<F>(&mut self, input: EffectGraphValue, invert: bool, build: F) -> EffectGraphValue
fn mask_current<F>(&mut self, invert: bool, build: F) -> EffectGraphValue
```

## EffectRenderOp

```rust
ColorAdjust { exposure: f32, contrast: f32, saturation: f32 }
WhiteBalance { temperature: f32, tint: f32 }
GaussianBlur { radius: f32 }
Sharpen { amount: f32 }
Vignette { intensity: f32, feather: f32 }
ChromaticAberration { amount: f32 }
Grain { amount: f32 }
Custom { key: String, params: serde_json::Value, cache_key: Option<String>, cache_policy: EffectCachePolicy }
```

Methods: `hash_signature(hasher)`, `cache_policy() -> EffectCachePolicy`, `estimated_cost() -> u32`

## EffectCachePolicy

```rust
Deterministic    // Same inputs → same outputs. Safe to cache across frames.
FrameDependent   // Depends on frame seed or temporal noise. Cache includes frame_seed.
```

## EffectPluginContract

```rust
EffectPluginContract::new(plugin_version: impl Into<String>) -> Self
    .with_api_version(api_version: EffectPluginApiVersion) -> Self
    .with_runtime_failure_policy(policy: EffectPluginRuntimeFailurePolicy) -> Self
    .with_library_policy(policy: EffectPluginLibraryPolicy) -> Self
    .is_api_compatible() -> bool
```

## EffectPluginRuntimeFailurePolicy

```rust
KeepDefinitionAvailable  // Report failure; definition remains available for repair/retry.
DisableDefinition        // Any failure disables the definition for this process.
```

## EffectPluginLibraryPolicy

```rust
KeepVisible             // Keep unavailable definition visible for inspection/development.
HideWhenUnavailable     // Hide unavailable definition from new-insertion UI.
```

Neither policy permits a failed effect to render as identity. Graph and custom
processor failures remain structured execution errors.

## Registration Functions

```rust
register_effect_definition(definition: EffectDefinition)
effect_definition(effect_type: &EffectType) -> Option<Arc<EffectDefinition>>
effect_library_types() -> Vec<EffectType>

register_plugin_contract(key: impl Into<String>, contract: EffectPluginContract)
plugin_contract(key: &str) -> Option<EffectPluginContract>

effect_plugin_runtime_status(key: &str) -> Option<EffectPluginRuntimeStatus>
effect_plugin_is_runtime_available(key: &str, contract: Option<&EffectPluginContract>) -> bool
effect_plugin_is_library_visible(key: &str, contract: Option<&EffectPluginContract>) -> bool
record_plugin_runtime_failure(key: &str, contract: Option<&EffectPluginContract>, reason: impl Into<String>)
```

## EffectNode

```rust
EffectNode::new(effect_type: EffectType) -> Self

// Properties
.evaluate_property(path: &str, time: TimeCode) -> Option<PropertyValue>
.evaluate_f32_by_suffix(suffix: &str, time: TimeCode, fallback: f32) -> f32
.set_static_value_by_suffix(suffix: &str, value: PropertyValue) -> Result<()>

// Fields
pub id: EffectId
pub effect_type: EffectType
pub properties: PropertyBag
pub is_enabled: bool
```

## Common PropertyValue Variants

```rust
PropertyValue::Float(f32)
PropertyValue::Int(i32)
PropertyValue::Bool(bool)
PropertyValue::Color(f32, f32, f32, f32)
PropertyValue::String(String)
```

## EffectEvalContext

```rust
EffectEvalContext { time: TimeCode }
```

## CustomEffectRenderProcessor

```rust
type CustomEffectRenderProcessor =
    Arc<dyn Fn(&mut Vec<u8>, u32, u32, &serde_json::Value, i64) -> Result<()> + Send + Sync>;
//             buffer     w     h    params                   frame_seed
```
