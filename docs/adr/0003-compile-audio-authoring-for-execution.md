---
status: accepted
---

# Compile audio authoring into one shared execution pipeline

## Decision

Playback, export, audition, analysis, and nested Sequence evaluation use the
same three-stage Interface:

1. `compile_audio_program` validates a Sequence and resolves one Program Output
   plus an optional transient audition overlay into immutable semantic IR.
2. `PreparedAudioPlan` binds that IR to one explicit Render Contract: sample
   rate, semantic channel layout, maximum block size, realtime/offline mode,
   and a processor-private Session scratch admission budget.
3. `AudioRenderSession` owns exclusive mutable buffers and processor state for
   one run. It renders exact integer-sample windows into caller-owned storage.

A retained native owner can remain valid while its processing capability is
stopped. Configuration refusals and pending installation must preserve control
queries, retry, seed, GUI service and Shutdown. The same command dispatcher then
borrows only that owner's main role; it neither starts a DSP scope nor resumes
processing to keep the connection alive. The main borrow ends before exclusive
lifecycle commands execute. Its actual allocations retain the original grant.

VST3 capability comes from the internal lifecycle that guards its real role
split, rather than a facade's cached processing flag. CLAP initially permits a
Stopped processing handle for its existing Ready/first-signal contract; a
committed seed waits for explicit Enter. Adapters derive these distinctions
from their existing lifecycle and seed owners, with no second state authority,
dispatcher, routing graph or wire protocol.

The Render Contract carries one canonical `AudioChannelLayout`, never a bare
channel count. The value is Mono, a validated named-speaker set in canonical
interleaving order, or a bounded Discrete bus with deliberately absent speaker
meaning. Standard 5.1(side), 5.1(back), and 7.1 are distinct values even where
their extents match another layout. Representability does not grant execution:
media, plugin, device, and export Adapters must negotiate an exact layout or
fail closed, and may not select a matrix from channel count alone.

Auxiliary inputs retain their own semantic layouts and per-processor packed
channel prefixes in the existing prepared schedule. Track/Bus taps still use
the Render Contract layout. Their existing standard channel matrix is resolved
at preparation, applied before destination-width PDC, and followed by Route gain
at target signal time. Unconnected ports own zero-filled PCM. Equal layouts keep
the direct path; connected mixed layouts reuse one preallocated, charged mapping
buffer. This changes neither author state nor the routing/execution authority.
Additional outputs retain their current Main-layout restriction. IPC22 derives
shared input capacity from the actual ordered port layouts at both ends, packs
short blocks at their current frame count, and charges the existing shared-region
byte extent. Equal-layout regions keep their former offsets; older protocol
versions refuse. Generic Worker views retain each input width. CLAP admits
independently typed Mono/Stereo auxiliary inputs and preserves their widths in
metadata, native planes, SDK capacity and declared/actual storage fees; its Main
and all outputs retain the render layout. VST3 likewise retains each auxiliary
input's exact Mono/Stereo Speaker Arrangement in cold and stopped retained
metadata. It reuses the SDK's existing per-bus planes and arrangement negotiation;
its native resource bound includes the widest admitted input. Both formats
validate counts and prepared storage against semantic layouts before processing.

`AudioProgramRuntime` is the shared high-level Implementation used by playback
and export. It recursively instantiates nested public outputs and binds media
through the narrow `AudioMediaResolver`/`AudioDecodedSource` Adapter Seam.
FFmpeg paths, asset databases, CPAL devices, export files, and UI state do not
enter the compiler or DSP core.

Timeline transport activity is an explicit consumer-declared fact for a rendered
window, independent of sample coordinates, realtime/offline scheduling, and
whether a device has started consuming PCM. `TimelineTransportActivity` has
explicit Playing/Stopped values; absence remains None. Playback preroll requests
carry Playing because their PCM is rendered for future advancing playback and
must remain valid when the same generation starts consuming. Idle/control callers
must not infer Stopped merely from a disabled device. This adds no clock, author
parameter, routing graph or project-file field.

The same optional fact now travels through the existing Render Request, PDC and
Scope windows, nested Runtime, isolated Process command, and format adapters.
Export supplies Playing; the App forwards the PCM consumer's fact. Independent
paused warmup owns a separate Runtime and supplies None, with no DSP/PCM promotion
to Playback. No device or processing-mode inference supplies transport state.

