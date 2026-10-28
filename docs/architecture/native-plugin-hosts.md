# Native-format Plugin Hosts and Effect Integration

This document describes the current Effect integration and native-format
OpenFX/VST3/CLAP plugin hosts. **Extension** is the formal name of the broader
platform; see [Extension System](extension-system.md) and
[ADR 0011](../adr/0011-use-managed-extension-platform.md) for its accepted target.
Effect integration uses EffectExtension* contracts and EffectType::External.
Native-format ABIs and authored definition identities remain separate. Native Worker crash/hang containment is not
evidence of the future Extension permission sandbox.

The current effect contract is visual-only and is not the VST3/CLAP host
ABI. Audio plugins adapt to the Audio Processor author/compiler boundary defined
in [Audio Pipeline](audio-pipeline.md); project files preserve their stable
native identity, parameters, expected buses, and opaque state without persisting
loaded binaries or runtime objects.

External audio processors use `mondrian-audio`'s Isolated Audio Processor Worker
boundary. A concrete VST3/CLAP discovery and ABI Adapter resolves an author
definition into one opaque Worker preparation payload; it must never load the
native module in the Mondrian process. One persistent supervised child owns each
mutable occurrence, while the existing Processor Host continues to own PDC,
sidechains, parameter timing, failure propagation, Playback, and Export semantics.
The generic boundary proves crash/hang/protocol containment only. It does not by
itself advertise that a native format, plugin, vendor UI, or security sandbox is
available.

## Native format admission

