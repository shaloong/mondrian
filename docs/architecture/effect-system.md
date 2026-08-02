# Effect System

Timeline text generation is not an Effect capability. Basic Title is closed
Clip content whose generated working-linear frame enters this effect system at
the ordinary Clip source boundary. The former disconnected renderer-only
`TextLayer` placeholder has been removed; text parameters and animation cannot
form a second effect-specific author model.

Basic Title raster residency is bounded as one aggregate Session budget:
rendered frames and `cosmic_text` glyph/outline payloads count against the same
cap. Preview disables the renderer's duplicate frame cache because its result
task already owns that residency; Export instead owns one job-local rasterizer
and no second result cache.

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

Adding an executable effect is a definition-bound authoring operation. Product
commands construct the complete instance through
`EffectNodeExt::with_defaults()` and only then pass it to
`Clip::add_effect_node` or `Clip::insert_effect_node_at`. Timeline does not offer
an `EffectType`-only convenience constructor: it cannot manufacture the
definition-owned parameter schema without reversing the authoring-to-execution
dependency direction. `EffectNode::new` remains the low-level empty data
constructor for explicit reconstruction and unresolved/migration paths; it is
not a product “add effect” operation. Preparation validates the complete
instance against the bound definition and fails closed rather than silently
backfilling missing defaults, because doing so would change persisted author
intent at an execution seam.

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
typed resource value rather than a free-form text parameter. LUT processing
space is a separate non-animatable enum because resource identity and color
interpretation are independent author facts. `unassigned` remains a valid
recoverable author value but blocks execution; no filename heuristic may
silently fill it.

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

`CompiledEffectGraph` is immutable production IR. Its graph, compiler-owned
schedule, node-use counts, cache profiles, subtree signatures, output-cache decision,
color-domain plan, graph signature, `EffectExecutionEnvelope`, exact
Definition-stage value/node bindings, and per-node execution modes are private
and exposed only through the read-only accessors required by downstream backend
planners; the topological schedule itself remains effects-owned. A node mode is
the intersection of the operation's real implementation and its owning
Definition contract; neither side can broaden the other. Callers cannot mutate
graph or execution semantics without recompiling all derived evidence.

`EffectRenderGraph` is the authoring/compiler input used by built-in and plugin
Definition builders. It is not an executable production IR. Raw graph
scheduling remains compiler-internal and raw plan/graph execution exists only
as test reference Implementation; Preview, Export, and renderer Adapters must
cross the `CompiledEffectGraph` Interface so topology, domain, capability,
lifetime, and cache evidence cannot be detached from one another.

Graph construction is fallible. `EffectGraphBuildError` identifies an enabled
instance by persistent `EffectId` and stable definition key when the definition
is missing, modeled but not executable, runtime-disabled, missing a required
resource, panics, or produces an invalid graph. Builders stage mutations in a
temporary graph and commit only after successful return. Built-in and plugin
builders share this boundary; the plugin SDK additionally exposes fallible
builder registration for definition-specific resource checks.

## Execution Contract and Preparation

Every executable `EffectDefinition` owns one complete
`EffectExecutionContract`. The contract declares the exact admitted pairs of
processing backend and sample representation, such as `(CPU, NormalizedU8)` or
`(GPU, Float32)`; deterministic, frame-seeded, or nondeterministic output;
stateless or ordered-session state; exact past/future temporal input; ROI
propagation; resource lifetime; and the maximum linear-chain or general-DAG
topology the definition may emit. Modes are not inferred as a Cartesian
product: admitting CPU-U8 and GPU-F32 does not admit CPU-F32 or GPU-U8. An
encoded RGBA8 Custom processor therefore admits only CPU-U8, while the current
production linear built-ins admit CPU-F32. These facts are definition
semantics, not renderer guesses. Spatial contracts call the same finite-support
calculation as the production kernel. In particular, the three-pass
fractional-box Gaussian may require a halo larger than `ceil(author_radius)`
because nonzero fractional outer taps accumulate across passes; neither Blur
nor Sharpen may advertise the author-facing radius as its input halo.

The declaration is not trusted as self-authenticating capability metadata.
After each instance emits its graph, preparation derives the reachable
Implementation requirements from the actual operations and branches. Every
declared exact execution mode must be implemented; determinism, temporal
extent, and ROI may only be equally or more conservative; and retained
dependencies require an adequate resource lifetime. Violations produce a typed
`EffectExecutionContractViolation` before the graph can enter Preview, Export,
or a Prepared Program. The check repeats when animated or topology-affecting
parameters reveal a new graph shape. A plugin therefore cannot label a
Gaussian branch pixel-local, label frame-dependent output deterministic, or
advertise a float Custom processor while supplying only the encoded RGBA8 ABI.

