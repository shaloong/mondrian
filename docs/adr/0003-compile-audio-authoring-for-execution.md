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

The Render Contract carries one canonical `AudioChannelLayout`, never a bare
channel count. The value is Mono, a validated named-speaker set in canonical
interleaving order, or a bounded Discrete bus with deliberately absent speaker
meaning. Standard 5.1(side), 5.1(back), and 7.1 are distinct values even where
their extents match another layout. Representability does not grant execution:
media, plugin, device, and export Adapters must negotiate an exact layout or
fail closed, and may not select a matrix from channel count alone.

`AudioProgramRuntime` is the shared high-level Implementation used by playback
and export. It recursively instantiates nested public outputs and binds media
through the narrow `AudioMediaResolver`/`AudioDecodedSource` Adapter Seam.
FFmpeg paths, asset databases, CPAL devices, export files, and UI state do not
enter the compiler or DSP core.

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
