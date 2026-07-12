# Audio Pipeline

Audio decode/mix and realtime output live in `mondrian-media`; the Playback
Engine owns Clock Master qualification and `mondrian-app` is the timeline/render
Adapter.

## Target authoring and execution boundary

The current flat Timeline Adapter is an implementation bridge, not the intended
professional audio model. The durable design follows three first principles:

1. Authoring records stable user intent; it never stores scheduler jobs, loaded
   plugin objects, device handles, or compiler-generated nodes.
2. One typed compiler defines preview, playback, audition, analysis, and export
   sound. Consumers may request and schedule different closures, but cannot
   reinterpret the author model.
3. Placement changes where the same processor abstraction is owned; Clip,
   Track, Bus, and Output effects do not become four unrelated effect systems.

### Sequence author aggregate

```text
Sequence
├── Timeline
│   ├── Audio Tracks
│   ├── Audio Contributions
│   └── Audio Transitions
├── Sequence Semantic Catalog
│   └── Audio Roles
├── Audio Program
│   ├── Track Mixer Channel state keyed by TrackId
│   ├── Mix Buses
│   ├── Program Outputs
│   └── typed Routes, sends, and sidechains
└── Public Output Interface
```

An Audio Track is the editorial container visible on the Timeline. Its Track
Mixer Channel is the same Track's persistent mixer state, addressed by
`TrackId`; it is not a second independently deletable entity. Mix Buses and
Program Outputs have their own identities because they have independent
lifetimes. All three compose common Channel Strip values, but retain typed ports
and legal routing rules. This is behavioral reuse without a universal node ID
or property bag.

An Audio Contribution is the smallest independently processable PCM-bearing
timeline component. A media Clip may expose several contributions when its
audio components must be processed or role-assigned separately. Applying an
audio effect to the whole Clip is a UI command that targets its selected
contributions; it does not force the audiovisual Clip to become one audio blob.
A nested public output instance is also one contribution in its parent.

### One processor model at every insertion point

An `AudioProcessorRack` is an ordered sequence of `AudioProcessorInstance`
values. The same author type is used at these insertion points:

- an Audio Contribution for Clip-level processing;
- a Track Mixer Channel for the entire Timeline audio Track;
- a Mix Bus receiving any number of typed Routes;
- a Program Output before a public Sequence output is exposed.

Channel Strips may expose fixed pre-fader and post-fader rack positions. The UI
can present only the ordinary pre-fader rack until an advanced placement is
needed; the author model must still record the placement explicitly. Pre-fader
and post-fader sends are Route tap points, not processors. A sidechain targets a
declared auxiliary processor port and never enters the main-input sum by
accident.

A Program Output's main source is a closed author choice:

```text
RoutedInputs | SemanticProjection
```

It cannot implicitly sum both forms. A dynamic Output Family additionally
declares whether members are the anchor, direct children, leaves, or all
descendants of a local Audio Role, and whether overlap is intentionally allowed.
The ordinary family contract is disjoint; parent-plus-child projections that
would count the same contribution twice require an explicit overlapping
contract. Public member identity derives from Output Family and local Role
identity, while the member manifest and expected signal contract carry their
own revisions. Names and Standard Semantic Keys never rebind members.

Every processor instance persists:

- a stable instance ID and stable definition reference;
- a versioned definition/schema identity;
- stable Parameter IDs, base values, and automation;
- bypass state and negotiated main/auxiliary bus layout;
- a versioned opaque state chunk when required by an external plugin;
- enough dependency metadata to resolve, migrate, diagnose, or preserve an
  unavailable plugin without guessing.

Built-ins, VST3, and CLAP definitions adapt to this contract. `VST` in product
planning means VST3; Mondrian does not infer legacy VST2 compatibility. VST3
class IDs and CLAP plugin IDs are authoritative external identities. Binary
paths, scan order, display names, parameter indexes, `EffectType::Plugin(String)`,
and JSON blobs are not. Plugin discovery should run outside the product process;
execution location is a host policy and may evolve from vetted in-process use to
isolation without changing project semantics.

