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

`mondrian-assets` persists one `AssetAudioComponentCatalog` per Asset. Initial
import assigns `AudioSourceComponentId::primary()` to the stream carrying the
container default disposition, or the first stream only when no default is
declared; every other stream receives its own persisted ID. A catalog binding
contains absolute stream index, optional container stream ID, native layout,
language, and the exact file fingerprint whose probe produced those facts.
Playback and waveform resolution compare that fingerprint with the live file;
Export freezes only Component selections reachable from its immutable Sequence
closure. Unknown IDs, changed files, missing streams, and signature drift fail
before PCM decode.

Reprobe/relink preserves existing logical IDs and their signatures. A new
unclaimed stream index receives a new ID, while a changed stream at a claimed
index remains unresolved until an explicit future rebind operation; neither
language nor current default disposition may silently retarget authored edits.

The Sequence persists one validated semantic `AudioChannelLayout`; it does not
persist a second channel count. The value is either independent Mono, a
canonical non-empty set of named speaker positions, or a bounded 1–64 channel
Discrete bus whose speaker meaning is deliberately absent. Standard Stereo,
5.1(side), 5.1(back), and 7.1 are canonical speaker-set values, so discovery
order cannot change plan/cache identity and the integer six can never alias the
two 5.1 meanings. The same layout crosses preparation, nested Runtime,
Playback PCM requests, decoded-source cache, PCM buffers, and Export. DSP
capacity and canonical interleaving are derived from this value; custom speaker
and Discrete layouts already pass through the common float Runtime without
being relabelled.

Representability is not execution admission. The media Adapter selects the
absolute physical stream with `-map 0:<index>`,
requests the output sample rate, and installs one explicit FFmpeg `pan` matrix
before PCM enters the Runtime. All mono/stereo/5.1(side) input/output pairs are
defined. Stereo fold-down averages L/R; 5.1 fold-down uses -3 dB center and
side coefficients and deliberately omits LFE. Mono feeds stereo L/R or 5.1
center; stereo feeds 5.1 L/R. Unlabelled one- and two-channel sources retain an
`Unspecified` probe fact but use the same explicit discrete defaults required
for ordinary PCM WAVE compatibility. Probe can faithfully project 5.1(back),
7.1, and bounded unspecified layouts into named or Discrete signal values, but
current FFmpeg execution still rejects unspecified 3+, 5.1(back), 7.1, custom
speaker, and Discrete outputs because no approved matrix exists. Product
authoring of custom layouts, user-authored mix matrices, plugin Bus negotiation,
and device/output packaging remain incomplete; FFmpeg defaults are never that
policy.

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
- one versioned `ParameterSchema` snapshot per stable Parameter ID;
- explicit bypass;
- one exact curve per parameter, whose default is the unkeyed value;
- opaque versioned external-plugin state.

An unavailable dependency remains serializable and editable but is not
executable. Compilation fails closed unless the instance is explicitly
bypassed. Plugin path, scan order, parameter array index, and display name are
never identity. Parameter map key, schema identity, and curve identity must be
identical. Project validation rejects non-finite values, values and Bezier
control points outside the hard range, disallowed interpolation mathematics,
and keyframes on non-animatable parameters before an immutable snapshot exists.

The current common executor admits built-in Gain schema 1 only when its complete
captured parameter schema equals the canonical definition. The required Gain
parameter is never synthesized during compilation. Its hard interval is
`[-120, +24] dB`, its ordinary editor interval is `[-60, +12] dB`, and invalid
persisted values are rejected. VST3/CLAP loading, process isolation, plugin-bus
layout negotiation, state entry, latency, and parameter-delivery capabilities
remain required work; no dry or flat fallback claims support.

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
- one supported semantic channel layout with count and order derived from it;
- maximum admitted block frames;
- `Realtime` or `Offline` processing mode.

Preparation now lowers the semantic graph into one dense execution schedule:

- stable topological slots for Track Channels, Buses, and the selected Output;
- destination-contiguous incoming Route ranges and Track-contiguous
  Contribution ranges;
- dense Processing Scope slots and Contribution-local Transition bindings;
- half-open active sample spans lowered once for the Render Contract;
- liveness-assigned scratch slots whose lifetime extends through the last
  downstream consumer;
