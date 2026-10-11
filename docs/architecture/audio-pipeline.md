# Audio pipeline

## CLAP render admission and symbolic thread roles

The CLAP discovery request includes the consumer's explicit realtime/offline
mode. After restoring native state while deactivated, discovery and execution
use the same render-extension admission helper. An optional extension is set
to the requested mode before activation; hard realtime requirements and declined
offline selection are refused. A plugin without that extension uses its default
realtime algorithm for either consumer. The resulting execution contract admits
only the actually probed mode, and activated latency/tail are validated again in
the prepared worker. Realtime and offline may legitimately have different
latency/tail; no mode-independent scheduling facts are inferred. Definition
identity and authored opaque state remain independent of the execution mode.

The isolated worker implements CLAP's thread-check extension with one fixed
main OS-thread identity and an instance-owned symbolic audio-role guard.
Reset, tail queries, start/process and stop are the only marked audio call
intervals. The same OS thread may correctly report both main and audio during
those intervals. Other instances and plugin-created background threads do not
inherit the role. Main callbacks, activation, state, ports, latency,
deactivation and destruction execute outside it. Borrowed RAII guards restore
the enclosing role after errors and unwind without locks or per-call allocation.
The guard is neither Send nor Sync, so it can only be released on its owner
thread. Nested markers preserve role state; they do not detect processor reentry.
Stop completes inside the audio role before main-thread deactivation.

Owned fixtures define independent realtime/offline gain and delay oracles,
hard realtime and declined-mode refusals, lifecycle thread identities,
partition/reset behavior and failure teardown. Existing sidechain states 0–2
retain their original ports, gain and three-frame delay; state 3 selects an
offline quality mode with a distinct gain and five-frame delay. These fixtures
exercise the named lifecycle contract. Full native GUI/MIDI/transport/vendor
and measured native realtime performance remain separate acceptance work.

Clean source `9f0183a` qualifies stage-aware main/auxiliary compensation with
122 source audio regressions, actual CLAP/VST3 late-detector Undo/save/reopen
PCM and three complete Golden rounds. The native late detector increases public
lookahead from 3 to 6 without moving author-time sound. Full evidence and scope
limits are in [modal/resource/output acceptance](../dev/modal-resource-lut-acceptance.md).
Broader layouts and complete Host/Scope product interactions remain separate
obligations.

Native auxiliary input names are bounded preparation metadata, keyed separately
from the execution bus contract. CLAP reads restored deactivated audio ports;
VST3 reads the validated activated bus metadata. The worker's exact version 2
ready handshake carries names once, validates membership and unique keys, and
keeps strings out of block commands, PCM layout and execution signatures.
Explicit control inspection returns those prepared facts without creating DSP.
The separate name accessor is called only by explicit inspection; normal factory
resolution does not call it. Native preparation still captures names once from
the bus metadata it already validates.

Channel execution is lowered into ordered pre-fader and post-fader stages.
The pre stage sums inputs and runs Scope/trim/pre racks, then immediately
publishes pre-fader sidechain taps. The post stage applies the fader/post rack
and mute, then publishes final taps and meters. The same causal stage order
solves latency; main-route order provides stable node storage only.
Scratch liveness spans stage positions, all main-route consumers and the public
output copy. Reciprocal pre-to-post dependencies and same-strip pre-to-post
connections are causal; final-tap feedback remains rejected. Preparation owns
stage sorting and storage; processing allocates no stage graph or buffers.
At each processor, main and all connected auxiliary arrivals align to their
maximum. Session-owned fixed main/auxiliary delay lines propagate that arrival
through Scope racks, contribution sums and exact port taps. Their storage and
lookahead count toward existing admission budgets and reset on every epoch.
Partial Scope blocks read the matching source slice; compensated warmup cannot
publish audio before the Clip start. Native and wider layout qualification remain
separate from source regression evidence.

The VST3 adapter uses a selected-plugin catalog, a deadline-bound metadata
child, and a distinct supervised audio-worker mode. Its authoring schema keeps
continuous parameter values normalized to `0..=1`; stepped parameters use
integer indices `0..=step_count` and hold interpolation, then normalize at the
ABI boundary. Discovery and worker both reject unsupported multiple main buses, layouts,
mode changes, metadata drift, and latency/tail drift. Same-main-layout auxiliary
inputs now preserve native bus slots through discovery and preparation; the
owned stereo Float32 packaged/native sidechain qualification passed at b5eb715.
Mixed layouts and broader vendor/platform qualification remain open. Worker
continuity entry releases the previous class and reloads the exact class from
the saved opaque state, while sample-offset points enter the VST3 process queue.
A reference Gain DLL built from the permissively licensed `vst3-rs` example
passed isolated realtime and offline stereo signal tests with gain `0.5`, plus
a fresh continuity entry with gain `0.25`. The same installed bundle passed
the App Preview and Export offline delivery paths on nonzero stereo Float32
PCM: both produced `+0.125/-0.125` from `+0.25/-0.25` with gain `0.5` over
the first 64 frames (absolute tolerance `1e-6`).
A separate stateful Gain fixture under
`crates/mondrian-audio/tests/fixtures/vst3-stateful-gain` verifies the current
host state envelope across component and controller state. Its ignored
integration test restores a nondefault gain, captures identical state, and
renders through an isolated worker without parameter events after two
continuity entries. Broader vendor Preview/Export parity remains a qualification
gate.
The current control frame limits serialized VST3 state to 128 KiB; larger state
is rejected explicitly. Third-party host/license notices are in `LICENSES/`.
The App routes selected VST3 classes through the same resolver used for
Preview, Export, and idle warmup; Inspector and Mixer use one shared Rack
projection. Explicit format/path selections are machine-local UI preferences
restored in one background catalog job, while `.mdp` retains only the VST3
class, vendor, exact binary fingerprint, parameter curves, and opaque state.
Selected `.vst3` directories resolve exactly one regular native binary under
the current architecture's `Contents` directory. The catalog fingerprints
that binary and rejects missing or ambiguous bundle layouts. Preferences keep
the selected directory, so startup resolution follows the same rule; an
individual native file remains a valid explicit selection.
VST3 worker startup is bounded to 15 seconds and continuity entry to 10
seconds; the realtime block deadline stays at 100 milliseconds. Those bounds
are separate because plugin construction and state restoration can be much
slower than block processing.

The isolated CLAP adapter has a child-side implementation in
`mondrian-audio::processor_isolation::clap_worker`. It runs only through the
application's hidden audio-worker entrypoint. The parent supplies an exact
installed-definition registration and schedules through the existing isolated
processor transport. The child revalidates CLAP descriptor, main audio ports,
latency, and tail, restores supplied opaque state before activation, resets
processor history at continuity entry, and interleaves Float32
blocks at the ABI boundary. The probe captures bounded CLAP parameter metadata;
the parent requires each authored schema to match the selected binary and the
worker checks the same metadata after state restoration. Stable definition-local
IDs map author curves to CLAP IDs. The worker converts exact sample-offset
events to CLAP value events in a preallocated, time-sorted buffer and rejects
out-of-range or fractional stepped values. It stops and deactivates the native
instance when the worker closes; a restart request fails the current instance.
New insertions retain the validated plugin parameter name and hidden flag as
display-only author metadata. Hidden parameters remain in the definition and
execution lane but ordinary Inspector controls omit them. Reopening without
the binary keeps the last captured names; parameters with no captured display
name show their stable IDs.
Selected-plugin instance creation captures the current values after restoring
the same opaque state, so a first-block parameter event does not reset a saved
setting to the plugin factory default. When no state is supplied, insertion
also saves the plugin's factory state in the discovery child, bounded to 64 KiB,
and persists those bytes on the author instance. A plugin without the CLAP
state extension remains parameter-only; a failed or oversized save rejects the
insertion. Auxiliary buses and subsequent plugin UI state edits remain pending
format work;
unsupported requests return typed errors, not silent bypasses.

CLAP descriptor discovery and execution-contract probing use a separate hidden child mode. The editor
process writes a bounded request into a private temporary directory, starts
the child with a five-second deadline, and reads at most 512 KiB of validated
metadata and optional captured state after successful exit. Plugin code is
loaded only in the child.
The probe restores the same author state, validates a Mono/Stereo main port,
and measures latency/tail for the requested rate and block extent. The
subsequent audio worker rechecks those facts. Explicit library selection is
available through the App's native file picker and session catalog. Inspector
and Mixer offer installed definitions; insertion probes the processor in the
child before the existing Rack author transaction.
An `InstalledClapAudioProcessorSpecResolver` supplies an App-shareable,
mutable backend catalog. Library scans happen before the write lock and commit
all descriptors from one selected path together; failed scans and cross-path
plugin ID collisions preserve the previous snapshot. Preview/Export preparation
copies one snapshot and retains the existing binary-revision checks. Selected
paths are persisted in bounded machine-local UI preferences. The Host starts
background restoration after its startup ownership checkpoints; each library
is re-discovered independently, so an unavailable binary cannot prevent
subsequent libraries from restoring. The App refreshes an active audio Program
when a restored definition becomes available.
The selected binary is fingerprinted with bounded SHA-256 reads before and
after descriptor discovery and contract probing, then verified again in the
processing child before native loading. A replaced binary fails admission.
Insertion captures the installed binary SHA-256 in the authored CLAP definition.
The shared resolver requires that exact revision for both Preview and Export;
explicitly unbound definitions (`binary_sha256: null`) remain editable but do
not execute until the user explicitly rebinds them. The alpha format requires
the revision field in every native plugin definition.

The public Runtime builders for an audition compile request and for a frozen
selected-range dependency closure accept an explicit processor resolver. Each
builder passes it to every nested Sequence occurrence. The App binds one
shared resolver to Preview, idle audio warmup, and its Export queue. Export
uses that queue resolver for normal audio and each stem, including the initial
aggregate resource admission pass. Default construction still uses built-ins
and rejects unresolved external definitions. An explicit rebind preserves
instance author state after probing the selected binary and comparing parameter
schemas; installed-plugin management remains product work.
The ignored installed-reference tests render a nonzero constant Float32 stereo
WAV through the App Preview adapter and Export's offline delivery adapter.
Both must return left/right samples of `+0.125/-0.125` for the saved 0.5 gain
state, within `1e-6`. A shared Runtime test also compares realtime two-block
and offline one-block output sample by sample. This qualifies that reference
gain contract and catches bypass, channel inversion, mode or block-boundary
drift; other plugin algorithms still require their own signal corpus.

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

The signed-rate product Action may expand a complete linked video/audio Clip
group and replaces every member's canonical source map in one author
transaction. Audio compilation lowers either direction through the existing
dense schedule with the map's exact covering/strict-predecessor boundary;
placement duration and Component Edit origins do not move. A direction change
anchors the same half-open source span at its old exclusive terminal boundary.
Picture hold is video-only. Audio is never converted to a zero-rate
single-sample hold as an incidental consequence of a picture operation.
The focused Retime Hero gate recompiles the persisted linked audio Clip after a
durable Project reopen, prepares the complete routed Track closure into the
dense schedule, and checks the exact source target there. Its metadata-only
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

The Inspector projects the canonical logical source of each
`AudioComponentEdit`. Media Clips offer Asset Component IDs; nested Clips offer
only the child Sequence's stable public outputs. Source selection and field
edits both enter one complete `AudioComponentEditRequest`, addressed by
`(TrackId, ClipId, AudioComponentEditId)`. Its closed mutation algebra covers
source, enabled state, static volume, static pan/balance, fade-in, fade-out, and
the complete channel-mapping policy. It never round-trips or replaces the
complete edit, so an interaction cannot overwrite unrelated automation, Role,
or processing state.

The Timeline Interface resolves the exact Track/Clip/Edit ownership, admits
the Track lock, validates media-versus-nested source kind and duplicate source
selection, rejects static volume or pan while its curve is signal authority,
and validates the complete Audio Program candidate before replacement. The App
audio-authoring Adapter adds only recoverable dependency proof: the current
Asset catalog must expose a requested media Component, or the Project-contained
child Sequence must expose a requested public Output. Neither UI nor the media
Adapter mutates Timeline storage. Matrix replacement is atomic; a partially
edited or non-canonical matrix never enters an immutable snapshot. Rejected and
no-op actions advance neither revision nor history. Successful source or field
changes are one undoable transaction and invalidate the prepared Runtime at the
current transport anchor; running playback re-prepares, while paused/stopped
playback releases the stale source. The former Inspector-specific source
payload and dispatcher do not coexist with this Interface.

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

`app_ui::audio_component_mapping` is a shallow product Adapter over that deep
Interface. It resolves current source-layout evidence only from an exact Asset
Component binding/probe match or the selected child public output; channel
count alone is never speaker semantics. The Inspector shows source and
Sequence destination layouts plus every non-zero coefficient. `Standard`
remains a versioned execution policy and is reviewable as its currently
resolved matrix without being silently persisted. A deliberate custom choice
materializes either that reviewed standard matrix or an exact empty matrix;
coefficient edits carry only the inspected source/destination layouts, channel
pair, and gain. Timeline rejects stale layout guards, rebuilds the entire
canonical sparse matrix inside the candidate, and commits once. This keeps a
64-channel editor from cloning one full matrix into every menu action.
If current evidence is unavailable or differs from an existing explicit source
layout, the author matrix is preserved and the product reports that execution
will fail closed instead of retargeting the user's intent.

The prepared Contribution, not FFmpeg or UI, owns the resolved matrix and native
source layout. It reads native interleaved PCM into one Session-preallocated
maximum-source scratch region, then maps into the Sequence layout before Clip
Scope processing, volume/pan, fades, Transitions, and Track summing. Nested
Sequences render their public output in the child Sequence layout and cross the
same matrix boundary at the parent Component. A child can no longer be silently
re-rendered in the parent's layout. The selected root Program likewise renders
in its authored Sequence layout. `AudioProgramDeliveryRuntime` is the sole
post-output execution Module for Playback and Export: it owns the Program
Runtime, maximum-block Program scratch, prepared standard matrix, and immutable
`AudioDeliveryEvidence` containing Program layout, target layout, mapping kind,
and coefficient count. Playback retains generation/watermark/device scheduling;
Export retains offline scheduling and file I/O. Neither may render Program PCM
and apply its own matrix. A compiler-proven silent Program uses an explicit
zero-coefficient `ProvenSilence` delivery rather than requiring a meaningless
custom-layout conversion. For audible Programs, unsupported device/export
pairs fail with both layouts instead of changing Program semantics.

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
- explicit `AudioProcessorSidechainRoute` values;
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
reachability from the same complete retained author graph as final validation.
Main and processor sidechain edges include disabled connections; processor
ownership covers both Rack stages on Track/Bus/Output and every Track binding
a shared Scope. Scope bindings and processor ownership are indexed once. Mixer
and patchbay Adapters use principal and processor addition queries to omit
impossible choices without owning another cycle algorithm or treating this
projection as transaction authority. Native auxiliary keys/layouts remain
processor preparation facts. The Mixer regression checks that feedback via a
retained sidechain disappears while valid parallel and output choices remain.

Mixer Bus deletion uses `inspect_audio_mix_buses` for all connection counts and
disconnect lock blockers. The same domain implementation admits individual Bus
removal, visits main and sidechain routes once, includes disabled retained edges
and both pre/post-fader target processors, and counts a route once per incident
Bus. The UI no longer maintains its own main-route-only deletion policy. Invalid
dependency addresses disable deletion with a localized reason. These inspection
facts do not change the existing author transaction, history or DSP execution.

`inspect_audio_processor_inputs` is the non-realtime control query for one exact
author processor. Its closed target selects a channel-strip Rack or a named
Scope with an explicit Component occurrence. Timeline resolves the canonical
Rack and verifies the Scope binding; Audio translates its occurrence and sends
the actual definition, parameters, opaque state and Render Contract through the
same prepared-factory resolver as execution. Ordered auxiliary keys/layouts and
mode/latency/tail facts are captured without creating a mutable render Session.
Locked authors remain inspectable. Rate/block admission is shared with prepared
plans; adapter panic, mode and auxiliary-contract admission use the existing
processor preparation seam.

Product controls must run this query on a bounded non-realtime worker, publish
only results still belonging to the current author target, and invalidate stale
facts before routing edits. The query is not a per-frame UI call or persistent
author bus schema. Full-Program resource admission and canonical routing
transactions remain required. Sidechain source/disconnect widgets and their
asynchronous product integration remain subsequent work.

This main-edge contract covers dry paths, pre/post-fader sends, submixes,
stems, and multiple parallel paths. A sidechain is a separate typed edge from
one Track/Bus strip port to `(AudioProcessorInstanceId, bus_key)`. It retains
the same enabled/static/Sequence-time gain controls and strong Route identity,
but can never become a Bus or Output summing destination. Complete author
validation resolves the target processor (including shared Scope occurrences)
and rejects instantaneous cycles across both edge families, even while an edge
is disabled. Routing edits, Bus disconnection, Track removal, structural Scope
compaction, automation, and Sequence identity forking preserve or remove every
strong reference atomically.

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