The plugin default is deliberately conservative: no execution mode is admitted,
temporal demand and ROI are unbounded/full-frame, and continuity state is
required. A plugin becomes executable only after replacing that complete
contract and binding a fallible evaluator. Unknown or incomplete capabilities
therefore block preparation instead of authorizing plausible-looking identity
output.

`PreparedEffectStack` binds enabled instances to one exact definition-registry
revision, validates their complete Parameter Schema, and resolves immutable
resources once. `PreparedEffectProgram` adds enabled Masks and retains the
result as a resource-bound, genuinely immutable program. It retains the
zero-time graph topology and compiled graph. Frame evaluation samples only
animated values; when the graph still has that shape it reuses the immutable
topology directly. A topology-affecting parameter may reveal another valid
shape, but that dynamic residency belongs to the caller's
`EffectExecutionSession`, never to the Program. The Session compiles, exactly
matches, and entry/byte-bounds those variants under its generation barrier.
An over-budget variant is compiled and used for the current call but not
retained. The uncached reference Interface compiles a nonzero variant
ephemerally. Every new shape repeats the Definition topology and execution
contract checks before binding; preparation does not claim that every possible
shape was compiled at time zero.

Resource preparation has explicit ownership. Every renderer
`PreparedVisualProgramCache` directly owns one `LutPreparationCache`; Preview's
long-lived visual cache and every Export attempt therefore have independent
entry and conservative logical-byte grants. Program construction calls
`PreparedEffectProgram::prepare_with_lut_cache(...)`. The plain `prepare(...)`
Interface is only an owner-local uncached convenience for references and tests;
it never reaches hidden process-global residency. Cache `clear`, visual-cache
scope rotation, and online reconfiguration synchronously evict its LRU
residency. Already prepared Programs remain valid because evaluators retain
immutable resource `Arc`s rather than the mutable cache owner.
Preview additionally binds those Programs once to the exact validated
author-generation identity. Current-frame, forward-prefetch, preroll, and range
lookahead consume that immutable binding, so Effect preparation and the
conservative full-Sequence fingerprint are not repeated at presentation
cadence. Raw/deserialized author snapshots still cross the fully checked
binding Interface.

`PreparedEffectProgram::retained_bytes_estimate()` reports a deliberately
conservative, lifetime-stable logical charge covering author/evaluator
captures, declared immutable resources, masks, execution evidence, its
immutable zero-time topology, and compiled zero-time graph. Evaluating a frame
cannot change this number. A shared `Arc` may be charged to more than one
Program so each cache owner is independently safe. Dynamic topology charges
appear only in the owning `EffectExecutionSession` diagnostics. These numbers
are for cache admission and regression evidence; they are not allocator usage,
GPU memory, process Working Set, or RSS. A plugin preparer that captures an
opaque immutable payload must add its logical charge through
`PreparedEffectEvaluator::with_retained_resource_bytes(...)`.

Every `CompiledEffectGraph`, whether produced by a Definition-bound prepared
program or the conservative graph compiler, retains one
`EffectExecutionEnvelope`; there is no second Render Graph or parallel
capability IR. Definition-bound programs retain ordered Definition stage
contracts together with each evaluator's exact incoming value, selected output
value, and every reachable node it emitted. Enabled identity stages remain
present with an empty emitted-node set. Conservative compilation derives
one-node stage evidence from the operations actually compiled. The aggregate
retains temporal, ROI, state, and resource obligations plus the intersection of
exact modes admitted by every stage. An empty exact-mode intersection means a
valid heterogeneous chain, not invalid author state; execution then requires
explicit representation and/or backend transitions.

The envelope now provides checked execution-demand planning for an exact
`TimelineTime`, complete frame extent, and requested output ROI. Finite history
is subtracted in that signed owner domain without inventing a zero boundary;
exact rational overflow fails with direction-bearing typed evidence, and
unbounded history or lookahead remains explicitly unbounded for an enclosing
source provider to resolve. Clip handles, transparent/missing source policy,
and source-domain bounds belong to the prepared placement/provider, not the
generic Effect planner. Pixel rectangles are intersected and expanded without
`u32` wrapping.
The result distinguishes an exact full-frame dependency from an
unknown-conservative full-frame request, and retains empty work as empty. The
same demand exposes state, resource-lifetime, aggregate exact modes, and
per-stage exact-mode obligations.