An entered epoch binds its exact optional activity on the first successful
render. A change refuses before native callbacks, PCM publication or history
consumption and requires a fresh epoch. Unentered stateless evaluation can change
activity, invalidating nested prefetched PCM through the existing cache owner.
IPC22 preserves None/Stopped/Playing in fixed 32-byte Process tags; old
helpers refuse. The VST3 borrowed processing role updates only its ProcessContext,
after canonical time installation, honoring requested/available validity. CLAP
sets IS_PLAYING only for explicit Playing. Ordinary Audio/SDK tests and App/Export
test-target compilation pass. Four Windows owned native gates verify actual flags,
PCM and Scope/retime/PDC for all three activities; each format has twelve canonical
rate/music/activity conditions and two epochs per condition. Separate bounded
Windows owned gates now verify actual App Media software-preroll PCM and public
Export Main/stem artifacts, including Source/child closure. App Note/author CAS
regressions also pass; see the compatibility matrix for exact conditions.
Complete product/device workflows, third-party Playing response, dynamic activity
events, signatures, loops and realtime behavior remain separate work.

Compilation derives every generated contribution's Track ID, Sequence range,
source mapping, and nested placement from the real `Track -> Clip` hierarchy.
It combines those facts with the attached `AudioComponentEdit` and referenced
`AudioProcessingScope`. Generated contributions and stages are immutable IR,
not persistent routable entities. Every generated operation retains typed
author origins for diagnostics and future state allocation.

Compilation retains nonempty Note lanes only for their actual selected active
Track rack processors, ordered by stable author IDs. Unselected/bypassed score
intent remains in the author aggregate. In the model-only M1 implementation,
Plan preparation checks event-count overflow and projects both endpoints with
the existing Ceil sample conversion; collapsed or overflowing endpoints are
typed errors. It then explicitly refuses selected Note execution before that
Plan's schedule/factory preparation. Empty or unselected score retains the
existing path. This per-Plan guard is not a whole-root no-media-I/O guarantee
for nested preparation. Executable events must later deepen the same pipeline
with explicit port, occurrence, epoch and resource admission; they cannot use
a separate vendor Timeline or bypass the existing Routing DAG.

The first M2 infrastructure slice adds concrete Note input and copied End
observations to the existing processor block IO and supervised process command,
without changing the processor invocation or parameter transaction stream.
An immutable factory Note contract binds definition-owned port keys, native
stable IDs/current indexes and finite input/output/retained-record/voice extents.
Its default is explicit zero support. Seed replacement and instance creation
recheck that same contract; existing allocation visitation observes its actual
Vec/String owners. Protocol v9 uses the reserved command count field and checked
appended shared regions; Note-free PCM/parameter offsets and total bytes remain
unchanged. A successful Note-mode Process returns its actual bounded End count
alongside any pending reprepare request through the same inline/scoped completion.

CLAP typed input retains exact f64 velocity, including zero On, and sorts sample
offset, parameter/Off/On phase, then source order using actual charged owner
layouts. Core NoteEnd is extent-checked before payload access and before the
parameter clock path: its time and velocity are unusable, while concrete Note ID,
original input P/C/K address (including those wildcards) and raw flags are copied.
Callbacks return no voice credit. End refusal is sticky through native return;
parent rejection after DSP permanently fails that adapter, preventing repeated
delivery. Flush preserves unconsumed Process End observations. The sole Hosted
ledger, prepared score and whole-root admission were still pending in that
carrier-only slice: selected scores retained the M1 refusal, native CLAP/VST3
Note requests were explicitly refused and HostNotePorts was not advertised.
Carrier controls alone grant no instrument, realtime or third-party qualification.

The next finite M2 slice projects checked Note endpoints once onto the Render
Contract grid and binds definition-owned port keys to cached native IDs/current
indexes at the exact selected Track rack occurrence. One Hosted processor owns
one non-cloneable Note ledger and bounded input, copied-End and admission scratch.
All targeted storage is prepared unbound before factory creation; only successful
existing Installed-root binding and entry establish its origin. Native Note IDs
are monotonically issued and never reused across cold epochs. Original input
tuples survive release-pending and early-End phases until their authored Off;
future Off/End events cannot finance current voice or record admission.