`AudioAutomationEditRequest` performs stable-ID key upsert, key removal,
interpolation-mode change, or explicit clear-to-default. It admits Track and shared-Scope locks before
copy-on-write detachment, mutates a complete candidate, validates the complete
Audio Program, and publishes atomically. Moving a key preserves its ID,
interpolation, and manual Bezier handles; constrained handles are recalculated.
Collisions at the same exact owner time fail
closed. Optional semantic curves canonicalize to their static value after the
last key is removed, while Processor parameters retain their intrinsic empty
curve because that curve also owns the unkeyed definition value. Channel Strip,
Routing, and Rack Modules retain only their static/topology operations, so no
second keyed write path can drift from this contract.
The underlying `ExactAutomationCurve::set_keyframe` also validates a detached
candidate before replacing the curve. Invalid handle spans or duplicate stable
IDs therefore cannot leave even an in-memory curve in an invalid state.
Exact numeric keys persist a manual, auto, or continuous tangent constraint;
older projects default to manual. Auto uses monotone neighboring slopes and
flattens local extrema, while continuous keeps both sides at one slope. Key
insertions, moves, removals, and exact-time edits recompute constrained handles
before validation. The UI exposes the modes on keyed audio curve points, and
the same curve is evaluated by realtime playback and offline export.

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
- processor auxiliary-bus declarations negotiated against the exact Render
  Contract, per-occurrence sidechain bindings, source-port snapshots, and
  bounded gain/PDC scratch;
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
is solved on the existing causal stages, aligning every processor's main and
connected auxiliary inputs before adding its declared latency, and resolved at
every contribution/route summing point;
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
Each realized Factory publishes an immutable auxiliary-input contract alongside
its latency/tail contract. Preparation rejects duplicate/invalid keys, layout
drift, an authored key absent from the realized definition, a dependency cycle,
or a detector path that cannot be aligned to the processor main-input signal
time. Unconnected declared buses are explicit silence; multiple Routes to one
bus sum through preallocated gain/PDC storage. The callback exposes a combined
disjoint main/auxiliary borrow, so a processor can modify main PCM while reading
its detector without copying, allocating, locking, or performing a map lookup.
Every untrusted Resolver, Factory, and Processor trait call is also an unwind
boundary. A panic during resolution, contract query, instance creation, state
entry, or block processing becomes a typed `AdapterPanicked` failure. State
entry/block failures mark the exclusive hosted instance poisoned before the
error leaves the Host; stateful outer Sessions poison their continuity epoch,
while a failed stateless instance refuses every later callback and requires a
new Session. This protects Mondrian from Rust unwind across the Host Interface,
but it is not crash, access-violation, hang, or deadline isolation. VST3/CLAP
remain unavailable unless a concrete out-of-process Adapter supplies those
stronger guarantees; the built-in resolver continues to fail them closed.

The `processor_isolation` deep Module now supplies that generic out-of-process
Adapter boundary without changing the Processor Host Interface. One immutable
Worker Spec binds the helper executable, opaque native preparation payload,
exact Render Contract, plugin latency/tail/mode/scratch facts, auxiliary buses,
stable Parameter-lane order, and startup/block deadlines. Preparation performs
a real child launch and handshake; Session creation launches a fresh persistent
child for every mutable occurrence and revalidates the same contract again.
Built-ins remain local, while `IsolatedAudioProcessorResolver` can dispatch only
external definitions through a concrete discovery/ABI Spec resolver.

Parent and Worker map one fixed temporary backing object sized from maximum block,
channel layout, auxiliary-bus count, and worst-case sample-accurate Parameter event
count. The render worker copies PCM/events into that preallocated region and sends
only a fixed 32-byte command over a private loopback control connection. It never
creates a process, opens a file, grows a container, or writes an unbounded PCM pipe
inside block execution. The child validates every range before exposing borrowed
views to its native Adapter. Output PCM is copied back only after one valid success
response; partial or failed native output is never published.

The parent owns the hard operation deadline. Timeout, disconnect, malformed bounded
protocol, Worker-reported failure, panic, abort, access violation, or contract drift
poisons the exclusive instance, terminates and reaps its exact process, and requires
a new Session/Worker. Worker shutdown is bounded as well. This keeps plugin execution
off the physical audio callback: Playback consumes the existing ahead-rendered PCM
queue, while Export uses the same Audio Program and isolated Processor semantics.
The boundary is failure containment, not a hostile-code security sandbox; native
VST3/CLAP discovery, ABI loading, vendor UI, and format qualification remain concrete
Adapter responsibilities and may not be inferred from the generic Worker protocol.
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

The PCM24 audio-stem artifact selects every public Program Output in authored
order at capture time. Capture retains one independently compiled semantic
closure per Output, freezes its stable ID and display name, and unions the
reachable Sequence/media identities only for external dependency resolution.
Execution never derives stems by soloing Tracks or by mutating one master mix:
each stem is rendered from its own frozen root through the same compiled Audio
Program Runtime used by mixdown. Every WAV is independently probed for exact
PCM codec, semantic layout, sample rate, and rational duration, then hashed.
Per-output loudness/true-peak evidence and immutable identity are written to a
versioned manifest before Storage atomically publishes the complete sibling
directory. Cancellation, a missing output, or one invalid file rejects the
whole artifact; no partial stem package is visible at the destination.

All stem Runtimes are retained only after a measurement pass proves their
aggregate occurrence, fixed-residency, and prepared-plan footprint fits the
single job grant. PCM execution is block-interleaved across outputs, with blocks
split at the shared source-cache window boundary, so every output consumes the
same fingerprinted decode window before bounded LRU pressure can advance to the
next window. One job-scoped `AudioSourceCache` is reused by ordinary mixdown,
professional delivery, and every stem; it is never recreated per output. Cache
entry/byte limits remain hard policy, so an exceptionally undersized grant may
still evict and re-decode rather than exceeding residency, but normal multi-stem
execution does not scale source decoding linearly with output count.

Offline export enters one fresh epoch at its exact sample-range start. Realtime
Playback carries its generation on every PCM work request: the first admitted
window is `Enter`, later windows are `Continue`. The Timeline Adapter maps the
Playback generation and rejects duplicate/missing entry or generation mismatch.
`AudioProgramDeliveryRuntime` owns the exact submitted-window cursor after entry,
including stateless Programs. It checks the signed start and checked extent only
after the common cancellation and whole-root native preflight; native holds and
wrong-start refusals consume no window. Unentered stateless direct execution
retains arbitrary window evaluation. This delivery cursor is not a private DSP
position or checkpoint. An admitted ordinary independent failure consumes its
exact submitted window so Media can replace it with equal-duration silence and
its already queued next window can continue; it does not poison stateless DSP.
Decoded source windows publish their optional cache start only after a complete
successful read. A replacement read invalidates the prior window before touching
its storage, so an error or cancellation cannot expose partial or zeroed PCM as
a cache hit for a later independent window. The prepared cache capacity stays fixed.
The Adapter declares `GenerationState` whenever its
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

Prepared source spans retain the Clip map's `SourceSamplingBoundary`. Dense
schedule lowering uses floor for covering targets and exact
`ceil(source_position) - 1` for strict-predecessor reverse targets, including
fractional rates; it never subtracts one output sample or a floating epsilon.
Zero-rate holds retain the captured boundary and therefore repeat the same
resolved source frame. Stateless nested audio may consume signed indexed
demands freely. A stateful nested reverse occurrence remains a typed admission
failure until checkpoint/replay or processor-specific reverse state proves
correct continuity; the compiler must not turn that blocker into silence.

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

Offline Program delivery now passes the exact rendered floating-point PCM
through a separate `AudioLoudnessAnalyzer` observation Module before the encoder
Adapter sees it. The analyzer applies BS.1770 K-weighting, 400 ms blocks at
100 ms steps, the EBU R128 -70 LUFS absolute and -10 LU relative gates, and a
four-times windowed-sinc true-peak reconstruction. Its immutable report retains
integrated/latest momentary/latest short-term loudness, linear/dBTP maximum,
complete-block count, and exact observed sample-frame count in Export job
diagnostics. Proven-silent Programs produce explicit digital-silence evidence;
analyzer construction, non-finite PCM, partial interleaved frames, or finalizing
failure aborts the Export instead of omitting evidence. The qualified weighting
matrix is deliberately limited to Mono, Stereo, 5.1(side), 5.1(back), and 7.1.
Discrete, custom, and immersive speaker sets fail closed until a normative
weighting table and conformance corpus are admitted. This Module never changes
PCM and is not placed on the realtime callback. Audio-stem export constructs an
independent analyzer for every frozen Program Output and persists every report
inside the package manifest; the primary report also remains available through
the common job diagnostics Interface.

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
Sessions. Idle sessions above a reduced capacity enter bounded asynchronous
retirement immediately; their physical permits remain occupied until child and
pump closure. Busy sessions finish their current window and then converge; diagnostics expose the
temporary over-capacity count instead of pretending the trim already happened.
Sequential windows reuse continuous `f32le` output, while a non-contiguous miss
restarts only the matching fingerprint/output-contract Session with bounded
coarse preroll followed by output-side exact trim. Stdout look-ahead and
stderr retention are byte-bounded, and generation cancellation kills, waits,
and joins the process and pump threads. Linked in-process decoding remains a
replaceable Adapter choice rather than a different source contract. Decode
runs only on audio render workers, never on the callback or UI thread. Native
child/pump construction uses a separate prebuilt Media startup lane so an
uninterruptible OS spawn cannot hold the canceled read waiter. Cancellation
before or during construction preserves partial ownership for the independent
teardown lane; the physical permit is released only after actual closure.
Startup completion envelopes retain producer leases through install/retire,
including channel disconnection. This changes lifetime scheduling, not PCM,
seek, channel mapping, or the Runtime's generation/publication authority.
SourceCache schema 6 reports that startup inventory independently from its
unchanged one-worker teardown contract. Logical responsiveness is not proof
of the separate 50 ms physical-retirement qualification target.

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
underrun recovery, and device-clock evidence. Every physical-output callback
touches only a fixed-capacity queue and atomics; it does not allocate, decode,
compile, log, inspect the Timeline, or lock a Session.

`audio_device` is the sole Realtime Audio Output Negotiation Module. Runtime
selection is an app preference, never Project or Sequence authoring. It is
either `SystemDefault` or one exact, bounded CPAL stable `DeviceId`; a missing
specific device remains the selected intent and produces a typed unavailable
failure rather than silently falling back. On every open/reopen attempt the
Module resolves that intent and enumerates the selected device's
`supported_output_configs`; it never combines a default configuration's sample
format with an unproven requested rate/channel tuple. Candidate
selection requires the exact requested channel extent and a range containing
the exact requested sample rate, then deterministically prefers F32/F64 and
signed higher-precision integer formats before lower-precision/unsigned
formats. The callback Adapter supports every scalar format represented by the
current CPAL Interface and clamps only at this physical sink conversion; common
float Program execution remains unclipped upstream.

CPAL exposes channel count but no portable speaker-position evidence, and its
WASAPI `WAVEFORMATEXTENSIBLE` lowering deliberately uses `DIRECTOUT` (zero
speaker mask). The portable path therefore admits versioned Mono and Stereo
device conventions plus explicit ordinal Discrete layouts. Windows named
layouts whose positions fit the normative 18-bit Windows speaker set use a
separate Media-owned WASAPI Adapter. It reopens the exact CPAL endpoint identity,
derives the mask from the canonical `AudioChannelLayout` order, and initializes
an event-driven stream with that exact `WAVEFORMATEXTENSIBLE` mask. Shared mode
uses F32 at the Program rate with the Windows audio engine's explicit
high-quality conversion flags. Exclusive mode first probes the Program rate
across F32, PCM24-in-32, packed PCM24, PCM32, and PCM16, then probes the nearest
standard device rates in deterministic distance order. Every format accepted by
`IsFormatSupported` receives its own period/alignment initialization attempt;
one driver's rejected initialization cannot hide a later valid combination.
`RequireExclusive` fails with the complete structured negotiation reason, while
`PreferExclusive` retains that reason and falls back to shared only after the
exclusive candidates are exhausted.

When exclusive negotiation selects another physical rate, the Program contract
and all Timeline/media-clock coordinates remain at the requested rate. A
stateful Blackman-Harris windowed-sinc converter runs at the single producer
enqueue boundary before the lock-free physical queue. Its filter storage is
preallocated, conversion never runs in the WASAPI callback, and clear/reprime
resets its history. Physical callback, underrun, buffered, discard, and capacity
counts are projected back to the Program-rate grid with checked integer-rational
mapping. Prefix discard rounds toward the first physical boundary that is not
earlier than the requested Program frame, so cross-rate activation has a bounded
sub-frame quantization of less than one Program sample. The selected physical
rate remains explicit in device evidence. Only
successful stream initialization is position/order evidence; matching channel
count is not. The render worker joins the `Pro Audio` MMCSS class and reuses the
same activation revision, underrun accounting, and playback-delay telemetry as
CPAL. Unsupported positions such as Wide, TopSide, and LFE2 fail closed instead
of being projected onto reserved Windows bits. Non-Windows named multichannel
remains rejected until an equivalent platform Adapter exists.

Success publishes one `RealtimeAudioOutputContract` with semantic layout, exact
Program rate, scalar format, buffer-range evidence, channel-semantics proof, and
counts for enumerated/channel/rate/executable candidates. Low-frequency device
evidence also retains the selected physical stream rate, container/valid-bit
widths, share mode, negotiated period/buffer, CPAL host, stable device identity,
resolved selection, whether it was the system default, optional device name,
and any nonfatal name query failure. Every callback snapshot embeds that exact
Contract; Audio
Playback rejects disagreement between the open event and callback observation.
Open failures carry a stable code, request, candidate counts, optional selected
Contract, and backend detail. `AppState` composes physical evidence with
`AudioDeliveryEvidence` only while the current stream Contract and delivery
target match, so diagnostics cannot join facts from different device states.

Physical discovery is a separate low-frequency Window Adapter. It performs no
CPAL/platform call on the UI thread or realtime callback, owns at most one bounded
one-shot attempt, and publishes an immutable catalog snapshot. Preferences may
select a catalog identity or request rediscovery. The device worker observes
the latest selection outside the callback. A selection change retires the old
stream through the ordinary destroy-then-observe `Lost` path and opens a new
checked stream generation. While following `SystemDefault`, it also compares
the current default's stable identity at low frequency and performs the same
generation handoff when the OS default changes. Failure, absence, or switching
uses the existing Synthetic Clock Master continuity policy; it never promotes
video to Clock Master. A transient discovery failure does not tear down an
already healthy stream.

Construction is fail-closed. `AudioPlayback` is returned only after its owned
PCM render worker has spawned successfully; worker creation failure is a typed
creation error, not a permanently empty completion queue. The App may retain
video transport by installing an explicit `ExecutionUnavailable` audio runtime
and using Synthetic Clock Master, while preserving the creation reason for
diagnostics. The device lifecycle worker has separate start-failure evidence
from an ordinary `OpenFailed` device attempt. Both render and device workers
retain owned `JoinHandle`s. Shutdown first cancels current work, closes and
wakes admission, then joins each worker; a device `Lost` event is emitted only
after the concrete physical stream has been destroyed. An unexpectedly finished
render worker or disconnected completion channel transitions once to
`ExecutionUnavailable`, cancels and clears outstanding work, and admits no
phantom in-flight requests. Concrete stream-generation identities are issued
with checked monotonic allocation; exhaustion is a structured creation failure
and never wraps to a reused or zero identity.

The PCM continuity contract is captured as an `AudioPcmContinuityModel` before
the renderer is erased behind a trait object. `prepare`, `reprime`, anchor
validation, and shutdown never call a foreign renderer merely to rediscover
that contract. Pure anchor, generation, configuration, shutdown, and worker
availability checks run before output deactivation or any other foreign call.
If admission subsequently rejects a renderer, ownership is transferred to the
single tracked retirement worker; the caller, render coordinator, and UI thread
never become its destructor path. The retirement queue has the same hard
in-flight ceiling as playback admission, so its capacity proof cannot be
invalidated by an accepted render burst.

The consuming `AudioPlayback::shutdown_and_wait` receipt closes the lifetime
inventory of both PCM render and concrete device-lifecycle workers. Workers
already joined after an unexpected disconnect remain in its cumulative
terminated/panic counts; an absent handle therefore cannot be reinterpreted as
"never started." Same-thread detachment and any panic fail the aggregate
closure used by commercial endurance qualification.

Shutdown is a captured two-phase protocol. `begin_shutdown` performs only
prebuilt atomic cancellation/signaling and queue wakeup; it invokes no trait
method, allocates no replacement work, and waits for no foreign owner. The
consuming phase then supervises the render coordinator, renderer-retirement
worker, output manager, and every device worker against one absolute deadline.
`AudioPlaybackShutdownEvidence` schema 3 and
`RealtimeAudioOutputShutdownEvidence` schema 2 distinguish configured,
started, returned, panicked, timed out, detached, abandoned, and residual
owners. Evidence is fail-closed and monotonic across handles already consumed
by ordinary polling.

Render completion messages contain only owned PCM or a stabilized plain error
string. A renderer's type-erased error value is transferred to the tracked
retirement worker and destroyed there; it cannot cross the completion channel
and later run an opaque destructor on playback polling, shutdown, or UI code.
Queue closure, queue overflow, worker panic, and owner-handoff failure latch
terminal retirement evidence instead of silently dropping the renderer or
error on the discovering thread.

