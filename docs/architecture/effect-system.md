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
the fallible `instantiate_effect_node()` boundary and only then pass it to
`Clip::add_effect_node`. The constructor resolves the current registered,
visually executable Definition, clones its validated defaults, and forks every
owner-local `AnimationTrackId`; two instances share Schema and default values,
never author identity. A stale or unavailable Definition fails before the
author transaction and cannot create an empty-property placeholder.
Timeline does not offer
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
execution. Product authoring addresses one concrete parameter with
`AnimationParameterAddress { AnimationTrackId, ParameterId }`. The string
property path is presentation/resource metadata and the final owner-local
mutation key after that stable address is resolved; it is never external Action
identity. Renaming or re-namespacing a path must not rename the parameter or
invalidate an already projected stable address. The schema owns value type, definition default, automation
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
from that same schema; UI metadata owns presentation grouping, Definition order,
and spatial layout hints. Missing order metadata retains deterministic address
order for older author data. Enum parameters use Hold automation over stable option keys.
Resource parameters distinguish unbound, project-asset, external-file, and URI
intent and require resource-level cache invalidation. LUT selection uses this
typed resource value rather than a free-form text parameter. LUT processing
space is a separate non-animatable enum because resource identity and color
interpretation are independent author facts. `unassigned` remains a valid
recoverable author value but blocks execution; no filename heuristic may
silently fill it.

Color curves are a first-class `PropertyValue::Curve`, not JSON or an array of
slider approximations. Core validates two through thirty-two finite normalized
points, exact domain endpoints, and strictly increasing x coordinates. The ten
`builtin.curves` curve parameters are deliberately non-animatable and retain
their definition-stable `ParameterId` plus instance-local `AnimationTrackId`.
Effects compiles Master/R/G/B and six Hue/Luma/Saturation secondary curves to
one immutable 256-sample resource. RGB master mode applies per channel; YRGB
mode applies a working-space CIE-Y delta before the channel curves. RGB curves
use endpoint-slope extrapolation outside `[0,1]`, and an exactly neutral
secondary set bypasses RGB/HSV conversion so negative scene-linear channels are
not altered by an identity grade.

`builtin.gamut_compression` and `builtin.highlight_recovery` are ordinary
Definition-backed, animatable, pointwise grades. Gamut Compression compiles an
`amount` in `[0,1]` and executes the fixed ACES 1.3 Reference Gamut Compression
in ACEScg/AP1. Linear Rec.709, Rec.2020, and P3-D65 working pixels cross fixed,
Bradford-adapted working/AP1 matrices; ACEScg is direct. Highlight Recovery
compiles scene-linear `threshold`, positive `rolloff`, and `[0,1]` `strength`,
then moves only above-threshold chroma toward the neutral axis while preserving
the exact working-space CIE Y. It repairs clipped-channel false color; it does
not claim to reconstruct RAW samples or missing spatial detail. Both operators
retain alpha and extended Float32 RGB, validate all authored values before graph
publication, require no scratch frame, and share one `EffectRenderOp` contract
across Preview and Export.

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
- `MaskCombine`
- `MatteMix`
- ordered `MultiInput`

`Mask` remains the generic picture-alpha primitive. Product Power Windows use a
different, explicit grade-matte topology: each `MaskSource` produces an
`AlphaMask`, `MaskCombine` reduces the ordered Window stack with Add, Subtract,
Intersect, or Difference, and `MatteMix` combines the ungraded base and graded
result. Its RGB equation is `mix(base.rgb, graded.rgb, matte.a)` and its output
alpha is always `base.a`; Window coverage can never become Clip transparency.

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
`TimelineTime`, complete frame extent, and requested output ROI. Finite past
and future extents are projected in that signed owner domain without inventing a zero boundary;
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
Viewer admission consumes the reported count directly only for Adapters whose
physical recording strategy can realize those lifetimes. The current wgpu
one-command-buffer Adapter cannot alias a released texture before submission,
so it derives and admits a second conservative physical demand: uploaded input
plus every recorded dispatch output, in both bytes and texture count. Preview,
Export, Viewer admission, and recorded evidence consume this same derivation;
none may substitute the smaller abstract peak. Concrete dispatch Adapters
additionally admit their implementation-private scratch; that storage is not
fabricated as a semantic graph value. Color-domain
conversions, continuity state, temporal input, and external-processor lanes
remain typed blockers until their concrete execution Adapters exist.
The graph-value heterogeneous current-frame planner applies the same boundary:
even a stateless, current-frame graph is rejected when its aggregate resource
lifetime is `ContinuitySession`, because a frame attempt does not own the
ordered Session required by that resource.