- a checked latency solution for every Contribution-to-Track and Route-to-node
  sum, including the exact `PreFader` versus post-fader source port;
- constant gain/pan and rack-gain fast paths;
- one explicitly selected scalar-reference or runtime-vectorized CPU kernel.

The prepared schedule contains no authoring maps and the Session performs no
Route search. Non-constant automation is validated once and lowered to ordered
sample event spans; the Session advances span cursors instead of searching or
validating author curves per sample. Latency is accumulated through each
pre-/post-fader rack and resolved independently at every real summing point;
the selected Program Output exposes the resulting total latency. Recursive
preparation is bottom-up: a parent Contribution receives the already-prepared,
instance-specific child-output latency. Missing nested latency fails closed and
can never be guessed as zero. Layout negotiation, processor realization, plugin
parameter-event batching, compensation-delay execution, and state entry also
belong here. The present executable processor set is zero-latency, so the
runtime does not yet claim general plugin delay compensation. Any admitted
non-zero-latency processor must arrive with its processor execution state and
discontinuity contract as one change.

### Stage 3: exclusive mutable Session

`AudioRenderSession` allocates the schedule's liveness-sized scratch bank before
rendering; it does not allocate one permanent buffer per author node or Route.
`render_into` accepts an exact signed start sample and exact frame count and
writes caller-owned interleaved float storage. The Session is exclusive mutable
state; plans may be shared, Sessions may not.

The immutable Plan and recursive Runtime both report selected-output latency.
This is execution scheduling information, not an instruction to rewrite author
time, Clip placement, automation coordinates, or nested source mappings.

Session construction allocates one fixed interleaved delay line for every
prepared Contribution and Route compensation input; zero-delay lines retain no
sample storage. The capacity report exposes the non-zero line count and exact
retained sample count. Contribution compensation executes after contribution-
local processing and before the Track sum; Route compensation executes after
the selected source port and before the destination sum. Neither path allocates
or grows during a block. Scalar/SIMD execution shares these same state lines,
and reference tests require whole-block and partitioned-block PCM identity.
These compensation lines are real mutable history, so arbitrary discontinuous
requests are not admitted once a plan contains one. Preparation marks the whole
closure `requires_state_entry` when any local processor, compensation input, or
prepared child output owns history. A stateful Session starts unentered. Its
coordinator must supply a fresh `AudioContinuityEpoch` and exact first sample;
the Session then accepts only exact contiguous blocks. Reusing an epoch or
submitting a gap is rejected without guessing. Any execution failure poisons
the epoch because an unknown prefix may have advanced history; only a fresh
entry resets all compensation state. Stateless Gain-only plans retain random
block evaluation and do not manufacture continuity obligations.

Offline export enters one fresh epoch at its exact sample-range start. Realtime
Playback carries its generation on every PCM work request: the first admitted
window is `Enter`, later windows are `Continue`, and the Timeline Adapter rejects
duplicate/missing entry, generation mismatch, or a non-contiguous sample. The
Adapter maps Playback generation to Runtime continuity rather than deriving a
reset from coordinates. The Adapter declares `GenerationState` whenever its
root Runtime requires entry. Any execution failure or PCM-contract violation
then invalidates the whole Playback generation: queued output and pending work
are discarded, the application submits the final device observation so Clock
Master falls back to Synthetic, and a fresh generation enters at Playback's
authoritative position. Exact-duration silence substitution remains legal only
for `IndependentWindows` renderers. Consecutive poisoned generations consume a
bounded recovery budget; exhaustion enters `RenderBlocked` and schedules no
more PCM until an explicit reprime, source rebind, or device-open boundary.
Completing fresh preroll clears the streak. Root stateful plans are therefore
admitted without permitting an unbounded retry loop. Stateful nested instances
now own independent private epoch streams. A nondecreasing parent time map
enters a child lazily at its first exact demanded sample and evaluates skipped
child samples in order before publishing later samples; cache misses therefore
cannot skip processor history. A root discontinuity invalidates child windows
and causes a fresh lazy child entry. Generic stateful reverse mapping fails
construction, with an execution-time defense, until checkpoint replay,
materialization, or processor-specific reverse state can prove partition-
invariant output. Stateless nested output remains freely indexable in either
direction. Propagating the child's requirement upward never makes root and
child share a coordinate or state domain.

