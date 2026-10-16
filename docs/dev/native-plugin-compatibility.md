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
| Audio layout | Native mono/stereo main output and up to 31 auxiliary inputs of the admitted layout; CLAP additionally admits up to 31 ordered same-layout auxiliary outputs and an absent main input, including auxiliary-only inputs; finite owned mono/offline Note and multi-output execution plus the real Surge stereo Main Runtime below; restored-state native metadata and causal PDC | VST3 native multiple outputs, mixed layouts, surround/ambisonic, broader third-party instruments, auxiliary-output sound and product workflow qualification |
| Processor-output graph | Track/Bus processor identity plus Main/Auxiliary key uses the existing routing DAG, producer PDC and Session storage; raw producer tap precedes later rack processors and Mute, retaining Track Solo semantics; schema 33 preserves version-32 author values. CLAP native output IDs bind to this same graph | Product output selection, other rack/owner families, broader native adapters and realtime/performance evidence |
| Audio parameters | Canonical author curves and sample offsets; CLAP plain values, VST normalized values; insertion/rebinding/history/archive state | Native display conversion, units/programs/presets, output events, plugin-origin gestures, large state/resources |
| Audio render mode and threads | Explicit VST realtime/offline selection; CLAP requested-mode probe and execution, hard-realtime/declined-mode refusal; instance-local CLAP symbolic thread checks | Dynamic restart/rescan/latency/tail handling, sleep/request-process scheduling and extension event loops |
| Audio transport and events | Canonical signal sample position reaches VST3 ProcessContext and CLAP seconds transport; unavailable tempo/beat/signature/transport facts are not invented | Tempo-map authoring, MIDI 1/2/SysEx, notes/expression/polyphonic modulation and event routing |
| Bounded Note execution | Public direct-root CLAP Track pre/post scores use the original admission receipt, same-restored-instance processing offer, existing DAG/IPC12 and exclusive Hosted ledger; finite owned Windows canonical-score-to-native-DSP, zero-input mono/offline and author-handoff controls pass | App Playback/Export qualification, windowed/precompiled/nested families, nonzero entry/seek/loop/reverse, VST3 notes, broader instrument shapes/event dialects and third-party processing, and physical realtime |
| Native editors | Product hosting, isolated editor protocol and observation/author handoff exist; first signal and same-view refresh have dedicated qualification gates | Vendor/platform matrix, resize/DPI/focus/keyboard, remaining GUI/timers/FDs and destructive later-entry continuity |
| OpenFX CPU image execution | Filter Source/Output, current-frame full-frame RGBA Float32, progressive; complete typed Straight/PM Float and Opaque signal, exact Normal Over, representation-aware cache and actual nested/opaque output; resident native instances | Other contexts/required inputs, formats, fields, mixed PAR/depth, tiles/scale/temporal/ordered scheduling; nonlinear/GPU/OCIO consumers retain explicit format admission |
| OpenFX capabilities and bootstrap | Actual Float32/CPU advertisement; optional early host bootstrap; CPU-disabled admission refusal; canonical PAR dimensions and progressive fields | GPU execution and negotiated colour/lifetime/synchronization contracts |
| OpenFX author controls | Numeric tuples/colour/bool/choice/StrChoice/UTF-8 text/resources; canonical curve queries and shared transactions | Spatial semantic types, hierarchy/buttons/custom/parametric/interacts, dynamic descriptors, plugin-origin writes/private data |
| OpenFX product callbacks | Product AuxSuite classifies transient handles; typed current-scope feedback and bounded Describe-only/Catalog notices reach their existing owners; controlled question, progress/cancellation and Log tracing exist | Product navigation and scenario/platform qualification; unsatisfied gotoTime fails explicitly |
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
instance's schema-4 processing offer (audio probe schema 3), then the existing registration checks,
worker specification, shared-block sizing, event merge and copied End ownership.
Plugin parameter outputs still pass through canonical author compile,
Source authorization/install, continuing immutable-score rebind and Ack.

