# Audio pipeline

Mondrian has one Sequence-owned author model and one author-to-PCM execution
pipeline. Playback, export, audition, analysis, and nesting may use different
schedulers and downstream consumers, but they cannot reinterpret placement,
processor order, automation, routing, transitions, or gain mathematics.

The governing decisions are [ADR-0002](../adr/0002-sequence-owned-audio-authoring.md),
[ADR-0003](../adr/0003-compile-audio-authoring-for-execution.md), and
[ADR-0004](../adr/0004-use-exact-author-time-across-media.md).

## Ownership and dependency direction

```text
mondrian-core
  exact TimelineTime / sample conversion / stable IDs / exact automation
        |
mondrian-timeline
  Track -> Clip placement + Sequence-owned audio author data and validation
        |
mondrian-audio
  compile -> prepare -> exclusive Session -> common float PCM
        |
  +-----+------------------+------------------+
  |                        |                  |
app playback Adapter   export Adapter   future analysis/audition Adapter
  |                        |
media decode/cache     media decode/cache + file encoder
```

`mondrian-audio` deliberately does not depend on FFmpeg, CPAL, asset databases,
UI, or export containers. Consumers implement `AudioMediaResolver` and return
`AudioDecodedSource` objects on the requested Render Contract. A source fills
fallible exact interleaved blocks; the Runtime never calls a decoder once per
sample and never requires whole-file PCM ownership. The compiler is
therefore testable without hardware or media files, and media/platform code
cannot acquire authority over Timeline routing.

## Persistent author model

### Placement is single-source

An audio Track owns Clips. A Clip owns placement, duration, source in/out,
exact speed mapping, nested Sequence identity, disabled state, and linkage to
other Clips. Those are the only persistent placement facts.

Each audio Clip owns one or more `AudioComponentEdit` values. An edit contains:

- a stable `AudioComponentEditId`;
- a media `AudioSourceComponentId` or nested `ProgramOutputId`;
- optional Sequence-local `AudioRoleId`;
- enabled state;
- exact edit-local origin (`local_time_in`);
- static and exact-automation volume/pan;
- independent fade-in/fade-out definitions;
- a restricted `AudioProcessingBinding { scope_id, scope_in }`.

It cannot contain a Track ID, Sequence range, Clip speed, source range, nested
Sequence ID, Route, or output. Compilation derives those from the owning
Track/Clip. This prevents two editable copies of placement from diverging.

The current media Adapter binds only the stable
`AudioSourceComponentId::primary()` logical component. Any other component ID
fails binding instead of being silently redirected to the primary stream. A
future asset stream catalog must make additional stable component identities
resolvable before authoring them.

### Processing scopes

`AudioProcessingScope` is a Sequence-owned processing definition containing
input gain/automation and one ordered `AudioProcessorRack`. It has no placement
or routing fields. Sharing a Scope means sharing one stateful processing
history. The restricted binding permits only identity plus an exact time
offset; arbitrary affine mappings would recreate a hidden placement model.

Razor splitting creates fresh Clip and Component Edit IDs for the right side,
retains the Scope ID, and advances edit-local and Scope-local origins by the
exact split offset. Trimming an in edge shifts those origins; moving or slipping
the Clip does not. Copy/paste and precomposition fork Scope, Processor,
Keyframe, and Edit IDs because they create an independent aggregate instance.
Duplicating a Sequence likewise forks every Sequence-local audio identity and
automation keyframe. References to a child Sequence's public output remain
unchanged because they cross the duplicated aggregate boundary.

### Mixer and routing

The Sequence `AudioProgram` owns:

- one `AudioTrackChannel` per audio Track, keyed by the Track ID;
- zero or more `AudioMixBus` values;
- one or more stable `AudioProgramOutput` values;
- explicit `AudioRoute` values;
- all `AudioProcessingScope` values;
- explicit `AudioTransition` values.

Track Channels, Buses, and Outputs share the `AudioChannelStrip` value but not
one universal node identity. A strip has input trim, ordered pre-fader rack,
fader/automation, and ordered post-fader rack. Track mute is applied only after
the post-fader rack.

Routes use stable typed endpoints. Source ports are `PreFader`,
`PostFaderPreMute`, or `PostMute`; destinations are Bus or Program Output.
Routes never name array indexes, display names, generated Clip stages, or child
internals. Instantaneous cycles are rejected. Feedback will require an explicit
delay/state operator with defined initialization and latency.

Disabled Clip or disabled Component Edit is absent from compilation. Persistent
Track mute gates `PostMute` while preserving pre-mute taps. Solo is not stored
on Track: an `AudioAuditionOverlay` selects a temporary closure without
rewriting or exporting the canonical Program.

### Processors and plugins