The validation App also retains a small App-lifetime Audio failure ledger at
the terminal owner-replacement seam. Before an unexpectedly stopped render
owner becomes `ExecutionUnavailable`, the Adapter absorbs its cumulative
substitution, render-generation recovery, underrun recovery, backend-loss, and
deactivation-failure counts. Commercial endurance snapshots add that retired
history to the current owner so counters remain monotonic even though the
move-only execution owner has been dropped.

The output queue has one non-cloneable Manager-owned producer handle and one
callback consumer. Enqueue validates the exact Program sample rate, semantic
channel layout, complete interleaved-frame shape, and remaining physical sample
capacity before admitting anything. If rate conversion is active, a
worst-case output extent is proven before the stateful converter advances, then
the complete converted result is admitted. Because the callback only removes
samples, a successful whole-buffer capacity preflight proves every subsequent
push; queued samples are never evicted to make room and media time is never
silently shifted. A
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
requests deactivation, drops the concrete physical stream, captures one final frozen
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

Every realtime output snapshot carries its selected device Contract and the
wall-clock `captured_at` instant at
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
Master only after active callback consumption covers the host-reported
remaining playback delay. Before that boundary, subtracting latency would
produce a negative effective device position: the observation is `Uncertain`
and Synthetic Clock retains authority. The Adapter may not clamp that position
to zero or label accepted-but-not-yet-played frames as device progress. Device
loss or rejected phase handoff returns authority to Synthetic Clock Master;
video presentation is never Clock Master. See
[Playback Engine](playback-engine.md) for the full state and evidence contract.

The ignored `playback_professional_cpal_av_gate` is the fail-closed physical-
output reference Adapter. It requires a proven primary-audio stream duration
long enough for the exact
30-minute 30000/1001 observation, qualifies a concrete 48 kHz stereo CPAL
generation, then drives the normal App event-loop seams at real wall cadence
while a generated picture completes through the real headless Viewer GPU
Adapter. Its versioned `cpal_av_48khz_30min_recovery_v2` policy requires at
least 99.5% current-video readiness, completed GPU presentation, Audio Device Clock Master
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
- a typed sidechain whose detector Track has no main Route remains reachable,
  negotiates its stable auxiliary bus, copies no callback PCM, produces the
  detector waveform exactly, and rejects an undeclared realized bus key;
- main and sidechain dependencies share deterministic topological ordering and
  disabled-edge-inclusive cycle validation; Sequence duplication rekeys the
  complete Route-to-Processor strong reference;
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
- offline Program observation matches the 48 kHz BS.1770 K-weighting reference
  coefficients, exercises both EBU gates, excludes LFE energy, detects a
  synthetic intersample peak at four-times reconstruction, and carries exact
  silence/finite-signal evidence through a probe-qualified FFmpeg export;
- a root audition request retains only selected Track program sources, while
  nested public outputs remain canonical independent Runtime instances;
- timeline/audio crates compile and test independently;
- app playback and export compile against the shared Runtime.
- dense schedule lowering produces topological node slots, contiguous Route and
  Contribution ranges, and liveness-reused scratch for Track→Bus→Output;
- scalar and runtime-vectorized kernels are sample-for-sample identical for the supported DSP
  set, including awkward block sizes and an eight-Track full-schedule render;
- exact fractional forward and reverse source mappings remain block-shaped and
  preserve their complete source-sampling contract; the semantic Program exposes
  that target without reconstructing boundary policy, and the dense schedule
  consumes `Covering` as floor or `StrictPredecessor` as `ceil - 1` exactly once;
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
- a manual real-child cancellation gate separately observes read return and
  physical-permit release against one original 50 ms limit, then consumes the
  raw source shutdown receipt to verify the actual child/pump joins and zero
  success/failure cache admission. Read return alone is not physical closure.
  Local cold-start cancellation exceeded that limit on 2026-09-04; it remains
  an open performance qualification defect, not a passing gate or a leak claim;
- concurrent identical window misses elect one decode leader and one follower
  cache hit;
- deterministic professional-audio acceptance tests reject missing/fake output
  facts and accept a complete versioned CPAL/A/V/memory evidence set; the real
  30-minute Adapter is ignored and never treats an absent fixture as a pass.

## Professional audio qualification

Remaining product work and sequencing live in [ROADMAP](../ROADMAP.md), not in
this architecture contract. Professional support cannot be claimed until the
versioned acceptance plan covers layout/device/encoder negotiation, isolated
plugin-host lifecycle and failure containment, sidechain product interaction
and reference-machine qualification, standards-based metering, authoring
UI/Undo/reopen, reference PCM and export parity, persistent
decoder locality/cancellation, and reference-machine realtime load, drift, and
memory evidence.

Those capabilities must deepen the same compiler, prepared schedule, Host, and
Runtime. Unknown layouts or dependencies continue to fail closed; no item may
be closed by adding only schema, a disconnected UI, or a consumer-specific
fallback mixer.

## Embedded audio for Reference Output

Professional Reference Output consumes exact 48 kHz windows from one public
Audio Program output. It never reads the Monitor Path or infers speaker meaning
from channel count. Rational video-frame boundaries select sample intervals,
and video plus audio schedule atomically as one bounded bundle; fractional
cadences therefore accumulate exact sample positions instead of rounding every
frame. Device routing and reference lock remain in the Reference Output Module,
not the Audio Program. See [Reference Output](reference-output.md).

The App's validation-only persistent Reference producer prepares that public
Program through a dedicated constructor: audition is empty, the target layout
equals the Sequence Program layout, and `Standard` delivery remapping is
rejected. Only identity delivery or already-proven silence is admissible. One
generation enters at the exact first frame's rational sample position and all
later windows must continue at the prior terminal sample. The Runtime shares
the App's sole `AudioSourceCache`; it does not create a qualification-only
decoder authority or borrow Monitor Path state.
An explicit CLAP rebind probes the currently installed definition and checks
its complete saved parameter schema against the authored snapshot before a
single Rack transaction updates the binary revision. Curves, opaque state,
bypass, and processor identity survive the edit and Undo/Redo.

### Scalar processor numeric contracts

Validated scalar processor schemas use their sole numeric channel contract for
static edits and automation range projection. CLAP descriptor bounds populate
the same canonical list; audio authoring does not introduce a second range model.

### Bulk automation key removal

AudioAutomationEdit::RemoveKeyframes carries stable IDs and an exact retain_time
in the resolved target owner domain. Timeline delegates to Core's atomic key-set
operation, then applies its existing lock, numeric-schema and complete Audio
Program validation before publishing the Sequence candidate. Optional channel
curves become unkeyed when deleting every key; the original evaluated value is
retained. Processor-owned curves keep their explicit Core curve object. Preview
and author mutation consume the same edit contract. Batch deletion creates one
author transaction and Undo entry through the existing App action path.


### Current audio automation author time

Timeline exposes audio_automation_author_time with a domain-qualified Sequence
coordinate and an optional, explicit AudioComponentAddress for Processing Scope
occurrences. Component targets carry their own stable address. Sequence-owned
fader/route/Rack curves retain Sequence coordinates. Component and Scope curves
use local_time_in and processing.scope_in respectively plus exact placement
elapsed time. Visual clip_time_in and source sampling rate/origin do not interpret
these audio author coordinates. Checked rational arithmetic rejects overflow.

The query resolves addresses through the existing automation and Component
inspection interfaces. Shared Scope queries require an occurrence even when
only one is currently bound; absent, stale or mismatched context fails explicitly.
For Inspector navigation, time clamps to the closed Clip placement extent;
execution retains its independent half-open contribution membership. Reading
coordinates does not mutate author state or bypass Track/Scope edit admission.

### Definition admission for audio curve drafts

Timeline owns one numeric automation admission implementation for gain, pan and
Processor parameters. It validates the complete curve against its definition,
including absolute incoming/outgoing control values, parameter identity,
animation permission and effective interpolation families. Component pan uses
its canonical [-1, 1] schema for persistence and author admission as well as UI
projection. Existing typed gain/pan errors remain the target diagnostics.

`AudioAutomationCurveContract` is an immutable inspection result. Preview and
canonical author edits prepare with its `preview_edit`; only the author path
publishes the resulting curve, after full Audio Program and lock admission.
This closes the case where moving a Continuous Bezier endpoint across another
key creates a newly required control outside the definition bounds. Rejection
leaves projection, Sequence, revision and history unchanged. Execution continues
to consume the same admitted Audio Program without a UI-specific interpretation.


Audio group value increments enter through `AudioAutomationEdit::OffsetKeyframeValues`.
`AudioAutomationCurveContract` supplies the current definition's numeric channel
contract to exact curve editing and validates the complete candidate afterward.
Preview and fresh author admission therefore share key bounds, control bounds,
interpolation and lock semantics. The UI submits one increment and stable ID set;
it does not clamp individual keys or replace the curve from display samples.


AudioAutomationEdit::SetInterpolations lowers complete-key interpolation through ExactAutomationCurve. AudioAutomationCurveContract admits its interpolation family against the current definition before preparing the candidate and checks all new absolute controls before preview or author publication. Owner locks and no-op outcomes retain the shared authoring semantics.


### Native automation signal qualification

Native event transport and native DSP output are qualified separately. The owned
CLAP sample-gain fixture consumes each SDK event batch's sample range, retaining
the last gain in blocks without events. The owned stateful VST3 fixture consumes
all queue points as piecewise linear segments, using the previous block's final
value at implicit offset -1 when sample zero is omitted. Neither fixture embeds
a Mondrian curve evaluator or allocates a per-process signal buffer.

The isolated-worker signal tests cover intermediate and final-sample changes,
unchanged blocks, VST3 adjacent-point jumps and realtime/offline modes. Upstream
simple gain examples retain their static-state tests; their whole-block gain
behavior is not an oracle for sample-accurate automation. The production host
continues to lower admitted exact curves through its preallocated event scratch
and the native SDK boundary.

The App native group-history matrix is a separate end-to-end gate. It uses a real
probed stereo WAV, definition-pinned instances, author-session interpolation
edits, durable project/library reopening and shared AudioProgramRuntime rendering
with different realtime/offline block partitions. Its receipt must name the
actually tested product and fixture hashes; compile-only output is not acceptance.

### Complete exact clipboard group authoring

Core capture retains exact authored keys and refuses incomplete stable selection.
Timeline PasteKeyframes prepares the complete target curve once, normalizes final
constraints once and applies the existing numeric definition and owner lock
admission. No per-key author transaction or alternate audio evaluation path is
introduced. The App audio curve command carries Sequence identity, exact target,
explicit shared-Scope occurrence and the common Copy/Cut/Paste operation. Its
sole transient clipboard stores complete relative-time keys and numeric unit/type.
Capture validates the whole selection before publication; Cut commits once first.
Paste creates fresh identities, maps current exact owner time, checks whole
Component extent and reuses Timeline batch admission. Sequence curves remain
unbounded by the viewport. Global Paste refuses an audio payload without an
explicit focused target. Localized actual curve menus share the same commands.
Real Host Copy/Cut/Paste, fresh pasted-group selection and subsequent Cut preserving
the original key pass. App codec/history/Unicode reopen matrices also pass. The
owned native CLAP/VST3 Track pre-fader gain fixtures additionally qualify fresh
pasted Auto/Continuous groups, stale Cut refusal, one Undo, saved live Redo and
sequential fresh Unicode reopen with sample-accurate realtime/offline PCM. Fresh
reopen releases the original Session first, preserving exclusive project leases.
Wider Component/Scope/route/rack admission and native contracts remain to qualify.

### Joint audio key-set transforms

AudioAutomationEdit::TransformKeyframes carries a complete stable key set, one
exact time delta and one definition-bounded value delta. The same curve contract
prepares UI candidates and canonical edits. ExactAutomationCurve admits the joint
candidate and normalizes Auto/Continuous controls against final topology once;
there are no intermediate axis-specific publications. Pointer requests use the
existing origin-relative parameter gesture so repeated moves do not accumulate.

Native CLAP/VST3 group-history qualification extends the same owned authoring
path with a joint time/value TransformKeyframes request after fresh clipboard
reopen. Stable identities and interpolation intent are retained; unselected time
and raw value remain unchanged while constraint normalization may adjust handles.
Missing identities, collision and locked owners refuse the whole transaction.
A successful request has one Undo entry; zero displacement retains generation
and history. Native realtime/offline PCM comparison is reused through Undo,
saved live Redo and fresh Unicode reopen. Execution receipts must name their
test source separately from any previously qualified product-worker source.

### Native declared-default reset qualification

The owned CLAP/VST3 gain history fixture also exercises ResetToDefault after
fresh Auto/Continuous curve archives, before and after joint time/value edits.
A processor always owns its exact curve: reset removes keys and restores the
schema default in that same curve, without creating a separate static-value owner.
The existing reference evaluator covers both keyed and unkeyed native PCM.

The fixture verifies missing/locked instance refusal, unchanged plugin state,
bindings and unrelated author data, one Undo, already-default no-op generation
and history, saved live Redo and fresh Unicode Undo/Reset archives with empty
session history. Native execution qualification must name the current worker
and owned fixture hashes; these tests do not broaden the host's negotiated bus,
event, editor, vendor or platform capabilities.

### CLAP auxiliary native inputs

CLAP discovery and worker admission share one deactivated-port scanner, after
restoring occurrence state. It requires one main input at index zero and one
main output, unique native IDs per direction, and at most 32 input ports. The
current shared processor contract admits mono or stereo auxiliary inputs with
exactly the main layout. Stable route keys are `clap.audio-input.<native-id>`;
display names do not determine identity. ABI order is retained in the prepared
contract and rechecked before activation, so missing or reordered ports fail
admission instead of redirecting an authored sidechain.

The existing isolated auxiliary contract and shared PCM transport are reused.
The worker allocates planar buffers and port pointer capacity at prepare time,
then copies each keyed input independently for every block. Disconnected buses
receive the shared runtime's explicit silence, with no main-input fallback.
Source-level ABI fixtures cover two distinguishable auxiliary inputs, silence
following signal, changed order, duplicate IDs, duplicate main flags, mismatched
layout, excessive count and unavailable metadata. These checks do not establish
current packaged-worker, authoring archive, PDC, vendor or macOS qualification;
VST3 auxiliary execution and mixed layouts remain separate open work.

