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

An audio Track owns Clips. A Clip owns placement, duration, one closed exact
`ClipSourceTimeMap`, nested Sequence identity, disabled state, and linkage to
other Clips. Source terminal boundaries are derived from that map and duration,
never persisted independently. Those are the only persistent placement facts.

Each audio Clip owns one or more `AudioComponentEdit` values. An edit contains:

- a stable `AudioComponentEditId`;
- a media `AudioSourceComponentId` or nested `ProgramOutputId`;
- a `Standard` or exact explicit source-to-Sequence channel mapping;
- optional Sequence-local `AudioRoleId`;
- enabled state;
- exact edit-local origin (`local_time_in`);
- static and exact-automation volume/pan;
- independent fade-in/fade-out definitions;
- a restricted `AudioProcessingBinding { scope_id, scope_in }`.

It cannot contain a Track ID, Sequence range, Clip speed, source range, nested
Sequence ID, Route, or output. Compilation derives those from the owning
Track/Clip. This prevents two editable copies of placement from diverging.

The forward-rate product Action may expand a complete linked video/audio Clip
group and replaces every member's canonical source map in one author
transaction. Audio compilation then lowers the new positive exact scale through
the existing dense schedule; placement duration and Component Edit origins do
not move. Freeze frame is video-only. Audio is never converted to a zero-rate
single-sample hold as an incidental consequence of a picture operation.
The focused Retime Hero gate recompiles the persisted linked audio Clip after a
durable Project reopen, prepares the complete routed Track closure into the
dense schedule, and checks the exact source coordinate there. Its metadata-only
source is semantic evidence, not PCM decode or export-audio evidence.

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
index remains unresolved until an explicit rebind; neither language nor current
default disposition may silently retarget authored edits. Rebind is an atomic
Asset-library operation against current probe and file-fingerprint evidence. It
changes only the selected logical Component's physical binding and preserves its
ID. Two logical Components may deliberately alias one physical stream after
explicit repair because deleting or retargeting the other ID could invalidate a
different Project; automatic import and reconcile never create such aliases.
The repair workflow is deliberately two-step when the stored probe is stale:
refresh current stream candidates by conservative reconcile, then explicitly
rebind one stable Component. Refresh may add IDs for newly discovered unclaimed
streams but cannot repair or remove an existing binding. Rebind changes only the
chosen binding. This keeps automatic evidence acquisition separate from user
authoring intent.

The Inspector selects the logical source of each `AudioComponentEdit` by its
stable edit ID. Media Clips offer Asset Component IDs; nested Clips offer only
the child Sequence's stable public outputs. Independent typed field actions
address enabled state, static volume, static pan/balance, fade-in, or fade-out;
they never round-trip or replace the complete edit, so an interaction cannot
overwrite unrelated automation, channel mapping, Role, or processing state.
The application derives actual Track ownership, enforces Track lock, resolves
the stable edit ID, validates the complete audio author aggregate, and commits
one Sequence snapshot only when the field changed. A rejected or no-op action
does not advance revision/history. Successful source or field changes are
undoable and invalidate the prepared Runtime at the current transport anchor;
running playback re-prepares, while paused/stopped playback releases the stale
source.

Physical stream refresh/rebind remains an Asset-library action and is never put
into Timeline JSON or Timeline Undo. It is deliberately separate from
placement-local mix authoring.

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
absolute physical stream with `-map 0:<index>`, requests the output sample rate,
and installs only an ordinal identity `pan=<N>c|c0=c0...` filter. Decoded-window
and persistent Session identity therefore contain the source revision, physical
selection, sample rate, and native semantic layout, but never a Clip-specific
downmix. Two edits using one source with different authored matrices share
native PCM safely.

Each Component Edit persists either `Standard` policy or one canonical sparse
`AudioChannelMixMatrix`. A matrix includes exact source/destination layouts and
destination-major non-zero coefficients; construction/deserialization rejects
duplicate or out-of-range edges, non-finite values, and magnitudes above 16.
Zero edges are omitted and absent edges are silence. No matrix adds implicit
normalization, clipping, limiting, or LFE policy. `Standard` resolves only after
the exact source layout is bound. Equal layouts use ordinal identity; approved
mono/stereo/5.1(side) conversions retain the documented averaging/-3 dB laws
and omit LFE from fold-down. Explicit one/two-channel Discrete compatibility is
versioned; ambiguous or unsupported pairs fail closed.

The prepared Contribution, not FFmpeg or UI, owns the resolved matrix and native
source layout. It reads native interleaved PCM into one Session-preallocated
maximum-source scratch region, then maps into the Sequence layout before Clip
Scope processing, volume/pan, fades, Transitions, and Track summing. Nested
Sequences render their public output in the child Sequence layout and cross the
same matrix boundary at the parent Component. A child can no longer be silently
re-rendered in the parent's layout. The selected root Program likewise renders
in its authored Sequence layout; Playback and Export apply a separate standard
delivery matrix only after the public output. Unsupported device/export pairs
fail instead of changing Program semantics.

### Processing scopes

`AudioProcessingScope` is a Sequence-owned processing definition containing
input gain/automation and one ordered `AudioProcessorRack`. It has no placement
or routing fields. Sharing a Scope shares its author definition and exact time
coordinate, not mutable DSP state. The restricted binding permits only identity
plus an exact time offset; arbitrary affine mappings would recreate a hidden
placement model. Preparation materializes a distinct processor occurrence for
each Contribution. Any state-domain sharing requires equality of the complete
signal projection, processor contract, parameter delivery, nested instance
path, evaluation mode, and continuity epoch; `scope_id`, plan equality, or a
fingerprint alone is insufficient.

