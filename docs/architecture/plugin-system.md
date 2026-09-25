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

The Windows application now includes a selected-binary Float32 filter host
using vendored BSD-3-Clause OpenFX HostSupport source and separate supervised
description/render children. The description child runs DescribeInContext for
one selected Filter and returns bounded Double/Boolean defaults, labels, hints,
numeric hard/display ranges, and animation/visibility flags. Plain and Scale
Double semantics are admitted; other parameter kinds and spatial Double
semantics receive an explicit unsupported result. The caller supplies a pinned
binary inspection, explicit frame time/rate/range/PAR, RGBA Float32
straight-alpha working-frame pixels, and
Double/Boolean parameter values. The parent checks binary identity, bounded
request/output sizes, finite input and output pixels, and a render deadline;
a native crash remains in the child. The host advertises only the Filter context and RGBA,
does not advertise tiles or temporal clip access, rejects unsupported
parameter types and authored Double values outside the plugin's hard range,
rejects parameter access at a different frame instead of returning the current
value as animated history, and pairs each successful BeginRender with EndRender even on
render failure. A real official Basic Gain filter qualification covers all
pixels in a nonuniform 4x4 frame at an authored gain of 1.5. Its reference
binary remains outside the product tree. The host ABI uses OpenFX bottom-left
pixel coordinates with negative row bytes over Mondrian's top-row-first frame.
`mondrian-app::openfx_effect` can now register one explicitly selected and
described Filter as a Float32 CPU definition in the shared compiled visual
effect graph. Stable parameter IDs derive from the plugin identifier and OFX
parameter name; the definition captures defaults, hard/display numeric bounds,
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
native folder picker. A closed product action inspects the bundle, describes
its image-effect entries, and registers only admitted Filter definitions; the
Effect Browser then exposes those definitions for ordinary Clip insertion.
Successful machine-local selections are saved in app UI preferences and
restored on a background catalog job at startup. A changed binary may offer
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
insertion flow. The selected OpenFX filter host remains disconnected from the
visual Effect registry/DSL and product insertion flow.

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

The first OpenFX Adapter should run a user-installed CPU image effect behind a
separate supervised worker and lower its declared image depth, components,
premultiplication, pixel aspect, render scale, temporal extent, region of
definition/interest, and parameter state into the canonical Effect contract.
Begin with a reference plugin that accepts the supported Float32 working-frame
contract; unsupported formats or color/premultiplication assumptions fail
admission. Never silently round-trip through RGBA8 or substitute a differently
interpreted effect in Export. Native GPU texture sharing and vendor overlays
are separate qualifications.

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