Root/Session window admission accounts for every priming and requested call,
subtracts the prepared stage's input-signal delay once, and checks all occurrences
before destination clearing, media access or DSP. Cold Note entry currently
requires public origin zero; nested Note entry and destructive state/reconfiguration
are explicitly refused. Inputs and copied Ends use the existing processor call.
Unrecoverable partial delivery or rejected output poisons that owner rather than
allowing a retry. Continuing parameter handoff prevalidates the unchanged score
and every bound peer, then publishes the new existing lease origin under the
Source guard while retaining record phases, tuples, counters and native IDs.

The shared Note contract now requires an admitted channel width and immutable
completion policy. `NativeEnd` retains its original voice/End rules;
`SuccessfulOffBlock` retires only original Off pairing records after that actual
call and all returned facts succeed, with no claim about DSP release voices.
Whole-root admission uses real priming/public call boundaries and grants no
same-call Off credit. Cached-IR identity checks still scan prior window events
and can take quadratic work; this foundation grants no RT/performance scope.
IPC15, full CLAP offer 5 and independent Note probe 2 reject older shapes;
CLAP remains explicitly NativeEnd/16 channels. VST3/canonical Note admission
is unchanged.

`AudioProgram` owns one optional validated `MusicalTimeReference`: the
Sequence-second position of quarter-note zero, a positive quarter-note duration,
and an optional constant `MusicalTimeSignature`. Positive u16 meter scalars are
an explicit product range, not VST3's entire int32 range. Bar zero starts at
quarter-note zero; negative bar selection uses exact floor. Unknown meter never
implies 4/4. Maps and an independent pickup/bar origin remain unavailable.

Existing author/sample time and Note coordinates remain unchanged. The entire
reference participates in compiled/closure identity, preparation, Ready and
callback binding; parameter handoffs/configuration seeds cannot replace it.
Document schema35 preserves unknown meter when migrating schema34 and refuses
nonnull meter mislabeled as legacy. The prior reference migration remains
chained; container/library versions are unchanged. IPC22 carries a required
nullable reference without a second clock registry.

Both adapters floor bars before format rounding. CLAP independently checks its
i32 bar number and i64 beat ticks. VST3 rounds the full rational bar start to
binary64 without CLAP's integer limit and advances it at each native chunk.
An anchored SDK meter refuses an independent signature setter. Optional validity
follows supplied facts and each format's contract. Scope parameters retain local
time; DSP transport retains Sequence signal time after PDC, without multiplying
Tempo by source retiming. Selected nested musical execution remains refused.

Meter implementation has Windows ordinary and owned-DLL SDK lifecycle/setter
qualification; matching current-worker/native meter DSP/PCM and third-party
workflows remain pending. Prior Tempo/PPQ/activity qualifications retain their
separate sources and conditions in the compatibility report. They do not qualify
the new meter workflow or realtime/performance/other-platform execution.

Note-enabled CLAP probes/workers now offer the actual shared NotePorts callback
table and validate the restored, render-bound port cohort. Raw rescan flags and
wrong-thread/unsupported observations stay bounded and sticky. Metadata-only
probes retain their original unsupported service. ProbeNotes schema 1 remains
the original Note-only offer. Schema 2 captures audio layout, parameters,
latency, tail, render mode and Notes from one restored, Notes-enabled native
instance; its public facade borrows the captured processing facts. The
application Resolver must consume this coherent offer before author Notes can
be enabled, rather than combining ordinary and Notes probe results.

The schema-2 query validates inactive metadata before activation and after
deactivation, without starting DSP. Existing unsupported callback, thread,
parameter-source or port-cohort observations cause refusal. Only this query
retains the same native-services owner beyond destruction, so callbacks during
destroy remain observable through the original source. Default probes and
workers keep inline services. This adds no second mutable authority; it also
does not establish complete accounting for SDK wrapper layouts, Arc ownership
or physical memory.

For empty selected scores, Runtime preparation projects a borrowed upper bound
using the existing prepared-logical policy before media resolution or processor
creation. The existing closure ledger charges that local Plan while children
are prepared; pending ancestors and completed siblings remain charged. A private
non-cloneable receipt is consumed once by the synchronous builder. Completion
requires actual policy bytes not to exceed the projection, then atomically
replaces the local reservation using the unchanged Session admission checks.
This is policy accounting, not a native execution permit or allocator/RSS bound.
Before auxiliary metadata exists, sidechain counting is conservative and can
reject a graph whose eventual actual policy would fit. Existing final footprint
values and limits are preserved.

