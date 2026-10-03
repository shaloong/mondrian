# Plugin System

The most concrete plugin surface today is the effect plugin contract. UI and command plugin surfaces are planned and should reuse existing command/widget boundaries.

The current effect contract is visual-only and is not the future VST3/CLAP host
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

OpenFX has a supervised binary-header discovery boundary in
`mondrian-app::openfx_adapter`. The app's hidden discovery child alone loads a
selected native binary, calls `OfxGetNumberOfPlugins` and `OfxGetPlugin`, and
returns bounded image-effect API v1 identifiers/versions. The parent pins the
canonical binary path and full SHA-256 before and after the child, validates
unique descriptors, and enforces a deadline; native failure never loads the
binary in the editor process. A selected `.ofx.bundle` resolves through the
current platform's `Contents/<architecture>` directory only when exactly one
regular `.ofx` binary is present. Discovery does not invoke `setHost`, image-effect
description, instance creation, or render. An independently built official
Float32 Basic example provides the local ABI discovery qualification fixture;
its binary stays outside the shipped product.

The application builds a selected-binary Float32 filter host on Windows,
Linux, and macOS using vendored BSD-3-Clause OpenFX HostSupport source and separate supervised
description/render children. The description child runs DescribeInContext for
one selected Filter and returns bounded Double/Boolean/Integer defaults, labels, hints,
numeric hard/display ranges, and animation/visibility flags. Plain and Scale
Double semantics are admitted; other parameter kinds and spatial Double
semantics receive an explicit unsupported result. The caller supplies a pinned
binary inspection, explicit frame time/rate/range/PAR, RGBA Float32
straight-alpha working-frame pixels, and
Double/Boolean/Integer parameter values. The parent checks binary identity, bounded
request/output sizes, finite input and output pixels, and a render deadline;
a native crash remains in the child. Frame transfer uses one exact-sized,
file-backed shared memory mapping with disjoint input/output ranges in native
Float32 byte order. The parent retains the backing file until child termination
and reaping, and reads output only after successful completion. The child
validates the complete mapping extent before native loading; the synchronous
native ABI receives bounded memory buffers instead of pixel file paths. This
removes explicit full-frame file writes/reads and the intermediate encoded input
vector, but remains a per-render process with OS-backed mapping allocation.
Resident instances and a reused transport still require lifecycle work. The host advertises only the Filter context and RGBA,
does not advertise tiles or temporal clip access, rejects unsupported
parameter types and authored Double values outside the plugin's hard range,
rejects parameter access at a different frame instead of returning the current
value as animated history, and pairs each successful BeginRender with EndRender even on
render failure. A real official Basic Gain filter qualification covers all
pixels in a nonuniform 4x4 frame at an authored gain of 1.5. Its reference
binary remains outside the product tree. The host ABI uses OpenFX bottom-left
pixel coordinates with negative row bytes over Mondrian's top-row-first frame.
`tests/openfx_reference` builds that pinned upstream Basic source without
Conan or vendoring the plugin. It checks the upstream commit at configure time
and emits a platform bundle outside the product tree for the ignored real-host
pixel test. Windows MinGW-built Basic passed the selected-binary host and
compiled-graph comparison. The reference test also exercises per-component
controls at a later frame with non-square pixels and unclamped Float32 HDR
and alpha values; macOS and Linux still require their own built
fixture and native qualification run.
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
qualification, and a resident worker to avoid one process launch per frame.

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

## Effect Plugins

Plugin effects are represented as `EffectType::Plugin(String)` and registered through `EffectDefinition`.

Plugin definitions may provide:

- display name and category path
- default properties
- graph builder
- branching graph builder
- custom render backend
- cache key/policy
- plugin contract/runtime availability metadata

The `EffectGraphDsl` exposes source/current nodes, unary ops, branches, blends, and masks.

## Runtime Safety

Graph builders and custom processors execute against staged state. Builder
errors/panics return `EffectGraphBuildError`; processor errors/panics return
`EffectExecutionError`, and neither commits partial graph or pixel state.
Failures are recorded through the plugin runtime-status path. The runtime
failure policy decides only whether the definition remains callable; the
library policy decides only new-insertion visibility. Neither policy permits a
failed instance to become identity output. Plugin code must still avoid panic
across the host boundary.

## Future UI Plugins

Future UI plugin entries should integrate through:

- command registry descriptors
- panel/workspace registry
- typed actions
- theme tokens
- platform services

Plugins must not directly own app state, OS handles, or renderer internals.

## Native-format adoption order