Linear-stage placement is a deterministic small dynamic program over explicit
`(lane, sample representation)` states. A lane declares one concrete CPU, GPU,
or external backend, its exact representations, and a relative dispatch cost.
Every adjacent placement change requires one exact directed transfer
capability; undeclared modes or transfers fail closed with a typed stage index.
This is candidate placement evidence for Definition-level `LinearChain`
contracts only. It owns no graph values, endpoint residency, fan-out/join
lifetime, barrier, or transfer execution, and rejects `GeneralDag` rather than
flattening one into topological order.

`plan_effect_graph_value_execution(...)` is the executable planning authority
for heterogeneous current-frame work. It derives from the same
`CompiledEffectGraph`, not from a second semantic IR. The caller supplies exact
source/output residency, concrete ordered lanes, directed transfer
capabilities, frame extent, and frame-local budgets. The planner places every
reachable node using its exact compiled mode, materializes each semantic graph
value at a typed `(lane, precision, color domain)` residency, and emits only
`Dispatch`, declared `Transfer`, and last-use `Release` steps. Each produced
materialization owns a completion token; dispatch waits name every unique
input producer, and transfers name their source wait and destination signal.

General DAGs retain their topology. A transferred fan-out value is
materialized once per exact target residency and shared by all branch
consumers; a join waits on the concrete branch materializations. A
deterministic shortest declared transfer path may cross multiple
lanes/representations, but never invents an edge or changes a color domain.
Every non-output materialization carries an explicit retirement dependency and
is released immediately after its final consumer token completes; plan order
alone never permits an asynchronous Adapter to free it. `Release` is also a
budget barrier: the Adapter completes that retirement before advancing to the
next plan step, so the verified live-set peak remains an execution guarantee
rather than a sequential estimate. The terminal live set must contain exactly
the requested output. Checked arithmetic and explicit limits cover peak host
bytes, peak GPU-device bytes, aggregate transfer bytes, materialization count,
and total Dispatch/Transfer/Release steps for graph-value residency. The
compiled value plan separately reports the exact peak count of simultaneously
live device-resident materializations from that verified live set. This count
is not inferred from device bytes: mixed precision, non-frame resources, and
format-specific allocation make byte division an invalid texture-count model.
Viewer admission consumes the reported count directly. Concrete
dispatch Adapters additionally admit their implementation-private scratch;
that storage is not fabricated as a semantic graph value. Color-domain
conversions, continuity state, temporal input, and external-processor lanes
remain typed blockers until their concrete execution Adapters exist.
The graph-value heterogeneous current-frame planner applies the same boundary:
even a stateless, current-frame graph is rejected when its aggregate resource
lifetime is `ContinuitySession`, because a frame attempt does not own the
ordered Session required by that resource.

`PreparedHeterogeneousEffectWork` is the first executable vertical slice over
that plan. With a caller-supplied scene-linear CPU Float32 working frame it
executes an exact unary CPU prefix through the scalar reference, requires one
explicit CPU-F32→GPU-F32 transfer, and lowers the exact remaining graph-value
tail through `lower_effect_graph_nodes_to_gpu_plan(...)`. The regression tracer
is real Gaussian Blur on CPU followed by Basic Correction and Grain in the
fused GPU point plan. Its result carries the CPU pixels, Session generation,
the immutable graph-value plan, completed CPU token, required upload wait, and
still-pending GPU-input/output tokens. An Adapter therefore receives the exact
transfer residency and lifetime steps without replanning. The result
deliberately contains no whole-graph CPU fallback Interface.
Preparation also derives the current scalar kernel's peak owned working-frame
count (including Gaussian/Sharpen premultiplication and separable scratch), and
execution checks that byte requirement against the bound Effect Execution
Session before allocating the prefix.

Current CPU RGBA8, CPU Float32, and fused GPU single-frame admission do not
execute the shallow linear-stage placement. Each scans every retained stage
for its one requested exact mode, then independently rejects temporal or
continuity-session obligations it cannot provide. Preparation does not reject
a valid stateful contract merely because these current executors lack a
continuity Session.

