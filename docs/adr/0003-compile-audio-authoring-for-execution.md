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
   rate, channel count, maximum block size, and realtime/offline mode.
3. `AudioRenderSession` owns exclusive mutable buffers and processor state for
   one run. It renders exact integer-sample windows into caller-owned storage.

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

Route closure is selected from the requested stable Program Output. Canonical
compilation contains no consumer-purpose flag. Solo is an explicit audition
overlay and produces a non-canonical closure. Scheduler windows, watermarks,
deadlines, and transport state never enter author semantics or plan identity.

Nested Sequences are sources selected by stable child Program Output ID. The
runtime may share immutable child plans, but each nested instance receives an
independent mutable Session and cache. Missing children and cycles fail before
rendering. Nesting beyond the shared 16-level Sequence render contract also
fails. The parent never reaches into child Tracks, Buses, Roles, or Routes.

Unresolved or unsupported processors fail compilation while their complete
author data remains intact. Bypass is explicit authored intent. There is no
implicit “plugin missing, therefore dry” rule. Realtime render failure is
reported to Audio Playback, which may preserve scheduled media position with
same-duration silence; that scheduling protection is not a second DSP
interpretation. Offline export fails the job and reports the reason.

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

Every processor must declare latency and supported layouts/automation cadence.
Parallel paths are delay-compensated before sums and Transitions. A latency or
layout capability change causes recompile and controlled re-entry; it cannot
mutate the live graph inside a block. The current executable processor set is
zero-latency built-in Gain, so preparation admits no unimplemented non-zero
latency path.

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
- Semantic Projection outputs, real VST3/CLAP hosts, PDC, richer channel
  layouts, sends/sidechains, meters, loudness, and state entry must deepen this
  pipeline. They must not create alternate author or execution paths.