Windows owned PCM-input Note controls qualify authored two-port scores at 44.1/48 kHz, literal per-sample output across partitions, release, parameter handoffs and invalid-entry refusal. The separate real Surge control below qualifies one finite stereo Main Runtime and actual rescan handoff. These finite offline paths do not qualify App Playback/Export, arbitrary third-party instruments, realtime performance or other platforms.

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
| Shared processor outputs | Shared-slice baseline: 5,343 ordinary tests across eight libraries passed, including 16 new controls; 269 ignored tests excluded. Three strict groups and the normal Product build passed. | Track/Bus shared DAG qualification; native adapters have their separate conditions below. Product output selection, realtime/performance and other platforms remain open. |
| Owned zero-input CLAP | Six fresh mono/offline Runtime cases at 44.1/48 kHz passed literal 14-frame PCM, NoMedia, Source/author/install/Ack and same-object/Drop-exit checks. | Owned fixture, not general third-party instrument support. |
| Current Audio and shutdown report | 616 ordinary Audio tests and 501 ordinary Core tests passed; respectively 109 and 1 ignored tests excluded. All-target/all-feature Audio Clippy with `-D warnings` and the fresh normal IPC12 Product build passed; the Product retains ten existing warnings. Temporary teardown and capacity-projection markers are absent. | Affected Audio/Core scope, not a new whole-workspace or full native qualification. Shutdown control, ACK, exit observation and cleanup remain distinct; the original budgets are unchanged. |
| Owned three-output CLAP | Direct scoped DSP observed 38 same-object SDK Process calls across three partitions and two entries. The fresh normal IPC12 Product passed 16 canonical Runtime combinations: 44.1/48 kHz, four Main/Aux/fanout routes, two partitions and two entries each; literal PCM, author-state serialization/restore, NoMedia, Source installation and same-object Drop exit0 passed. | Windows mono/offline owned fixture. Aggregate grant is explicitly unbounded; canonical child Process count is unobserved. Live private-DSP state capture, finite aggregate-grant acceptance, third-party multi-output, stereo native workflow, GUI, realtime/performance and other platforms remain unqualified. |
| Surge XT Effects VST3 | The Audio slice normal Product passed the original exact Runtime state/restore, PCM and owner gate. The earlier stress failure received ACK in about 20ms, remained running beyond the 100ms exit budget, then was killed/reaped. | The old post-ACK exit failure remains unresolved. A successful regression does not prove a fix or identify the faulty phase. Windows/offline/48k/stereo/max64 only; no realtime, GUI, algorithm or other-platform qualification. |
| Ring Out CLAP | The Audio slice normal Product passed four cold Runtime cases covering native bypass and active control, block64 and 31/33 partitions, 10,240 frames including the actual startup transition and steady range. | Windows/offline and the stated state/parameters; not independent algorithm, GUI, realtime or other-platform qualification. |
| CLAP port/config metadata | Optional main input and ordered auxiliary inputs/outputs have owned controls. Actual Surge census reports the default auxiliary input and three outputs; the current host does not expose the configuration extension. IPC12 and probe3/processing-offer4 carry exact output identity and order, refusing older incompatible workers/offers. | Configuration selection and real auxiliary-output sound remain unqualified. The finite Surge Main result below does not imply active port rescan or a new host configuration service. |
| Surge XT 1.3.4 CLAP instrument | One actual Windows/offline Runtime passed captured-state restore (50,208 bytes), restored NoteOffer4, 48 kHz stereo/block256, 16,384 finite Main frames with nonzero energy, On127/Off8193 and native input cursor16384. Its real nonempty rescan completed the Track canonical transaction, Source install and Preserved Ack; object and cursor continuity passed, NoMedia was zero, and the same retained process handle observed Drop exit0. Main1 and auxiliary2/3 were prepared with a zero-filled stereo auxiliary input32768; latency0/tail96000 were observed. | Single finite Runtime, explicit unbounded aggregate grant. No independent algorithm, release envelope, terminal Off, auxiliary sound, GUI, App Playback/Export, project reload, realtime/performance or other-platform qualification. Successful Drop does not resolve the older intermittent shutdown failure or independently prove its ACK. |
| OpenFX CPU Filter | The independent BitCrush gate and finite native controls cover the stated current-frame, full-frame RGBA Float32/progressive CPU boundary. The readonly-constructor correction passed 456 checks. | Other contexts, temporal/tiles/scale/fields, native GPU interop and other platforms remain outside this qualification. |
| OpenFX metadata notices and lifecycle | Windows: 76 distinct ordinary controls and 22 actual owned/official Basic cases passed with the fresh normal Product. Describe returns six ordered UTF-8 notices without Render; the live Catalog delivers four feed notices without replay or author mutation. Exact 32/33 limits, Fatal/invalid-UTF-8/native-failure prefixes, cancellation, single Load/Describe, failed-Describe Unload and live descriptors during Unload passed. | Schema2 metadata requests only; whole-request context is not emitter or Render identity. Per-request32 notices, per-bundle256/1 MiB text+id, one result slot. Log uses the existing tracing path; sink output is not separately qualified. No whole-heap, realtime, third-party, GUI, GPU, other-platform or comparative-performance qualification. |
| OpenFX exit observation | Windows: five ordinary and seven actual child-owner controls passed. A fresh normal Product passed two official Basic sessions: resource refusal without advancing work followed by exact rendering/graceful shutdown, and separate Retiring rejection/fallback settlement. Both sealed 4096-byte schema2 maps carried same-handle exit observations; the decoder passed 45 synthetic controls. | Default-off diagnostics only. UTC facts do not replace QPC deadlines or original errors; fallback is not graceful close. No D30C remediation, third-party compatibility, comparative performance or other-platform qualification. |
| Airwindows metadata | Actual inactive parameter index 6 has current zero and a negative default despite an advertised 0..1 range. It occurs before state restore; the host correctly rejects it. | No clamp or plugin-name exception. Upstream inactive-default initialization is supporting evidence; the actual native writer/source-build binding remains unproven. |