`EffectTemporalFrameProvider` and
`EffectExecutionSession::execute_temporal_roi_f32` form the exact scalar
semantic reference for finite-history, stateless CPU-Float32 work. One request
binds the scheduler generation, continuity evidence, exact Clip visual-domain
`TimelineTime`, the ordinary Render Plan's deterministic output-frame seed,
complete frame coordinates, output ROI, and cooperative cancellation. The seed
is part of cache identity and directly drives the current-time unary tail; it
is never inferred from a retimed source coordinate. The provider identity must
fingerprint the complete source revision, stream, Clip-to-source or nested
mapping, color/alpha interpretation, geometry, and any source-generation seed
grid. Provider responses must match the requested time, extent, ROI, and
representation exactly; cancellation is checked before and after fetch and
before cache publication.

`collect_temporal_frame_demands` walks that same compiled graph without
fetching pixels. The first production tracer admits exactly one
`Source -> TemporalFrameMix -> current-time unary tail`: history with upstream
Effect values is rejected because the compiled upstream parameters are bound
at the output Clip time. `PreparedTemporalFrameSet` freezes one complete,
de-duplicated batch and proves generation, request, ROI, extent, and tile
equality before it can implement the provider. Its lookup performs no decode,
nested rendering, color conversion, or title work.

The scalar reference consumes the existing `CompiledEffectGraph`; it does not
introduce another Render Graph. It stages the exact expanded source tile in
full-frame coordinates, evaluates ordinary unary operations with their normal
global pixel coordinates, and crops only after evaluation. Exact ROI laws
produce an explicit finite halo. The separable Gaussian implementation keeps
its sliding sums in Float64 and rounds only when storing Float32 pixels; this
prevents removed pixels outside an exact halo from leaving Float32 cancellation
residue and creating a seam against full-frame execution. This is an internal
numerical-stability rule, not a second working-precision contract.
`UnknownRequiresFullFrame` remains a
conservative full-frame request with no exact-halo evidence, so an optimized
tile backend cannot present fallback as proved tiling. Working-frame staging,
memoized temporal nodes, the provider tile, and the output crop are admitted
against one explicit transient byte budget before allocation.

`TemporalFrameMix` is the finite-history proof Processor. It coverage-correctly
interpolates the current upstream frame with an exact signed past sample.
Crossing zero remains ordinary exact arithmetic; only the prepared placement
and its source Adapter may decide whether that sample is a valid hidden handle,
transparent, or unavailable. Tests require exact signed request times and
require an expanded Gaussian ROI result to equal the crop of the scalar
full-frame result. Stateful/continuity-owned resources, unbounded
temporal demand, unresolved color domains, unsupported graph-value shapes, and
unsupported operations return typed failures rather than current-frame,
identity, or cropped output.

Export now uses the two-phase boundary: it collects a frozen batch before
pixel work and materializes every media or nested temporal binding addressed by
the renderer-owned `PreparedVisualFrameClosure`. Its job-local decoder and
closure materializer normalize those resolved pixels into the parent working
contract; neither owns a recursive Sequence interpretation. Only then does the
job-generation Effect Session execute the graph. A nested temporal source whose
raster differs from the admitted Effect extent fails closed rather than
resampling before the Effect. Basic Title history and Adjustment-stack history
remain explicit blockers.

Preview uses the same closure-addressed two-phase contract without decoding in
Viewer evaluation. It submits every exact media demand through the existing
generation-bound asynchronous Scheduler, requires a CPU-working representation
in the media request/cache key, and reports typed Temporal Pending while any
Frame Store value is missing. Nested demands are materialized from the same
closure node/binding identities; Preview has no second recursive Timeline
resolver. Only a complete set is frozen and passed to the retained Preview
Effect Session under that generation's monotonic cancellation token; no
current/displayed frame can stand in for absent history. Future/unbounded input,
stateful continuity, animated/effected upstream history, and heterogeneous
temporal execution remain outside this first tracer.

Prepared dependency identity combines the definition-registry revision with
semantic fingerprints of immutable resources. `.cube` files are parsed,
validated, hashed, and retained as shared immutable payloads during
preparation; frame evaluation neither rereads nor rehashes them. External-file
currentness is checked only through the explicit low-frequency dependency
revalidation Seam. Resource preparation classifies recovery ownership:
unassigned processing space or an unbound path requires an author edit, while a
bound external file that is currently unreadable or invalid may recover after
an external change. Only the latter is polled. Production Preview observes
exact immutable visual-program instances on a bounded background worker; a
stale or recovered dependency evicts the owning program and forces a clean
reprepare before the paused-current-frame fast path can reuse output.