`PreparedHeterogeneousEffectRoute` is the renderer-owned immutable preparation
Seam over the bounded production shape. It binds one compiled
graph, extent, graph-planning budget, semantic fingerprint, graph-value plan,
CPU prefix, and GPU suffix before source pixels exist. Preview and Export bind
pixels to that same object; neither execution worker may choose lanes or
replan. A changed extent or batch grant fails before CPU execution.
Effects also derives one opaque, versioned `HeterogeneousEffectShapeIdentity`
from the complete dispatch/transfer/release sequence, lane/backend choices,
exact value formats, and executable operation kinds. Frame extent and authored
parameter values are deliberately outside this shape identity. Renderer
exposes it through the prepared route, and Export binds it into the attempt
ledger; Export does not walk graph nodes or reinterpret value domains to
reconstruct a second route fingerprint.
`PreparedHeterogeneousEffectWork` then executes the prepared semantics. With a
caller-supplied scene-linear CPU Float32 working frame it
executes an exact source-closed CPU DAG prefix directly from the planned
materialization inputs/outputs, atomically publishes every CPU-frontier value
needed by one or more explicit CPU-F32→GPU-F32 transfers, and lowers the exact remaining graph-value
tail through `lower_effect_graph_nodes_to_gpu_plan(...)`. The regression tracer
is real Gaussian Blur on CPU followed by Basic Correction and Grain in the
fused GPU point plan; a second gate proves source fan-out, independent unary
branches, a Blend join, exact last-use release, and the joined value entering
the same GPU tail. The GPU suffix consumes the same planned values and may
rejoin point-operation branches through any canonical scene-linear BlendMode;
the shared renderer compositor preserves authored opacity and the complete
64-bit frame seed used to derive each Dissolve pixel decision. Real-device
tests compare all canonical modes, alpha/opacity boundaries, channel ties, and
Dissolve against the CPU Float32 scalar reference. Unary, Blend, nonempty
MultiInput, and Mask nodes with an already materialized matte use the same
Float32 pixel primitives as the scalar reference. Synthetic `MaskSource` nodes are zero-input CPU DAG values: Path/BVH
preparation and full-frame Float32 rasterization consume a generic checkpoint
Seam, so token-backed temporal execution and deadline-backed heterogeneous
execution share one Implementation without flattening the latter's exact stop
reason. Prepared geometry, maximum row scratch, raster output live-set, and
kernel scratch are all included in the same checked Session demand; a stop
returns neither partial pixels nor a completion token.
Its result carries the ordered CPU boundary materializations, Session generation,
the immutable graph-value plan, each completed CPU token/required upload wait,
each exact `EffectValueFormat`, and still-pending GPU-input/output tokens. An
Adapter therefore receives the exact transfer residency and lifetime steps
without replanning. The result
deliberately contains no whole-graph CPU fallback Interface. Before the CPU
prefix starts, its GPU grant independently admits upload bytes, physical device
bytes, physical texture count, and optional readback bytes. Byte limits never
stand in for resource-count limits.
The graph-value plan is dependency ordered, not a serialized backend timeline:
an upload first required by a later topological consumer may appear in the plan
list after an earlier independent GPU dispatch. Preparation accepts it only when
it is an exact, non-converting CPU-Float32-to-GPU-Float32 transfer with matching
value format and explicit CPU/GPU lanes. It collects every such frontier value
before GPU submission, so execution remains one-way CPU-prefix→GPU-suffix. A GPU
readback, CPU dispatch after GPU entry, representation conversion, or external
lane remains rejected.
Preparation compiles exact CPU materialization use counts, rejects any input
that is not produced by the source-closed prefix, and proves that the only
remaining live CPU values are the ordered transfer frontier. Runtime moves last-use values,
clones only fan-out values with future consumers, releases join inputs, and
checks the resulting peak plus Mask geometry/row scratch and Gaussian/Sharpen scratch against the bound
Effect Execution Session before allocation. Input and fan-out copies observe
the same fixed-size cooperative checkpoints as long-running kernels. The GPU
route additionally publishes the exact logical bytes retained by every CPU
frontier materialization. Renderer batch admission sums the immutable caller
input plus all of those values for every item; it never assumes that one route
has exactly one output frame.
The GPU
Adapter executes an ordered MultiInput with at least two inputs as a left fold
over the shared straight-alpha BlendMode compositor. An N-input dispatch owns
`N - 2` private intermediate textures plus its semantic output; the physical
recording requirement and evidence count every one in bytes and resources,
and terminal failure removes unpublished private outputs. One-input
MultiInput lowers to a distinct, bit-preserving texture copy so graph-value
identity and last-use lifetime remain exact without a shader or color-domain
round trip. Multiple CPU-frontier uploads are supported within this one-way
CPU-prefix→GPU-suffix shape. `MaskSource` remains a cancellable CPU raster;
its execution contract admits CPU NormalizedU8, CPU Float32, and GPU Float32,
while the current heterogeneous Power Window route rasterizes it in the CPU
prefix and transfers its `AlphaMask` without an RGB reinterpretation. GPU
`MaskCombine` evaluates all four Window operations, and GPU `MatteMix` applies
the combined matte while preserving the ungraded base alpha. A legacy GPU
`Mask` dispatch still applies its generic picture-alpha algebra. Real-wgpu
tests compare the complete `MaskSource -> MaskCombine -> MatteMix` route with
the CPU Float32 reference, including HDR/negative RGB and programme alpha. A
later backend transition or readback
inside the graph, color-domain conversion beyond these non-converting typed
transfers, temporal/stateful work, and external lanes remain typed blockers rather than
being flattened or silently rerun.