This processor model admits audio effects, not instruments by accident. VST3 or
CLAP instruments, MIDI/event generators, and multi-output sound sources require
an explicit source/event author model before they can be exposed; an effect slot
cannot silently reinterpret them. A native plugin editor is only a view over a
host-owned instance. Parameter gestures and opaque-state changes must mark an
undoable author transaction or a versioned state capture; closing the editor or
saving the Project cannot depend on unobserved runtime-only mutation.

The existing video `EffectNode` remains a video author type. Reusing its string
paths and RGBA graph capabilities for audio would create false unification;
shared concepts belong in small foundation values such as stable IDs, typed
parameters, curves, enable/bypass state, and migration protocols.

### Canonical processing order

Each Audio Contribution lowers in this order:

```text
media or nested output resolution
→ source channel interpretation
→ exact source-time mapping and resampling
→ clip input trim
→ ordered clip processor rack
→ clip volume/pan automation and fade envelopes
→ explicit two-input Audio Transition, when present
→ owning Track input sum
```

Each Track, Bus, or Output Channel Strip lowers in this order:

```text
typed input sum
→ input trim
→ pre-fader processor rack
→ pre-fader send taps
→ fader/pan automation
→ post-fader processor rack
→ post-fader send taps and typed output ports
```

Every multi-input merge, including Track sums, Buses, sends, sidechains,
crossfades, wet/dry branches, Program Outputs, and nested outputs, aligns paths
using declared processor and child latency. Dynamic latency or bus-layout change
produces a new compiled plan and controlled re-entry; it cannot mutate a running
graph inside an audio block. Internal `f32` PCM may exceed `[-1, 1]`. The mixer
does not normalize, `tanh`, hard-clip, or limit unless an explicit authored
processor or final sample-format conversion requires it.

Processors declare latency, tail/ring-out, required preroll, processing quantum,
reset, warmup, replay, checkpoint/state-transfer support, and determinism. These
capabilities affect compilation and state entry, not author identity. Clip
audible extent and processor tail are separate: the compiler must know the tail
even when the current product policy cuts it at the Clip boundary, so later
ring-out controls do not require a graph redesign.

### Fades, overlaps, and crossfades

A fade-in or fade-out is an envelope on one contribution. An Audio Transition is
a stable author entity naming exactly two contributions, an exact Sequence-time
interval, and a pair of curves such as constant-gain, equal-power, or explicit
custom curves. It is evaluated after each side's Clip-local processing and
before Track summing. Other material overlapping the same interval is
unaffected.

Timeline overlap alone means additive summing; it never silently creates a
crossfade. Creating a Transition validates both references, interval ordering,
available source handles, time maps, and channel contracts. Insufficient handles
must be resolved by an explicit edit policy; the compiler cannot stretch, hold,
or synthesize source samples without authored intent. Transition and Clip fade
envelopes multiply when both are present, making their combination predictable.

Razor/split is required to be acoustically neutral. A split initially creates
two Timeline ranges of the same Audio Contribution and processing scope, so it
does not reset a compressor, reverb, or external plugin at the cut. An edit that
makes one side independent explicitly forks the contribution and processor
identities. Duplication creates new identities. The compiler never infers state
continuity from equal hashes.

### Automation and parameter events

Parameter identity is `(AudioProcessorInstanceId, ParameterId)`. Built-in
Parameter IDs are namespaced and schema-versioned; VST3 and CLAP adapters map
their stable native IDs without substituting array indexes. A definition
snapshot retains enough metadata to display and preserve automation while the
plugin is unavailable, but metadata never makes an unresolved plugin executable.

Automation positions use exact rational author time in the owner's natural
domain:

- contribution/Clip automation is contribution-local and follows an edit;
- Track, Bus, and Program Output automation is Sequence-local;
- Transition curves are transition-local;
- child automation remains child-Sequence-local and is mapped through each
  nested instance independently.