Preserved native parameter preparation also borrows the entire retained Runtime
tree before its first local schedule clone or scratch allocation. It adds the
current Root closure's logical footprint once, then only the requested weights
of actual replacement local Plans. An equal nonselected peer contributes no new
Plan; the selected Session retains the existing forced-replacement rule. The
estimator first checks existing canonical Program compatibility, including the
bitwise immutable Note score, and reuses current cached Note storage without
reprojecting endpoints. It consults the same frozen Root grant. The final actual
owner-union/overlap checks and Source installation, Drop, CAS and Ack semantics
remain authoritative and unchanged. This preflight is a policy check, not a new
reservation or lifetime-accounting issuer. StateEntry preparation and all
externally retained new/retired Preserved handles still require their own
admission work; this change does not close those frontiers.

Public nonempty-score Plan preparation still runs the allocation-free M1
validation and returns typed Unsupported before projection/factory resolution.
Initial Note IR preparation peak and author-handoff clone admission, including
all externally retained prepared handoff handles, must be charged to the existing
prepared-resource grant before lifting this gate. Empty-score policy admission
does not discharge those obligations. Owned model
tests and direct fixture-DLL processing verify only their named boundaries;
they do not establish production Timeline Notes, Parent IPC, instruments,
realtime safety, comparative performance or third-party compatibility.

An authored Processor Instance is unique across all Sequence insertion points.
Preparation preserves Rack order and materializes generated processor
occurrences keyed by the author instance plus exact Contribution or Routing
Node owner and insertion point. Sharing one Processing Scope shares definition
intent only: overlapping Contributions receive independent mutable processor
state. Algebraically combining Gain processors is forbidden even when it would
produce the same scalar result, because that would erase the boundary required
by state, latency, parameter delivery, bypass, and diagnostics.

The admitted signal order is normative:

```text
decoded component / nested public output
-> Scope input trim and ordered Scope rack
-> edit-local volume, pan, fade, and explicit Transition envelope
-> Track sum
-> Track input / pre-fader rack / fader / post-fader rack / mute gate
-> typed Routes and topologically ordered Buses
-> Program Output strip
-> consumer Adapter
```

Internal PCM is floating point and may exceed `[-1, 1]`. Summing never
normalizes, clips, applies `tanh`, or inserts a limiter. Output limiting,
loudness, dither, channel packaging, and monitor calibration are explicit
processors or downstream delivery/monitor contracts.

All author time is exact rational `TimelineTime`. A Render Contract supplies
the concrete Evaluation Grid. Each output sample is mapped through Clip speed,
source in, edit-local offset, Scope-local offset, and nested boundaries without
floating-point-seconds chunk boundaries. Exact automation evaluation must be
invariant under block partitioning.

Each generated processor receives a borrowed parameter-event batch from
Session-preallocated storage. Lanes use stable `ParameterId` order. A constant
lane emits one event at block offset zero; a varying lane emits exact
definition-domain values at every sample offset. VST3/CLAP/native/GPU Adapters
may lower that batch only after capability and schema negotiation; parameter
array indexes and normalized ABI values never become author identity.

Route closure is selected from the requested stable Program Output. Canonical
compilation contains no consumer-purpose flag. Solo is an explicit audition
overlay and produces a non-canonical closure. Scheduler windows, watermarks,
deadlines, and transport state never enter author semantics or plan identity.

Nested Sequences are sources selected by stable child Program Output ID. The
runtime may share immutable child plans, but each nested instance receives an
independent mutable Session and cache. Missing children and cycles fail before
rendering. Nesting beyond the shared 16-level Sequence render contract also
fails. The parent never reaches into child Tracks, Buses, Roles, or Routes.

Unresolved or unsupported processors survive semantic compilation with their
complete author data intact, then fail concrete plan preparation when no
Resolver can bind them to the Render Contract. Bypass is explicit authored
intent. There is no implicit “plugin missing, therefore dry” rule. Realtime
render failure is reported to Audio Playback, which may preserve scheduled
media position with same-duration silence only for an independent-window
renderer; a stateful generation fails as a whole. That scheduling protection
is not a second DSP interpretation. Offline export fails the job and reports
the reason.

## Mutable state and processor contracts

Stateful processor identity is the resolved signal closure plus continuity
epoch, processor origin, nested instance path, time mapping, direction, and
processing mode. Equal author fingerprints may locate immutable-plan reuse but
never authorize mutable-state sharing.

