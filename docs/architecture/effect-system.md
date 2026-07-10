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
- ordered `MultiInput`

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
Clips with no enabled effects or masks must reuse the process-wide compiled
identity graph. Plain media and solid clips dominate preview/export playback,
so render-plan evaluation should not rebuild a source-only graph or take the
compiled-effect-graph LRU mutex every frame just to represent the identity path.

## Float/Linear Execution

`mondrian-effects` exposes a typed float/linear execution boundary for effect
graphs that can operate directly on working-space `f32` RGBA pixels.
`compiled_effect_graph_supports_rgba_f32(...)` and
`apply_compiled_effect_graph_rgba_f32(...)` use the same validation rules. All
existing built-in unary render operations (`ColorAdjust`, `WhiteBalance`,
`GaussianBlur`, `Sharpen`, `Vignette`, `ChromaticAberration`, `Grain`, and
`Lut3D`) execute in this domain. The float path preserves extended scene-linear
values and does not clamp RGB to 0..1 as the legacy RGBA8 path does.

Spatial operations sample straight-alpha input through premultiplied-alpha
intermediates so transparent pixels cannot contaminate visible colors. Grain
uses the graph frame seed, and LUT intensity blends back to the unbounded float
source after normalized LUT sampling. These rules are shared by media, solid,
and adjustment-layer execution.

`Blend`, `Mask`, `MaskSource`, and ordered `MultiInput` nodes use the same float
working-frame contract. Mask rasterization produces native float coverage rather
than quantizing through an 8-bit matte. The DAG executor transfers owned buffers
according to compiled node use counts, clones only for concurrently live branch
consumers, and recycles consumed buffers. Dissolve combines the frame seed with
the pixel index and is therefore compiled with a frame-dependent cache policy.
Its opacity is a stochastic gate; accepted pixels perform a full source-over so
opacity is not multiplied into alpha a second time.

Adjustment-layer passes use `apply_compiled_effect_graph_pass_rgba_f32(...)`
when the graph is float-capable. This keeps ordinary color-correction layers in
the same linear working frame instead of forcing an RGBA8 scratch boundary.
Media, solid-color, and float-capable adjustment timeline layers use
`blend_rgba_f32_pixel_seeded(...)` for built-in blend modes while remaining in
the float/linear compositor. Dissolve uses the same stable frame/pixel seed
contract as the legacy RGBA8 path so preview and export make identical dither
decisions.

Float/linear graph execution has its own bounded output cache keyed by compiled
graph signature, typed float input signature, dimensions, and frame seed when a
graph is frame-dependent. Deterministic multi-op color-correction chains should
reuse this cache rather than forcing repeated full-frame float adjustment work
during preview scrubbing or export retries.

Unsupported graph nodes and render ops return structured
`EffectFloatExecutionError` / `EffectFloatUnsupportedReason` values so renderer
callers can make an explicit legacy fallback decision. Custom/plugin processors
remain unsupported until their ABI declares a float implementation. CPU float
support does not imply GPU execution support.

`lower_effect_graph_to_gpu_plan(...)` is the backend-neutral GPU boundary. It
accepts only a compiled single-source unary chain and emits an immutable fused
point plan without wgpu objects. ColorAdjust, WhiteBalance, Vignette, and Grain
are supported in source order with a bounded eight-op pass; spatial operations,
LUT resources, custom processors, and branching graph nodes return typed
`EffectGpuPlanBlocker` values. This makes capability checks deterministic and
keeps renderer ownership separate from effect graph semantics.
`get_or_lower_effect_graph_to_gpu_plan(...)` stores successful plans and
deterministic blockers in a bounded 256-entry LRU keyed by compiled graph
signature. Playback and diagnostics share the returned `Arc` instead of
re-traversing and reallocating an unchanged graph every frame.

File-backed LUT caches key existing files by canonical path and invalidate on
file fingerprint changes. Tests that validate cache behavior should use local
cache instances rather than the process-global cache so workspace-level
parallel test runs remain deterministic.