The compiler maps these domains through source, speed, and nesting transforms to
the Sequence sample timeline once. It then emits bounded parameter events at
exact sample offsets. Curve results cannot depend on render block size. A host
adapter must report whether it applied sample-accurate, block-accurate, discrete,
or unsupported automation; unsupported automation is preserved and blocks or
explicitly degrades the relevant admission rather than being ignored. The
existing `TimeTicks = frame * 1000` helper loses time-base identity and therefore
cannot become the audio automation persistence contract.

Audio and visual automation must not fork into separate persisted curve
systems. Both use ADR-0004 Timeline Time, stable Parameter IDs, typed values,
interpolation constraints, and exact temporal handles. Their consumers differ:
the renderer evaluates at frame/shutter instants, while the Audio Compiler emits
sample-offset events. UI snapping is likewise independent: video tools normally
snap to Sequence frames, audio tools may snap to samples or free time, and both
write the same exact author coordinate.

### Compilation, demands, and mutable state

The Audio Compiler lowers the validated aggregate into an immutable typed DAG.
Generated operations retain deterministic author origins for diagnostics,
latency analysis, cache manifests, and DSP state allocation, but users cannot
route to them. Instantaneous cycles fail validation. A future feedback feature
must introduce an explicit delay/state operator with defined initialization and
latency instead of weakening the whole graph to permit arbitrary cycles.

Execution identity has four separate layers:

1. Consumer Demand Intent states who needs which outputs, atomic success, and
   service-level requirements.
2. Signal Closure resolves only the required roots, Routes, processors,
   formats, automation, and nested dependencies. Only this layer decides plan
   sharing.
3. State Domain and State Entry Plan determine mutable processor identity and
   how valid state is reached for the requested continuity epoch.
4. Scheduler runtime owns block ranges, deadlines, watermarks, and frontiers;
   these coordinates never enter semantic fingerprints.

Consumer purpose and service-level policy do not enter the Signal Closure
fingerprint. Monitor and recorder demands may therefore share their common
program path, while device conversion, dither, file encoding, loudness/meter
accumulators, and queue state remain separate downstream closures and State
Domains. Activation position belongs to a State Entry Plan when state is absent;
it is not part of an already-established State Domain identity.

The Playback Engine remains sole Transport, Clock, epoch, and product-policy
authority. An Audio Execution Coordinator reconciles immutable, low-frequency
demand snapshots and returns admission/evidence; it may reject stale work, keep
the last valid plan, detach a failed sink, or protect realtime position with
same-duration silence, but cannot invent a second playback state machine.
Offline export shares author semantics, compiler IR, and processors without
pretending to be a realtime Demand Session.

Processing mode remains explicit. A processor may advertise distinct realtime
and offline capabilities, but the mode participates in plan and State Domain
identity. A realtime-only processor is either rendered in a truthful realtime
export path or blocks that export contract; it is never bypassed merely because
the scheduler requested faster-than-realtime work.

Mutable state is isolated by resolved closure, continuity epoch, processor
origin, nested instance path, evaluation/time-mapping context, direction, and
processing-mode contract. Seek/cold entry, exact continuation, and controlled
transition are distinct. A checkpoint restores only its checkpoint position;
replay is still required to reach a later target. Exact PCM cache data does not
prove live processor state. Third-party processors default to no replay,
checkpoint, or state transfer until the adapter proves those capabilities.

Canonical fingerprints use a versioned deterministic binary encoding of the
migrated, validated author model. They locate candidates only. Installation or
reuse additionally compares normalized typed descriptors and a complete
dependency manifest, and hashes never authorize mutable state sharing.

### Required scenario behavior

