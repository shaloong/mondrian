# Render Pipeline

The intended render path is shared by preview and export:

Tests that inspect prepared-program dependency currency must bind all assertions to
a stable Effect Definition Registry revision. A concurrent definition registration
is a legitimate program invalidation event, not evidence that an external resource
fingerprint changed.

```text
Raw/deserialized author snapshot or Program-cache miss
  + SequenceId + SequenceRevision + Effect Registry Revision
  -> canonical author fingerprint validation
  -> Prepared Visual Program
       -> Prepared Visual Schedule
       -> per-Clip Prepared Effect Programs / blockers
Trusted Preview Authoring Session + Author Generation
  -> Prepared Visual Program Binding
       -> constant-time validated reuse inside that trust scope
Frozen Export capture + checked range closure
  -> frozen Prepared Visual Program Binding set
Prepared Visual Program set + inclusive root range
  -> Prepared Visual Range Closure
       -> exact range-selected Programs / media / Transitions / title fonts
TimelineEvaluationRequest
  -> Prepared Visual Program query
  -> owner-scoped Timeline Frame Preparation
       -> ordinary Timeline Evaluation
       -> finite-temporal exact-time evaluation
  -> FlatVisualItem (Clip or two-input Transition)
  -> per-Sequence TimelineRenderPlan
  -> Dynamic Effect Evaluation + Consumer Execution Admission
  -> transient PreparedVisualFrameClosure
       -> exact nested instance/time/canvas/color/temporal bindings
       -> consumer-owned route evidence, no pixels
  -> TimelineRenderPlanElement materialization
  -> Clip Sampling / Generated Source
  -> Effect Graph Evaluation
  -> Layer Composite
  -> Color Transform
  -> Display or Export Encode
```

Timeline evaluation consumes exact Timeline Time in a declared Sequence domain
and an explicit video Evaluation Grid. `TimelineEvaluationRequest` carries one
`FramePosition`; its frame and time base cannot be separated or supplied
through parallel fields. Evaluation requires that position's time base to
exactly equal the prepared/source Sequence grid and requires a nonnegative
frame. A mismatch fails closed before placement selection: the renderer never
normalizes an equivalent-looking ratio, substitutes the source grid, or clamps
a negative request to frame zero.
Subframe shutter/temporal samples use the same Timeline Time representation and
do not introduce a renderer-private tick scale.

Flattening retains two independent exact coordinates for each active Clip:
`clip_time` for all Clip-owned visual processing and `source_sample` for
media/nested sampling. `source_sample` is a `SourceSampleTarget`, not a naked
time: its `Covering` or `StrictPredecessor` boundary is part of plan, cache, and
validation identity. Transform, Opacity, visual Effect compilation, Masks,
and Basic Title evaluation consume only `clip_time`; decoder and nested
Sequence Adapters consume only `source_sample`. Preview and Export invoke this
same lowering and may differ in scheduling or quality policy, never in time
interpretation. Timeline lowering obtains `source_sample` only through the
Clip's canonical source-time map; the renderer never reads or reconstructs a
parallel source range or speed field.

Production evaluation first prepares one immutable `PreparedVisualProgram` for
the exact Sequence author revision and Effect-definition registry revision. Its
`PreparedVisualSchedule` first selects active visible Tracks through one merged
global activity index, restores authored Track order, then queries only those
Tracks' Clip and Transition indexes. Its per-Clip `PreparedEffectProgram`
bindings retain definition, resource, execution-envelope, immutable zero-time
compiled graph, and immutable zero-time topology. Dynamic topology residency
belongs only to the consumer's bounded Effect Execution Session. The canonical
source-only identity graph is the sole
process-wide compiled graph; every non-identity compiled graph belongs to its
Prepared Program, one frame-local uncached reference evaluation, or the
consumer's bounded Effect Execution Session. Effect preparation
failure is retained as a Clip-local blocker, so an unavailable effect later in
the timeline does not disable an unrelated Preview region. Transition
definition identity, Property Bag, and opaque parameters are also captured once
behind one shared immutable snapshot; repeated interval queries copy only its
`Arc`, while exact progress and endpoint samples remain frame-local.
Unsupported Transition definitions or payloads become preparation-time
Transition blockers instead of being rediscovered on every active frame.

`Prepared Visual Author Fingerprint` is the conservative equality guard paired
with Sequence identity/revision in Program semantic identity. Its
versioned, domain-separated SHA-256 covers exact raster, frame rate, pixel
aspect, field order, color settings, authored Preview scale, Basic Title
safe-area margin, complete video Track/Clip records, and video Transitions.
Complete containers are deliberate: a new visual author field may cause a false
invalidation until the projection is revised, but it may not silently create a
false cache hit. Copy-on-write allocation identity, cache ownership, process
generation, and runtime resources are absent. Raw/deserialized snapshots and
Program-cache misses recompute and compare the complete fingerprint. A
generation-scoped live Preview binding reuses that proof only inside one exact
Authoring Session and monotonic Author Generation; neither value becomes part
of Program semantic identity. A manually assembled or deserialized snapshot
that reuses a Sequence ID/revision with different covered author state is
rejected before Program reuse.

`PreparedVisualRangeClosure` is the sole recursive dependency authority for one
inclusive root range. It pins one exact `PreparedVisualProgram` per selected
Sequence, requires one Effect-definition registry revision across the set, and
collects only reachable Sequence, file-backed picture Asset, Transition, and
Basic Title font identities. Each recursion step queries the Program's interval
index, projects nested retime ranges, expands finite temporal extent
conservatively, and treats unbounded extent as the whole child Sequence.
Duplicate Sequence identities, missing selected children, active-path cycles,
depth overflow, Program identity/fingerprint mismatch, or a mixed registry
revision fail closed. Export converts its half-open delivery interval to this
checked inclusive form for immutable dependency capture; Preview lookahead uses
the same closure and may binary-search for its first media-demand frame. The
range closure contains no per-frame Render Plan, placement-instance execution
node, pixels, decoder/GPU state, audio demand, or publication policy; those
belong to frame closure or the consumer.

One `TimelineRenderPlan` describes one Sequence instance only. The renderer
then constructs one transient `PreparedVisualFrameClosure` for the requested
root frame. This is the sole recursive visual execution authority shared by
Preview and Export. It fixes every exact child instance before source
materialization: nested `source_sample` is projected with the child's Evaluation
Grid using its exact covering/strict-predecessor rule, then becomes a covering
target for that resolved child frame; active-path cycles and the product depth limit fail
closed; each child receives its own authored/Preview canvas and composed
`ProgramColorContext`; ordinary placements, both Transition endpoint sides,
and every exact temporal request receive typed bindings. Equal
`(SequenceId, frame)` samples reached through different placements remain
distinct nodes with distinct instance paths, because state, diagnostics, and
future resource lifetime are placement-instance semantics. The closure stores
Plans, temporal demands, and consumer route evidence only. It owns no decoded
frame, GPU resource, pending state, export encoder, or presentation policy.
Closure construction builds one request-scoped `SequenceId` index over borrowed
immutable snapshots before evaluation. Duplicate identities fail closed before
any frame work. The separately named root role may alias the exact same
borrowed object already present in the canonical collection; another object
with the same identity is still ambiguous and rejected. Nested lookup is
constant-time and the index cannot outlive the request snapshots it references.
Before lowering each distinct Sequence, the closure resolves exactly one
`PreparedVisualProgramBinding` from the consumer-owned cache or frozen Export
snapshot and retains its exact `Arc<PreparedVisualProgram>` on every instance
node. The raw `checked` binding Interface validates Sequence ID, revision, and
the conservative canonical author fingerprint against the exact immutable
author snapshot. It deliberately does not compare against a later process-live
Effect Registry: a frozen Export job must remain executable after admission.
Live Preview binding creation additionally proves the Program's current Effect
Registry revision inside the consumer-owned cache. Export capture instead
stabilizes one Registry revision across its selected range, retries an
unpublished concurrent mutation, and freezes the checked Program set at queue
admission. Frame and range queries then repeat only constant-time typed
identity/revision/Program checks; they never serialize the complete Sequence or
query a live Registry. Preview scopes the binding set by open Authoring Session
plus monotonic Author Generation, and current-frame, forward-prefetch, preroll,
and cold range lookahead share it. A generation change retires the hot binding
set; a Project reopen rotates the entire Program-cache scope. Raw/deserialized
snapshots and scalar reference Adapters retain the fully checked binding
Interface and may not claim generation authority. Each Program
also freezes one typed `PreparedVisualMaterializationContract` containing its
authored raster, authored Preview raster scale, and Basic Title safe-area
margin; the node copies that exact contract rather than sampling Sequence
settings again. Child-canvas policy and the per-frame consumer callback read
the authored Preview scale only through the validated Program contract and
normalize it at their existing consumption seams. The callback otherwise
receives only the validated Program plus exact frame, execution raster, and
Program Color Context; it cannot evaluate the raw Sequence or substitute a
different Program.

Before rendering audio or creating an encoder process, Export preflights only
the selected half-open root-frame range. Each reachable frame is dynamically
evaluated through its `PreparedVisualProgram`, then receives one explicit
Effect route before any source resolution or pixel work. A complete, exact CPU
Float32 route always wins. Export considers the current bounded
CPU-prefix/GPU-suffix route only when the compiled execution envelope has an
empty homogeneous exact-mode intersection and therefore proves that an
explicit execution transition is required. A homogeneous CPU-NormalizedU8,
GPU-only, stateful, temporal, or domain-blocked graph remains unchanged for the
single compositor admission seam; inability to enter CPU Float32 is not itself
evidence of heterogeneity. The heterogeneous Adapter presently
accepts Media, Basic Title, and Nested Sequence placements; Solid Color,
Adjustment, and Transition inputs remain on a complete CPU route or fail
preflight as an unsupported placement. A selected heterogeneous route replaces
the element's graph with identity only in the downstream compositor plan, so
the Effect is neither omitted nor evaluated twice. The rewritten plan must
still pass ordinary Float32 compositor admission.

Float operation-shape support, color-domain resolvability, and exact
execution-mode admission remain independent proofs. Interactive Preview first
tries one complete CPU Float32 route, then may select one complete CPU
NormalizedU8 route as an explicit diagnosed degradation; it never combines
per-graph answers into an undeclared transfer path. Final Export retains
Float32 through the complete working composite and its single root output
boundary. Even an 8-bit SDR delivery authorizes quantization only there, never
an implicit intermediate RGBA8 composite. A plan whose only executable route
is NormalizedU8 therefore fails Export preflight before decode, audio render,
or encoder startup. Dynamic evaluation rejects animated builder or topology
failure; admission rejects temporal or continuity obligations, unsupported
exact modes, unresolvable domains in the selected representation, and every
heterogeneous shape or placement outside the bounded production Adapter.

Preflight freezes a versioned, conservative route-contract ledger containing
placement, maximum extent, execution-plan residency/step/backend/precision
shape, CPU/GPU partition, operation kinds, and resource upper bounds. Render
evaluation must match one frozen shape and remain inside all frozen maxima; its
exact compiled-graph fingerprint still binds CPU completion to the renderer
continuation. Thus animated parameter values may vary without fabricating a
second topology, while runtime shape drift fails before pixel execution.
Range preflight builds the same canonical frame closure used by rendering,
against the actual delivery raster and resolved Export color context; it no
longer maintains an Export-private nested walker or last-sample identity
shortcut. It pins immutable Programs and rejects the selected closure before
audio or encoder startup. Frame rendering builds and re-admits the exact
render-time closure immediately before media resolution, Generated Source
execution, nested materialization, or pixel work. A changed or misbehaving
dynamic builder cannot rely on earlier admission evidence to enter an
expensive consumer. A blocker outside the reachable closure cannot reject the
job; the same blocker fails closed when an exported frame can actually reach
it.

Preview performs that same dynamic evaluation before its canonical
media-demand collector can publish a decode request and before a
generated-source Adapter runs. The renderer prepares an immutable
`PreparedTimelinePreviewEffectRoutes` ledger for every Sequence node. The root
must prove either one complete CPU compositor route or one complete Viewer
Effect route in which every placement is full-GPU or carries an exact prepared
CPU-F32→GPU-F32 route. A non-root node must still prove the complete CPU route
because nested Preview currently materializes the child into a CPU working
frame. Media demand derives `cpu_working_required` from that frozen placement
entry, not from a second lowering attempt. Blend, projected geometry, layer
count, and actual GPU resource admission remain later typed Viewer proofs;
this ledger does not overclaim them. Unsupported Effect-route mixtures and
non-CPU nested nodes fail before decode instead of claiming an Adapter that the
materializer does not own.

Preview prefetch/preroll walks the already-prepared closure's nodes, and
Window/Headless materialization resolves only typed child bindings. The App
Adapter retains pending/cancellation, media scheduling, Viewer identity, and
pixel composition responsibilities, but it does not interpret nested time,
cycle/depth, child canvas/color, Transition side, or temporal membership. Its
materialization context holds no root/child Sequence collection: transform
projection, generated-title requests, nested frame identity, and temporal Solid
Color extent all consume the node's materialization contract.
The production Preview evaluation Interface accepts three cohesive values: one
immutable frame request binding the root author closure, raster, runtime scale,
and color context; one pair of media/title source Adapters; and one
generation-scoped execution binding carrying Program-cache trust, scratch
ownership, cancellation, author-snapshot identity, and dependency observation.
Callers cannot pass those authorities as independently ordered scalar
arguments or accidentally pair one generation's cancellation with another
Program binding.

Export keeps the raw root/nested Sequence snapshots only at the
Program-resolution and closure-preparation Seam. Once the closure exists, its
frame materialization context narrows to frozen media dependencies, Project
color environment, the job-local visual Session, cancellation, and
diagnostics. Heterogeneous route preparation resolves a nested canvas from the
child's frozen Program contract; Transition lowering resolves it from the
closure child node. Neither path can recover author state through
`SequenceId`.

Recursive CPU materialization has one explicit resource proof. Before any
child pixels are requested, the closure computes a checked conservative
RGBA32F active-byte bound: one output for every distinct prepared instance,
plus four canvases at the largest node extent for the compositor's worst
coexisting output/Transition endpoints/Effect result. Preview and Export admit
that bound against the same owner-scoped
`TimelineCpuWorkingSetGrant::max_active_bytes` later used by
`TimelineCompositeScratch`; arithmetic overflow or an insufficient grant fails
before allocation. Decoded inputs, Effect-session residency, and OCIO
processors remain under their independent grants. This conservative proof may
later be replaced by a tighter deterministic postorder/last-use schedule, but
no implementation may retain unaccounted nested working frames.

The non-recursive compositor has one narrower exception: a complete
source-only identity composite owns no output allocation. After ordinary graph
execution admission, it may share the immutable input `CpuColorFrame` only
when the plan contains exactly one Media layer, the background is transparent,
opacity is exactly one, blend mode and transform are identity, source and
requested extents match, and the source descriptor is the exact Sequence
working-space/linear-Float32/CPU/straight-coverage contract. Its active
compositor charge is therefore zero; the decoded input remains charged to its
separate owner. Any additional layer or output-contract change returns to the
ordinary conservative estimate.

The effects Module now has a graph-value-aware heterogeneous planning seam
behind that production admission rule. `CompiledEffectGraph` binds
each Definition stage to its exact incoming/output values and emitted nodes,
then retains the real implementation/Definition mode intersection per node.
`plan_effect_graph_value_execution(...)` assigns typed value residency,
declared transfers, producer completion waits/signals, shared fan-out
materializations, and completion-gated last-use releases directly over that
graph. A `Release` is a budget barrier that must retire the allocation before
the Adapter advances to the next step. The planner verifies the terminal live
set and checked host/device/transfer/count budgets, including the exact peak
number of simultaneously live device materializations. Viewer active-resource
admission consumes that count directly; it never divides device bytes by an
assumed RGBA32F frame size to guess a texture count. The older linear-stage
placement remains only a shallow feasibility diagnostic.

Renderer wraps `PreparedHeterogeneousEffectWork` in one cloneable
`PreparedHeterogeneousEffectRoute` that also freezes the compiled graph,
extent, graph-planning budget, and semantic fingerprint before source pixels
are materialized. Batch binding rejects a different extent or resource grant;
the worker executes this object and never replans it. The underlying work
consumes the planned CPU materialization DAG when the caller already owns a
scene-linear CPU Float32 working frame. It moves last-use values, clones only
live fan-out inputs, executes unary and join nodes through the shared Float32
primitives, admits the exact live-frame/kernel-scratch peak, and leaves one
explicit upload pending. Gaussian Blur followed by the exact Basic
Correction/Grain fused GPU point tail remains the spatial gate; a CPU
fan-out/Blend join followed by one GPU point operation proves that the prefix
is no longer flattened into a unary chain. A generated MaskSource/Mask join is
also executable in that prefix: its geometry preparation and raster loop use
the attempt's checkpoint Seam, and its retained geometry plus maximum row
scratch are charged beside the live pixel frames. After the single upload, the
prepared GPU suffix consumes the same value plan as explicit dispatch and
release steps: a shared GPU value can feed two point-operation branches and a
scene-linear Normal Blend join. Linear tails within the fused-operation capacity
remain one pass; longer tails split into exact admitted point dispatches. The
Preview and Export compare the prepared physical recording requirement with
their frozen GPU grant before CPU prefix execution. The wgpu Adapter repeats
materialization identity, wait, signal, release and terminal-live-set
validation before recording, and its real-device test matches the
complete scalar graph. One command buffer retains every referenced texture
until exact submission completion, so admission separately charges the
conservative non-aliasing recording bytes when that exceeds the abstract plan
peak. More than one backend transfer, GPU Mask/MultiInput/non-Normal joins,
color-domain conversion and temporal/stateful GPU dispatch still fail before
pixel execution. The returned evidence distinguishes completed CPU work from
pending upload and GPU output tokens.