The source Seam is block-shaped even when a Clip speed map produces reverse,
repeated, or non-contiguous coordinates. The Session resolves one absolute
source-frame index per output frame into preallocated storage and crosses
`AudioPcmSource::read_indexed_interleaved` once per active Contribution block,
not once per sample or channel. The Adapter must preserve the supplied order and
duplicates, treat negative coordinates as silence, and either fill the complete
pre-zeroed block or fail it. This keeps exact time mapping outside media decode
while permitting a decoder, nested Runtime, or future resampler to optimize
contiguous runs internally.

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
explicit rounding policy. Static source-time spans map the first sample exactly
and advance an `i128` rational accumulator across the block, including
fractional forward and reverse rates; they do not reconstruct general Timeline
values per sample. Export range boundaries are converted once from the Sequence
frame grid; chunks then advance integer samples only.

### Realtime execution performance contract

The dense schedule, liveness scratch reuse, scalar reference kernels, runtime
SIMD dispatch, affine source accumulator, and non-constant automation event-span
preparation are implemented. Preparation validates each exact curve once,
lowers its event boundaries to the Evaluation Grid, and stores immutable core
segment evaluators; block execution advances a local span cursor and never
validates or searches the author curve per sample. Hold, Linear, and Bezier
semantics remain core-owned and are exactly block-partition invariant. Realtime
rendering does not scan author Routes or use tree/map lookup in sample loops.
Future plugin event batches, latency/PDC, and state-entry obligations must deepen
this schedule without reintroducing author graph interpretation.

`AudioRenderSession::new` establishes a typed fixed `AudioRenderCapacity` for
maximum frames, channels, liveness scratch slots, and samples per slot.
`render_into` performs no Session-owned allocation, buffer growth, author-map
search, or event-container construction; the media Adapter fills caller-owned
preallocated storage. The normative realtime path is CPU block DSP with a scalar reference kernel and
vectorized kernels selected during preparation. A render worker runs ahead into
the fixed-capacity playback queue; the device callback only consumes that queue
and atomics. After Session construction, the realtime execution path admits no
heap allocation, blocking lock, file or decoder I/O, processor construction,
graph mutation, logging, unbounded retry, or wait on UI/GPU completion. Stateful
processors execute in declared rack order. Independent routing nodes may run in
parallel only when the prepared dependency schedule and available block
headroom prove that scheduling overhead is beneficial.

GPU audio is an optional prepared execution backend, not a second author graph
and never a realtime requirement. It is admissible only for processors that
declare deterministic block/batch behavior, channel/layout support, state
ordering, minimum efficient batch, fixed buffering latency, cancellation, and
device-loss behavior. Preparation must include transfer/queue latency in PDC,
bound all in-flight buffers, and reject a contract whose deadline cannot cover
the required batch. Realtime execution may not submit a tiny block and
synchronously wait/read back on every callback. CPU SIMD remains the qualified
fallback; any live backend handoff requires an already prepared equivalent path
plus explicit continuity/state-entry policy. Offline rendering may choose larger
GPU batches. Native VST3/CLAP instances normally remain isolated CPU/plugin-host
Adapters unless the plugin itself exposes a separately qualified GPU execution
contract.

Performance acceptance is workload- and deadline-based rather than a claim from
an enum or benchmark of one Gain processor. The current `dense_schedule_v2`
matrix pairs 1/8/32/64 Tracks with 0/2/8/16 Buses, explicit Track→Bus→Output
Routes, 64/256/1024-frame blocks, and scalar/SIMD PCM parity; a non-ignored
three-Bus parity test keeps the routed topology in ordinary CI. Fixed-reference-
machine matrices must continue to cover sample rates/layouts, active Clips,
Transitions, automation density, nested Sequences, stateful built-ins, plugin
instances, seek/re-entry, and decoder pressure. Reports include render-worker
p50/p95/p99/max duration and deadline headroom, callback underruns, queue depth,
allocations after preparation, CPU time, memory plateau, PDC, cancellation
latency, and CPU/GPU/backend provenance. PCM parity and block-partition tests
remain mandatory for every optimized backend.

### Shared recursive Runtime