| Scenario | Author operation | Required compiled behavior |
|---|---|---|
| Clip EQ or denoiser | Add one processor to selected Audio Contribution racks | Each contribution processes before its envelopes and Track sum |
| Whole-Track compressor | Add one processor to the Track Mixer Channel rack | All current and future contributions on that Track share one ordered stateful path |
| Multi-Track vocal Bus | Route selected Track outputs to one Mix Bus and add processors there | Tracks sum once at the Bus; latency and sidechains remain explicit |
| Master limiter | Add processor to Program Output | Monitor and export consume the same authored limited output unless a Delivery Mapping selects another port |
| Crossfade | Create one Transition referencing two contributions | Two processed, latency-aligned branches receive paired sample-domain curves; unrelated overlaps are unchanged |
| Parameter keyframes | Write stable Parameter-ID automation in the owner domain | Events map to exact sample offsets and remain invariant under scheduler block size |
| Nested Sequence twice | Instantiate the same public child output twice | Immutable child plan may be shared; all mutable plugin/DSP state is isolated per nested instance |
| Missing VST3/CLAP | Preserve instance, state chunk, parameters, and expected I/O contract | Preview may explicitly bypass/degrade by policy; export blocks or requires an explicit override and loss report |
| Razor an effected Clip | Split Timeline range without changing sound | Processor continuity is preserved until an explicit independent edit forks the processing scope |
| Processor latency changes | Accept a reported capability revision | Recompile and controlled re-entry occur; no in-block graph mutation or silent A/V shift |

### Current implementation gaps

The repository does not yet implement this author or compiler model:

- `Sequence` currently stores only Timeline Tracks and has no Audio Program,
  Semantic Catalog, public audio interface, Buses, or typed Routes.
- `Clip.effects` stores video `EffectNode` values and must not be generalized by
  merely adding audio enum variants.
- `TimelineAudioPcmRenderer` directly decodes active Clips into a flat list and
  bypasses Clip processors, Track Channel Strips, Buses, transitions, nesting,
  automation, latency compensation, and plugin state.
- `AudioMixer` is an interim flat mixer and currently applies hidden `tanh`
  soft-clipping; the compiled mix core must replace that behavior.
- current automation property paths and `frame * 1000` time coordinates are not
  stable audio Parameter IDs or exact audio time.

These are migration facts, not alternate supported semantics. New audio work
must move through the author snapshot and compiler boundary rather than deepen
the flat app Adapter.

### Module ownership

The dependency boundary is fixed. After the common time/parameter foundation is
migrated, the first audio author-to-IR vertical slice creates one physical
`mondrian-audio` crate:

- `mondrian-timeline` owns pure Sequence audio author data, validation,
  migrations, and undoable commands. It knows processor descriptors and state
  blobs as data, but never loads a plugin or executes DSP.
- `mondrian-audio` owns author-to-IR compilation, Signal
  Closure resolution, latency analysis, processor host contracts, mutable State
  Domains, execution coordination, and the common PCM block contract. It depends
  on `mondrian-core`, `mondrian-timeline`, and `mondrian-playback`, but never on
  FFmpeg, CPAL, platform UI, or `mondrian-app`.
- `mondrian-media` depends on the Audio Engine interfaces to implement media
  decode/source-provider adapters and caches; it does not interpret Timeline
  routing or processor order.
- platform/CPAL audio code implements physical device discovery, stream
  lifetime, and the realtime sink interface owned by `mondrian-audio`.
- `mondrian-playback` remains the sole Transport, Clock Master, continuity-epoch,
  and recovery-policy owner.
- `mondrian-export`, analyzers, recorders, and monitor paths submit demands and
  consume outputs/evidence; they do not compile a private audio graph.
- `mondrian-app` wires snapshots, providers, demands, and evidence. It must not
  remain the production Timeline PCM renderer.

Do not create empty per-format crates merely to mirror this diagram. VST3 and
CLAP begin as host adapters behind the same Audio Engine contract and split
physically only when platform dependencies or process isolation require it.

The initial crate should remain internally modular rather than split further:

```text
mondrian-audio
├── block / signal_format
├── ir / compiler
├── processor / parameter_events
├── latency / state
├── demand / coordinator
├── execution
└── source / sink / plugin host interfaces
```