Current CPU RGBA8, CPU Float32, and fused GPU single-frame admission do not
execute the shallow linear-stage placement. Each scans every retained stage
for its one requested exact mode, then independently rejects temporal or
continuity-session obligations it cannot provide. Preparation does not reject
a valid stateful contract merely because these current executors lack a
continuity Session.

`EffectTemporalFrameProvider` and
`EffectExecutionSession::execute_temporal_f32` form the exact budget-aware
scalar semantic reference for finite, stateless CPU-Float32 temporal work. One request
binds the scheduler generation, continuity evidence, exact Clip visual-domain
`TimelineTime`, the ordinary Render Plan's deterministic output-frame seed,
complete frame coordinates, output ROI, and cooperative cancellation. The seed
is part of cache identity and directly drives the current-time Effect DAG; it
is never inferred from a retimed source coordinate. The provider identity must
fingerprint the complete source revision, stream, Clip-to-source or nested
mapping, color/alpha interpretation, geometry, and any source-generation seed
grid. The provider exposes exact retained Float32 coverage bytes and copies a
requested subregion into executor-owned capacity; it cannot decode, convert,
recursively evaluate, or allocate at this seam. Cancellation is checked during
bounded copies, between planned tiles, inside kernels and stitching, and before
cache publication.

`prepare_temporal_frame_execution` expands cross-time dependencies without
fetching pixels. Its `PreparedEffectTemporalExecution` owns the root
`CompiledEffectGraph`, scheduler request, exact-time value projection, and raw
source demand batch as one immutable object; collection and execution therefore
cannot use different graph evaluations. `TemporalFrameBlend` addresses its
Definition stage input, not merely a source time. When that input has earlier
Effect stages, the Effects Module reevaluates the same bound
`PreparedEffectProgram` at `output_time + sample_offset`; the projection
targets the same stage index's input in that sampled graph. Animated
parameters, dynamic topology, frame-seeded operations, and Mask geometry
consequently use their own exact
sample instant. Stage contracts and ordering must remain stable, and a temporal
op may not address an internal same-stage value whose cross-time identity is
undefined. Both conditions fail closed.

