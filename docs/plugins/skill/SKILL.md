---
name: mondrian-plugin-dev
description: >-
  Build, scaffold, and debug Mondrian video editor plugins. Use this skill
  whenever the user wants to create a new Mondrian plugin, add a visual effect,
  write a custom render processor, set up plugin versioning/caching, or debug
  plugin registration and rendering issues. Also use when the user mentions
  "Mondrian plugin", "effect plugin", "custom effect", "Mondrian SDK", or asks
  about extending Mondrian with new visual processing capabilities. The skill
  understands the mondrian-effects crate API, EffectGraphDsl, custom render
  backends, cache policies, plugin contracts, and the monorepo crate structure.
---

# Mondrian Plugin Development

Develop Mondrian plugins by understanding what the developer wants to build,
choosing the right approach, and generating code that follows the conventions
in `docs/plugins/`.

## Quick links

- Full handbook: `docs/plugins/README.md` (in the Mondrian repo)
- API references: `docs/plugins/sdk/` — EffectDefinition, GraphDsl, Contract, Runtime
- Source code: `crates/mondrian-effects/src/` — effect.rs, plugin_sdk.rs, graph.rs, execution.rs, plugin_contract.rs

## Workflow

When a user asks to build a Mondrian plugin, follow this sequence:

1. **Assess** — what type of plugin/effect? (linear graph, branching graph, custom render)
2. **Scaffold** — create the crate if needed, wire up Cargo.toml and registration
3. **Implement** — write the effect definition using the appropriate builder pattern
4. **Configure** — set cache policy, version contract, failure/degradation policy
5. **Register** — ensure the plugin's `register()` function is called at app init
6. **Verify** — check the plugin appears in the effects library and renders correctly

## Step 1: Assess the effect type

Ask these questions to determine the right approach:

| Question | Options | Builder method |
|----------|---------|---------------|
| Can it be expressed with built-in ops? | blur, sharpen, color, vignette | `with_graph(...)` |
| Does it branch/blend/mask from current output? | glow, bloom, soft focus | `with_branching_graph(...)` |
| Does it need custom encoded CPU pixel algorithms? | custom RGBA8 filter | `with_custom_render_backend(...)` |

**Rule: prefer the highest-level API that can express the effect.**
Built-in ops > graph DSL > custom render backend.

## Step 2: Scaffold a plugin crate

For new plugins, create the crate structure:

```
crates/mondrian-plugin-<name>/
├── Cargo.toml
└── src/
    └── lib.rs
```

**Cargo.toml template:**

```toml
[package]
name = "mondrian-plugin-<name>"
version = "0.1.0"
edition = "2021"

[dependencies]
mondrian-core = { path = "../mondrian-core" }
mondrian-effects = { path = "../mondrian-effects" }
serde_json = "1"
```

**Add to workspace:** In root `Cargo.toml`, add `"crates/mondrian-plugin-<name>"` to the `[workspace].members` array.

**lib.rs template:**

```rust
use mondrian_effects::{EffectType, register_effect_definition};

pub fn register() {
    // Register effect definitions here
}
```

**Wire into the app:** In `crates/mondrian-app`, add the plugin crate as a dependency and call `mondrian_plugin_<name>::register();` during app initialization.

## Step 3: Implement the effect

### Pattern A: Linear graph (simple chain of built-in ops)

Use when the effect is a sequence of built-in operations.

```rust
use mondrian_core::automation::{PropertyDescriptor, PropertyValue};
use mondrian_effects::effect::EffectDefinitionError;
use mondrian_effects::{
    register_effect_definition, EffectColorDomainContract, EffectDeterminism,
    EffectExecutionContract, EffectExecutionModes, EffectGraphTopology, EffectPluginContract,
    EffectPluginDefinitionBuilder, EffectRenderOp, EffectResourceLifetime,
    EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent, EffectType,
};

pub fn register() -> Result<(), EffectDefinitionError> {
    let plugin_type = EffectType::Plugin("plugin.<author>.<name>".to_string());
    let amount_id = plugin_type
        .parameter_id("amount")
        .expect("static parameter ID");

    let definition = EffectPluginDefinitionBuilder::new(
        plugin_type.key(),
        "Display Name",
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
            "plugin.<author>.<name>.amount",
            "Amount",
            PropertyValue::Float(0.5),
        ).with_parameter_id(amount_id.clone()))
        .with_graph(move |effect, context, graph| {
            let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 0.5);
            // Chain operations. Each apply() operates on the current output.
            graph.apply(EffectRenderOp::GaussianBlur { radius: amount * 10.0 });
            graph.apply(EffectRenderOp::Sharpen { amount: amount * 0.5 });
        })
        .build();

    register_effect_definition(definition)
}
```