Seek/cold entry, exact continuation, and controlled transition are distinct
state-entry operations. A future checkpoint restores only its checkpoint
position; replay/preroll is still required to reach a later state. Plugins
default to no checkpoint, replay, or state transfer capability until a host
Adapter proves it.

Every processor must separately declare compensable algorithmic latency,
meaningful tail (`None`, finite non-zero frames, or `Infinite`), and supported
layouts/automation cadence. Parallel paths are delay-compensated before sums
and Transitions. Intentional audible delay is signal-processing semantics, not
algorithmic latency, and must not be reported to PDC merely because both use
sample history. Sequential Rack tails are conservatively combined with checked
arithmetic; native plugin sentinel values are normalized by the concrete
Adapter. A latency, tail, layout, or Session-storage capability change causes
re-preparation and controlled re-entry; it cannot mutate the live graph inside
a block.

Built-in, hosted, and test processors use the same prepared Host contract.
Intentional audible delay reports signal history and tail without claiming
compensable latency; algorithmic lookahead reports compensable latency without
changing public Timeline alignment. Every processor must prove bounded Session
storage, fresh-epoch behavior, block-partition invariance, mode admission, and
failure semantics through that common contract. A new processor may deepen the
Host but must not introduce a processor-specific playback, export, or nested
output path.

Preparation solves checked Contribution and port-specific Route compensation
at every sum. Each prepared stage retains its input-signal delay, and all
downstream automation/parameter delivery evaluates the signal coordinate rather
than the later execution coordinate. A nested child public output is already
Timeline-aligned, enters its parent with zero algorithmic latency, and still
propagates its state-entry obligation. Prepared Contribution and
Route compensation executes through Session-preallocated,
block-partition-invariant delay lines. Preparation propagates a state-entry
obligation through nested outputs; stateful Sessions require a fresh continuity
epoch, exact first sample, and strictly contiguous blocks, poison the epoch
after execution failure, and reset history only on a new epoch. Realtime
Playback binds one explicit entry to each render generation. Each nested
instance owns a private epoch stream: a nondecreasing parent time map enters at
the first exact child sample and replays all skipped child samples in order.
Stateless child outputs remain arbitrarily indexable. Generic stateful reverse
mapping fails closed until a processor-specific reverse contract, checkpoint
replay, or materialized child output can prove block-partition-invariant
results.

The selected Program Output's total algorithmic latency is exposed only as
bounded `public_output_lookahead_frames`. A fresh epoch keeps separate public
and internal execution cursors. Its first non-empty request evaluates and
discards that lookahead in Render-Contract-sized blocks, then returns the exact
requested Timeline interval; subsequent execution stays the fixed lookahead
ahead. Meter time remains public time. Preparation fails before Session creation
when either lookahead frames or aggregate interleaved compensation storage
exceeds its explicit Render Contract budget, and Session construction rechecks
the admitted delay-line sample total.

Contribution execution is causal rather than equivalent to source activity.
Preparation extends the half-open source interval by checked source/rack
algorithmic latency and declared Scope-Rack tail; an infinite tail remains
consumer-bounded. Source silence after the Clip is explicit and does not call
the media Adapter. Stateful Contribution occurrences remain pending at root
entry, enter lazily at the first exact intersecting sample, and then receive
strictly contiguous callbacks through that causal interval. Track, Bus, and
Output occurrences enter immediately because their strips execute every
requested block. This prevents both truncated Clip tails and fake pre-Clip
history. Edit envelopes remain after Scope processing, so their authored fade
law may gate the Scope tail without changing processor lifetime semantics.

## Consequences

An ordinary native parameter replacement must remove the parameter component
actually charged to its current Session. Initial Session admission retains
the conservative Plan model; a successful ordinary replacement publishes the
complete new scratch layouts and retains that charged value beside the Plan
and scratch owners. The private local projection adjusts parameter and fixed
weights by the same checked delta. StateEntry uses this projection for old
storage and the initial model for its newly allocated destination. This local
fact belongs to the existing resource grant; it creates no new budget issuer
or claim of complete physical ownership.

Before native Bind, ordinary preparation computes every retained Runtime's
next subtree footprint and requires the Root result to equal existing overlap
admission. Final Source publication moves prevalidated Plan/scratch/charge
owners and publishes those cached subtree values together. All fallible
arithmetic remains before the final guards. Caller-owned retirement and exact
native Ack keep their existing lifecycle.

