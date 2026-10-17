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
| Audio transport and events | Canonical signal samples reach VST3 ProcessContext and CLAP seconds transport. The exact Sequence reference supplies CLAP constant Tempo; the candidate no-Note VST3 path supplies anchored quarter notes and Tempo through the existing SDK owner | Candidate lifecycle acceptance is held below. Tempo maps, event PPQ, signatures/playing facts, MIDI 1/2/SysEx, notes/expression/polyphonic modulation and event routing remain separate gaps |
| Bounded Note execution | Public direct-root CLAP Track pre/post scores use the original admission receipt, same-restored-instance processing offer, existing DAG/IPC15 and exclusive Hosted ledger; completion and admitted channels are explicit. Finite owned Windows canonical-score-to-native-DSP, zero-input mono/offline and author-handoff controls pass | App Playback/Export qualification, windowed/precompiled/nested families, nonzero entry/seek/loop/reverse, VST3 notes, broader instrument shapes/event dialects and third-party processing, and physical realtime |
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
instance's schema-5 processing offer (embedded processing schema 3), then the existing registration checks,
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

## Current qualification summary (2026-10-17)

| Area | Result and conditions | Limit |
| --- | --- | --- |
| Shared processor outputs | Shared-slice baseline: 5,343 ordinary tests across eight libraries passed, including 16 new controls; 269 ignored tests excluded. Three strict groups and the normal Product build passed. | Track/Bus shared DAG qualification; native adapters have their separate conditions below. Product output selection, realtime/performance and other platforms remain open. |
| Owned zero-input CLAP | Six fresh mono/offline Runtime cases at 44.1/48 kHz passed literal 14-frame PCM, NoMedia, Source/author/install/Ack and same-object/Drop-exit checks. | Owned fixture, not general third-party instrument support. |
| Current Audio checks | The final candidate passes 669 ordinary Audio tests; 114 native controls were precisely excluded. Full Audio Strict and eight resource tests pass. The matching normal IPC15/Discovery3/CLAP Probe4 test-profile Product builds with the original ten App library warnings. Twelve owned/real-plugin native gates and the SDK Gain probe pass under the original budgets. | Not whole-workspace or App test-suite regression. Earlier TestWorker and Unprobed CLAP retained-configuration post-ACK exit failures remain open. The new fresh-probe retained path passes without closing those failures. Overall integration remains HOLD; no budget or retry workaround changed. |
| Native-probe Factory preparation | Fresh discovered VST3 and normal CLAP probes move their bounded input-name snapshot into the existing Factory; actual create still validates restored native contracts and names in the original same-child Ready scope. Ten new ordinary controls pass, including 0+1 versus 1+1 launches, keyed name equality/drift cleanup, storage/seed ownership, strict Discovery3/normal Probe4 and the 31-port boundary. Both native sidechain negatives require the exact restored-port error from actual create; original PCM/state controls pass. CLAP seed, overflow and retained controls pass. | Manual/generic and CLAP Note-offer specs retain real validation launch plus strict shutdown. Probe is not proof of start/split/Ready; a name change is explicit unsupported presentation drift. Root-grant coverage of cold probes, complete native heaps, realtime safety and timed overhead improvement are not established. |
| CLAP authored constant Tempo | One exact native gate passed four rate × None/Some Runtime cases at 44.1/48 kHz: two epochs, different partitions, literal PCM, Scope-local gain automation, 3/2 source retiming and six-frame PDC. The same actual native objects survived re-entry and their retained OS handles observed Drop exit0. | Owned stereo/offline fixture; 32 MiB local scratch and explicitly unbounded aggregate grant, not a budget-edge test. No beat/bar/meter/playing, tempo map, selected nested musical execution, third-party, realtime/performance or other-platform qualification. The separate 2s Drop observation does not prove the 100ms graceful budget. |
| Candidate VST3 anchored musical audio | The bounded Worker gate now passes all eight 44.1/48 kHz × None/Some × signed-start cases. The canonical gate again passes Scope-local automation, 3/2 source retiming, two three-frame delays, six-frame PDC and two epochs with independent Q/Tempo bits and PCM. Continuous objects and host-serial seek rebuilds pass. | The previous failing temporary Factory validation instance is removed from the fresh discovered VST3 path; this does not establish a 100ms teardown fix. Canonical control uses 32 MiB session scratch and an unbounded aggregate grant. No full callback-count/order, Note/Event PPQ, realtime/performance or other-platform qualification; overall integration remains HOLD. |
| Owned three-output CLAP | Direct scoped DSP observed 38 same-object SDK Process calls across three partitions and two entries. The fresh normal IPC15 Product passed 16 canonical Runtime combinations: 44.1/48 kHz, four Main/Aux/fanout routes, two partitions and two entries each; literal PCM, author-state serialization/restore, NoMedia, Source installation and same-object Drop exit0 passed. | Windows mono/offline owned fixture. Aggregate grant is explicitly unbounded; canonical child Process count is unobserved. Live private-DSP state capture, finite aggregate-grant acceptance, third-party multi-output, stereo native workflow, GUI, realtime/performance and other platforms remain unqualified. |
| Surge XT Effects VST3 1.3.4 | Candidate Some and original None gates each pass two restored Runtime observations plus native bypass: Windows/offline/48 kHz/stereo/block64, 4096 finite frames per Runtime, exact prepared reference, Source closure and same-object/Drop exit0. The original state/input and finite root grant (one occurrence, 32 MiB fixed, 16 MiB prepared) are retained. | These are finite PCM/lifecycle observations, without an independent DSP or Tempo-response oracle. The old post-ACK exit failure remains unresolved; observed Drop exit0 does not prove the 100ms graceful budget. No GUI, realtime/performance or other-platform qualification. |
| Official AGain Sample Accurate VST3 3.8.0.0 | The unmodified MIT SDK 3.8.0 reference passes actual List/Probe v3, capture and fresh restore of its 56-byte state. Three canonical Runtime cases pass 4096 frames each: two fresh Gain ID1=0.5 cases match an independent half-gain PCM reference bit for bit; native Bypass ID0=1 matches unity output. Windows/offline/48 kHz/stereo Float32/block64, absent musical reference, sequence-zero Hold constants, the same finite grant/Source closure/native owner and Drop exit0 checks as Surge. Both original Surge gates pass with the same current Product and fresh-probe Factory path. | The reference was built with the SDK's `/fp:fast`; inputs are exact finite dyadics and the oracle includes canonical Graph zero normalization. This is constant-parameter/reference-plugin qualification, not automation ramps, offset Bypass, nondefault private-state capture, GUI, realtime/performance, other-platform or whole-standard qualification. Original exit failures remain open. |
| Ring Out CLAP | The Audio slice normal Product passed four cold Runtime cases covering native bypass and active control, block64 and 31/33 partitions, 10,240 frames including the actual startup transition and steady range. | Windows/offline and the stated state/parameters; not independent algorithm, GUI, realtime or other-platform qualification. |
| Clack SDK Gain reference | The existing unmodified MIT/Apache-2.0 example source, built through a local standalone manifest, passes actual normal Probe4 with embedded processing3, restored four-byte gain state, parameter and port metadata at 48 kHz/stereo/Realtime/block512. | Contract-probe qualification only. The example is not an oracle for sample-accurate automation; its event batches can apply gain repeatedly to the whole buffer. No DSP/PCM, Factory, upstream binary/build-lock, realtime-performance or platform qualification. |
| CLAP port/config metadata | Optional main input and ordered auxiliary inputs/outputs have owned controls. Actual Surge census reports the default auxiliary input and three outputs; the current host does not expose the configuration extension. IPC15, normal Probe4 with required names and embedded processing3, full processing-offer5 and standalone Note probe2 carry exact identity and order, refusing older incompatible workers/offers. The pinned owned parent-query gate executes full and standalone probes for both default and restored state; original public zero-input Note and canonical three-output gates also pass. | Normal Probe4 is a private envelope, not a revision of Note offers or shared processing metadata. Configuration selection and real auxiliary-output sound remain unqualified. The finite Surge Main result below does not imply active port rescan or a new host configuration service. |
| Surge XT 1.3.4 CLAP instrument | One actual Windows/offline Runtime passed captured-state restore (50,208 bytes), restored NoteOffer5/notes2 with NativeEnd and sixteen admitted channels, 48 kHz stereo/block256, 16,384 finite Main frames with nonzero energy, On127/Off8193 and native input cursor16384. Its real nonempty rescan completed the Track canonical transaction, Source install and Preserved Ack; object and cursor continuity passed, NoMedia was zero, and the same retained process handle observed Drop exit0. Main1 and auxiliary2/3 were prepared with a zero-filled stereo auxiliary input32768; latency0/tail96000 were observed. | Single finite Runtime, explicit unbounded aggregate grant. No independent algorithm, release envelope, terminal Off, auxiliary sound, GUI, App Playback/Export, project reload, realtime/performance or other-platform qualification. Successful Drop does not resolve the older intermittent shutdown failure or independently prove its ACK. |
| OpenFX CPU Filter | The independent BitCrush gate and finite native controls cover the stated current-frame, full-frame RGBA Float32/progressive CPU boundary. Three owned descriptor gates now verify separate Source/Output temporal refusal and disconnected optional admission; six existing capability/parameter regressions pass, including normal resident/Graph execution. The readonly-constructor correction passed 456 checks. | Descriptor admission adds no temporal rendering. Other contexts, temporal/tiles/scale/fields, native GPU interop and other platforms remain outside this qualification. |
| OpenFX metadata notices and lifecycle | Windows: 76 distinct ordinary controls and 22 actual owned/official Basic cases passed with the fresh normal Product. Describe returns six ordered UTF-8 notices without Render; the live Catalog delivers four feed notices without replay or author mutation. Exact 32/33 limits, Fatal/invalid-UTF-8/native-failure prefixes, cancellation, single Load/Describe, failed-Describe Unload and live descriptors during Unload passed. | Schema2 metadata requests only; whole-request context is not emitter or Render identity. Per-request32 notices, per-bundle256/1 MiB text+id, one result slot. Log uses the existing tracing path; sink output is not separately qualified. No whole-heap, realtime, third-party, GUI, GPU, other-platform or comparative-performance qualification. |
| OpenFX exit observation | Windows: five ordinary and seven actual child-owner controls passed. A fresh normal Product passed two official Basic sessions: resource refusal without advancing work followed by exact rendering/graceful shutdown, and separate Retiring rejection/fallback settlement. Both sealed 4096-byte schema2 maps carried same-handle exit observations; the decoder passed 45 synthetic controls. | Default-off diagnostics only. UTC facts do not replace QPC deadlines or original errors; fallback is not graceful close. No D30C remediation, third-party compatibility, comparative performance or other-platform qualification. |
| Airwindows metadata | Actual inactive parameter index 6 has current zero and a negative default despite an advertised 0..1 range. It occurs before state restore; the host correctly rejects it. | No clamp or plugin-name exception. Upstream inactive-default initialization is supporting evidence; the actual native writer/source-build binding remains unproven. |