Custom processor Implementations follow the same immutability rule. Definition
preparation captures one process-local processor binding and revision into each
emitted graph node. Compiled execution never resolves that node by mutable
string lookup. Replacing a Definition creates a new binding and graph
signature; already prepared Preview or Export work continues to own its
original processor until the program is retired. There is no parallel Custom
processor registry: `EffectDefinition` / `EffectPluginDefinitionBuilder`
evaluation is the sole implementation-binding path. Compilation fails closed
when any reachable raw Custom node remains unbound, so ambient process state
cannot change the meaning of an author/program revision. No compile-time or
per-frame global Custom implementation lookup exists.

Disabled instances are the only implicit identity operation. An enabled
modeled-only effect, missing plugin definition, unbound/invalid LUT, or failed
builder never becomes an unchanged frame. The shared timeline render-plan
compiler maps these failures into `MondrianError::EffectGraphEvaluationFailed`,
so preview and export both stop before publishing a misleading result. Any
future operator-approved plugin bypass must be an explicit, diagnosable policy
above this compiler seam rather than a warning followed by identity output.
`EffectPluginRuntimeFailurePolicy` controls whether a failed definition remains
callable or becomes disabled; the quarantine key combines the persistent effect
key with the exact Definition Registry revision frozen into preparation.
Consequently a late failure from an older compiled Program cannot disable a
replacement Definition using the same key. The Contract has no independently
mutable Registry: it is installed only as part of `EffectDefinition`.
`EffectPluginLibraryPolicy` controls only whether the current unavailable
Definition generation appears in new-insertion UI. Neither contract changes
frame semantics or authorizes identity fallback.

## Effect Color Domains

Every `EffectDefinition`, including plugin-authored definitions, declares an
`EffectColorDomainContract` at registration. There is no implicit compatibility
default in the definition or plugin-builder APIs. A builder may resolve a
topology-affecting parameter to a stricter per-instance contract through
`append_unary_in_domain`; graph construction scopes that override to the one
node and restores the definition contract afterwards. The resolved domain is
compiled and signed, never retained as mutable graph-builder ambient state.

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

Built-in transform/blend/solid-color state and the exact Clip source-time map
are not removable user effects. The Inspector may present them in an
effect-like stack for consistency, but deleting these core Clip contracts is
invalid and must be rejected by domain code. Source retiming remains a typed
time-domain transform, not visual parameter automation.

## Masks, Mattes, Blend Modes, Adjustment Layers

- Masks are clip components that compile into mask graph nodes.
- Blend mode is clip/track compositing state, not a unary color effect.
- Adjustment layers are clips whose effects apply to the accumulated lower image.
- Mattes should be expressed as mask/graph inputs rather than hidden UI-only flags.

## Ordering

Effect stack order affects graph output. UI reorder operations must mutate the clip effect vector, then selection/navigation state may follow without an extra undo entry.

## Optimization

`EffectCachePolicy` is a reproducibility contract and cross-call reuse
admission, not a performance hint. `Deterministic` output may be reused under a
complete semantic key. `FrameDependent` output may be reused only when the
explicit frame seed is part of that key. `Uncacheable` output must not create or
consume a cross-call subtree/output-cache entry. Policy combines
conservatively, so one uncacheable reachable operation makes every dependent
subtree and the graph output uncacheable.

Semantic identity remains distinct from reuse admission. A consumer may still
need a mandatory identity for evidence, publication, and one execution attempt;
that identity does not authorize a later call to reuse the pixels. The
renderer/effects system may otherwise merge deterministic unary operations,
cache admitted subtrees, and skip an admitted identity graph. Every
optimization must preserve graph and execution-contract semantics.

Compiled graphs carry one versioned, complete semantic identity containing the
output, ordered node topology, edge contracts, operation values, Custom
processor revision, and all mask geometry. Equality on this complete identity
is authoritative for Session-owned pixel, topology, and GPU-plan residency.
GPU-plan keys additionally retain the execution Envelope and exact stage
bindings because pixel-identical graphs can admit different node execution
modes. The derived 64-bit graph signature is only a compact diagnostic;
matching signatures never prove semantic equivalence.
Consumers that must carry identity across the effects crate seam use the
complete 32-byte `semantic_fingerprint()`; effects-owned Session-cache equality
still compares canonical structure, so this transport fingerprint
does not weaken the authoritative collision-safe comparison.