Current status: the Timeline recognizes CLAP/VST3 definition identities.
`mondrian-audio` has a supervised processor-worker contract and a CLAP child
factory; the application dispatches its packaged executable into this worker
before initializing graphics or media. A parent-side CLAP registry can refer
to installed definitions without loading native code. The child loads a CLAP
library, checks descriptor identity, exactly one main Float32 input/output
port with the requested mono/stereo port type and channel count, latency, and
tail, restores supplied
opaque state through CLAP's state extension before activation, and processes
interleaved blocks through planar CLAP buffers. It accepts numeric CLAP
parameter lanes with stable IDs and exact author-time automation; the worker
rechecks probed metadata and emits sorted sample-accurate CLAP value events.
It rejects auxiliary buses and incompatible layouts.
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
The initial supported subset is audio effects with explicitly negotiated
Float32 buffers and channel layouts; instruments, MIDI, plugin UI, and
unimplemented extensions remain unavailable with typed reasons.

VST3 follows through the same supervised Worker transport with its own child
entrypoint and the pinned MIT-licensed `vst3-host` crate and MIT/Apache licensed
`vst3-rs` bindings. Its Adapter must honor component/controller separation, bus activation,
parameter timing, latency, tail, state, and offline/realtime process modes.
Earlier SDK releases have different terms and are not an implicit substitute.
The host must not ship third-party plugin binaries. Keep required SDK copyright
and license notices, and review the trademark rules before using VST branding.

The admitted OpenFX Adapter runs a user-installed CPU Filter behind a separate
supervised worker, with frame timing, pixel aspect, Float32 RGBA, and authored
scalar parameters lowered into the canonical Effect graph. The official Basic
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
supervised child. A session owns frame facts and the input buffer, host, local
PluginCache, ImageEffect API handler, selected binary and effect instance in
that lifetime order. Destruction reverses the order. Neither selected rendering
nor description registers stack-owned handlers in the process-global cache.
Clips and scalar parameters read their owning instance's frame facts; no global
mutable raster or timing variables remain.

Reuse binds the exact raster extent. Every request applies a complete scalar
snapshot; omitted controls revert to descriptor defaults. Validation precedes
parameter mutation. Changed values are assigned before callbacks and bracketed
with BeginInstanceChanged/InstanceChanged/EndInstanceChanged. Time changes use
ChangeTime; edits at the same instant use ChangeUserEdited. Clip metadata changes
also notify Source/Output and refresh preferences. Unsupported negotiated pixel
contracts fail. The current independently evaluable route rejects required
sequential rendering and temporal clip access at description, creation and
render admission; optional sequential optimization remains legal. Begin/render/
end actions explicitly report nonsequential execution during edits and seeks.
The output allocation is reused and cleared, with a per-render
fetch flag preventing publication of a previous frame. Native failures poison
the handle; subsequent calls fail until the owner retires and recreates it.

The executable now exposes the resident native session protocol described below.
The standalone single-frame API uses that protocol and retires its session after
one frame. Definition callbacks still call that standalone API until factory
binding, execution cancellation and coordinated native grants are integrated.
Combined memory admission and vendor sequential-state support remain required.
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
a control stream and an immutable two-frame shared mapping. The child proves a
fresh random nonce and protocol version before receiving the selected binary or
mapping path, validates the preparation envelope and exact file size, verifies
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
The authored Filter definition now binds an immutable processor factory. Its
instances belong exclusively to each execution Session and author EffectId;
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

Source and Output handles borrow the current mapped frame only during legal
OFX actions. Each fetch creates an independent property handle, sharing raster
storage without owning it. The child maps Source read-only and Output writable.
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
identifier, and starts no worker during graph evaluation. Each admitted native
instance owns one OpenFxFilterSession. Parameter snapshots and exact frame
context still come from the canonical author graph; edits and seeks do not use
an alternate interpretation. The executor moves its atomic input stage into
the host envelope, so this binding adds no input raster. Failures restore that
stage before propagating the typed error; the caller's published frame remains
unchanged. Session eviction, errors and observed owner stops retire the child.
Changing only the frame generation preserves independent-frame instances;
author occurrence, registration revision and raster extent remain exact keys.

The current controlled host-frame charge is three complete Float32 rasters:
two shared mapping ranges and the returned output vector at peak. Shared physical pages are counted once. This
charge is checked before worker creation, together with executor staging and
live graph values. Heterogeneous preparation includes active recipe charges
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
plus three host rasters require 663,552,000 logical bytes; a one-byte-short
working grant rejects, and a 768 MiB grant succeeds with all pixels and distinct
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
