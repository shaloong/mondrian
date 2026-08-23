# Troubleshooting Mondrian Plugins

## Effect not appearing in the Effects Library

**Symptom:** Plugin registers successfully, but effect doesn't show up in the UI's effects panel.

**Checklist:**

1. Verify `register_effect_definition()` is actually called at startup
2. Check `effect_library_types()` return value — does it include your EffectType?
3. Check `effect_definition(&your_type)` returns `Some(...)`
4. Check the registered Definition's Contract uses the intended library policy
5. Verify API version compatibility: `contract.is_api_compatible()` returns `true`
6. Check runtime status: `effect_plugin_runtime_status(key)` — is `disabled` false?

**Common causes:**
- Plugin crate's `register()` function not called in app init
- Plugin API version incompatible with runtime (major mismatch, or plugin minor > runtime minor)
- Plugin was disabled by a previous runtime failure (if using `DisableDefinition`)
- Library policy is `HideWhenUnavailable` and plugin is not compatible

## Effect applies but no visual change

**Symptom:** Effect is on the clip, parameters are set, but preview shows no difference.

**Checklist:**

1. Check `EffectNode.is_enabled` is `true`
2. Check parameters — zero/edge values may cause the graph builder to return early (identity)
3. Check `graph.is_identity()` on the built graph — true means all ops were skipped

**Common causes:**
- Parameters default to values that produce no visual effect (e.g., radius=0, opacity=0)
- Graph builder returns early when parameters are below threshold — check the early-return condition
- Definition has no admitted execution mode or the prepared graph failed its contract
- For custom render: `params_builder` returns `Ok(None)`, the definition's explicit identity result

## Custom processor not being called

**Symptom:** Custom render backend is configured, but the processor closure never executes.

**Checklist:**

1. Use `with_custom_render_backend(...)`, the sole supported path that embeds
   the processor directly; an unbound manually constructed raw Custom node is
   intentionally rejected at compilation
2. Check `params_builder` returns `Ok(Some(...))`; `Ok(None)` intentionally skips the processor and `Err` fails graph construction
3. Check `effect_plugin_runtime_status(key)` — a quarantined current Definition generation is rejected before execution
4. Inspect whether the engine-emitted, Definition-bound internal
   `EffectRenderOp::Custom` node appears in the compiled graph; never construct it manually

**Common causes:**
- `params_builder` returns `Ok(None)` because the definition deliberately treats current parameters as identity; missing required state should instead return `EffectGraphBuildError`
- Plugin was disabled by a previous failure (check runtime status)
- The prepared graph was rejected before execution because its Definition contract was too optimistic

## Performance issues

**Symptom:** Plugin causes frame drops, slow preview, or high memory usage.

**Diagnosis:**

1. Check `CompiledEffectGraph::estimated_cost()` — high values mean expensive graph
2. Check `CompiledEffectGraph::node_profiles()` and each profile's `estimated_cost`
3. Check `CompiledEffectGraph::output_cache_enabled()` / profile `output_cache_enabled`
4. For custom render: check that cache_key is provided and stable

**Fixes:**
- For an already prepared immutable external resource, include its exact content/revision identity in `cache_key`; otherwise fail closed
- Express built-in work as Graph DSL nodes so the runtime can reason about subtrees; do not fabricate raw Custom nodes
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
- Panics in Definition preparation and graph builders are caught by `catch_unwind`
- Panics in custom processor result in staged buffer being discarded
- Errors are recorded internally against the immutable Definition generation
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
- An external dependency was not bound to one immutable content/revision identity

**Possible causes (expected behavior):**
- Preview may use lower resolution for performance — this is normal
- Export uses full resolution — visual difference from resolution change is expected
- Preview and Export have separate owner-scoped Sessions and budgets, but cache ownership must not change effect semantics

## Parameter persistence issues

**Symptom:** Parameter values reset or are lost after reopening a project.

**Checklist:**

1. Every product descriptor explicitly binds a stable `ParameterId`
2. Property paths use the `plugin.<author>.<name>.<param>` convention as current UI/authoring aliases
3. Parameter value types and schema revisions remain compatible

**Common causes:**
- Changed `ParameterId` without migration — old projects lose the stable identity
- Changed a parameter type (Float → Int) — old values can't be deserialized
- Changed the plugin key — old projects can't find the effect definition