The original Root also retains a private, lazily allocated subtotal H for
logical Plan roles owned by actual pending and retired handoffs. Its published
steady footprint C and frozen grant G remain distinct: preparation checks
C+H+prospective N against G before ordinary local cloning. A receipt on the
actual owner enrolls N after all preparation gates; successful installation
checks C'=C-O+N and H'=H-N+O before Source guards, then applies one bounded atomic
delta with the ownership swap. The receipt is the last owner field and credits
only after preceding Plan/scratch owners finish destruction. Ack cannot retire
an externally retained Preserved handoff. The actual Root-owned StateEntry
cohort carries its own receipt through partial errors and retries, and exact
Ack releases its storage on the existing lifecycle path.

This is a logical-role policy, not an allocator union or a second state/grant
authority. Only exclusive Root operations can increase H; concurrent receipt
Drop can decrease it, so role changes use atomic RMW rather than stale stores.
Owned Preserved and private receipt controls have actual finite qualification.
Two repeated Windows owned VST3 editor scenarios also qualify real StateEntry
cohort fees through partial preparation, cancellation/retry, metadata Drop,
installation and exact Ack, plus pending nested Runtime destruction. They use
actual unequal new/old weights and count the unchanged root Session role.
Initial StateEntry preparation now borrows the complete retained tree to bound
new local roles R before the first clone or Plan retain. Every affected local
uses the existing requested logical policy, including equal/nonselected peers;
unaffected locals contribute their local Plan weight once. Checked C+R goes
through the original Root checker, which adds H once. Actual N must fit R before
source inspection. Persisted-cohort retries keep their existing C+H validation
without another R or enrollment. Repeated real Windows VST3 controls qualify
root/nested tight logical rejection with zero measured host-thread allocations
and root exact-grant partial retry/install/Ack/PCM. The finite native family has
H0 initially and no sidechain, transitions or Notes; conservative R can exceed N
in other shapes. This does not establish physical, fixed-budget or SDK bounds.
Opaque native destructor ordering, installed Runtime-first destruction,
same-Source Superseded CAS, native Ack refusal, additional native budget shapes,
accounting-metadata/fixed/physical completeness and real-time performance still
require separate acceptance.

- `mondrian-timeline` owns author data and validation only.
- `mondrian-audio` owns compiler, immutable plan, common DSP, recursive runtime,
  and source interfaces; it has no FFmpeg, CPAL, UI, or export dependency.
- `mondrian-media` owns decode/cache and physical output implementations, not
  Timeline routing semantics.
- `mondrian-playback` remains the sole Transport, Clock Master, continuity
  epoch, watermark, and recovery authority.
- `mondrian-app` and `mondrian-export` provide media Adapters and consume the
  same Runtime; neither contains a private Timeline mixer.
- Semantic Projection outputs, real VST3/CLAP hosts, additional Adapter layout
  lowering, processor sidechains, true-peak/loudness observation, and further
  stateful/lookahead processors must deepen this pipeline. They must not create
  alternate author or execution paths.

## Retained compiled Note payload logical accounting

The immutable Program's selected Note arrays and port-key strings contribute
one actual retained payload A to each prepared Plan role. If any lane has a
note, A is the checked outer CompiledNoteLane capacity layout plus every retained
row's AudioNote capacity layout and raw key-byte capacity. Initialized empty
rows are included in that case. An all-empty execution score contributes A0;
retained empty caller metadata is explicitly outside this logical policy and
must not be described as absent physical storage.

The common local weight is B0 + actual prepared Note payload S + A. Existing
Session projections, original Root C/H/G ceilings, overlap O/N roles and retained
fees consume that same weight. Multiple Plan roles sharing one Program Arc can
conservatively charge A per role; this is not exact physical union accounting
and needs no separate Note ledger or identity registry.

Native-author requested preparation reads next_program A after its original
bitwise score/topology eligibility and original B0+cached-S checked sum. It does
not replace a larger next owner with old-program capacity. Its generic schedule
clone is checked immediately afterward: actual cloned S must fit the old actual
S upper before processor rebind/foreign interaction. This is post-allocation
logical acceptance; detail-string refusals can allocate.