The product Rack UI projects these definitions through one shared App UI
Module. That Module consumes Timeline's `inspect_audio_processor_rack`
Interface for every Processing Scope, Track, Bus, and Program Output address;
it does not scan Tracks or reproduce lock admission. The Interface returns Rack
contents, complete-Sequence Scope binding count, and the authoritative edit
blocker from one immutable author snapshot. A selected Clip shows each distinct
bound Scope exactly once, even when multiple Component Edits bind it. An invalid
address becomes an explicit disabled projection instead of silently hiding
author state.

The first product slice supports canonical Gain and linked-channel sample-peak
Lookahead Limiter insertion, stable-ID reorder/remove/bypass, and schema-driven
unkeyed numeric parameters. Sample Delay remains an execution contract fixture
rather than a claimed product effect. Existing VST3/CLAP and unknown built-in
snapshots remain visible by their persistent identity and captured schema;
unavailable dependencies are never deleted or silently flattened. Keyed curves
remain read-only in this slice because a fallback value is not the playhead
evaluation. Automation editing must first bind an exact Scope-local coordinate
and stable Keyframe identity.

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

Normative strip controls enter through `AudioChannelStripEditRequest`, addressed
by the same closed `Track | Bus | ProgramOutput` owner type as Rack addresses.
The edit algebra owns static input trim and static fader only; all keyed changes
enter the unified `AudioAutomationEditRequest` Interface described below. A
static fader edit is rejected while automation is authoritative; UI therefore
cannot modify or display the curve's fallback value as though it were the
playhead value. Removing the final key canonicalizes back to one static fader
instead of retaining an empty competing curve. Address errors, temporary Track
lock blockers, and invalid mutations are separate typed results.

`inspect_audio_channel_strip` is the common read-only Interface used by Mixer
and Channel Strip Rack inspection. Track, Bus, and Program Output resolution and
Track-lock admission therefore have one Timeline Implementation. Admission
happens before copy-on-write detachment; a changed candidate still validates the
complete Audio Program before publication, while a no-op advances no author
revision. Track mute remains a Track field and the Mixer submits the existing
Track transaction; it is not duplicated into `AudioChannelStrip`. Transient
solo remains outside author state.

Routes use stable typed endpoints. Source ports are `PreFader`,
`PostFaderPreMute`, or `PostMute`; destinations are Bus or Program Output.
Routes never name array indexes, display names, generated Clip stages, or child
internals. The same edge represents both a channel's principal route and a
parallel send: there is no second Send graph. Each Route owns an explicit
enabled flag, a static level, and optional Sequence-time level automation using
the stable `mondrian.audio.route_gain.db` Parameter ID. The automation curve is
the value authority when present; static level is used only when it is absent.
Levels, keys, and Bezier controls are confined to `[-120, +24] dB` before a
snapshot can compile. Disabled Routes are retained as author intent but do not
enter the selected output's signal closure. Instantaneous cycles are rejected
across the complete author graph, including disabled edges. Feedback will
require an explicit delay/state operator with defined initialization and
latency.

All Bus and Route lifecycle mutations enter through the single
`AudioRoutingEditRequest` Interface. It creates, renames, or removes Buses;
creates, removes, or rewires Routes; and changes enabled/static gain without
exposing the underlying authoring collections. Route keys enter the unified
automation Interface. `CreateBus` may create one onward Route in the same transaction,
and its receipt returns both allocated stable identities. Bus removal requires
an explicit `RejectIfConnected` or `Disconnect` policy. Disconnect atomically
removes every incoming and outgoing strong Route reference, and refuses when
that would edit a Route sourced from a locked Track. Every other mutation of a
Track-sourced Route obeys the same Track lock. Static Route gain is rejected
while its curve is signal authority, and deleting the last key canonicalizes
back to the retained static gain. Every changed candidate passes complete Audio
Program validation, so an instantaneous Bus cycle or invalid endpoint never
publishes partial state.
`inspect_audio_route_candidates` is the matching bulk read Interface: it checks
existing endpoint closure, captures Track-lock admission, and precomputes
disabled-edge-inclusive Bus reachability once. Mixer/patchbay Adapters may use
its constant-time addition query to omit impossible choices, but may not own a
parallel cycle algorithm or treat the projection as transaction authority.

This minimal edge contract already covers dry paths, pre/post-fader auxiliary
sends, submixes, stems, and multiple parallel paths. Sidechain inputs are not
ordinary summing destinations: they require a typed processor-input endpoint
and processor bus negotiation, and must not be simulated by weakening Route
destination types.

Disabled Clip or disabled Component Edit is absent from compilation. Persistent
Track mute gates `PostMute` while preserving pre-mute taps. Solo is not stored
on Track: an `AudioAuditionOverlay` selects a temporary closure without
rewriting or exporting the canonical Program. App owns that overlay for one
open `AuthoringSessionId` and Sequence. Changing it rebuilds the root Playback
Runtime at the authoritative transport anchor; a failed replacement restores
the prior overlay. Recursive child Program Outputs always compile canonically,
because parent Track identities have no meaning inside a nested Sequence.
Paused playback clears its prepared source, while export, analysis, idle
warmup, and other canonical consumers pass an empty overlay.

### Unified audio automation authoring

`AudioAutomationTarget` is the sole keyed-curve address across Component
volume/pan, Processing Scope input gain, Channel Strip fader, Route gain, and
Processor parameters. Every variant carries stable typed owner identity; array
indexes, display names, plugin paths, and UI property strings cannot address a
curve. `inspect_audio_automation` resolves the canonical curve, numeric hard and
soft ranges, allowed interpolation modes, edit admission, and its explicit
`AuthoringTimeDomain` (`Sequence`, `AudioComponentEdit`, or
`AudioProcessingScope`) from one immutable Sequence snapshot.