The current local environment uses Windows x64, FFmpeg 8.1.2 and static OCIO
2.5.2. No macOS/Linux runtime or performance result is implied by these checks.
Normal Product retains ten existing warnings; App test typechecking reports
ten library and twenty-five test warnings in unchanged areas. Zero strict Audio
diagnostics is a separate result. OpenFX qualification is stated separately above.

IPC15 startup carries bounded JSON metadata followed by the exact opaque adapter
bytes on the existing connection. It removes a second JSON byte-array encoding
that could inflate an admitted 512 KiB payload beyond the 1 MiB control limit.
Complete metadata is preflighted before spawn; the child validates version,
nonce and combined extent before reserving raw storage. Raw remains at most
512 KiB, metadata plus raw at most 1 MiB, excluding only the four-byte prefix.
The required nullable musical reference is checked in preparation, Ready and
callback binding; schema34 preserves absent/null legacy references and refuses
legacy-tagged non-null values. Eight ordinary wire controls cover exact limits, arbitrary/empty bytes, truncation,
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

## VST3 event contracts and local Note adapter

Required discovery/payload version 2 carries the expected event-bus contract
through the existing probe, registration and payload owners. The actual restored,
configured, stopped instance is rechecked during preparation, cold rebuilding and
retained configuration. Canonical VST3 Note remains refused; the new musical
reference does not enable it.