### Pattern B: Branching graph (glow/bloom/soft-focus)

Use when the effect branches from the current output, processes the branch, then blends back.

```rust
.with_branching_graph(move |effect, context, graph| {
    let radius = effect.evaluate_f32_parameter(&radius_id, context.time, 6.0);
    let opacity = effect.evaluate_f32_parameter(&opacity_id, context.time, 0.35);

    // Early return if parameters produce no visible effect (identity optimization)
    if radius <= 1.0e-4 || opacity <= 1.0e-4 {
        return;
    }

    graph.blend_current(BlendMode::Screen, opacity, |graph, source| {
        graph.apply_to(source, EffectRenderOp::GaussianBlur { radius })
    });
})
```

`radius_id` / `opacity_id` 必须由 `plugin_type.parameter_id(...)` 创建，并分别绑定到
`PropertyDescriptor::with_parameter_id(...)` 后再由 `move` 闭包捕获。

Available `BlendMode` variants include `Normal`, `Screen`, `Multiply`, `Overlay`,
`LinearDodge`, and `Subtract`; consult `mondrian_core::BlendMode` for the complete set.

### Pattern C: Custom render backend

Use when built-in `EffectRenderOp` variants cannot express the pixel algorithm.

```rust
let amount_id = plugin_type
    .parameter_id("amount")
    .expect("static parameter ID");

let builder = builder.with_custom_render_backend(
    // 1. Params builder — produce the JSON params for the processor
    Arc::new(move |effect, context| {
        let amount = effect.evaluate_f32_parameter(&amount_id, context.time, 1.0);
        Ok(Some(serde_json::json!({ "amount": amount })))
    }),
    // 2. No external resource identity is needed for this processor
    None,
    // 3. Cache policy
    EffectCachePolicy::Deterministic,
    // 4. Pixel processor — called on a staged buffer
    Arc::new(|buffer, width, height, params, frame_seed| {
        // buffer: &mut Vec<u8> — RGBA pixel data, length = width * height * 4
        // Process pixels in-place. Return Ok(()) on success.
        // On Err or panic, staged result is discarded and execution fails.
        Ok(())
    }),
);
```

**Custom processor rules:**
- Declare `EffectExecutionModes::CPU_U8`; this ABI does not implement CPU
  Float32 or GPU modes.
- Process on the provided `buffer` in-place. Do NOT allocate a new full-size buffer.
- `Ok(())` commits the staged result. `Err(...)` or panic discards it.
- The buffer is a separate staged buffer — semi-finished pixels won't leak to the output frame.
- Do NOT do file I/O or network requests inside the processor — load resources beforehand.
- Graph builders and cache-key builders have the same no-I/O rule.
- The high-level SDK does not yet expose author-selected external-resource
  preparation/revalidation. Fail closed for those instances instead of using a
  path-only key or loading on the frame path.

### Pattern D: Mask effect

`mask(...)` and `mask_current(...)` require both an explicit `MaskOp` and a
subtree whose output domain is `EffectColorDomain::AlphaMask`. Their closures
must return `EffectGraphValue` as the final expression. The current high-level
plugin DSL does not expose `MaskSource` or an RGB-to-matte producer, so do not
construct a raw `EffectRenderOp::Custom` to fake one; an unbound processor or
non-alpha mask domain is rejected. Clip masks are injected by the engine.

## Step 4: Configure cache policy

Choose the right cache policy for the effect:

| Effect characteristics | Policy | Provide cache_key? |
|----------------------|--------|-------------------|
| Pure built-in ops, no external deps | `Deterministic` (default) | No |
| Uses an already prepared immutable LUT/model | `Deterministic` | Yes — exact content/revision identity |
| Contains randomness, noise, time variation | `FrameDependent` | Depends |
| Custom processor with stable inputs | `Deterministic` | Yes — identify the resource |

**Critical:** Never mark a frame-dependent effect as `Deterministic` — it will cause incorrect frame reuse.

## Step 5: Configure version contract

Every plugin effect should declare a contract:

```rust
.with_plugin_contract(
    EffectPluginContract::new("0.1.0")  // plugin version (semver)
        .with_runtime_failure_policy(EffectPluginRuntimeFailurePolicy::KeepDefinitionAvailable)
        .with_library_policy(EffectPluginLibraryPolicy::KeepVisible),
)
```

