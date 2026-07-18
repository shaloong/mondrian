# Effect System

Effects are timeline-instance operations that transform image data through a compiled render graph.

This document describes visual effects. Audio processing uses the distinct
Sequence-owned Audio Processor model in [Audio Pipeline](audio-pipeline.md).
Both domains reuse foundation concepts such as strong IDs, typed parameter
descriptors, exact automation curves, migration, and compiled execution, but do
not share `EffectNode`, RGBA capabilities, JSON parameters, or runtime graphs.

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

## Parameter Schema and Addressing

Every product property carries a versioned `ParameterSchema`. Its
`ParameterId` is definition-stable and is the only identity accepted by effect
execution. The string property path is an instance address for author commands
and Inspector routing; renaming or re-namespacing that address must not rename
the parameter. The schema owns value type, definition default, automation
capability, unit/range, admitted Hold/Linear/Bezier mathematics, stable enum or
resource intent, localization identity, and cache impact. Auto Bezier,
Continuous Bezier, and Ease are editor presets that produce Bezier handles;
they are not additional persisted execution semantics. Effect registration
rejects zero schema versions, empty message IDs, malformed defaults/ranges,
invalid enum sets, and duplicate Parameter IDs before a definition enters the
registry.

The schema is executable rather than decorative. `AnimatedProperty` enforces
finite values, hard-range policy, allowed interpolation, dense channel layout,
strict keyframe ordering, and stable enum option indices on every mutation and
when project author state is validated. Soft ranges and steps drive the editor
from that same schema; UI metadata only owns presentation grouping and spatial
layout hints. Enum parameters use Hold automation over stable option keys.
Resource parameters distinguish unbound, project-asset, external-file, and URI
intent and require resource-level cache invalidation. LUT selection uses this
typed resource value rather than a free-form text parameter.

Parameter cache impact describes whether a value changes output, selects a
resource, or changes topology. Processor capabilities are not copied into every
parameter: color/alpha domain, CPU/GPU implementation, determinism, temporal
extent, and ROI remain owned by `EffectDefinition` and the compiled graph. A
topology-impacting parameter forces those capabilities to be resolved again.

## Graph Model

`EffectRenderGraph` supports:

- `Source`
- `UnaryEffect`
- `DomainEffect`
- `Blend`
- `Mask`
- `MaskSource`
- ordered `MultiInput`

`CompiledEffectGraph` stores schedule, node use counts, cache policies, estimated cost, subtree signatures, output cache flags, and a compiled color-domain plan.

## Effect Color Domains

Every `EffectDefinition`, including plugin-authored definitions, declares an
`EffectColorDomainContract` at registration. There is no implicit compatibility
default in the definition or plugin-builder APIs. Current built-ins explicitly
declare the scene-linear working RGB contract.

The contract distinguishes scene-linear RGB, named log/perceptual RGB,
display-linear RGB, display-encoded RGB, non-color data, and alpha/mask values.
`DomainEffect` carries that contract into the authored graph. Compilation
propagates the output domain of every reachable node and produces explicit
`EffectDomainTransition` edges for convertible RGB boundaries. Data and alpha
crossings are not guessed or color converted; they produce typed
`EffectDomainBlocker` values unless the graph supplies the matching payload
contract, such as an `AlphaMask` input to a mask node.

The timeline compositor owns scene-linear RGB at the graph source and output.
The renderer is therefore the only layer allowed to resolve planned RGB-domain
edges through the active stock-OCIO configuration. Effects do not load OCIO,
select a project color engine, or perform display transforms themselves.
The CPU timeline renderer materializes every legal RGB transition in-place on
the float pixel buffer through the exact project `ColorEngine` and stock OCIO
processor. Preview and export call this same renderer boundary. Processor
failure, invalid data/alpha crossings, and backends that have not materialized
the plan fail closed; they are never relabeled as an RGBA8 fallback. This
guarantees that adding a display- or log-domain effect cannot silently execute
its math on scene-linear samples. GPU lowering retains one exact preserving RGB
processing domain on `CompiledEffectGpuPlan`; mixed or non-preserving domains
remain typed blockers. The legacy working compositor accepts only
`SceneLinearRgb`, so a non-linear plan cannot execute before the renderer has
materialized its surrounding stock-OCIO passes.

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
during preview scrubbing or export retries. Domain-processed entries also key
the exact color engine, working identity, and OCIO configuration generation so
cached pixels cannot cross project color semantics.

Unsupported graph nodes and render ops return structured
`EffectFloatExecutionError` / `EffectFloatUnsupportedReason` values so renderer
callers can make an explicit legacy fallback decision. The renderer may satisfy
legal RGB transitions through
`apply_compiled_effect_graph_rgba_f32_with_domain_processor(...)`; transition
failures and domain blockers are fail-closed and never authorize RGBA8
execution. The encoded executor returns `EffectExecutionError` for any domain
plan because it has no typed OCIO runtime. Custom/plugin processors remain
unsupported until their ABI declares a float implementation. CPU float support
does not imply GPU execution support.

`lower_effect_graph_to_gpu_plan(...)` is the backend-neutral GPU boundary. It
accepts only a compiled single-source unary chain and emits an immutable fused
point plan without wgpu objects. One preserving scene-linear, log/perceptual,
display-linear, or display-encoded processing domain is retained as part of the
plan rather than interpreted by the effects crate. ColorAdjust, WhiteBalance, Vignette, and Grain
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