The shared Note contract now requires completion policy and admitted channel
width. `SuccessfulOffBlock` returns pairing slots after the actual successful
call, without claiming DSP voice termination; CLAP keeps `NativeEnd`/16 channels.
Whole-root admission follows actual priming/public calls, grants no same-call Off
credit, and preserves IDs and failure poisoning. Cached identity checks can be
quadratic; this foundation does not qualify realtime or performance.

The Windows owned test-profile Product passed eleven original native gates:
VST3 retained/seed and two event-metadata controls, CLAP retained/seed/overflow,
editor user-close/frame-clear retry, CLAP full/standalone Note probes, zero-input
public Notes and three-output Runtime/DAG. The latter
covers 16 rate/route/partition combinations and two entries with literal PCM.
MNE3 stable capture produces fresh ordinary PCM; unannounced default-flag drift
is refused on the same cohort/object with zero DSP calls. Post-Drop same-handle
Exit0 is an OS observation, not proof of the original 100 ms graceful budget.

The complete VST3/CLAP Worker Box header, including the fixed 32-by-2 event
contract, now enters the original lane profile and sole Root FixedResident fee.
Startup compares the actual dynamic Box layout before Ready; scoped lane checks
observe their own seven roles separately. IPC15 requires eight fixed owners and
rejects old or unknown shapes. Resource controls prove exact-header admission
and a one-byte-short Root refusal before Session factory creation. Validation
children run before the final Root grant; cold nested/transient allocations,
plugin-private heap and physical realtime remain unqualified.

