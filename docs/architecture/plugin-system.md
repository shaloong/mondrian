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
interleaved blocks through planar CLAP buffers. It rejects unsupported
parameter lanes, auxiliary buses, and incompatible layouts.
Descriptor discovery executes one installed library in a separate deadline-bound
child and validates its bounded response. The same child can probe a selected
definition for exact Mono/Stereo port, latency, and tail facts after restoring
its author state. `DiscoveredClapAudioProcessorSpecResolver` binds explicitly
selected libraries to those probes during preparation; the processing child
checks the facts again before admission. The App's Preview, idle warmup, and
Export queue now share one injected resolver; the default remains built-in only.
An installed reference-plugin test verifies the Preview adapter launches the
real CLAP worker and renders a block. Automatic installed-path enumeration,
project insertion UI, state capture, parameter automation, binary revision
binding, and a full Preview/Export signal-parity test remain required before
CLAP is advertised as a complete product feature. VST3 and OpenFX binaries remain
unhosted and unavailable. The visual Effect registry/DSL does not host OpenFX.

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

VST3 follows through the same Worker, using the VST 3.8 or newer MIT-licensed
SDK. Its Adapter must honor component/controller separation, bus activation,
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