Every insertion point uses `AudioProcessorInstance`, regardless of whether the
definition is built-in, VST3, CLAP, or a future host format. Author data keeps:

- stable instance and definition identity;
- schema version and stable Parameter IDs;
- explicit bypass;
- exact automation curves;
- opaque versioned external-plugin state.

An unavailable dependency remains serializable and editable but is not
executable. Compilation fails closed unless the instance is explicitly
bypassed. Plugin path, scan order, parameter array index, and display name are
never identity.

The current common executor admits built-in Gain schema 1. VST3/CLAP loading,
process isolation, layouts, state entry, latency, and parameter-delivery
capabilities remain required work; no dry or flat fallback claims support.

### Fades and transitions

A Clip fade is a unary Component Edit envelope. A Transition is a separate
strong author relationship between exactly two Component Edit IDs plus an exact
Sequence-time range and paired curve law. It is evaluated after each endpoint's
Scope/edit processing and before Track summing. Other material in the same
overlap is unchanged. Overlap without a Transition is normal summing.

Constant-gain and equal-power curves are currently executable. Copying both
endpoints recreates a Transition with new IDs and translated Sequence time;
copying one does not. Structural edits discard a Transition when either
endpoint or its complete interval is no longer valid rather than silently
retargeting it.

## Compilation and execution

### Stage 1: semantic compilation

`compile_audio_program(Sequence, AudioCompileRequest)` first validates the full
author closure, resolves one stable Program Output, computes the required Route
closure, and derives immutable generated contributions. Every contribution
records deterministic origins:

- Component Edit ID and Clip ID;
- owning Track ID and derived Sequence range;
- exact source time map from Clip placement/speed/source-in;
- media component or nested public output identity;
- Processing Scope and exact edit/scope origins;
- edit automation, fades, and relevant Transitions.

Only generated IR uses the term “Compiled Audio Contribution”. It is never
persisted and users cannot route to it. Semantic Projection outputs currently
fail compilation explicitly; only `RoutedInputs` is executable.

### Stage 2: preparation

`PreparedAudioPlan` binds immutable semantic IR to an `AudioRenderContract`:

- positive sample rate;
- positive interleaved channel count;
- maximum admitted block frames;
- `Realtime` or `Offline` processing mode.

Preparation is where layout negotiation, processor realization, latency
analysis, and scratch sizing belong. The present executable processor set is
zero-latency, so the runtime does not claim general plugin delay compensation.
Future non-zero-latency admission must build an explicit compensation plan
before a Session begins.

### Stage 3: exclusive mutable Session

`AudioRenderSession` allocates Track, Bus, Output, and route scratch buffers
before rendering. `render_into` accepts an exact signed start sample and exact
frame count and writes caller-owned interleaved float storage. The Session is
exclusive mutable state; plans may be shared, Sessions may not.

The normative signal order is:

```text
decoded component or nested public output
-> Scope input gain / ordered Scope processors
-> edit volume / pan / fades / paired Transition envelope
-> Track input sum
-> Track strip: input, pre-fader rack, fader, post-fader rack, mute
-> typed Routes
-> topologically ordered Bus strips
-> Program Output strip
-> consumer boundary
```

All internal PCM is floating point. Values outside `[-1, 1]` are legal.
Nothing normalizes, clips, applies `tanh`, or inserts a limiter. A limiter,
loudness target, dither, channel packager, or monitor calibration must be an
explicit processor or downstream contract.

Exact automation is evaluated in its owner domain and is invariant under block
partition. Timeline-to-sample conversion uses `AudioSamplePosition` with an
explicit rounding policy. Export range boundaries are converted once from the
Sequence frame grid; chunks then advance integer samples only.

### Shared recursive Runtime

`AudioProgramRuntime` combines compiler, prepared plan, Session, media binding,
and nested public outputs. Nested children are recursively compiled using the
selected `ProgramOutputId`; cycles, missing children, and depth beyond the
shared 16-level Sequence render contract fail before playback or export begins.
Each nested instance owns an independent child Session and a
bounded PCM window cache. The parent cannot inspect child Tracks, Buses, Roles,
or private Routes.

Media sources use the same block principle. Each bound source has one aligned
4,096-frame Runtime hot window, so ordinary forward, reverse, and speed-mapped
sample access crosses the trait boundary once per block rather than once per
sample. A source failure rejects the entire requested block; Playback may then
substitute exact-duration silence at its downstream scheduling boundary, while
Export fails the job. Partial decoded data can never shift later media time.