The production overload is owned by `EffectExecutionSession`. It binds the
request generation and evaluates both the root and every sampled graph through
`PreparedEffectProgram::evaluate_with_session`; ordinary frame evaluation and
later pixel execution use that same owner. Dynamic topology reached only by a
temporal sample is therefore charged, reused, trimmed, and retired by the same
bounded Session as current-time topology. The session-free overload remains
the uncached scalar/reference path and is not a production cache owner.

The projection references the unique compiled Effect IR rather than copying
operations into another Render Graph. Ordinary edges stay within one exact-time
context; temporal edges move to an earlier Definition stage at another exact
time. This strictly decreasing stage address makes recursive finite taps
acyclic by construction. Exact `(time, graph value)` addresses, source times,
edge use counts, and context fingerprints are de-duplicated deterministically.
Hard limits of 512 graph contexts and 65,536 expanded values bound hostile or
accidentally combinatorial graphs before pixels or Masks are allocated. The
root aggregate temporal extent must cover every discovered raw source time,
every sampled context must admit stateless CPU Float32 execution, and its
conservative spatial demand must fit inside the root request's already admitted
coverage. A violated Definition contract is an error, never a cropped result.

The legacy `collect_temporal_frame_demands` / `execute_temporal_f32` convenience
Interface remains valid for source-fed finite taps followed by current-time
graph work. It
intentionally lacks an exact-time prepared-program evaluator and therefore
rejects effected temporal inputs instead of reusing output-time parameters.
Timeline production uses the prepared Interface. `PreparedTemporalFrameSet`
freezes one complete, de-duplicated batch and proves
generation, request, ROI, extent, and tile equality before it can implement the
provider. The demand batch computes checked exact Float32 coverage bytes before
callers materialize any dependency. Preview and Export admit that aggregate
against their CPU active-working-set grant first. A frozen coverage tile may
then satisfy any contained execution subregion without another decode, nested
render, color conversion, title evaluation, or hidden allocation. Missing or
ambiguous coverage fails closed. Mask/MaskSource/MaskCombine/MatteMix use the
same exact-region
contract: `PreparedMaskRaster` binds one evaluated Mask to the complete frame
extent, validates finite geometry, flattens cubic Path segments once, and
builds the nearest-segment index once. Every direct or tiled raster uses
full-canvas pixel centers, bounded row-crossing scratch, and cooperative
cancellation. The 4,096-point author Path limit prevents valid snapshots from
creating unbounded preparation or scratch obligations.

The scalar reference consumes the existing `CompiledEffectGraph`; it does not
introduce another Render Graph. It retains only the exact expanded source tile
and carries a checked raster-region descriptor containing both tile origin and
complete-frame extent. Coordinate-dependent operations such as Vignette and
frame-seeded Grain therefore observe the same global pixels as full-frame
execution instead of silently re-centering or re-seeding at the tile origin.
Finite-kernel operations evaluate the admitted halo locally and crop only after
evaluation; an operation whose implementation contract needs the complete
frame rejects a partial region. Exact ROI laws produce an explicit finite halo.
The separable Gaussian implementation keeps its sliding sums in Float64 and
rounds only when storing Float32 pixels; this prevents removed pixels outside
an exact halo from leaving Float32 cancellation residue and creating a seam
against full-frame execution. This is an internal numerical-stability rule,
not a second working-precision contract. `UnknownRequiresFullFrame` remains a
conservative full-frame request with no exact-halo evidence, so an optimized
tile backend cannot present fallback as proved tiling.