The current upstream VST3 SDK is MIT licensed; CLAP is MIT and OpenFX is
BSD-3-Clause. Product support may use their APIs while retaining each
dependency's license notices. The VST name and logo have separate Steinberg
trademark rules. Mondrian should load user-selected installed plugin binaries
and must not bundle third-party plugins as if the format license covered their
content. Verify the license of any reference plugin shipped with tests or an
installer separately. Sources: [Steinberg SDK README](https://github.com/steinbergmedia/vst3sdk/blob/master/README.md),
[CLAP repository](https://github.com/free-audio/clap), and
[OpenFX license](https://github.com/AcademySoftwareFoundation/openfx/blob/main/LICENSE.md).

VST3 now has a bounded class-discovery/probe child and a separate supervised
audio-worker entrypoint. Selected files or `.vst3` directory bundles resolve
to one canonical binary for the running architecture and SHA-256 revision;
class IDs and parameter IDs are independent of scan order. The worker reloads
the class, restores the authored state, selects the
requested realtime or offline mode, and rechecks one active mono/stereo main
input/output bus, parameter metadata, latency, and tail before processing. It
delivers normalized VST3 parameter points with sample offsets and rejects
events beyond the native host queue's 4096-point block bound. A continuity
entry creates a fresh instance from authored state. A real VST3 SDK Gain DLL
has passed isolated stereo processing, parameter change, and reset checks.
The App's hidden child dispatch, insertion UI, and persistent installed catalog
restoration are present. The reference Gain bundle passed both App Preview and
Export offline delivery with identical nonzero PCM samples. A separate stateful
Gain fixture passed component/controller state capture, restoration, and worker
audio after continuity re-entry. Broader vendor compatibility remains before
the product advertises VST3 support. Vendor
editors and auxiliary buses require separate qualified slices. Unbound
definitions record `null` and cannot authorize native execution.

Each offline Program Output owns its actual consumer Source in
`mondrian-export::queue::audio_delivery`. Exclusive preparation of temporary PCM
provides the quiescent block boundary. A deferred native restart uses the shared
Runtime's capture, canonical plan, Source installation and retirement, followed
by explicit Fresh entry at the next unwritten public sample. The installed
Source generation supplies the epoch; the adapter adds no DSP graph, clock or
project-state authority. A new hold during Fresh entry or its single render
attempt fails the job rather than starting an unbounded retry loop. Stems retain
their cold requirements and partition each remaining occurrence/fixed/logical
job quota in authored order before constructing the live set. The disjoint hard
slices sum to the original grant; each Runtime must admit its reprepare overlap
inside its own slice. This bounds the existing Runtime ledger, not all SDK,
foreign-plugin or outer-consumer allocations.

Each successful CLAP activation supplies one fresh signal entry, including a
retained restart. Reconfiguration revokes availability before stop/deactivation;
pending cohorts
and staged configurations still block entry until the original seed/Source
installation; later entries keep the native reset contract. Repeating reset
immediately after reactivation can otherwise cause a restart/reset cycle.

Retained CLAP parameter revalidation reads definitions through get_info, not
mutable current values through get_value. Definition failures explicitly mark
current_bits=unqueried. Complete cold snapshots and canonical value/author
projections retain their finite/range/step/readonly checks; raw presentation
values confer no author authority. No meter clamping or plugin exception applies.

Export observes Audio native-child settlement through Core's existing counted
registrar in the Queue's original state and lock. Observation starts before cold
precompiled factory work, including stem measurements and failed construction;
Sessions and actual child owners retain their own obligations. Missing forwarding,
counter faults and unconfirmed results permanently invalidate completeness.
Live schema3 and shutdown schema5 expose these facts; pending scopes and children
count as owned resources. This adds no teardown executor or DSP permission.
Confirmed exit/reap remains separate from graceful acknowledgement and OFX closure.

CLAP null/empty audio-port types retain their native arbitrary-audio declaration.
The format adapter explicitly maps fixed Main/output lanes by ordinal identity
to the requested Mono/Stereo render contract; one/two-lane arbitrary auxiliary
inputs expose the corresponding host Mono/Stereo lane contract. This assigns
host routing meaning without inventing native speaker metadata. Named types
still require exact widths, while custom types and other widths are refused.
Cold and stopped scans share this processing projection; no PCM mapping loop,
graph, author state or IPC field is added.

OpenFX discovery in `mondrian-app::openfx_adapter` loads installed binaries only
in a supervised child. An optional exported OfxSetHost receives the actual native
host before OfxGetNumberOfPlugins and OfxGetPlugin. Discovery returns bounded
image-effect API v1 headers; it does not issue per-plugin setHost, Describe,
CreateInstance or Render actions. The parent pins the canonical binary path and
full SHA-256 before/after the child and validates unique descriptors/deadlines.
A selected bundle resolves through its platform Contents/architecture directory
only when exactly one regular native binary is present.

The application builds the selected CPU image host on Windows, Linux and macOS from
BSD-3-Clause OpenFX HostSupport and product adapters. Separate description and
resident execution children run real native actions. Description preserves
Double/Integer scalars, numeric 2D/3D tuples, RGB/RGBA, Boolean, Choice, StrChoice
and String defaults, labels, hints, numeric bounds and animation/visibility.
Plain/Scale Double semantics are admitted; spatial Double, custom/parametric,
label and rich-text authoring need additional contracts. Numeric curve queries
come from the canonical author evaluator; constants and current-frame samples
retain distinct native query authority rather than pretending to be history.

The parent supplies pinned binary identity, explicit time/rate/range/PAR and
complete Float32 RGBA values with canonical Core association metadata for Filter.
Explicit Generator requests carry Output staging format and no incoming image.
Resident instances reuse supervised workers and exact-sized shared mappings
(Filter Source/Output, Generator Output only), with strict sequenced control packets
and borrowed cancellation/deadline authority. Native code borrows mapped rasters
only during legal actions; failed/unfetched output cannot publish an old frame.
No plugin binary is loaded into the editor, and ordinary renders neither launch
a process nor serialize frame files. The graph executor owns native factory
instances and charges controlled shared/executor storage; arbitrary private
plugin allocations are not a bounded-RSS promise.

Native qualification covers the CPU Float32/RGBA Filter/current-frame subset and
limited Windows owned Output-only Generator Direct/real NoImage graph execution.
The GeneratorProbe passes literal RGBA, retained-owner and pre-native frame13 refusal
checks; a separate unfetched-Output negative also passes. Untouched official Basic
authored/resident and SessionProbe capabilities regressions pass under their original
supervision with empty stderr and clean owners. This is not a fully verified Generator
context or third-party qualification; spatial Double/Normalised defaults required by
official Rectangle remain unsupported.
It uses owner-supplied extent/PAR/time without a fake Source. Catalog/Effect Browser
insertion remains Filter-only. Renderer CPU-plan admission and typed composition
reject reachable CustomSource nodes with `SourceMaterializationUnsupported` before
their provider/raster work; a compositor caller may already have materialized media.
Preview root/nested routes reject through their CPU blocker (`NoCompleteRootRoute`
at root). Export uses the shared plan preflight and converts its error to text;
that path is source-reviewed, without an independent negative runtime gate.
Dead source nodes remain admitted.
This does not add a Timeline source content type. Tiles, temporal access, GPU,
General/Transition/Paint/Retimer and custom/overlay interactions remain unavailable.
The untouched pinned official Basic and the separately owned SessionProbe
qualify actual pixels, automation, instance reuse, edits/seeks, cancellation,
failure isolation and selected contracts. A successful Basic run cannot qualify
all vendors, features, platforms or throughput. Bottom-left native coordinates
use negative row bytes over the product's top-row-first raster; canonical
project/clip geometry includes PAR while image bounds remain pixel coordinates.
`tests/openfx_reference` builds external licensed fixtures without shipping them.

`mondrian-app::openfx_effect` can now register one explicitly selected and
described Filter as a Float32 CPU definition in the shared compiled visual
effect graph. Stable parameter IDs derive from the plugin identifier and OFX
parameter name; the definition displays the plugin's bounded UTF-8 label and
captures defaults, hard/display numeric bounds,
animation capability, order, and exact binary identity. The persisted Effect
key includes the plugin identifier, declared version, and full binary SHA-256;
a changed binary therefore has a new identity and cannot reinterpret an
existing Clip instance. Hidden or initially
disabled controls retain their plugin defaults without becoming editable.
The graph evaluates author values at exact Clip-local time, lowers the bound
sequence frame rate, source range, and sample aspect ratio, and invokes the
supervised child through the existing Float32 processor seam. Its cache policy
is uncacheable and determinism conservative until a vendor-specific contract
is qualified. The official Basic reference test now also compares direct host
pixels with graph execution when that external fixture is supplied.

The Graphics menu selects one installed `.ofx.bundle` directory through the
native folder picker. A closed product action queues the selected bundle in
the same background catalog worker used for startup restoration. That worker
inspects the bundle, describes its image-effect entries, and registers only
admitted Filter definitions; the Effect Browser then exposes those definitions
for ordinary Clip insertion. Successful machine-local selections are saved in
app UI preferences only after the completed worker result is polled. A failed
scan cannot leave a persisted selection; dismissing the native picker queues
nothing. A changed binary may offer
a new Effect definition, while authored instances retain their old key and
fail closed until the original revision is available. The
project does not embed native plugin binaries.

Broader product admission still needs plugin page/group layout and dynamic
control enablement in Inspector metadata, representative vendor-plugin
qualification, native interactions and measured performance across supported
capabilities. Resident execution and worker reuse are already implemented.

Broader image-effect support needs qualified property, parameter, clip/image,
memory, progress, threading, and render suites across vendor plugins.
Admission must preserve the same compiled visual graph semantics
for Viewer and Export, run native plugin code outside the editor process, and
compare real reference-plugin pixels over time, color, alpha, and failure cases.
An effect definition alone does not qualify OpenFX support.
The visual processor seam admits an explicitly bound CPU Float32 callback
and charges one full-frame scratch image for transactional execution. This is
an internal prerequisite for the host; it does not load or execute OpenFX
binaries by itself.

An isolated host feasibility check used OpenFX upstream commit
`e40728885390ec16276d11e00025de9b4282060c` and its Basic Gain example.
The upstream HostSupport library and demo host were built outside the product
tree. Replacing the demo's directory scan with `PluginBinary` for one selected
`.ofx` file still completed `Load`, `Describe`, instance creation, and `Render`
with `OFX_PLUGIN_PATH` pointing to a nonexistent directory. The locally
modified demo passed RGBA Float32 rows and the reference effect returned its
frame-1 first pixel as `(0.988235, 0.988235, 0.988235, 4.0)` from an input of
`(63/255, 63/255, 63/255, 1.0)`; its fixed parameter getters supplied two
successive gains of 2. The alpha value above 1 confirms this check did not
silently clamp or quantize the working frame. This proves the selected-binary
HostSupport route can execute the reference action at Float32 precision, but
the demo's fixed 720×576 images, parameters, PAL timing, and PPM output still
violate Mondrian's working-frame contract. None of its output is admitted as
product OpenFX rendering. The product Adapter must replace those demo
contracts with typed frame geometry, row stride, color/alpha intent, frame
time, and authored parameters before registration.
After replacing the demo's fixed getter with descriptor defaults and mutable
parameter instances, authoring `scale = 1.5` before `CreateInstance` and
disabling per-component scaling produced the Float32 pixel
`(0.370588, 0.370588, 0.370588, 1.5)` from the same input. This checks the
parameter-suite path separately from the hardcoded-gain render check; it is
still an external feasibility fixture, not a product Adapter.

## Effect Extension Integration

Externally registered effects are represented as `EffectType::External(String)` and registered through `EffectDefinition`.

Effect Extension definitions may provide:

- display name and category path
- default properties
- graph builder
- branching graph builder
- custom render backend
- cache key/policy
- extension contract/runtime availability metadata

The `EffectGraphDsl` exposes source/current nodes, unary ops, branches, blends, and masks.

## Runtime Safety

OpenFX discovery and catalog publication ownership are specified in
[OpenFX catalog lifecycle](openfx-catalog-lifecycle.md). Catalog workers prepare
definitions; a live App owner publishes them. Cancellation reaches identity reads
and both isolated discovery/describe waits, and catalog destruction settles its
worker. This direct-child contract does not qualify other contexts, temporal
images, native GPU interoperability or plugin-created descendant processes.

Graph builders and custom processors execute against staged state. Builder
errors/panics return `EffectGraphBuildError`; processor errors/panics return
`EffectExecutionError`, and neither commits partial graph or pixel state.
Failures are recorded through the extension runtime-status path. The runtime
failure policy decides only whether the definition remains callable; the
library policy decides only new-insertion visibility. Neither policy permits a
failed instance to become identity output. Extension code must still avoid panic
across the host boundary.

## Future Extension integration

The [Extension platform](extension-system.md) will manage contributions and
public workbench interfaces after the intended native-format host work. Web UI
uses real HTML/CSS/DOM in isolated surfaces, with host theme tokens and scoped
APIs. Rust entries also require restricted execution; shared Rust Widget trait
objects are not a public cross-binary UI contract. Neither runtime directly owns
AppState, OS handles, or renderer internals. Processing stays with its owning
Effects/Audio domain and qualified native Adapter.

## Native-format adoption order

Current status: the Timeline recognizes CLAP/VST3 definition identities.
`mondrian-audio` has a supervised processor-worker contract and a CLAP child
factory; the application dispatches its packaged executable into this worker
before initializing graphics or media. A parent-side CLAP registry can refer
to installed definitions without loading native code. The child loads a CLAP
library, checks descriptor identity, one main Float32 input/output
port and bounded auxiliary inputs with the requested mono/stereo port type and channel count, latency, and
tail, restores supplied
opaque state through CLAP's state extension before activation, and processes
interleaved blocks through planar CLAP buffers. It accepts numeric CLAP
parameter lanes with stable IDs and exact author-time automation; the worker
rechecks probed metadata and emits sorted sample-accurate CLAP value events.
Auxiliary input keys use stable native port IDs; discovery and admission share
one scanner after restored-state callbacks. The worker feeds distinct planar
ports through the existing isolated bus contract and shared PCM transport,
with buffers allocated during preparation. It rejects incompatible layouts,
duplicate IDs/main roles and changed auxiliary order. Source ABI fixtures
cover two distinct sidechains and subsequent silence. Clean ae3c78f packaged
worker/strict owned stereo DLL qualification also covers restored port state,
activation-valid latency, PDC, routing Undo and Unicode archive output.
Mixed layouts, other vendors and formats retain their separate open scope.
Descriptor discovery executes one installed library in a separate deadline-bound
child and validates its bounded response. The same child can probe a selected
definition for exact Mono/Stereo port, latency, and tail facts after restoring
its author state. `DiscoveredClapAudioProcessorSpecResolver` binds explicitly
selected libraries to those probes during preparation; the processing child
checks the facts again before admission. The App's Preview, idle warmup, and
Export queue share one injected resolver backed by the same session catalog.
The mutable `InstalledClapAudioProcessorSpecResolver` scans outside its lock,
publishes every descriptor from one selected library atomically, and rejects
duplicate plugin IDs from another path without altering the old snapshot.
Reselecting the same path replaces its definitions, while a preparation keeps
one immutable snapshot and re-probes the selected binary.
Discovery records a bounded SHA-256 fingerprint of each selected binary and
checks it again after descriptor discovery, after contract probing, and inside
the processing child before native loading. A changed installed binary fails
admission rather than silently changing the sound between Preview and Export.
Each newly inserted CLAP processor also persists the selected binary SHA-256
inside its definition reference and a bounded name/visibility snapshot for each
editable parameter. The latter is display-only, so automation and execution
remain keyed by CLAP parameter ID; hidden parameters stay authored but do not
appear in ordinary controls. Both Preview and Export compare that authored
revision with the installed registration before preparing a worker. An
explicitly unbound definition remains editable but needs explicit rebinding
before native execution.
The Inspector/Mixer exposes an explicit rebind action on each CLAP instance.
The App re-probes the installed plugin with the instance's saved opaque state,
requires the complete parameter schema and stable plugin identity to match,
then submits one optimistic Rack edit that changes only the binary revision.
The edit preserves processor identity, bypass, parameter curves, and opaque
state and participates in Undo/Redo. Missing binaries, changed parameter
schemas, locked Tracks, and stale edits leave the Project untouched.
The generic external Rack-edit transport rejects `RebindClap`; only the App's
probe-backed product action may submit that Timeline mutation.
Installed reference-plugin tests verify that the Preview and Export audio
adapters each launch the real CLAP worker and produce `+0.125/-0.125` samples
from the same nonzero stereo WAV pattern with saved 0.5 gain. The shared Runtime
compares realtime and offline modes over different block partitions with a
`1e-6` per-sample tolerance. A local CLAP fixture verifies that parameter
events retain their sample offsets at the ABI. The Clack Gain example receives
multiple events but applies each gain to the whole buffer, so its output is
checked against that implementation rather than used as a sample-accuracy oracle.
The Inspector and Mixer insertion menus expose session-installed definitions
and a native picker for an explicitly selected CLAP binary. The App probes a
selected definition in the isolated helper before submitting one Timeline Rack
insert transaction. Canceling the picker changes neither catalog nor project;
a failed scan retains the previous catalog. A successful selection records its
canonical path in bounded machine-local UI preferences, never in `.mdp`.
At restart, the Host schedules those paths on a dedicated background worker;
each library is re-scanned in its isolated child and publishes only after
validation. Missing libraries report a visible restore failure. A changed
binary may scan successfully but fails the Project instance's fingerprint
binding during audio preparation; the App surfaces that preparation failure
and clears stale audio. Neither case changes project author state.
Installation and restoration do not
advance the project author generation. Automatic installed-path enumeration,
plugin management and relocation UI, state capture, expanded parameter-control UI,
and broader vendor/plugin signal-parity coverage remain required before
CLAP is advertised as a complete product feature. The VST3 audio backend can
host an explicitly selected binary or directory bundle through installation and
insertion flow. The selected OpenFX Filter host now registers admitted
definitions in the visual Effect registry and exposes them through the Effect
Browser; its current contract is limited to the qualified Filter/Float32
subset described above.

The ignored installed-reference acceptance tests use a built `mondrian`
executable and the separately built MIT/Apache licensed Clack gain example
DLL. They check dynamic-library discovery, render-contract probing, resolver
payload/state binding, and Float32 signal processing at 0.5 gain through the
real child protocol; the in-process
fixture separately tests continuity, sample values, and deactivation.

The first real audio Adapter should host a user-installed CLAP effect inside
the existing isolated Processor Worker. CLAP's C ABI and MIT-licensed headers
make it a small first proof of discovery, bus/port negotiation, processing,
state round-trip, deadlines, crash containment, and Preview/Export parity.
Audio effects use explicitly negotiated Float32 buffers and channel layouts.
The capability matrices scope finite instrument, event and embedded-editor
support; unimplemented extensions return typed reasons.

On the Windows full-owner CLAP path, first new Idle editor Open exchanges
strictly empty-key canonical Some values after joining DSP and before creating
the view. The original active processor temporarily owns the exclusive audio
role for flush; GUI work remains on the main role. Valid output and new flush
requests stay in the existing Poll/author/Ack source. Unsupported selected
parameters refuse before mutation; later native failures remain terminal.
No values are sent for None/keyed curves or an already attached view.

VST3 follows through the same supervised Worker transport with its own child
entrypoint and the pinned MIT-licensed `vst3-host` crate and MIT/Apache licensed
`vst3-rs` bindings. Its Adapter must honor component/controller separation, bus activation,
parameter timing, latency, tail, state, and offline/realtime process modes.
Earlier SDK releases have different terms and are not an implicit substitute.
The host must not ship third-party plugin binaries. Keep required SDK copyright
and license notices, and review the trademark rules before using VST branding.

The admitted OpenFX Filter path runs a user-installed CPU Filter behind a separate
supervised worker, with frame timing, pixel aspect, Float32 RGBA, and authored
typed parameter values and canonical curve sources lowered into the Effect graph. The official Basic
Gain reference plugin passed direct host and graph pixel comparison on Windows
at the pinned upstream source revision above. Broader depth/component/
premultiplication, render-scale, temporal, region-of-definition, and
region-of-interest behavior still requires vendor qualification. Unsupported
color and alpha contracts must fail admission. Never silently round-trip through
RGBA8 or substitute a differently interpreted effect in Export. Native GPU
texture sharing and vendor overlays are separate qualifications.

LV2 is a later Linux audio Adapter. Apple Audio Units belong to the macOS
platform Adapter and hardware/release qualification. Do not plan VST2 as a new
integration: the SDK was discontinued. AAX requires its own Avid agreement and
distribution conditions, so it is outside the open-format path. Each
third-party plugin retains its own license; support for a format does not
authorize bundling a plugin.

Every native Adapter needs a versioned corpus with one openly redistributable
reference plugin, a host-versus-reference output comparison at matched sample
or pixel contracts, automation/state save-reopen, Preview/Export parity,
missing/upgraded plugin recovery, crash/hang quarantine, and a declared
tolerance. Record the plugin binary identity and input/output contract in
evidence. A "loaded" result without process and output evidence is not
qualification.

Format and license references: [CLAP](https://github.com/free-audio/clap),
[VST3 licensing](https://steinbergmedia.github.io/vst3_dev_portal/pages/VST%2B3%2BLicensing/Index.html),
[OpenFX](https://github.com/AcademySoftwareFoundation/openfx),
[LV2](https://github.com/lv2/lv2), and
[AAX](https://developer.avid.com/aax/).

### Native OpenFX session ownership

The C ABI creates, renders and destroys an opaque session exclusively inside a
supervised child. A session owns frame facts, host, local PluginCache, ImageEffect
API handler, bootstrap module reference, selected binary and effect instance in
that lifetime order. Source/Output borrow the owner's mapped frame ranges. Destruction reverses the order. Neither selected rendering
nor description registers stack-owned handlers in the process-global cache.
Clips and typed parameters read their owning instance's frame facts; no global
mutable raster or timing variables remain.

Reuse binds the exact raster extent. Every request applies a complete typed
parameter/query-source snapshot; omitted controls revert to descriptor defaults. Validation precedes
parameter mutation. Changed values are assigned before callbacks and bracketed
with BeginInstanceChanged/InstanceChanged/EndInstanceChanged. Time changes use
ChangeTime; edits at the same instant use ChangeUserEdited. Clip metadata changes
also notify Source/Output and refresh preferences. Output clip/image metadata reports its negotiated Straight, Premultiplied or
Opaque representation. Source clip/image metadata reports the prepared caller's
actual straight, associated or opaque representation; a render cannot mutate
that immutable association. SDK default clip preferences derive Output defaults
from this actual Source metadata. The parent never unassociates Source through
zero-alpha emission.

Native C ABI and resident responses resolve complete `ColorFrameAlpha` values:
StraightFloat, PremultipliedFloat or Opaque. `EffectFloatFrame` owns extent,
precision, RGB domain, actual association and the one complete raster. Raw finite
Straight/associated pixels retain signed alpha, alpha above one, HDR/negative RGB,
positive subnormal alpha and zero-alpha emission. Opaque ignores its physical
alpha component and publishes canonical alpha one with unchanged finite RGB.
Unknown preference metadata is rejected before Render. Nonfinite RGB or required
Float alpha fails before publication and retires the faulty native worker.

`EffectAlphaContract::NATIVE_FLOAT` admits all current Core representations and
negotiates only those three truthful native outputs. Provider declarations are
retained in the sole compiled graph and signatures. Scalar execution, Normal
Over, nested working frames and current persistent artifacts preserve the actual
association. A coverage consumer materializes only representable normalized
straight values; extended alpha or zero-alpha emission returns a typed consumer
refusal outside native DSP. That refusal does not disable the definition or
retire a valid processor. A failed heterogeneous frontier cannot replay a
stateful provider through a fallback graph.

CPU working-identity conversion retains actual association for an identity
request; an actual identity change materializes its declared coverage contract
before OCIO. CPU output conversion reuses its existing unique output raster clone for this
explicit materialization before OCIO and validates the complete result after
OCIO; a legal matrix may overflow finite HDR input. Ordinary Basic positive coverage remains
usable; Opaque delivery closes full finite associated images with the sole
Normal Over law before stock OCIO/output encoding. GPU transfer admission is
explicit `FiniteStraightCoverage`; it is not a claim that Float division and
reassociation are bitwise reversible. Native/cache raw preservation has a bit
oracle; conversion/Over has independent numerical oracles. Artistic blends,
resampling and temporal tile routes retain explicit coverage semantics in this
slice. This does not qualify all OpenFX contexts, extensions or arbitrary vendor
plugins. The current independently evaluable route rejects required
sequential rendering and temporal clip access at description, creation and
render admission; optional sequential optimization remains legal. Begin/render/
end actions explicitly report nonsequential execution during edits and seeks.
The output allocation is reused and cleared, with a per-render
fetch flag preventing publication of a previous frame. Native failures poison
the handle; subsequent calls fail until the owner retires and recreates it.

The standalone single-frame API uses the resident protocol and retires after
one frame. Authored graph execution uses prepared native factory instances with
session reuse, borrowed execution control and coordinated frame-memory grants.
Vendor sequential/temporal-state support remains a separate contract.
The official Basic Gain child qualification
covers repeated edits, seeks, changed input and pixel aspect, omitted defaults,
nonfinite input, extent rejection and repeated native destruction/recreation.
The separate product-worker test checks discovery and compiled-graph pixel
parity with the same admitted Float32 Filter contract.

An instrumented external Basic wrapper separately checks nonsequential action
flags and nonempty change-action groups. Seven supervised cases cover normal
and optional sequential requests, required sequence/temporal rejection,
instance/runtime contract changes, and a plugin returning successful Render
without fetching a new output. This hostile contract fixture complements the
untouched official pixel reference; it is never part of the product package.

### Resident OpenFX control and frame transport

OpenFxFilterSession owns one supervised child, an ephemeral loopback listener,
a control stream and immutable context-sized shared storage: Filter has Source
and Output ranges; Generator has only Output, with no Source allocation or copy.
The child proves a fresh random nonce and protocol version before receiving the
selected binary or mapping path, validates the preparation envelope and exact
file size, verifies
the selected binary SHA-256 and reports readiness. Plugin stdout/stderr are not
part of the protocol. Native initialization is deferred to the first authored
frame; later requests retain that same instance and shared ranges.

Control packets have a length prefix, strict typed JSON schema and a two-MiB
limit checked before allocation. Header/body progress survives short socket
poll timeouts. Every frame has a monotonically checked sequence; input belongs
to the parent before submission and output becomes readable only after the
matching successful response. The child has exclusive mapping access during
native processing. Idle child reads may wait for the first packet byte; once a
packet begins, its remainder has a finite deadline. Responses are likewise
bounded. Binary fingerprints remain verified before/after parent rendering and
before child rendering; fingerprint optimization requires separate evidence.

Parent I/O checkpoints borrow the owning execution checkpoint and observe child status
and operation deadline. A canceled, failed, malformed or timed-out operation
terminates and reaps the child before output publication or mapping retirement.
Graceful shutdown drops the native instance before acknowledging closure, then
verifies process exit; a stalled shutdown is killed/reaped under a short cap.
The authored definition binds an immutable processor factory with an explicit
input role. Its instances belong exclusively to each execution Session and author EffectId;
preparation/rendering borrow EffectProcessorControl and propagate the observed
scheduler stop through the transport. Transport/native faults remain faults,
including the adapter's own timeout without an observed owner stop. This
transport does not add a process-global worker pool or sequential-state
interpretation.

Resident preparation and rendering now take the owning scheduler's borrowed
checkpoint directly. Both core cancellation tokens and the effect executor's
sticky deadline/cancellation control can drive the same transport. No adapter
creates a mirror token or listener thread. Owner stops kill and reap the child
before the shared mapping is released or a frame is returned; native hang
qualification covers cancellation and a shorter owner deadline independently.

Filter Source and Output handles borrow the current mapped frame only during legal
OFX actions; Generator exposes Output alone. Each fetch creates an independent
property handle, sharing raster storage without owning it. Filter Source is
read-only and Output writable; Generator has no incoming raster or Source clip.
RAII retires caller addresses on every initialization/render exit, including
exceptions. Source can be fetched during Render and user-edited change actions;
Output is available during Render only. Its first fetch clears the current
staging range once under a mutex. Unfetched or rejected frames cannot reuse an
old Output, and no cached image wrapper keeps a prior caller address. An atomic
per-session live-handle count is checked at each image-enabled action boundary.
An unreleased handle makes the session fail before publication; remaining
callbacks cannot fetch images, and the supervised worker is retired instead of
reusing the invalid instance.

Shared-frame copies poll the owner at one-MiB intervals and again immediately
before returning preparation or rendered output. Qualification also transfers
an asymmetric 257-by-257 raster spanning a full and partial copy block, then
seeks and reverses its rows/pixels to reject stale or misordered Source data.

This ownership follows the [official clipGetImage contract](https://openfx.readthedocs.io/en/main/Reference/api/file/ofxImageEffect_8h.html):
each fetch returns its own action-scoped handle, while underlying image data
may be shared. Image handles must be released before that action returns.

### Authored OpenFX execution instances and frame admission

OpenFxProcessorFactory retains the helper path, pinned inspection and selected
identifier/context, and starts no worker during graph evaluation. Each admitted
native instance owns one OpenFxFilterSession. Filter binds SingleImage; the explicit
Generator registration binds NoImage and source-output alpha admission. Parameter
snapshots and exact frame context still come from the canonical author graph;
edits and seeks do not use an alternate interpretation. For Filter, the executor
moves its atomic input stage into the host envelope, adding no input raster.
Failures restore that stage before propagating the typed error; the caller's published frame remains
unchanged. Session eviction, errors and observed owner stops retire the child.
Changing only the frame generation preserves independent-frame instances;
author occurrence, registration revision and raster extent remain exact keys.

The controlled native raster charge is context-specific: Filter retains two
shared ranges and peaks at three rasters with its returned output; Generator
retains one Output range and peaks at two. Both add the same fixed native owners;
executor staging and live graph values are charged separately before worker creation.
Shared physical pages are counted once. NoImage completion may not return capacity
above its actual admitted staging allocation. The post-call check rejects before
publication or retention; it cannot prevent arbitrary Rust provider allocation
before return, and extra temporary copies belong to the factory active peak.
Heterogeneous preparation includes active recipe charges
in cpu_required_working_bytes(), which Export's existing attempt contract
consumes; the batch scheduler must not see pixel scratch alone. Frame sizing
has the same extent/size validation as the transport.

This logical frame grant excludes executable mappings, control-plane metadata
and arbitrary plugin-private heaps. It is not a physical process RSS limit.
Production grants now explicitly bound optional native residency per execution
owner. Machine-class/pressure decisions feed scalar Preview, CPU-prefix and CPU
fallback Sessions; each may retain at most half its class working envelope,
with 2/4/8 instances on Minimum/Standard/Professional and none on unknown or
below-minimum machines. Three Preview banks therefore have an explicit aggregate
idle ceiling of 1.5 working envelopes, independent of pixel-cache grants. Export
freezes its own single-bank instance/byte grant per attempt. Active execution
still counts resident siblings and current native charge within the unchanged
working grant. Reconfiguration evicts before allocation; background owners
apply frozen requests serially and trim to the next admitted request, so an
in-flight accepted frame does not inherit a later pressure reduction.
Factory-backed ephemeral execution still uses
the same frame admission and owner cancellation. Standalone qualified Sessions
with explicit retention reuse workers without definition-global mutable state.

The SessionProbe authored-graph qualification returns no Output on its second
native render. The first frame succeeds, the second fails and retires the
instance, and a new generation succeeds with fresh state. This distinguishes
native instance reuse from starting a new process every frame. The mixed
CPU/GPU-prefix qualification interrupts the five-second native hang at the
scheduler's deadline, preserves the input and leaves runtime quarantine empty.

Ordinary Float32 Session methods now require the borrowed execution owner as
well. Renderer media, solids, adjustments and transitions forward that same
control; Preview CPU fallback and nested materialization use the authoritative
generation token, and Export uses its existing job token. Native owner stops
retire retained instances, preserve source pixels, and never quarantine the
plugin. Identity/cache outputs and final compositor publication also poll the
owner. The product regression exercises the five-second native hang through
both scalar and ordinary Timeline composition, including exact deadline and
cancellation reasons. These are cooperative boundaries, not hard deadlines for
arbitrary builtin color/blend kernels or arbitrary plugin heap allocation.

The heterogeneous preparer still rejects a CPU-only graph ending with a GPU
upload and no GPU dispatch suffix; such a route must not be assumed to migrate
native work to that executor. Additional format/vendor and resident pressure qualification remain outstanding.

The pinned official Basic authored-UHD qualification executes a 3840x2160
unclamped Float32 frame through the production factory. Two executor rasters
plus three host rasters require 663,552,000 logical pixel bytes; the complete
controlled native thread/mutex owner reservation is additionally charged by
the sole native sizing getter. A one-byte-short total working grant rejects, and a 768 MiB grant succeeds with all pixels and distinct
corner samples preserved. Decoded input, metadata and arbitrary native heaps
remain separately owned. This is active frame/pixel qualification, not proof of realtime throughput or
DCI/8K support. Production retention grants are described above and require
their own pressure and residency evidence.


### Exact scalar Integer parameters

The selected Filter adapter now admits OFX Integer parameters as signed 32-bit
values, alongside Double and Boolean. Native descriptors retain hard/soft bounds,
defaults and animation flags. Rust rejects fractional/nonfinite/out-of-range
bounds; author controls use canonical `PropertyValue::Int`, a one-unit editor
step and the declared hard range. Shared author curves evaluate to exact integer
values before worker serialization. ABI kind 3 carries an exactly representable
integer through its double slot; the native owner checks kind, integrality and
range before assigning the complete snapshot or issuing change callbacks.

Integer parameter instances store their value independently per native session.
Current-frame `paramGetValueAtTime` returns that authored snapshot; other times
still return MissingHostFeature rather than pretending to provide an animation
history. Choices, multidimensional controls and arbitrary-time parameter queries remain
separate capability work. The real SessionProbe Integer mode reads the
Integer via the OFX suite and multiplies every output channel; product acceptance
checks native bounds/type rejection and a canonical animated integer midpoint
through the shared compiled graph. This is a contract probe, not a vendor matrix.


### Plugin-preferred scalar interpolation

Scalar Integer/Double descriptor properties register `kOfxParamInterpType` with
its normative Linear default. The description worker retains ConstantStep,
Linear or Smooth as the canonical Hold/Linear/Bezier preference. Unknown values
reject the description rather than silently choosing a different curve. This
preference becomes the first admitted mode in the shared author parameter
contract. Plugin preferences govern new keys only; users can still choose other
admitted modes and existing authored keys are not reinterpreted.

Interpolation metadata is writable on descriptors and read-only on native
instances. The property suite enforces read-only flags for scalar/array setters
and reset, while internal host property writes retain their own authority. A
real probe sets Hold for an Integer and Smooth for a Double, attempts all three
forbidden instance mutations, verifies unchanged metadata and executes the Hold
curve through the product compiled graph. A separate invalid-metadata probe
requires description rejection before registration.

## Native Choice parameters

The matched native description ABI copies bounded Choice labels and optional
signed visual ranks into owned Rust metadata. Values retain their original
zero-based integer indices; effect properties use those indices as enum keys.
Display sorting never changes canonical option ordinals or authored values.
Duplicate captions are allowed; invalid defaults, mixed/duplicate ranks, gaps,
and numeric bounds on Choice are rejected before catalog registration.

The native instance stores its integer value and validates reads/writes against
the instance's current option count. Non-animatable snapshots are constant at
any finite query time; potentially animated values currently require the current
frame until their complete temporal author binding is implemented. Snapshot application validates all controls before assigning values
and sending grouped change notifications. Render callbacks use timed reads;
current-value reads and writes are exercised in change callbacks.

The external pinned official Basic wrapper qualifies indices 0/1/2, default
restoration, same-instance edits, invalid values/types, signed extreme display
ranks, and equal direct/compiled-graph Float32 pixels. An invalid plugin write
in InstanceChanged preserves the current index. This does not qualify
animated temporal access, dynamic option rebinding, or the wider vendor matrix.

## Native String Choice parameters

OpenFxParameterValue is the sole typed native value model, including StrChoice
strings; no scalar-name alias remains. Both Choice families carry canonical keys
separately from captions and signed display ranks. Integer Choice keys encode
original indices; StrChoice keys copy the declared ChoiceEnum strings. The matched
ABI bounds and copies borrowed enum/default metadata during its callback and owns
value CStrings for each serialized resident call.

HostSupport registers the StrChoice type and Option/Enum/Order properties. Native
instances retain string keys, reject unknown assignments, and provide timed access
at any finite time for non-animatable snapshots, or only at the current frame for
potentially animated controls. Unspecified StrChoice defaults select the first enum
per SDK semantics. Description validation rejects mismatched arrays, unknown
defaults and duplicate keys. Effects author Enum values using the original keys
and pass those same strings to the prepared native processor.

Unicode/invalid-key unit tests and seven real native scenarios qualify current
Choice/StrChoice values, explicit/unspecified defaults, same-instance edits,
compiled-graph pixels and malformed-description rejection before registration.
String default storage, choice arrays and descriptor-owned strings outlive the
synchronous metadata callback; Rust copies the borrowed payload before returning.
Full temporal, dynamic metadata, editor/interact and vendor-format qualification
remain separate requirements.

Invalid scalar query times return ErrValue without changing caller output.
HostSupport Boolean varargs reads publish only successful values, avoiding
uninitialized reads after a rejected query. String varargs reads likewise retain
caller output on failure. The owned discrete probe now asks past/future fractional
times and NaN/infinity for static Double, Boolean, Integer and Choice/StrChoice
controls. Three real native scenarios qualify those static queries; full
animation/temporal support still requires authoritative author-curve evaluation.


## Native String parameters

The matched native/Rust description ABI preserves all six SDK String modes,
including the independent FilePath/DirectoryPath existence policy. Unknown modes
or non-boolean existence flags reject admission. String values remain exact UTF-8
(including empty values, CR/LF, tabs and RTF markup); embedded NUL is rejected.
A single value is bounded to 64 KiB, and the combined String/StrChoice snapshot to
1 MiB. Defaults and complete authored snapshots are validated before the plugin
CreateInstance action or grouped change callbacks. Omitted controls reset to declared defaults.

The native String instance implements bounded reads/writes and constant static
reads at any finite time. Invalid times and oversized/null writes preserve values
and caller outputs. Animated historical access remains a separate temporal binding.
Resident transport owns its CStrings through each synchronous native call and
reuses the same supervised worker across accepted edits.

SingleLine and MultiLine bind to the shared author Text modes. Label and RTF modes
are described and executable by the native session but reject product registration
until faithful read-only Label and rich-text authoring contracts exist.
They must not silently become generic editable text. The owned reference wrapper
qualifies metadata and String-dependent Float32 pixels through the real SDK suite;
this is not a complete third-party plugin or native editor qualification.


Every concrete native parameter instance receives the owning FilterEffectInstance
as its SDK parameter set. `paramSetValue`/`paramSetValueAtTime` notify that owner
after successful writes; leaving the optional base constructor argument null
breaks the suite even when the stored value itself is valid. The external probe
checks successful scalar, Boolean, Integer, Choice, StrChoice and String writes
in CreateInstance, where the SDK correctly suppresses change notifications until
creation succeeds. Vector/color instances now retain the same owner, but their
value execution remains unimplemented and is not qualified by these tests.


String-mode deserialization uses exact tagged object shapes, including modes
with no payload. An empty struct wire variant enforces surplus-field rejection;
Serde internally tagged unit variants alone do not enforce that requirement.
The public typed modes and their canonical serialized representation remain
simple; the strict wire seam does not introduce alternate authoring values.


Native FilePath and DirectoryPath controls bind to shared Resource properties,
with external-only references and the SDK existence policy preserved as Existing
or ExistingOrNew. Empty values map to Unbound; execution lowers only exact UTF-8
external paths back to SDK String values through the sole compiled graph. Paths
are not canonicalized, expanded or converted lossily. The resource contract
rejects non-Unicode paths and values exceeding the SDK's 64 KiB UTF-8 bound
before authoring. The processor remains uncacheable; authoring
path selection does not create output objects or resolve them every frame.
Label and RTF authoring still require their own product contracts.
SDK path availability is checked by the App's isolated resource inspection helper
at explicit confirmation, independently of native rendering. No per-frame stat
or alternate processor interpretation is added; the effect still receives the
exact authored UTF-8 value after the guarded author transaction commits.

The SDK existence flag is an availability constraint, not an input/output role.
Allowing an absent path must not imply that the plugin writes that path or that
packaging may discard it. The owned `strings_paths` probe qualifies all four
file/directory existence variants through real native Float32 rendering and
the resource dialog's authoring, Undo/Redo and durable reopen workflow.


## Native compound parameter execution foundation

The matched C/Rust execution ABI carries one fixed four-slot numeric packet per
SDK parameter; the native kind determines its active dimension. Double2D,
Double3D, RGB and RGBA retain SDK double precision. Integer2D/Integer3D use
signed 32-bit components, represented exactly by the ABI doubles and checked
before conversion. Defaults and hard/display bounds preserve each component's
index. No compound parameter is split into scalar author identities.

A shared bounded native tuple state implements the six SDK instance interfaces.
It validates every component before assigning the complete tuple. Snapshot
application similarly validates all parameter targets before assigning values
or invoking grouped change callbacks. Static finite-time queries retain the
snapshot; invalid times and rejected setters preserve caller outputs and state.
Static floating tuples provide zero derivatives and finite constant integrals,
including reversed intervals and representable results whose interval alone
would overflow. Animated historical access still requires author-curve binding.

A parameter snapshot rejected before the native commit boundary leaves the
instance usable and restores its last accepted timing. Violations of the
prepared frame/storage contract remain transport faults that retire the instance. Once parameter assignment or a
plugin action starts, the instance remains failed until the complete render
succeeds. Exceptions and callback/image-lifetime failures retire the instance;
the supervised public transport also retires its worker on request failure.
MSVC explicitly enables exception unwinding so native owners are destroyed
before C ABI exception handlers return a status. Template SDK base names avoid
colliding with the inherited HostSupport `Param::Base` type.

The owned pinned Basic wrapper exercises all six tuple defaults beyond Float32
precision, asymmetric bounds, int32 extremes, static-time reads, atomic rejected
setters and floating derivative/integral output preservation. Raw native frame
qualification additionally checks malformed whole snapshots preserve output and
permit a subsequent valid request; existing failing-plugin tests require failed
instances to remain retired. Public compound descriptions now preserve all six
SDK shapes and complete ordered component bounds. Numeric metadata validates
arity, finite bounds, signed int32 integer ranges, default admission and the
slider intersection with each hard range. Discrete controls carry no numeric
channels. There are no duplicated scalar bounds or compatibility aliases.
All numeric SDK descriptors declare their preferred interpolation, including
compound and color controls. The effect adapter binds exact DoubleVec2/3,
DoubleRgb/Color and IntVec2/3 author values and lowers evaluated tuples to fixed
four-slot native packets without Float32 parameter narrowing. Integer tuples
admit the declared numeric interpolation with integral component steps.

SDK default numeric limits are constructed as typed Double/Int properties,
including their reset values and every declared component. They never pass
through formatted text or numeric locale parsing: Double hard limits retain
the exact IEEE maximum, integer limits retain the signed int32 domain, and
color display defaults remain 0..1. Description qualification asserts the
official Gain example's otherwise unspecified maximum equals the complete
Double limit. This also removes a mutable global string-initialization cache.

Exact Inspector transactions and durable project tuple roundtrips are covered
by the authoring foundation. The owned `compound_author` SDK wrapper additionally
qualifies all six non-animatable tuple shapes through the rebuilt ordinary
product's supervised worker and sole compiled graph: each final component edit
changes actual pixels, Undo/Redo restores the corresponding output, and durable
Unicode-path project reopening preserves precise values, component contracts,
effect identity and output without importing library media. Untouched first
components exercise exact Double and int32 SDK reads; each tuple component
contributes independently to the output. This complements the original SDK
default, atomic rejection and native action/lifetime probes.

Historical curve access, spatial/time/color interpretation, native plugin
editors and the vendor matrix remain separate outstanding qualification work;
these owned static fixtures do not qualify complete format support.


### Canonical OpenFX parameter query snapshots

Animated graph bindings carry the complete canonical AnimatedProperty and an exact
sequence-frame TimeScale. The resident worker derives the current SDK value from
Core PreparedPropertyQuery and reuses unchanged frozen snapshots across renders.
Native value, derivative, integral and key queries call this same Core evaluator;
there is no native curve interpreter or floating-to-author-time quantization.
Sparse compound channels retain their static values. Queries validate complete
results before writing caller outputs, and calculus uses SDK frame units.

The worker owns old and candidate snapshots through native publication and failure
teardown. Successful publication retains the new contexts before any fallible
response write; destruction precedes releasing the final query contexts. Native
parameter source revisions trigger change notifications even when a changed curve
has the same current value. Host assignments publish all current values and query
sources before invoking change callbacks. Canonical curves are currently read-only
from the SDK: mutations are rejected; untimed value writes return Unsupported.
They cannot claim a persistent edit outside the editor authoring session.
Full SDK author edits remain pending.

The owned SessionProbe qualifies independent cubic value/calculus, Hold extension,
ordered key queries and complete sparse Double2/Double3/RGB/RGBA/Integer2/Integer3
values across non-monotonic seeks and instance teardown. Its reference raster
formula is independent of Core. This scope does not imply all OFX contexts,
formats, temporal image dependencies, native interacts or vendor compatibility.

The owned curve lifecycle fixture also qualifies nine renders on one resident
instance at the same frame: repeated unchanged snapshots, replacement by a curve
with an equal current value, Curve/Constant/CurrentFrame transitions, and return
to Curve. SDK callbacks assert exact parameter-change counts and query authority;
reference pixels depend on an off-current endpoint, so stale snapshots cannot
pass merely because the current sample is unchanged.

The native authoring acceptance dispatches the actual visual-effect curve action,
checks one author-generation increment and structured repeated-edit rejection,
and preserves stable key identity through Undo/Redo. It saves the undone state
without losing the live session's Redo, opens a fresh Unicode .mdp with no history,
and compares complete canonical properties and native compiled-graph pixels.
It then repeats save/reopen for the redone state, using the real SQLite library.
This qualifies editor-owned curve edits; SDK-originated author edits remain pending.
The owned native group-history acceptance also defines a joint time/value mode:
one canonical TransformMany request must retain stable IDs, reject a colliding
candidate without either axis changing, and preserve native query/output evidence
through Undo, saved live Redo and fresh reopen. The SDK probe independently
rejects partial axis snapshots and checks derivatives and integrals; qualification
requires an explicit ignored test against rebuilt product and fixture binaries.

The same owned history probe defines declared-default parameter reset through
the editor parameter write contract. It checks constant SDK value/calculus and
empty key enumeration after reset, constrained curve restoration with one Undo,
and shared-graph native pixels through saved live Redo and fresh Unicode reopen.
No second reset interpretation is implemented in the host. Qualification requires
rebuilt native binaries and explicit execution of the ignored probe.

CLAP audio ports are scanned while deactivated. Latency, unlike ports, is queried
through the main-thread handle after activation, and tail through the prepared
processor handle. Probe and worker share this lifecycle ordering. Activated
latency/tail mismatch causes stop, release and deactivation before rejection;
no native instance with stale scheduling facts is admitted.


### VST3 actual-instance audio bus preparation

The audio adapter now admits bounded native auxiliary input slots with the same
Mono/Stereo layout as the main bus, through the existing typed contract and worker
protocol. Actual roles and exact speaker arrangements are queried after restoring
the selected class state, rechecked after activation and at state reentry. The
worker uses the existing upstream per-bus prepared processing API; it neither
flattens sidechain audio into main channels nor adds another plugin host.

The exact previously pinned MIT host is retained locally under
`vendor/vst3-host`, with original author/license/provenance,
a separate upstream-format configuration, a four-file metadata query extension
and removal of existing trailing whitespace in three loader files.
A version upgrade alone cannot expose the required loaded-instance declarations.
The SDK role/activation contract is documented in
[IComponent](https://github.com/steinbergmedia/vst3_pluginterfaces/blob/master/vst/ivstcomponent.h).
Clean b5eb715 product worker and owned stereo Float32 CLAP/VST3 fixtures passed
native sidechain activation/state/PCM/partition/reset and author routing/PDC/
Undo/save/reopen qualification. Three native main-bus parameter/state regressions
also passed; complete Golden passed 3/3 at the same source. See
[the scoped receipt](../dev/modal-resource-lut-acceptance.md#current-vst3-auxiliary-modal-resource-and-lut-acceptance).
Sidechain GUI controls, broader layouts, MIDI/editor/vendor/platform and
performance qualification remain open. Earlier ae3c78f/3b44084 product and
Golden receipts retain their named source scope.


### OpenFX CPU admission and canonical geometry

The product host advertises Float32/RGBA Filter and explicit Output-only Generator
CPU contexts; Generator qualification is limited to the owned Windows controls above.
The supported-depth array is populated before any plugin bootstrap or Describe
call. The 1.5.1 CPU property is registered on descriptors; an instance starts with
its descriptor value and owns subsequent overrides. Unsupported CPU capabilities
are refused during description, instance creation and after change/preference
actions before rendering. The API version remains 1.4 with capability discovery
for implemented additions; this is not a claim of all OpenFX 1.5.1 features.

An optional exported OfxSetHost receives that same native host before plugin
count and header enumeration. Failed is a successful empty discovery and no
further enumeration/action is called; other non-success statuses are errors.
Discovery owns an opaque native host until library unload. Selected description
and sessions retain an extra module reference across bootstrap and HostSupport
enumeration; the instance and selected binary die before that reference and host.
No generic vendor cache or process-global bootstrap owner is introduced.

Progressive rendering passes FieldNone. Project size/extent and clip RoD/ROI use
canonical X = pixel width times PAR, while image bounds and render windows stay
in pixel coordinates. Rust request admission and the native C seam both refuse
nonrepresentable canonical extents without imposing an arbitrary PAR maximum.
These constant-time checks add no steady-state bootstrap calls or raster copies.
Filter/Generator duration remains host-owned. The pinned SDK permits GetTimeDomain
only in General; Timeline Bounds queries do not implement that action. General,
Transition/Paint/Retimer, GPU, temporal access, interact/author callbacks, colour
negotiation and whole-host throughput remain separate unfinished work.

### Selected OpenFX binary API admission

The product-owned `SelectedPluginBinary` uses the existing platform binary loader,
HostSupport plugin factories and handle ownership. It enumerates at most 1024
headers after the optional early host bootstrap and admits only registered API
versions. Other APIs and future image-effect API versions receive no `setHost` or
actions. Admitted plugins retain their original export index, so later handle
creation cannot address another plugin after skipped entries. All-unsupported
binaries produce an empty discovery; malformed supported headers fail within the
supervised worker. The generic vendored filesystem/XML scanner is unchanged.

### OpenFX early host ownership and exact API matching

Discovery owns a Host followed by an ImageEffect cache before loading/calling
the optional early bootstrap. This provides a live host for exposed host-only
ImageEffect image-memory calls, including a null instance handle. The same
product-owned cache lifetime binding is used by description and resident owners.
Its product-owned lease uses immediate acquisition and returns native HostBusy
for a concurrent owner or same-thread reentry. No caller waits for another session
to be dropped. Render callbacks take no additional lease lock.
The cache is constructed after the lease and destroyed before it. The lease clears
the vendor host callback pointer
before its Host dies; the supervised process admits one native host at a time.
Plugins and their DLL teardown still occur before the binding is released.
This adds no multihost or native multithread capability claim.
Native session admission validates complete frame geometry, alignment, finite
samples, exact storage and parameter-array/count transport before host ownership,
so invalid transport returns its own error even while another owner is active.
Private Rust owners are neither Send nor Sync and are destroyed on their owning
thread. Semantic parameter validation remains tied to the selected descriptor.

Rust/native enumeration compare only the admitted ImageEffect API v1 literal
including NUL before examining identifier/callback metadata. Unknown API names
receive no arbitrary product 64-byte/ASCII restriction, callback or owned string
allocation. Current known-API identifier/resource limits remain unchanged.
Original export indices are retained. Unknown-version entries remain skipped.


### Actual progressive image identity

Each fetched Source/Output image reports `OfxImagePropField=OfxFieldNone` from
the progressive CPU image authority. Its ASCII UniqueIdentifier belongs to the
selected worker's session owner and current frame binding. Repeated fetches in
one binding retain the label; Source and Output differ. Rebinding, changed input
at equal time, seeks and new native owners change the identity, allowing plugin
analysis caches to invalidate correctly. The namespace is parent-issued per actual processor lifetime, including fresh
children/reopen, and is not a content-addressed persistent key. The property owns its string; raster
addresses retain the existing action-scoped borrowing rules. No raster hash or
frame copy is needed, and exhausted counters fail rather than reuse an ID.


### CPU thread and mutex suite ownership

The exclusive resident native worker grants one active CPU to its selected host.
NumCPUs reports that allowance, independent of hardware CPU count. A lazy owned
callback thread executes each batch while the native caller blocks. Its TLS
reports index zero and spawned=true only on that real callback thread; plugin
self-created threads and the native caller report spawned=false. Recursive
multiThread returns ErrExists before dispatch; independent overlapping batches
fail to launch under the one-job owner capacity. Every successful call returns
after its callback completed. That accepted caller retains the job reservation
until it consumes its own result; another caller cannot replace the completion
between worker publication and caller acknowledgement. The thread is reused across frames and joined after
plugin cache/unload teardown while the selected Host remains alive. There is no
global pool and no new host-concurrent Render or tile capability.

The host owns 1024 reusable mutex state slots. Opaque handles carry a monotonic
generation and slot, are never dereferenced, and cannot alias a later binding.
Each successful same-thread lock/trylock increases recursive depth; competing
trylock returns Failed without changing depth. Positive initial count belongs to
the creator. Negative initial counts are accepted and maintained as signed
accounting, with actual acquisition retaining mutual exclusion; they do not
create semaphore credits. Destruction invalidates its generation and wakes
waiters, which return BadHandle instead of acquiring a replacement. Slot capacity
and generation exhaustion return ErrMemory. Invalid handles return BadHandle;
unlock without ownership is rejected as ErrValue. As required by plugin lifetime,
plugin-created threads must complete before Unload/host teardown.

The native reservation getter is the sole sizing authority for the fixed suite
state, configured 8 MiB callback stack and page guard. Windows uses CRT-aware
_beginthreadex with reservation semantics; POSIX configures stack and guard in
pthread attributes. Effects factory resident admission charges that maximum
before native loading, even if the callback thread remains lazy. This is logical
owned suite storage, not a whole-native-heap or committed process RSS limit.
Existing SDK descriptor/cache/property allocations, OS/CRT metadata, executable
mappings and plugin private allocations remain separate. The getter includes
the embedded ThreadSuite owner object once; there is no separate existing Host
sizeof charge in the raster sizing formula. Unsupported stack geometry refuses sizing.
The existing parent deadline/cancellation authority still terminates and reaps a
stuck native process.

This closes the exposed suite under a one-CPU grant. It does not claim multi-CPU
throughput qualification or arbitrary plugin performance. A larger allowance
requires an explicit domain-owned CPU allocation contract and machine/build
latency receipts.
[Released suite contract](https://github.com/AcademySoftwareFoundation/openfx/blob/OFX_Release_1.5.1/include/ofxMultiThread.h),
[thread safety](https://openfx.readthedocs.io/en/main/Reference/ofxThreadSafety.html).


### Bounded native Float32 bit transport

The parent copies the already admitted immutable typed input into its shared
range in 1 MiB POD blocks. Both full binary identity checks remain at their
existing pre/post invocation seams; the native C boundary still admits input
geometry/finite values and validates completed output. The control response
still proves the exact invocation sequence and actual output association.

Before sending a native render request, the parent acquires one owned RGBA
allocation under the invocation checkpoint/deadline and its admitted resource
grant. After observed completion it copies raw native-endian blocks into that
same spare capacity.
No frame or f32 reference is published until the entire checked range is
initialized. Cancellation drops the allocation without reading uninitialized
slots. The shared output borrow ends before a subsequent worker request, and
the returned frame owns independent storage. Opaque output has its ignored
physical A lane canonicalized to 1 while all RGB bits remain unchanged; other
representations preserve every raw channel bit, including negative zero,
subnormal values, HDR, extended alpha and zero-alpha associated emission.

The sole Effects controlled storage admission validates complete shape, sample
precision, finite values and exact alpha domain with checkpoints at each
65,536-pixel block and before publication. It allocates no second raster and
does no association conversion. There is no per-frame profile allocation or
logging. Original cancellation/deadline, changed-binary and native isolation
semantics remain in place.

Root's pre-Float r30 release baseline failed all four unchanged public-call
performance gates (1080p/UHD at 30/60 fps), with 600 measured samples per group
and no correctness errors. This source stage targets scalar representation
transport overhead; it has not established any throughput result. It requires
fresh source/product/test/fixture receipts and the same gate budgets, plus
separate complete Float graph and cold-residency qualification.


### Native output resource admission and residency

The parent acquires the complete single output owner before incrementing the
request serial, copying input, sending a request, or advancing authored native
parameters/render state. A failed allocation returns the typed Effects
`OutputAllocation` host-resource refusal. It does not retire the valid native
worker, quarantine the plugin definition, publish staged pixels, or replay the
invocation. Filter retains its three-raster active charge; Generator uses the
two-raster active charge described above. Moving the output owner earlier adds
no raster. Actual Vec capacity must
fit the admitted output bytes. An allocator-provided larger capacity is a typed
OutputCapacity refusal before DSP, not silently uncharged retained storage.
Allocator usable-size and OS bookkeeping remain separate excluded frontiers.
Actual completion fills
the same exclusively owned allocation and uses the sole controlled frame
validator. Both binary SHA checks and the native boundary validation remain.

Effects alone records the exact prepared occurrence/provider revision and
decides Retained or Evicted using its existing optional count/byte grant. If
optional retention is denied, the instance is actually dropped before the
receipt is issued; a later invocation must prepare a new instance. A scheduler
stop wins before retention and staged publication. No application-side spare
instance is retained. These facts describe resource admission, not a native
processor fault and not successful execution. Persistent plugin user-blocked outcomes use this same typed invocation/retention authority, with actual native qualification recorded separately.

This source amendment is not runtime or performance qualification. Original
r30 failures remain failed; the complete Float/bulk chain requires newly pinned
artifacts and all four original public render budgets plus cold/residency gates.

### OpenFX image identity across process replacement

The image authority uses the parent-issued `EffectProcessorOwnerId` as its
namespace. `Prepare` transports the same typed lifetime once; the native create
boundary rejects null/nil identity before acquiring the SDK host lease and
retains a deep copy of exactly 16 bytes. Per-binding generations and distinct
Source/Output suffixes remain stable for repeated fetches of the same binding.
Reprepare, eviction, and project reopen create a new namespace, even when the
OS recycles PID or the raster/time returns to a previous coordinate. There is
no per-pixel hash or per-frame UUID generation.

A conforming plugin may persist UID-indexed analysis past one child lifetime.
The owned qualification fixture saves an actual UID/RGB analysis record, then
restores it in fresh processes with changed source pixels; an independent Basic
gain oracle detects a stale cache hit. The public-worker contract additionally
checks the actual native UID prefix against the exact parent lifetime. This is
private analysis-cache qualification, not proof of host private-state
serialization, every plugin's persistence format, or whole project save/reopen.

Known fixed image-identity storage charges three 16-byte values: the parent
session owner, child Prepare owner, and retained native namespace. Temporary
call stack, allocator bookkeeping and vendor/plugin heap remain explicit
frontiers. Fresh build/native tests and the unchanged release performance gates
are required before recording runtime/performance acceptance.

### Bounded native auxiliary feedback (Stage A)

The product-owned OpenFX Message V1/V2, Progress V1/V2 and Timeline V1
dispatchers resolve actual registered Host/Descriptor/Instance handles without
casting an untrusted callback token. Intrusive guards are constructed before
the SDK base and destroyed after its DestroyInstance action. Copied auxiliary
timeline/text state outlives that action; callback threads join before the
Host's auxiliary owner is destroyed. Wrong object kinds and dead/unregistered
tokens refuse without dereferencing their storage. A bounded complete UTF-8
journal replaces stdout logging; oversize, invalid or full-slot posts fail
atomically. The policy is 32 notices, 4096 text bytes and 256 identifier bytes,
not a maximum claimed by the OpenFX standard.

Persistent Error prevents rendering/publication until a real authored
InstanceChanged or explicit clear repairs it. Warning and information permit
output. A terminal report has the exact native invocation serial; the
supervisor attaches the same parent-issued Core EffectProcessorOwnerId used by
image namespaces and its actual nonzero child PID. The compiler/bank attach
EffectId, sizing/alpha format and provider revision. Valid UserBlocked and
pre-DSP HostResourceRefused remain distinct from cancellation, deadline,
malformed protocol and native faults. Effects alone decides optional Retained
or Evicted ownership for each recoverable disposition. A parameter change keeps
the author occurrence/provider revision, so a genuinely retained child can
repair its existing private instance; dimension, format, provider replacement
and real eviction require a new parent identity. App stores bounded facts only.

Both scalar whole-graph/Timeline and heterogeneous whole-batch routes refuse
more than 32 declared feedback-bearing occurrences before the first stateful
callback. Zero-feedback providers retain their original small-grant path. For
feedback providers, the control reserves its complete receipt capacity before
prepare/DSP and checks actual capacity against the admitted bytes. Completed
receipt batches move through fixed inline storage without a full raster clone
or a replay of advanced state. The CPU Viewer fallback preserves the canonical
typed error and actual successful feedback instead of converting all outcomes
to text. Current Viewer projection requires its existing exact work key,
author generation, playback epoch and consumed/active execution authority.
Stale facts cannot clear a new owner or publish notifications. UserBlocked
projects to a blocked Viewer delivery, without fabricated frame completion or
plugin quarantine; repair and removal retire only the observational view.

Noninteractive Question returns Failed, never a fabricated Yes/No/Default.
Timeline getTime/getTimeBounds return the actual request snapshot; gotoTime
returns Failed for a finite unsupported interactive navigation and ErrValue
for nonfinite input. Progress owns its real begin/update/end lifecycle.
The native journal also supplies current-work live progress and cooperative
abort through the same supervised authority described below. Actual generic GUI
Question/reply uses the bounded current-work consumer described in the C1 section;
ordinary realtime/noninteractive work still returns Failed without a modal wait.
Interactive Timeline navigation and Interact remain required closures on this
same work/owner/channel, with no additional instance, graph or request authority.

The wire is one current generation (v8), with strict complete schema admission
and no older ownerless fallback. The fixed auxiliary state/report, guard cap,
maximum separately owned snapshots and bounded parent/child packet buffers are
charged before loading. This is a conservative known-host logical ledger, not
whole process RSS: allocator bookkeeping, OS/CRT thread state, existing SDK
objects and arbitrary plugin allocation remain separate. Existing host UID
std::string capacities and SDK image-property UID copies are also explicit
unmeasured frontiers, not mislabeled plugin heap. The original UHD optional
384 MiB cap remains unchanged; active admission may execute while actual
optional retention evicts, and cold prepare/performance must be measured.

This source contract does not establish runtime or performance acceptance.
The owned suite/status, actual Effects repair/eviction, current-work consumer,
source stability and fresh release timing gates must be run on pinned artifacts.

### Native Log diagnostic delivery

The supervisor emits native Log notices once to the existing product tracing
target `mondrian.openfx.plugin`, after matching work sequence, native revision,
parent owner and both binary checks. Fields carry the actual parent
EffectProcessorOwnerId, actual nonzero child PID, native invocation and complete
plugin-authored UTF-8 text/ID. This is an execution diagnostic, not successful
frame/presentation evidence or an authoring request. Both Viewer and export use
this same supervisor; the App notification projection does not emit a second
log or a user popup for Log notices. Information and warning delivery remains
the current-work consumer contract.

The existing product subscriber uses rotated bounded JSONL and stderr,
with its existing filters and non-blocking writer/backpressure policy. It does
not write to the native child control stream or stdout. It remains best effort,
not durable audit publication, and cannot promise persistence after abrupt
process failure or an unavailable user-state log directory. These properties
do not establish fatal/discovery/unload notice or live interaction support.


### Active versus retained native processor storage

Native factory sizing uses the shared Effects `EffectFloatProcessorStorage`
contract. Active admission covers two mapping ranges, its one pre-admitted
output Vec, reserved callback stack/guard and fixed thread/mutex/namespace POD.
After a successful response, the complete typed output Vec moves into the
Effects caller's frame; the native session retains only the pair of shared
ranges and fixed owner reservation. Effects separately charges its input,
staging, cache, mask workspace and resident siblings. The unchanged Professional
preview policy is 768 MiB working and 384 MiB optional native retention.

This is a conservative controllable logical-owner ledger, not complete RSS.
FrameState Source/Output UID strings and SDK-owned property string copies remain
known unmeasured host allocations; they are not arbitrary plugin heap. SDK
descriptor/cache allocations, executable mappings, OS/CRT bookkeeping, arbitrary
plugin memory and allocator usable-size overhead remain separate frontiers.
Real authored-factory owner/PID reuse, cold cost and fresh unchanged release
frame budgets require independent qualification; source sizing does not confer
performance PASS.

CPU Preview treats invalid complete Float frame storage as a typed frame-contract refusal. It does not start another backend to conceal an invalid extent or sample. Scheduler stops remain cancellation/deadline outcomes through scalar admission.


### Cooperative current-work control and live progress

The supervised native session uses wire generation 8 with one existing TCP
connection and one request authority. Its domain-owned control pump is the sole
socket reader/writer in the child; native actions remain on the dispatch thread.
Render and Cancel name the exact parent-issued processor owner and checked work
serial. Cancellation accepted while queued remains sticky during activation.
One original completion retains its slot until every byte commits, and a checked
local acknowledgement token lets dispatch recognize that original completion
even when the next valid request is already queued. Serial/token exhaustion
refuses before work admission. Invalid old/future/foreign cancellation does not
mutate the current work; a malformed peer protocol is still a supervisor fault.

The native scoped control borrow exposes genuine SDK abort observations and
fixed POD progress to this same owner. It never invokes the parent's borrowed
Rust control closure on a foreign thread. Native callbacks and declared CPU
computations finish before that borrow clears; native destruction precedes pump
shutdown. The only cloned socket is a shutdown capability, not a second reader.
Stop shuts the socket, wakes the bounded slot and joins the pump; no detached
reader can retain an invocation. A pump panic retires the supervised process.

One progress POD coalesces in the child. Packet reader/writer owners are admitted
before native DSP, bounded to two MiB per direction and reused; partial cursors
survive polls. Input is serviced before an outbound write quantum, so a full
progress socket cannot mask Cancel. Accepted live packets prove parent owner,
request serial, native owner, monotonically increasing revision and progress
generation before a product observer sees them. Terminal snapshot/lifetime,
binary fingerprints, complete Float-frame and output-owner checks remain
unchanged. Progress is not frame readiness or Retained/Evicted evidence.

The parent owning thread samples its canonical work checkpoint throughout I/O.
An explicit cancellation submits one exact Cancel and drains within the smaller
of the original scheduler/native deadline and a 100 ms retirement grace. A
deadline already expired does not acquire extra grace. An uncooperative plugin
is killed/reaped under that authority; advanced DSP is never replayed. A late
Rendered/UserBlocked reply cannot override the owning stop or publish pixels.

The pump adds a declared 8 MiB requested stack and conservative 64 KiB guard
allowance, fixed owner/cursor/slot sizes, and admitted packet capacities to the
canonical native active/retained policy. These are logical resource policy, not
exact OS committed/reserved bytes or whole-process RSS. OS/CRT/TLS, Arc headers,
SDK property/string storage, serde-owned metadata/transients and allocator
bookkeeping remain explicit known resource frontiers; unsupported reservations
or limits refuse. No throughput, complete memory or all-vendor compatibility
acceptance follows from the existence of these interfaces.

Owned SessionProbe controls distinguish actual Render-before-cancel/SDK-abort
from immediate worker retirement, and a real live .375 observation from a
terminal-only snapshot. Public Effects-factory tests bind the same occurrence,
provider revision, child PID, parent/native owner and terminal receipt. They
retain independent Basic unit-gain pixels and one-action/no-replay checks.
Question modal/reply is carried by the actual generic GUI consumer below.
Interactive Timeline navigation and OFX Interact remain required closures;
noninteractive Question/goto failure does not claim those services are complete.

### Bounded native ingress and completed provider admission

The reusable native request validates raster storage through the sole borrowed
Effects controlled validator with the same work-owner checkpoint before its
first scan, at each 65,536-pixel block and before completion. This precedes output
reservation, request serial advancement, shared input copies and native DSP.
The private request contract requires an explicit checkpoint at every caller;
the convenience call without a scheduler supplies its explicit no-op owner.

Bank-backed and direct CPU provider completion share one metadata admission and
the same controlled storage validator. Exact extent, format, alpha negotiation
and numeric violations retain their actual diagnostic; they are not described
collectively as a changed pixel count. An owner stop during either bank admission
or the outer staged publication remains sticky and reaches the existing stopped
outcome: the sole bank owner retires, staged pixels do not publish, DSP does not
replay and a valid provider is not quarantined as a fault. This amendment keeps
the existing two completed-provider scans and all finite/shape/alpha checks;
it does not claim a measured speedup or remove binary fingerprints/output clear.
Native C++ scans, allocator/metadata/OS frontiers and full release timing require
their separate qualification; this source proposal is not full latest-OFX support.

### Whole-call OpenFX performance diagnosis

The `openfx_frame_diagnostic` release example carries the same complete public
`OpenFxFilterSession::render` gate as the integration harness, including native
execution, ownership checks, file fingerprints, copies and typed output admission.
It preserves the independent full-raster oracle outside the timer and the original
32 warmups, 600 samples and 80% stage budget. Selecting this example avoids building
unrelated integration companion binaries; it does not narrow the timed boundary.
The explicit diagnostic helper path must match the actual worker role/path/hash in
the unified machine/build receipt. Run with the same release/all-feature profile.

Diagnostic traces and phase timers are attribution evidence, with overhead and
source/artifact identity recorded separately. They are not performance acceptance.
A candidate still requires a fresh uninstrumented whole-call run under the unchanged
budget; a native-only interval cannot substitute for that result.

Validation-only `render_diagnostic` delegates to the same generic owned render
path as ordinary `render`/`render_observed`. Its fixed caller-owned POD report
records the actual parent owner, child PID and original request serial; it grants
no work authority or native action policy. The current strict wire carries the
explicit child-probe selection and bounded report; its fixed owning storage is
charged by the canonical native active/retained ledger. Normal
execution uses a zero-sized statically dispatched no-op probe with no diagnostic
clock, queue, log or allocation. Instrumentation keeps all mutable-frame input,
complete output, fingerprint, copy, output-clear and owner checkpoint contracts.

The phase attribution example retains the original full-raster independent gain
oracle and whole public timer. Every sample and original 80% budget miss is
reported after timing, but this diagnostic test deliberately produces no PASS
verdict. Parent idle sleep counts and requested/observed durations are recorded
inside the inclusive completion wait; they must not be added again as separate
phases or interpreted as exact child DSP time. The report/retained sample Vec are
explicit validation-caller storage, not native resident or whole RSS proof. Fresh
same-source release/all-feature artifacts and measured instrumentation overhead
are required. Performance acceptance still uses the uninstrumented original gate.

`owned_invocation_elapsed` starts after the fixed diagnostic report reset and
ends after the canonical owned render attempt, including failure retirement.
Its phase sum and unassigned gap are internal attribution only. The outer
`render_ns` remains the complete public-call interval, including report setup
and final reporting overhead; it is the sole original budget authority.


### OpenFX actual GUI, Question and idle action feedback

A generic Inspector opens native interactivity only while its actual effect
PropertySection Widget remains visible in the current private author view. A
selection/model DTO is insufficient. The native worker pairs BeginInstanceEdit
and EndInstanceEdit on the same instance, including a legal Failed Begin reply;
that private setup result is distinct from the real generic GUI lifetime.

An explicit nonrealtime editor-refresh work may ask a bounded Question through
the existing sole TCP pump and task result channel. Yes/No come only from local
buttons after the original modal has a real successful Window presentation.
Unavailable, stale, full, disconnected or dismissed service returns native Failed;
it does not invent No, a native fault or loss of otherwise legal Render output.
Human waiting cannot extend an absolute scheduler deadline. Ordinary playback,
selection metadata and native booleans do not grant that waiting purpose.

TCP action serial and SDK Render invocation count are independent nonwrapping
coordinates. Render→idle End→Render uses wire 1/2/3 and native Render 1/2.
Diagnostic `sequence` remains the original Render TCP action serial; it must not
be compared to the native invocation count after an idle action. Late original
cancel/reply/GUI callbacks cannot consume replacement work.

End drains its bounded notices immediately. The canonical residency bank binds
actual preparation/provider/processor owner and Retained disposition before
publishing an explicit GUI-action receipt. This carries no new FrameWork,
RenderedScope, presentation ticket or author edit. App rechecks the current
private author view and previously observed exact retained native binding before
updating the same processor-feedback/notification state. Full delivery is an
observational failure, not another lifetime authority or a reason to replay DSP.

Before idle SDK action/decode the same bank admits declared temporary metadata
against its actual working grant. Resource denial may evict through the canonical
bank, without provider quarantine; it never increases the optional 384 MiB cap.
Actual bounded result/drain storage and cloned author metadata are declared.
SDK properties/serde temporary allocation, Arc/allocator administration and
OS/CRT remain explicit resource frontiers, not a whole-RSS guarantee.

Native parameter writes, native-origin author gestures, SyncPrivateData,
Timeline goto/Interact/Overlay and other formats/contexts remain separate missing
vertical consumers. Current code proposals do not establish arbitrary-plugin,
physical-modal, cross-platform or realtime performance qualification. Root's
r171/r245 pre-C1 whole-call measurements failed the unchanged frame budgets;
new C1 source requires fresh pinned build and qualification before such claims.


### Default-off child and native attribution

A measured render requests a fixed report on the same TCP owner/work serial.
Normal requests explicitly disable it: the Rust dispatcher and sole C++ render
law instantiate no-op probes, with no diagnostic clocks, raster owner, per-frame
log or new queue. The first Output-fetch clear remains one shared operation;
its optional clock is outside the pixel loop and protected by the existing fetch
mutex. All native computations join before the borrowed CABI diagnostic pointer
is cleared by RAII, including exceptional exits. Existing finite/shape/alpha,
mutable-frame input, fingerprints, output clear and cancellation checks remain.

The child records disjoint admission, binary verification, parameter arena,
actual cold create, native dispatch, and terminal preparation. Native phases
record session admission/image binding, suite setup, parameter/clip actions,
sequence begin/ROI/Render/end, raw output admission and auxiliary report copy.
Output clear is nested inside plugin Render. Render includes host callbacks and
legal spawned computations, so it is not pure plugin algorithm/DSP cost.

Child total ends before Shared completion, pump serialization/socket commit and
original acknowledgement. Those costs are unmeasured by the response carrying
this report. Child dispatch can start while parent request submission finishes;
never subtract across process start coordinates as an exact transport residual.
Native spans nest inside child NativeDispatch, and those spans overlap parent
Await. Each level has its own nonoverlapping sum/unassigned duration; adding
levels or adding nested clear/sleep again would double count time.

The strict generation is 8, with no older helper fallback. The parent verifies
exact parent owner, original wire/action serial, actual native Render invocation,
input bytes, accepted output alpha and bounded timing metadata. Diagnostic
counter overflow is a reported measurement failure, not a plugin DSP fault.
Fixed inline report copies and the borrowed native pointer have a declared
conservative logical reservation; slot/Response storage is counted by its actual
owner size. Existing packet capacity remains bounded. OS/CRT/SDK/serde allocator
transients remain known frontiers, so this is not whole RSS/resource acceptance.

Fresh same-source release product and diagnostic-example artifacts are required.
The paired HD30/UHD30 32-warmup/200-sample run is attribution only. The original
600/all-four uninstrumented whole-call 80% budget and complete analytic pixel
oracle remain the performance acceptance. Previous parent-only performance
failure remains valid evidence; this source change is not a repair or PASS.

The CLAP note-port facade interprets `preferred_dialect` using its official
unsigned 32-bit field carrier. The separate C enum typedef is not the carrier
for this metadata field. Enabling the SDK note-port feature for owned native
fixtures does not expose a product host note-port service or accept note/MIDI
outputs without a canonical consumer.