`AudioAutomationEditRequest` performs stable-ID key upsert, key removal, or
explicit clear-to-default. It admits Track and shared-Scope locks before
copy-on-write detachment, mutates a complete candidate, validates the complete
Audio Program, and publishes atomically. Moving a key preserves its ID,
interpolation, and Bezier handles; collisions at the same exact owner time fail
closed. Optional semantic curves canonicalize to their static value after the
last key is removed, while Processor parameters retain their intrinsic empty
curve because that curve also owns the unkeyed definition value. Channel Strip,
Routing, and Rack Modules retain only their static/topology operations, so no
second keyed write path can drift from this contract.

### Processors and plugins

Every insertion point uses `AudioProcessorInstance`, regardless of whether the
definition is built-in, VST3, CLAP, or a future host format. Author data keeps:

- stable instance and definition identity;
- one versioned `ParameterSchema` snapshot per stable Parameter ID;
- explicit bypass;
- one exact curve per parameter, whose default is the unkeyed value;
- opaque versioned external-plugin state.

An unavailable dependency remains serializable and editable but is not
executable. Semantic compilation preserves its definition, schema/curves, and
opaque state; concrete plan preparation fails closed unless a resolver realizes
it for the exact sample-rate/layout/block/mode contract or the author instance
is explicitly bypassed. Plugin path, scan order, parameter array index, and
display name are never identity. Parameter map key, schema identity, and curve
identity must be identical. Project validation rejects non-finite values,
values and Bezier control points outside the hard range, disallowed
interpolation mathematics, and keyframes on non-animatable parameters before
an immutable snapshot exists.

The built-in schema-1 processor definitions and every hosted definition are
admitted only when the complete captured parameter schema matches the resolved
definition; required parameters are never synthesized during compilation.
Compilation retains Processor Instance and Parameter IDs, and preparation
creates one ordered occurrence with independent mutable state for each exact
owner and insertion point.

Every realized processor uses the same prepared Host contract: fixed
algorithmic latency, explicit `None`/finite/infinite tail, continuity and mode
admission, bounded per-Session storage, borrowed parameter batches, and
block-partition-invariant execution. Intentional audible delay is signal
semantics and must not be reported as PDC latency. Rack latency/tail arithmetic
is checked, and native plugin sentinel values must be normalized by the
Adapter. Resolution and instantiation happen before callback execution;
unavailable or unsupported external definitions fail closed without a dry or
flat fallback.

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

A present Clip fade has an exact duration strictly greater than zero and no
greater than the owning Clip duration. Absence is the only canonical zero-fade
state. Product UI therefore maps an entered duration of zero to `None`, and
quantizes positive seconds once at the UI/domain boundary; serialized author
state never carries two representations for “no fade”.

## Compilation and execution

### Stage 1: semantic compilation

`compile_audio_program(Sequence, AudioCompileRequest)` first validates the full
author closure, resolves one stable Program Output, computes the required Route
closure, and derives immutable generated contributions. Every contribution
records deterministic origins:

- Component Edit ID and Clip ID;
- owning Track ID and derived Sequence range;
- exact constant source origin/scale lowered from the Clip source-time map;
- media component or nested public output identity;
- Processing Scope and exact edit/scope origins;
- edit automation, fades, and relevant Transitions.
- standard/explicit Component channel-mapping intent.

Only generated IR uses the term “Compiled Audio Contribution”. It is never
persisted and users cannot route to it. Semantic Projection outputs currently
fail compilation explicitly; only `RoutedInputs` is executable.

Track mute is resolved at the same closure Seam, not later in App code. A muted
Track's `PostMute` edge is mathematically closed and therefore cannot
retain its media, nested-output, Scope, or Track-processor dependencies.
`PreFader` and `PostFaderPreMute` edges remain selected and retain the complete
source branch. This applies equally to a principal route and a parallel send;
there is no “muted Track means no audio” shortcut. Audio Track `is_visible` is
presentation state and never changes PCM routing.

The resulting `CompiledAudioProgram` publishes one typed
`AudioProgramExecutionDemand`. `ProvenSilent` means the selected Program Output
has no remaining Contribution and no active processor that could affect it, so
a consumer may substitute exact silence without constructing an execution
Session. `RequiresExecution` is conservative across the processor Adapter
Seam: an active Bus, Track, Scope, or Output processor may retain a tail or
generate signal from zero input, and the current processor Interface publishes
no silence-preservation proof. App/UI code cannot weaken this evidence by
walking Tracks, Clips, or media metadata. This keeps source selection, pre-mute
sends, Buses, processor tails, future generators, and pure silence under one
compiler interpretation.

Semantic compilation is also the sole author-to-execution ownership Seam.
Copy-on-write author racks, parameter maps, and opaque plugin-state byte lists
are copied into ordinary detached execution-owned containers. A compiled plan
therefore cannot observe later author mutation, and author allocation identity
or sharing is never part of execution equality, scheduling, or Session state.

### Stage 2: preparation

`PreparedAudioPlan` binds immutable semantic IR to an `AudioRenderContract`:

- positive sample rate;
- one supported semantic channel layout with count and order derived from it;
- maximum admitted block frames;
- `Realtime` or `Offline` processing mode;
- maximum processor-private bytes admitted for one Session.
- maximum internal public-output lookahead frames and maximum PDC delay-line
  bytes admitted for one Session.

The current product contracts admit at most 480,000 internal lookahead frames,
256 MiB of aggregate interleaved compensation storage, and 256 MiB of
processor-private Session storage. These are independent ceilings. A consumer
may lower either ceiling to zero when its selected graph requires none; zero is
not an invalid contract and never grants an implicit fallback.

Preparation now lowers the semantic graph into one dense execution schedule:

- stable topological slots for Track Channels, Buses, and the selected Output;
- destination-contiguous incoming Route ranges and Track-contiguous
  Contribution ranges;