Port roles and stability follow the [CLAP audio ports contract](https://github.com/free-audio/clap/blob/main/include/clap/ext/audio-ports.h).

The separate owned CLAP sidechain DLL has an independent weighted PCM signal,
three-frame latency/tail and opaque-state-controlled native port configuration.
The ignored installed-fixture matrix uses current packaged discovery and worker
execution in both processing modes, two block partitions, restored reordered or
removed inputs, delay reset and explicit silence. An intentionally stale prepared
bus order after state replacement must fail child admission. This fixture is
separate from existing gain fixtures and does not encode the host's routing logic.
Execution receipts must identify the source, current product and fixture hashes.

The App's native sidechain history fixture routes a detector-only track to one
native input while another remains disconnected. It uses real WAV decoding and
the shared public delivery runtime to check three-frame latency compensation,
realtime/offline partition-invariant PCM, missing/locked transaction refusal,
one Undo, saved live Redo, and fresh Unicode routed/undone archives with one
SQLite asset and no inherited session history. This remains a named reference
matrix; it does not establish general vendor or platform qualification.

### CLAP activation-time latency

Port metadata is inspected while deactivated, but native latency is queried on
the main thread only after activation, as required by the CLAP latency extension.
Probe returns that activated value; the worker independently activates and checks
latency and tail against the prepared contract. Either mismatch stops/releases
the processor and deactivates the instance before admission fails. Shared audio
scheduling and PDC continue to consume the single execution contract.

A source ABI fixture returns an invalid latency before activation and3 afterward.
It verifies probe/admission and deactivation after stale latency refusal. The
owned native sidechain DLL also guards activation with per-instance shared state,
while retaining its independent3-frame delay/tail and prepared DSP buffers.
This enforces the [CLAP latency lifecycle](https://github.com/free-audio/clap/blob/main/include/clap/ext/latency.h)
without adding work to audio callbacks or another latency interpretation.


## VST3 native auxiliary audio-bus contract

`Vst3PluginRegistration` carries the existing ordered
`AudioProcessorAuxiliaryInputContract`, and discovery transports it through the
existing `AuxiliaryBusWire` representation. A selected class is restored and its
processing mode applied before querying actual native bus declarations. Neither
channel count nor a separate default-instance discovery query determines bus role.

The vendored exact `vst3-host` 49fc189 dependency adds owning-thread,
non-processing `Plugin::native_audio_bus_info()` in its existing component seam.
It reads bounded `IComponent::getBusCount/getBusInfo` declarations, retaining
native roles, names, channel counts and default flags in bus-index order.
Preparation verifies one native main input at index0, one main output, at most31
auxiliary inputs, and exact matching Mono or L/R Stereo speaker arrangements.
Inactive admitted audio buses are activated using the existing host API and all
metadata/layouts are rechecked. Bus keys are `vst3.audio-input.<native-index>`;
names may duplicate or change without becoming author identity. Any activation,
shape, role or arrangement failure refuses preparation. Mixed layouts remain
separate execution work.

The supervised worker prepares `BusAudioBuffers` once, retains every native slot,
and supplies main and keyed auxiliary PCM separately. Processing reuses capacity
for partitions up to the admitted maximum; disconnected buses receive explicit
silence from the shared runtime, and missing or wrongly sized PCM refuses the
block. State reentry restores the same class/state, reactivates/rechecks buses and
uses the same admitted contract. No second mixer, PDC owner, author transaction,
plugin process or automation evaluator is introduced.

Two source regressions independently reject duplicate main roles, surround
arrangements with the same channel count, missing/changed prepared metadata and
unbounded inputs; audio library111 passed/0 failed/14 ignored. These are source
contracts; they did not themselves qualify native execution, performance or
vendors. The subsequent owned native qualification below exercises actual
activation, per-bus processing, state changes, reset and routed
Undo/save/reopen/public PCM.

Dependency provenance and the preserved MIT license are in
`vendor/vst3-host/MONDRIAN-PATCHES.md` and `LICENSE`.
Four upstream Rust files contain the documented metadata extension; three
loader files only lose existing trailing whitespace. All other 26 Rust files
retain their upstream contents. Optional
upstream features are preserved but not enabled in Mondrian. The vendor package
has its own workspace boundary and is excluded from the product workspace.
`cargo metadata` confirms it is not a product member; product all-feature gates
do not activate upstream GUI/device tooling. The original libc0.2.186 and all
other registry versions are retained; only the host package source identity
changes in Cargo.lock.


### Owned VST3 sidechain qualification fixture

The separate `vst3-sidechain` fixture preserves the stateful gain reference and
uses fresh class/controller identities. Default-inactive auxiliary ports require
real native activation. An independent weighted stereo DSP, fixed3-frame delay,
matching latency/tail, state-controlled weights/port removal and the existing
sample-offset linear queue convention serve as output oracles. Native SDK slots
and duplicate names do not become inferred main roles or alternate author IDs.

An ignored audio integration matrix checks both modes, two block partitions,
opaque component/controller state, changed native count, reset/silence and stale
admission. The App CLAP/VST3 cases share the existing author-history/public-PCM
assertion body (distinct native setup), checking detector-only routing, locked
and missing refusal, one Undo, saved live Redo and fresh Unicode project archives.
Fixture build/test compilation is not native execution proof. Clean source
`b5eb715b4d9134cf9793cc409a7978a945f0c5ed` was rebuilt into the actual product
worker and explicitly qualified: App CLAP/VST3 sidechain history 2 passed,
audio native sidechain matrix 2 passed, and three separate native main-bus
parameter/state regressions passed, all with zero failures and zero ignored.
The auxiliary fixture checks both processing modes, independent weighted PCM,
two partition patterns, opaque state-controlled ports, real activation,
3-frame delay/tail, reset/silence and stale-contract refusal. The App matrix
checks detector-only routing, PDC, locked/missing refusal, one Undo, saved live
Redo and fresh Unicode project archives. Source and binary identities remained
stable. Complete Golden also passed 3/3 at this source; current modal/resource,
portable LUT/OCIO, native window and audio-history gates passed separately.
See [the scoped receipt](../dev/modal-resource-lut-acceptance.md#current-vst3-auxiliary-modal-resource-and-lut-acceptance).
These owned stereo Float32 references do not qualify sidechain GUI controls,
mixed layouts, MIDI, plugin editors, arbitrary vendors, devices or performance.
The prior ac6f069 source and 3b44084 Golden receipts retain their original scope.


### Session-bound product input observations

App owns a non-realtime input-observation service with one physical worker,
a capacity-one result channel, a 16-request coalescing FIFO and a 16-entry
transient cache. Explicit queries for different targets remain accepted FIFO
intents; repeated target state is coalesced, and capacity pressure refuses a
new request rather than silently dropping an accepted one. Cancel/catalog
invalidation retains ownership of running discovery until it terminates.
A received message alone does not authorize joining: UI polls `is_finished`
and only joins terminal threads. Queue work is checked against current state
before admission and before starting the next job.

The cache key contains the actual author processor, exact target and Scope
binding, Render Contract, Sequence and typed in-memory AuthoringSessionId.
`inspect_audio_processor_input_author` owns read-only membership/binding checks
for both Audio preparation and App stale-result comparison. Structurally shared
semantic author equality avoids native probing or author allocation identities
in UI projection. The existing resolver and processor-preparation contracts
remain the sole native interpretation; control queries create no DSP Session,
project mutation, history or EventBus request.

The closed product Action carries query/cancel intent and typed session identity.
Inspector and Mixer bind read-only cached state to localized inspect, cancel and
refresh controls. Project/canonical processor/render/session changes and catalog
invalidation reject late replies. Errors are retained as quiet control state;
explicit refresh retries. Native bus display metadata and source/disconnect
controls remain subsequent work; available input counts alone do not complete
sidechain routing or its native Undo/archive/output acceptance.

### Connecting observed auxiliary inputs

Product connection menus bind exact transient session, Sequence, rack/Scope
occurrence, processor and successful observation ID. App requires the current
cached Ready contract to match the complete binding and include the bus key,
then delegates once to the existing routing author transaction. This admits no
native probe, secondary author owner or optimistic event before validation.
Refresh/catalog invalidation, author/render changes and project reopen invalidate
old menus. Timeline still owns final source lock, address and retained-cycle
admission. Generic author routing contracts retain unavailable native keys for
storage/offline authoring; interactive creation uses observed input facts.

Successful input observations are stored as shared immutable snapshots. Cache
reads clone the `Arc`, preserving the result identity without copying port
vectors or allocating another boxed payload. Only successful publication mints
an observation ID; no identity is needed for a failed control result.

## Native plugin signal timeline

The canonical `AudioProcessorProcessContext` carries the signed input-signal
sample position after processor-path latency compensation and the prepared exact
sample rate. Isolated worker commands already carry these facts; native adapters
consume them for every block and state-entry seek, without a second playhead.

CLAP receives a stack-owned transport event with seconds timeline validity only.
Signed samples become the ABI's 2^31 fixed-point seconds using checked integer
nearest rounding, ties away from zero; unrepresentable positions fail explicitly.
The independent steady counter is unavailable (`-1`), rather than derived from
the sequence playhead. No tempo, time signature, beat/loop or playing state is
invented from realtime/offline mode.

VST3 receives a typed per-block `ProcessTimelineContext`. The active prepared
sample rate is verified, projectTimeSamples is absolute and signed, and optional
values/validity flags are cleared. SDK requirements select available facts;
requests alone do not make a field valid. Native chunking advances only valid
optional coordinates, preserving the absolute position and unavailable fields.
The typed setter is allocation-free on the product's in-process vendor adapter
inside the supervised Mondrian child; it does not add a second product IPC call.

Owned independent time-dependent PCM fixtures cover realtime/offline, 44.1/48 kHz,
nonzero starts, negative latency-adjusted starts, forward/backward seeks, reset
history and uneven partitions. Existing sidechain/render fixture state meanings
are retained. Musical maps and actual playing/recording/loop facts still require
canonical authoring and playback contracts before those optional fields can be
advertised. Fixture correctness does not qualify arbitrary vendor plugins.

Native SDK ABI qualification is a workspace-owned audio integration test using
the exact same `vst3` dependency and lock as production. Its independent C++
oracle compiles unmodified pinned official SDK headers and reports actual
layouts and target-encoded interface identifiers. Runtime output, include-tree
provenance, compiler and executable hashes are required; an excluded vendor
workspace with independent registry resolution is not qualification evidence.


### Canonical VST3 SDK runtime

The workspace, vendored host's independent Cargo entry and both owned VST3
fixtures resolve one local `vendor/vst3-sdk` package and one original
`com-scrape-types` COM runtime. The runtime binding surface is generated from the
verified complete official VST3 3.8.1 tree at immutable commits using the original
upstream generator; only header enumeration is sorted. The original generator's
SDK test-interface and C++ helper exclusions are recorded in provenance. No ABI
definitions or compatibility shim are handwritten.
The three license texts, verified generation source manifest, independent
generator lock and shipped-file hashes are retained. The reproducible preparation
tool verifies Git blob and SHA256 for every input and uses a fresh separate
generation directory/Cargo home. Current generation used Windows GNU and LLVM
18.1.8 after MSVC's installed STL rejected that Clang version; native MSVC and
macOS ABI validation remain separate required gates.

The audio workspace ABI test checks independent official C++ sizes, alignments,
field offsets, target-encoded GUIDs and compile-time method signatures for
IMidiMapping2, IMidiLearn2, ITransportControl and orchestral articulation IInfo.
New interface availability does not advertise optional host implementation,
MIDI2 event routing or vendor-plugin qualification. Native performance receipts
include the entire canonical SDK source/provenance/license tree and explicit
immutable SDK/generator identity because a path dependency has no Git source in
Cargo.lock. Generator/package/source changes invalidate prior performance proof.


### Independent VST3 COM call-through

An independent C++ helper built against unmodified pinned official SDK headers
queries a real canonical Rust `ComWrapper` for IMidiMapping2, IMidiLearn2,
ITransportControl and orchestral articulation IInfo, then invokes every one of
the nine actual virtual methods. Oracles cover MIDI2 by-value byte payload,
MIDI1/MIDI2 mutable mapping output arrays, signed samples and quarter-note
transport union discrimination, nullable transport position, 17-by-16
articulation output, cross-interface pointer adjustment, canonical FUnknown
identity and balanced foreign reference lifetime through final Rust destruction.
No replacement VST3 interface/vtable or compatibility shim is declared.

The ignored workspace integration test verifies current source/helper hashes,
actual compiler transcript/dependency artifacts and the entire 69-file official
include tree before loading the helper. Its receipt collector only records a
completed build. This is execution evidence for the selected ABI slice on the
actual platform/compiler pair; it does not implement optional host features or
qualify third-party plugin behavior. Separate native MSVC/macOS runs are required.

### Typed CLAP parameter-event storage

The child worker's current input stream owns SDK `ParamValueEvent` values in one
preallocated vector and original insertion ordinals in another. Both owners
retain their actual vector capacities across reset/process. Admission bounds
the event count; a full owner refuses another event before either vector can
grow. An allocation-free unstable sort orders ordinals by `(timestamp, ordinal)`
and preserves equal-time event order. Clack's standard `InputEventBuffer` and
`InputEvents` adapter borrow the typed events; no ABI declarations or generic
event byte arena are added. Transport remains a separate stack-owned event.

The worker-source allocator oracle measures successful allocator requests on
its current test thread, with const TLS bookkeeping and all fixture inputs
outside measurement. Preparation at two block capacities must differ by exactly
the derived planar PCM, SDK parameter-event and ordinal storage bytes. A warmed
owner receives the full admitted two-parameter event count, with deliberately
non-numeric equal-time ID order, then resets/processes at positive, negative
and repeated timeline positions without any Rust allocation or reallocation.
An independent plugin-side event and PCM oracle checks native ordering and
values. This is an allocator-request correctness gate, not RSS, third-party
heap measurement or a realtime latency qualification.

Each event owner occupies its actual `Vec::capacity()` multiplied by its SDK
event or ordinal element width. This local closure does not yet make the native
resource contract complete: known payload/state/descriptors, other vectors and
native bus buffer owners must join the canonical resource admission contract,
with shared immutable allocations counted once. Discovery's current zero
plugin-session scratch field does not establish that those allocations are free.
Unknown plugin-owned memory remains separately observed process pressure.
Future MIDI and heterogeneous input/output events need one bounded typed event
admission/lifetime contract that extends the shared execution path.

### Scalar Rack pointer transactions

Audio parameter pointer edits reuse the canonical authoring Session's existing
Sequence gesture, with no secondary Rack history or copied-value rollback. Each
live update lowers only SetParameterStaticValue through the established Rack edit
transaction and audio reconciliation seam. Finish seals one Undo entry. Cancel
restores the current gesture's exact author origin through Session cancellation,
preserving existing Undo/Redo and reconciling audio with the restored parameter.
An invalidated owner or intervening author transaction seals the prior valid
interaction instead of restoring a snapshot over unrelated newer edits.

The App intent contains only typed session/Sequence identity, the exact Rack or
Scope component occurrence, processor definition, parameter schema and scalar.
It cannot embed arbitrary Rack edits, native state or opaque nested actions.
Same-ID reopen/rebind, lock/deletion, schema change and Scope occurrence changes
refuse old queued controls before author mutation. The closed scalar family also includes input Trim, static Fader, ordinary Route
level and processor sidechain level. Native editor gestures remain a separate
unfinished author interaction path and are not implied by these controls.

### Native performance harness identity

The release audio callback gate requires a five-artifact audio group in the
shared collector: the exact CLAP/VST3 worker executables, both owned reference
fixtures, and `--audio-test-executable`. Before discovery, worker startup or
measurement, the audio test verifies its actual `current_exe()` path and SHA256
against that unique build receipt role. Its JSONL also records that same binary
identity. A missing or stale executable receipt cannot qualify a callback run.
OpenFX-only collection keeps its existing separate three-artifact group.

The collector artifact-group regression uses controlled temporary source and
binary-byte fixtures with mocked read-only version/machine queries; it is a
receipt-format oracle and does not execute those fixtures or claim native
performance acceptance. The actual release gate still requires completed current
build commands/transcript, immutable source hashes, exact PCM and each real
audio deadline.


### Closed scalar audio author lowering

`SetAudioScalar` contains an explicit open-author session/Sequence lease and one
closed owner: processor parameter, Channel Trim, Channel Fader, ordinary Route
level or processor sidechain level. App admission reuses canonical Timeline
strip/Rack/route inspections, exact processor definition/schema, currently
selected Scope occurrence, route endpoints/bus and static automation authority.
It lowers only the corresponding existing `SetParameterStaticValue`,
`SetInputTrimDb`, `SetFaderDb`, `SetRouteGainDb` or
`SetProcessorSidechainGainDb` request. No opaque nested action can acquire an
author gesture, and no native metadata request supplies author authority.

All five scalar fields share the existing AuthoringSession gesture and bounded
Undo history. Valid pointer updates reconcile the same canonical audio program;
Cancel reconciles the Session-restored author origin. Stale or independently
superseded groups do not restore an older whole Rack/Channel snapshot over newer
edits. Control projection performs no realtime worker work, and callbacks retain
only the precise owner instead of copying complete projection collections.
Actual Host gesture, typed refusal and performance receipts qualify this seam
separately from source design and native plugin/editor feature matrices.


### Native audio control-channel overhead

The worker control protocol uses version-6 framing for typed pending and bounded configuration/seed control. A response writes
one stack-owned 16-byte header before the existing bounded UTF-8 detail, using
`write_all` for partial and interrupted writes. Success still carries no detail.
The reader's allocation and framing rules are unchanged. Instrumented writer
qualification checks independent literal wire bytes, write-call counts, partial
writes, UTF-8 bounds and propagation of write/flush failures; it is not a network
packet or operating-system syscall trace.

Each parent control socket enables TCP_NODELAY once after successful acceptance.
Its cached timeout records a read/write pair only after both socket setters
succeed. Repeated operations with the installed budget need no socket-option
update. EnterState, normal operations and bounded shutdown still select their
existing budgets; any partial timeout update invalidates the cache and closes
the worker. Shutdown refuses the exchange if its short budget cannot be installed,
then terminates/reaps instead of potentially using an earlier longer timeout.
Callback deadlines, scheduling priority and protocol semantics are unchanged.

The recorded release baseline independently passed PCM correctness and exposed
21 missed 8-sample callback deadlines across the two max-64 Uneven realtime cases.
Repeated socket-option updates and fragmented headers are proven avoidable work,
but their contribution to those tails is not established by writer/option-count
qualification. The full current-source isolated-worker release gate must still
pass every original audio deadline with all observations retained. Native pending
root preflight adds another control round trip when applied; that cost belongs
inside the same production measurement and may not be exempted.


## Native editor author event source prerequisite

The VST3 adapter exposes one ordered, bounded source for parameter Begin/Value/End,
explicit group Begin/End and configuration DirtyChanged. `NativeEditorEventDrain`
returns either a complete event vector or a sticky typed Incomplete failure.
Event-source and DSP-feedback capacity exhaustion explicitly refuse the native
callback; malformed normalized values also refuse without clamping or publication.
A failed ComponentHandler cannot become complete by draining or adding a fake End.
Source storage retains its fixed event capacity; it contains no author callbacks,
Session history, project state or candidate-install authority.

Open-editor, progress, context-menu and unit-cache notifications remain a distinct
control-plane contract. They cannot delimit author groups and do not promise
cross-order with the author source. The old parameter-only drain and author
variants in HostNotification are removed; there is no compatibility alias.
Fallible Plugin/editor drains preserve transport/unavailable errors instead of
turning a failed helper poll into an empty successful event stream. The vendor
helper protocol carries the typed Complete/Incomplete result unchanged.

This is source capture only. The product does not yet open a native editor, lower
these events into the canonical author Session or recover incomplete native
configuration. That consumer must bind the exact retained native object/cohort,
author Session/Sequence and occurrence/definition, refuse incomplete gestures,
and restore/install the admitted configuration via the sole owner service before
a new source is admitted. A fresh ComponentHandler/native object resets source
failure; this prerequisite neither rolls back controller state nor retires a valid
processor. The vendor helper's general auto-recovery is not an author owner lease
and cannot authorize publication after a changed instance. Product integration
must use its actual isolated processor child, not nest a vendor helper/editor clone.

Native window, control-thread/processor-role split, main-thread callback service,
ordered Session transaction/dirty-state save, resize/DPI/modal focus, real plugin
compatibility and release performance remain separate qualifications. Native
opaque configuration is still a fresh-Enter seed, never a post-DSP replay checkpoint.


## Scoped native editor and DSP roles

The VST3 adapter can borrow two disjoint roles from one loaded native
component/controller. The main editor role is not Send and checks the actual
initializing thread. The processor role borrows the existing prepared DSP
storage and can move to a scoped audio thread. Neither role owns a second
instance/controller or exposes activation, setup, state restore, termination or
reload. Both borrow the original Plugin, retaining its module until the split
ends. Ending a temporary editor role preserves the native owner's view; real
processing joins before full native control mutation. Enter/replacement separately
removes the view and its exact parent before the original native object changes.

Inline and split processing use one borrowed implementation of the existing
bus validation, parameter/event chunk routing, DSP call and output cleanup.
IPlugView attachment, sizing and removal likewise use one implementation.
Controller presentation updates use the existing deferred processor-to-control
queue; they are not native author edits. Restart flags are source facts: taking
them does not authorize configuration of an active processor.

This interface is a native lifecycle prerequisite, not proof that the product
Windows editor, installed-runtime/author lease, grouped author transaction,
automation write policy or physical GUI workflow has been delivered. The
product main pump, serial control consumer and exact current owner admission
must close those boundaries before support is claimed. The existing editor-to-
DSP feedback mutex remains in this stage and needs actual contention/performance
qualification; scoped field borrowing alone does not make it lock-free.

The main editor service also dispatches non-background Data Exchange on the
actual controller thread through the existing HostApplication. It neither
waits for background queues nor discards the bounded diagnostic snapshots;
Data Exchange receiver references are still released before module unload.


## Windows native child main message service

The native child initializes STA/OLE on its actual main thread before loading
plugin code and retains that guard until all native owners and modules have
been released. Successful OleInitialize S_OK and S_FALSE calls are balanced;
a conflicting apartment is a typed initialization failure. VST3 FUnknown's
COM-like ABI is distinct from the Windows apartment contract: no interface
marshalling or unrestricted callback-thread permission is implied.

The worker-owned Windows parent temporarily borrows the same editor role and
uses the existing sole host HWND procedure and container creation/resize/destruction
helpers. The parent retains its OLE owner and native attachment identity, so
another object's temporary role cannot service/remove/destroy this HWND. The main
pump dispatches a bounded batch for all windows on that thread, then applies
controller presentation and main-thread Data Exchange, DPI and IPlugFrame resize
work. The host HWND procedure records source facts without invoking the plugin.
Actual user resizing goes through the same view's resize constraints on main;
accepted client extents, not pointer coordinates, update its native parent.
Close removes the view before destroying its parent. Failed removal keeps the
parent live and requires explicit failure/isolation teardown, never reuse.

A pointer-free thread-message wake signal carries no command or owner authority.
The existing child transport and DSP completion slot can wake the actual pump
after publishing their own bounded item. WM_QUIT is reported to the owner rather
than silently discarded. The transport, scope join, exact installed owner and
coherent native author-state boundary remain product consumer obligations; this
adapter alone is not a completed product editor or physical GUI qualification.

Thread roles follow the official VST3 UI/processing model: initialize,
terminate, IEditController and IPlugView are on the actual native main thread;
IAudioProcessor::process is on the processing role. setProcessing is legal on
the UI or processing thread and is performed outside the split by the current
lifecycle owner. This host deliberately joins/quiesces before coherent author
state capture, even though realtime VST3 getState may be called on the UI thread
while processing under the SDK. CLAP on_main_thread likewise requires actual
main-pump delivery after request_callback; an atomic request flag or a source
draft does not itself deliver that callback.

Primary contracts: [Microsoft OleInitialize](https://learn.microsoft.com/en-us/windows/win32/api/ole2/nf-ole2-oleinitialize),
[VST3 threading model](https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical%2BDocumentation/API%2BDocumentation/Index.html),
[IAudioProcessor](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IAudioProcessor.html).

### Native VST3 callback thread admission

The same ComponentHandler records its actual initializing control-thread identity.
Begin, value, end, group and dirty callbacks from another thread fail before the
ordered author source or DSP feedback sink is changed. Such an author violation
makes that source sticky-incomplete; polling or a later main-thread End cannot
manufacture completion. Restart/open/context-menu and control notifications also
refuse a wrong-thread call, but do not acquire author group membership or corrupt
a valid author stream. This follows the SDK UI-thread callback contract; a mutex
is storage synchronization, not a thread-role authorization. The actual processor
role only calls DSP methods. A processor-side request must be serviced by the
same native main/controller owner before it invokes host control callbacks.

This is source-only producer admission. Native artifact tests, the product
installed-owner consumer, HWND lifetime and physical GUI qualification remain
separate gates. VST3 interfaces use a COM-like ABI; Windows OLE initialization
does not grant cross-apartment permission to call controller interfaces.


### Deferred native lifecycle boundary

VST3 restart flags and CLAP core restart requests retain their isolated child.
An admitted public Program window completes once under its still-active contract,
including every nested fill and stateful sibling. The next public root boundary
performs bounded pending queries over the prepared closure before source reads,
DSP, meter changes, continuity poisoning or caller-buffer clearing. Private
nested fills carry that admitted root permission and do not cut the parent block.

The isolated wire distinguishes complete PCM with pending requests from faults.
Its fixed pending query performs no native DSP, metadata, allocation or lifecycle
service. This extra IPC observation contributes to the complete callback deadline
and requires the native performance gate; no zero-cost claim is made.

Foreign PCM rendering preserves DeferredForReprepare as a typed outcome through
completion stabilization and Media/App admission. It cancels/quiesces old work,
retains the renderer owner, clears queued PCM by generation and transfers physical
clock authority truthfully. It substitutes no silence and increments no render
fault counter. Until canonical nonRT reprepare installation exists, the retained
source stays RepreparePending and no automatic old-contract retry is scheduled.
Explicit source replacement can install a newly prepared owner.

This boundary feature is not complete dynamic recovery: retained-child control
service, real per-occurrence immutable factories and PreparedAudioPlan/PDC rebuild,
peak-grant admission and meter-handle transfer remain required. Generic opaque
state is configuration, not an exact DSP checkpoint; scheduled activation-profile
changes and offline exact continuation require separate proven capabilities.

### Bounded retained-native configuration control

Native configuration work is a typed non-realtime command on the retained
exclusive processor object. Its cohort combines the callback serial with raw
request flags in one AtomicU64. Every nonempty callback increments the bounded
serial, including repeated flags; exhaustion remains sticky. Service compares
and retires the exact initially observed cohort only after legal lifecycle,
activated metadata and bounded state capture. A later same-bit callback remains
pending and receives StillPending; there is no automatic reactivation loop.

The child command protocol has named zero-PCM configuration and cohort commands.
Their finite control timeout is separate from processing deadlines. Fixed
fields and lane/state extents are validated before allocating; the child counts
actual scalar Vec capacity, capture buffers and state-envelope capacity. The
parent records its separately decoded allocation. Capability refusals retain
the child. Native lifecycle failure, panic, malformed transport or a crashed
child remain faults. Configuration-bound old Enter/process calls defer through
a typed response before copying any stale PCM; they do not kill the owner.

The bounded VST3 slice admits latency-only requests with the unchanged applied
ProcessSetup. Successful DSP-delivered scalar values establish the constant
component profile; controller values alone do not. Checked stop/deactivate/
activate leaves the retained instance activated but stopped. Read-only native
bus/parameter inspection and actual latency/tail queries precede state capture.
Fixed SDK IBStream objects share a combined state extent, report ignored native
write/seek overflow and seal after capture. Optional stream attributes are not
exposed by this dedicated capture path. The original two-stream state codec is
retained; no alternative ABI/runtime or legacy compatibility path is added.

CLAP stops in the instance's audio role, deactivates in its fixed main role,
flushes admitted constant plain values while inactive, configures render mode
before structural inspection/activation and reads actual activated latency/tail.
Bounded main callbacks and PluginState capture obey the same thread ownership.
Current scalar values can change without altering structural descriptor facts.
The fixed host writer detects overflow even if native save reports success.

ReadyForInstall contains actual activated facts, a codec-tagged configuration
seed and a control token. It leaves old-contract DSP blocked. It does not alter
the author document, Undo/dirty state, immutable execution factory, prepared
PDC, source/Media generations or Clock Master. Configuration is not a generic
DSP checkpoint. An exact occurrence/plan/session/epoch candidate transaction,
fresh-seed final activation comparison, complete resource peak, canonical
PDC/meter transfer and ordinary fresh epoch/preroll remain required before a
product recovery path can resume. Time-varying activation profiles and exact
offline continuation need separately proven capabilities.

The incremental byte-buffer receipt excludes retained descriptor/payload/state,
COM objects, PCM/bus/port buffers, newly retained successful scalar evidence and
structural metadata, shared factory allocations and candidate-plan/PDC owners.
Those known owners must be accounted accurately and Arc sharing deduplicated
before canonical installation. Unknown vendor-private heap remains separate
physical-pressure evidence. No full-memory or performance pass follows from
this bounded control layer. Root preflight PollPending IPC and the successful
scalar-evidence pass remain in the complete measured realtime execution path;
process-only numbers cannot exempt their cost or certify a root deadline.

Owned native controls check three/seven-frame sidechain PCM before/after legal
reactivation, then instantiate the captured configuration twice with actual
product workers. Additional controls issue a same-bit restart during capture
and deliberately ignore state-writer overflow. These test a retained candidate
and its capability boundaries, independently from the future installation and
Clock Master recovery gate.

### Exact bounded host byte owners

`NativeHostFixedBytes` owns one exact allocator-requested byte-slice Layout.
Its constructor validates this incremental owner grant before allocating and
returns structured allocation failure instead of invoking an infallible Box
constructor. The global allocator's usable-size overhead, OS pages, stacks and
vendor-private heap remain distinct physical-pressure evidence. This API does
not claim a complete worker or root resource receipt.

The retained CLAP configuration writer uses this fixed owner. Writes cannot
grow it; even a native state callback that ignores a write error cannot publish
truncated state. Its Box-to-Vec state-codec transfer retains the same allocation
and full capacity, including unused storage. The child buffer receipt charges
that capacity once; the separately decoded parent allocation remains another
owner. Allocation unavailability is a typed temporary host resource refusal,
distinct from exceeding the grant or lacking plugin state support.

The independent storage integration oracle counts actual GlobalAlloc requested
layouts with preinitialized thread-local bookkeeping. It checks odd/zero/full
extents, below-grant zero-allocation refusal, exact deallocation, full writes,
short-state transfer without allocation, actual Vec spare capacity, ignored
overflow and distinct-attempt reset. This is a reviewable gate, not an executed
native memory or performance receipt. Full retained/candidate installation
still requires accurate metadata, COM/SDK/bus/PCM, shared Arc/container, IPC,
mapping/control and PDC owners with shared identities deduplicated before peak
admission. Native preflight IPC remains in whole-root deadline measurement.


### Bounded local allocation union snapshots

`NativeHostAllocationLedger` observes complete local Vec/String/Box owners using
actual requested layouts and capacities, deduplicating only their same live
physical allocation base. Its boxed body and exact fixed table are charged at
their own overlapping peak. Recording cannot grow storage, and ignored grant,
table or conflicting-layout failures prevent a truncated snapshot. Zero-sized
owners allocate nothing. Nested owned containers require separate receipts.

The result is a reported local subset peak, not complete native or candidate
admission. Opaque SDK capacities, shared allocation headers and other omitted
owners require their own canonical public/fork proofs, not a zero-byte fallback.
Foreign workers own independent address spaces/launch identities; matching
virtual addresses are not shared physical allocations. Shared mapping backing,
OS/allocator/kernel/device and vendor-private memory are separate explicit
domains. Full root install still requires complete coverage, validated old/new
peak, exact Plan/Session/occurrence/epoch/cohort and one atomic install authority.
The paused/fresh-generation policy does not turn configuration state into a DSP
checkpoint or automatically resume an opaque stopped instance.

The new ledger and independent actual GlobalAlloc source oracles are not yet
compiled or executed. No complete resource, PCM or deadline pass is claimed.

### Generated continuity observations

The sole Audio Program/Delivery Runtime exposes an allocation-free read-only
`AudioContinuityEvidenceVisitor`. Existing root/nested Session fields supply
immutable Plan ID, exclusive Session ID, exact epoch, public completed head and
lookahead execution head. Local processor fields independently supply the full
occurrence and latency-adjusted input-signal head, including legal negative PDC
coordinates. Unentered/lazy/stateless/poisoned/failed states are explicit; none
fabricates a recoverable head. Visiting invokes no source/native DSP or reset
and creates no collection; consumers admit their own bounded storage.

These are generated-coordinate facts, not a DSP checkpoint, physical callback
anchor, state-preservation promise, prepared PDC migration or permission to
resume. Same-Plan aliases still own distinct Sessions. Retained restart service
must separately reconcile callback-submitted/estimated-audible positions,
unconsumed complete PCM and each generated cursor. Standards permit reactivation
to reset private delay/reverb buffers; generic opaque state cannot establish
continuous output. Full known-owner admission, retained PCM journal, candidate
PDC installation and qualified physical output continuity remain separate gates.

### Same-child configuration seed staging and commit

The non-RT candidate control has a separate bounded seed ownership step. The
issuing child retains fixed metadata: exact serviced callback cohort and control
token, final activated contract, native codec, byte extent and SHA256 of its
published configuration. A public ticket is only a projection; authority comes
from comparison with this same exclusive child's private ready record. It is
not an author ID, class, occurrence, plan, Session, epoch or root install lease.

StageSeed transfers the original parent candidate bytes through the existing
ordered control socket to that same child. Its decoder validates fixed bounds
before reserving, records actual Vec capacity, and adopts that one owner without
a second seed copy. A bounded allocation/capacity refusal drains the body with
fixed stack storage, preserves framing and the stopped child, and remains a
typed safe refusal. Exact content/codec/contract/cohort/token mismatches cannot
replace a ready seed. Repeated identical staging preserves the original owner;
the incoming and retained allocations overlap and both belong in peak accounting.

CommitSeed requires matching preowned storage and an atomic unchanged retired
request-serial claim. It clears no newer flags, including a repeated same bit.
The child then moves the existing seed and updates its admitted contract without
allocating or invoking native lifecycle/state callbacks. VST3 moves the actual
configuration into its future-Enter payload, so later explicit fresh entries
destroy/reload the component from the new seed. CLAP retains the legally activated
component and owns the same installed candidate seed; its explicit Enter resets
processing state. Both adapters check native pending requests before state entry
can destroy/reset an owner. A callback after the serial claim remains a later
pending generation, observed before the next root window; it does not disappear
with a fresh VST3 component.

The parent accepts only the exact staged receipt, including actual child capacity,
and clears its cached old pending notice only after matching commit success.
These named operations use protocol V6 and the current cached non-RT timeout
policy. They add no per-DSP event allocation or alternate evaluator. Root
PollPending remains a real extra realtime IPC round trip requiring full-root and
nested release deadline acceptance; direct callback timings do not cover it.

The seed grant covers only this one additional child Vec. Original payload/state,
parent candidate, duplicate stage overlap, fixed decoder/ready metadata, old and
candidate graphs, mappings, SDK/native bus/descriptor owners, queues and PCM
journal require the canonical complete known-host peak ledger. This is neither a
full-resource installation receipt nor physical RSS/vendor heap accounting.

Same-child acceptance is not root installation, author commit, playback completion
or private DSP continuity. The sole Media worker must still consume exact current
renderer/hold generation and root/nested Plan/Session/occurrence/epoch authority,
prepare complete PDC and resources bottom-up, reject supersession, and install
only while output is quiesced. Generic native lifecycle may reset private DSP;
the supported product policy is an accurately installed AppliedPaused result,
followed only by an explicit Play/seek through a fresh generation/preroll. The old
bounded PCM journal and generated-head/callback-anchor facts remain separate;
opaque state is never used to restore or replay that old continuity.

The owned reference qualifications exercise legal component/activation profile
changes in realtime and offline modes, resource and identity refusals on a live
blocked child, exact commit, duplicate rejection, and two fresh positive/negative
anchors against independent seven-frame PCM. They require fresh matching
product/fixtures and actual execution. Source-only packet checks are not native,
performance, vendor, whole-root, or Undo/save-reopen acceptance.


### Native editor attachment ownership and temporary thread roles

The child retains one real native object, controller and HWND parent. A temporary
editor/DSP split borrows that object; ending it joins the real DSP before full
native mutation but does not close its view. The worker-owned Windows parent can
stay open across coherent control-side state capture. Lifecycle replacement,
Enter rebuilding, rebind and child shutdown explicitly remove that exact view
and destroy its parent before releasing the native owner/module.

The sole typed `EditorAttachment` retains the real view on failed `removed` and
refuses another remove or live UI operation; that failure is sticky because its
actual platform attachment is unknown. After successful removal, failed frame
cleanup retains only the frame-clear obligation and never repeats `removed`.
Unsuccessful attach only receives frame cleanup, never an unmatched removal.
Temporary editor-role Drop has no view ownership. Public native operations
expose exact removal/frame failure classes. Drop is not successful-close evidence.

Each actual plugin load has one child-local, unforgeable attachment identity.
The native parent checks it before service/remove/destroy, preventing another
object or reused COM address from being treated as its owner. This read-only
native lifetime fact is not an author Session, installed-runtime lease, plan ID
or child seed permission. The installed Audio runtime and existing author
Session remain the only execution and edit admission authorities.

On unresolved detach, the isolated product consumer must reply with a fatal
lifecycle result and terminate its child before Rust releases native objects or
their module. The standalone vendor Drop can only report failed fallback removal;
it cannot prove a failing plugin detached. That child-retirement consumer, real
IPlugView/HWND probe, exact installed-owner adapter and physical GUI qualification
remain separate gates; the source-level ABI tests do not complete them.

This source stage adds an 8-byte identity field per native object and confined
borrow, plus a process-local checked atomic serial. The same-main OLE lifetime is
shared by an `Rc` only when the Windows parent exists; its allocation/header and
OS native window bookkeeping must enter the sole known-owner ledger before
whole-graph performance or resource qualification. No extra native controller,
processor instance, root-window scheduler, author history or callback queue is
created.

## Initial actual Runtime/native object editor-owner installation

AudioProgramRuntime is the sole issuer of editor-owner leases from existing
root/nested AudioRenderSession Hosted records. Each record is unbound, prepared
but invisible, or installed. Preparation asks the actual retained child for its
current PID/nonwrapping load serial, then issues a typed Core binding ID and
requires the same object's acknowledgement. Publication revalidates the actual
installed Core claim, preparation token, root/local Plan/Session, occurrence
and current continuity epoch for the entire closure before bounded record moves.
Failure/cancellation publishes no partial leases and creates no author history.

The sole authenticated worker codec is V6, rejecting V5 and unknown kinds before
foreign dispatch. Original named commands 1–8 retain their exact bodies; command
9 is PollNativeObservations, and 10 is BindNativeObject. The parent verifies
active retained Child handle/PID and reply correlation; PID/class/author ID
alone cannot identify a native load. VST observations come from the actual
Plugin attachment/load serial. Enter clears its old binding before reload.
CLAP object/editor capability currently remains explicitly unsupported.

Root state entry uses the sole Session epoch admission and revokes all old
root/nested leases before lifecycle work, including child components that will
enter lazily. Local Contribution first-entry clears its exact record before
native entry. Editor requests must revalidate both current installation and
the actual Hosted native object; copying a lease makes no permanent permission.
A StageSeed/CommitSeed receipt, attachment token or queued readiness event is
neither a root installation claim nor author permission.

Candidate installation must reuse this issuer at its actual quiesced Session
replacement boundary. Unentered destinations need preparation against their
actual allocated next Plan/Session and epoch, with foreign acknowledgements
before ownership move; no candidate installation API is implied by this initial
consumer. Ordered author callbacks, coherent opaque-state transactions, actual
Windows IPlugView/main/DSP/reader lifecycle, complete resource grants and new-lane
performance remain separate required slices. Portable adapters prove authority
contracts; the ignored rebuilt-product VST gate proves child/load binding IPC
only when executed against matching current sources/artifacts.

### Native CLAP log observations

CLAP Host Log copies at most512 native bytes into already constructed fixed
MPSC storage inside the actual SDK Shared/HostWrapper owner. The audio callback
allocates no message, takes no lock, waits for no other thread, formats no
String and performs no terminal I/O. Producers have bounded reservation
attempts; the unique main/control receiver consumes only published slots.
Queue incarnation is a private non-wrapping storage fact, not Runtime/author
permission; its counter is touched only before native owner construction.
Full/contention/sequence exhaustion remain lifetime-sticky loss facts.

The sole V6 PollNativeObservations9 request carries a bounded consumption
policy (Preserve/Drain). Object-capability binding preparation preserves logs;
serial diagnostic consumption drains at most32 fixed records. The same response
contains actual child PID, optional actual editor object, raw message bytes,
all seven official severities and loss flags. The retained Child handle/PID is
checked before parent exposure. Rust padding is never serialized. Invalid
UTF-8, an incomplete truncated suffix, null messages and unknown signed
severity remain distinct; a diagnostic Fatal is not a DSP kill request.

The ingress body is part of the actual SDK Shared/HostWrapper allocation; it
does not create another log Arc. Complete allocator-layout/SDK/old+candidate
resource admission remains the sole known-owner ledger gate: sizeof the queue
body alone is not a complete Arc or whole-process grant. The fixed observation
reply/receiver/control stack are additional actual owners to account.

Qualification is layered: existing native API activation/PCM RED, actual logged
full-capacity callback allocation/parameter order/PCM controls, native
activation/background/process→same-child Poll9 records, fixed queue publication
controls, defensive wire/UTF-8 controls, and unchanged release/root/device
deadlines. Source availability is not a measurement receipt. Discovery/probe
message delivery and the product serial diagnostic→notification consumer still
require connection and qualification; the EventBus is never the request or
realtime ingress. Current inline CLAP thread roles remain correct; a future
distinct DSP lane requires its actual participating role identity.


### Native log delivery on the installed root

The actual isolated processor's `PollNativeObservations` tag 9 remains the one
native observation protocol. Binding preparation uses Preserve; the canonical
Runtime's serial diagnostic consumer uses Drain. Every diagnostic reply is
checked against the retained Child handle and PID before source projection.
The read-only instance capability performs no foreign calls. A private Runtime
cursor fairly selects one root/nested local Session occurrence per work item,
leaving other native queues unconsumed; shared author IDs never merge owners.

The adapter projects up to 32 messages with 512 retained bytes each into Core's
completion carrier. This bounded copying creates no second native queue or
severity/UTF-8 interpretation. Diagnostic source records are not editor leases,
restart tickets, state seeds, DSP checkpoints or installation authority. A
fatal severity is a report and does not itself kill or reset valid DSP.

Current V6 contains object facts and logs. A later ordered author extension must
consume its author/state fields from the **same** Drain result at the exact
Hosted-record/author-lease boundary and extend the same completion. It must not
discard author fields during log projection or add another editor poll.

Qualification separates the owned SDK callback/zero-allocation oracle, actual
isolated worker main/background/audio logs, whole canonical Runtime/PDC source
and PCM oracle, serial Media admission, and App presentation controls. None
proves whole known-owner allocation, physical output deadlines, vendor DSP
continuity or all-standard compatibility. Queue/SDK Shared, native wire and
Core response storage can coexist; their real layouts/capacities belong in the
sole owner ledger. The extra observation IPC and callback/control interference
must be included in release/root/device measurement. The public 8-frame leaf
deadline remains 166.7 microseconds at 48 kHz; old failures are not relabeled.

Temporary discovery/probe logs still need their separate operation-result
projection. Their temporary native owners must not be presented as an installed
root or silently substituted for an actual product diagnostic source.

## Native audio editor control lifecycle (Windows VST3 source slice)

The same actual isolated VST3 component/controller owns its native parent and
IPlugView. Main remains its recorded OLE/COM and window/message owner. DSP borrows
disjoint processing storage on a real scoped thread; reader owns exactly one
decoded command until the correlated main response is acknowledged. Enter,
configuration, seed, binding, editor mutation and termination wait for DSP join.
Inline CLAP retains its existing one-thread execution; no empty GUI reader lane
or claimed CLAP GUI/multithread qualification is introduced by this slice.

The serviced reader uses a 100 ms socket timeout only to check closure of its
existing command slot. The borrowed Read adapter retries WouldBlock/TimedOut
inside the same read, preserving partial header and payload progress in the sole
decoder. Idle commands have no new deadline. Closure, poison, EOF and other I/O
errors remain distinct. Teardown closes both slots, shuts down the read half and
joins the exact reader before releasing its owners; it preserves independent
operation, cancellation and join failures. This adds no queue, heap owner or DSP
scheduling role. The interval bounds individual socket waits, not DSP deadlines
or a hard end-to-end teardown guarantee. Real TCP controls cover partial-header
cancellation and both
header/body completion across multiple polls.

Current protocol V6 has one decoder and one observation response: Poll9 uses
Preserve or Drain, Bind10, Open11, Close12. Preserve observes the same live object
without joining/restarting DSP or capturing opaque state. Drain/control uses the
joined full owner. Cleanup stage comes only from the canonical attachment model.
No V5 compatibility reader, separate EditorPoll, controller or owner registry is
retained. HostLog records remain in the same Poll9 response and consumption path.

Public Core editor targets are read-only addresses. The sole Runtime rechecks its
opaque installed lease, actual current Media claim, exact root/local Plan/Session,
occurrence, continuity epoch, binding and child load serial before native calls.
Media enqueues typed editor work through its existing serial RenderWork; metadata
and queued readiness do not install an object or authorize author edits. Completion
again checks actual renderer identity, Media generation and current Source claim.

Removal failure retires the isolated child without another removed call. A
successful removal followed by failed frame clear retains the same object in
control-only cleanup. A retained whole-tree state-entry cohort may retry its
named state-entry command on that same owner: the adapter retries the exact Close
before restoring state, retains partial peer preparation, and emits its own typed
state-entry reply. The cleanup lane starts no DSP borrow. Explicit ClosePending
immediately holds Media even while paused; root preflight also detects native
window-origin cleanup before any DSP.
Normal parent PCM does not send illegal Process or map cleanup to configuration,
silence, success or a device fault. The held renderer claim and original complete
PCM journal remain valid for exact Close retry. Open/seek cannot supersede pending
cleanup. Close plus exact output quiescence/final anchor reaches completed-paused;
only explicit fresh continuity releases it. PlaybackEngine uses the single
NativeAudioControlHold for configuration and editor cleanup, without old aliases.

Known resource requests use actual concrete reader/processing slots and adapter/
SDK Box Layouts, plus separately declared 2 MiB reader/8 MiB DSP stack reservations.
Parent/child preparation compares the same profile before foreign load. Actual
decoded Vec/Box capacities/alignments, staged seed and configuration reply overlap
are checked against canonical dynamic ceilings. This known subset is not a whole
candidate grant or measured RSS: thread runtime/TLS, allocator/kernel bookkeeping,
error Strings, native HWND/platform storage and private plugin allocations remain
explicit frontiers. RT DSP admission invokes the canonical native scheduling
adapter before Ready and explicitly restores on the same DSP thread before join;
no parent-visible native scheduling evidence or deadline/performance acceptance
is claimed from source inspection alone.

The production and portable contract source is uncompiled/unexecuted until its
recorded qualification commands run. Owned IPlugView/vendor tests and current
product-helper native tests separately exercise real platform messages, same
object DSP, native user Close and failure retirement. They do not establish
physical GUI, full plugin interoperability, RT timing, native editor author Undo,
automation or save/reopen. Ordered author/state Poll9 ingress, coherent opaque
state and the canonical AuthoringSession execution fence/install handoff remain
required next stages. Opening a real view alone is not complete native authoring.


### One actual native snapshot at installed root preflight

For an installed native Hosted record, root preflight now uses one Poll9 Preserve
response containing actual object identity, editor cleanup and a non-consuming
pending lifecycle request. The retained child main role services preceding
commands before reading those fields and writing the reply. Its pure request
read never clears the native cohort or existing adapter/Session sticky ticket.
Requests arriving after that observation remain pending for the next admitted
root boundary; an already-admitted complete root window retains its old contract.

The parent validates the authenticated child PID and exact installed load object
before applying cleanup or configuration refusal. Frame-clear cleanup still
holds output; unresolved removal still retires the owner. General diagnostic or
capability observation keeps the state-entry timeout; root preflight uses the
configured process timeout and its existing timeout-cache/error policy. No
additional transport, cached request authority or per-child block query is added.
Non-installed adapters retain their existing pending-only preflight path.

The current V6 alpha observation schema includes a fixed eight-byte pending field
after cleanup (tag + exact VST flags). There is one encoder/decoder, no historical
V6 payload reader. Unknown VST bits are preserved; zero VST flags and unknown
tags are refused. The fixed owner/diagnostic storage and this field must be part
of the actual worker lane/resource accounting; this change is not a whole-owner
grant. A shorter command path is a causal optimization, not a timing pass: the
original 166.7 microsecond eight-frame gate and all 21 r30 misses remain evidence.
The fresh release gate must include this preflight round trip and all production
scheduling/main/DSP work without removing outliers or substituting device budgets.


### Native worker shared command interpretation

The authenticated version-6 worker stream has one decoder for the existing
first eight command tags and their bounded configuration/seed bodies. Inline VST3
and CLAP continue to read and respond on the same current thread and socket.
This refactoring adds no thread, native editor instance, native state capture,
installed-owner permission or extra queue. One owner dispatcher performs the
existing lifecycle and configuration requests; its shared PCM interpretation
retains exact extent, alignment, parameter lane, offset and finite-value gates.

Safe bounded seed refusal consumes the complete known body before the next
command and cannot invoke a phantom native stage. Capability refusals keep the
same native owner and correlate one existing reply. A future Windows main/DSP
consumer must reuse this decoder/dispatcher after DSP join, account for its
reader/stack/slot owners, and independently qualify actual thread entry/exit.
Portable localhost contracts exercise the first eight literal current V6 tag values, exact control
payloads, refusal framing, incomplete-body failure, same-instance PCM output
and matching replies. They do not qualify a native GUI, vendor callbacks,
physical device behavior, installed-editor authority or realtime deadlines.


The canonical SDK port-owner seam can report CLAP AudioPorts' two actual Vec
allocations inside their owning implementation: complete capacity/layout and
local base identity with an immutable owner borrow. The audio ledger consumes
these typed SDK facts without private casts or recreating clap_audio_buffer
layout. Inline AudioPorts bodies and referenced PCM remain separate owners.
This finite port seam still does not expose all opaque SDK/shared allocations.
Its source-only allocator/capacity/buffer-order cases need the one canonical
SDK dependency and actual root/native qualification before any support claim.


### Canonical vendored Clack dependency

All root and owned reference-plugin consumers use the same local host/common/
extensions/plugin source tree at vendor/clack. Its independent runtime workspace
preserves upstream authors/licenses, verified immutable Git blobs and explicit
local source/manifest/license deltas. The allocation observer is a public owning
SDK seam; no private pointer/layout cast or global cache mutation is used.
Current clap-sys is still the exact previously pinned revision. Source packaging
does not establish latest-standard coverage, native behavior, full known-host
residency or deadline acceptance. Qualification/format changes must update actual
shipped provenance hashes and preserve their original source lineage.


### Native pending-contract prerequisite

The typed Core request and exact Audio plan/Session/occurrence ticket are the
canonical pending lifecycle contract. Session and whole-root preflight precede
source reads, DSP, meter publication and caller output mutation. This narrow
prerequisite exposes a default-empty processor query and prepares the Session
gate for independent behavioral regression. Actual native IPC producers, child
lifecycle service, candidate/PDC/resource installation, Media/Clock Master hold,
diagnostics/editor services and device deadlines require their later consumers
and qualification; this prerequisite alone does not implement those features.


The pending ticket is sticky on the exact old Session: an empty later callback,
native seed service, cancellation or fresh Enter cannot clear it. A separately
allocated replacement Session has a new identity and no old ticket. Partial
native service or failed graph installation keeps the old Session paused and
unable to admit DSP; a changed private configuration is not an installed PDC.
The controlled existing-API oracle verifies this domain boundary, not a full
candidate/Media/physical output installation or continuous vendor DSP recovery.


### Cancellation precedes public native preflight

`AudioProgramRuntime::render_into_cancellable` checks an already-canceled request
before the root/nested native request walk. That path performs no pending query,
source read or DSP call and leaves caller PCM untouched. A pending configuration
cannot replace that cancellation result. The admitted path keeps its later check
so cancellation observed after preflight still prevents source/DSP execution.
Cancellation does not clear an old Session's pending configuration ticket.

## Physical native realtime scheduling ownership

`mondrian-audio-scheduling` exposes a same-thread native lease for dedicated
producer/DSP responsibilities, independently of the canonical render mode and
processor lifecycle. Windows entry/priority and explicit same-thread restoration
are checked; entry and confirmed exit use different evidence types. Main/editor/
reader and Offline work are not DSP scheduling roles. Existing inline processing
does not become a scoped DSP executor merely by depending on this adapter.
See [native audio scheduling](audio-scheduling.md) for actual producer admission,
unsupported-platform evidence, known-owner frontiers and unchanged deadline gates.
Native registration never substitutes for full-root/device/performance proof.


### Generated CLAP 1.2.10 ABI input

One `vendor/clap-sys` path package supplies the complete immutable official
1.2.10 include-tree-generated raw ABI to root, Clack and both native fixtures.
Header modules reexport the same declarations. The generator excludes only
header-local version/color objects and the inline compatibility predicate;
independent native oracles compare their Rust value projections with the headers.
Actual C enums retain their raw carriers. Clack converts constants into the
explicit event ID, flag and Rust array extent carriers with compile-time bounds.
The anonymous window union is the generated raw union; X11 uses `c_ulong`.

The full ABI gate compares 137 official structs, all declared/promoted fields,
all generated named aliases, 295 constants and 228 callback carriers against an
independently compiled official C++ header report. Each native platform needs
its own receipt. Generated input migration does not implement missing host
services, native editor/session consumers, vendor behavior or realtime deadlines;
the standard feature matrix and compatibility receipts retain those boundaries.

### SDK allocation-container ownership

The canonical Crossbeam `ArrayQueue` has a public borrowed `buffer_storage`
view of its actual fixed `Box<[Slot<T>]>` allocation. It reports the internal
slot slice's requested layout and identity without copying private slot fields
in Audio, mutating a queue or reading concurrent payloads. Nested values require
separate owner proofs. Workspace and standalone VST host use the same pinned,
licensed canonical queue package; the queue algorithm is unchanged.

The canonical VST `ComWrapper` exposes a borrowed `payload_storage` view from
its actual owned Arc. This covers the generated COM header plus Rust class data,
not std's reference-count prefix or nested class allocations. A separately
qualified exact compiler/target std Arc profile is needed for full allocation
reconstruction. No private header casts or interface-pointer layout guesses are
permitted.

The standalone allocation oracle observes real requested layouts and lifetime,
and metadata reads must allocate nothing. These owning APIs alone are not a
complete SDK visitor, root old/candidate/temporary resource admission, native
compatibility result or realtime performance qualification. Host-returned COM
objects and background-owned nested payloads must remain accounted before a
candidate installation can be admitted.

### Native VST host storage owning views

The canonical VST runtime exposes a weak COM allocation lifetime without
retaining the live class. The host records created message, attribute, stream,
bounded capture and menu objects before exposing them to foreign code. Message
and stream attributes can independently outlive their parent object, so their
actual allocation lifetimes are recorded separately. Temporary strong borrows
are retired after the registry unlocks, outside author and installation guards;
last-strong menu retirement can release plugin-owned targets.

Owning views report actual Vec/String/Box requests, queue slot buffers, and
separate Arc payload observations. They include recycled parameter queues,
bus samples/pointers, callback notifications, menu/progress tables, metadata,
levels and module wrapper storage. Shared aliases require one process-local
allocation deduplication. The registry's retained and observation Vec capacities
are also owners, including after typed observer refusal.

These APIs are a local inventory prerequisite, not a complete root admission or
installation capability. Full Data Exchange and externally consumed output-event
payload ownership, std platform-buffer/Arc/thread qualification, old/candidate/
temporary peak accounting and the supervised foreign-memory envelope remain
required. Arbitrary callback captures and nonexclusive shared payloads refuse
explicitly. Rust 1.97.1 Thread metadata uses Arc with System directly; a global
allocator journal alone does not observe that owner. Configuration state is not
a private DSP checkpoint, and no uninterrupted resume or deadline qualification
follows from these storage views.


### Native author source and coherent main capture prerequisite

The VST host keeps one bounded ordered native author source and its existing
GUI-to-DSP feedback sink. Source status reports the actual nonwrapping revision,
unconsumed event count, open parameter-edit count, timestamp-group depth and
sticky whole-source refusal. Repeated Begin for one parameter is counted.
FinishGroup closes the timestamp scope only; individual parameter edits can
remain open, as in the standard's multi-fader example. Draining event records
does not close those boundaries or clear a source failure.

The processing role can queue actual unconsumed native parameter values into its
existing bounded input queue before a zero-sample parameter flush. It validates
the existing feedback suffix before clearing that feedback; an orphan or full
sink refuses the whole operation without a successful prefix. This operation
does not advance PCM or sample position, stop/deactivate the component, reload
its state, or invent author events. Physical thread and native processing proof
belong to the matching worker and owned-view qualification, not these SDK
callback controls.

One bounded component/controller capture implementation serves stopped native
configuration capture and exclusive main-thread author state capture. The
configuration path retains its stopped admission. The author path permits an
already Processing component only after the DSP borrow has joined; it does not
pretend configuration bytes are a private DSP checkpoint. Capture coherence
admits only an actual DirtyChanged(false) callback suffix; a new value, boundary
or dirty=true callback preserves the source for retry. A native clean callback
does not mark the project saved.

This SDK prerequisite is not a completed native editor authoring workflow.
Product publication still requires the exact installed same-object binding,
retained parent delivery, sole AuthoringSession transaction, canonical parameter
Plan handoff and authenticated acknowledgement. Save, candidate service and
fresh Enter require a genuinely closed source with no outstanding delivery;
author generation freshness alone cannot establish that fact. Physical native
GUI acceptance, complete owner-resource admission and realtime deadlines remain
separate actual execution gates.

### Data Exchange ownership and native-module retirement

The Data Exchange lifecycle owner keeps the real dispatcher JoinHandle, while
the dispatcher borrows a separate shared core. Shutdown flushes accepted
exchange work, closes queues and releases receiver references, then stops,
wakes and joins the dispatcher before component termination and module
destruction. Last parent retirement follows the same join boundary; the
dispatcher never owns that parent or a JoinHandle for itself. No detached
thread or skipped self-join permits code or COM receiver retirement to outlive
the loaded module.

Independent original-API lifecycle controls exposed the former parent/worker
cycle and missing shutdown join, then passed after this ownership split. Those
controls exercise the real dispatcher and owned SDK receiver. They establish
the host retirement boundary, not qualification of every third-party receiver,
native module, platform or realtime deadline.

Owning observations cover the parent and core Arc payloads separately, actual
ExchangeQueue Boxes, boxed block metadata, retained aligned pointer/Layout
requests, both fixed queue buffers, and the diagnostic ring's live Vec
capacities. Queue lifetime is pinned by the existing lifecycle lock while
these borrows are inspected. Observing a queue never drains it or dereferences
concurrent plugin payload. Copied diagnostics use one fixed FIFO ring; byte
admission counts each payload's allocator capacity, and taking a batch moves
payload ownership into a separately accounted caller Vec.

The actual dispatcher exposes its standard Thread identity, name, explicit
stack reservation request, JoinHandle presence and completion state. These
facts are distinct from std's private heap owners and the platform's committed
pages or kernel bookkeeping. An independently qualified compiler/target std
thread allocation profile is still required; an observer without that profile
refuses the family instead of charging zero. Rust's direct System-owned Thread
metadata is not proved by a GlobalAlloc journal alone. Shared Arc observations
also require qualified complete requests and allocation-identity deduplication.

These local owners do not grant whole-root old/candidate/temporary peak
admission or install a replacement Session. That gate must additionally cover
all SDK and execution owners, source/author fences, retained PCM, PDC and
same-child installation. Private plugin heaps and the supervised foreign-memory
envelope remain separate facts. Configuration bytes are not a DSP checkpoint;
neither continuous resume nor realtime acceptance follows from storage views.

### Exclusive retained output-event payload observation

The canonical queue exposes a FIFO initialized-value borrow only through its
exclusive mutable owner. It does not pop, move, replace or drop elements or
change head/tail/stamps, including on observer refusal or panic. Callback
interior side effects are not rolled back. Slot allocation remains observed
through the existing owned buffer API; no external private-slot layout is copied.

The retained VST child observes nested output-event Vec capacities only after
the actual processing role has joined and the queue's Arc has exclusive
ownership. A separately retained library output-consumer alias refuses that
borrow, even when the queue is empty. Complete external-consumer handoff is a
separate ownership contract. No diagnostic drain or MIDI delivery is substituted
for allocation observation. These views are a known-owner prerequisite, not
whole-root peak admission, native event-format coverage or deadline acceptance.

### CLAP main-thread callback slices

The host coalesces `request_callback` into its existing pending bit. Each service
slice performs at most sixteen real `on_main_thread` calls on the retained main
thread, outside the audio-role guard. A request made by the last callback remains
pending for the next slice; reaching the slice limit is successful even when the
plugin keeps requesting callbacks. An empty later slice performs no unsolicited
callback. The in-process SDK fixtures exercise both a forty-callback chain and
an exactly sixteen-callback chain through the actual CLAP ABI. This local repair
does not establish the complete idle native GUI/timer pump or realtime deadline
qualification; those are separate product execution gates.

### VST3 Moduleinfo and factory compatibility JSON5

Bundle Moduleinfo and the real IPluginCompatibility stream share one direct
typed JSON5 parser. The maintained parser decodes the official grammar;
the host no longer strips comments or rewrites a partial JSON document.
Single quotes, unquoted/escaped keys, hexadecimal and signed numbers,
leading/trailing decimal points, Unicode whitespace and escaped line
continuations therefore reach the same existing metadata schema.

An allocation-free raw-input envelope preserves the four MiB byte limit,
bounds containers to 128 levels (including ignored extension fields), and
rejects unterminated strings/comments before recursive deserialization.
It does not normalize or copy the document and is not a second value parser.
The format library's nonfinite float values remain explicit typed values:
the existing finite-positive snapshot rule rejects them instead of turning
them into null. Existing integer, count, string, UID, compatibility and
bundle-relative snapshot-path validation remains authoritative. JSON5 syntax
acceptance does not make an invalid metadata value acceptable.

Owned bundle tests and an actual SDK COM factory/compatibility interface
exercise this discovery path. They are format/schema qualification gates,
not a third-party plugin, whole-root allocation, editor or realtime deadline
compatibility claim. Cargo records the exact parser package in both the
product workspace and the standalone host lockfiles.

### Pure immutable native configuration factory rebinding

A prepared factory can bind reusable native configuration to a new immutable
factory without touching the retained native instance. The concrete VST3/CLAP
preparation codecs replace only their state field. Helper arguments, binary
identity, bus/schema lanes, cached activation contract, shared layout, exact
mode, deadlines and native-lane profile remain unchanged. This operation
performs no probe, process launch, Enter, lifecycle or controller operation;
it is suitable for preparing the next authored Plan while keeping the same
Hosted child and native view. The old immutable factory remains intact.

Output is measured and written by two bounded passes of the same concrete
serializer, reserving the observed extent once and refusing capacity/extent
drift. The old preparation Vec is not cloned only to be immediately replaced.
Empty configuration remains an explicit empty state, distinct from no state.
Future real child creation still validates its activated contract and can
refuse a configuration whose activation facts differ.

This is a pure payload capability, not author admission or a complete resource
grant. Exact native source/delivery acknowledgement, sole AuthoringSession
publication, same-live Plan handoff, current renderer/installation claim and
old/new/temporary allocation overlap must be independently admitted before
publishing the new factory. Concrete decode and metadata allocations also
belong in that peak; retained footprint alone is insufficient. Configuration
does not establish private DSP continuity. Unsupported adapters do not grant
a handoff, and this prerequisite does not complete AppliedPaused installation,
native editor authoring or realtime acceptance.


### Frozen Runtime policy and ordinary parameter-plan overlap

Each root and generated local Runtime retains the actual
`AudioRuntimeResourceGrant` supplied at preparation. `resource_footprint()` is
observed logical accounting, never a source of replacement permission.
`admit_parameter_plan_overlap` resolves every actual old local Plan/Session in
the current root and accepts only unchanged retained render/DSP/PDC shape. It
checks old plus new immutable-plan logical weights against the original prepared
ceiling, and old fixed residency plus actual new event Vec capacities, reported
complete extra local owners and bounded admission bookkeeping against the same
fixed ceiling. Receipt identities are batch sorted, duplicate materializations
refused, and allocation aliases in the local union charged once. Final consumption
rechecks root/local/new Plan IDs and event buffer allocation identity/layout.

The non-Clone receipt is a resource result only: it grants no native edit,
Source installation or author transaction permission. Same-live handoff must
also settle actual author delivery, confirm current canonical author generation,
complete same-object native Bind/peer-state work before the bounded final CAS,
then swap actual plans/event storage and retire old owners outside all guards.
This stage does not wire or qualify that final handoff by itself.

Logical weights and the reported complete-owner subset are not whole-process
telemetry. Complete old/new host-owner coverage, qualified standard-library
Arc/thread owners, native worker temporaries, OS allocations and plugin heaps
remain separately required for whole-root memory acceptance. No allocator/private
header guessing, new grant derived from usage, automatic resume or performance
pass is introduced here.


### Compact Media diagnostic delivery

Media transfers a nonempty or loss-bearing native diagnostic batch as one owned
box through its existing completion and Playback event path. App consumes that
same batch after the actual renderer/generation/installation admission checks.
Complete empty batches do not allocate a delivery box. Diagnostics retain their
source/loss facts and cannot authorize a native parameter edit. Ordinary PCM
work/completions gain no box or extra allocation from this change; native editor
control payloads are separately owned outside the hot enum storage.


### CLAP callback output ownership prerequisite

The concrete CLAP worker uses the same preallocated typed output owner for
`process` and inactive `params.flush`. A successful `try_push` copies a known
global ParamValue or actual GestureBegin/GestureEnd into retained host storage;
it never means accept-and-discard. Cookie pointers and borrowed native event
bytes are not retained. Exact SDK type/space/size checks precede concrete reads,
and the prepared plain numeric schema, native-call extent, sample order and
non-wrapping sequence are checked without callback allocation or container growth.

The source retains original flags, readonly definition facts, signed signal-grid
origin/rate and sample offsets. Flush carries an explicit origin with no invented
sample timeline; its raw offset is retained because the standard loses timing
inside flush. Legal unwrapped Values stay unwrapped. Readonly and DONT_RECORD
values are transient observation candidates, never automatic author writes.

The current prerequisite has **no canonical author/transient consumer yet**.
Accepted events remain in this bounded source; no Poll9 diagnostic drain clears
them. The subsequent same-Poll9 source/delivery/Session/Plan/Ack consumer must
explicitly accept a complete prefix before releasing storage. This stage does
not advertise GUI, HostParams, recording, modulation, notes, MIDI or SysEx support.
Unimplemented event semantics return false instead of silently being discarded.

The resolver reserves the exact new output Vec owners and the actual complete
ClapWorker Box layout through the existing processor Session scratch obligation.
Construction verifies actual Vec capacities before native instance creation and
checks the existing Session scratch ceiling. This is a known additional subset,
not complete old/new SDK, foreign plugin heap, OS, thread, or deadline acceptance.

Normative sources are the immutable CLAP 1.2.10
[events.h](https://github.com/free-audio/clap/blob/195b42a004144fab0b3cf95e9c067187d15365b7/include/clap/events.h)
and [params.h](https://github.com/free-audio/clap/blob/195b42a004144fab0b3cf95e9c067187d15365b7/include/clap/ext/params.h).
The owned native callback-result fixture and allocation controls must be executed
with matching current artifacts; source preparation alone is not native qualification.


### Immutable factory storage observation

`PreparedAudioPlan::visit_factory_host_owned_storage` borrows the actual factory
bindings and invokes the same prepared factory's nested-owner observer. The
isolated adapter reports its complete input-name/parameter/bus/argument Vec
capacities, every nested String (including ParameterId's actual owner), opaque
preparation payload, helper path and optional dispatch text. Pure native seed
rebinding creates new real owners and this observer reports those new owners;
it does not launch, reload, Enter or capture private DSP state. Scalar built-in
factories have no nested allocations and explicitly implement that inventory.
Unimplemented adapters return a missing-observer error, never a successful zero.

Coverage travels with each borrowed identity/layout. Complete Vec/String/Box
owners may enter the existing known local union under the original frozen policy
ceiling. The actual factory Arc is reported as ArcPayload, and platform encoded
PathBuf/OsString capacity as StdPlatformEncodedBuffer; neither may be converted
to a complete allocation or counted as free. No std header pointer is cast or
dereferenced. Qualified source/compiler/target/allocator proofs remain required
for those frontiers. Known subset functional overlap admission must preserve
these unresolved facts and is not whole root/SDK/OS/plugin physical peak PASS.

The actual mutable child, editor/controller ownership, current installation CAS,
canonical author fence, source delivery/Ack and DSP/PDC contracts remain their
sole existing consumers. This immutable factory observation creates none of them
and never grants a native owner or changes the audio deadline.

### Native observation wire and cleanup errors

The observation reply writer borrows the actual fixed observation value. The
shared decoder consumes each complete tagged payload directly into the requested
observation or binding result, including rejecting a mismatched reply after
consuming its payload. No per-poll observation Box is introduced: these queries
can precede PCM windows. The exceptional editor-cleanup error owns a boxed exact
target so ordinary audio result/error carriers stay bounded. That allocation
occurs only when returning the cleanup deferral, never for successful PCM.
The allocation ledger is consumed by value; its snapshot still includes the
historical overlap of its allocated owner and slot table, rather than claiming
a continuing lease or complete SDK/OS allocation coverage.


### CLAP thread-safe GUI refusal boundary

Clack's HostGui resize/show/hide wrapper now returns the exact implementation
bool for ordinary refusal without converting false to an error-formatting/Host
Log path. This avoids known wrapper heap requests when a plugin calls this
thread-safe API from its own audio or background role. The actual native bool
protocol and the host's independent GUI admission remain unchanged. Invalid
wrapper/panic paths retain their existing failure boundary.

`clap_gui_refusal_allocation` is an independent joined-background-thread Clack
ABI control of all three standard callbacks. Its allocation interval excludes
thread construction, teardown and test bookkeeping. It is neither a GUI/DSP
lifecycle receipt nor full native memory/deadline acceptance. The current
Windows main/GUI/parameter-output consumer remains separate qualification work.

### CLAP Windows GUI and the actual main/DSP lanes

The Windows CLAP adapter borrows the same retained `PluginInstance` into an
initializing-main GUI/timer role and an exclusive scoped DSP role. Both inline
and scoped PCM paths call the same borrowed signal algorithm. Main identity and
the participating DSP thread's symbolic audio role are instance-specific; a
background thread does not inherit either role. Native main service continues
through the existing bounded reader/command lane even with no PCM or incoming
commands. Windows DSP realtime admission uses the existing scheduling adapter;
an offline scope records no realtime registration.

The embedded Win32 GUI uses the existing format-neutral native parent/WndProc.
Host GUI requests use fixed scalar mailboxes with nonwrapping GUI incarnations
and resize publication serials. Create/scale/actual-size/parent/show and joined
hide/destroy/parent-destroy occur on the actual initializing main thread. A
`closed(true)` observation skips hide and still receives exactly one native GUI
destroy acknowledgement. Unresolved attachment retirement cannot unload its
plugin module. The shared native-object/binding/Runtime installation issuer and
the single Open/Poll/Close command stream are retained.

This layer advertises Windows `clap.gui` and `clap.timer-support`. HostParams,
paused flush output author admission, state dirty, rescan/clear, floating GUI and
other platform GUI remain separate required consumers; they are not completed
by a native window, generated ABI or a legal refusal. The owned Windows tests
exercise actual Clack ABI callbacks, HWNDs, idle main timers and one real DSP
call; until executed they are source only, and in-process controls do not qualify
an isolated child, vendor GUI, Undo/save/reopen, full resource envelope or audio
deadlines. Fixed slot/role/stack profile accounting retains its known-owner
subset boundary, including the explicit absence of extra Clack role Boxes.

### Nonterminal native author prefix, same-session Plan and Ack

The V7 VST main role owns a bounded ordered callback prefix and source status. Preserve does not consume it; Drain retains exactly one delivery until its exact old-source revision/binding and new installed binding are acknowledged. Official timestamp groups may finish while individual parameter edits remain open. A selected retained prefix may install while a newer callback/feedback suffix remains owned; the suffix is subsequently delivered in order. Unselected peer feedback, schema/restart changes, source incompleteness and missing producers are explicit refusals, not empty-source evidence.

An outstanding delivery holds PCM before old compiled constants can overwrite native edits. Accepted nonterminal Value batches promptly replace only canonical parameter bindings using the actual prepared whole tree, preserve Session/DSP continuity and release samples after Ack. No process position, PCM extent, Stop/setProcessing reset, Enter, state reload or fabricated continuity is introduced by that parameter-only handoff. A failed Ack leaves the new Plan installed and its exact prefix outstanding; retry preserves any newer suffix and cannot republish author history or install again. Source closedness for coherent state, save or configuration seeding additionally requires actual completed parameter/group boundaries and no unacknowledged delivery.

The owned attachment receipt has constant size across repeated parameter Plan changes. Only cleanup of that actual same native object may use the original attachment target after its author binding changes. Open and author writes still require the current installed binding. Real reload/revocation/object changes invalidate the receipt.

Resource accounting uses actual old/new prepared Plans, parameter scratch capacities, binding records and complete factory-owned allocations under the existing frozen Runtime grant. Arc payload and platform-encoded storage coverage, thread runtime/TLS/bookkeeping, OS pages and complete SDK physical peak remain explicit unqualified frontiers; this functional admission is not whole-root physical PASS. The shared issued AuthorExecutionSnapshot is allocated before final guards; its Arc control allocation is also a stated frontier. Cold targets/results use owned boxes outside successful PCM work, and those bodies are distinct from queue slot storage.

Qualification is split: portable real Runtime/Plan/Hosted contracts, portable actual Media/Core queue contracts, and matching-build native IPlugView/zero-sample-flush tests. A real view fixture and native same-object tests do not establish physical surface presentation or end-to-end App author save/reopen.

### Coherent terminal state on one retained Strip materialization

An exact retained terminal native author proposal may now prepare/install/Ack
on one changed PreFader or PostFader materialization. Its actual selected native
object already supplied coherent component/controller state after legal
processing-thread parameter flush and main-thread state capture. The canonical
Program stores those accepted bytes; the next immutable factory is rebound by
the existing pure seed API. This handoff performs no Stop, Enter, setState,
sample-position change or additional controller/plugin construction. The same
Runtime/Hosted ownership, frozen resource grant, author fence, current Source
claim and installed native Bind remain authoritative. Actual VST Ack retains
the captured bytes as that child's future Enter seed.

The original closed prefix is privately retained by its Hosted owner. Newer
Begin/Value callbacks do not invalidate acceptance of that prefix and are not
removed by its Ack. A failed Ack retains InstalledAwaitingAcknowledgement; retry
does not commit author state or install another Plan. PCM remains held for each
unaccepted newer suffix, then continues on the original Session after that
suffix's own parameter Plan/Ack. Terminal acceptance does not imply that the
source is globally closed while a later gesture is open.

Scope insertion or multiple changed live materializations remains the explicit
`CoherentPeerStateRequired` frontier. Publishing a future immutable seed cannot
pretend to update another current DSP object's private state. That workflow
requires real coherent peer propagation and legal lifecycle/continuity before
it can be supported. This refusal is temporary incomplete product capability,
not successful compatibility or whole native GUI editing support.

Qualification remains separate: portable tests exercise real Runtime/Plan/Hosted
and exact Source installation with an explicit eight-byte test ABI; a matching
Windows owned-view test checks real component/controller state survives future
Enter on the same child. These do not establish App Undo/save/reopen, full Scope
peers, physical Window presentation, complete SDK/OS memory or audio deadlines.


## Session storage preparation before actual processor creation

`AudioRenderSession::new` uses one internal storage constructor for node PCM,
source and parameter scratch, compensation delay lines, sidechain buffers and
meters. `PreparedProcessorHost::allocate_storage` performs the same canonical
scratch/extent checks and reserves occurrence storage without calling a factory.
Only the consuming population step creates exclusive processor instances. The
ordinary public constructor still returns a complete Unentered Session.

The private unpublished storage envelope carries no installed native binding,
author permission, DSP state or resource grant. Dropping it creates no processor.
Retained Scope/reprepare transfer will reuse these actual allocation owners only
after independent whole-root topology, native state-entry and frozen overlap
admission; that transfer and resumed transport are not completed by this step.
No second PDC or automation interpretation is introduced. Full SDK/std/foreign
physical ownership remains a separate qualification frontier.

### Explicitly unbound native parameters

Prepared native parameter metadata keeps one stable lane per definition, while
its optional prepared curve controls host event emission. An absent curve emits
an empty lane at every partition; no definition default, synthetic constant or
second evaluator replaces it. The existing curve builder, exact occurrence
owner-time offset and event summary lower present curves. Same-contract author
handoff may remove or recreate curves without deleting parameter identities,
changing signal topology or rebuilding the native object. Actual next event and
lane capacities still enter the existing frozen overlap grant before install.

The independent controls for this model cover ordered author operations,
transactional failure, real archive reopening, absent Inspector values and
compiled partitioned PCM. They do not qualify native callback origin, native
state capture/activation, GUI delivery, whole SDK/OS memory, or realtime deadlines.
HostParams advertisement requires its complete retained native-source and same
Poll/Session/Plan/Ack consumer, beyond this canonical reference prerequisite.


### Native parameter catchup is a pending observation

The same actual DSP role performs bounded zero-sample parameter catchup before
native author capture. Main continues servicing actual window messages while
that DSP operation completes, so a newer Value can remain in the owned feedback
sink after the operation. A validated `DspFeedbackPending` reply with a nonzero
actual feedback count and no outstanding Hosted delivery retains that source
for the next fair serial Poll. It is neither a contract failure nor a successful
author prefix. Sticky source failure, changed owner, mismatched delivery and a
pending refusal with zero actual feedback remain explicit errors.

A presentation-empty observation is never `ClosedEmpty` authority. Existing
current source checks still defer PCM and lifecycle/reseed operations before
mutation until the actual callbacks are accepted. No process loop is retried
inside one observation, no callback is erased, no PCM is fabricated, and no
sample clock or native lifecycle advances during control catchup.

The portable Runtime controls exercise pending source retention, exact later
Plan/Ack, continued samples and contradictory/sticky refusals. Matching native
Scope preparation still needs its independent two-child/window/coherent-state
positive markers before its target behavior can be classified as RED or GREEN.


### Retained CLAP parameter controls and observations

The Windows native main owner advertises HostParams and HostState with one
bounded source shared by actual Process/flush output and main rescan/clear/dirty
callbacks. Void callbacks retain their raw flags/order or make incompleteness
sticky; a full output source returns false without losing its accepted prefix.
request_flush is a separate pending bit. The same exclusive DSP slot performs
active idle params.flush without a dummy Process; inactive flush belongs to main.
The init/main OS pump continues during idle input and owns timers until plugin
unregister or teardown. No second decoder, plugin, controller or author queue exists.

Poll9 Preserve never consumes this source or captures state. Full Drain selects
at most 128 callbacks and stops at its first rescan, performs actual main
get_info/get_value/value_to_text queries, and retains the exact prefix until the
canonical Session, same-live parameter Plan and old/new-binding Ack accept it.
New callbacks remain a separate suffix, including while an Ack is retried.
ClosedEmpty also excludes flush requests and source publication in progress.
V8 is the sole wire schema; old versions refuse before native actions.

RESCAN_VALUES refreshes existing unkeyed references from actual queries without
recording points; keyed curves and explicitly absent references remain intact.
Clear removes host references in original order and never restores a descriptor
default. INFO names, modules, hidden and periodic facts are current presentation
metadata. Readonly/DONT_RECORD values retain their original flags/origin for
ephemeral UI display. Process author output carries the actual signed sample
position plus offset/rate into the exact Sequence/occurrence owner, without UI
time or occurrence-boundary clamping; flush has no invented sample timestamp.

Fixed source payload, inline controls, signatures and output Vec capacities enter
the existing Session scratch policy. Returned facts/strings, retained prefix and
state capacities have separate bounded owners; the single reply has an aggregate
control-frame bound. These bounds do not prove all SDK/std/OS/plugin heaps or
whole-root physical peak/deadlines. A metadata-critical RESCAN_ALL change remains
retained for a real reprepare candidate; this parameter-only handoff cannot change
schema/topology. Stateless terminal configuration, general author consumption
without an opened editor and whole-Scope terminal state-entry remain distinct
consumer work. Windows GUI/params source implementation does not qualify those
remaining features, non-Windows GUI or whole native realtime compatibility.


### Native parameter completion without a State extension

The same Poll9 author delivery carries an explicit `NativeAudioAuthorCompletion`:
`Nonterminal`, `ParametersOnly`, or `CoherentConfiguration`. Optional CLAP State
absence is distinct from a failed actual `state.save`. Only a selected prefix
covering the actual closed source can be a parameter-only completion; no empty
opaque bytes or synthetic End is created. The existing Session/Plan/Ack path
accepts parameter references and keeps the same child, view and DSP continuity.
Opaque state, when actually present, retains its separate peer propagation rule.

A no-State plugin's private dirty, Clear or Rescan controls remain retained with
an explicit configuration refusal until their real persistence/metadata path is
implemented. Parameter-only finality is not a general private-state guarantee.
Known capacity admission, SDK/OS/foreign heap frontiers and realtime deadline
qualification keep their existing meanings; this finality fact grants none.


### Parameter reference controls without a State extension

CLAP Clear updates host automation references. Nonstructural VALUES/INFO/TEXT
Rescan queries actual plugin values/metadata; neither operation requires the
optional plugin State extension. With no authored opaque payload, a closed
ordered reference transaction uses `ParametersOnly` completion and the existing
Session history/Plan/Ack path. A preceding Clear remains absent during VALUES;
schema defaults never become observed current values. Private StateDirty remains
unrepresentable without a real persistence mechanism, and structural ALL needs
the dynamic schema/state-entry candidate consumer. Neither boundary discards the
retained original prefix or fabricates an opaque configuration.

## Native shared state entry on the retained Runtime

Opaque native author completion for a shared Scope or repeated materialization
requires every actual affected Hosted record to prepare the same canonical
configuration. The sole Runtime enumerates its full retained tree. Typed Scope,
insertion, Sequence, local Plan/Session, occurrence, epoch, Source generation,
binding and actual child/load incarnation are checked before foreign work.
Unselected pending callbacks, transient parameter observations, publishing,
scheduled flushes and retained deliveries cannot be called ClosedEmpty.

Storage is allocated through the existing Session and Host constructors. Frozen
grant admission precedes all destructive native calls. Actual Vec/Box capacities
and complete factory allocations enter the existing alias ledger; old buffers,
new buffers, metadata and accepted child seed overlap remain owned. Unqualified
Arc payload/std platform/native/OS coverage is reported separately. This proves
the admitted known resource policy, not complete physical peak or realtime cost.

All affected VST children close the real view, restore through their one ordinary
native loader, verify the fresh load serial on the same child, and acknowledge
Bind before final author/Source guards. A partial failure keeps the whole root
held with exact old seeds and prepared receipts; retries do not restore completed
peers again. Removal failure retains its strict retirement boundary. The bounded
commit moves whole Hosted records and publishes fresh Sessions/leases. No plugin
factory creation, SDK callback, allocation or retired-owner drop occurs there.

This transition explicitly loses old private DSP continuity. Original-prefix Ack
on the fresh incarnation precedes fresh Enter at the accepted current sample.
That Enter consumes the already restored object once. CLAP peer restoration is
explicitly unsupported by this VST slice; a pure future factory seed is not
evidence that the current native DSP applied the state.


## Native state entry and installed object binding

The installed renderer's existing Core registration and current claim are the
only authority for native author and editor bindings. A private cold entry
context carries that proof and the actual root identity to each retained local
Session. Root Track/Bus/output instances bind after successful actual Enter;
Contribution and nested instances enter at their real first signal coordinate
and bind before Process. Each local Session keeps its own continuity epoch.
Ordinary PCM continuation performs no new Bind or whole-tree binding pass.
Queued renderer entry passes its actual cancellation token. Source validation
precedes native preflight and follows native Bind. Cancellation or a stale Source
revokes the entire accepted partial set; standalone DSP has no author permission.

Successful native Prepare establishes a private one-shot first-initialization
fact after state, mode, metadata and activation were accepted. The first explicit
signal entry consumes it and records the supplied signed coordinate on the same
CLAP/VST object and GUI. It performs no reset, reload, synthetic Process or state
capture. A later seek retains the real reset/reload path and its continuity
discontinuity. Pending author prefixes still require canonical Session/Plan/Ack
before entry. Previous view targets can resolve only to cleanup of the exact
same retained object and Sessions under the current Source; stale Open/author
leases cannot use this receipt.

The context and first-initialization fields are fixed host metadata. Their
actual compiled Runtime/Host/worker layouts belong in existing owning visitors;
cloned Source references share its existing allocation. They provide no new
grant for plugin heaps, OS resources or old/new physical overlap. Native deadline
and before-first-Play App admission remain separate qualification obligations.
The stopped Source/Scope recovery path retains its original prefix and current
schema; this entry slice introduces no alternate interpreter or state-entry tag.

Root entry can leave an untouched local Contribution/nested object waiting for
signal demand. Full author recovery for a new callback in that post-DSP lazy gap
must distinguish current UI/control binding from absent future execution-position
evidence; this slice does not claim that additional workflow is complete.


## Native observation binding before local signal demand

A root state entry may leave Contribution and nested objects awaiting their
first local sample. Their UI/control permission is bound to the installed root
Source, actual local Session and existing local continuity epoch, independently
of an execution position. The cold entry pass observes/binds these retained
objects without Enter, reset, Process or an invented sample anchor. A subsequent
actual lazy Enter revokes that observation binding and binds its new real local
epoch before Process. Normal PCM and silent gap windows do not repeat this pass.
Pending native callbacks and outstanding accepted deliveries still hold entry;
the observation binding cannot relabel a retained Process callback's position.
Source/cancellation checks surround the entire cold set; failure revokes all
accepted partial bindings rather than publishing a mixed ready prefix.

One wide native observation result carries an explicit attachment transition
before plain values and author proposals. A retained transition compares the
private actual attached object with the current observed child/load incarnation.
App refreshes its author target while keeping the original cleanup receipt for
already queued Close. A replacement clears view presentation and never opens a
new window. Repeated immutable target facts permit a cancelled/stale presentation
completion to be retried; they are metadata, not another owner issuer or queue.
Every later Open/Close/author request remains re-admitted by the sole Runtime.
The detach receipt is fixed Hosted metadata and the completion uses existing
cold ownership; compiled layout/physical observer qualification remains required.
This slice neither measures realtime deadlines nor grants unknown native/plugin
heaps, OS stack resources or whole-root old/new allocation peaks.
