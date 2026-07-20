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
| Does it need custom pixel algorithms? | LUT loader, custom filter | `with_custom_render_backend(...)` |
| Is it just parameter evaluation? | legacy, non-visual | `with_evaluator(...)` (avoid for new plugins) |

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
use mondrian_effects::{
    EffectPluginContract, EffectPluginDefinitionBuilder, EffectRenderOp, EffectType,
    register_effect_definition,
};

pub fn register() {
    let plugin_type = EffectType::Plugin("plugin.<author>.<name>".to_string());

    let definition = EffectPluginDefinitionBuilder::new(plugin_type.key(), "Display Name")
        .with_plugin_contract(EffectPluginContract::new("0.1.0"))
        .property(PropertyDescriptor::new(
            "plugin.<author>.<name>.amount",
            "Amount",
            PropertyValue::Float(0.5),
        ))
        .with_graph(|effect, context, graph| {
            let amount = effect.evaluate_f32_by_suffix(
                "plugin.<author>.<name>.amount", context.time, 0.5
            );
            // Chain operations. Each apply() operates on the current output.
            graph.apply(EffectRenderOp::GaussianBlur { radius: amount * 10.0 });
            graph.apply(EffectRenderOp::Sharpen { amount: amount * 0.5 });
        })
        .build();

    register_effect_definition(definition);
}
```

### Pattern B: Branching graph (glow/bloom/soft-focus)

Use when the effect branches from the current output, processes the branch, then blends back.

```rust
.with_branching_graph(|effect, context, graph| {
    let radius = effect.evaluate_f32_by_suffix(
        "plugin.<author>.<name>.radius", context.time, 6.0
    );
    let opacity = effect.evaluate_f32_by_suffix(
        "plugin.<author>.<name>.opacity", context.time, 0.35
    );

    // Early return if parameters produce no visible effect (identity optimization)
    if radius <= 1.0e-4 || opacity <= 1.0e-4 {
        return;
    }

    graph.blend_current(BlendMode::Screen, opacity, |graph, source| {
        graph.apply_to(source, EffectRenderOp::GaussianBlur { radius });
    });
})
```

Available `BlendMode` variants: `Normal`, `Screen`, `Multiply`, `Overlay`, `Add`, `Subtract`.

### Pattern C: Custom render backend

Use when built-in `EffectRenderOp` variants cannot express the pixel algorithm.

```rust
.with_custom_render_backend(
    // 1. Params builder — produce the JSON params for the processor
    Arc::new(|effect, context| {
        let path = effect.evaluate_str_by_suffix(
            "plugin.<author>.<name>.asset_path", context.time, ""
        );
        Ok(Some(serde_json::json!({ "asset_path": path })))
    }),
    // 2. Cache key builder — stable key for caching (None if not cacheable)
    Some(Arc::new(|effect, context| {
        let path = effect.evaluate_str_by_suffix(
            "plugin.<author>.<name>.asset_path", context.time, ""
        );
        Some(format!("asset:{}", path))
    })),
    // 3. Cache policy
    EffectCachePolicy::Deterministic,
    // 4. Pixel processor — called on a staged buffer
    Arc::new(|buffer, width, height, params, frame_seed| {
        // buffer: &mut Vec<u8> — RGBA pixel data, length = width * height * 4
        // Process pixels in-place. Return Ok(()) on success.
        // On Err or panic, staged result is discarded and execution fails.
        Ok(())
    }),
)
```

**Custom processor rules:**
- Process on the provided `buffer` in-place. Do NOT allocate a new full-size buffer.
- `Ok(())` commits the staged result. `Err(...)` or panic discards it.
- The buffer is a separate staged buffer — semi-finished pixels won't leak to the output frame.
- Do NOT do file I/O or network requests inside the processor — load resources beforehand.

### Pattern D: Mask effect

Use when applying an alpha mask generated by a subtree.

```rust
graph.mask_current(false, |graph, source| {
    graph.apply_to(source, EffectRenderOp::Custom {
        key: "plugin.<author>.<name>.mask".to_string(),
        params: serde_json::json!({"shape": "ellipse", "feather": 0.3}),
        cache_key: Some("mask-v1".to_string()),
        cache_policy: EffectCachePolicy::Deterministic,
    });
});
```

Set `mask_current(true, ...)` to invert the mask.

## Step 4: Configure cache policy

Choose the right cache policy for the effect:

| Effect characteristics | Policy | Provide cache_key? |
|----------------------|--------|-------------------|
| Pure built-in ops, no external deps | `Deterministic` (default) | No |
| Depends on external file (LUT, model) | `Deterministic` | Yes — hash of file path/content |
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
- Check `effect_plugin_is_library_visible()` returns true
- Check API version compatibility: `contract.is_api_compatible()`
- Check plugin not disabled: `effect_plugin_runtime_status(key)`

### Effect applies but no visual change
- Check `is_enabled: true` on the EffectNode
- Parameters at zero/default that produce identity (add early-return in graph builder)
- Graph builder returned early due to parameter check

### Custom processor not called
- Check `register_custom_render_processor()` is called (done automatically by builder)
- Verify params_builder returns `Ok(Some(...))`; `Ok(None)` is an intentional identity and `Err` is a structured build failure
- Check `effect_plugin_is_runtime_available()` — disabled plugins skip execution

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
EffectRenderOp::ColorAdjust { exposure: f32, contrast: f32, saturation: f32 }
EffectRenderOp::WhiteBalance { temperature: f32, tint: f32 }
EffectRenderOp::GaussianBlur { radius: f32 }
EffectRenderOp::Sharpen { amount: f32 }
EffectRenderOp::Vignette { intensity: f32, feather: f32 }
EffectRenderOp::ChromaticAberration { amount: f32 }
EffectRenderOp::Grain { amount: f32 }
EffectRenderOp::Custom { key, params, cache_key, cache_policy }
```

## Property types

```rust
PropertyValue::Float(f32)     // Sliders, continuous values
PropertyValue::Int(i32)        // Integer values
PropertyValue::Bool(bool)      // Toggles
PropertyValue::Color(r,g,b,a)  // Color pickers
PropertyValue::String(String)  // Paths, URLs, text
```

Properties are automatically editable in the Inspector panel and can be animated with keyframes.
