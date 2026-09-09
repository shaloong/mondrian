# Execution Resource Coordination

Preview source reservations use Media's frozen sampling precision rather than
inferring storage precision from transfer function. Initial playback, seek,
cache recovery and predictive admission use that same key-derived footprint;
encoded ten-bit HEVC therefore reserves its actual float source plus working
float frame. No deadline or memory limit changes accompany this accounting.
The completion pump preserves the physical lease's admission class. A purely
speculative capacity rejection caused by later Current work retires without
remembering a source failure for the generation. Missing leases, Current grants
and completions adopted by the pending current demand retain strict failure
evidence, including the actual payload bytes and admission class in diagnostics.

`mondrian-app` owns one lightweight product policy Module that coordinates
resource demand without becoming an execution scheduler. It converts three
inputs into a versioned immutable decision:

- an explicit machine resource profile;
- current demand facts from Preview, Audio, Proxy, Thumbnail, Waveform,
  Media Import, existing-Asset mutation, and Export;
- a fixed-cadence product-process-tree memory observation plus independent
  whole-system pressure (`Nominal`, `Elevated`, or `Critical`).

The default profile Adapter reads physically installed memory through Windows
`GetPhysicallyInstalledSystemMemory`, Linux `/proc/meminfo`, or macOS
`hw.memsize`. A platform or failed probe without reliable evidence is classified
as `UnknownConservative`, not falsely reported as an 8 GiB machine. Measured machines below 8 GiB are explicitly
`BelowMinimum`; the half-open range [8 GiB, 16 GiB) is `MinimumSupported`,
[16 GiB, 32 GiB) is `Standard`, and 32 GiB or more is `Professional`.
Unknown capacity uses the same conservative budgets as the 8 GiB policy while
remaining distinguishable evidence; below-minimum capacity gets
smaller derived-media budgets and a coarser realtime Preview floor.

## Interface and ownership

The coordinator publishes a named typed projection for each execution domain;
there is no uniform background record padded with zero or irrelevant fields.
Proxy receives global/automatic dispatch plus requested parallelism. Thumbnail
and Waveform receive automatic admission, dispatch, and their own cache grant.
Media Import receives dispatch plus requested parallelism. Existing-Asset
mutation receives only dispatch. Export receives dispatch plus one complete
resource grant that its queue freezes for each dispatched attempt. Preview
receives its runtime scale, Frame Store limits, trim request, Basic Title cache
grant, and instance-owned Effect pixel-cache/GPU-plan-cache grants, plus an
independent Viewer GPU idle-retention/active-working-set grant and one atomic
CPU-prefix/GPU-tail heterogeneous execution grant. Adding a field therefore requires one concrete
consumer at that domain Seam rather than teaching every Module a fake common
shape.

UI raster residency has its own projection: the Window host applies bounded
SVG icon entry/byte limits to the UI-domain cache. This is not Preview media
work and is not projected in Headless mode.

Realtime Audio has its own typed projection rather than borrowing the uniform
background-domain shape. It carries decoded-source entry/byte limits,
persistent FFmpeg Session capacity, and an idle-warmup window count. The
nominal 8/16/32 GiB classes receive respectively 16/32/64 source entries,
64/128/256 MiB of decoded PCM, 2/4/8 persistent Sessions, and 1/2/3 optional
idle-warmup positions. Below-minimum and unknown-capacity profiles remain
explicitly distinguishable. Elevated pressure halves residency and limits
warmup to one position; Critical pressure quarters residency and disables idle
warmup. Realtime audio admission itself remains true.

The coordinator owns none of the following:

- a cross-domain job value or universal queue;
- worker threads or a worker pool;
- domain cancellation tokens;
- queue ordering, deduplication, deadlines, or retry policy;
- domain failure categories or terminal evidence.

Each deep execution Module applies the decision behind its existing Interface.
This preserves Locality: Proxy remains responsible for artifact identity and
fairness, Export for immutable snapshots and publication, Thumbnail/Waveform
for derived-media residency, Preview for frame scheduling, and Media
Import/existing-Asset mutation for Project-generation publication.

The Export queue exposes two deliberately different observation revisions.
Its complete diagnostics revision covers resource-grant publication, dispatch
admission, cooperative yield, counters, and retained jobs. Its jobs revision
advances only when the lightweight `ExportJobSnapshot` collection changes.
Both are wrapping, non-consuming dirty tokens for equality comparison only;
neither is an event count, job generation, or linearizable snapshot version.
The App event-loop Adapter observes the latter for product-model refreshes.
Consequently a playback-driven resource decision cannot dirty the editor
widget tree or schedule a Preview render merely because Export received a new
grant or closed its dispatch Seam. Headless diagnostics retain the broader
evidence without becoming UI state authority.