Author entities and migrations do not move into this crate; they stay in
`mondrian-timeline`. FFmpeg decoders, CPAL streams, and concrete plugin-format
loading do not define the compiler core. This prevents both a circular
Timeline↔Audio dependency and a new all-purpose media/runtime crate.

### Foundation implementation order

1. Introduce canonical Timeline Time, Time Domains/Transforms, stable Parameter
   IDs, and exact curve primitives in `mondrian-core`. Keep compatibility
   adapters while preparing one transactional project-schema migration from
   legacy `TimeCode`/`TimeTicks`; no new persisted audio field may use the legacy
   coordinate.
2. In one vertical slice, add only the necessary Sequence author entities in
   `mondrian-timeline` and create `mondrian-audio`: one Contribution → Track
   Channel → Program Output path, one built-in Gain processor, exact automation,
   immutable IR, reference execution, save/open migration, undo/redo, realtime
   demand, and identical preview/export reference samples.
3. Put the current decoder and sink behind the new interfaces, make the product
   path consume the compiled slice, and remove hidden `tanh`. Do not keep the old
   flat mixer as a second interpretation path.
4. Extend author data and compiler together in independently testable slices:
   typed Routes and Buses with latency compensation; fades and two-input
   Transitions; nested public outputs and per-instance State Domains; Roles,
   Semantic Projections, and Output Families. No full unused schema is built in
   advance.
5. Prove the processor host contract with built-ins and a controlled fake
   external adapter before integrating real VST3 or CLAP discovery, state, UI,
   and failure isolation.

The first slice is complete only when save/open migration, undo/redo, headless
compile, deterministic reference PCM, realtime demand, and offline export all
exercise the same author semantics. A crate that contains only types or forwards
the old flat mixer does not satisfy the boundary.

## Core types

- `AudioBuffer`: interleaved `f32` PCM with explicit sample rate and channels.
- `AudioSamplePosition`: signed integer sample position paired with its sample
  rate. Rational timeline time is resolved through an explicit floor, ceil, or
  nearest policy without an intermediate floating-point-seconds value.
- `AudioMixer`: interim flat mixer used by the current Timeline Adapter; it is
  not the target Audio Program compiler/executor.
- `AudioSourceCache`: caches decoded source PCM by media path.
- `RealtimeAudioOutput`: one concrete CPAL stream with a fixed-capacity PCM queue
  and callback-only atomic telemetry.
- `RealtimeAudioOutputManager`: lazy, non-blocking device lifecycle
  Implementation retained behind Audio Playback's internal output Seam.
- `AudioPlayback`: deep Module owning device lifecycle, integer-sample PCM
  scheduling, render worker, generation invalidation, watermarks, preroll, and
  immutable evidence.
- `AudioPlaybackMode`: the transport permission Interface with distinct `Idle`,
  `Preroll`, and `Consume` modes. It prevents a caller from conflating PCM
  preparation with permission for the device callback to consume it.
- `AudioPcmRenderer`: narrow render Interface implemented by the app timeline
  Adapter and the headless deterministic Adapter.

## Timeline interaction

Audio clips live on audio tracks. Track mute/solo controls are audio semantics;
video track visibility is separate. The app Adapter renders exact integer-sample
windows requested by Audio Playback; it does not own scheduling. Seek, stop,
stream replacement, or rejected handoff increments the generation. Completion
acceptance checks generation before changing in-flight accounting or output, so
old work cannot become audible or corrupt the current watermark.
The internal queue is bounded by the in-flight policy and reprime synchronously
removes queued old-generation windows. At most one already-executing old window
may finish, after which the generation gate discards it.

## Realtime ownership and lifecycle

The target clock, device-loss, preroll, and handoff semantics are specified in
[Playback Engine](playback-engine.md). Normal playback uses qualified consumed
device samples as Audio Device Clock Master. When output is unavailable,
transport continues on Synthetic Clock Master; displayed video is never master.

