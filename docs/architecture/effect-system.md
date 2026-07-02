# Effect System

Effects are timeline-instance operations that transform image data through a compiled render graph.

## Data vs Execution

- `mondrian-core::effect_data` defines `EffectType` and `EffectNode` as pure serializable data.
- `mondrian-effects` owns definitions, defaults, graph compilation, execution, caching, and plugin contracts.
- `mondrian-timeline::Clip` stores effect instances and namespaces their properties by `EffectId`.

## EffectNode

An effect instance contains:

- `id: EffectId`
- `effect_type`
- `properties: PropertyBag`
- opaque `params`
- `is_enabled`

When placed on a clip, property paths are prefixed as `effect.<effect_id>.<rest>` to avoid collisions between multiple instances of the same effect type.

## Graph Model

`EffectRenderGraph` supports:

- `Source`
- `UnaryEffect`
- `Blend`
- `Mask`
- `MaskSource`
- deferred `MultiInput`

`CompiledEffectGraph` stores schedule, node use counts, cache policies, estimated cost, subtree signatures, and output cache flags.

## Basic Properties

Built-in transform/speed/blend/solid color are clip properties, not removable user effects. The Inspector may present them in an effect-like stack for consistency, but deleting core transform properties is invalid and should be rejected by domain code.

## Masks, Mattes, Blend Modes, Adjustment Layers

- Masks are clip components that compile into mask graph nodes.
- Blend mode is clip/track compositing state, not a unary color effect.
- Adjustment layers are clips whose effects apply to the accumulated lower image.
- Mattes should be expressed as mask/graph inputs rather than hidden UI-only flags.

## Ordering

Effect stack order affects graph output. UI reorder operations must mutate the clip effect vector, then selection/navigation state may follow without an extra undo entry.

## Optimization

The renderer/effects system may merge deterministic unary ops, cache deterministic subtrees, skip identity graphs, and keep frame-dependent ops such as grain out of cross-frame caches. These optimizations must preserve graph semantics.

## Float/Linear Execution

`mondrian-effects` exposes a typed float/linear execution boundary for the
subset of effect graphs that can operate directly on working-space `f32` RGBA
pixels. `compiled_effect_graph_supports_rgba_f32(...)` and
`apply_compiled_effect_graph_rgba_f32(...)` use the same validation rules:
currently `Source` plus unary `ColorAdjust` and `WhiteBalance` nodes are
supported. The float path must preserve extended scene-linear values and must
not clamp RGB to 0..1 as the legacy RGBA8 path does.

Adjustment-layer passes use `apply_compiled_effect_graph_pass_rgba_f32(...)`
when the graph is float-capable and the requested blend mode is `Normal`. This
keeps ordinary color-correction layers in the same linear working frame instead
of forcing an RGBA8 scratch boundary.

Unsupported graph nodes and render ops return structured
`EffectFloatExecutionError` / `EffectFloatUnsupportedReason` values so renderer
callers can make an explicit legacy fallback decision. Blur, sharpen, vignette,
chromatic aberration, grain, LUT, custom/plugin processors, masks, non-normal
blend modes, and multi-input nodes remain legacy-only until they gain their own
float contract.

File-backed LUT caches key existing files by canonical path and invalidate on
file fingerprint changes. Tests that validate cache behavior should use local
cache instances rather than the process-global cache so workspace-level
parallel test runs remain deterministic.