The scalar executor consumes the prepared time-expanded topological schedule
and its exact use counts as its sole liveness authority. A pure private
scheduling Module dry-runs that same liveness program before pixel work. A
last-use value moves into its consumer, a
fan-out value is cloned only while another edge remains, and Blend/MultiInput
releases each joined overlay immediately. It fails if an edge is over- or
under-consumed, a non-output value survives, or the owned-byte ledger does not
end with exactly one output. Every live graph value, final output, and
implementation-owned same-sized kernel scratch allocation is included in one
Session working-set model together with the provider's retained source
coverage. Every executor-owned allocation is admitted before it is created.
The output `Vec` transfers into shared `Arc<Vec<_>>` ownership without copying
the pixel allocation; cache publication therefore does not require a hidden
second output buffer. Gaussian Blur accounts for its separable scratch, Sharpen for the
simultaneously live blurred result plus
Gaussian scratch, and full-frame Chromatic Aberration for its replacement
buffer.

If the requested ROI fits the grant, it executes once. Otherwise the Session
retains exactly one final output buffer and derives a deterministic recursive
two-dimensional partition. Each non-overlapping tile is proved against the
remaining grant from the same graph, ROI law, scratch contract, schedule, and
use counts; at most 4,096 tiles are admitted. An indivisible tile that still
does not fit, an unsafe tile-count fan-out, or a full-frame-only operation that
cannot satisfy a partial request fails during planning before any provider
copy. Only one tile live set coexists with retained source coverage and the
final output, and cancellation never publishes a partially stitched frame. A
tile is attempt-local and can neither read nor publish an output-cache entry;
only the complete request may populate the Session cache. A planning-only UHD
gate proves that a two-frame pixel-local temporal request partitions into 64
`480x270` tiles and peaks at 402,278,400 bytes under the standard 384 MiB
grant, without allocating test pixels. A small exact ROI therefore scales with
its expanded input tile rather than
allocating a zero-filled complete-frame buffer, while a complete Preview or
Export request now automatically uses this bounded scalar tiling when direct
execution does not fit. Regression references require
expanded Blur, Vignette, frame-seeded Grain, Mask/MaskSource and Power Window
`MaskCombine`/`MatteMix`, plus a finite temporal
fan-out/Blend/MultiInput DAG (including coordinate-seeded Dissolve) to equal the
corresponding full-frame/current-frame reference crop in direct and tiled
production execution. Signed past/future and duplicate-sample multi-tap gates
also prove deterministic request order, exact de-duplication, scalar reference
output, and direct/tiled parity. This does not claim GPU temporal tiling,
unbounded input, stateful continuity, or cross-time identity for internal
same-stage temporal branches. A separate effected-input gate proves that an
upstream exposure evaluated as `+1` at the output and `-1` at the sampled
instant produces the exact two-context reference instead of reusing current
parameters.

`TemporalFrameBlend` is the finite signed-tap proof Processor. It
coverage-correctly interpolates the current Source frame with the exact sample
at `output_time + sample_offset`. Crossing zero remains ordinary exact
arithmetic; only the prepared placement and its source Adapter may decide
whether that sample is a valid hidden handle, transparent, or unavailable.
Tests require exact signed request times, retime mapping, duplicate-time
de-duplication, and an expanded Gaussian ROI result equal to the crop of the
scalar full-frame result. Stateful/continuity-owned resources, unbounded
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
current/displayed frame can stand in for an absent sample. Masks are prepared
once per reachable exact-time graph context and run in this same production
contract. Unbounded input, stateful continuity, internal same-stage temporal
branches, and heterogeneous temporal execution remain outside it.

Preview and Export enter this contract through the renderer-owned
`TimelineCompositeScratch::prepare_timeline_frame_execution`. That deep
Interface binds the consumer generation, evaluates the ordinary Render Plan,
and freezes all finite-temporal batches with the scratch's one retained Effect
Session. Application and queue code do not invoke a second temporal planner or
choose a different topology owner.

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

- Masks are Clip-owned author components that compile into mask graph nodes.
  Scalar values use stable `AnimationParameterAddress` identities. Complete
  shape keys use stable key identities and exact Clip-local time; Hold is the
  required boundary for incompatible primitive/path topology, while Linear is
  admitted only for compatible endpoints. Mask stack order uses stable
  `Before(MaskId)` / `After(MaskId)` placement rather than indexes.