Export submits that route through its job-local
`ExportVisualRenderSession`. The same Session owns the retained Effect
Execution Session, attempt generation, cancellation checkpoints, and one GPU
runtime whose context/resource pool is shared with the final output boundary.
The private `queue::visual_effect_execution` deep Module owns Export placement
vocabulary, canonical fingerprinting, frozen-ledger validation, typed attempt
errors, GPU completion/readback, and terminal policy behind that Session.
Exact graph-value route preparation remains renderer-owned and is the same
object consumed by Preview; Export no longer replans at pixel execution.
`queue::mod` remains the composition Seam: it owns the Session lifetime,
Prepared Visual Program cache, decoder/audio/encoder resources, and final
publication flow rather than exposing a second general-purpose visual runtime.
Execution is strictly CPU prefix → upload → exact GPU suffix → submit/wait →
readback. The CPU completion and GPU binding must agree on complete graph
fingerprint, generation, extent, frame seed, and `WorkingColorSpace`; the
readback must return the same working-space, linear-Float32, CPU-resident,
straight-coverage-alpha frame descriptor. Only then does the ordinary
transform/blend compositor consume the identity-rewritten element.
The readback buffer may enter `map_async` only after the encoder that writes it
has been submitted. A move-only map lease unmaps or cancels that mapping on
every success, cancellation, deadline, callback, poll, and unpack exit. Export
drives the exact submission in bounded poll slices under the attempt token and
one non-renewing monotonic deadline; submitted resources without observed
completion are dropped and poison the job-local heterogeneous runtime rather
than entering the shared texture pool.

Route selection is a pre-start decision: a complete exact CPU route is retained
even if the heterogeneous route is also preparable. After a heterogeneous CPU
prefix starts, cancellation, upload, recording, submission, wait, readback, or
evidence failure is terminal for that attempt. No path may reinterpret the
whole graph on CPU after partial work. Viewer/UI lowering does not synchronously
perform this Export-owned execution.

Preview consumes the same prepared heterogeneous work through three distinct
Modules rather than reproducing Export's offline Session. Pure
`preview_viewer_plan` lowering first attempts the ordinary complete-GPU Viewer
plan. Only `EffectRequiresCpu` opens the bounded media-input heterogeneous
route; unsupported blend/transform/placement, missing CPU-working input,
excess layer count, or an invalid batch fails closed. Planning performs no
pixel work. `preview_visual_execution_task` then submits the addressed
CPU-prefix batch to its own bounded `FrameWorkBroker` and dedicated serial
worker. The CPU-prefix grant and no-readback GPU-continuation grant are frozen
from the same immutable Preview resource decision and travel with that
attempt.

The successful worker result deliberately retains a move-only Broker execution
lease. That lease crosses the App result-pump, Viewer recording, queue
submission, and actual GPU-completion Seams; replacing the downstream Effect
with identity is legal only after addressed CPU completions match the exact
continuation bindings. Each Adapter callback carries the exact
`ViewerGpuSubmissionId`; its retained submission owner carries the move-only
Broker lease and private `FrameExecutionId`. A late callback from an abandoned
submission is therefore discarded before it can reach or consume a newer
in-flight candidate. Window and Headless hand the matching wgpu
`SubmissionIndex` to the same `app::viewer_gpu_device_progress` Module. A
move-only progress permit is reserved before recording; after queue submission
the Adapter first installs the retained owner and exact callback, then commits
the returned index through that permit without a fallible post-submit
transition. Its dedicated non-UI worker performs bounded exact-index
`PollType::Wait` calls and, if necessary, bounded latest-submission cleanup
waits; native wait success can drive the callback but cannot replace its typed
completion evidence. `request_device` is followed immediately by one
device-generation progress owner and the generation's unique
`set_device_lost_callback`; runtime, timestamp, surface-renderer, and queue
consumers are constructed only after that callback exists. The work-done
callback only stages its lifecycle notice and marks a cleanup ticket. Because
wgpu may invoke work-done before device-lost in the same native poll, the
progress worker re-reads shared generation health after `Device::poll`
returns and only then emits the post-poll completion barrier. Adapter lifecycle
polling cannot consume the staged callback before that barrier. Adapter event loops only drain the callback/progress
channels and enforce the caller's one non-renewing monotonic deadline. Every
bounded wait iteration has an 8 ms ceiling for active submission progress,
pre-submit renderer cleanup, and device retirement. On
Windows, any remainder after the native poll uses a thread-local high-resolution
waitable timer rather than `thread::park_timeout`; the latter commonly expands
short bounds to a 15.6/31 ms scheduler tick and would turn GPU completion
observation into a presentation-phase bottleneck. This pacing is not completion
authority, and the worker always rechecks the callback and generation-health
level predicates.
continuation in a Viewer batch shares that submission, so Headless observes it
once and completes the remaining evidence without serial waits or deadline
renewal. Neither the ordinary nor heterogeneous Headless route may use an
unbounded device wait, and neither Adapter may add a competing main-loop
`Device::poll` cadence for Viewer-submission completion. Optional timestamp
query maintenance remains separate telemetry and conveys no lifecycle
authority. Pre-submit renderer backpressure converts the already-reserved
progress permit into an explicit renderer-cleanup barrier; the device worker
drives that unindexed internal work and wakes a retry without fabricating a
Viewer submission completion. An ordinary complete-GPU output may publish after
queue-ordered submission while its capacity-one owner remains retained for
physical completion evidence. A heterogeneous output cannot publish before its
exact callback and completed-batch validation. Timeout, device-poll failure, or
authority revocation quarantines the submitted owner without freeing its visual
terminal authority or Frame Store media-protection leases; its exact late
callback is retirement-only and cannot publish. Conditional clear by semantic
output key is forbidden: two submissions may resolve the same semantic output.
A non-timeout device-progress error terminalizes the whole device generation,
not just the indexed batch: future permits are rejected, Headless returns typed
terminal evidence, and Window removes that generation's output and explicitly
falls back until device rebuild. The device-lost callback can terminalize an
idle generation with no submission identity. Every ordinary and heterogeneous
publication seam rechecks the shared terminal, and an idle loss revokes an
already-current ordinary physical artifact. Explicit wgpu `Destroyed` and an
unexpected `Unknown` loss remain distinct typed causes even though both make
wgpu work terminal. Native Window/surface replacement carries the
same progress owner, completion lifecycle, quarantined resource envelope, and
deferred cleanup forward; it must not drop an old frame merely because the UI
surface changed.

Adapter teardown closes admission and appends one FIFO retirement envelope to
the existing progress worker; the Window/UI or Headless caller performs no
poll, join, or cancellation wait. The envelope retains the device, queue,
execution runtime, callback lifecycle, current physical output lease, optional
timestamp resources, deferred cleanup, and media/native owners until exact
callback completion or actual wgpu terminal evidence. wgpu loss never replaces
the independent D3D `copy_ready` proof: a decoder source retires only after its
fence completes or that native fence returns the typed device-removed sentinel.
Other native retirement errors remain fail-closed. A generation slot is
pre-reserved before its worker is created and is held through retirement. The
process admits at most four active-or-retiring Viewer device generations; a
worker panic/disconnect quarantines both envelope and admission token, so
repeated rebuilds cannot create an unbounded detached-worker or envelope leak.
The Headless Adapter assigns every submission a unique resource key, retains
the move-only `ViewerGpuPresentationOutputLease` in the submission owner, and
moves it into a separate capacity-one current physical slot only after
publication succeeds. Preview retains only cloneable metadata. Reuse and
conditional clear require the exact `(PreviewOutputKey, resource_key)` artifact,
and timeout/device/authority retirement clears that artifact only when the
current slot still names the same physical submission. A current on-time result may publish, a missed
Playback deadline emits `Late`, a current execution failure emits `Failed`,
and superseded/non-Playback work is released without fabricating a second
terminal delivery. No CPU rerun is allowed after the prefix starts.

`EffectExecutionDemand` derives a checked signed finite/unbounded temporal
window and exact-or-conservative input ROI from the same prepared execution
envelope. `prepare_temporal_frame_execution` compiles a time-expanded value
projection over the sole `CompiledEffectGraph` IR. Negative history and
positive lookahead coexist; exact duplicate source times and graph values are
requested once, then retained through their final expanded edge use. A
temporal stage samples its stable Definition-stage input. Earlier Effect stages
are reevaluated from the same `PreparedEffectProgram` at the exact sampled Clip
time, so animated parameters, dynamic topology, frame seeds, and Masks cannot
borrow output-time state. Each cross-time edge strictly lowers the stage index;
unstable contracts, internal same-stage addresses, unbounded expansion, or a
sampled spatial demand outside root coverage fail closed. Immutable Mask
geometry uses global frame coordinates per exact-time graph context and is
shared by direct or tiled execution; retained geometry and bounded row scratch
are admitted as part of the Effect working set.
`PreparedTemporalFrameSet` is the decode-free provider used by the bounded
CPU-Float32 scalar reference. Its demand batch carries the checked exact
Float32 source-coverage byte total. Preview and Export admit that total against
their CPU active-working-set grant before materialization; the Effect Session
then accounts retained coverage in the same execution peak as its own output
and tile allocations. Crossing owner-domain zero is not a boundary event. The
exact `TimelineClipExecutionRef` and prepared placement mapping decide whether
a requested source handle exists.

That scalar reference keeps the exact input ROI rather than expanding it into
a zero-filled complete frame. Its raster-region contract preserves global
frame coordinates for coordinate-dependent kernels, finite-support kernels
consume only their implementation-derived halo, and complete-frame-only
kernels fail closed on a partial region. The expanded schedule/use counts are
the liveness authority: last-use values move, fan-out clones only remain while
needed, and joins release inputs immediately. A pure dry-run proves the exact
scalar live set before pixel work. Resident graph values, concrete kernel
scratch, retained frozen source coverage, and one final output allocation share
the Effect Session hard grant. The final `Vec` becomes `Arc<Vec<_>>` without a
full-frame pixel copy. Over/under-consumed edges or a byte-ledger mismatch fail
closed.

Preview and Export use this same execution boundary. A complete request that
does not fit directly is partitioned by the Effect Session into a deterministic
non-overlapping 2D schedule; each tile reuses the frozen source coverage, is
proved from the same ROI and liveness contracts, and is stitched only into the
one retained output. The 4,096-tile hard limit, indivisible-budget failure and
cancellation all stop before partial publication. Internal tiles are
attempt-local and never enter the Effect output cache; only the complete frame
may publish there. Direct and tiled production
paths have exact pixel-parity and peak-budget tests, including a Path Mask after
a finite temporal fan-out/join DAG and a signed past/future multi-tap DAG with
duplicate samples, plus an effected upstream stage whose parameters differ at
the sampled instant. This evidence applies only to the bounded CPU-Float32
temporal scalar contract; it is not evidence of GPU temporal tiling, internal
same-stage temporal addressing, or stateful continuity execution.

Export connects this tracer to its job-local decoder and closure-backed nested
materializer. It resolves every typed demand address first, adapts the result to
straight-alpha parent working pixels, freezes the complete batch, and executes
under the export generation/cancellation Session before replacing that graph
with identity for ordinary compositing. Media and Solid Color are admitted; a
nested source is admitted only when its closure-bound exact raster already
matches the Effect extent. Basic Title temporal input, Adjustment-stack temporal
input, unbounded input, stateful continuity, and internal same-stage temporal
input fail closed. Preview emits the same exact batch into its existing
asynchronous media Scheduler. Temporal media keys require CPU-working output
and therefore cannot coalesce with an opaque native-surface request. A missing
Frame Store value returns typed Temporal Pending after all demands have been
submitted; only a complete generation-bound set is frozen and executed under
the Preview generation cancellation token. Viewer evaluation neither waits for
decode nor substitutes the current/displayed frame as history.

Preview Runtime owns a bounded reusable program cache across root/nested Viewer
evaluation, prefetch, and preroll. Export instead treats selected-range
dependency capture as the publication boundary for one exact visual Program
set. Capture builds the root and selected nested closure against one stable
Effect registry revision; a concurrent mutation discards and retries that
unpublished candidate. The same Programs produce the frozen media, Transition,
and nested-Sequence evidence. Queue admission validates the exact Sequence
revisions, range, Program-derived evidence, and physical dependency
completeness, then the job-local Export Visual Render Session installs those
exact `Arc<PreparedVisualProgram>` values. Worker preflight and frame rendering
resolve exclusively from that installed set and never rebuild from the live
registry. Unrelated Sequences are not prepared merely because the Project
snapshot contains them. In Preview, a revision miss recompiles; successful
publication evicts older
author/definition revisions of that Sequence. A structurally failed preparation
never replaces the last valid entry. External resources use an explicit
low-frequency revalidation Seam;
cache lookup performs no filesystem work. Cross-author-revision reuse is
limited to prepared Clip programs without external dependencies, so a Timeline
edit cannot carry an unobserved stale LUT payload into a new revision. The
cache additionally owns a private Authoring/Open scope generation. Sequence and
Effect-registry revisions authorize hits and incremental Clip-program reuse only
inside that scope. `rotate_scope` advances the address generation and clears all
resident programs as one operation; a long-lived Preview Runtime invokes it
when `AuthoringSessionId` changes or project lifecycle cancellation begins.
Within that scope, `bind_author_snapshot` computes the complete conservative
fingerprint at most once per selected Sequence and monotonic author
generation. It retains a typed preparation or residency blocker for the same
generation, Sequence revision, Effect-registry revision, and unchanged
residency policy, so an unadmittable Program cannot create a presentation-rate
retry loop. An actual cache reconfiguration invalidates only retained
`ResidencyRejected` blockers before the next lookup; structural preparation
failures remain cached, while the same author snapshot can be admitted
immediately after its grant grows. Cache diagnostics publish binding
hits/misses, generation rotations, fingerprint evaluations, retained failure
hits, and rejected residency.

Each `PreparedVisualProgramCache` also directly owns one
`LutPreparationCache` with a separate entry and conservative logical-byte grant
from its cache configuration. Program preparation borrows that owner only for
low-frequency resource binding. It reads and hashes the complete `.cube` file
before accepting a resident payload, then shares the immutable prepared LUT
only among Programs owned by that visual cache. `rotate_scope` and `clear`
retire both Program and LUT residency, while `reconfigure` synchronously
applies both grants, trims any newly over-budget residency, and retires
resource-policy-dependent negative admission evidence. Because Preview Runtime
and every Export attempt construct different visual caches, neither can evict
or prolong the other's LUT residency. The logical charge used here is
cache-admission evidence, not allocator, GPU-memory, Working Set, or RSS
measurement.

The same ownership boundary applies to Effect pixels. Preview's retained
`TimelineCompositeScratch` owns one `EffectExecutionSession` and binds it at the
sole Preview generation activation/invalidation seam. Export's job-local
`ExportVisualRenderSession` owns a different compositor scratch and binds the
queue's exact monotonic attempt generation. Encoded, Float32, per-node, and
temporal pixel caches plus dynamic Effect-graph topology variants share one
aggregate Session entry/byte budget. A Prepared Effect/Visual Program retains
only immutable zero-time topology, so frame evaluation cannot silently grow
the charge already admitted by the visual-program cache. Every production
frame-graph binding must use the same retained Session that later executes that
Preview or Export attempt; only scalar reference/test helpers may use uncached
topology compilation. An over-budget dynamic topology remains correct for that
call and simply receives no reuse. GPU lowering plans and deterministic
blockers use an additional small entry/byte grant on the same owner. All
residency classes are synchronously retired on generation change; no
process-global Effect frame/non-identity-compiled-graph/topology/plan cache or
per-Sequence-frame scratch can outlive its consumer. Preview Viewer lowering
borrows that retained Preview
scratch, while Export diagnostics borrow the job-local scratch. Preview
resource policy may resize only its own Session and cannot evict an Export job.

`TimelineCompositeScratch::prepare_timeline_frame_execution` is the deep
production entry for one Sequence frame. It binds the generation before work,
evaluates the ordinary Render Plan with the retained Session, and prepares
every root/sample temporal graph through that same Session. Preview and Export
receive one `PreparedTimelineFrameExecution` containing the rewritten ordinary
plan, exact temporal batches, and aggregate source-coverage bytes; neither
Adapter stitches a session-free temporal pass onto a separately evaluated
plan. The session-free Timeline temporal wrappers exist only in test builds for
scalar parity evidence and are absent from production artifacts.