Clips with no enabled effects or masks must reuse the process-wide compiled
identity graph. Plain media and solid clips dominate preview/export playback,
so render-plan evaluation should not rebuild a source-only graph or take the
general graph compiler on every frame just to represent the identity path. This
canonical source-only graph is the sole process-wide compiled graph. Every
non-identity compiled graph is owned immutably by a `PreparedEffectProgram`,
produced ephemerally by its uncached reference Interface, or bound through the
consumer's `EffectExecutionSession`; there is no generic process-global
compiled-graph LRU. One `PreparedVisualProgram` also shares a single identity
`PreparedEffectProgram` across all identity Clips in that Sequence revision;
it must not allocate a topology cache per plain Clip.

## Float/Linear Execution

`mondrian-effects` exposes a typed float/linear execution boundary for effect
graphs that can operate directly on working-space `f32` RGBA pixels.
`compiled_effect_graph_supports_rgba_f32(...)` and
`compiled_effect_graph_supports_rgba_f32_with_domain_processor(...)` are
complete capability/admission queries and use the same validation rules as
`apply_compiled_effect_graph_rgba_f32(...)`. The narrower
`compiled_effect_graph_has_rgba_f32_execution_shape(...)` and
`compiled_effect_graph_has_resolvable_rgba_f32_domain(...)` inspect only
operation topology and color-domain resolvability, respectively. They may help
a renderer choose a whole-frame representation, but never substitute for exact
execution-mode, temporal, state, or lifetime admission. All existing
executable built-in unary render operations (`ColorAdjust`,
`GaussianBlur`, `Sharpen`, `Vignette`, `ChromaticAberration`, `Grain`, and
`Lut3D`) execute in this boundary. The float path preserves extended scene-linear
values and does not clamp RGB to 0..1 as the legacy RGBA8 path does.

Spatial operations sample straight-alpha input through premultiplied-alpha
intermediates so transparent pixels cannot contaminate visible colors. Grain
uses the graph frame seed, and LUT intensity blends back to the unbounded float
source after normalized LUT sampling. These rules are shared by media, solid,
and adjustment-layer execution.

Gaussian Blur admits the full authored 0–200 pixel range without a hidden
clamp or an algorithm switch at a common radius. Three separable fractional-box
passes approximate the matching Gaussian variance with continuous edge
weights. Every radius is linear in frame pixels, parameter animation remains
continuous when an integer support boundary changes, constant fields remain
constant, and premultiplied-alpha semantics are unchanged. This avoids the
former 24-pixel complexity cliff where ordinary 4K frames could suddenly
execute a radius-proportional convolution.

Primary Color is not a Rec.709-coded display adjustment. Sequence evaluation
projects the exact `WorkingColorSpace` into graph construction; its operation
stores that identity, uses the corresponding RGB luminance coefficients for
saturation, applies exposure as a power-of-two scene-linear gain, and pivots
contrast at 0.18. CPU and GPU consume the same operation values. The old
WhiteBalance operation used additive RGB offsets without an observer,
illuminant, or chromatic-adaptation contract and was therefore capable of
plausible-looking false color. Its persisted author type remains modeled for
structured diagnosis, but it is not registered as selectable or executable.

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

All encoded, Float32, per-node, temporal pixel, and nonzero dynamic-topology
residency belongs to one explicit `EffectExecutionSession`; there is no
process-global Effect frame, non-identity compiled-graph, or topology cache.
Preview's production Runtime and each Export Visual Render Session own
different instances. The main entry and
aggregate byte grants are partitioned across four pixel classes plus
backend-neutral topology without multiplying the caller's cap; GPU lowering
plans retain their separate small planning grant. Reconfiguration trims every
class synchronously. Binding a different Preview generation or Export attempt
generation clears pixels, topology variants, GPU plans, and blockers as one
conservative barrier, so canceled or superseded work cannot publish into the
next execution lifetime. A topology too large for its partition still
correctly binds the current graph but receives no cross-call reuse.
Convenience functions construct uncached ephemeral Sessions rather than
borrowing hidden global residency.

