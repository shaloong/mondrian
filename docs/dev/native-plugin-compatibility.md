# Native plugin compatibility and qualification

## Objective and standards baseline

The product objective is complete, negotiated VST3, CLAP and OpenFX hosting,
with canonical authoring transactions, correct preview/export output, usable
native and product controls, and measured execution cost. Loading an ABI and
passing owned fixtures do not qualify every conforming plugin. Optional standard
features must have an explicit implemented or unavailable negotiation result.

Checked on 2026-10-08:

- [VST SDK 3.8.1](https://steinbergmedia.github.io/vst3_dev_portal/pages/Versions/Version%2B3.8.1.html),
  published 2026-08-11. The new transport-control, MIDI 2 articulation and exchange
  interfaces require explicit implementation and qualification.
- [CLAP 1.2.10](https://github.com/free-audio/clap/tree/1.2.10), also reviewed on 2026-10-09.
  Stable extensions and draft extensions are separate obligations.
  The pinned 1.x binding's ABI compatibility does not establish feature parity.
- [OpenFX 1.5.1 release](https://github.com/AcademySoftwareFoundation/openfx/releases/tag/OFX_Release_1.5.1).
  Main-branch clarifications and extensions must be identified separately from
  the published release. The current product advertises host API 1.4; selected
  newer properties do not establish complete 1.5.1 hosting.

The detailed [audio standard matrix](native-audio-standard-features.md) and
[OpenFX standard matrix](native-openfx-standard-features.md) identify actual
product consumers and pending closure for each feature family.

## Implementation boundary

| Feature family | Current implemented boundary | Remaining work |
| --- | --- | --- |
| Native discovery and isolation | Selected class/descriptor, fingerprinted native binary, supervised discovery and persistent processing child, bounded control traffic and shared buffers | Standard scan locations/workflow, permitted vendor corpus, platform and native editor lifecycle |
| VST3 SDK interfaces | One canonical runtime binding package generated from verified SDK 3.8.1 sources; independent layout/signature and actual cross-language calls for all nine methods of the four new interfaces | Optional host-service implementation, MIDI 2 event routing, native MSVC/macOS/Linux qualification; upstream generator excludes SDK testing interfaces |
| Audio layout | Native mono/stereo main output, up to 31 auxiliary inputs of the admitted layout; explicit absent CLAP main input, including ordered auxiliary-only inputs; finite owned mono/offline Note execution; restored-state native port metadata and causal PDC | Native multiple-output admission, mixed layouts, surround/ambisonic, third-party instrument processing and zero-input stereo DSP |
| Processor-output graph | Track/Bus processor identity plus Main/Auxiliary key uses the existing routing DAG, producer PDC and Session storage; raw producer tap precedes later rack processors and Mute, retaining Track Solo semantics; schema 33 preserves version-32 author values | Native adapter multiple-output qualification, product output selection, other rack/owner families and realtime/performance evidence |
| Audio parameters | Canonical author curves and sample offsets; CLAP plain values, VST normalized values; insertion/rebinding/history/archive state | Native display conversion, units/programs/presets, output events, plugin-origin gestures, large state/resources |
| Audio render mode and threads | Explicit VST realtime/offline selection; CLAP requested-mode probe and execution, hard-realtime/declined-mode refusal; instance-local CLAP symbolic thread checks | Dynamic restart/rescan/latency/tail handling, sleep/request-process scheduling and extension event loops |
| Audio transport and events | Canonical signal sample position reaches VST3 ProcessContext and CLAP seconds transport; unavailable tempo/beat/signature/transport facts are not invented | Tempo-map authoring, MIDI 1/2/SysEx, notes/expression/polyphonic modulation and event routing |
| Bounded Note execution | Public direct-root CLAP Track pre/post scores use the original admission receipt, same-restored-instance processing offer, existing DAG/IPC v10 and exclusive Hosted ledger; finite owned Windows canonical-score-to-native-DSP, zero-input mono/offline and author-handoff controls pass | App Playback/Export qualification, windowed/precompiled/nested families, nonzero entry/seek/loop/reverse, VST3 notes, other instrument shapes/event dialects, third-party processing and physical realtime |
| Native editors | Product hosting, isolated editor protocol and observation/author handoff exist; first signal and same-view refresh have dedicated qualification gates | Vendor/platform matrix, resize/DPI/focus/keyboard, remaining GUI/timers/FDs and destructive later-entry continuity |
| OpenFX CPU image execution | Filter Source/Output, current-frame full-frame RGBA Float32, progressive; complete typed Straight/PM Float and Opaque signal, exact Normal Over, representation-aware cache and actual nested/opaque output; resident native instances | Other contexts/required inputs, formats, fields, mixed PAR/depth, tiles/scale/temporal/ordered scheduling; nonlinear/GPU/OCIO consumers retain explicit format admission |
| OpenFX capabilities and bootstrap | Actual Float32/CPU advertisement; optional early host bootstrap; CPU-disabled admission refusal; canonical PAR dimensions and progressive fields | GPU execution and negotiated colour/lifetime/synchronization contracts |
| OpenFX author controls | Numeric tuples/colour/bool/choice/StrChoice/UTF-8 text/resources; canonical curve queries and shared transactions | Spatial semantic types, hierarchy/buttons/custom/parametric/interacts, dynamic descriptors, plugin-origin writes/private data |
| OpenFX product callbacks | Native message/progress/timeline primitives exist | Typed application messages, real cancellation/progress/transport responses and consistent persistence |
| Product interaction | Shared Rack control identity preserves keyboard ownership through canonical refresh; focused text owns clipboard and bounded local Undo/Redo; single-line visual BiDi/EGC navigation and associated caret geometry; exact model ingress with one-line source projection | Remaining control adoption, accessible numeric names, multiline visual navigation and physical/native-editor acceptance |
| Performance | Persistent children, bounded mappings, preallocated native audio buffers and execution budgets | Measured native DSP plus IPC percentiles/deadlines, video frame budgets, warm/cold/memory/overload and vendor/platform matrices |

Note preparation retains checked, fallible vector reserves, actual-capacity
acceptance, in-place ID sorts and the existing Project/Resolve/Bind/Retained
payload readers. String cloning remains infallible. Direct unwindowed selected
scores reserve B0 + compiler payload + maximum lowering-phase payload before
materialization; this logical reservation does not bound physical allocation
peaks or replace the final native/IPC/Hosted and author-handoff grants. A private
ledger helper carries the same non-cloneable receipt through actual Plan
preparation and completes the original Session accounting once. It preflights
every target before media, child or native-factory work. Only local CLAP Track
pre/post targets in a direct root are admitted; ordinary standalone Plan APIs and
windowed, precompiled, nested and mixed-format Note families retain refusals.

Public Runtime Note processing requires an installed owner, cold entry at sample
zero and positive contiguous forward blocks. Empty Note requests refuse before
native control I/O; unsupported entry and changed-score handoff preserve the
existing owner and clock. The discovered CLAP resolver uses the same restored
instance's schema-2 processing offer, then the existing registration checks,
worker specification, shared-block sizing, event merge and copied End ownership.
Plugin parameter outputs still pass through canonical author compile,
Source authorization/install, continuing immutable-score rebind and Ack.

Windows owned PCM-input Note controls qualify authored two-port scores at 44.1/48 kHz, literal per-sample output across partitions, release, parameter handoffs and invalid-entry refusal. This finite offline path does not qualify App Playback/Export, arbitrary third-party instruments, realtime performance or other platforms.

App PCM preparation carries the prepared root score's required origin through
Media's existing continuity model. Explicit fresh entries are checked before
output deactivation and Source/PCM retirement. Pause, end-of-program and denied
recovery suspend PCM without admitting Enter, retaining installed Source/control
ownership and actual callback retirement obligations. A late Preserved Ack cannot
resume suspended PCM. Author State Entry intent updates and nonzero Continue
windows retain their semantics; ordinary stateful plans gain no fixed-origin
restriction. Notes still require entry at zero. Arbitrary seek replay and complete
App Note Playback/Export qualification remain outside this evidence.

Host-model controls cover the corresponding Audio, Media and App handoffs; this adds no third-party or realtime qualification.

### Authored content extent and frame ranges, 2026-10-16

`Sequence::total_duration` now takes the maximum end of every Clip and authored
Note, including unordered lanes and legal overlapping Clip placements. Mute,
bypass, plugin availability and tail do not redefine authored duration. App and
Preview share the checked last-occupied-frame projection, `ceil(end)-1`, clamped
to zero. Export retains its exclusive-end ceiling and explicit Out/WorkArea
rules. No project field, cache, playback clock or second duration authority is
introduced. Nested Export controls qualify selected visual ranges and finite
Transition handles; they do not claim rendered transparent pixels or audio DSP.

The table describes implementation boundaries. Qualification below states the tested conditions; detailed consumer obligations remain in the linked audio and OpenFX feature matrices.

## Current qualification summary (2026-10-16)

| Area | Result and conditions | Limit |
| --- | --- | --- |
| Shared processor outputs | Before the shutdown-report change: 5,343 ordinary tests across eight libraries passed, including 16 new controls; 269 ignored tests excluded. Three strict groups and the normal Product build passed. | Track/Bus shared DAG qualification; native multi-output adapters, product output selection, realtime/performance and other platforms remain open. |
| Owned zero-input CLAP | Six fresh mono/offline Runtime cases at 44.1/48 kHz passed literal 14-frame PCM, NoMedia, Source/author/install/Ack and same-object/Drop-exit checks. | Owned fixture, not general third-party instrument support. |
| Current shutdown report | 597 ordinary Audio tests passed, including nine new regressions and one inert child fixture; 106 ignored tests excluded. Diagnostic Audio and normal Product builds passed with optional non-RT phase markers; the normal Product retained ten existing warnings. Earlier five-crate strict Clippy passed with zero diagnostics. | Markers were removed from source after diagnosis. The next native-candidate build must refresh the Product; no overall compatibility qualification is implied. |
| Surge XT Effects VST3 | Older mixed-source stress reproduced restore-b validation shutdown failure on attempt two: ACK in about 20ms, child still running after the 100ms exit budget, then killed/reaped. Fresh same-source diagnostic builds passed five instrumented and twelve unflagged cases through the original PCM and owner gates. The instrumented cases completed all shutdown phases. | The old post-ACK exit failure remains unresolved. Successful later cases do not prove a fix or identify the faulty phase. Windows/offline/48k/stereo/max64 only; no realtime, GUI, algorithm or other-platform qualification. |
| Ring Out CLAP | Four cold Runtime cases covered native bypass and active control, block64 and 31/33 partitions, 10,240 frames including the actual startup transition and steady range. | Windows/offline and the stated state/parameters; not independent algorithm, GUI, realtime or other-platform qualification. |
| CLAP port/config metadata | Optional main input and ordered auxiliary inputs have owned controls. Actual Surge census reports the default auxiliary input and three outputs; the current host does not expose the configuration extension. | Configuration selection and native multiple-output processing remain unqualified. The 24-file native adapter candidate has source review only. |
| OpenFX CPU Filter | The independent BitCrush gate and finite native controls cover the stated current-frame, full-frame RGBA Float32/progressive CPU boundary. The readonly-constructor correction passed 456 checks. | Other contexts, temporal/tiles/scale/fields, native GPU interop and other platforms remain outside this qualification. |
| Airwindows metadata | Actual inactive parameter index 6 has current zero and a negative default despite an advertised 0..1 range. It occurs before state restore; the host correctly rejects it. | No clamp or plugin-name exception. Upstream inactive-default initialization is supporting evidence; the actual native writer/source-build binding remains unproven. |

The current local environment uses Windows x64, FFmpeg 8.1.2 and static OCIO
2.5.2. No macOS/Linux runtime or performance result is implied by these checks.
Normal Product and App libtest retain their existing warning baselines; zero
strict diagnostics is a separate result.

## Open issues and integration constraints

- The intermittent Surge failure was reproduced after a valid shutdown ACK,
  with the child still running beyond the existing 100ms exit budget. Host
  termination and reaping succeeded; the resulting exit1 is not a spontaneous
  plugin crash. Narrow temporary phase markers will distinguish ingress join,
  owner destruction and later process exit. Later bounded instrumented and
  unflagged runs did not reproduce it; temporary markers have been removed.
  No timeout widening or retry workaround has been applied.
- The report's `exit_loop` timer describes the whole exit
  loop plus its last existing result. It is not the timestamp/duration of the
  final `try_wait`, nor a native Drop duration.
- CLAP native multi-output and OFX exit-time candidates remain in separate
  working trees and have not completed actual host execution. Preserve the new
  shutdown report when rebasing their overlapping files.
- Process operation, shutdown control and graceful exit currently share budget
  policy. The final kill/reap wait has no separate timeout, so the existing
  100ms values do not describe a strict total shutdown limit. Budget changes
  require a separate contract review and are not a fix for the observed failure.
- A safe conservative resource-overlap bound may reject a tight grant. It is
  not a minimum-peak or whole-plugin-heap measurement; physical realtime safety
  requires actual instrumentation.

## Qualification and record policy

A result records the tested feature, plugin/version, platform, relevant input and
render settings, suite/result, and remaining limits. A mock, SDK validator,
successful load or compilation does not establish third-party compatibility.
Preview/export, state round trips, interruption and ownership are checked where
applicable. A changed build must not be silently substituted during an active run;
correctness and release-performance results remain distinct.

Performance records use comparable hardware, plugin, input, sample rate/block
size or image format, quality and build profile. Measure warmup, processing and
host overhead, percentiles/deadlines and resource costs as needed; a crash timeout
is not an audio deadline, and a wall-clock gate duration is not DSP cost.

Keep concise current conclusions, useful reproduction inputs and actionable open
issues. Do not accumulate historical executable copies, source-hash inventories,
raw receipts or repeated audit packets. Rerun the complete affected suite when
needed. Local next steps and candidate ownership are in the small ignored
`.scratch/plugin-host-sprint/processor-output-routing-checkpoint.json`; Git and
the feature matrices retain the durable implementation/contract record.