The compositor scratch also owns one non-`Send`
`RenderCpuColorExecutionSession`. Source/import transforms, effect-domain
edges, nested working-to-working conversion, Program Output, and monitor
adaptation reuse its bounded OCIO processors on the same execution owner.
Export owns the same resource only inside one job; Thumbnail owns one inside
its dedicated worker. Convenience color helpers construct an uncached
reference Session, so production loops must call their `_with_session`
variants. No processor cache is hidden in thread-local or process-global state.
This prevents a reopened Project with deliberately identical durable IDs and
revisions from reusing process-local programs produced by the prior open
lifetime. Export render sessions are snapshot-local cache owners and therefore
start with a fresh scope. The
`PreparedVisualProgram::dependency_refresh_required` Interface is reserved for
background observation: registry revision, retryable external-change blockers,
and stale prepared dependencies request atomic eviction; permanent
author/definition blockers do not create polling churn. Production Preview
registers every root and nested program with one bounded, low-frequency
`PreviewVisualDependencyObserver`. Its worker performs dependency reads away
from the UI/render hot paths and returns the exact immutable program instance.
The Preview composition root discards superseded results, evicts the affected
Sequence program, and clears derived root Viewer output before the paused
exact-current fast path. Decoded media residency remains untouched because its
physical identity is independent. Observer startup failure or unexpected worker
exit is a typed Preview blocker: the first observation clears derived Viewer
state and later polls do not repeatedly rotate generation. Export instead binds
that immutable job-local Program snapshot and preflights it before encoder
admission; render-time registry lookup cannot reinterpret the admitted job.
The direct Sequence `RenderPlanSource` remains a crate-private scalar reference
Adapter used by renderer unit parity tests. Its evaluator and direct color
diagnostic collector are not public product Interfaces, so App, Golden,
Preview, and Export code cannot accidentally create a second production
interpretation.

Clip transforms are authored against stable source and Sequence picture
extents, not against whichever decode/output sizes an execution happens to use.
Preview and Export therefore lower them through the renderer-owned
`project_affine_to_sampled_extents`: source authoring extent → decoded sampled
extent and Sequence authoring extent → composite sampled extent. Export freezes
the full source extent in each file dependency and includes requested decode
width/height in its cache key. Media, nested Sequence, Solid Color, and both
Transition endpoints use this same projection; Basic Title uses the equivalent
cropped-title projection. A 4K-authored clip exported at 1080p must keep the
same composition, not apply its auto-fit scale a second time.

## Current Implementation

`mondrian-renderer::timeline_render_plan` lowers one Sequence frame through one
internal Implementation. Production calls
`TimelineCompositeScratch::prepare_timeline_frame_execution(...)`, which uses
`evaluate_prepared_visual_program_with_session(program, request, session)` and
the Session-owned finite-temporal preparation under one generation. The session-free
`evaluate_prepared_visual_program(program, request)` and direct
`evaluate_timeline_render_plan(source, request)` remain scalar reference
Adapters for tests and diagnostics. All three enter the same Clip/Transition
lowering after selecting only their Effect-graph resolver. The request carries:

- the exact nonnegative `FramePosition` on the Sequence Evaluation Grid
- render intent: preview, export, thumbnail, or analysis
- render quality: interactive, draft, or final
- color target: display, export, or working space
- scheduler policy: whether frame dropping is allowed
- preview/export resolution scale

The result is a `TimelineRenderPlan` that retains that single exact
`FramePosition`, ordered `TimelineRenderPlanElement` values, and diagnostics
for active visual items, emitted elements, zero-opacity skips, and
unrenderable skips. It does not expose a parallel numeric frame and time base.

`collect_timeline_color_diagnostics(...)` reports the clip override, working
space, and output space together with their canonical `ColorEncodingSpec`
values. `collect_timeline_color_diagnostics_with_display_view(...)` attaches
the resolved OCIO display/view for presentation diagnostics. Renderer
diagnostics must consume `ColorSpace::encoding()` rather than duplicating
color-space metadata.

`evaluate_prepared_visual_program_with_session(...)` is the production
render-plan entry point. Callers choose an explicit
`TimelineEvaluationRequest` intent and an owner-scoped execution Session so
Preview, Export, Thumbnail, and analysis paths cannot accidentally share
ambiguous defaults or process-global state. `RenderPlanSource` remains the flat
semantic Interface between placement selection and lowering; the prepared
schedule and direct reference Adapter must return identical items at every
exact time.

`TimelineRenderPlanElement` variants are:

- `Media`
- `Adjustment`
- `SolidColor`
- `BasicTitle`
- `NestedSequence`
- `CrossDissolve`

Each element carries opacity, blend mode, transforms where applicable, effect graph, frame seed, and color/media interpretation data.

`CrossDissolve` contains two typed endpoint plans (`Media`, `SolidColor`,
`BasicTitle`, `NestedSequence`, or explicit transparent coverage) and one
coefficient derived from exact elapsed/duration author time. Timeline
evaluation replaces the two endpoint placements with this single Track-stack
item, evaluates Clip effects and transforms independently for each endpoint,
and never clamps a demanded source time to zero. Unknown plugin Transition
definitions and unsupported built-in parameter payloads fail plan compilation
instead of substituting a Cross Dissolve.

### Basic Title generated-source boundary

`TimelineBasicTitlePlan` carries one fully evaluated Basic Title at exact
Clip-local visual author time plus the ordinary Clip opacity, blend, transform,
effect graph, and frame seed. `BasicTitleRasterizer` is the single Preview and
Export generation Interface. It:

- resolves the exact named system-font family, weight, and style;
- fingerprints the resolved face bytes plus face index;
- binds the first fingerprint observed for each family/weight/style query to
  the owning Preview or Export generation Session, rechecks the dependency
  before cache reuse and after rasterization, and fails closed if it changes;
- rejects a missing family, inaccessible face data, or any shaping run that
  selects an undeclared fallback face;
- shapes against the Sequence canvas and persisted total `title_safe_margin`;
- emits a tightly cropped straight-alpha `CpuColorFrame` in the Sequence
  working space plus an affine mapping back to full-resolution author
  coordinates;
- keys and bounds its Session cache by evaluated title semantics, author and
  sampled geometry, title-safe margin, working space, and resolved font
  fingerprint. The complete 32-byte `BasicTitleRasterRequestIdentity` and
  `BasicTitleRasterIdentity` are cache authority; no truncated `u64` projection
  may authorize request coalescing or frame reuse.

A font-catalog refresh rotates the Preview generation Session; it must not
silently mutate an existing Session's output identity. Export resolves every
static family/weight/style query reachable through the selected root/nested
range at queue admission, retains deduplicated exact font-source bytes under a
separate hard byte grant, and constructs a font database containing only those
in-memory sources. Its job Session never queries the live system catalog.
Nested Sequences and Transition endpoints use the same frozen set.

The cropped generated frame is lowered as an ordinary media-shaped compositor
input. Clip Transform, effects, masks, blend, Cross Dissolve, nested working
space conversion, and root output transform therefore have one interpretation;
there is no Preview-only or Export-only text compositor. An empty title yields a
transparent generated frame. A missing dependency blocks the frame/job rather
than substituting another font or claiming successful output.

Preview owns a bounded background title worker, pending state, and result
residency (entry and host-byte budgets, with at most one bounded oversize
current result) so font discovery, shaping, allocation, and an accumulation of
large title rasters never execute on or exhaust the winit/UI thread. Export
owns one `ExportVisualRenderSession` for the complete job and passes it through
closure-addressed nested materialization and Transition endpoints, allowing
identical title requests
to reuse the same bounded raster cache without introducing global mutable font
state. Preview shutdown signals the worker before disconnecting transport, so
queued obsolete title jobs are skipped and shutdown waits for at most the one
bounded CPU generation already executing. An unexpected worker disconnect
converts every pending request to a typed terminal failure; it cannot leave the
Viewer in an infinite Pending loop.

The same Export visual Session also owns the job's explicit media decode
context. Root media, nested Sequences, and both Transition endpoints lower
through that one context and the job's shared cancellation token. A video
request always binds the frozen dependency fingerprint; the media Adapter
checks it before Session admission and after frame materialization. It also
uses the frozen physical video-stream index; missing or invalid bindings fail
instead of falling back to stream zero. The
decoded-and-transformed layer retains that exact revision as execution
evidence together with the stream binding, so its frame-local cache entry and
the FFmpeg work that produced it cannot silently refer to different source
generations or streams. Terminal job cleanup
retires decoder Sessions together with the visual Session; no product Export
path relies on thread-local decoder lifetime.

Effect resource/definition binding and static topology compilation belong to
Prepared Visual Program construction, not repeated frame lowering. Frame
evaluation samples animated parameters and binds them to an admitted topology;
that step remains fallible. An enabled Effect with no executable definition,
unavailable runtime, invalid resource, panicking builder, or invalid graph
becomes a typed Clip blocker and returns
`MondrianError::EffectGraphEvaluationFailed` when active. Preview and Export
therefore never publish a false identity frame; only an explicitly disabled
Effect is identity. Backend-specific admission remains separate from
author/resource preparation.

`TimelineMediaPlan::source_sample` is the sole media-decode target emitted by
Timeline evaluation. It remains exact through Preview scheduling, frame-store
identity, Export decode caching, and `PreviewDecodeRequest`; those consumers do
not rebuild seconds or microseconds and cannot omit the sampling boundary from
cache identity. Without a media frame-rate override, the complete Clip target
is preserved until the decoder stream grid. An explicit override consumes its
boundary once on that declared source Evaluation Grid. A
`TimelineNestedSequencePlan` likewise retains the complete child-local target;
Preview and Export receive the child-grid frame already projected once by the
renderer-owned `PreparedVisualFrameClosure`, then materialize that addressed
child evaluation. Parent frame numbers are never reused as child frame numbers,
and no layer emulates reverse by subtracting an epsilon or nominal frame.

`timeline_composite` exposes one color-managed composition contract:
`composite_timeline_elements_color_frame(...)`. It returns a typed
`CpuColorFrame` whose descriptor records domain, encoding, residency, dimensions,
color space, and RGB/coverage alpha association. Viewer preview and export must consume this typed working-frame
contract, then apply their respective working -> output boundary through
`RenderOutputColorBoundary` and renderer color stage execution helpers.
Bare RGBA8 buffers are valid only at source import, debug/golden snapshot, UI
presentation readback, and CPU encoder boundaries. They are not a renderer-stage
exchange format.

Program and nested-sequence working canvases use transparent black as their
initial value and retain straight coverage alpha through CPU and GPU
compositing. Viewer background/checkerboard treatment is presentation-only and
must not mutate the Program frame. The active root Sequence may publish this as
a texture-free transparent presentation; the Viewer user preference chooses
checkerboard or display black behind it without creating a render layer.
Export selects an explicit `ExportAlphaMode`: `Preserve` keeps straight alpha
only for a validated alpha-capable codec/container contract, while
`FlattenBlack` composites over scene-linear black before the final output
transform. Codec selection alone
must never imply alpha preservation, and setting alpha opaque after an encoded
output transform is not a valid flatten operation.

Cross Dissolve is a compositor operation, not two ordinary layers with reduced
opacity. At its Track position, the compositor prepares each endpoint
independently (including its own opacity, transform, and Clip effect graph),
then interpolates the two prepared results in the scene-linear working space.
RGB is associated with coverage for the interpolation and restored to the
public straight-alpha contract afterward. This preserves transparent edges and
endpoint semantics; sequential source-over layers would produce different and
incorrect weights. Preview and Export consume the same typed operation and CPU
reference formula. The bounded Viewer GPU graph reuses the ordinary source
preparation Interface for both Transition inputs, materializes those branches,
and records one dedicated coverage-correct interpolation pass. A real-wgpu
readback test compares that pass per channel with the shared CPU formula; GPU
lowering must never substitute an approximate opacity pair.

`LinearFloatSource` accepts either an external scene-linear `ColorSpace` or an
internal `WorkingColorSpace`. Its frame descriptor preserves that role through
`ColorFrameSpace::Color` versus `ColorFrameSpace::Working`, and the CPU input
executor requests the corresponding typed OCIO endpoint before producing the
sequence working frame. When a decoder supplies external ACES2065-1/ACEScg or
linear RGB float samples, the renderer bypasses RGBA8 without mislabeling them
as pre-existing working frames. Media decoders must opt into this entry point
explicitly.

Nested sequences return typed linear `CpuColorFrame` values to their parent.
Child working identities may be converted to the parent working identity by an
explicit OCIO working-to-working processor, but they are never sent through a
display/export transform or RGBA8 quantization during closure-backed nested
materialization. Preview and export apply their display/deliverable transform
exactly once at the root boundary.

The CPU timeline compositor keeps identity-transform media, solid-color layers,
and float-capable adjustment layers in the typed float/linear working frame.
Before allocating pixels, the production typed-color entry point derives a
`TimelineCpuWorkingSetEstimate` from the exact output extent, selected
whole-frame precision, active elements, source extents, and Transition
endpoints. `TimelineCpuWorkingSetGrant` independently bounds transient frames
and reusable compositor scratch. Cross Dissolve therefore accounts for the
output canvas plus both endpoint canvases at the same time, and also accounts
for an endpoint Effect result when that value can coexist with all three.
Legacy Preview fallback accounts for the encoded canvas and the Float32
working-frame conversion concurrently. Solid materialization, encoded-source
adaptation, Effect output, and Adjustment scratch are retained-buffer
requirements rather than undocumented allocations. An owner that cannot admit
either bound receives `TimelineCpuWorkingSetError` before the first output
allocation; it may select a coarser future Preview request or fail Export
capability validation, but it may not begin a frame and silently change
semantics under pressure. The grant excludes decoded input payloads, Effect
Session residency, and OCIO processor residency because those resources have
separate owner-scoped grants. Reducing the retained grant synchronously drops
compositor scratch that no longer fits.

The GPU Viewer has the same pre-allocation invariant at its own execution
boundary. `estimate_viewer_gpu_active_working_set()` inspects the complete
`ViewerGpuExecutionRequest` and returns a checked
`ViewerGpuActiveWorkingSetEstimate` before command recording. Its independent
stage totals cover every explicitly supplied CPU upload, renderer-owned native
bridge/encoded-RGB/working output, heterogeneous continuation peak,
external-domain Effect round trip, both Cross Dissolve endpoints and output,
Adjustment segment/blend, working accumulators, spatial
prefilter/horizontal/output textures, Program Output, visible Program scopes,
monitor adaptation, and display-calibration output/LUT. The native bridge
reserves a 2:1 storage-pixel envelope over its visible NV12/P010 extent, and
D3D12 descriptor validation rejects larger codec padding before growing the
bridge pool. The media-owned decoder surface remains under the Frame Store
native-resource grant instead of being counted twice. Native/GPU/CPU source
alternatives are summed conservatively because a failed preferred path can
have created partial resources before its correctness fallback is selected.
All extent, count, and byte arithmetic is checked.

`ViewerGpuExecutionResourceGrant` therefore contains two unrelated limits:
the exact-contract pool's idle count/bytes and the pressure-stable maximum
active texture count/bytes for the request plus the Viewer owner's presentation
residency. The active limit is admitted before the first upload, native import,
Effect, composite, or output texture is created. Failure returns a typed
`ViewerGpuActiveWorkingSetAdmissionError`; execution does not reduce precision,
reinterpret the Effect graph, or conceal the rejection with a CPU fallback.
`ViewerGpuActiveWorkingSetDiagnostics` retains the installed grant and most
recent admitted estimate. Driver padding, swapchain ownership, OCIO
pipeline/LUT caches, decoded CPU payloads, and idle textures remain separately
governed and are not falsely charged to this frame-local estimate.

The final renderer GPU output boundary has a narrower equivalent contract for
offline and non-Viewer callers. Once color lowering has produced an exact
`RenderGpuOutputStageResourcePlan`,
`estimate_render_gpu_output_active_working_set()` charges the referenced
working input texture, encoded output texture, and padded readback buffer by
checked bytes and resource count. A caller-supplied
`RenderGpuOutputExecutionResourceGrant` is admitted before backend pipelines,
textures, or the readback buffer are materialized. Export freezes this grant
with its attempt; rejection is a typed
`RenderGpuOutputActiveWorkingSetAdmissionError` and an explicit Export GPU
fallback reason. Idle output-texture retention remains a separate pool policy.
The estimate deliberately excludes OCIO backend caches and an upstream
heterogeneous continuation because those have independent bounded owners.

Graph identity is only a pixel fast path after execution admission. Both the
float and RGBA8 entry points first admit every graph that can actually execute,
including active Adjustment layers and both Transition endpoints; an
identity-shaped graph carrying temporal, ordered-state, or unsupported
exact-mode obligations therefore fails before the output buffer is
written. A legal CPU-Float32 identity stays on the working path, while a legal
CPU-NormalizedU8-only identity selects the encoded route only for an explicitly
degradable Preview. The same graph fails Final Export preflight instead of
producing a nominally high-bit-depth file from an already quantized working
composite. Export repeats this invariant after actual layer composition:
observing any legacy-RGBA8 diagnostic is a terminal render error before the root
output transform. Identity graphs with incompatible exact modes cannot make a
mixed frame appear executable. A legal source-only identity graph remains a
zero-operation passthrough. `TimelineCompositeExecutionDiagnostics` proves
whether this zero-copy route actually executed; callers and performance gates
must not infer passthrough from layer count. In a multi-layer Float32 plan, an
eligible first layer may initialize the transparent canvas directly, but that
is separately reported and never claims whole-frame passthrough. When the first
two elements are both exact full-frame CPU-working media with identity Effect
graphs, identity transforms, Normal blend, effective opacity, and a transparent
background, the compositor may fuse first-layer alpha initialization and the
second source-over operation into one output write. Any missing condition
selects the ordinary scalar element loop. The fused operation retains the same
straight-alpha float equation, including canonical transparent initialization:
an effective alpha at or below the compositor epsilon publishes zero RGBA
rather than preserving hidden source RGB. It has separate execution evidence
so a performance gate cannot infer it from an authored two-layer shape. The
named 4K fusion gate fixes resolution, rate, layer count, and blend shape,
retains the complete output across release optimization, and requires both
per-frame fusion counters and deterministic canonical pixel samples.