Float/linear output reuse keys the complete compiled graph identity, typed
float input fingerprint, dimensions, and frame seed when a graph is
frame-dependent. Temporal reuse additionally keys a versioned semantic domain,
the complete provider identity, exact generation/time, full extent, requested
and expanded ROI evidence, and continuity evidence. Domain-processed entries
also key the exact color engine and working identity so cached pixels cannot
cross Project color semantics. Process-global OCIO reload generation remains
diagnostic evidence and is not allowed to substitute for exact semantic
identity.

The encoded and float input identities are type-separated SHA-256 content
fingerprints over every input channel bit; the former 64-bit frame hashes are
not cache authority. Per-node reuse is addressed by complete graph identity
plus node ID, so the compiled `subtree_signature` remains profiling and
diagnostic metadata only. A renderer-owned domain processor supplies an
`EffectDomainProcessorCacheKey` derived from its complete canonical semantic
identity: exact color engine/config, working space, dynamic properties, and
implementation revision. Equal keys assert bit-equivalent processing.
Truncated hashes, display labels, and process-global reload generations are
invalid domain keys.

For a Custom operation, the complete graph identity always includes evaluated
`params`. An optional `cache_key` adds stable external-resource or
implementation identity; it never replaces frame-varying parameters. A LUT
path key therefore cannot freeze an animated intensity or another parameter at
the first compiled frame.

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

The encoded custom-processor fallback is also fail-closed. Missing processors,
processor errors and processor panics return `EffectExecutionError`; staged
pixels are discarded. `timeline_composite` wraps encoded, float and unresolved
domain failures in `TimelineCompositeError`, and production preview/export
propagate that error instead of substituting black or unchanged pixels.

`lower_effect_graph_to_gpu_plan(...)` is the backend-neutral GPU boundary. It
accepts only a compiled single-source unary chain and emits an immutable fused
point plan without wgpu objects. One preserving scene-linear, log/perceptual,
display-linear, or display-encoded processing domain is retained as part of the
plan rather than interpreted by the effects crate. ColorAdjust, Vignette, and Grain
are supported in source order with a bounded eight-op pass; spatial operations,
LUT resources, custom processors, and branching graph nodes return typed
`EffectGpuPlanBlocker` values. This makes capability checks deterministic and
keeps renderer ownership separate from effect graph semantics.
`lower_effect_graph_nodes_to_gpu_plan(...)` lowers only an exact unary tail
ending at the same compiled output. It admits GPU Float32 per selected node
from the node's implementation/Definition intersection, records the exact
source value, output value, selected node IDs, and complete graph fingerprint,
and rejects hidden color-domain transitions. This is the continuation used by
`PreparedHeterogeneousEffectWork`; it does not relabel a heterogeneous
Envelope as homogeneous GPU work.
`get_or_lower_effect_graph_to_gpu_plan(...)` requires an explicit
`EffectExecutionSession`. Successful plans and deterministic blockers use that
owner's independent entry limit and conservative retained-byte budget. The key
contains complete compiled graph identity, the complete execution Envelope,
and exact Definition-stage value/node bindings; two pixel-identical graphs
whose node admission differs therefore cannot reuse one another's plan or
blocker. Preview Viewer lowering, CPU compositor diagnostics, and each Export
job reuse only their own returned `Arc`; no process-global GPU-plan cache can
retain another Project or execution generation. Online reconfiguration trims
synchronously, and the same generation barrier that retires pixel residency
also retires every plan and blocker. `lower_effect_graph_to_gpu_plan(...)`
remains the uncached reference helper.

The `.cube` adapter accepts one finite 3D table of size 2 through 129, requires
the exact payload count, honors finite monotonic `DOMAIN_MIN`/`DOMAIN_MAX`, and
rejects 1D or combined 1D+3D files rather than partially interpreting them.
Sampling uses tetrahedral interpolation with red-fastest cube ordering and
domain normalization. File-backed LUT preparation establishes semantic
identity with SHA-256 over the complete parsed table, retains the parsed table
behind `Arc`, and carries that fingerprint in the visual-program dependency
identity. Low-frequency revalidation reparses and compares the semantic
fingerprint, so same-size or timestamp-preserving replacement cannot survive
validation. Before a preparation-cache hit is accepted, the owner reads and
hashes the complete file bytes; path, size, mtime, and a recent-check interval
are never freshness authority. Matching bytes avoid reparsing and reuse one
immutable payload within that owner. The cache is LRU bounded by both entry
count and logical bytes; an individually oversized payload is returned to the
requesting Program but is not retained. The frame hot path never polls the
filesystem, and Preview residency cannot mutate an Export attempt's cache.
