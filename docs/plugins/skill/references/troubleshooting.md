# Troubleshooting Mondrian Plugins

## Effect not appearing in the Effects Library

**Symptom:** Plugin registers successfully, but effect doesn't show up in the UI's effects panel.

**Checklist:**

1. Verify `register_effect_definition()` is actually called at startup
2. Check `effect_library_types()` return value — does it include your EffectType?
3. Check `effect_definition(&your_type)` returns `Some(...)`
4. Check `effect_plugin_is_library_visible(key, contract)` returns `true`
5. Verify API version compatibility: `contract.is_api_compatible()` returns `true`
6. Check runtime status: `effect_plugin_runtime_status(key)` — is `disabled` false?

**Common causes:**
- Plugin crate's `register()` function not called in app init
- Plugin API version incompatible with runtime (major mismatch, or plugin minor > runtime minor)
- Plugin was disabled by a previous runtime failure (if using `DisablePluginDefinition`)
- Degradation policy is `HideFromEffectLibrary` and plugin is not compatible

## Effect applies but no visual change

**Symptom:** Effect is on the clip, parameters are set, but preview shows no difference.

**Checklist:**

1. Check `EffectNode.is_enabled` is `true`
2. Check parameters — zero/edge values may cause the graph builder to return early (identity)
3. Check `graph.is_identity()` on the built graph — true means all ops were skipped
4. Check `EffectRenderPlan.is_identity()` — true means no ops in the plan

**Common causes:**
- Parameters default to values that produce no visual effect (e.g., radius=0, opacity=0)
- Graph builder returns early when parameters are below threshold — check the early-return condition
- Effect's `evaluate_into` path is taken but evaluator is not set up correctly
- For custom render: `params_builder` returns `None`, so the processor is never invoked

## Custom processor not being called

**Symptom:** Custom render backend is configured, but the processor closure never executes.

**Checklist:**

1. Verify `register_custom_render_processor()` was called (done automatically by `with_custom_render_backend`)
2. Check `params_builder` returns `Some(...)` — if it returns `None`, processor is skipped
3. Check `effect_plugin_is_runtime_available()` — disabled plugins skip all execution
4. Check that the `EffectRenderOp::Custom` node appears in the compiled graph

**Common causes:**
- `params_builder` returns `None` because a required property is missing or evaluates to `None`
- Plugin was disabled by a previous failure (check runtime status)
- The effect's `evaluate_render_into` is never called because the graph builder path is preferred

## Performance issues

**Symptom:** Plugin causes frame drops, slow preview, or high memory usage.

**Diagnosis:**

1. Check `CompiledEffectGraph.estimated_cost` — high values mean expensive graph
2. Check `CompiledEffectNodeProfile.estimated_cost` per node to find bottlenecks
3. Check if `node.output_cache_enabled` is `true` for expensive nodes
4. For custom render: check that cache_key is provided and stable

**Fixes:**
- Add `cache_key` for deterministic custom processors that depend on external resources
- Split large custom processors into smaller subtrees (runtime can cache subtrees independently)
- Use `EffectCachePolicy::Deterministic` instead of `FrameDependent` if the effect is truly deterministic
- For linear chains: consider whether ops can be merged or simplified
- Check that early-return (identity when params are zero) is implemented

## Build errors

**Symptom:** Cargo build fails after adding a plugin crate.

**Checklist:**

1. Plugin crate is listed in root `Cargo.toml` `[workspace].members`
2. All path dependencies use correct relative paths (e.g., `path = "../mondrian-core"`)
3. Dependency versions are compatible with the workspace
4. Plugin crate uses `edition = "2021"`
5. `mondrian-app` (or wherever `register()` is called) has the plugin as a dependency

**Common causes:**
- Missing workspace member entry
- Incorrect relative path in `Cargo.toml` (paths are relative to the plugin crate's location)
- Using features that don't exist in the dependency crates
- Version mismatch between workspace and plugin dependencies

## Runtime errors

**Symptom:** Plugin compiles but panics or produces errors at runtime.

**Key guarantees:**
- Panics in evaluator/graph builder/render builder are caught by `catch_unwind`
- Panics in custom processor result in staged buffer being discarded
- Errors are recorded via `record_plugin_runtime_failure()`
- Semi-finished pixels never leak to the output frame

**Debugging:**
1. Check `effect_plugin_runtime_status(key).last_error` for recorded error messages
2. Check if the plugin was disabled: `effect_plugin_runtime_status(key).disabled`
3. For custom processors: test with a minimal buffer first
4. Verify buffer dimensions: `buffer.len() == width * height * 4`

## Preview/Export mismatch

**Symptom:** Effect looks different in preview vs. exported video.

**Possible causes (code issues):**
- Frame-dependent effect declared as `Deterministic`
- Custom processor uses `frame_seed` but cache policy is `Deterministic`
- File paths differ between dev and export environments (use relative or content-hash-based cache keys)

**Possible causes (expected behavior):**
- Preview may use lower resolution for performance — this is normal
- Export uses full resolution — visual difference from resolution change is expected
- Cache TTL may differ between preview and export modes — this is a runtime scheduling difference, not a bug

## Parameter persistence issues

**Symptom:** Parameter values reset or are lost after reopening a project.

**Checklist:**

1. Parameter keys are stable and don't change between plugin versions
2. Property paths use the `plugin.<author>.<name>.<param>` convention
3. Default values are reasonable (project stores only deviations from defaults)

**Common causes:**
- Renamed a parameter key without migration — old projects lose the value
- Changed a parameter type (Float → Int) — old values can't be deserialized
- Changed the plugin key — old projects can't find the effect definition