Before either optimization or pixel execution, the typed compositor validates
every Media and Transition endpoint against its exact working-frame descriptor
and storage extent. A Rec.709 working frame passed to a Rec.2020 Sequence, an
encoded/display-domain frame, a premultiplied frame, or malformed storage fails
with `TimelineCompositeError`; it is never relabeled as the requested working
space.
Timeline blend modes, including seeded Dissolve, are implemented by
`mondrian-effects`' float pixel blend contract and must not round-trip through
RGBA8 scratch buffers. Extended working values therefore remain available to the
final output boundary. Every executable built-in unary render operation,
including Primary Color, blur, sharpen, vignette, chromatic aberration, grain,
and LUT, runs in this path through the same `mondrian-effects` float contract.
Primary Color carries the Sequence working-space identity and coefficients into
both CPU and GPU plans; contrast pivots at linear 0.18. LUT execution first
satisfies its explicitly authored processing-domain transition, then applies
the same domain-normalized tetrahedral cube on Preview and Export CPU paths.
GPU LUT execution remains a typed blocker until a backend implements that exact
contract; it may not substitute trilinear or guessed-domain output. Spatial
effects use premultiplied-alpha sampling internally while the
typed public frame remains straight-alpha. The Viewer spatial Module records its
horizontal RGBA32F intermediate as `PremultipliedCoverage`; shader uniforms are
derived from the input/output descriptors, and the vertical pass restores the
declared straight/opaque output contract. Premultiplied frames are rejected at
OCIO, effect, composite, display-calibration and output seams. Affine geometric transforms (scale, rotate,
translate) are implemented
in the float/linear path using inverse-affine mapping with bilinear sampling,
so media and solid layers with non-identity transforms no longer require legacy
RGBA8 fallback. Custom processors without a float ABI and non-unary effect graph
nodes are handled separately: custom processors still require the diagnosed
RGBA8 fallback, while built-in Blend, Mask, MaskSource, and ordered MultiInput
nodes execute in the CPU float DAG. Clip masks rasterize directly to float matte
coverage. Solid layers must materialize their float source when they carry an
effect graph or affine transform, execute that same compiled graph, and then use
the shared layer sampler; diagnostics must never claim a solid effect is float
while bypassing its pixel semantics.
Mask preparation and raster failures retain their typed `MaskRasterError` source
through renderer diagnostics. Preview classifies this deterministic contract
failure as blocked rather than a transient retryable execution failure.
`TimelineCompositeDiagnostics` makes that fallback explicit: preview/export
callers can see whether a composite stayed on the float/linear path or fell back
to legacy RGBA8 because of transform or effect support. Blend-mode counters stay
in the diagnostic contract for future unsupported blend contracts, but current
built-in media, solid, and float-capable adjustment blend modes are expected to
remain float/linear. Preview and export diagnostics must aggregate these
counters so performance smoke reports and job panels can identify which legacy
color path blocked a fully float/linear frame. Callers should use
`TimelineCompositeDiagnostics::color_path_summary()` as the renderer-owned
contract for high-level path state and structured legacy reason breakdowns
instead of re-inferring path safety from individual counters.

Compiled effects also carry a backend-neutral color-domain plan. A graph whose
nodes require log/perceptual, display-linear, or display-encoded RGB is not a
legacy effect. `TimelineEffectColorRuntime` maps the sequence working domain and
named effect domains to exact OCIO identities, and the CPU timeline compositor
executes each planned edge in-place around the relevant graph node. Preview and
export both supply a resolved Project-engine/working-domain context to this
renderer-owned boundary.
The float effect-output cache includes the engine/config/working-space identity.
`TimelineEffectColorRuntime` derives the typed
`EffectDomainProcessorCacheKey` as a SHA-256 semantic fingerprint over the
complete `ColorEngine` identity and Sequence working space; a truncated runtime
hash or reload generation cannot authorize pixel reuse.
If a processor cannot be resolved, `timeline_composite` returns a structured
`TimelineCompositeError` identifying execution or media/solid/adjustment domain
blockers and publishes no working frame. It must never execute those nodes in
scene-linear by accident or route them through the RGBA8 compositor. Data and alpha-domain graph errors
remain blockers rather than color-conversion requests. GPU graph lowering also
remains blocked until the GPU scheduler materializes the same OCIO edges.

Encoded decoded media enters the graph as a typed source/import RGBA8 boundary
(`CpuEncodedColorFrame::source_rgba8`). Scene-linear planar-f32 decoder output
enters as `LinearFloatSource`, so app preview, thumbnails, and export bypass
RGBA8 quantization while preserving the external source color identity.
Synthetic float data and effect graph intermediates use the same typed float
entry point.
Preview and export decode keys retain `DecodedVideoRangeContract`, not only an
already-flattened range value. Auto work can therefore consume a frame-level
range and fall back to the probe, while user Full/Limited overrides remain
distinct cache identities and authoritative sampling policy.
CPU preview/export fallbacks use `RenderInputTransform` plus
`execute_cpu_source_input_stage_with_session(...)` to dispatch the typed source
to the RGBA8 or float executor through the execution owner's processor Session.
GPU preview retains that same `CpuSourceColorFrame`
and executes the input transform without first materializing a CPU working
frame. Timeline media layers therefore carry typed working frames, not naked
RGBA slices. `CpuColorFrame` stores its linear
`WorkingRgbaF32Frame` payload in shared immutable storage so preview caches, lazy CPU
fallbacks, and export scheduling can clone the typed frame contract without
deep-copying a full 16-byte-per-pixel working frame. Copies that need owned
mutable float data must happen explicitly at execution boundaries.

The GPU compositor accepts renderer-owned working frames produced by native
decoder import and applies non-singular media affine transforms plus supported
fused point effects in one working-space render pass. These operations must not
materialize a CPU frame or schedule GPU readback; readback is reserved for an
explicit presentation, debug, or encoder boundary.

Rec.601 PAL/NTSC delivery keeps its distinct primaries, transfer, and matrix
tags through the export signal contract; swscale matrix selection and
post-encode validation are derived from that same contract.

The float transform path (`CpuColorTransformExecutor::input_to_working_float`
and `transform_float`) operates directly on f32 data without u8 quantization,
preserving HDR/log/10-bit precision. The `RenderColorTransformBackend::CpuOcioFloat`
backend indicates the float path was used, and `used_rgba8_boundary: false`
proves no intermediate quantization occurred. The existing RGBA8 path remains
available for decoded media, UI raster, debug/golden boundaries, and legacy
effects, with `used_rgba8_boundary: true` in diagnostics.
When a float output transform needs OCIO's contiguous f32 buffer, it must pack
the borrowed `CpuColorFrame` into exactly one owned typed
`Vec<[f32; 4]>`, expose that storage directly as a contiguous mutable view, and
avoid a second `Vec<f32>` flattening allocation or full-frame repack.
`RenderOutputColorBoundaryFloat::program_scopes(...)` is the renderer-owned CPU
reference seam for program video scopes. It measures the boundary's encoded
float pixels with their exact output color-space identity, preserving 10-bit/HDR
signal precision and keeping monitor adaptation outside the measurement. The
real-time GPU path must implement equivalent GPU reduction rather than reading
the full output texture back to the CPU.

`GpuProgramScopesRuntime` is that real-time path. A demand-driven request
records atomic-u32 histogram, waveform, vectorscope, and signal-excursion counts
directly against the retained display-encoded Program Output texture. A second
compute pass materializes three RGBA8 linear display textures for the UI. The
count buffer, pipelines, display textures, uniforms, and display bind group are
retained by request shape; normal playback performs no scope readback. The only
readback is a test-only 2x2 GPU/CPU reference comparison.

Color-transform executors emit `RenderColorTransformDiagnostics` for input and
output boundaries. Preview diagnostics aggregate transform calls, transformed
pixels, and temporary RGBA8 boundary crossings so performance smoke tests can
catch accidental CPU-bound color work as the GPU path comes online.

Production GPU compositing enters only through `GpuFrameCompositor` and
renderer-owned execution Sessions. Compiled Effect operations and concrete
Viewer/Export pass execution return
`GpuColorFrameHandle` values carrying the same descriptor contract as CPU
frames, rather than naked textures or byte vectors. The removed experimental
RGBA8 `RenderPipeline`/`FrameCompositor` family is not a compatibility path:
reintroducing a compositor requires the same ColorFrame identity, working-space
semantics, resource grant, and Preview/Export parity as the production path.

`RenderColorStagePlanner` sits between timeline evaluation/compositing and the
CPU/GPU color executors. It produces ordered stage plans for CPU transforms,
GPU OCIO transforms, upload, and readback. App and export crates should consume
renderer stage plans instead of deciding CPU/GPU/readback behavior locally.
`RenderOutputColorBoundaryPlanner` is the final-output boundary wrapper around
that planner. Its CPU-only mode is the current correctness execution path;
its PreferGpu mode must produce GPU/upload/readback stages plus explicit
blocker diagnostics instead of silently falling back to a CPU output stage.
`RenderOutputColorBoundaryExecutor` is the lower-level CPU final-output
execution boundary: callers choose an explicit strategy at construction time,
and the executor owns the final-output plan/execute sequence instead of
exposing low-level transform executors to app/export code. App/export CPU
reference callers use `execute_cpu_output_boundary_rgba8(...)`, which returns
encoded RGBA8 pixels plus transform/stage diagnostics as one boundary contract;
native GPU app/export callers must use
`RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend(...)`
with `RenderGpuOutputBoundaryRuntimeOwnedBackendContext`. GPU planning,
backend-object preparation, or recording failures are reported structurally and
must not silently run the CPU executor.
The Window Viewer holds `RenderGpuOutputBoundaryRuntime` for one live
device/surface execution Session. Export constructs a separate runtime inside
one `ExportVisualRenderSession`: it is cold at attempt start, may reuse backend
objects only across frames of that attempt, and is released at terminal
publication. Optional final-output recording/readback owns an independent frame
table even though it shares the device/pool with required heterogeneous
execution. A route-local output failure clears that table and may select the
declared CPU output fallback, but it does not poison the required
CPU-prefix/GPU-tail backend. Once that shared backend is `Ready`, the only
post-submit output-route failure that changes it to backoff is expiry of the
bounded total GPU-readback deadline, where completion is no longer known. No
runtime, frame table, or backoff state is process-global or shared between
Viewer and Export jobs.
The runtime owns the OCIO shader cache, pure backend prep cache, concrete
backend-object cache, GPU frame id allocator, and GPU frame resource table, and
exposes an executor-level record method that accepts only the per-submission
device/queue/encoder/load-op context. Lower-level code that already owns a
prepared pipeline, OCIO bind group, and pass node may still use
`RenderGpuOutputBoundaryBackendContext`.
Every `OcioGpuShaderRequest` carries the exact `ColorEngine`; this engine is part
of the shader-cache key and is used by core while selecting the config and
extracting the Processor shader. A Standard plan cannot be reused by ACES or
Custom OCIO solely because source, destination, display, and view strings match.
Normal project-engine switching does not flush unrelated warm shader plans.
Every OCIO reuse and concrete-resource admission decision uses a private,
domain-separated 32-byte canonical identity. Public `u64` shader, layout,
resource, and node keys are diagnostic projections only. The full identity is
carried from the shader/resource plan through packed LUT and uniform payloads,
uploaded resources, bind groups, pipeline layout, shader modules, render
pipeline, and pass node. Upload recomputes identity from the bytes actually
submitted to wgpu and validates row/length metadata; a caller-supplied or stale
compact hash can never stand in for payload identity. Bind and record seams
compare the full identities, so even an intentionally forced compact-key
collision cannot authorize cross-contract reuse.
For native GPU OCIO execution, `RenderGpuColorPassSchedule` is the bridge
between the stage plan and backend recorder: it requires GPU-resident source and
target frame handles, a blocker-free `RenderColorTransformGpuPlan`, and a
matching `OcioGpuWgpuRenderPassNodePlan`. The schedule also owns the renderer
entry points that bind a resolved input frame view into the wrapper bind group
and record the output target through `OcioGpuWgpuRenderPassRecorder`; preview
and export callers must not duplicate OCIO pass assembly. Resolved GPU frame
views must come from `GpuColorFrameResourceTable`, which validates the
`GpuColorFrameHandle` contract before exposing the backend texture/view/sampler
payload. Callers that already have concrete wgpu resources should use
`record_wgpu_from_resources` so resource resolution, wrapper bind-group
preparation, output target construction, and recorder invocation stay in one
renderer-owned path. CPU boundary frames and empty GPU targets must enter that
resource table through `GpuColorFrameUploadPlan` / `GpuColorFrameAllocationPlan`
and `GpuColorFrameUploader`, not ad hoc texture creation in preview or export.
Every table key comes from a renderer-owned `GpuColorFrameIdAllocator`. Frame
identity is the complete `(allocator authority, authority-local sequence)` pair:
the raw sequence alone is diagnostic and never authorizes equality, hashing, or
resource lookup. Construction claims a process-unique non-zero authority,
allocators are deliberately non-cloneable, and both construction and allocation
return typed failure. `u64::MAX` is reserved as the terminal sequence sentinel,
is never issued, and exhaustion remains stable rather than saturating, wrapping,
or reusing a live key.

Public GPU execution seams accept a typed
`GpuColorFrameResource<GpuColorFrameWgpuResource>` (or resolve one from the
runtime-owned resource table) whenever a concrete texture is consumed. Native
import, YUV conversion, display calibration, Viewer spatial execution, and
readback validate both the complete frame contract and the complete frame id;
another texture with an identical descriptor is not interchangeable. Raw
`wgpu::TextureView`, sampler, bind-group assembly, render target, and copy
recording seams remain renderer-internal. This prevents an Adapter from pairing
a valid logical handle with an unrelated physical resource while preserving
the high-level Preview, Export, Window, Headless, and native-import extension
boundaries.

The per-resource bind-group cache follows the same fail-closed rule. Its
process-wide key sequence uses checked allocation, never issues `u64::MAX`, and
propagates exhaustion through compositor/OCIO runtime construction. A cache-key
allocation failure may reject a new runtime; it may never alias an existing
bind-group entry.
Use `RenderOutputColorBoundaryStagePlan::gpu_resource_plan(...)` to bridge a
blocker-free final-output GPU stage plan into `RenderGpuOutputStageResourcePlan`
so transfer descriptors, frame ids, and the scheduled GPU transform stay
consistent. CPU-only plans and GPU plans with native blockers must fail
structurally at this bridge instead of falling back to CPU execution. Then call
the resource plan's materialization helper to fill the resource table. If the
same stage plan ends in `ReadbackToCpu`,
`RenderGpuOutputStageResourcePlan` carries the `GpuColorFrameReadbackPlan` and
owns resource-table lookup plus readback-copy recording. Preview/export callers
should use
`RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend(...)`;
the runtime owns planning, resource-plan derivation, backend-object
preparation, materialization, schedule validation, pass recording, and optional
readback in renderer-owned order. The returned `RenderGpuOutputStageRecord`
must carry `RenderColorStageDiagnostics` for the recorded upload, GPU color
pass, optional readback, blocker breakdown, and pixel budget so preview/export
telemetry does not infer GPU execution after the fact. The blocker breakdown
must distinguish missing shader modules, OCIO resource bind groups, fullscreen
wrappers, and render pipelines. Lower-level renderer code that already owns a validated
`RenderOutputColorBoundaryStagePlan` or `RenderGpuOutputStageResourcePlan` may
call the matching stage/resource recorder with renderer backend contexts.
GPU-to-CPU output for encode, thumbnails, tests, or debug captures must use one
of those renderer-owned paths. Export binds each readback wait to the exact
`wgpu::SubmissionIndex`, checks the attempt cancellation token between waits,
and uses 10 ms device-poll slices under one non-renewing 30-second monotonic total
deadline. A single poll-slice timeout is not device failure; cancellation
terminates the attempt, while only total-deadline expiry records
`ReadbackTimedOut` and moves an already-`Ready` attempt GPU backend into
backoff. Map, unpack, planning, admission, or recording failures remain
route-local and cannot poison required heterogeneous execution.

