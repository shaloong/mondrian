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
// params_builder returns Result<Option<serde_json::Value>, EffectGraphBuildError>
// Ok(None) is intentional identity; missing/invalid state returns Err
// The staged RGBA8 ABI admits EffectExecutionModes::CPU_U8, not CPU_F32/GPU.

// Required execution contract (the conservative default admits no backend)
.with_execution_contract(contract: EffectExecutionContract) -> Self
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
fn mask<F>(&mut self, input: EffectGraphValue, invert: bool, mask_op: MaskOp, build: F) -> EffectGraphValue
fn mask_current<F>(&mut self, invert: bool, mask_op: MaskOp, build: F) -> EffectGraphValue
```

Every branch/blend/mask builder has
`FnOnce(&mut EffectGraphDsl, EffectGraphValue) -> EffectGraphValue`; its final
expression must return the value (no trailing semicolon).

## EffectRenderOp

```rust
ColorAdjust {
    exposure: f32,
    contrast: f32,
    saturation: f32,
    working_color_space: WorkingColorSpace,
}
GaussianBlur { radius: f32 }
Sharpen { amount: f32 }
Vignette { intensity: f32, feather: f32 }
ChromaticAberration { amount: f32 }
Grain { amount: f32 }
TemporalFrameBlend { sample_offset: TimelineTime, mix: f32 }
Lut3D { lut: Arc<PreparedLut3D>, intensity: f32 }
```

White Balance remains modeled-only / execution-unavailable; no executable
WhiteBalance op exists. Custom work is bound only through
`EffectPluginDefinitionBuilder::with_custom_render_backend(...)`; never
construct raw `EffectRenderOp::Custom` or leave its processor unbound.

Methods: `hash_signature(hasher)`, `cache_policy() -> EffectCachePolicy`, `estimated_cost() -> u32`

## EffectCachePolicy

```rust
Deterministic    // Same inputs → same outputs. Safe to cache across frames.
FrameDependent   // Depends on frame seed or temporal noise. Cache includes frame_seed.
Uncacheable      // No cross-call reproducibility; reusable output keys are forbidden.
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
    -> Result<(), mondrian_effects::effect::EffectDefinitionError>
effect_definition(effect_type: &EffectType) -> Option<Arc<EffectDefinition>>
effect_library_types() -> Vec<EffectType>

effect_plugin_runtime_status(key: &str) -> Option<EffectPluginRuntimeStatus>
```

Plugin Contracts are attached through
`EffectDefinition::with_plugin_contract(...)` before the sole Definition
registration. Availability, library visibility, and failure quarantine are
engine-owned projections scoped to that Definition registry revision.

## EffectNode

```rust
EffectNode::new(effect_type: EffectType) -> Self // empty PropertyBag
EffectNode::with_defaults(effect_type: EffectType) -> Self // EffectNodeExt

// Properties
.evaluate_property(path: &str, time: TimelineTime) -> Option<PropertyValue>
.evaluate_parameter(parameter_id: &ParameterId, time: TimelineTime) -> Option<PropertyValue>
.evaluate_f32_parameter(parameter_id: &ParameterId, time: TimelineTime, fallback: f32) -> f32
.set_static_value_by_parameter(parameter_id: &ParameterId, value: PropertyValue) -> Result<()>

// Fields
pub id: EffectId
pub effect_type: EffectType
pub properties: PropertyBag
pub is_enabled: bool
```

## PropertyValue Variants

```rust
PropertyValue::Bool(bool)
PropertyValue::Int(i64)
PropertyValue::Float(f32)
PropertyValue::Double(f64)
PropertyValue::Vec2(glam::Vec2)
PropertyValue::Vec3(glam::Vec3)
PropertyValue::Color(mondrian_core::Color)
PropertyValue::Vec4([f32; 4])
PropertyValue::Enum(String)
PropertyValue::Resource(ParameterResourceReference)
PropertyValue::Text(String)
```

## EffectEvalContext

```rust
EffectEvalContext {
    time: TimelineTime,
    working_color_space: WorkingColorSpace,
}
```

## CustomEffectRenderProcessor

```rust
type CustomEffectRenderProcessor =
    Arc<dyn Fn(&mut Vec<u8>, u32, u32, &serde_json::Value, i64) -> Result<()> + Send + Sync>;
//             buffer     w     h    params                   frame_seed
```