- dense Processing Scope slots and Contribution-local Transition bindings;
- half-open source-active and causal-execution sample spans lowered once for
  the Render Contract; a Contribution executes through its checked
  source/rack algorithmic latency plus finite tail, or remains unbounded for an
  infinite tail;
- liveness-assigned scratch slots whose lifetime extends through the last
  downstream consumer;
- a checked latency solution for every Contribution-to-Track and Route-to-node
  sum, including the exact `PreFader` versus post-fader source port;
- ordered dense Processor ranges for every Scope/pre-fader/post-fader Rack,
  with deterministic author origin plus generated owner/insertion identity;
- constant Scope/edit/fader fast paths that do not erase Processor boundaries;
- constant Route-level fast paths plus prepared Route automation spans and one
  Session-preallocated interleaved gain lane;
- one explicitly selected scalar-reference or runtime-vectorized CPU kernel.
- one resolved native source layout and canonical prepared channel mixer per
  Contribution, with coefficient count and maximum native channel width in the
  schedule/capacity evidence.

The prepared schedule contains no authoring maps and the Session performs no
Route search. Automation is validated once and lowered to ordered sample event
spans; the Session advances span cursors instead of searching or validating
author curves per sample. Each generated Processor has stable Parameter-ID
lanes. Static lanes produce one offset-zero event per block; varying lanes
produce exact values at every sample offset in Session-reused storage. Latency
is accumulated through each
pre-/post-fader rack and resolved independently at every real summing point;
the selected Program Output exposes the resulting internal lookahead. Every
automation and Processor occurrence also receives its checked input-signal
delay: Scope input, each Rack prefix, edit envelope, channel fader, and Route
send evaluate `execution sample - stage delay`, so PDC cannot move a keyframe
relative to audible content. Recursive preparation is bottom-up. A child
public output is already Timeline-aligned and therefore enters its parent with
zero algorithmic latency; its state-entry obligation still propagates.
Preparation owns layout negotiation, processor realization, plugin-specific
batch lowering, and each realized processor's latency, tail, state, and
private-scratch contract. It checked-sums every realized occurrence's private
scratch and rejects required bytes above the admitted Render Contract before
any Session is constructed. It also rejects public
lookahead or aggregate interleaved compensation storage beyond the explicit
Render Contract budgets. Generic compensation execution and root Session entry
consume the remaining realized facts. Ordinary
tests use a custom stateful non-zero-latency Host Adapter to prove general PDC,
whole/partitioned PCM, explicit entry, mode rejection, and failed-entry
poisoning. The built-in Sample Delay separately proves a production stateful
definition without mislabelling audible delay as compensable latency. The
built-in linked-channel sample-peak Lookahead Limiter is the first production
Processor that declares nonzero algorithmic latency and exercises those same
PDC and public-lookahead contracts.

The Lookahead Limiter owns a deep private Module rather than adding special
cases to graph traversal or the Host Interface. Preparation validates the exact
versioned Ceiling/Lookahead/Release schemas, converts non-automatable Lookahead
milliseconds upward to a fixed sample count, and admits every delay, parameter,
gain, peak-value, and peak-index buffer as processor-private Session scratch.
The callback uses a fixed-capacity monotonic queue, so the maximum linked
absolute sample over `[signal time, signal time + lookahead]` is updated in
amortized constant work instead of rescanning the window. Audio, Ceiling, and
Release are delayed together; downstream PDC therefore cannot shift parameter
time. Attack is immediate, release is a one-pole recovery, and all channels use
one gain so the spatial image remains coherent. A final per-sample rounding
guard enforces Ceiling. Non-finite PCM or contract drift poisons the owning
continuity epoch before any output publication. This is deliberately a
sample-peak limiter: no documentation, meter, or delivery evidence may call it
true-peak until a separately qualified oversampled detector and reference
corpus exist.

### Stage 3: exclusive mutable Session

`AudioRenderSession` allocates the schedule's liveness-sized scratch bank before
rendering; it does not allocate one permanent buffer per author node or Route.
`render_into` accepts an exact signed start sample and exact frame count and
writes caller-owned interleaved float storage. The Session is exclusive mutable
state; plans may be shared, Sessions may not.

Session construction also allocates one state slot per generated Processor
occurrence plus the largest Parameter-lane and sample-event batch required by
any one Processor block. Racks execute in authored order and reuse that bounded
batch storage sequentially. Capacity evidence reports occurrence count,
maximum lane/event capacity, and the sum of processor-declared private scratch
bytes. Shared immutable Scope definitions do not merge these state slots.

The `processor_host` Module exclusively owns factory binding, occurrence state,
state entry, and main/auxiliary bus dispatch. `built_in_processors` owns native
definition validation, factories, and DSP instances; adding a built-in cannot
grow graph traversal or callback orchestration into a definition registry.
`processor_parameters` owns preallocated event-batch lowering. The `render`
Module owns graph traversal, PCM flow, summing, envelopes, and delay placement;
it neither reconstructs processor batches nor reaches into a processor's
mutable state. A failed entry consumes and poisons its new epoch before any
instance resets, so partial multi-processor reset can never resume old history.
At root entry, stateful Track/Bus/Output occurrences enter immediately because
their strips evaluate every requested block. A stateful Contribution occurrence
remains pending until its causal execution span first intersects a request,
enters at that exact sample, then receives contiguous zero-padded callbacks
through its declared tail. Pre-Clip silence is not synthesized as hidden
processor work. Once the source interval ends, the source Adapter is no longer
called; explicit zero input flushes the occurrence. Edit gain/pan/fades remain
after the Scope Rack by normative order, so a Clip fade-out gates any Scope tail
beyond the Clip while an unfaded edit preserves it.