Export readback is serialized through the export frame contract selected from
delivery sample depth: 8-bit delivery writes RGBA8 raw-video bytes, while
10-bit and 12-bit delivery read the renderer GPU `Rgba16Float` boundary and
pack normalized channels into FFmpeg `rgba64le` pipe bytes. When GPU output is
unavailable, 10/12-bit delivery CPU fallback must use the renderer-owned
`execute_cpu_output_boundary_float(...)` helper, which applies the working ->
output OCIO float transform without u8 quantization. The caller flattens the
float result into `[f32]` and uses
`ExportFrameContract::pack_rgba_f32(...)` to produce `rgba64le` pipe bytes.
If both the GPU output path and renderer-owned CPU float helper fail, 10/12-bit
export fails closed. It must never manufacture an `rgba64le` payload from an
RGBA8 boundary. Export diagnostics record `FloatBoundaryUnavailable`, retain
the independent GPU fallback reason, and emit the renderer error before the job
can produce a misleading high-bit deliverable. Health report schema 3 names
this evidence as an output-precision failure, not a fallback.
Tone-mapped export delivery must also be explicit. If an export color context
requests tone mapping but the final export output boundary does not carry an
OCIO export view/display-view transform, the export health report must fail
with a structured output-transform issue instead of relying on the color-space
pipeline's `tone_map` flag. That flag is not a substitute for an OCIO view
transform.
After encoding, `mondrian-export` runs ffprobe through one typed
`ExportValidationExpectations` contract. Stream presence is a closed
`Required(exact constraints) / Forbidden` algebra rather than independent
booleans. Validation proves the mux family and MP4/QuickTime major brand,
exactly one required video/audio stream, codec/profile, encoded bit depth,
constant rational frame rate, dimensions, pixel format, range, primaries,
transfer, matrix, duration, audio codec/sample rate/channel count/layout, and
either exact authored static HDR metadata or its required absence. Bit depth is
derived from the already exact pixel format because `bits_per_raw_sample` is
not consistently populated across encoders. The returned `ExportOutputProbe`
also retains each stream's integer `start_pts`, `duration_ts`, and rational
`time_base`; acceptance compares A/V start and end boundaries in exact integer
arithmetic before converting the final error to milliseconds. Container
duration alone cannot prove aligned stream boundaries. This typed evidence is
consumed by Headless acceptance. Encoder success without it is an export
failure; the queue may publish `Completed` only after validation and atomic
final publication.

The encoder and validation CLI processes use
`mondrian-media::SupervisedChild` as one lifecycle boundary. Encoder stderr is
drained from spawn, not after raw-frame stdin has filled; frame allocations move
through a chunked stdin pump and return to the renderer for reuse. The worker
retains cancellation authority during rendering, video decode, audio source
reads, pipe writes, encoder wait, and FFprobe validation. A canceled pipe write
kills and reaps the encoder rather than waiting for FFmpeg to consume another
frame. FFprobe uses a 30-second monotonic per-invocation deadline, strict
bounded stdout (16 MiB for stream reports and 4 MiB for first-frame HDR
evidence), and a 64 KiB stderr tail. Cancellation, deadline, output-limit, and
I/O stages remain distinguishable in the resulting failure instead of being
collapsed into an exit-code string.

## Internal Float Precision

Working-space CPU frames and the GPU compositor use 32-bit float. This is a
fixed correctness contract, not a user preference. Native normalized encoded
sources and encoded output boundaries may use 16-bit float because their
values are bounded and they cross into a 10/12-bit delivery or a subsequent
32F working transform; OCIO LUT resources remain 32F.

Deterministic precision tests measure the current budget:

| Case | Measured worst error |
|---|---:|
| f16 round-trip over normalized `[0,1]` | `0.000244141` (`0.9998` 12-bit code) |
| f16 round-trip over scene-linear `[-16,16]` | `0.000488043` relative, `0.00390625` absolute |
| Hypothetical 256-layer HDR composite with f16 storage after every pass | `0.00274086` absolute |

The accumulated composite result is why working targets remain
`Rgba32Float`. A user-selectable 16F/32F working toggle would allow projects to
change numerical semantics and is not offered. At 3840x2160 one RGBA16F target
is 63.28 MiB and one RGBA32F target is 126.56 MiB; compositor ping-pong targets
are 126.56 MiB versus 253.13 MiB. The 2x memory/bandwidth cost is accepted for
working correctness, while bounded encoded boundaries retain 16F. A future
float deliverable or an unbounded intermediate must use a separately validated
32F boundary rather than relaxing this policy globally.

Current CPU preview/export execution uses `execute_cpu_input_stage(...)` for
encoded source boundaries and `execute_cpu_input_stage_float(...)` for
scene-linear decoder payloads. It uses
`execute_cpu_output_boundary_rgba8(...)` for final display or export output,
and `execute_cpu_output_boundary_float(...)` for
10/12-bit delivery CPU fallback. The float helper delegates to
`CpuRenderColorStageExecutor::output_transform_float(...)` and returns a
float `CpuColorFrame` with output transform applied; app/export code should
not instantiate the final-output executor directly or call
`execute_cpu_output_stage(...)` for final timeline output. Direct
`CpuColorTransformExecutor` usage is limited to renderer internals and its
focused unit tests.

GPU input transforms have their own renderer contract instead of piggybacking
on final-output plans. `RenderGpuInputStageResourcePlan` accepts a decoded
`CpuSourceColorFrame` in the `Source` domain. Encoded RGBA8 uploads as
`Rgba8Unorm`; scene-linear f32 uploads as `Rgba32Float` without quantization or
an intermediate CPU OCIO transform. It then records the OCIO GPU input
transform and produces a GPU-resident linear
working frame in a renderer-selected `Rgba32Float` texture. The working format
is not a caller option. It rejects CPU-only plans, plans with native GPU
blockers, non-source uploads, and non-working outputs. This is the guarded bridge for
preview playback to move input OCIO off the CPU. The runtime-owned entry point
is `RenderGpuOutputBoundaryRuntime::record_wgpu_input_stage_owned_backend(...)`,
which reuses the same renderer-owned shader cache, backend-prep cache,
backend-object cache, frame-id allocator, and frame table as output boundaries.
`GpuFrameCompositor` can consume the resulting `GpuColorFrameHandle` directly
as a media layer through `GpuCompositeLayerSource::GpuFrame`, so the planned
preview path can become GPU input transform -> GPU working composite -> GPU
output boundary without re-uploading that media layer. CPU sources still
require one host-to-device upload; native decoder surfaces use the separate
low-copy import path below.
Native hardware-decoded frames must enter through the separate renderer-owned
`GpuNativeDecodedFrameImportPlan` contract. That contract does not model the
decoder surface itself as a color-frame handle; it records the decoder handle
family, source texture format, and shader-visible video sampling contract
before validating renderer backend support. Physical source extent and
renderer output extent are separate mandatory fields. The native YUV pass
samples the complete visible source into the requested output extent with
explicit luma/chroma reconstruction, then OCIO produces the ordinary RGBA32F
working frame. This matches the CPU Preview ordering (scaled encoded source,
then input color transform), preserves full source/color metadata, and avoids
full-resolution working allocations when Viewer quality is lower than source
resolution. The sampling contract includes
range, YCbCr matrix, transfer characteristic, effective bit depth, and chroma
siting. Backend adapters must not bake in their own BT.709/BT.2020,
limited/full, PQ/HLG, or chroma-location guesses. A valid import produces only
the post-sampling/post-input-transform linear working `GpuColorFrameHandle`.
This keeps media residency reporting, OS texture import probing, and renderer
graph resource ownership decoupled.
The import plan carries the complete `RenderInputTransform`: OCIO engine
selection, tone-map policy, working space, and the required GPU backend. The
video sampling matrix and transfer must exactly match the resolved source color
space. A CPU OCIO backend or conflicting source/sampling contract fails before
GPU frame allocation. Backend adapters therefore cannot bake in their own
source-to-working transform or silently reinterpret PQ/HLG and
BT.709/BT.2020.
The renderer import helper must also validate the incoming native payload before
backend execution. `GpuNativeDecodedFrameImportSource` exposes a
`GpuNativeDecodedFrameSourceDescriptor` (extent, decoder handle family, and
source texture format), and `execute_native_decoded_frame_import(...)` rejects a
payload whose descriptor does not match the import contract.
The renderer implements this source contract for media-owned
`PreviewNativeDecodedFrame` payloads and owns the fallible
`DecodedVideoSurfaceFormat` -> `GpuNativeDecodedFrameTextureFormat` mapping.
Unsupported planar/unknown media formats return a structured source-format
error before backend planning; app/window code does not duplicate that format
table.
Backend support validation, video sampling validation, and output working-resource validation
are all required: a renderer backend must never be asked to import a
D3D11/NV12 contract while receiving a different native surface, and it must not
sample a YCbCr surface without explicit range/matrix/bit-depth/chroma metadata.
The app viewer path now carries each decoded media layer's
`CpuSourceColorFrame` plus `RenderInputTransform` alongside its lazy CPU
working-frame fallback. During
window recording it first tries `record_wgpu_input_stage_owned_backend(...)`
for each eligible media layer, feeds successful outputs to
`GpuFrameCompositor` as GPU-resident working frames, and records a structured
CPU-working-upload fallback only for layers whose GPU input stage fails.
`PreviewDecodeOutcome::Frame(RgbaFrame)` becomes the encoded variant;
`PreviewDecodeOutcome::FloatFrame` becomes the shared scene-linear float
variant and uses the same GPU input stage.
`PreviewDecodeOutcome::NativeGpuFrame` must flow through the renderer native
decoded-frame import contract instead of being wrapped in `CpuEncodedColorFrame`
or silently transferred to CPU.
On Windows, native import copies the FFmpeg-owned D3D12VA surface into a
renderer-created shared NV12/P010 texture before YUV conversion. The bridge
retains the source `ID3D12Resource` and decode fence only until its copy-ready
fence completes; it must not defer that release until the next frame is
imported. A bounded decoder may need that same surface in order to produce the
next frame, so next-import retirement creates a circular wait even though the
Viewer output is already independent. `ViewerGpuExecutionRuntime` exposes a
non-blocking completed-source retirement operation. The Headless Adapter calls
it after observing the exact asynchronous GPU completion, while the Window
Adapter advances it
at the beginning of every Preview prepare tick, including ticks that currently
have only a Loading candidate. Completion-query failure remains typed and
fail-closed. Renderer-owned bridge textures, pipeline state, and completed
Viewer outputs are unaffected; only the decoder-source command-lifetime lease
is retired.
Both `CpuEncodedColorFrame` and `LinearFloatSource` store decoded payloads in
shared immutable storage so preview media-cache hits, GPU source contracts, and
queued preview frame clones do not deep-copy a full source frame. GPU upload
plans preserve that shared u8/f32 payload and expose packed bytes only at
`Queue::write_texture`; float source upload therefore does not allocate a
second full-frame byte buffer. CPU-transform boundaries may
still request owned mutable bytes, but that copy must be visible at the boundary
rather than hidden in ordinary frame cloning. Media decode callers should use
the shared `CpuEncodedColorFrame` constructors when handing a decoded
`RgbaFrame` to the renderer.
Telemetry must report the actual path as `GpuOcio`, `CpuOcio`, or a mixed
variant; it must not infer GPU residency from preview-plan eligibility alone.
When a preview frame contains media layers, the viewer frame-residency payload
also carries an app-layer native video import readiness report. That report is
computed from media decoder residency, platform import probing, and renderer
native import support. Today it reports `CpuDecodedMedia` for CPU RGBA8 and
RGBA-f32 preview media; zero-copy and low-copy readiness
must only appear after a real decoder GPU handle and renderer import support are
both present.

Stage helpers return `RenderColorStageExecution<T>`, not the raw transform
result. App, export, tests, and benches must read frames from `.result` and
aggregate `.stage_diagnostics` where they expose observability. Preview
diagnostics, export job diagnostics, and export performance smoke reports record
stage-plan counts, CPU/GPU stage mix, transfer stages, GPU blockers, and touched
pixels from actual renderer stage executions so later GPU execution work can
prove it removed CPU bottlenecks instead of merely moving code around.
`RenderColorTransformError::ExecutionFailed` must keep the
transform direction plus typed input/output descriptors with the backend reason;
renderer GPU output smoke additionally records a real wgpu upload + GPU color
pass + readback boundary and emits `MONDRIAN_RENDERER_GPU_OUTPUT_JSON` (or JSONL
via `MONDRIAN_RENDERER_GPU_OUTPUT_SMOKE_OUTPUT`) so dashboards can verify the
native final-output path without launching the app window. The smoke report
contains both raw stage/runtime counters and a versioned `health_report` as the
sole high-level contract. That report embeds the derived health summary, fixed
checks, root causes, actions, and evidence so dashboards can distinguish
skipped adapters, incomplete native GPU sequencing, backend-cache/object
preparation failures, readback-size mismatches, GPU blockers, and CPU/GPU
parity failures without reverse-engineering the counters. The renderer owns the
serializable contract types for this layer directly in production code:
`RenderGpuOutputFrameReport`, `RenderGpuOutputStageDiagnosticsReport`,
`RenderGpuOutputRuntimeDiagnosticsReport`, and `RenderGpuOutputHealthReport`.
Smoke tests, perf tooling, and future app/export integrations must build on
those types instead of carrying a test-private schema copy.

## Required Semantics

- Disabled clips and zero-opacity clips do not enter the render plan.
- Clip blend mode overrides track blend mode; otherwise track blend mode applies.
- Adjustment layers operate on lower accumulated pixels, not as standalone media.
- Solid colors and Basic Titles are generated sources, not file-backed frames.
- Basic Titles enter the ordinary source/effect/transform/compositor path only
  after exact font/layout generation; Preview and Export share that generation
  contract and fail closed on dependency drift.
- Nested sequences must preserve the configured nested color-processing mode.
- Effect graphs are compiled from clip effects plus masks at the evaluated time.

## Preview vs Export

Preview and export may use different scheduling, cache lifetime, and readback strategy. They must share timeline interpretation, clip ordering, effect evaluation, blend semantics, and color-management decisions.

### Export artifact publication

The offline renderer freezes the normalized absolute output route and one
explicit `ExportOutputPolicy` at queue admission. Admission requires the
route's parent to already exist as a directory and canonicalizes that parent;
the worker never uses recursive directory creation as a substitute for durable
directory publication. A product flow that needs to create a directory must do
so before admission through the shared Storage directory seam.

`CreateNew` is the default and rejects an occupied route at admission. It is
checked again atomically at publication, so a file created by another actor
during a long export wins the route and the validated sibling is retained.
`OverwriteExisting` is explicit last-writer-wins authority over the direct file
present at final publication time (or creates the route if it is absent). It is
not compare-and-swap against an identity observed at admission; product UI must
not describe it as conflict detection or revision-safe replacement.

Export never lets FFmpeg open the final route. Before encoding,
`mondrian-storage` allocates one unique direct sibling object and returns an
identity-bound external-writer reservation. FFmpeg receives only the
reservation path. After encoder exit, Export reclaims the same kernel object;
identity mismatch fails before validation, so a path substitution cannot turn
different bytes into the admitted deliverable.

The reclaimed guard remains the authority during output validation and then
crosses the queue's Publishing Gate. File flush, atomic `CreateNew` or
`ReplaceExisting`, namespace postcondition classification, and parent-directory
durability all belong to the shared Storage primitive. Export owns no platform
`ReplaceFileW`, rename, or directory-sync implementation.

Only `FilePublicationEvidence` becomes a durable Export publication result and
authorizes `Completed`/`Published`. `BeforeNamespace` retains the exact
validated sibling for explicit recovery, `DurabilityUnconfirmed` proves the
final namespace changed but not crash durability, and
`NamespaceIndeterminate` preserves every observed possible new-object route.
Those latter outcomes fail the job with typed artifact evidence; none is
collapsed into an ordinary encode failure or inferred by checking whether a
path exists.

The optional `validation` build feature exposes a read-only semantic trace over
the exact `PreparedVisualFrameClosure` already produced by each consumer. The
trace normalizes scheduler generation while retaining the immutable Program
author fingerprint, compiled Effect graph fingerprint, full temporal request
and ROI set, exact nested source-time projection, and placement-instance path.
It performs no evaluation or materialization. The Export validation Adapter
then invokes the same private closure preparation and prepared-node working
compositor as a real job. Cross-Module gates compare that trace and the final
working-linear frame directly with Preview; separately asserting that each side
succeeded is not parity evidence.

For audio, `TimelineExportSnapshot.media[AssetId]` freezes only stable Component
bindings selected by the encoded public Audio Program Output over the exact
half-open export interval. `compile_audio_dependency_closure` performs that
selection after routing compilation and before physical media binding. It also
retains every exact selected root/nested `CompiledAudioProgram` occurrence,
addressed by Sequence, public Output, and projected dependency window rather
than a coarse Sequence-ID cache. Disabled, post-mute-gated, and off-range
contributions therefore cannot become dependencies merely because their author
Clips remain in the Project. Pre-fader and post-fader/pre-mute Routes remain
executable when a Track is muted; Track visibility is UI state and never gates
audio. Each binding carries an
absolute physical stream index, native layout, and the source fingerprint that
authorized it. Export resolution cannot consult the live Asset Library, fall
back to `0:a:0`, or reinterpret a missing Component; it uses the same standard
channel-matrix lowering as realtime Playback and fails the job when the frozen
binding or file revision no longer matches. Media decode returns native-layout
PCM; the shared prepared Contribution applies the same canonical Component
matrix used by Playback before any Sequence processing.
The frozen Sequence `AudioChannelLayout` is authoritative for Program
execution. Export's requested packaging layout is a distinct delivery contract
applied after the selected Program Output through the same
`AudioProgramDeliveryRuntime` used by Playback. Export admission owns the exact
FFmpeg layout-name capability table shared by command construction and output
validation: AAC/PCM admit the explicit Mono, Stereo, 5.1(side), 5.1(back), and
7.1 lowerings; MP3 admits only Mono/Stereo. Unsupported named/custom or
Discrete pairs fail during preset resolution with codec plus Program/target
layout evidence instead of being reduced to a matching `-ac` count.