Device discovery is lazy: a project without an audible audio clip remains on
Synthetic Clock Master and does not start a meaningless output stream. Device
discovery and stream construction never execute on the UI thread. CPAL
streams are `!Send`, so a named device thread retains concrete stream ownership
for its entire lifetime. It publishes only a sendable handle made of `Arc`,
atomics, and the lock-free PCM queue. Open failure retries exponentially from
250 ms to a 5 s ceiling. An asynchronous stream error removes the handle and
reopens while Synthetic Master continues.

A new stream remains inactive while Audio Playback invalidates old render work,
clears queued PCM, anchors scheduling to the current exact timeline
`TimeCode`, and queues at least 120 ms. Callback consumption is enabled only
after that preroll **and** a `Consume` permission. Playback `Priming` supplies
`Preroll`: render workers may fill the same generation to the high watermark,
but the callback remains inactive and cannot run ahead of the held transport
anchor. Transitioning to `Playing`/`Recovering` supplies `Consume` and activates
the already-primed generation without clearing or rerendering it. Each clock
observation carries the exact media anchor for
active-consumption frame zero; the Playback Engine derives candidate media
phase from that anchor and integer consumed frames. Phase error over the 20 ms
policy budget rejects the stream and starts a fresh preroll without moving or
reanchoring the published timeline.

Rendered, queued, callback-requested, latency-adjusted, and consumed positions
are distinct. Current CPAL evidence is explicitly graded
`CallbackConsumptionEstimate`; it is not a backend device position.

Render scheduling accumulates integer sample frames, not floating-point seconds.
Timeline/edit boundaries must be converted once with
`AudioSamplePosition::from_timecode`; playback, decode placement, buses, meters,
and export then carry integer positions. Positions at different rates fail
closed until an explicit resampling Adapter maps them. This prevents 29.97/59.94
and long-project chunk boundaries from independently rounding the same edit.
The realtime timeline Adapter now uses this contract for window, clip, and
identity-speed source boundaries. Animated/non-1x `SpeedMap` evaluation remains
frame-domain and is isolated as a legacy branch; export and time-remapped audio
must migrate to the same sample-domain Interface before the exact audio mapping
roadmap item can be closed.
The production policy uses 80 ms windows, 120 ms preroll, a 460 ms high
watermark, and at most eight admitted windows. These values are one validated
`AudioPlaybackConfig`, not environment-variable semantics scattered through the
app. A failed or malformed window is replaced with exact-duration silence and
structured evidence; dropping it would shift every later sample against its
declared media anchor and is forbidden.

## Realtime callback contract

The callback may read/write only the preallocated queue and atomics. It does not
allocate, log, decode, inspect the timeline, perform file I/O, publish general
events, or take a contended lock. Inactive output writes silence without draining
the PCM queue. Active starvation writes silence and increments underrun evidence.

## Underrun recovery

Underrun policy uses missing output sample frames, never UI poll counts. The
product threshold is 960 frames at 48 kHz (20 ms) per activation interval. New
missing frames emit `UnderrunObserved`; totals below threshold leave Audio Device
Clock Master active. Reaching threshold emits `UnderrunRecoveryStarted`, submits
the final pre-deactivation callback position, then deactivates output, clears
queued PCM, invalidates the render generation, and reprimes from current
Synthetic time. This avoids both a one-underrun master flap and a hidden 20 ms
freeze at handoff.

Recovery remains explicit in `AudioPlaybackState`, snapshots retain current
interval missing frames and recovery count, and reactivation again requires the
full 120 ms preroll. Callback silence is evidence of missing audio, not a reason
to let video presentation become authoritative.

## Remaining depth

Device lifecycle, PCM scheduling, generations, watermarks, preroll, and Clock
Master qualification now have narrow Interfaces. Remaining Audio Playback depth
is backend-position evidence, bounded resampling/slew for small non-zero phase
error, richer long-window underrun hysteresis, and reference-machine drift gates.
Current handoff accepts phase already inside budget; it does not claim to
correct it.

UI Modules may request waveform or transport actions. They never decode audio or
own device state. Export consumes timeline/audio data through export
orchestration, not realtime output state.