This accounting does not grant public nonempty Note execution. Compiler
clone/sort allocation, simultaneous lowering arrays, initial original-ledger
reservation, Hosted/factory peaks, coherent offers/IPC/lifetime and complete
physical/native/RT qualification remain separate prerequisites. Public unsupported
and endpoint errors keep their established order. No live Plan migration or
grandfathered fee policy is introduced.

## Processor output routing contract (2026-10-16)

The native lane's fixed owners also include `AdapterResident`: the complete
concrete retained Worker Box header, including inline adapter metadata. Its real
type layout enters the existing Factory session reservation and sole Root
`FixedResident` grant; the child compares its pure type profile before native
preparation and checks the actual dynamic Box layout before Ready. IPC13 requires
all eight owner layouts and refuses the old profile. This does not account for
nested cold heap owners or qualify whole-process residency. Resolver validation
and discovery cold peaks precede the final Session grant; non-Windows adapters
without a native lane profile do not acquire this qualification.

Approved for implementation; this section does not claim completed native
multi-output support. The current single mutable Main bus and strip taps cannot
represent independent plugin outputs. Relaxing a format's output-count check
would give it incorrect buffers and erase signal identity. Extend the existing
Main/auxiliary model and sole Route/DAG instead of replacing either executor.

Factories declare ordered, stable-keyed auxiliary output contracts, validated
against the Render Contract and revalidated for seeds and instance creation.
An empty auxiliary contract preserves the existing single-Main behavior.
Callbacks borrow Session-owned mutable auxiliary PCM beside the existing Main.
Every admitted native output receives real storage, connected or otherwise;
unconnected outputs never implicitly sum into Main.

An authored Processor-output Route names the unique Sequence-local processor
ID and either Main or an auxiliary bus key. Preparation derives its existing
occurrence; it does not persist another owner/instance registry. The first
slice admits Track/Bus racks only. Scope-generated and Output-rack sources are
rejected explicitly. Bypassed routed producers retain author intent but fail
execution preparation; they cannot fabricate auxiliary output. A raw tap is
captured immediately after that processor returns, before subsequent Rack
processing, before the owner mute stage. Audition Solo filtering applies to
Track-owned processor sources just as for pre-mute Track sends; Bus owners
follow the existing Bus rules. Processor execution adds no separate mute/Solo
gate.

Reuse the existing pre/post stage DAG, including its conservative rejection of
same-stage feedback. Routes and sidechains resolve to dense prepared source
slots. A processor tap's arrival is its aligned input delay plus its own
algorithmic latency, not the remaining Rack latency. Fanout executes the
producer once per window. All auxiliary buffers and referenced Main snapshots
survive until their final consumers and are preallocated/charged through the
existing storage visitors, FixedResident and Root grant; no second budget is
introduced. Failed processing must not publish partially updated or stale PCM.
Replacement overlap uses a conservative allocation union: metadata already in
the current fixed footprint can be charged again while its owner is retained.
This is an upper bound, not qualification of a minimum peak grant or physical heap.

The additive author endpoint requires document schema33 and a value-preserving
32-to-33 migration that rejects new endpoints mislabeled as legacy. Archive and
library formats remain unchanged. Clone/remap, strong references, node removal,
cycle checks and authoring footprint migrate with the endpoint. Shared-DAG
literal PCM, intermediate taps, PDC, fanout, partitioning, budget boundaries and
project roundtrips qualify the first slice; independent native fixtures and
real plugins qualify subsequent format adapters. Existing VST3/CLAP guards
remain until their complete native buffers, IPC and contracts are implemented.

### Retained restart and native I/O migration boundary

The current retained restart transaction preserves every port contract and
shared mapping; it can update admitted scheduling facts, not native I/O geometry.
The real CLAP mode19 Runtime gate confirms that a legal inactive Mono-to-Stereo
detector change is refused before activation, while old publication remains held
and original-child retirement exits normally. This is an open capability gap.

Future migration must reuse the existing joined native processing scope and
Root transaction. Inactive native metadata supplies a typed proposal using the
existing ordered port contracts. Before allocating replacement planes or a
shared mapping, admit their peak together with all retained owners under the
original grant. Rebuild routes and PDC from the unchanged author Program; an
unresolvable referenced port must refuse preparation. State/configuration and
mapping installation must identify the same observed native candidate and
Source permission, retain old owners through acknowledgement, and require Fresh
Enter. Do not relax transfer port equality or resume the old mapping in isolation.
IPC22 currently provides none of this topology migration.