- The product Mask Interface is transactional and separate from Effect
  definition insertion: Track placement/time are re-derived at dispatch,
  Track and Mask locks are both enforced, and no string-path UUID parser is an
  authoring authority. Preview and Export consume the same persisted Mask
  state through the sole compiled Effect program.
- Mask motion analysis is a pure Effects service over explicit finite luminance
  frames. Deterministic bounded feature selection and normalized patch
  correlation feed robust median translation or an actual 8-DOF homography
  with checked solve/refit and deterministic RANSAC. The result carries match,
  inlier, RMS and spatial-coverage evidence; weak texture, ambiguous matches,
  low confidence, cancellation and degenerate/non-finite projective mappings
  fail closed. Effects owns no decoder, worker, cache or author transaction.
- Blend mode is clip/track compositing state, not a unary color effect.
- Adjustment layers are clips whose effects apply to the accumulated lower image.
- Mattes should be expressed as mask/graph inputs rather than hidden UI-only flags.

### Qualifier matte pipeline

`builtin.qualifier` is an executable General-DAG Definition, not a color filter
that mutates a frame's alpha in place. Its prepared operation consumes
`SceneLinearRgb` and produces an explicit Float32 `AlphaMask`. Normal output
feeds that value into the canonical Mask node beside the unchanged source;
Matte Preview instead uses a separate `AlphaMask -> SceneLinearRgb` domain
operation that produces opaque grayscale. The preview switch therefore has
`Topology` cache impact, while the structured sample set has ordinary `Value`
impact. Preview and Export compile and execute the same graph.

`PreparedQualifier` is the sole execution interpretation for both HSL and 3D
selection. HSL uses circular hue plus bounded saturation/luminance ranges in
the exact working-space luminance basis and safely maps negative/extended
scene-linear samples. The 3D mode evaluates up to sixteen normalized RGB-cube
Include/Exclude samples as include-union followed by exclude subtraction.
Matte refinement applies bounded separable box denoise, Gaussian feather,
Clean Black/White, and optional inversion with cooperative checkpoints. The
complete prepared payload has one semantic fingerprint used by graph/GPU cache
identity. CPU execution uses two bounded scalar scratch planes; GPU execution
uses one, two, or four explicit passes according to the admitted refinement.
No UI-private key color, hidden matte allocation, or Preview-only algorithm is
allowed.

## Ordering

Effect stack order affects graph output. Reorder intent carries the moving
`EffectId` and a `Before(EffectId)` or `After(EffectId)` anchor. Snapshot indexes
never cross the Action Seam: stale, missing, and self-referential identities
fail closed, while an already-adjacent relation is an explicit no-op. A changed
move mutates the Clip effect vector in one author transaction; selection may be
reconciled afterward without a second Undo entry.

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

`coverage` owns the canonical Float32 Alpha algebra shared with Renderer. It
defines positive coverage, premultiplied-to-straight conversion, and weighted
straight-alpha interpolation without a semantic epsilon. Exact zero is the
only transparent identity after clamping; all positive values remain
observable through unary Effects, Blend/MultiInput joins, temporal sampling,
Transitions, spatial filtering, and repeated source-over. The scalar corpus
uses an independent f64 oracle at and below the smallest nonzero 16-bit UNORM
code. Real-wgpu compositor and Viewer-spatial tests prove the matching WGSL
policy; GPU absence remains addressed by the separate sealed qualification
work rather than weakening this semantic contract.

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
plausible-looking false color; that operation has been replaced rather than
retained as an execution fallback.

White Balance now compiles normalized temperature (±100 mired) and tint into a
working-RGB 3×3 matrix. Tint follows the normal to the CIE 1960 UCS Planckian
locus, and Bradford adaptation maps the native D65 or D60 white through the
exact Linear Rec.709, Linear Rec.2020, Linear P3-D65, or ACEScg/AP1 matrices.
The compiled matrix is the sole CPU/GPU payload, so neither backend reinterprets
color temperature and extended scene-linear/HDR values are not clipped.