Proxy Generation applies the same projection rule at its own Interface rather
than sharing Export's job representation. Its complete diagnostics revision
covers Project binding, dispatch policy, admission/deduplication/freshness
counters, cooperative resource yield, attempt phases, and terminal evidence.
Its model revision advances only for Project binding, admitted demand, exact
priority promotion, Queued-to-Running dispatch, and terminal publication.
`poll_finished` consumes that model revision for the App event-loop Adapter;
policy publication and Running-to-Queued resource yield therefore cannot claim
a user-visible completion, replay terminal evidence, or dirty the editor widget
tree. Both revisions are wrapping equality tokens, never attempt identities,
event counts, or linearizable snapshot versions.

The snapshot `revision` is a saturating diagnostic publication counter, not
decision identity. A consumer compares the projection it owns (for example,
Preview's trim level) and applies semantic transitions from those fields.
Counter exhaustion therefore cannot make a changed decision look like an old
one or suppress resource-policy application.

### Cross-domain dispatch slots

Seven independently scheduled heavy domains participate in one small, pure
admission state machine: Proxy, Thumbnail, Waveform, paused-Audio warmup,
Media Import, existing-Asset mutation, and Export. The state machine sees only
aggregate `queued`, `running`, `user_initiated`, and monotonic terminal-
generation facts. It never receives a job payload and cannot execute, cancel,
reorder, retry, or terminalize domain work.

The below-minimum/unknown/8 GiB, 16 GiB, and 32 GiB classes admit at most one,
two, and four domain dispatch seams respectively. Elevated pressure admits one
and Critical pressure admits none. Realtime Preview/Audio closes automatic
heavy work but retains one slot for an explicitly admitted user Export; that
attempt freezes a CPU-capable execution policy unless its authored closure
requires GPU execution. This is deliberately a coarse cross-domain concurrency
grant, not a claim that every admitted domain has the same internal cost. Each
domain still applies its own bounded parallelism and byte/resource grants.

Allocation follows five invariants:

1. explicit user work is selected before automatic work when a slot is free;
2. a physically running incumbent is sticky until its safe scheduling
   boundary, so policy does not repeatedly cancel useful work;
3. terminal-generation advance or a bounded no-start grace permits
   round-robin rotation, preventing a continuous multi-file domain from
   starving another queued domain;
4. every revocation enters `draining` and creates a `close_epoch`, even when
   the cached pre-close demand snapshot reported `running == 0`;
5. no replacement opens until every draining domain has acknowledged the exact
   close epoch and that same domain has subsequently supplied a fresh
   `running == 0` observation. Domains may complete this proof independently;
   capacity reopens only after the draining set is empty.

Export's cooperative execution gate reports a yielded running attempt
separately from a physically executing attempt. The App demand projection moves
that bounded yielded population from `running` to `queued`: the immutable job
still requests its user slot, while the allocator receives the required fresh
`running == 0` proof. Keeping a yielded attempt in `running` would make Nominal
recovery wait for job completion while the same job waits for dispatch to
reopen after Critical pressure. Queue identity, cancellation, snapshot, and
publication authority remain unchanged across this slot reacquisition.

Handoff is a close/acknowledge/resample transition. `domains_to_close` repeats
every draining domain idempotently under the current `close_epoch`. The
composition root synchronously closes each concrete dispatch Seam—including
Window-owned Thumbnail/Waveform and App-owned domains—and acknowledges that
exact domain/epoch only after the close call returns. An acknowledgement proves
only that new starts are excluded; it does not prove that work racing the
earlier snapshot has drained. A later resource tick may retire a draining
domain only when that domain's demand was freshly sampled and reports
`running == 0`; cached observations for other domains neither help nor delay
that proof. No replacement opens while any domain remains draining.
Pressure/profile recomputation from cached demand cannot supply this proof. A
new revocation renews the epoch and invalidates partial acknowledgement of the
older close set. Likewise, running work first observed outside the tracked
allocation while dispatch is closed enters `draining` and produces an
acknowledgeable close transition instead of being treated as harmless.

Closing a Seam never discards user intent: every domain retains its own bounded
queue and cancellation/evidence rules. A completely idle Window includes the
coordinator's one-second monotonic deadline in its native `WaitUntil`
selection, so grace expiry and pressure observations cannot stall merely
because no input or playback event occurs. Headless Preview calls the same
resource tick before each bounded candidate; a frozen GPU or export attempt is
not reinterpreted mid-submission.

Paused-Audio warmup is a real participant rather than synchronous work hidden
on the main thread. Its dedicated single worker retains at most one latest
demand, binds it to the exact Project, Authoring Session, author generation,
Asset Library database revision, Sequence revision, and playhead, prepares only
causal blocks ending at the playhead, and publishes bounded terminal evidence.
Closing only its physical dispatch seam retains that one demand so the slot
allocator can see it; playback, seek, critical pressure, or author-generation
change disables the automatic policy and cooperatively cancels both queued and
running speculative work.

Window and Headless consumers call the same UI-independent App resource tick.
The tick samples current cross-domain demand on every call but lets one
coordinator-owned monotonic cadence decide whether a native memory observation
is due. Due observations are requested from one coordinator-owned background
Adapter; the UI/Headless caller only drains a completed immutable result and
never performs process enumeration or handle queries inline. While a request
is in flight, the Window uses a bounded low-frequency result-poll deadline;
Headless naturally consumes it from its existing bounded cadence. Probe startup
or channel failure becomes explicit conservative pressure evidence rather than
blocking or silently retaining a nominal classification. That low-frequency
policy Seam is the only Host path that applies the complete Preview projection
to `PreviewProductionRuntime`, once per resource tick. Per-candidate
Window work reads only the frozen `PreviewViewerGpuExecutionDecision` and
applies it through the renderer-owned `ViewerGpuExecutionRuntime`; it must not
reconfigure Preview scheduling, caches, or trim a second time. Headless applies
the same complete Preview decision and its narrow Viewer projection from the
exact snapshot used by that candidate, so it cannot invent a test-only quality
or residency budget. Preview diagnostics count complete policy applications,
and the Host boundary test proves that a per-frame Viewer projection does not
increment that count. This statement is deliberately scoped to Headless
Preview: it does not claim that a Headless presentation call owns or dispatches
every other background execution domain.

The lazy native process-memory observer remains an App-owned worker, including
the never-started case. Qualification first closes its request admission and
then consumes a terminal receipt against the same absolute deadline used by the
other `AppState` owners. The receipt distinguishes no startup attempt, failed or
partial startup, normal join, panic, timeout/detach, queued/running work,
retained resources, and cumulative failures; only exact lifecycle closure may
support terminal quiescence. Ordinary `Drop` may request shutdown as bounded
best-effort cleanup, but it is not worker-return evidence. Runtime endurance
facts use one atomic mutually exclusive queued/running inventory, preserve
started-versus-unexpected-exit identity, and monotonically count supported
native-probe failures. The terminal receipt reuses that same failure ledger, so
a failure discovered before or during shutdown cannot regress during merge;
unsupported probes remain explicit capability evidence and do not increment it.

Every bounded Headless candidate turn advances that complete resource cycle
before it polls an in-flight GPU submission, promotes a Prepared Viewer
Successor, reuses an exact-current alias, or requests new Preview work. Those
fast paths are output arbitration, not permission to suspend product-process-
tree observation or pressure policy. The professional video profile therefore
requires `resource_policy_applications >= planned observation frames`; a
startup-only application or a cadence starved by successor/alias reuse fails
the gate even when every frame and GPU submission otherwise succeeds.

## Pressure sampling and Preview residency

One process-owned monotonic origin drives a one-second pressure-observation
cadence independently from presentation frequency. The injectable
`observe_pressure_from_probe_at` Seam makes the exact cadence testable; calls
inside one interval neither re-probe nor republish a decision, and a late tick
does not manufacture catch-up samples. Pressure uses the checked platform-native
private-memory metric for the complete Mondrian root-plus-descendant process tree relative to
physically installed memory, plus independent available/load evidence when the
platform exposes it. The retained observation preserves the requested scope,
Backend, member count, inventory attempts, completeness, and failure instead
of flattening them into a scalar. `CurrentProcess` is never accepted as the
product value. The Windows Tool Help Adapter takes two descendant inventories,
opens every member for memory query and synchronization, retains those handles
through the second inventory, and accepts the sample only when membership is
unchanged and every non-blocking handle wait reports `WAIT_TIMEOUT`. A signaled
handle means the sampled process exited; `WAIT_FAILED` or any other wait status
is a distinct liveness-query failure. Linux `/proc` and macOS libproc use the
same two-inventory rule plus PID/start identity to reject PID reuse. Windows
Private Commit, Linux `RssAnon`, and macOS physical footprint remain typed,
non-interchangeable metrics. Neither result may be collapsed into a complete
sample. An unsupported tree Adapter, mismatched scope/metric, unstable child
inventory, failed member query, or partial sample places policy at least in
Elevated pressure while retaining the prior Critical state when applicable;
whole-system evidence may raise pressure further but cannot prove product
residency. The
classifier is hysteretic: Nominal enters Elevated at 60% and Critical at 75%;
Elevated returns to Nominal only below 50%; Critical remains Critical through
66.7%, then falls to Elevated until usage drops below 50%. This prevents both
frame-rate-driven probe traffic and budget churn near a threshold.

The Preview projection carries a persistent `PreviewFrameStoreConfig`, not only
a one-shot trim request. Machine class supplies the baseline media count, byte,
native-resource-unit, and Viewer count/byte budgets; Speculative and Aggressive
trim divide those baseline limits by two and four respectively. Failure
retention remains independently bounded. A changed Preview projection
reconfigures the shared playback-owned Store; identical typed limits are an
idempotent no-op even when a consumer ticks frequently. Reconfiguration
immediately evicts ordinary LRU entries until the new limits hold, and future
admission continues to use the lower limits. Explicit current-media and
current/stale Viewer pins are not
silently discarded; their exceptional residency remains visible in diagnostics
until the owning Preview boundary can safely release it. A stronger trim may
separately release decoder-backed media without conflating that lifecycle
action with Store budget configuration.

The same current-media resource-unit limit governs physical Interactive decoder
Session residency. The App partitions the family grant across the decode
workers that actually started and publishes that per-worker capacity through
shared worker-family resources. Workers reuse released Sessions before opening
another; a lower decision retires only slots whose native-output leases have
already reached zero. This keeps multilayer native decode proportional to the
same authority that admits its Frame Store surfaces instead of hiding a fixed
two-layer bottleneck or creating an uncharged decoder pool.

Viewer GPU output residency is a separate typed grant because renderer-owned
working/output textures are not Frame Store entries. Nominal
below-minimum/8/16/32 GiB profiles retain at most 1/2/3/3 idle textures per
exact extent/format/usage contract and 48/128/256/384 MiB across contracts.
Speculative trim retains at most one per contract while preserving the
machine-class byte grant. That bound is the frame-to-frame hot-set envelope,
not a duplicate-cache target: halving it can evict one UHD float working
texture as later display textures return and reintroduce synchronous GPU
allocation into realtime successor preparation. Aggressive trim grants zero
idle residency and requests idle release. This
narrow `PreviewViewerGpuExecutionDecision` contains only the renderer grant and
that release instruction; the Headless Adapter does not receive the unrelated
Frame Store, decode, title, or Effect policy fields. The single
`ViewerGpuExecutionRuntime` owner applies a changed grant online: idle LRU
entries above the new limits are retired synchronously, resources checked out
by an active candidate remain valid, and their later release observes the new
grant. Window and Headless therefore share one renderer Interface and one
policy projection without moving GPU resource ownership into UI or App state.

The same renderer grant independently admits one complete active Viewer
request. Below-minimum/8/16/32 GiB classes permit respectively
384 MiB/48, 768 MiB/64, 2 GiB/96, and 4 GiB/160 active logical texture
bytes/resources; unknown capacity uses the conservative 8 GiB values while
remaining distinguishable evidence. These active limits derive only from
machine class and remain identical across Nominal, Elevated, Critical,
transport-active, Speculative, and Aggressive decisions. Pressure can reduce
resolution only by causing Preview to issue a new explicit quality request; it
cannot shrink the grant beneath a frozen candidate or change its precision.
The renderer estimates and admits the complete request before texture
creation. Idle pool limits, active Viewer limits, and the inner heterogeneous
continuation grant are three different authorities and may not substitute for
one another.
Media-owned decoder surfaces remain charged to the Frame Store native-resource
grant. The Viewer grant covers the renderer-owned native bridge under a
validated 2:1 codec-padding envelope plus encoded-RGB and working outputs, so
native memory is neither omitted nor double-counted across owners.

The configured eight-frame playback prefetch/preroll depth is only a temporal
upper bound. Admission subtracts resident, pinned, and pending CPU bytes plus
native decoder-resource units from the same `PreviewFrameStore` diagnostics.
Each candidate reserves a conservative source-to-working/CPU-fallback cost;
when remaining bytes or native units cannot cover the next candidate, the
window shortens instead of oversubscribing. Nominal 8/16 GiB baselines are
192 MiB plus six native units and 384 MiB plus eight native units.

The Audio projection follows the same persistent-limit principle. A changed
projection reconfigures the shared Playback `AudioSourceCache`.
Ordinary PCM LRU entries and idle decoder Sessions are released immediately;
busy decoder Sessions finish their current window and converge afterward.
Temporary over-capacity residency and both PCM/Session trim totals remain
observable. Resource pressure has authority only over residency and optional
idle warmup. It cannot alter the prepared audio graph, automation, sample
coordinates, channel matrices, processor state, or error semantics.

Waveform receives an independent aggregate cache grant. Its service partitions
that grant into completed envelope residency and a private decoded-PCM cache,
then applies both online. The Waveform worker keeps one persistent decoder
Session because it is a single sequential analysis domain; it never consumes
Playback's PCM or Session entitlement.

Thumbnail and Waveform publish physical single-worker phase separately from
lifecycle pending state. A shared RAII activity ledger records
`WaitingForDispatch`, `Running`, and completed results awaiting publication.
The domain result consumer acknowledges the exact request identity and
generation. Consequently resource demand uses a real Running count; channel
queueing, a closed dispatch gate, result-publication backlog, cancellation, and
an obsolete Project generation cannot be collapsed into one inferred phase.
An obsolete request that is still physically executing remains visible as
resource usage but can never regain publication authority.

Preview Basic Title retains one result cache only. The worker's internal
`BasicTitleRasterizer` cache is disabled because it held the same frame bytes a
second time; the result cache receives a pressure-sensitive 16/32/64/128 MiB
grant for below-minimum/8/16/32 GiB classes before trim. Export owns one
job-local rasterizer and no second result cache. Its frozen job grant carries
4/8/16/32 entries and 16/32/64/128 MiB for those classes before the applicable
pressure divisor, so Preview and Export cannot create an unbounded title
residency pool.

Effect execution is likewise Session-owned rather than process-global.
Preview's typed projection grants 32/64/192/384 MiB and 8/16/32/64 entries for
below-minimum/8/16/32 GiB classes; realtime/Elevated and Critical pressure
divide cache residency by two and four. GPU lowering additionally receives
8/16/32/64 entries and 0.5/1/2/4 MiB across those classes, under the same
pressure divisors. Plan bytes conservatively include key and container
overhead; a plan larger than the grant executes uncached instead of escaping
the budget. The per-execution working-byte ceiling is a correctness admission
contract and is not shrunk underneath an in-flight frame. Export uses its
job-owned Effect Session and must receive an offline job budget at that Seam;
Preview's grant cannot be borrowed as an implicit cross-domain pool.

Finite temporal execution exposes checked Float32 source-coverage bytes before
materialization. Preview and Export first admit the complete frozen coverage
against their CPU active-working-set grant; the Effect Session then includes
that retained coverage, immutable prepared Mask geometry, maximum Path
row-crossing scratch, exact shared-sample liveness, one final output and one proved scalar tile live set in
its hard per-execution peak. Prepared Path segments and their spatial index are
built once per evaluated graph and shared by every tile. When direct execution
does not fit, deterministic tiling reduces only the tile live set. It cannot
hide source or output residency, exceed the 4,096-tile operational cap, or
publish a partial result.
A planning gate fixes the standard-class UHD two-frame case at 64 `480x270`
tiles and a 402,278,400-byte logical peak under the 384 MiB grant; it does not
substitute an allocation-free estimate for later process-memory qualification.

The CPU compositor grant also governs recursively materialized nested visual
outputs. A canonical `PreparedVisualFrameClosure` performs checked conservative
admission before Preview or Export requests child pixels: it charges one
RGBA32F output for every distinct placement/temporal instance plus four
largest-node canvases for the compositor's worst local coexistence. The check
uses the same owner-scoped `max_active_bytes` that each later composite uses;
it is not a separate advisory estimate. Therefore a wide or deep nested frame
cannot accumulate unbounded host outputs outside the compositor grant.
Decoded media, Effect-session residency, and OCIO processor residency remain
separately governed.

The renderer's Prepared Visual Execution Module consumes that admitted closure
with one iterative child-before-parent schedule. It retains at most the one
Adapter output already counted for each prepared instance and calls each node
exactly once; Preview and Export no longer own recursive stacks or hidden
nested-output pools. The schedule is request-local metadata proportional to the
already bounded closure node/binding count. It does not create another worker,
cache, GPU grant, or decoder residency owner.

Preview heterogeneous execution freezes two correlated grants before any CPU
prefix starts. The CPU grant bounds one atomic addressed batch, Effect Session,
graph steps/materializations, and retained input-plus-all-frontier pixels. The
Effects-prepared route publishes the exact frontier charge, and Renderer sums
it with each immutable input before scheduling; policy never assumes one
frontier per item. The
GPU grant bounds the matching upload/device continuation and grants zero
readback because the value proceeds directly into the Viewer composite. Its
texture/resource admission consumes the heterogeneous plan's exact peak live
device-materialization count; it never derives a texture count by dividing
device bytes by an assumed RGBA32F frame size. The 8 GiB class admits one UHD
RGBA-F32 input plus one frontier value with a 256 MiB retained pixel grant;
wider frontiers consume their exact additional bytes. The 16 GiB class raises
the batch pixel grant to 768 MiB while
retaining the same graph semantics. Elevated or Critical pressure may trim
optional Effect cache residency but cannot reinterpret or shrink these grants
under an admitted attempt.

OCIO CPU processor residency follows the same explicit-owner rule. Preview's
`TimelineCompositeScratch` owns one `RenderCpuColorExecutionSession` spanning
source adaptation, effect-domain conversions, nested working conversion, and
the final Program/monitor boundary. Below-minimum/8/16/32 GiB profiles retain
at most 8/16/32/64 processors before the same pressure divisor is applied.
Each Export job and the Thumbnail worker own independent bounded Sessions and
release them with that execution lifetime; none borrow Preview's grant or a
thread-local cache.

Export's projection is one coherent `ExportExecutionResourcePolicy`, not a
collection of live knobs. It grants the attempt-local Prepared Visual Program
cache including its LUT Preparation Cache, Effect pixel/topology/GPU-plan
residency and temporal/ROI working limit, heterogeneous route-contract ledger,
CPU color-processor Session, GPU output idle-texture pool, and Basic Title
cache. The policy separately freezes `gpu_visual_active` for the complete
GPU-resident visual closure and `gpu_output_active` for the final output plus
readback. The route ledger is correctness/admission state, not GPU-plan cache
residency: its entry and conservative logical-byte grants remain stable across
Nominal and Elevated pressure, while retained GPU-plan entries/bytes may
shrink. The below-minimum/8/16/32 GiB profiles grant 8/16/32/64 immutable route
contracts at 128 conservative logical bytes each. Their GPU visual grants are
384 MiB/768 MiB/2 GiB/4 GiB across 48/64/96/160 active textures. Every node
includes already-retained nested outputs plus conservative upload,
Effect-domain, Transition, and working-composite demand; rejection happens
before that node records and becomes terminal only if an earlier GPU node has
already started. The same profiles grant
384 MiB/512 MiB/1 GiB/2 GiB and four resources to one final GPU output
boundary. That pressure-stable grant covers the exact working input texture,
encoded output texture, and padded readback buffer; the separate idle pool may
still be trimmed. The policy additionally freezes eight resident-encoder
surfaces and 512 MiB of conservative logical surface bytes for the qualified
Windows HEVC route. Export rejects admission before execution unless the exact
NV12/P010 pool fits both limits. Renderer detached input leases remain charged
to their originating GPU output pool until the cross-queue completion wait has
been enqueued; poisoned leases and destination surfaces remain owned by the
resident Adapter until retirement rather than being returned under unknown
native state. A plan that exceeds either active limit is rejected before GPU
allocation and records an explicit `ActiveWorkingSetRejected` CPU-fallback
reason rather than silently reducing delivery precision. The queue copies the current
policy when a pending job becomes one `Running/Preparing` attempt, and that
immutable value constructs the attempt's single `ExportVisualRenderSession`.
A realtime decision also freezes whether equivalent CPU-capable visual work
may use opportunistic GPU acceleration. While Preview or Audio owns realtime
execution, the one admitted explicit Export uses its exact Float32 CPU route
for closures that support it, including the final color/output boundary, and
does not qualify a resident hardware-encode route. A closure whose Effect
contract has no exact CPU Float32 route retains its required GPU execution;
the scheduling policy cannot reinterpret authored processing semantics.
A later resource decision may close dispatch or request a safe-boundary yield,
but it cannot resize or reinterpret the running attempt. Only a later
dispatched attempt freezes the later grant.

The Window UI projection grants the exact source-and-size SVG raster cache
64/96/128/256 entries and 8/16/32/64 MiB across those same machine classes.
Elevated/realtime and Critical pressure divide both limits by two and four.
Reconfiguration trims synchronously; parsed bundled SVG geometry is a separate
finite product-artwork set.

## Priority and degradation order

Realtime Preview and Audio are always admitted. When transport is active,
automatic background Modules and explicit Import/Proxy mutation stop dispatching
new work. One concrete user Export may receive a single bounded offline slot at
Nominal or Elevated pressure; this is the execution path used by Concurrent
Recovery qualification. Its CPU-equivalent work yields the GPU to realtime
Preview and Reference Output; GPU-required Effect work remains explicit and
bounded by the frozen Export policy. Critical pressure closes that slot as well. Additional
Export attempts, Import, and user Proxy requests remain admitted into their
bounded domain queues and resume when policy grants a slot. The coordinator itself owns no cancellation
token, but each deep Module may implement a cooperative safe-boundary yield
without terminalizing or recreating the user's intent. Proxy cancel/requeues
the same running attempt when its backend acknowledges the resource yield;
Export blocks the same attempt at queue-owned execution gates. Domains without
such a contract complete already-running work under their own bounded policy.

Existing-Asset relink and audio-Component repair are explicit FFmpeg probe
work, so they follow the same rule: intent remains bounded and visible while
its ordered worker dispatch is paused. They cannot bypass realtime priority
merely because their final SQLite commit occurs on the event loop.

Outside realtime playback, explicit heavy demand or Elevated pressure closes
new automatic admission. Explicit Import, existing-Asset mutation, Export, and
user Proxy intent remains in its bounded queue and is selected before automatic
work whenever a coarse slot becomes available. An automatic attempt already
running is allowed to reach its safe boundary; an already queued automatic
request may drain only within the remaining slot allocation and cannot displace
an explicit waiter. Elevated pressure additionally reduces the whole heavy
allocation to one domain. At Nominal pressure with no explicit heavy demand,
bounded automatic queues may admit and dispatch normally. This gives the
product one simple priority order:

1. realtime Preview and Audio, plus at most one bounded explicit Export;
2. explicit user Import, existing-Asset mutation, remaining Export, and Proxy work;
3. automatic derived-media and recovery work.

Whole-process pressure then applies monotonic degradation: Elevated pressure
suppresses automatic work and trims optional residency; Critical pressure also
pauses new explicit/offline dispatch and requests aggressive trimming. Preview
may temporarily reduce runtime resolution, without changing source
   selection, proxy/original policy, color processing, author settings, or
   export semantics.

Recovery publishes a later immutable decision and reopens domain dispatch.

## Media Import execution

Media Import is an instance-owned App Module with a fixed worker set, bounded
batch/file/result capacity, cooperative batch cancellation, and bounded
terminal evidence. A batch binds the current Project generation, but workers
never retain or write an Asset Library. Its two-stage execution is:

1. a worker supervises the packaged Isolated Media Probe Helper. The Helper
   canonicalizes the path, captures a complete source fingerprint, probes
   immutable media metadata, verifies the same revision, and returns one
   versioned bounded response. Cancellation or a 120-second monotonic deadline
   kills and reaps the process before the attempt becomes terminal;
2. the serialized App poll/commit Seam locks the import generation, rechecks
   batch existence and cancellation, verifies the fingerprint again, and only
   then passes the candidate to the current Project's Asset Library. One
   SQLite transaction publishes metadata, stable audio bindings, and requested
   folder placement together.

Preparation and commit are separate Interfaces. Worker state retains only the
Preparation Adapter and packaged executable path; the serialized execution
owner retains the Asset Library Commit Adapter. Neither the Helper nor its
supervising worker owns an Asset Library handle. This capability split makes
“worker cannot write Project state” structural rather than a convention.

Media Import exposes two wrapping, equality-only observation tokens. Its
complete diagnostics revision covers resource-policy publication and worker
phase as well as batches, counters, publications, and terminal evidence. Its
model revision covers only product-visible Project-generation, batch
admission/rejection, cancellation, prepared-result availability, publication,
counter, and terminal-record changes. `set_resource_policy` and a queued/running
phase transition advance only complete diagnostics. The App event-loop Adapter
polls the model revision, so a fixed-cadence resource decision cannot dirty the
editor model or recursively request another resource decision merely because
Import dispatch or parallelism changed. Explicit diagnostics observe that
policy immediately. Admission rejection advances both revisions because its
bounded rejection counter and terminal record are real product evidence.

Host integration validation exercises the packaged Isolated Media Probe Helper
rather than replacing it with an in-process probe. Its orchestration wait remains
bounded, but admits normal debug-build process cold-start and scheduling jitter;
the wait is a correctness deadline, not a three-second media-probe performance
service-level objective. The Helper retains its independent 120-second product
deadline and process-reaping contract.

Closing or replacing the Project:

- advances the import generation;
- cancels queued and running old-generation work;
- clears App presentation state for those batches;
- classifies any late prepared candidate as `Superseded`;
- forbids that candidate from writing either the old or new Asset Library,
  publishing an Asset event, or configuring Proxy state in the new Project.

A Media Import or Proxy Project-binding replacement always advances generation, even when the
persisted `ProjectId` is unchanged (for example, reopening the same archive
into a new Authoring Session bound to a fresh Project Library Generation).
Project identity is not execution-binding
identity.

Import capacity accounting retains a superseded probe until its Helper has been
terminated and reaped and the worker/result transport is released, but product
demand counts only current-generation batch remainder. Superseded work cannot
impersonate current user intent or retain an uninterruptible in-process FFmpeg
call that starves the new Project's automatic Thumbnail, Waveform, and Proxy
work.

Explicit cancellation is available through a stable batch identity. Queued
probes terminate immediately. A running Helper is killed and reaped before
cancellation or deadline evidence is published, so no foreign FFmpeg call
remains in the App process. `Drop` cancels and wakes every worker, then joins
within a bounded grace period. Only the supervising parent worker may be
detached after a rare settlement overrun; it owns no Project or Asset Library
handle, its result receiver is gone, and it has no path to the serialized
commit Seam.

## Proxy cooperative yield

Proxy has two independent dispatch gates. Closing global dispatch asks every
Running attempt to yield. Closing only automatic dispatch asks Running
`Import` and `PlaybackRecovery` attempts to yield, while Running and Queued
`User` attempts remain eligible. In both cases queued demand stays queued and
the selected Running attempts receive a resource-yield request through their
existing cooperative cancellation token.

Requeue is allowed only if that exact current attempt returns `Canceled` while
the yield request is still current and the service is not shutting down. The
service then retains the same attempt ID, request, current origin, generation,
and queue intent, installs a fresh cancellation token, advances queue ordering,
and returns it to Queued. That yield creates no terminal cancellation record
and does not poison failure memory. If the backend crossed publication before
observing cancellation and completes normally, normal terminal handling wins.
Project-generation rotation, explicit user cancellation, and shutdown remain
their own terminal policies and cannot be mistaken for a resource yield.

An exact higher-rank request promotes both Queued and Running attempts. Queued
promotion invalidates the stale queue entry, enqueues the same attempt in the
higher-rank queue, and wakes dispatch. Running promotion updates the attempt's
current origin so diagnostics and eventual terminal evidence describe the
strongest admitted intent. A Running automatic attempt whose resource yield
was already requested cannot revoke the monotonic cancellation token; after
the backend acknowledges cancellation it requeues under the promoted `User`
origin and may resume even while automatic dispatch remains closed.

Terminal consumption is also ordered independently from attempt admission.
Every terminal publication receives a service-lifetime monotonic sequence;
the serialized App Adapter retains that sequence as its cursor and consumes
only a strict delta. `attempt_id` is not a valid cursor because concurrent
attempts may complete out of admission order. Bounded-retention gaps remain
explicit diagnostic evidence. Only a failure from the delta snapshot's current
Project execution generation may enter user-visible status; retained evidence
from a retired binding stays diagnostic. A nonterminal policy revision can
never replay an old failure into the status log.

## Export dispatch

The Export Execution Module admits and exposes explicit jobs while dispatch is
paused. Pending jobs retain their immutable Timeline Export Snapshot, output
reservation, generation, cancellation authority, and visible lifecycle state.
Resuming dispatch wakes the dedicated offline worker.

Each running attempt owns one queue-issued `ExportExecutionGate`. Executors
rendezvous with it at frame, audio-block, and phase boundaries. Closing dispatch
therefore waits the same attempt at its next safe boundary; it neither creates a
replacement attempt nor discards already completed work. The gate checks
dispatch, cancellation, shutdown, and entry to `Publishing` while holding the
same queue-state lock. When Publishing is admitted it commits the attempt's
irreversible-publication state under that lock, eliminating a pause/cancel race
between the check and the transition. After that commit the gate remains open:
later realtime pressure, shutdown, or cancellation cannot interrupt the final
atomic publication or change terminal disposition away from its result.

That boundary is externally visible as
`ExportPublicationState::Committing`, not inferred from a progress fraction or
a private boolean. `ExportCancelOutcome::TooLateCommitting` is returned without
mutating the attempt's cancellation token, and its own bounded counter
distinguishes rejected late requests from accepted cancellations. Queue
diagnostics report a running-yield request only when dispatch is closed and at
least one reversible running attempt can actually reach such a boundary;
pending-only, terminal, cancelling, and committing jobs cannot manufacture this
fact. Completion publishes `Published` only from durable Storage publication
evidence. Cancellation and execution failure before namespace mutation publish
`NotPublished`; a typed pre-namespace publication failure may do the same even
after the queue acquired `Committing` authority. If the final route names the
new object but parent-directory durability is not confirmed, the terminal
state is `DurabilityUnconfirmed`. An indeterminate namespace postcondition, or
an untyped failure/panic after committing authority, is conservatively
`OutcomeUnknown` and never mislabeled as successful cancellation.

The Queue Interface also exposes allocation-free `can_cancel` and
`has_terminal_history` observations for product-action availability. These are
guidance only: `cancel` remains the sole authority and returns the exact
`Requested`, `AlreadyRequested`, `TooLateCommitting`, `AlreadyTerminal`, or
`NotFound` outcome under the queue lock. Terminal-history cleanup likewise
returns the exact removal count from the same lock. “Terminal” includes
Completed, Failed, and Cancelled evidence; the product must not label this as
completed-only cleanup or report success when no evidence was removed.
# Professional realtime Viewer grant

The App maps the `Professional` machine class to
`ViewerGpuExecutionResourceGrant::professional_realtime()`, the same
Renderer-owned grant used by sealed 4K/8K qualification. The pool remains
demand-driven: selecting the class allocates nothing. Speculative pressure
reduces duplicate retention to one resource per exact contract, while
aggressive pressure releases all idle textures. Neither pressure state lowers
the 4 GiB active byte ceiling, 160-texture ceiling, working precision, or
frame semantics.

Lower machine classes retain their smaller product grants and are not implied
to satisfy the 8K qualification profile. Machine classification from system
RAM also does not prove GPU capacity; the coordinated matrix records and
checks the reference-machine GPU inventory and the real Renderer workload.