Preview media decoding must convert source media into the sequence working
color space before compositing. The source color space resolves from clip
override first, then validated executable media metadata, then the configured
missing-metadata policy. Resolved or assumed probe values are not media metadata;
callers must use `VideoStreamInfo::executable_color_space()` and
`MissingColorMetadataPolicy::resolve_input_decision(...)` so preview and export
share the same `InputColorResolution` branch diagnostics. When preview/export
logs or UI need to explain why metadata was rejected or unsupported, they should
attach `VideoStreamInfo.color_interpretation` and `VideoStreamInfo.color_metadata`
raw CICP tags rather than rebuilding diagnostics from path or decoder text.
Export jobs carry the evidence inside each
`TimelineExportSnapshot.media[AssetId]` dependency, and
preview emits the same diagnostic summary when missing-metadata policy rejects a
media asset. Preview also stores the latest viewer-request rejection as
`PreviewColorRejection` so panels, diagnostics, and automated smoke tests
can inspect the rejected asset id, path, missing-metadata policy,
`InputColorResolution` branch, working color space, and media diagnostic summary
without scraping logs. Playback prefetch must not overwrite this viewer-facing
snapshot.
Filename/free-form candidates, partial CICP, and profile-name ICC mappings must
produce the same plan as missing metadata. Only a typed source declaration,
complete validated CICP, or explicit Override may change source color. YUV
materialization also requires the decoder's explicit supported matrix; the
renderer source color identity is never a matrix fallback.
Preview perf smoke reports and export frame diagnostics expose the same
per-branch input color-resolution counters, including override, detected
metadata, missing-policy assumptions, and missing-policy rejects. Data/non-color
texture resolution is reported as its own branch instead of being merged into
overrides, and it must come from asset payload classification rather than the
Interpret Footage color-space picker. These counters are part of the render-path
health contract: preview can remain real-time while still reporting whether it
was driven by authoritative media interpretation, non-color asset
classification, or by project policy. Aggregated report fields such as
`policy_assumptions` and `explicit_metadata_or_override` must be derived from
`InputColorResolutionSourceCounts` in `mondrian-timeline`, not hand-maintained
in preview/export reporting code.
Media metadata quality must travel through the same reporting path: export job
diagnostics carry `asset_issue_summary`, and preview media smokes serialize
`media_color_issues`, both derived from `VideoColorDiagnosticIssueAggregate`
rather than from parsed warning strings. Export freezes selected visual Asset
identities beside the exact Program set before queue admission. Delivery
validation and frame/job reporting deterministically derive
`ExportMediaDiagnosticSet` from those identities plus the frozen media
records; they do not repeat nested reachability or inspect the live Effect
registry. Those Asset identities originally come from
`PreparedVisualProgram::preflight_range_dependencies`, which performs
interval-index queries rather than a frame-by-frame long-program scan. The
prepared schedule is therefore the sole authority for hidden/muted Tracks,
disabled Clips, Transition endpoint handles, placement/retime projection, and
nested reachability. Finite temporal Effect extent conservatively expands a
nested child window; unbounded extent admits the whole child Sequence. Missing
children, cycles, or depth overflow fail closed. No exporter-owned recursive
Track/Clip walker may be reintroduced. Assets outside the selected range and
assets on hidden or muted Tracks do not poison static-HDR delivery validation,
while conservatively reachable Transition, nested, and temporal dependencies
do. Preview/export parity cannot be skewed by unrelated assets loaded in the
project.

`prepare_timeline_export_dependencies` is the immutable capture Interface that
joins those prepared visual demands with compiled Audio Program demands. It
also carries renderer-selected Transition identities to the App's physical
source-handle Adapter, so that Adapter validates external extents without
repeating range reachability. Its non-persistent
`PreparedTimelineExecutionSnapshot` retains the exact same-revision visual
Programs, exact selected audio Program occurrences, their selected
Sequence/Asset/Component/Transition evidence, and the queue-admitted frozen
Basic Title font closure. An empty selected font-query set is already a complete
dependency closure and capture seals it immediately as an exact empty set;
only a non-empty query set remains unresolved until the queue's font Adapter
freezes the selected face bytes. The resulting
Sequence ID set is the only nested author closure copied into
`TimelineExportSnapshot`; the App never clones the whole Project or interprets
Track/Clip membership a second time. Deserialization deliberately drops the
process-bound Program attachment, so queue admission must rebuild and validate
it before the job becomes visible. The exact
`ResolvedTimelineExportRange` provides an inclusive frame-start view for
prepared visual queries and one half-open Timeline Time view for audio. Capture
and offline execution must consume that same resolver rather than maintaining
independent in/out or Work Area rounding rules. Sequence duration is a
content-derived default end, not a canvas boundary: an explicit Work Area or
Out point may retain a blank/transparent tail beyond the last Clip. Worker
audio execution reads
`AudioProgramExecutionDemand` from this attachment: only `ProvenSilent` may
skip PCM preparation. `RequiresExecution` builds the Runtime even when there is
no media Component, because a selected Bus/output processor may generate
signal or retain a tail. Runtime preparation consumes the frozen Program
occurrences directly and never recompiles author routing.
Timeline export writes accumulated render-path color diagnostics into
`RenderJob.diagnostics.color`. The worker updates this snapshot while frames are
actually rendered. App panels, logs, and JSONL reports should consume
`ExportJobDiagnostics::color_report()` or
`ExportJobColorDiagnostics::health_report()` as the stable semantic contract
instead of re-evaluating timeline state or rebuilding derived counters in UI
code. The embedded summary carries the same health concepts used by preview
reports:
explicit metadata/override totals, policy assumptions/rejections, data-texture
bypasses, legacy RGBA8 reason totals, float/linear completeness, GPU blockers,
GPU blocker breakdowns, legacy RGBA8 reason breakdowns, and GPU path readiness.
Export simulation perf JSONL includes this evidence only through the versioned
`color_report`; `color_health*` fields are not a supported external report
surface. Export simulation `passed` must include that report verdict: default
runs require diagnosed frames, fully float/linear composites, GPU path
readiness, zero GPU blockers, zero upload/readback transfer stages, and zero
structured legacy RGBA8 reasons.
Preview perf JSONL follows the same pattern with `preview_color_report` and,
for app-scale playback probes, `preview_playback_color_report`. The app preview
service owns the derivation from raw counters to report summary fields so perf
tests do not hand-maintain color health semantics. Preview media decode/cache
and continuous-playback smokes fail closed on those reports: default runs
require a present health summary, fully float/linear composites, GPU path
readiness, zero GPU blockers, zero upload/readback transfer stages, zero
structured legacy RGBA8 reasons, and zero missing-metadata policy rejections.
Live app-window viewer output diagnostics can additionally be persisted with
`MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT`. That JSONL stream records the last viewer
GPU output attempt, including the derived health summary, display/presentation
readiness, native GPU stage sequence, blocker breakdown, output texture state,
external texture registration outcome, and frame context needed to correlate
failures with a concrete sequence id, timeline frame, preview size, external
texture key, output target/color space, tone-map flag, optional display/view,
and the latest structured viewer color rejection when preview metadata policy
rejects an asset. The stream now also carries renderer-owned structured stage
evidence through `RenderGpuOutputStageDiagnosticsReport`, so downstream budget
or triage tools can migrate away from app-local flat stage counters without
redefining the renderer schema. It also carries the latest renderer runtime
snapshot through `RenderGpuOutputRuntimeDiagnosticsReport`, so viewer triage can
see shader-cache and backend-object failures from the same JSONL record stream.
The same records include cumulative ready/degraded/blocked/failed/rejected/waiting
health counts so smoke tooling can enforce viewer GPU-output budgets directly
from the JSONL stream. `viewer_gpu_output_budget` consumes this stream, emits a
versioned health report, and fails closed when ready/failed/blocked/rejected/
degraded thresholds are not met or when the reported cumulative `health_counts`
disagree with the statuses replayed from the JSONL records. Empty streams fail
explicitly with a `records` budget failure instead of being reported only as
missing ready frames. The report's embedded summary replays
`display_issue_summary` records into reason counts and payload-blocker counts,
and it replays viewer color rejections into a
`VideoColorDiagnosticIssueAggregate`, allowing smoke runs to fail on HDR,
wide-gamut, unsupported presentation, UI payload blockers, or metadata-policy
rejections even when a temporary health budget permits degraded frames. It also
fails closed when renderer-owned evidence is missing by default: each record is
expected to include both `accumulated_stage_report` and `runtime_report`, and
budget thresholds `max_missing_stage_reports` / `max_missing_runtime_reports`
govern temporary exceptions.
Per-reason display thresholds are part of the contract, so CI can relax one
failure class for investigation without silently tolerating the rest, and
unknown future display reasons still fail closed instead of disappearing inside
an aggregate display-issue allowance. The same evaluator lives in
`app::viewer_gpu_output_health` so Rust smoke tests and the CLI share one
budget implementation. The Module also owns the canonical attempt-outcome,
health-status, cumulative-count schema and the pure readiness classifier used
by the Window producer. The Window may collect display and texture-registration
facts, but it must not redefine `Ready`, `Degraded`, or terminal failure
classification. The ignored `viewer_gpu_output_budget_smoke` test reads
`MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT`, writes the same health report into
`MONDRIAN_PERF_OUTPUT` when configured, and fails the test on budget violations.
`app::viewer_gpu_output_residency` separately owns the canonical lowering from
declared preview layers and completed renderer execution into residency
evidence. Pre-execution records are explicitly `GpuWorkingCompositePlanned`
with `execution_observed=false`; only a completed renderer record may publish
`GpuWorkingCompositeExecuted`, zero-copy success, or observed upload/readback
counts. Platform import capability is an explicit input, never a hidden test
dependency or a substitute for frame execution. The Window captures one native
video import probe per renderer/Window Session and supplies that same immutable
snapshot to hardware-decode admission plus declared and executed residency;
per-frame re-probing may not splice different capability generations into one
attempt.
Export diagnostics may expose additional preflight helpers, but final job-level
counters must be produced from the frame render path.
Preview exposes the same frame-level input color-resolution source counts from
its preview-intent evaluation path. Tests compare preview and export source
counts for the same timeline frame so display/output differences cannot hide a
divergence in media input interpretation.
Nested-sequence depth is a timeline-level contract exposed as
`MAX_NESTED_SEQUENCE_RENDER_DEPTH`. The visual
`PreparedVisualFrameClosure` is the sole visual enforcer; export audio and
diagnostic source-count paths must use the same limit instead of local
hard-coded values.
Preview final-frame cache keys must include the effective color context so a
monitor/output transform change cannot reuse stale pixels from a previous view.
For OCIO-backed preview this includes the resolved display and view names, not
only the output color-space enum, because two views on the same display can
produce different presentation pixels.

Preview callers must use `TimelineEvaluationRequest::preview(position, ...)`.
Export callers must use `TimelineEvaluationRequest::export(position)`. Each
adapter constructs the position explicitly from its selected frame and the
exact prepared Sequence grid at the evaluation boundary. Any future thumbnail,
analysis, AI, or cache-warm path should add an explicit intent instead of
reinterpreting sequence state directly.

Preview and export health reports share a common color report vocabulary
defined in `mondrian_renderer::color_report_vocab`. Shared check codes
(`fully_float_linear`, `gpu_path_ready`, `gpu_blockers`, `transfer_stages`,
`legacy_reason_total`, `policy_rejections`) and normalized root-cause/action
codes are the canonical contract for cross-report comparison. Preview and
export reports may emit additional context-specific codes (e.g.,
`preview_gpu_color_stage_blocked`), but normalization functions map them to
shared canonical forms for parity testing. App and export crates must not
recompute color health semantics locally; they consume renderer-owned summary
fields and shared vocabulary codes.

The renderer test suite contains an explicit preview/export semantic-signature
contract. It allows request settings such as intent, quality, color target, frame
drop policy, and resolution scale to differ, but requires media source timing,
nested-sequence source timing, element order, blend/opacity, transforms, clip
interpretation, and nested color-processing mode to remain identical for the
same sequence frame. Renderer and app tests also pin the final color boundary:
for the same working frame, output color space, tone-map flag, and engine,
preview display and export delivery targets must produce identical RGBA pixels
while preserving distinct output domains in diagnostics. App-level parity tests
also compare `TimelineCompositeColorPathSummary`, the shared fields of the
preview/export color-health summaries, and the normalized versioned report
verdict/check/root-cause/action signatures for the same timeline frame, so
preview and export cannot silently diverge in float/linear versus legacy RGBA8
composite routing, stage scheduling, GPU blocker accounting, health booleans, or
diagnostic interpretation. The renderer golden suite includes a stable RGBA hash for
this preview/export Rec.709 boundary parity contract; intentional color-pipeline
changes must update that hash with the same care as image golden references.
The app preview suite also pins a stable Rec.2020-working to sRGB-output
multilayer preview/export RGBA hash, so preview and export cannot drift together
without an explicit golden update. App golden constants identify the exact
Mondrian Standard package and SDR View generation whose pixels they protect.
The Generated Delivery Hero gate adds a product-level nonzero-Work-Area proof:
it resolves the authored Solid Color/Transform/Opacity through the canonical
Preview Timeline, composites in working linear, flattens at Program Output,
and samples an independent Rec.709 opacity/geometry oracle. Both finished
deliverables are decoded through the production Preview media Adapter and
compared at the same coordinates. This remains deterministic Mondrian
regression evidence; it does not replace the independent external color
references described below.
The Golden v12 Proxy/Relink gate adds fixed-corpus signed-retime evidence
without a second time interpreter. CFR and VFR Clips each execute exact `1/2`,
`-1/2`, and reverse hold maps. Preview and Export plans must preserve the same
complete `SourceSampleTarget`; reverse uses `StrictPredecessor`, never a
rounded time or nominal frame number. Each fixture renders the expected
Program plus adjacent-covering and wrong-direction counterfactuals, presents
reverse and hold through the proxy-backed production Headless Viewer, exports
the held frame from an immutable original-source snapshot, and ordinarily
reimports/decodes the published H.264/AAC file. The VFR proof additionally
binds physical requested/selected/duration PTS and crosses unequal 60/20 ms
presentation intervals, so `avg_frame_rate` cannot masquerade as cadence.
Bounded lossy-codec error must be both absolutely and proportionally closer to
the expected Program than either counterfactual. Source-target equality,
proxy/original resolution, decode provenance, GPU completions, export terminal
disposition, stream boundaries, Undo restoration, and pixel distances remain
one typed operation report. Metadata-only speed, parallel Export mapping,
covering-for-reverse, or proxy-backed delivery therefore fails closed.
The Color Media Hero gate adds a file-backed proof at frame 500. It imports HLG
Main10 and sRGB straight-Alpha through production media authoring, evaluates the
original rather than proxy source on two adjacent Hero Tracks, shares the
float-linear composite and Program Output path with Preview, and exports/reimports
frame `500..501`. Its exact Track/Clip/Asset placement anchor must survive the
stage reopen and the complete run's final reopen. The gate is valid coexistence
and roundtrip evidence, not an independent absolute HLG/PQ/Log reference.

Golden images generated by Mondrian are regression evidence, not an independent
quality reference. `color_reference` defines the strict boundary for external
reference frames: provenance class, exact producer/specification version,
stimulus SHA-256, payload SHA-256, payload format, pixel encoding, dimensions,
alpha semantics, reference white, and nominal peak are all explicit. Unknown or
placeholder identities, stale hashes, shape mismatches, NaN/Inf, out-of-domain
PQ/HLG signals, and contradictory opaque alpha fail closed before comparison.
PNG, OpenEXR, and numeric JSON decoding remains injected through
`ColorReferenceDecoder`, keeping file codecs out of the realtime renderer while
allowing validation tools to preserve float negative and extended-range samples.
Only public-specification and independent-application origins qualify as
independent quality evidence; a Mondrian regression golden can use the same
import path but cannot promote itself to an external reference.

Scene-linear CPU/GPU validation uses the renderer-owned
`LinearRgbaAccuracyBudget` and `LinearRgbaAccuracyReport` contract. RGB and
alpha are evaluated independently: RGB reports maximum absolute error, mean
absolute error, RMSE, nearest-rank P99 error, and non-finite sample count, while
alpha uses its own coverage budget. A single peak-delta assertion is not an
adequate color gate because it cannot detect broad low-amplitude drift or
distinguish color arithmetic from alpha corruption. Real-wgpu point-effect
tests include negative and above-one working values and fail closed on NaN or
infinity.

The current Mondrian Standard package additionally has a digest-pinned numeric quality
corpus. The same generated working-space samples execute through the production
CPU OCIO SDR and PQ output boundaries; a separate implementation of the View is
not used as the oracle. The corpus enforces objective invariants rather than
self-comparison: finite normalized outputs, exact alpha preservation, neutral
axis, monotonic tone response, dense non-negative hue-boundary continuity, a
locally dense negative-channel continuity path, 10-bit ramp cardinality, and
legal/full-range signal codes. Public ColorChecker 2005 D50 xyY coordinates are
converted through an explicit Bradford D50-to-D65 adaptation and XYZ-to-linear
Rec.2020 matrix before entering that same production boundary. They provide
externally sourced stimuli, while future independent application frames provide
external output evidence on top of the invariants.

Encoded SDR sRGB output validation uses a separate
`SrgbDisplayAccuracyBudget`/`SrgbDisplayAccuracyReport` contract. It converts
RGB code values through the explicit sRGB -> XYZ D65 -> Bradford-adapted XYZ
D50 -> CIELAB chain, then reports CIEDE2000 maximum, mean, and nearest-rank P99
error. Alpha remains coverage and has an independent code-value limit. The
real-wgpu plain output and OCIO display/view tests both use this contract, so a
small number of large errors and broad low-level drift are independently
bounded. The exact P99 implementation uses linear-time selection rather than a
full sort; this remains validation work and is not executed in the playback
frame loop.