Primaries retains the persisted compatibility identity `builtin.color_wheel`
while presenting itself as `Primaries`. Its executable contract is
shadow-preserving Lift, Gain, signed reciprocal Gamma, then final Offset. ASC
CDL has the new stable identity `builtin.asc_cdl` and implements the v1.2
no-clamp SOP formula followed by Saturation using the fixed ASC Rec.709
coefficients `[0.2126, 0.7152, 0.0722]`. Both operations preserve positive HDR
headroom. All three grade families use the same typed, animatable PropertyBag,
CPU Float32 reference, GPU Float32 lowering, graph/cache identity, Preview, and
Export path.

Crop is one ordinary built-in Effect, not a second `Clip` geometry model. Its
four stable, animatable percent parameters evaluate to normalized source-edge
insets and hard-clear pixel centers outside the remaining rectangle to
transparent black before the Clip spatial transform. CPU RGBA8, CPU Float32,
ROI/tiled Float32, and fused GPU Float32 share the complete source-frame
coordinate rule; a partial ROI never reinterprets its local origin as the
source origin. The operation is deterministic, stateless, pixel-local, part of
compiled graph/cache identity, and follows the same authoring, persistence,
Inspector, Undo/Redo, Preview, and Export Interfaces as every built-in Effect.

`Blend`, `Mask`, `MaskSource`, and ordered `MultiInput` nodes use the same float
working-frame contract. Mask rasterization produces native float coverage rather
than quantizing through an 8-bit matte. Its prepared geometry is immutable and
shared by all tiles of one attempt; retained geometry and worst-case row scratch
are included in the temporal hard-budget proof. The DAG executor transfers
owned buffers according to compiled node use counts, clones only for concurrently
live branch consumers, and recycles consumed buffers. Dissolve combines the
frame seed with the pixel index and is therefore compiled with a frame-dependent
cache policy.
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
plan rather than interpreted by the effects crate. ColorAdjust, creative
`Lut3D`, White Balance, Primaries, ASC CDL, Vignette, Grain, and Crop are
supported in source order with a bounded sixteen-op pass. Each primary grade
carries only Effects-compiled matrices/vectors; a LUT operation carries only
the immutable `PreparedLut3D` and animated intensity. Neither contains a wgpu
object and therefore neither can create a second renderer inside the effects
Module. Neighborhood spatial operations, custom processors, and branching graph nodes return typed
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

The renderer materializes the LUT subset of one fused plan as one RGBA32F 3D
atlas. Unique LUTs are sorted by complete semantic fingerprint and occupy
separate blue-axis slabs, so graph order and animated intensity reuse the same
immutable device upload. The shader applies the same declared-domain
normalization, red-fastest addressing, six-way tetrahedral interpolation, and
unbounded-source intensity blend as the CPU reference. Multiple LUT and point
grade nodes therefore stay in one pass without trilinear substitution. The
device cache is LRU bounded by both entry count and logical texture bytes;
oversized sets execute once without residency. Cache hits, misses, uploads,
evictions, bypasses, and current residency are exported as runtime evidence.
No-op LUT intensity binds the dummy resource and performs no upload.

## Product qualification

An executable Definition is not product completion by itself. The versioned
`visual-authoring-roundtrip-v1` Golden slice enters every claimed effect through
the same external product Action seam, requires one Author Generation and
Sequence Revision per mutation, and records stable `EffectId`, parameter, and
stack-order evidence. Visual report schema v10 extends that ordered stack from
Primary Color and LUT to finite-kernel Gaussian Blur, Sharpen, and one
Clip-local basic Mask authored through the closed Mask Action Interface.
Sharpen's static parameter is exercised through Undo/Redo; the Mask proves
stable Mask/shape-key identities, scalar parameters, Clip-local shape
animation, and shared graph execution. Durable close/reopen must preserve the
complete Effect order and Mask author state.

The execution half compiles the same reopened stack independently for Preview
and Export. Matching diagnostic signatures are necessary but not sufficient:
both graphs must expose exactly one Gaussian Blur and one Sharpen operation,
and the final Headless float-linear raster must be byte-identical before and
after reopen. This is a regression and integration contract, not an
independent numerical or visual reference. Product qualification still needs
such references before either effect can satisfy the complete operator DoD.