The immutable Plan and recursive Runtime report
`public_output_lookahead_frames`, not public PCM latency. On the first non-empty
request of a fresh epoch, the Session evaluates that many internal frames in
bounded Render-Contract-sized blocks and discards their output, then returns the
requested block at its original Timeline coordinate. Later requests retain one
internal cursor exactly `lookahead` frames ahead of the public cursor. Meter
timestamps and consumer continuity remain public coordinates. The lookahead is
fixed admission/capacity evidence; it never rewrites author time, Clip
placement, automation coordinates, or nested source mappings.

Session construction allocates one fixed interleaved delay line for every
prepared Contribution and Route compensation input; zero-delay lines retain no
sample storage. It also allocates one interleaved Route-gain lane sized to the
maximum block. The capacity report exposes both obligations. Contribution
compensation executes after contribution-local processing and before the Track
sum. A Route first delays the selected source-port signal for compensation,
then applies its level at the destination's Sequence sample time while summing;
automation is evaluated at the aligned destination signal time rather than the
raw execution cursor. Neither path
allocates or grows during a block. Scalar/SIMD execution shares these same
state lines, and reference tests require whole-block and partitioned-block PCM
identity within the stated floating-point tolerance.
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

Offline export first lowers the exact half-open public Sequence interval through
`compile_audio_dependency_closure`. That deep Module consumes the compiled
Program Output, not Tracks or Clips, and returns routed media Components plus
the exact selected root/nested semantic Program occurrences. Each occurrence
is addressed by Sequence ID, public Output ID, and exact dependency window
including endpoint inclusion; a Sequence-ID-only cache is invalid because one
child may be selected through different Outputs or projected ranges. Reverse
and frozen nested source maps therefore retain enough endpoint evidence that a
boundary sample is neither dropped nor admitted from an adjacent Clip.
The closure's root Program is structurally required even when its selected
Signal Closure is `ProvenSilent`; exact silence is evidence, not an absent
Program.

Export queue admission retains that complete closure and compares pure
recompilation against it before publication. Worker audibility consumes its
root `AudioProgramExecutionDemand`; media-component presence is never an
audibility heuristic. `ProvenSilent` alone permits exact-silence substitution.
`RequiresExecution` includes processor-only Buses/Outputs, tails/generators, and
selected pre-mute sends even when the media Component set is empty.
`AudioProgramRuntime::build_from_precompiled_closure_for_range_with_resource_grant`
then prepares directly from those exact root/nested Programs before media
binding or resource reservation; it performs no second author compilation or
range selection. Consequently an off-range, post-mute-gated, disabled, or
unrouted source cannot fail a selected export or consume its source-window
grant. The ordinary `build` Interface remains the whole-Program contract used
by continuous Playback and consumes the same mute-resolved Signal Closure.

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

Every prepared Track, Bus, and Program Output has one post-mute meter target.
During a public render block the Session measures each target immediately after
its strip, before liveness reuse may overwrite the scratch slot, but stages the
result privately. Only complete success publishes the whole target bank under
one block serial and one seqlock; a later node failure can therefore never
expose a partially new graph. Hidden lookahead/entry priming does not enter the
public bank. A shared read-only `AudioMeterObserver` is obtained before callback
ownership transfers and allows control/analysis threads to read one internally
consistent owned `AudioMeterFrame`: exact Sequence sample range, layout, stable
target identity, and per-channel finite sample peak, RMS, over-full-scale count,
and non-finite count. Callback publication uses fixed atomic slots and allocates
and locks nothing; snapshot allocation is confined to the reader. A target
absent from the compiled Signal Closure is absent from the bank rather than
reported as false zero. This is deliberately not labelled true peak or loudness: EBU
R128/ATSC A/85 filtering, windows, gating, channel weighting, oversampled true
peak, hold/decay presentation, and offline normalization remain separate
versioned observation or processing stages.

Exact automation is evaluated in its owner domain and is invariant under block
partition. Timeline-to-sample conversion uses `AudioSamplePosition` with an
explicit rounding policy. Static source-time spans map the first sample exactly
and advance an `i128` rational accumulator across the block, including
fractional forward and reverse rates; they do not reconstruct general Timeline
values per sample. Export range boundaries are converted once from the Sequence
frame grid; chunks then advance integer samples only.

### Realtime execution performance contract

The realtime path uses a dense schedule, liveness-based scratch reuse, a scalar
reference kernel, prepared SIMD dispatch, affine source accumulation, and
precomputed automation event spans. Preparation validates each exact curve once,
lowers its event boundaries to the Evaluation Grid, and stores immutable core
segment evaluators; block execution advances a local span cursor and never
validates or searches the author curve per sample. Hold, Linear, and Bezier
semantics remain core-owned and are exactly block-partition invariant. Realtime
rendering does not scan author Routes or use tree/map lookup in sample loops.
Plugin ABI event lowering and additional state-entry obligations must deepen
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
machine execution writes exactly twelve unique JSONL records to the fresh
`MONDRIAN_AUDIO_LOAD_MATRIX_OUTPUT` path. A missing/duplicate workload, profile
mismatch, nonzero vectorized deadline miss, or vectorized p99 above the block
deadline invalidates the suite; stdout or a zero-test Cargo exit is not
performance evidence. Fixed-reference-
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
`AudioSourceCache`. It opens a stable identity containing the complete
`MediaFileFingerprint`, absolute stream selection, and native signal layout,
then decodes only aligned ten-second PCM windows. One cross-source weighted LRU
has online-reconfigurable entry and byte limits; the App resource decision
publishes explicit 8/16/32 GiB-class Playback limits and reapplies them when
complete product-process-tree or whole-system pressure changes. A lower limit synchronously removes ordinary
PCM LRU entries. An in-flight result observes the newest limit before
publication, so reconfiguration cannot interrupt or reinterpret a render.
Terminal decode failure memory is separately capped at 64 identities. Negative
and post-EOF coordinates are silence; arbitrary seeks request the containing
window. Resident bytes, effective limits, reconfiguration/trim totals,
hits/misses, decodes, failures, oversize windows, in-flight single-flight
leaders, and evictions are structured diagnostics. The concrete decoder uses
an independently reconfigurable LRU pool of isolated persistent FFmpeg child
Sessions. Idle sessions above a reduced capacity terminate immediately. Busy
sessions finish their current window and then converge; diagnostics expose the
temporary over-capacity count instead of pretending the trim already happened.
Sequential windows reuse continuous `f32le` output, while a non-contiguous miss
restarts only the matching fingerprint/output-contract Session with bounded
coarse preroll followed by output-side exact trim. Stdout look-ahead and
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

