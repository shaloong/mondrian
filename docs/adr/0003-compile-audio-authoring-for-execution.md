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

## Mutable state and future processors

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

The current executable built-ins are canonical Gain and stateful Sample Delay.
Sample Delay owns an exact non-negative integer-sample parameter, bounded
Session storage, explicit fresh-epoch reset, and block-partition-invariant
history; it reports zero compensable latency so PDC cannot erase its audible
effect, and a finite tail equal to that delay so a Scope occurrence can flush
after source silence. A non-zero-latency test Adapter proves generic Host instantiation, PDC,
mode admission, entry failure, and partition invariance, but is not a product
processor claim. The first production lookahead or group-delay Processor must
also freeze public-output lookahead/latency normalization so Playback, Export,
and nested pull evaluation return the same Timeline-aligned samples rather than
leading silence or a truncated tail.

Preparation already solves checked Contribution and port-specific Route
compensation at every sum and propagates child-output latency bottom-up; a
missing child preparation dependency fails closed. Prepared Contribution and
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