This perceptual contract is deliberately named sRGB and rejects malformed
RGBA8 buffers. It must not be applied to Rec.709, Display P3, PQ, HLG, or
scene-linear data. HDR validation requires an absolute-luminance-aware model
and target display contract rather than relabeling CIELAB thresholds.
The production PQ GPU conformance gate applies that model to both the versioned
Mondrian Standard 1000-nit View and the independent ACES 2 reference View,
using the matching CPU OCIO processor as the semantic oracle.

## Engine-Owned Output View Transform

Export tone mapping is delivered through the selected color engine's typed OCIO
output View, not through the color-space pipeline. The three transform
contracts are:

- **Input color-space transform**: encoded `source → working` conversion via
  `RenderInputTransform`. Does not carry tone mapping.
- **Preview display/view transform**: viewer presentation resolved by
  `RenderOutputColorBoundary::from_intent(...)` with `target: Display`.
  受 monitor/surface/display policy 影响。
- **Export delivery view transform**: encoded delivery output resolved by the
  same constructor with `target: Export`. A named intent becomes an internal
  `RenderColorTransform::delivery_view(...)`, which dispatches through the
  explicit working-identity OCIO display processor.

Before that output boundary executes, the Export Module resolves preset-owned
codec profile/chroma/Alpha and explicit-or-Sequence-default bit depth/range into
one `ResolvedExportDeliveryContract`. The root contract selects internal
RGBA8/16F transport, final pixel format, FFmpeg lowering, and post-encode
expectations. Nested Sequences remain working-domain render inputs and cannot
select a different deliverable precision. Invalid 4:2:0/4:2:2 dimensions or
profile/signal combinations fail at admission; the renderer must not crop,
round, or ask FFmpeg to choose a substitute profile.
`expected_export_video_signal` is the single shared projection from that
admitted delivery plus Sequence output color intent to encoded pixel format,
range, CICP primaries/transfer/matrix, and static-HDR metadata expectation.
Queue execution, post-encode validation, and Golden acceptance consume this
projection; no acceptance Adapter may reconstruct FFmpeg color-tag policy.

### Single Output-Intent Authority

`SequenceSettings::root_program_color_context(project_cm)` resolves exactly one
`OutputTransformIntent` from the effective color engine and the sequence output
target. Mondrian Standard carries its immutable package identity, ACES carries a
target-aware preset, Custom OCIO carries the requested output target and resolves
its display/view/output-endpoint tuple from the pinned config identity, and an
explicitly display-referred workflow carries `Colorimetric`.

`DisplayManagementPolicy` is limited to monitor/profile identity, Viewer mode,
and tone-map policy. It cannot replace the engine-owned output View. The sequence
settings UI therefore exposes the engine and output target, but no independent
export display/view selector. This prevents preview pixels, encoded pixels, and
container color metadata from describing different output transforms.

`export_output_boundary_from_context(...)` in the export crate resolves
the boundary exclusively through `RenderOutputColorBoundary::from_intent(...)`.
Mondrian Standard resolves its output-target view from the pinned package;
Custom OCIO resolves only a binding matching the encoded output target;
colorimetric intent carries no view. If tone mapping is requested but the
resolved boundary has no view, export records
`ToneMapRequestedWithoutExportViewTransform`.

HDR metadata validation occurs before encoder launch, again before libx265
parameter construction, and after encoding against the finished bitstream. The
post-encode contract probes only the first decoded video frame when
`StaticHdrMetadataPolicy::WriteAuthored` was selected, because FFmpeg exposes libx265 ST 2086 and
MaxCLL/MaxFALL SEI as frame side data rather than stream fields. It compares the
encoded values at the x265 chromaticity/luminance quantization scales and fails
closed on missing, malformed, or changed metadata. Exports that do not request
`Omit` deliveries incur no frame-side-data probe. Source HDR10+ and Dolby
Vision metadata is never claimed as passthrough across rendered pixels: health
reports warn on referenced dynamic-HDR sources, and enabling the current
`WriteAuthored` request fails before encoding until a validated dynamic
metadata authoring backend exists. The exact Project engine determines whether
a Standard output-target contract applies. For Standard HLG/PQ, MaxCLL cannot exceed the View's fixed
1000-nit content peak. ST 2086 mastering-display peak remains independent
because it describes the authoring monitor, not the brightest content pixel; a
valid 4000-nit mastering display can therefore describe content formed by the
1000-nit Standard View.

Health reports distinguish export output-View availability from preview
display/view: `output_transform_issues` records when tone mapping was requested
but the engine-owned output intent could not provide a View. This is a Fail
condition in the health report. The action code is
`inspect_export_output_intent`.

## Preview/Viewer GPU Output Boundary

The preview/viewer path connects to the GPU color output boundary through
the app-window `prepare_viewer_gpu_preview()` function. The call chain is:

```
User scrub/play
  → RedrawRequested
  → prepare_viewer_gpu_preview(device, queue, session, host)
    → apply the current Preview ViewerGpuExecutionResourceGrant
    → estimate_viewer_gpu_active_working_set(complete request)
    → admit pressure-stable active texture bytes/count
      → fail before texture creation when the hard grant is insufficient
    → resolve the currently laid-out ViewerExternalTexturePresentation
    → host.gpu_preview_frame_for_current_state()
      → AppState::preview_frame_execution_request()
      → WindowPreviewAdapter::gpu_preview_frame()
        → resolve_preview_timeline_with_programs_and_observer()
          → prepare_visual_frame_closure()
            [renderer-owned recursive semantic authority]
          → materialize typed closure nodes/bindings
            [Preview media outcomes; no private Timeline walker]
        → returns PreviewGpuFrame {
            working_input,
            program_output_boundary,
            monitor_adaptation,
          }
          where working_input is GpuComposite { layers }
    → reserve move-only Viewer progress permit and lifecycle identity
      [recording does not begin without bounded cleanup/progress capacity]
    → RenderGpuOutputBoundaryRuntime::record_wgpu_input_stage_owned_backend()
        for media layers with source/input contracts
    → RenderGpuOutputBoundaryRuntime::record_wgpu_working_composite()
    → GpuViewerSpatialRuntime::record()
        → working-linear prefilter/crop/resize into visible Viewer pixels
        → transfer typed output into the shared OCIO frame table
    → RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_gpu_frame_owned_backend()
        → Program Output GPU transform (the same OCIO display/view intent as delivery)
        → retain the typed Program Output texture for scopes/cache diagnostics
    → optional GpuProgramScopesRuntime::record()
        → exact atomic aggregation from Program Output
        → GPU-only histogram/waveform/vectorscope display textures
    → optional RenderGpuOutputBoundaryRuntime::record_wgpu_intermediate_color_transform_owned_backend()
        → stock-OCIO colorimetric Program Output → monitor adaptation
        → no pass when both display identities match
    → optional GPU ICC monitor calibration
    → queue.submit()
      → transfer the move-only presentation output lease
      → install exact completion callback and retained capacity-one owner
      → infallibly commit the exact wgpu SubmissionIndex through the permit
          → dedicated non-UI bounded PollType::Wait(exact index, 8 ms)
          → high-resolution remainder pacing on Windows; recheck exact callback
      → frame_renderer.register_external_texture_view()
      → ordinary Viewer candidate: ordered submission may publish
      → heterogeneous candidate callback:
          → validate completed continuation batch
          → finalize the exact visual FrameWorkBroker lease
          → publish only when current and on time
  → refresh newly published external Viewer frame and paint
```

The exact scopes aggregation groups four horizontally adjacent samples per
shader invocation and coalesces equal destination counters before the global
atomic write. Every pixel, tail pixel, excursion, histogram bin, waveform
sample, and vectorscope sample remains counted; flat or locally coherent image
regions avoid the worst global-atomic contention. The ignored
`program_scopes_gpu_perf` gate records two warmups and eight 4K samples with
hardware timestamps. It requires pooled pipelines/count storage/display
textures, GPU p95 <= 5 ms, and CPU command-recording p95 <= 0.5 ms by default.
Timestamp mapping and its CPU completion wait exist only in the test harness,
outside the production recording path.

The native `working_input` is a GPU-composited working texture. CPU fallback is
the separately diagnosed raster preview path; it is not a second interpretation
of this native stage graph.
Each source layer retains its own exact Effect extent. It may differ from the
Viewer canvas extent; the layer transform projects that source into the output
canvas. Heterogeneous upload and completion evidence are therefore validated
against the source extent, never coerced to the presentation extent.
The `program_output_boundary` is a `RenderOutputColorBoundary` carrying the
sequence Program Output intent. Monitor selection cannot replace that View.
`RenderMonitorAdaptation` separately carries the preview-only monitor identity;
it permits same-class SDR-to-SDR or HDR-to-HDR colorimetric conversion and
fails closed on SDR/HDR class changes because those require an explicit
rendering/tone-mapping policy. The Viewer record retains both Program Output
and final monitor handles in one pooled GPU resource table. Scopes consume the
former; presentation and optional ICC calibration consume the latter. Neither
route performs upload/readback between these stages.

Display invalidation likewise uses the full `DisplayOutputIdentity`, covering
all pixel- and admission-affecting snapshot fields rather than a truncated
generation hash. ICC profile identity is a domain-separated SHA-256 value, and
the sampled calibration LUT has a second full identity binding source color
space, profile identity, cube edge, and every float sample. GPU calibration
caches and prepared passes compare that complete LUT identity. Compact
diagnostic projections may be logged, but never authorize frame, LUT, or
display-dependent resource reuse.

Viewer GPU hardware timestamps and CPU command-recording attribution expose
Program Output and Monitor Adaptation as separate stages. Identical identities
still emit the ordered zero-duration monitor marker so profiling remains
structurally comparable without adding a render pass.

Active-texture admission is not a scheduling-quality decision inside the
renderer. Preview may respond to a typed rejection by issuing a later,
explicitly coarser request through its normal quality policy. Export must
surface capability failure for the immutable requested extent. Neither caller
may retry the already-admitted request at lower precision or route an accepted
GPU Effect suffix back through whole-graph CPU execution.

### CPU Fallback Path

When the window GPU output path cannot execute, the viewer falls back to the
raster preview path (`composite_resolved_preview`). It resolves the same root
Program Output context as the GPU Viewer, executes
`execute_cpu_program_monitor_presentation_rgba8_with_session()`, retains the
Program Output descriptor/diagnostics, consumes its uniquely owned float pixels
through a second stock-OCIO encoded-float adaptation to the sRGB UI atlas, and
quantizes only after both transforms. Callers that need Program Output pixels
for scopes use the retained-output API instead. The fallback therefore cannot
replace the program View with the atlas identity or insert an intermediate
RGBA8 round-trip.
The Program Output transform makes exactly one owned typed output from the
borrowed composite and exposes its contiguous `[f32; 4]` storage directly to
OCIO. The presentation-only Interface then consumes and reuses that uniquely
owned pixel buffer for monitor adaptation; it retains only the Program Output
descriptor and diagnostics. The retained-output Interface used by Export or
scopes instead keeps Program Output pixels immutable and creates a separate
monitor output. Neither Interface flattens into a second `Vec<f32>` and repacks
another full-frame allocation.
GPU-output failures are recorded at the window boundary:

- `cpu_output_fallback_frames` — Number of frames using CPU fallback.
- `cpu_output_fallback_pixels` — Total pixels through CPU fallback.
- `PreviewGpuOutputBlocker::CpuFallbackRequested` — Typed blocker with reason.

CPU fallback is never silently used. Health reports distinguish:
- `Pass` — Clean GPU color output.
- `Warn` — GPU blocked but CPU fallback succeeded.
- `Fail` — Fail-closed color rejection.

### GPU Preview Cache Key

The `ViewerPreviewCacheKey` includes:
- `sequence_id`, `width`, `height` — Frame geometry.
- `plan_identity` — Domain-separated SHA-256 identity of the working color
  space, output color space, tone-map policy, complete color engine and output
  transform, ordered elements, complete media-frame identities, transforms,
  each Effect graph's strong semantic fingerprint and output-cache policy. A
  frame-dependent graph also includes its explicit frame seed.

A decoded media-frame identity is derived from the complete `MediaPreviewKey`,
including exact source time, source revision, decode geometry, alpha/range,
input/working color semantics and engine, plus actual decode-output evidence:
selected stream PTS, payload kind, decoded extent, applied RGBA contract or
native surface/sampling contract, handle family, and execution path. Cache and
playback-ring hits preserve the selected-PTS evidence of the retained payload.
A decode path that cannot report selected PTS remains presentable but cannot
authorize semantic cross-call Viewer reuse; the root execution nonce keeps a
later materialization distinct without scanning the full pixel payload.

Generated titles derive their Preview source identity in O(1) from the
renderer-owned strong request-plus-font identity, typed frame descriptor, and
sampled-to-author mapping. Nested Sequences derive theirs from the complete
resolved child Viewer-plan identity, exact child frame/extent, parent working
space, and final typed descriptor. They do not re-scan 4K `RGBA32F` payloads
merely to name an output. If a reachable nested frame or Effect is stateful or
`Uncacheable`, non-reusability propagates through the nested
`MediaPreviewFrame`; the root execution nonce remains the sole final
presentation disambiguator.

The old `DefaultHasher<u64>` media, title, nested-frame and Viewer-plan
signatures are not equality authority. Compact hashes may appear in
diagnostics, but Frame Store lookup and CPU/GPU presentation registration retain
the complete 32-byte identity; external resource keys encode all 64 hex digits.

Changing the complete Color Engine/config identity, working/output space,
display/view transform, or display contract changes semantic identity and
forces re-rendering. A process-local OCIO reload generation is only
operational backend/residency evidence: it may trigger resource revalidation
or eviction, but it is never pixel-equivalence or cache-key authority.

This key is mandatory semantic output identity, not cache admission.
`viewer_preview_plan_allows_cross_call_reuse(...)` separately requires every
reachable Effect output to permit reuse. A plan containing an `Uncacheable`
graph retains identity for evidence and one execution/presentation attempt, but
the runtime binds it to a fresh execution nonce, never treats a matching
Current/Pending candidate as reusable, and skips final-raster and registered
external-GPU-output lookup/insertion. A stable semantic identity can therefore
never accidentally convert nondeterministic pixels into a cross-call cache hit.

Viewer presentation geometry is deliberately not part of the timeline/render
plan cache key. It scopes the external texture handoff instead: output extent
or normalized visible-region changes clear `Current`, allocate a new spatial
identity/key, and rerun only the spatial and display-boundary tail over the
current working candidate.

## GPU Working-Space Compositing

The preview/viewer path has a bounded native GPU working-space compositing path
through `gpu_compositor.rs`. For supported layer stacks, the app window records:

```
Resolved preview layers
  -> for each retained Windows D3D11 NV12/P010 media layer:
       ViewerNativeVideoImportRuntime
       -> bounded D3D11/DX12 shared-texture bridge entry
       -> native YUV shader into encoded-float Rgba16Float source texture
       -> OCIO GPU input transform into Rgba32Float working texture
       -> insert returned working resource into the composite frame table
  -> for each media layer with a source/input contract:
       RenderGpuOutputBoundaryRuntime::record_wgpu_input_stage_owned_backend()
       -> upload encoded RGBA8 once as Rgba8Unorm, or scene-linear f32 once as Rgba32Float
       -> run OCIO GPU input transform into Rgba32Float working texture
  -> GpuFrameCompositor::record()
     -> reuse one full-frame, opaque, identity GPU working layer directly
     -> sample GPU-resident media layers directly
     -> upload only media layers that already have a materialized CPU working fallback
     -> composite media/solid layers into an Rgba32Float working texture
     -> insert the working texture into RenderGpuOutputBoundaryRuntime frame table
  -> GpuViewerSpatialRuntime::record()
     -> prefilter strong downscales in working-linear light
     -> crop the visible source region and reconstruct it into Rgba32Float presentation pixels
  -> RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_gpu_frame_owned_backend()
     -> OCIO GPU display/output transform
  -> frame_renderer.register_external_texture_view()
```

Viewer publication is transactional. The window registers the newly completed
output and publishes its candidate identity before it unregisters the prior
texture key. Native-import backpressure or a failed candidate therefore leaves
the last completed frame registered instead of clearing the Viewer to white.
Intentional presentation invalidation (project/display/geometry changes) still
owns explicit clearing; an incomplete replacement does not.
The same execution evidence reports peak native-import contract pools and
bridge entries. Contract pools are globally bounded and completion-aware, while
each contract retains its own bounded in-flight bridge ring.

This keeps preview playback GPU-resident from input conversion through output
transform for the supported subset. Retained Windows D3D11 decoder surfaces use
a low-copy path, not zero-copy: the decoder array slice is copied once into a
shareable single-slice NV12/P010 texture, while YUV conversion, OCIO input,
working composite, and output remain GPU-resident. CPU-decoded media still uses
one typed source upload. The app media preview cache stores the decoded source
frame plus the `RenderInputTransform` contract without eagerly materializing a
CPU working frame. CPU working frames are generated lazily only when the CPU
reference compositor or a runtime fallback actually needs them. It also removes
the extra `UploadToGpu` stage between working composite and output transform;
the renderer contract is covered by `from_gpu_working_frame()`.