| Stage | runtime_failure_policy | library_policy |
|-------|------------------------|----------------|
| Development | `KeepDefinitionAvailable` | `KeepVisible` |
| Pre-release / stable | `DisableDefinition` | `HideWhenUnavailable` |

These policies never authorize identity output after failure. Builders and
custom processors must return a structured error; preview/export callers stop
until the instance is repaired or explicitly disabled.

## Step 6: Register and verify

1. Plugin crate's `register()` is called at app startup
2. Effect appears in the Effects Library panel
3. Drag onto a clip → parameters appear in Inspector
4. Preview and export produce identical visual results

## Common debugging scenarios

### Effect not appearing in library
- Check `register_effect_definition()` is called
- Check the registered Definition's Contract uses the intended library policy
- Check API version compatibility: `contract.is_api_compatible()`
- Check plugin not disabled: `effect_plugin_runtime_status(key)`

### Effect applies but no visual change
- Check `is_enabled: true` on the EffectNode
- Parameters at zero/default that produce identity (add early-return in graph builder)
- Graph builder returned early due to parameter check

### Custom processor not called
- Use `with_custom_render_backend(...)`; Definition evaluation is the sole
  supported path that embeds an immutable processor binding
- A manually constructed raw Custom node without a binding is intentionally
  rejected at compilation; there is no ambient compatibility registry
- Verify params_builder returns `Ok(Some(...))`; `Ok(None)` is an intentional identity and `Err` is a structured build failure
- Check `effect_plugin_runtime_status(key)` — a quarantined current Definition generation is rejected before execution

### Performance issues
- Add `cache_key` for deterministic effects that depend on external resources
- Split expensive effects into smaller subtrees (runtime can cache subtrees independently)
- Check `estimated_cost()` on EffectRenderOp — costs above 4 trigger more aggressive caching

### Build errors
- Plugin crate must be in workspace `members`
- mondrian-app must declare dependency on the plugin crate
- All dependencies must use compatible versions (check root Cargo.toml)

## Plugin key conventions

- Effect type: `EffectType::Plugin("plugin.<author>.<name>")`
- Property paths: `"plugin.<author>.<name>.<param_name>"`
- Custom render key: `"plugin.<author>.<name>"` (matches effect key)
- Cache keys: stable, deterministic strings like `"lut:sha256:<hash>"` or `"kernel:v2"`

## When to read full docs

This skill covers the most common patterns. Read the full documentation for:

- Complete EffectGraphDsl method reference → `docs/plugins/sdk/effect-graph-dsl.md`
- Full EffectDefinition API → `docs/plugins/sdk/effect-definition.md`
- Runtime execution and cache internals → `docs/plugins/sdk/runtime.md`
- In-depth caching strategies → `docs/plugins/04-performance-and-caching.md`
- Version compatibility rules → `docs/plugins/05-versioning-and-compatibility.md`
- Best practices and anti-patterns → `docs/plugins/06-best-practices.md`

## Available EffectRenderOp variants

Built-in ops that can be used in `graph.apply()` and `graph.apply_to()`:

```rust
EffectRenderOp::ColorAdjust {
    exposure: f32,
    contrast: f32,
    saturation: f32,
    working_color_space: WorkingColorSpace,
}
EffectRenderOp::GaussianBlur { radius: f32 }
EffectRenderOp::Sharpen { amount: f32 }
EffectRenderOp::Vignette { intensity: f32, feather: f32 }
EffectRenderOp::ChromaticAberration { amount: f32 }
EffectRenderOp::Grain { amount: f32 }
EffectRenderOp::TemporalFrameMix { past_offset: TimelineTime, mix: f32 }
EffectRenderOp::Lut3D { lut: Arc<PreparedLut3D>, intensity: f32 }
```

Write `ColorAdjust` as
`EffectRenderOp::ColorAdjust { exposure, contrast, saturation,
working_color_space: context.working_color_space }`. White Balance is currently
modeled-only and execution-unavailable; there is no executable WhiteBalance
render op. Bind custom CPU RGBA8 work only with
`EffectPluginDefinitionBuilder::with_custom_render_backend(...)`.

## Property types

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

Validated schemas drive Inspector editing. `Resource` and `Text` are not
animatable; other variants follow their schema's allowed interpolation (for
example Bool/Int/Enum use hold semantics).