The stopped-owner control and backend lifecycle-record fixes use the same
retained owner. The primitive census writes directly to bounded metadata without
collecting names; its successful-call probe observed zero same-thread Rust host
allocator operations. This does not cover plugin heaps or prove realtime safety.
The shared foundation passed 656 ordinary Audio, eight resource and strict Audio
Clippy gates. The SDK now checks the complete signed sample window before input
consumption or DSP, including continuous time only when valid. Five newly linked
Main SDK Note integration gates pass: public/scoped near-limit refusal preserves
PCM and same-instance audit; negative, partial and exact-limit windows remain
valid. Post-native counter failure keeps cleanup and the original plugin error.
The anchored musical SDK slice separately passes 335 ordinary controls and nine
owned DLL gates. Its validated exact reference projects absolute signed samples
to quarter notes and constant Tempo, with nearest-even binary64 rounding across
native chunks. Independent mode7 DSP controls cover 44.1/48 kHz, local/scoped
roles, None/Some and negative/near-limit positions, including same-object
processing counts and queued-parameter refusal/recovery. The new reference
currently admits pure audio only: queued/staged events, busy/poisoned ingress
and nonempty checked batches refuse before consumption. Late ingress survives
success and error paths; callback inputEvents is temporarily null and restored.
This does not project Event PPQ. The existing process Box contains the reference
in place; the SDK complete-Box observer does not charge that nested owner to
the Audio grant.
The candidate no-Note VST3 adapter now consumes the same Core reference without
a second clock or state source. Exact preparation/Ready/seed and original native
metadata/rebuild checks remain; selected nested musical execution and canonical
VST3 Note stay refused. The owned canonical and real Surge controls above pass,
but the bounded Worker shutdown failure keeps integration on hold. CLAP retains
its separate seconds/constant-Tempo semantics; system clocks remain unavailable.
Older Gain/Surge and OFX exit failures remain open; a late reaped Exit0 never
converts a deadline failure into a pass. No third-party instrument or other-platform
qualification is implied.

The independent MNE4 SDK oracle admits Main Event0 with Aux inactive, preserving
the old three modes and SDK gates. Inline/scoped calls each pass two runs of 1025
pairs with literal PCM and complete actual-PID records of 515 calls/4104 tuples.
Its 1024-call/8192-event audit is reserved on control; complete records are written
only after processing stops, separately from the twelve-byte plugin state.

The same component retains declarations and accepted activation facts; a query
does not invent a native active-state getter: `host_active` records only that
owner's last successful activation ACK, independently of its restored default flag.
Preparation rejects topology drift.
Loading now limits event buses to 32 per direction and rejects invalid metadata;
zero-channel declarations remain visible. The checked subset requires nonnegative
note IDs, MIDI channels 0..15 within an active declared bus, valid ordered offsets
and an empty legacy input, preserves original
scalar values, and retires remaining input after a started processing failure.
It does not roll back an executed prefix. Canonical VST3 Note still needs an
SDK/adapter consumption of the existing Sequence musical reference for event PPQ. Third-party
instruments, other platforms, realtime and performance remain unqualified.

## Open issues and integration constraints

- The temporal-clip baseline Output case failed before its Describe action chain
  and left a descendant for Job reaping. It is not a valid admission RED. The
  corrected candidate passes all three descriptor gates; this does not explain
  or close that startup/ownership failure.
- The intermittent Surge failure was reproduced after a valid shutdown ACK,
  with the child still running beyond the existing 100ms exit budget. Host
  termination and reaping succeeded; the resulting exit1 is not a spontaneous
  plugin crash. Bounded diagnostic runs have not located the failed phase or
  root cause; neither Gain nor Surge has been attributed to plugin destruction.
  Later successful runs do not establish a fix. No timeout widening or retry
  workaround has been applied.
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