`AudioProgramRuntime` combines compiler, prepared plan, Session, media binding,
and nested public outputs. Nested children are recursively compiled using the
selected `ProgramOutputId`; cycles, missing children, and depth beyond the
shared 16-level Sequence render contract fail before playback or export begins.
Each nested instance owns an independent child Session and a
bounded PCM window cache plus a preallocated output-index ordering table. The
table groups arbitrary stateless access and makes stateful nondecreasing access
replay in child-time order without allocating during a block. The parent cannot
inspect child Tracks, Buses, Roles, or private Routes.

Media sources use the same block principle. Each bound source has one aligned
4,096-frame Runtime hot window, so ordinary forward, reverse, and speed-mapped
sample access crosses both the Runtime source Seam and decoded-media Seam in
blocks rather than once per sample. A source failure rejects the entire requested block; Playback may then
substitute exact-duration silence at its downstream scheduling boundary, while
Export fails the job. Partial decoded data can never shift later media time.

The current playback and export media Adapter is a shared
`AudioSourceCache`. It opens a stable source identity from path, byte length,
and modification timestamp, decodes only aligned ten-second PCM windows, and
retains them in one cross-source weighted LRU capped at 128 entries and 256 MiB.
Terminal decode failure memory is separately capped at 64 identities. A new
file fingerprint cannot reuse old PCM. Negative and post-EOF coordinates are
silence; arbitrary seeks request the containing window. Resident bytes, hits,
misses, decodes, failures, oversize windows, in-flight single-flight leaders,
and evictions are structured diagnostics. The concrete decoder uses an
eight-entry LRU pool of isolated persistent FFmpeg child Sessions: sequential
windows reuse continuous `f32le` output, while a non-contiguous miss restarts
only the matching fingerprint/output-contract Session with bounded coarse
preroll followed by output-side exact trim. Stdout look-ahead and
stderr retention are byte-bounded, and generation cancellation kills, waits,
and joins the process and pump threads. Linked in-process decoding remains a
replaceable Adapter choice rather than a different source contract. Decode
runs only on audio render workers, never on the callback or UI thread.

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

Each Audio Playback generation also owns a fresh monotonic
`ExecutionCancellationToken`. Reprime, seek, device recovery, source replacement,
and shutdown cancel the old token before incrementing generation. The token is
carried through `AudioPcmRenderer`, `AudioProgramRuntime`, nested Runtime calls,
`AudioDecodedSource`, and the media window Adapter. A canceled read returns
without admitting PCM or remembered failure into the shared cache. Export uses
the same Interface with its own live token; it does not inherit Playback state.

Normal playback may qualify consumed device samples as Audio Device Clock
Master. Device loss or rejected phase handoff returns authority to Synthetic
Clock Master; video presentation is never Clock Master. See
[Playback Engine](playback-engine.md) for the full state and evidence contract.

The ignored `playback_professional_cpal_av_gate` is the fail-closed physical-
output reference Adapter. It requires a proven primary-audio stream duration
long enough for the exact
30-minute 30000/1001 observation, qualifies a concrete 48 kHz stereo CPAL
generation, then drives the normal App event-loop seams at real wall cadence
while a generated picture completes through the real headless Viewer GPU
Adapter. Its versioned `cpal_av_48khz_30min_v1` policy requires at least 99.5%
current-video readiness, completed GPU presentation, Audio Device Clock Master
residency except at most five seconds of startup fallback, absolute delivery
clock drift at most 20 ms, callback-frame versus monotonic-duration divergence
at most 1,000 ppm with a 100 ms minimum allowance, no callback
underrun/recovery or render-to-silence substitution,
no source decode failure/oversize window, a bounded persistent Session pool
with observed sequential reuse, each steady sequential ten-second window within
the 460 ms output high-water duration, the 256 MiB global source-cache budget, and
the shared whole-process Private Commit plateau contract. Missing environment
fixture, output device, callback facts, native memory facts, or presentation
facts fail rather than skip.

The observation begins only after the same stream generation, fresh callback,
Active Audio Playback, and Audio Device Clock Master remain continuously
qualified for one second. A bounded 30-second tail may extend execution until
both the current uninterrupted callback interval and Playback Evidence cover
30 minutes; it never reduces either acceptance duration.