### Paused-playhead idle preparation

Speculative paused-audio preparation is a background execution domain, not an
`AppState` render helper. `app::audio_idle_warmup` owns exactly one sequential
worker and one bounded latest-demand slot. The main/event-loop thread may
submit only an immutable snapshot containing the open authoring-session
identity, Project and author generation, Asset Library database revision,
active Sequence revision, exact playhead sample, Sequence closure, Asset
Library handle, source-cache handle, and a copied
`AudioRuntimeResourceGrant`. Graph compilation,
`TimelineAudioPcmRenderer` construction, DSP execution, and decoded-source
access happen only on that worker.

Renderer construction exposes the compiler-owned
`AudioProgramExecutionDemand`. A `ProvenSilent` demand completes warmup without
issuing PCM blocks or media reads; `RequiresExecution` follows the ordinary
causal render chain. The event-loop Module never duplicates that decision with
a Track/Clip audibility helper. Realtime Playback consumes the same evidence:
only a demanded Program Output is attached to Audio Playback, while a proven
silent output leaves Synthetic Clock Master authoritative.

Each admitted snapshot freezes its hard Runtime grant; a later resource-policy
revision cannot reinterpret executing audio. `set_dispatch_enabled` and the
independent automatic-work policy have different authority: the former closes
physical execution and retains at most one pending demand so cross-domain
allocation can observe it, while the latter owns speculative admission and
cancels both pending and running work when disabled. Entering realtime Playback,
Critical pressure, explicit heavy work, closing/reopening a Project, advancing
the author generation, receiving a newer playhead demand, or shutdown cancels
queued work and cooperatively invalidates the executing token. A policy pause
does not retain speculative Runtime state behind realtime work.

Asset Library database revision is part of both duplicate identity and author
binding, not merely a diagnostic field. The worker verifies it before renderer
construction, immediately after construction, and before and after every
rendered block. A revision mismatch directly classifies the attempt as
`Superseded`, so a relink or other SQLite media-binding mutation cannot publish
preparation derived from the previous library state. If an authoritative
Project-binding rotation has already canceled that same attempt, its single
terminal record may instead be `Canceled`; either ordering forbids
`Completed`, duplicate terminal publication, and warm-state publication under
the new binding. A revision-probe error remains a typed execution failure
rather than being mistaken for equality.

The worker prepares only the current playhead's causal block chain. It renders
the available preceding 80 ms blocks in ascending sample order and finishes
with the block beginning at the current playhead; it never probes an arbitrary
future position. One render generation uses `Enter` on the first block and
`Continue` on subsequent contiguous blocks. Near sample zero, unavailable
negative predecessor blocks are omitted rather than clamped into duplicate or
future work.

Every admitted attempt has exactly one bounded terminal record:
`Completed`, `Canceled`, `Superseded`, or `Failed`. Diagnostics distinguish the
single queued slot, physical running identity, completion/cancellation/failure
counts, retention position, and latest terminal detail. The event-loop Adapter
only polls these records; speculative failure is diagnostic and never changes
Timeline truth, transport state, or the audio delivered for Playback/Export.

Owner destruction first closes admission and cooperatively cancels queued and
running work. It joins an already-finished worker directly, but never waits
without a bound for an unfinished worker on the owner/UI thread: ownership of
that join moves to a detached reaper, or the worker is detached if the reaper
cannot be created. This protects App teardown responsiveness; it does not claim
containment of a permanently non-cooperative in-process decoder or plugin.
Such third-party execution still requires a supervised process-isolation
Adapter before production support can claim a hard termination deadline.

## Playback boundary

Audio Playback, not the audio compiler, owns device lifecycle, render
generations, bounded in-flight work, watermarks, preroll, callback consumption,
underrun recovery, and device-clock evidence. The CPAL callback touches only a
fixed-capacity queue and atomics; it does not allocate, decode, compile, log,
inspect the Timeline, or lock a Session.

Construction is fail-closed. `AudioPlayback` is returned only after its owned
PCM render worker has spawned successfully; worker creation failure is a typed
creation error, not a permanently empty completion queue. The App may retain
video transport by installing an explicit `ExecutionUnavailable` audio runtime
and using Synthetic Clock Master, while preserving the creation reason for
diagnostics. The device lifecycle worker has separate start-failure evidence
from an ordinary `OpenFailed` device attempt. Both render and device workers
retain owned `JoinHandle`s. Shutdown first cancels current work, closes and
wakes admission, then joins each worker; a device `Lost` event is emitted only
after the concrete CPAL stream has been destroyed. An unexpectedly finished
render worker or disconnected completion channel transitions once to
`ExecutionUnavailable`, cancels and clears outstanding work, and admits no
phantom in-flight requests. Concrete stream-generation identities are issued
with checked monotonic allocation; exhaustion is a structured creation failure
and never wraps to a reused or zero identity.

The output queue has one non-cloneable Manager-owned producer handle and one
callback consumer. Enqueue validates exact sample rate, semantic channel
layout, complete interleaved-frame shape, and remaining sample capacity before
admitting anything. Because the callback only removes samples, a successful
whole-buffer capacity preflight proves every subsequent push; queued samples
are never evicted to make room and media time is never silently shifted. A
rejected complete buffer invalidates the Audio Playback generation and follows
bounded reprime/blocked recovery, even when its renderer otherwise permits
independent-window silence substitution. The playback chunk and high watermark
must fit the fixed two-second device queue at configuration admission.