The current local environment uses Windows x64, FFmpeg 8.1.2 and static OCIO
2.5.2. No macOS/Linux runtime or performance result is implied by these checks.
Normal Product and App libtest retain their existing warning baselines; zero
strict diagnostics is a separate result.
Audio native results refer to the accepted Audio slice; the later OFX-only
Product qualification is the separate row above.

IPC12 startup carries bounded JSON metadata followed by the exact opaque adapter
bytes on the existing connection. It removes a second JSON byte-array encoding
that could inflate an admitted 512 KiB payload beyond the 1 MiB control limit.
Complete metadata is preflighted before spawn; the child validates version,
nonce and combined extent before reserving raw storage. Raw remains at most
512 KiB, metadata plus raw at most 1 MiB, excluding only the four-byte prefix.
Eight ordinary controls cover exact limits, arbitrary/empty bytes, truncation,
stream alignment, old-version refusal and preflight before spawn. Original
adapter/Seed formats, decoded-byte accounting, owners and timeouts remain.
These are wire and individual-allocation ceilings, not a complete finite
startup-peak grant: metadata encoding/parsing and adapter decoding have temporary
overlapping owners. The actual Surge startup and later finite author/PCM result
have the separate tested conditions above.

Native rescan projection now fallibly reserves its tuple destination and moves
each ID/value in order. A real 775-value delivery previously became capacity1162
when a fallible collect reused a larger-element allocation, correctly tripping
Core's 1024 cumulative-capacity limit after 12 rendered blocks. The limit remains;
four Runtime DTO controls cover 1024, 512+512, aggregate overflow and missing
scalars. Input and target capacities are checked. The two Vec owners briefly
overlap on the non-realtime poll; retained-capacity checks do not establish a
complete transient-peak grant or whole-plugin heap qualification.

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
- CLAP native multi-output and the real Surge Main Runtime have the finite
  offline qualification above. Broader instruments, auxiliary sound, product
  output selection and realtime remain open.
- OFX same-handle exit-time observation passed the finite Windows controls and
  official Basic sessions above. The original 100ms policy and sole Child owner
  remain unchanged; the intermittent exit issue still requires reproduction
  and causal diagnosis.
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