This gate proves a concrete OS output stream consumed the production PCM path;
it does not claim acoustic loopback or speaker-waveform verification. On
2026-07-18 it passed a complete local 30-minute run with one stable 48 kHz
stereo stream: 86,474,752 consumed frames, 168,926 callbacks, zero underrun,
render substitution, recovery, drift, or rejected terminal delivery, and
53,964/53,964 video Ready samples with 107,924 headless GPU presentations. The
report is local development evidence; no redistributable fixture or fixed-
reference-machine baseline is checked into the repository.

On 2026-07-18 the shorter development Adapter reached the same production path
on the local Windows machine with CPAL stream generation 1: 13,312 active
callback frames, 191 callbacks, zero underrun, retained Audio Device Clock
Master, 78 completed headless GPU executions, and zero maximum delivery-clock drift over
the sampled interval. Before persistent Session reuse, the same source's first
ten-second process-per-window miss took about 1.02 s. That historical cold
measurement justified this work but is not steady-state evidence. The
professional gate now reports cold, sequential, and random-restart maxima
separately and applies the 460 ms playback high-water rule only to observed
sequential reuse. The local complete run opened one Session, reused it for 180
sequential windows, and measured a 79,973 us steady maximum against the 460 ms
bound; wider fixed-machine/device coverage remains outstanding.

With a generated 24-second 48 kHz AAC development fixture, the product renderer
opened one Session, decoded three ten-second windows, reused it twice, and
reported a 953,057 us cold maximum versus a 142,261 us sequential maximum; PCM
residency was 9,216,000 bytes. The external parity gate matched sequential
reference PCM across a sequential boundary and an evicted-window random
restart, and the real child cancellation gate returned inside 50 ms. These are
local development facts, not the fixed-machine 30-minute acceptance report.

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
- Old-generation cancellation is observable inside executing source work and
  cannot poison decoded-window success or failure residency.
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
- dense schedule lowering produces topological node slots, contiguous Route and
  Contribution ranges, and liveness-reused scratch for Track→Bus→Output;
- scalar and runtime-vectorized kernels are sample-for-sample identical for the supported DSP
  set, including awkward block sizes and an eight-Track full-schedule render;
- exact fractional forward and reverse source mappings remain block-shaped and
  preserve floor semantics;
- the ignored fixed-reference load matrix exercises 1/8/32/64 Tracks at
  64/256/1024-frame blocks, compares scalar and SIMD PCM, and fails when p99
  exceeds the block deadline;
- decoded-media block reads cross aligned windows exactly, reuse hits, evict by
  global PCM bytes, and invalidate after file replacement;
- a manual external AAC parity gate compares a cold window, sequential boundary,
  and evicted-window random restart against one sequential reference decode;
- a manual real-child cancellation gate requires kill/wait/join observation
  within 50 ms and zero success/failure cache admission;
- concurrent identical window misses elect one decode leader and one follower
  cache hit;
- deterministic professional-audio acceptance tests reject missing/fake output
  facts and accept a complete versioned CPAL/A/V/memory evidence set; the real
  30-minute Adapter is ignored and never treats an absent fixture as a pass.

## Required depth before professional audio completion

The architecture is now on the product path, but these are explicit remaining
gates rather than implied support:

1. Extend the implemented fingerprinted Component Catalog, stable non-primary
   binding, and explicit mono/stereo/5.1 standard matrices with user-visible
   Component selection/rebind, deterministic custom-layout mapping, and
   authored mix-matrix policy. Unknown identities and unsupported layouts must
   continue to fail instead of being guessed.
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
8. Prove the persistent, cooperatively cancelable decoder Sessions on the fixed
   reference machine: cold open, sequential boundary, random restart,
   cancellation return, cross-source LRU pressure, and source-cache bytes must
   appear in the same long-run evidence report.
9. Non-constant built-in automation is presegmented during preparation and the
   realtime Session exposes its fixed allocation envelope. Extend the existing
   scalar/SIMD matrix to Buses, Transitions, dense automation, nested Sequences,
   decoder pressure, and stateful processors; add plugin event batches, declared
   latency/PDC, and state-entry obligations. Only then evaluate qualified GPU
   batch processors against measured CPU SIMD headroom and added latency.

No item may be closed by adding only schema, an effect enum, a disconnected UI,
or a consumer-specific fallback mixer.