Callback activation is revisioned per concrete stream. Deactivation returns a
checked `(stream_generation, quiescence_revision)` token, disables new active
blocks, and is acknowledged only after every active callback block that already
reserved the retired revision has completed. Revision exhaustion fails closed
and disables consumption; identities never wrap. A fresh render generation
cannot schedule or enqueue PCM until acknowledgement, and Playback clears the
queue again after acknowledgement before admitting new PCM. The exact queue
prefix discard, interval-counter reset, activation timestamp, and callback
enable transition execute under one control transition; an observer can see an
inactive pre-activation state or the complete new interval, never a partially
activated interval. Callback telemetry
therefore separates saturating diagnostic totals from the checked
`active_callback_consumed_frames` interval counter: inactive callbacks may
report callback activity but cannot consume queued PCM or advance media time,
and an active-counter overflow marks the stream failed without wrapping.

Device retirement follows destroy-then-observe ordering. The device worker
requests deactivation, drops the concrete CPAL stream, captures one final frozen
snapshot through a read-only observer, and only then publishes typed `Lost`
evidence. `AudioPlayback` pairs that loss with the exact last media
`AudioSamplePosition` for the matching stream generation and retains a fixed-size
lifecycle aggregate (opened/lost counts by typed reason, last generations, and
last frozen loss) after transient events disappear. With the `validation`
feature, the only public injection seam is an exact-current-generation
controlled recycle on `AudioPlayback`; it uses this same loss/drop/reopen path.
Production builds expose no fake device-loss event injection.

The worker's newer deactivation revision can become atomically visible before
stream destruction has finished and before the corresponding `Lost` event can
be published. `AudioPlayback` treats that strictly newer revision as a
superseded local quiescence obligation: it keeps PCM admission and callback
activation closed and waits for the frozen lifecycle event. It never reuses or
renews the old token. A mismatched stream identity, a non-increasing revision,
or any other control error remains terminal for the poll. This explicit
transient closes the device-retirement race without weakening callback
quiescence evidence.

Every realtime output snapshot carries the wall-clock `captured_at` instant at
which its counters and callback age formed one coherent fact. The callback is
the single non-blocking writer of one revisioned telemetry publication domain;
activation/deactivation is a separate revisioned control domain serialized only
among control threads. A reader publishes a snapshot only when both revisions
and both in-progress markers remain stable around all field reads. It records
`captured_at` after those reads, so a descheduled reader cannot pair a future
consumption counter with an earlier instant. Neither callback path takes a lock
or waits for a reader. Non-realtime observers retain the last proven immutable
snapshot; bounded read contention returns that older fact with its original
capture instant, allowing normal age/freshness policy to degrade authority
without fabricating progress or misdiagnosing device loss. Revision exhaustion,
overlapping same-stream callback writers, or contention before any coherent fact
exists marks the stream failed and publishes no invented clock progress.

The App maps that instant onto the Playback Engine's sole monotonic high-water
mark; poll start, event handling time, and Evidence time are not substitutes. On confirmed
physical loss, a matching current-generation frozen snapshot is optional
continuity evidence: the Engine applies it atomically when valid, but invalid,
stale, mismatched-rate, or mismatched-generation final evidence can never block
the mandatory handoff to Synthetic Clock Master. A stream that never became
Audio Device Master cannot acquire phase authority merely because it emitted a
loss snapshot.

The in-process render-worker join relies on the processor contract's
cooperative cancellation. This is a truthful lifecycle guarantee for built-in
processors, not a hard deadline for arbitrary third-party code. A production
plugin Adapter that must survive non-cooperative execution requires supervised
process isolation and bounded termination evidence before the product can claim
hard shutdown latency.

Each Audio Playback generation also owns a fresh monotonic
`ExecutionCancellationToken`. Reprime, seek, device recovery, source replacement,
and shutdown cancel the old token before incrementing generation. The token is
carried through `AudioPcmRenderer`, `AudioProgramRuntime`, nested Runtime calls,
`AudioDecodedSource`, and the media window Adapter. A canceled read returns
without admitting PCM or remembered failure into the shared cache. Export uses
the same Interface with its own live token; it does not inherit Playback state.

The App transport boundary is the sole Timeline-to-audio lowering authority. It
converts exact author time to one `AudioSamplePosition` at the configured rate
and explicit rounding policy; every Media-side prepare, clear, reprime,
validation, poll, restart event, and media anchor then carries that sample
position unchanged. Media never re-lowers `FramePosition` or `TimelineTime`.
Wrong rate, negative realtime anchor, output rate/layout contradiction,
unrepresentable sample coordinate, generation overflow, and sample-cursor
overflow are structured errors checked before Playback mutation; none becomes
sample zero, saturation, or a different phase.

Reprime renders continuously from its exact event-time sample while output is
inactive, so stateful DSP executes the hidden interval. Each poll compares the
current exact authority with that immutable render anchor. A negative delta
waits without unsigned conversion. Activation is permitted only after callback
quiescence and after the physical queue contains `elapsed_skip + preroll`; the
exact expired prefix is discarded, the current authority becomes the new media
anchor, and consumption starts. Admission may grow toward
`elapsed_skip + high_watermark` but is capped by physical queue capacity.
Overflow, skip-plus-preroll beyond capacity, or exact-discard shortage fails
closed and leaves output inactive under Synthetic Clock Master.

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
the 460 ms output high-water duration, the effective product source-cache budget, and
the shared `product_process_tree_private_commit_v2` plateau contract. Every
sample must prove a complete Mondrian root-plus-descendant inventory, so the
persistent FFmpeg audio child Sessions contribute to the 4 GiB cap and settled
growth rather than disappearing behind a current-process-only measurement.
Missing environment
fixture, output device, callback facts, native memory facts, or presentation
facts fail rather than skip.