The current playback and export media Adapter is a shared
`AudioSourceCache`. It opens a stable source identity from path, byte length,
and modification timestamp, decodes only aligned ten-second PCM windows, and
retains them in one cross-source weighted LRU capped at 128 entries and 256 MiB.
Terminal decode failure memory is separately capped at 64 identities. A new
file fingerprint cannot reuse old PCM. Negative and post-EOF coordinates are
silence; arbitrary seeks request the containing window. Resident bytes, hits,
misses, decodes, failures, oversize windows, and evictions are structured
diagnostics. The current concrete decoder is an accurate-seek FFmpeg CLI
Adapter; replacing it with a persistent in-process Session must not alter the
block Interface, sample coordinates, or cache policy. It runs only on the audio
render worker, never on the callback or UI thread.

Playback and export both use this Runtime. Their only differences are Render
Contract mode, scheduling, error handling, and downstream sink:

| Consumer | Scheduler/contract | Execution failure |
|---|---|---|
| Playback | deadline-bound integer windows, `Realtime` | reported to Audio Playback; same-duration silence may protect scheduled position |
| Export | sequential integer windows, `Offline` | export job fails with the exact compile/bind/execute reason |
| Nested output | parent-driven source windows, inherited contract | propagates failure to the owning root execution |

## Playback boundary

Audio Playback, not the audio compiler, owns device lifecycle, render
generations, bounded in-flight work, watermarks, preroll, callback consumption,
underrun recovery, and device-clock evidence. The CPAL callback touches only a
fixed-capacity queue and atomics; it does not allocate, decode, compile, log,
inspect the Timeline, or lock a Session.

Normal playback may qualify consumed device samples as Audio Device Clock
Master. Device loss or rejected phase handoff returns authority to Synthetic
Clock Master; video presentation is never Clock Master. See
[Playback Engine](playback-engine.md) for the full state and evidence contract.

## Correctness invariants

- A valid snapshot has a closed Track/Channel/Scope/Route/Transition graph.
- Every audio Clip has at least one Component Edit; every edit's source kind
  matches its owning Clip kind.
- Track Channel keys exactly equal audio Track IDs.
- Placement and source mapping come only from Track/Clip.
- A Transition's full range lies inside both endpoint Clips.
- Route cycles, duplicate strong IDs, unknown Roles, and missing ports fail.
- Unresolved processors fail closed and preserve author data.
- Block partition cannot change PCM results.
- Internal summing has no implicit nonlinear operation.
- Playback and export compile and execute the same Program semantics.
- Nested instances do not share mutable Session state.
- Playback and Export media sources are bounded by bytes and source revision;
  neither may retain whole-file PCM as its execution Interface.

## Verification already present

Automated tests currently prove:

- compiler placement derives from the real Track/Clip hierarchy;
- invalid unscoped audio authoring and Bus cycles reject validation;
- Clip/Track/Bus/Output gain and automation are block-invariant;
- internal PCM is not clipped or `tanh`-shaped;
- Track mute gates the post-mute route;
- unresolved VST3/CLAP instances fail closed;
- timeline/audio crates compile and test independently;
- app playback and export compile against the shared Runtime.
- decoded-media block reads cross aligned windows exactly, reuse hits, evict by
  global PCM bytes, and invalidate after file replacement;
- a manual external AAC parity gate compares arbitrary bounded windows against
  a sequential reference decode at start, middle, and tail positions.

## Required depth before professional audio completion

The architecture is now on the product path, but these are explicit remaining
gates rather than implied support:

1. Define channel-layout and sample-format types, deterministic resampling,
   media component/stream selection, and up/down-mix policy; reject unknown
   layouts instead of guessing.
2. Add processor capability negotiation, real built-in stateful processors,
   latency propagation/PDC, seek entry/preroll, discontinuity tests, and nested
   latency evidence.
3. Implement isolated VST3 and CLAP host Adapters with scanning, stable native
   IDs, state chunks, bus/layout negotiation, sample-accurate parameter events,
   crash/hang containment, and realtime/offline capability reporting.
4. Add typed sends and sidechains without weakening Route types; add meters and
   loudness analysis as explicit downstream/processor stages.
5. Execute Semantic Projection outputs and delivery mappings without cloning
   or reinterpreting the canonical Program graph.
6. Add editor commands/UI for Clip/Track/Bus/Output racks, automation, fades,
   Transitions, routing, audition overlays, missing-plugin repair, and atomic
   undo/redo.
7. Add reference PCM fixtures for fades/Transitions/nesting/PDC, long 29.97 and
   59.94 projects, block-size matrices, seek/discontinuity, plugin failure,
   export parity, and reference-machine realtime load/drift gates.
8. Replace the per-window FFmpeg CLI decoder Adapter with persistent,
   cooperatively cancelable decoder Sessions plus bounded look-ahead. Prove
   worst-case window-miss latency under long-GOP/compressed audio and keep
   source cache bytes inside the same long-run evidence report.

No item may be closed by adding only schema, an effect enum, a disconnected UI,
or a consumer-specific fallback mixer.