### Capability Classification

- **`GpuNative`** — All media sources are GPU-resident, every executed layer
  uses Normal blend mode and a supported working-linear effect plan, and the
  executed stack has ≤5 layers. Native D3D11 media enters through the bounded
  low-copy import backend; procedural and adjustment layers require no import.
- **`GpuWithUpload`** — Layer structure supports GPU compositing, but at least
  one layer enters from CPU memory. The preferred media path uploads decoded
  RGBA8 or scene-linear f32 once and runs GPU OCIO input before compositing. If that input
  stage is unavailable, the app records a structured fallback and uploads an
  already materialized CPU working frame when one exists. Source-only preview
  cache entries fail closed for that GPU attempt instead of silently performing
  an unplanned CPU input transform inside the app-window render pass.
- **`CpuFallback`** — GPU compositing not possible. Reason is classified as:
  - `EffectRequiresCpu` — Effect graph needs CPU execution
  - `UnsupportedBlendMode` — Only Normal is GPU-supported
  - `UnsupportedTransform` — Transform cannot be represented by the GPU compositor
  - `FrameNotGpuResident` — Frame must be uploaded
  - `TooManyLayers` — Exceeds the bounded 5-layer GPU composite stack
  - `GpuUnavailable` — No GPU device/queue

### GPU resource residency

Texture reuse is owned by concrete renderer execution Sessions and their typed
resource tables/pools. Viewer and Export apply explicit count/byte grants and
active-working-set admission; no standalone process-wide or unbudgeted
`TexturePool` may retain anonymous textures. Pool keys must preserve the
complete size, format, usage, and stage identity required by their owner, and a
raw `wgpu::Texture` is never sufficient cross-Module execution evidence.

### GPU execution timing

Renderer performance evidence separates four boundaries: CPU command recording
and queue submission, hardware GPU timestamp duration, CPU completion wait, and
end-to-end wall time. `GpuTimestampFrameTimer` owns the renderer-side timestamp
query contract and enables it only when both encoder timestamp features are
available. `GpuTimestampQueryRing` gives execution gates and continuous
telemetry a bounded asynchronous path: non-blocking polls recycle completed
slots, a full ring discards telemetry instead of back-pressuring presentation,
and offline gates perform one final wait only after the measured interval. The
production presentation loop must never introduce a per-frame query wait. A reported GPU duration therefore
means commands bracketed on the hardware timeline, not record/submit/wait wall
time. Gate reports also identify the adapter and retain compositor and spatial
pass diagnostics so regressions remain attributable to a concrete execution
path.
Each ring slot resolves five ordered hardware counters: frame begin, after
working compositing, after Viewer spatial processing, after the output color
boundary, and frame finish after optional display calibration. The four
adjacent deltas are reported alongside total GPU duration. Marker ordering is
validated before submission; a recording failure abandons and immediately
releases its slot without polling or waiting. Missing, duplicated, or
out-of-order markers fail the telemetry sample instead of producing misleading
attribution.
The native-decoder import prefix is a separate earlier queue submission and is
therefore forbidden from borrowing these Viewer stage meanings or query slots.
Its renderer-owned bounded ring records post-acquire/start, after native YUV
decode, and after source-to-working input color. Deferred samples retain exact
backend-runtime-local candidate/import tokens and report
`yuv_decode_marker_bracket_us` plus
`input_color_marker_bracket_us`. Cross-runtime reports add a separate execution
session identity. The first timestamp is inside the wgpu command buffer, so
neither delta covers decoder execution, the cross-device copy, queue waits, or
the preceding raw acquire transition. The brackets can include implicit
barriers, scheduler gaps, and backend command placement/reordering, so they are
stage attribution rather than pure shader timings. Timing is disabled by
default and only an explicit bounded `Enabled { capacity }` policy may allocate
the `1..=256` slot ring. The execution owner performs device polling;
native-import telemetry only collects callbacks after that poll. A full ring
records `dropped`, while disabled, unsupported, or failed timing remains
inactive with an exact reason. Neither condition back-pressures render
execution. The public diagnostics preserve the invariant
`submitted_imports = samples + pending + missing + dropped`, including when a
failure retires formerly pending slots. `submitted_imports` counts only native
imports that returned a valid working-frame output; a bridge error after
ambiguous queue acceptance formed no usable output and is deliberately outside
that coverage total.
When timing is active, every successful Viewer recording owns one move-only
candidate receipt. The Adapter removes it at most once through
`ViewerGpuExecutionRecord::take_native_video_import_timing_receipt`; it must not
infer expected coverage from layer count or from whichever asynchronous samples
have completed so far. The receipt is closed by exact per-import terminal
decisions in that Viewer candidate and enforces
`submitted_imports = scheduled_samples + missing_samples + dropped_samples`.
Here `scheduled_samples` proves only that asynchronous readback registration
succeeded. An active zero-import candidate has an explicit all-zero receipt;
disabled or unsupported timing has none. An `Err` Viewer recording has no
receipt, while any sample it scheduled before failing keeps that failed
candidate token and must remain unmatched rather than being reassigned to a
later successful frame.
The Headless Adapter keeps this evidence opt-in as well: its default
constructor passes `Disabled`, while performance gates supply both the
renderer-ring capacity and an independent workload-derived App observation
capacity. The move-only renderer receipt is consumed once after the Viewer
submission succeeds and projected into cloneable execution evidence qualified
by a process-local Headless session ID. Completed native samples receive the
same session ID and enter a separate bounded observation buffer; Adapter
overflow is counted independently from renderer-ring `dropped` samples. The
Adapter collects only immediately after device polls that already exist for
Viewer submission progress or suffix timestamp maintenance. Offline
finalization reuses the suffix ring's single successful `finish_all` wait, then
drains native evidence without a second hidden poll; if the suffix wait is
unavailable or failed, native final evidence fails closed.
Successful Viewer records also expose CPU preparation attribution for native
input/import, working composite, spatial, output-boundary, and optional display
calibration stages. These timings end at command preparation and never claim to
be GPU execution time; they identify CPU-side bridge waits or per-frame object
construction before adding lower-level GPU pass timestamps.
Native import attribution further separates source validation, non-blocking
bridge acquisition, cached pipeline/intermediate preparation, YUV recording,
source-to-working color-stage preparation, resource extraction, and internal
bridge submission. Multi-layer candidates accumulate those costs before the
performance gate calculates each field's percentile.

OCIO GPU preparation caches the complete immutable static-pipeline assembly by
one engine-qualified canonical shader-plan identity plus the output texture
format. A warm frame therefore does not rebuild resource
contracts, wrapper source, Naga artifacts, pipeline layouts, or render
pipelines. The concrete backend object also owns the wrapper bind-group layout;
per-frame input bind groups reuse that layout instead of creating another
layout object. LUT payload hashes are computed once when the immutable shader
plan is extracted, rather than walking a 57^3 payload during every frame.

Every cache-hit authorization in this preparation chain uses a domain-separated
32-byte canonical identity, including shader requests, extracted shader plans,
translation artifacts, resource layouts, static pipelines, wrapper artifacts,
concrete backend objects, shader modules, and render pipelines. The shader-plan
identity covers the exact engine/request, generated source, complete LUT
payloads, uniforms, and resource metadata; downstream identities retain the
exact subset that can affect their concrete object. Existing `u64` fields are
compact diagnostics and contract error evidence only, never cache equality
authority. A forced collision of those compact projections must therefore
produce two cache misses and two distinct prepared artifacts.

The retained ignored `color_view_gpu_perf` hardware gate uses a spatially
varying GPU-resident 3840x2160 Linear Rec.2020 input and rotates Mondrian
Standard SDR, PQ, HLG, and the official ACES 2 1000-nit PQ preset. Sixty warm
samples per View report GPU timestamp and CPU-record p50/p95/p99, cold preparation, shader
size, LUT shapes, pass count, uploads, and readbacks. The measured timestamp
contains exactly one complete OCIO output pass and excludes input generation,
initialization, timestamp mapping, and CPU completion wait. Standard SDR, PQ,
and HLG require p95 <= 5 ms; Standard PQ additionally requires p95 <= 80% of
the like-for-like ACES PQ reference on the measured adapter. Environment
variables may tighten, but not silently disable, either budget.
The schema-4 report snapshots cache counters around the measured interval and
fails when any warm sample extracts a shader, prepares a static pipeline or
backend object, creates an OCIO wrapper input binding, allocates an output
texture, or evicts a pooled texture. Wrapper-binding and exact-contract texture
pool hits must cover every rotated View sample, so the timestamp budget cannot
mask recurring per-frame GPU object churn.

The July 2026 RTX 3050 Laptop/DX12 production-path smoke run used 20 warm
hardware-timestamp samples per View. The current segmented SDR v2 graph measured
1.094/1.109/1.381 ms p50/p95/p99 at 4K; Standard PQ measured p95 1.395 ms and
HLG p95 0.984 ms. All measured samples reused shader, static pipeline, backend
objects, wrapper bind groups, and output textures without upload or readback.
An earlier direct per-pixel OCIO `GradingRGBCurve` graph measured about 175 ms
and was rejected; the shipping graph bakes that same curve to a 4096-entry OCIO
1D LUT and retains the separate 61-cube gamut surface.

The same ignored integration target retains a separate schema-1 input and
primitive-transform gate. It rotates OCIO identity, Linear Rec.2020 to encoded
Rec.709 (matrix + OETF class), Rec.709 to working, and Sony
S-Log3/S-Gamut3.Cine to working. Identity and matrix/OETF use the production
GPU intermediate-transform recorder; decoded-source cases use the production
GPU-resident input-stage recorder. Source allocation and initialization happen
before timestamps, and measured samples contain one OCIO pass with neither
upload nor readback. The report carries exact source/destination identities,
processor cache id, shader/LUT resource shape, cold and warm CPU record cost,
GPU p50/p95/p99, and runtime-cache deltas. Its 5 ms default p95 budget and
creation-free warm-path gate are independent of the View-versus-ACES gate so
input-stage regressions cannot be hidden by output-stage results.

Renderer-owned color stages share a device-scoped exact-contract texture pool
across native import and Viewer output runtimes. A candidate returns its typed
resources only after its prior commands were submitted to the same ordered GPU
queue, or after recording was abandoned before submit. The next frame can then
reuse matching extent/format/usage storage without a CPU completion wait. Idle
resources use global LRU eviction. The standalone renderer default is three
resources per contract and 384 MiB, but the production Viewer does not treat
that default as an entitlement: `ViewerGpuExecutionResourceGrant` applies the
App Preview projection online to the single device/runtime owner. Shrinking the
grant synchronously retires excess idle LRU entries; checked-out resources stay
valid and observe the new limits when released. `clear_idle_resources()` drops
only idle textures, while device/surface reset additionally clears
candidate-scoped runtime state. Window applies the typed grant after every App
resource tick and rechecks it immediately before bounded Viewer execution;
Headless applies the same projection from its candidate's resource snapshot.
Pool hits, misses, releases, evictions, retained resources, and retained bytes
remain observable in runtime diagnostics.
CPU upload plans use the same pool before `queue.write_texture`; synchronous
export readback returns its completed input/output resources before releasing
the runtime lock. Route-local failure clears only the output runtime's frame
table. Later frames can therefore reuse matching pool storage without retaining
per-frame table entries or invalidating the required heterogeneous runtime.
The working compositor acquires both ping-pong accumulation targets from this
same pool. Multi-layer/effect semantics and pass ordering remain unchanged; the
pool only replaces repeated exact-contract allocation after an ordered submit.

Presentation ownership is an explicit move, not a borrowed `TextureView` or an
id-only lookup. The output boundary resource table and display-calibration
runtime expose contract-validated `take` operations: the complete typed handle
must match, a mismatch leaves the producer's entry intact, and success removes
the actual `GpuColorFrameResource` from producer ownership. A move-only
`ViewerGpuPresentationOutputLease` then owns that resource for the complete
advertise/sample lifetime, so a visible frame cannot simultaneously re-enter
the reusable pool. The lease captures the pool generation at detachment and
returns the allocation exactly once on drop only while that generation remains
current. Device/runtime invalidation increments the generation, clears idle
storage, and causes late leases from the retired generation to drop their
backend resources instead of repopulating the reset pool. Diagnostics separate
normal releases, invalidations, and stale-generation drops. This ownership
contract is independent of native present-timing receipts: timing evidence may
refer to a presented frame, but it does not clone or replace the physical
resource lease.

Detached presentation ownership remains part of the same Viewer owner's active
working-set grant. The shared texture pool registers the exact texture count
and logical bytes when a `ViewerGpuPresentationOutputLease` is created and
retires that demand only when the lease drops. Window and Headless publication
are capacity-one: when command recording may begin, the pool may expose zero or
one detached current output. The next candidate's admission adds that physical
residency to its request-local source, effect, composite, spatial, Program
Output, scope, monitor, and calibration demand. Adapters neither estimate nor
report this physical residency.

Admission also prevents a first-frame-only success. The pure request estimate
projects the exact texture that `take_presentation_output()` will detach:
display calibration's RGBA16F output when calibration exists, otherwise the
RGBA16F monitor-adaptation output when that pass exists, otherwise the Program
Output texture at the requested Encoded8 or EncodedFloat16 precision. That
texture is already counted once in its producing stage. The runtime adds only
the component-wise count/byte shortfall between this prospective current output
and all live detached outputs as `presentation_continuity_reserve`. Total
admission is therefore the candidate working set plus the component-wise
maximum of the capacity-one current output and its prospective replacement,
never their blind sum. With no current output, the first candidate reserves
enough headroom for its own future replacement; with an exact current output,
live ownership consumes that reserve. Two identical consecutive candidates
therefore require the same grant. A request that cannot remain replaceable is
rejected before its first texture allocation instead of publishing one frame
and entering permanent active-working-set refusal.

More than one live detached output is bounded backpressure, not an input to
that component-wise formula. An unordered count/byte aggregate cannot identify
which lease publication will replace and therefore cannot prove the following
steady state. Window and Headless lifecycle occupancy prevents another record
while a candidate lease is awaiting publication; admission nevertheless
checks the capacity-one invariant and fails closed if an Adapter bug, reset
race, or future multi-slot owner exposes two leases. A future multi-slot design
must provide explicit replaceable lease identity and ownership transitions
instead of weakening this check or reviving aggregate approximation.

This ledger spans pool generations: reset invalidates return authority and idle
storage but an old-generation lease remains charged until its actual backend
resource drops. Current and high-water texture/byte ownership plus accounting
overflow are exposed in pool diagnostics. Internal accumulation uses a wider
checked representation; any demand that cannot be represented by the public
`u64` grant model fails admission conservatively instead of disappearing or
wrapping. Dropping a current-generation lease atomically moves it from detached
active demand to idle retention, while dropping an invalidated-generation lease
atomically removes active demand and destroys the backend resource.

`ViewerGpuExecutionRecord` keeps its concrete output owner private and carries
a single consumable ownership authority. `take_presentation_output(&mut
record)` consumes that authority before detaching either the color-output table
entry or the display-calibration output, then returns the common lease. A
second take is a typed error; a missing producer resource also fails closed and
cannot later be retried against a coincidentally reused identity. Ordinary
per-frame clear leaves detached leases valid. Viewer device/runtime `reset`
clears candidate owners and invalidates the pool generation, so an already
published lease remains the physical owner but cannot restore old storage to
the reset runtime when it is dropped.

### Integration Status

Renderer unit tests that construct independent `GpuContext` owners use a
process-local capacity-two admission permit attached to the context lifetime.
The default parallel Rust test harness therefore still exercises concurrency
without provisioning an unbounded number of DX12 devices and queues. Existing
device/queue adapters are not charged again, integration/performance binaries
retain their own explicit resource policy, and production builds contain no
test admission state.

The production preview path uses GPU compositing for supported media, solid,
and adjustment elements:

- a single full-frame GPU working layer with Normal blend, full opacity,
  identity transform, and no non-identity effect bypasses the composite pass;
  the typed working handle is reused without allocating two 4K RGBA32F
  accumulators, while any semantic difference returns to the normal compositor;

- media frames must either already be in the sequence working color space or
  carry a source/input contract whose target working color space matches the
  sequence; differing source/preview extents are supported through
  inverse-affine GPU sampling;
- media and solid sources may carry a lowered working-space-aware ColorAdjust,
  Vignette, or Grain chain; effects run before source-over composition;
- adjustment layers process the lower accumulated working pixels and blend the
  result back with their opacity, matching the CPU float reference semantics;
- media and scene-linear procedural-solid layers may use invertible affine
  transforms; external effect-domain solids materialize before their OCIO
  round trip so authored effect/transform order remains unchanged;
- all executed layers must use `BlendMode::Normal`;
- skipped leading/identity/zero-opacity adjustments do not consume capacity;
  the remaining executed layer count must be ≤5.

If any condition is not met, the GPU-preview candidate path records
`GpuCompositingDiagnostics { cpu_fallback_composites, first_blocker }` and
returns unavailable for that GPU candidate instead of running the CPU reference
compositor on the UI/event thread. Playback and buffering paths may show a stale
viewer frame or loading state, but they must not synchronously composite media
frames just to produce a fallback candidate. Paused still-frame preview may use
the raster CPU correctness path. This fail-closed gate is intentional:
unsupported effects, blend modes, transforms, or resampling must not silently
run through a visually different GPU approximation, and unsupported GPU
compositing must not make transport controls or window close unresponsive.