The observation begins only after the same stream generation, fresh callback,
Active Audio Playback, and Audio Device Clock Master remain continuously
qualified for one second. A bounded 30-second tail may extend execution until
both the current uninterrupted callback interval and Playback Evidence cover
30 minutes; it never reduces either acceptance duration.

This gate proves a concrete OS output stream consumed the production PCM path;
it does not claim acoustic loopback or speaker-waveform verification. The
professional report keeps cold open, sequential reuse, and random restart
as separate evidence classes and applies the playback high-water rule only to
qualified sequential reuse. Local development observations are diagnostics and
do not belong to this architecture contract or establish a reference-machine
baseline.

## Correctness invariants

- A valid snapshot has a closed Track/Channel/Scope/Route/Transition graph.
- Every audio Clip has at least one Component Edit; every edit's source kind
  matches its owning Clip kind.
- Track Channel keys exactly equal audio Track IDs.
- Placement and source mapping come only from Track/Clip.
- A Transition's full range lies inside both endpoint Clips.
- Route cycles, duplicate strong IDs, unknown Roles, and missing ports fail.
- Unresolved processors fail closed and preserve author data.
- Intentional audible delay is never reported as PDC-compensable implementation
  latency; the two semantics require distinct evidence.
- Processor-private Session storage is admitted during preparation against the
  explicit Render Contract budget and rechecked at Session construction.
- Block partition cannot change PCM results.
- Internal summing has no implicit nonlinear operation.
- Playback and export compile and execute the same Program semantics.
- Native decoded PCM cache identity is independent of Clip channel mapping.
- Every explicit matrix targets the owning Sequence layout; nested matrices
  also name the exact child public-output layout.
- Delivery layout adaptation occurs only after the selected root Program Output.
- Nested instances do not share mutable Session state.
- Old-generation cancellation is observable inside executing source work and
  cannot poison decoded-window success or failure residency.
- Playback and Export media sources are bounded by bytes and source revision;
  neither may retain whole-file PCM as its execution Interface.

## Verification gates

The automated suite must prove:

- compiler placement derives from the real Track/Clip hierarchy;
- invalid unscoped audio authoring and Bus cycles reject validation;
- present zero-length or over-Clip fades reject author validation, while the
  product zero control emits canonical absence;
- Clip/Track/Bus/Output gain and automation are block-invariant;
- Clip static gain and edge fades are sample-identical between the normative
  scalar and runtime-vectorized prepared schedules;
- internal PCM is not clipped or `tanh`-shaped;
- Track mute gates the post-mute route;
- unresolved VST3/CLAP instances survive semantic IR and fail closed at default
  plan preparation;
- a custom stateful non-zero-latency factory drives Host instantiation, PDC,
  continuity entry, realtime-mode admission, and partition-invariant PCM;
- built-in Sample Delay validates exact integer authoring, keeps audible delay
  outside PDC, resets on seek entry, remains partition invariant, and fails plan
  preparation when its exact Session storage exceeds the Render Contract;
- built-in Lookahead Limiter persists and reopens its canonical millisecond and
  decibel schemas, rounds Lookahead upward once, drives real parallel-route PDC,
  keeps Ceiling automation on signal time, links channels, resets on seek,
  rejects non-finite PCM, and fails before Session construction when exact
  scratch is one byte over budget;
- the optimized monotonic peak window matches an independent window-rescanning
  scalar reference across irregular block partitions, runtime SIMD gain
  application, and a non-zero fresh seek entry;
- Track/Bus/Program Output sample-peak/RMS observation publishes one complete
  block-atomic bank, preserves unclipped and non-finite evidence, and never
  exposes hidden priming or claims loudness/true-peak conformance;
- a root audition request retains only selected Track program sources, while
  nested public outputs remain canonical independent Runtime instances;
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
- a separate ignored linked-limiter matrix installs one real stateful limiter
  on each of 1/8/32/64 Track Mixer Channels, enters an explicit continuity
  epoch, compares scalar and SIMD PCM at 64/256/1024-frame blocks, and fails
  when runtime-vectorized p99 exceeds the realtime block deadline;
- decoded-media block reads native-layout PCM across aligned windows exactly,
  reuse hits, evict by global PCM bytes, and invalidate after file replacement;
- standard mono-to-stereo, explicit stereo swap, and child-mono-to-parent-stereo
  paths execute through the shared prepared matrix boundary; scalar/reference
  matrix results are sample-identical;
- a manual external AAC parity gate compares a cold window, sequential boundary,
  and evicted-window random restart against one sequential reference decode;
- a manual real-child cancellation gate requires kill/wait/join observation
  within 50 ms and zero success/failure cache admission;
- concurrent identical window misses elect one decode leader and one follower
  cache hit;
- deterministic professional-audio acceptance tests reject missing/fake output
  facts and accept a complete versioned CPAL/A/V/memory evidence set; the real
  30-minute Adapter is ignored and never treats an absent fixture as a pass.

## Professional audio qualification

Remaining product work and sequencing live in [ROADMAP](../ROADMAP.md), not in
this architecture contract. Professional support cannot be claimed until the
versioned acceptance plan covers layout/device/encoder negotiation, isolated
plugin-host lifecycle and failure containment, sidechains and standards-based
metering, authoring UI/Undo/reopen, reference PCM and export parity, persistent
decoder locality/cancellation, and reference-machine realtime load, drift, and
memory evidence.

Those capabilities must deepen the same compiler, prepared schedule, Host, and
Runtime. Unknown layouts or dependencies continue to fail closed; no item may
be closed by adding only schema, a disconnected UI, or a consumer-specific
fallback mixer.
