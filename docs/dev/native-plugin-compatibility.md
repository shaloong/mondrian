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
| Audio transport and events | Canonical signal samples reach VST3 ProcessContext and CLAP seconds transport. The exact Sequence reference supplies CLAP constant Tempo; the candidate no-Note VST3 path supplies anchored quarter notes and Tempo through the existing SDK owner | Candidate lifecycle acceptance is held below. Tempo maps, signatures/playing facts, MIDI 1/2/SysEx, broader Note/event dialects, expression/polyphonic modulation and event routing remain separate gaps |
| Bounded Note execution | Public direct-root CLAP Track pre/post scores and VST3 scores with an explicit musical reference use the original admission receipt, existing DAG/current IPC18 and exclusive Hosted ledger; completion and admitted channels are explicit. Finite owned canonical-score-to-native-DSP controls pass under the separate format conditions below | App Playback/Export qualification, windowed/precompiled/nested families, nonzero entry/seek/loop/reverse, broader instrument shapes/event dialects and physical realtime; third-party qualification is separate below |
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
every target before media, child or native-factory work. Local Track pre/post
targets in a direct root admit CLAP; VST3 additionally requires an explicit
musical reference. Standalone, windowed, precompiled and nested Note paths retain refusals.
Mixed-format execution has no native qualification.

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

For the ordinary None-prefix handoff, cancellation and claim validation precede
the actual acknowledgement. A confirmed acknowledgement completes successfully
even if its callback cancels the operation; it is not reported as retryable after
the receipt has been retired. Three ordinary controls cover these boundaries.

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

## Current qualification summary (2026-10-18)

| Area | Result and conditions | Limit |
| --- | --- | --- |
| Shared processor outputs | Shared-slice baseline: 5,343 ordinary tests across eight libraries passed, including 16 new controls; 269 ignored tests excluded. Three strict groups and the normal Product build passed. | Track/Bus shared DAG qualification; native adapters have their separate conditions below. Product output selection, realtime/performance and other platforms remain open. |
| Media completion owner retirement | Four exact baseline failures reproduce caller/producer-thread destruction and publication after stop. All four pass after the fix and remain covered in the current 108 ordinary Playback controls (default plus validation features), including foreign Drop panic, 250 renderer plus 250 PCM capacity and the original 256/257 foreign-owner boundary. Only original retained Work enters the existing retirement worker; original180s/2s and compiler-idle boundary checks remain. | Windows ordinary qualification; one ignored actual MMCSS case is excluded. Native-child observation is separate below. No whole-shutdown100ms, cold-construction custody, realtime, comparative-performance or other-platform qualification. |
| Native child settlement observation | The prior Windows baseline passes Core509, Audio719, Playback105 and App74 ordinary controls, plus two actual OS-owner controls. Current/nested/held Sessions and Timeline renderer aliases retain counted scopes; only the original ProcessOwner reports its child's final result. Its matching normal Product/owned-VST3 Runtime-to-Media controls pass: real forwarding/producer registration, Retiring gives incomplete/unresolved, Settled gives complete/resolved, zero PCM and original180s/2s Job settlement. Queued App typed-refusal controls also cover retirement and external-alias unresolved receipts. | Verified within this finite admitted-instance boundary; the earlier false-healthy projection is fixed. Test-only HANDLE cleanup does not upgrade original-owner evidence. No residual Child custody, legal native App handoff, PCM, whole-shutdown100ms, third-party or other-platform qualification; [thread/heap limits](native-audio-standard-features.md#shared-playback-shutdown) still apply. |
| Cold native construction observation | Audio725, Playback108 and App76 ordinary controls pass (909, ten new); normal Product and the additional integration type check pass. The original App builder borrows its Media tracker domain through probes, validation and retained creation, with Session counting before populate and original-owner registration immediately after spawn. Four actual owned VST3/CLAP probe gates pass, including rejected registration preserving successful metadata without false settlement. Two additional actual App owned-VST3 gates pass installed-catalog construction through Timeline/Runtime/Media prepare and shutdown: released aliases give complete closure; an external alias gives an unresolved immutable receipt. Both verify the original Source claim is removed. App76 was rerun after this test-only addition; unchanged180s/2s/Job/idle-boundary checks apply. | Finite Windows owned probe and zero-PCM/no-device App closure qualification. This does not prove each App cold probe's registration or legal native handoff. Independent discovery/create_instance, input inspection, Idle Warmup and precompiled Export construction remain unobserved. No residual custody, whole-shutdown100ms, realtime, full heap, comparative-performance or other-platform qualification. Existing App warnings remain; no strict GO. |
| Owned zero-input CLAP | Six fresh mono/offline Runtime cases at 44.1/48 kHz passed literal 14-frame PCM, NoMedia, Source/author/install/Ack and same-object/Drop-exit checks. | Owned fixture, not general third-party instrument support. |
| Latest full matched IPC18 validation | All 33 selected audio stages pass: SDK384 and Audio713 ordinary tests, eight resource tests and 61 distinct native controls across 64 receipts. SDK/Audio precise exclusions are 12/137; unchanged Core505, Timeline259 and Project59 retain their prior qualification. Audio Strict has zero diagnostics. Matching current SDK, Audio, owned DLLs and normal Product cover zero-input, Note, state, sidechain, music, CLAP, AGain, both Surge Effects gates and Ring Out, plus the rescan controls below. | Windows finite functional qualification, with all Cargo idle at each gate's boundary snapshots; not all-call scheduling or performance proof. Original 180s/2s budgets and shutdown assertions are unchanged. All eleven earlier exit/startup failures and the current OFX readonly shutdown failure remain open. No whole-workspace, broader restart/MDA recovery, App Playback/Export, realtime, comparative-performance or other-platform qualification. |
| SDK restart-source and parameter-input ownership | Twelve owned SDK initial/cache/lifecycle/musical native controls pass. Same-native-owner witnesses and non-wrapping Source lineage preserve newer raw/gesture/feedback suffixes and refuse external retirement conflicts. The strict zero-sample path consumes either the original validated offset-zero host batch or certified initial COM queues; only the actual successful initial handoff marks their origin, and every subsequent write invalidates it. Mode14 SDK, joined-Worker and Product IPC controls pass; the Product has no authored lanes and produces literal .75 PCM from a component independently restored to .375. Existing mode13 deferred-setter and invalid/missing-handler controls remain passing. | Strict input consumes once through the original processor/context; mixed, foreign, busy, poisoned or nonzero-offset input still refuses. The Product IPC lane has no native call counter; its second no-pending Poll retains the existing replay behavior. No new parameter authority, realtime or whole-heap qualification. Broader Runtime/third-party recovery beyond the fixed MDA control below remains unqualified. |
| Joined VST3 parameter-rescan collector and child acknowledgement | Eight owned joined-Worker controls pass, including native command eligibility and SelectedTerminal entry. An admitted owner collects one coherent exact-4 prefix with its original witness; exact acknowledgement retires it once and preserves newer raw/feedback/input suffixes. Fresh collection refuses multiple or externally retired markers, metadata drift and prior Source failure/feedback/pending input; held-prefix Ack preserves later input suffixes. Product IPC separately passes delivery, rebind, wrong/exact and repeated acknowledgement. Its successful adapter Ack clears only sticky A, without a parent Source installation. None-prefix SelectedTerminal entry preserves old delivery and fresh initial COM input until exact reentered acknowledgement and first PCM; externally retired Some-prefix entry refuses. | TCP controls qualify dispatch; actual VST and separate CLAP held-receipt controls qualify their respective main-role/full-owner contracts. Pending-input negatives use save_state refusal, not DSP-counter auditing. The adapter-only IPC gate and portable uninstalled-Session-ticket hold are distinct from canonical Runtime recovery below. No held CLAP Product/Runtime, global callback suspension, state-entry recovery, broader MDA recovery, realtime, whole-heap or other-platform qualification. |
| Canonical VST3 parameter-rescan handoff | Nine portable controls pass authentic SomeA admission, later-request preservation, exact Ack/conflict, final preflight, peer guards and owned Note-ledger continuity. One real mode13 DLL Runtime passes 48k/stereo/block4: eight literal input-times-.75 frames, Source-first hold without consumption, two natural coherent prefixes, real canonical author transactions and one-shot Source installations, exact Ack and resumed PCM on the same object/session/epoch. | Owned Windows/offline direct-root main-input effect, one media resolve and finite grants. The real gate has no Session ticket or Note; Note IDs and suffixes arriving during Ack are portable-model evidence only. No native-call count, broader third-party rescan recovery, state entry, 100ms graceful-exit, full-heap, realtime or other-platform qualification. |
| Owned zero-input VST3 | MNE6 SDK2/Product2 gates pass: actual native inputs0/null, one stereo Main output, literal PCM/PPQ/full same-PID audits at 44.1/48 kHz. SDK covers Some/None; Product uses Some/direct-root Notes, NoMedia, Source/owner continuity and cold Ready topology refusal. Restore reconciles native bus activation before processing; cold setup preserves pending-input error priority. | Owned Windows/offline fixture; absent input has no auxiliaries. NoNote generators, live topology drift, MDA, GUI, App Playback/Export, full heap, realtime/performance and other platforms remain unqualified or refused. |
| Local VST3 Note Worker | Explicit scalar On/Off request with an existing musical reference requires active Main Event0 and no event outputs; it derives at most sixteen channels and SuccessfulOffBlock completion. Three real Product/owned MNE4 gates pass: 44.1/48 kHz, negative/near-limit anchors, exact PCM/PPQ/tuples, enabled empty batches, cold/parent refusals and terminal poisoning. Conversion storage has one cold capacity/layout admission; current IPC18 requires the separate layout while retaining eight fixed owners. | Local explicit native IDs; canonical qualification is separate below. No true zero-input instrument, third-party Note, realtime/performance, whole Root heap or other-platform qualification. Failed-child termination can precede its audit; terminal tests establish parent non-reuse and same-handle settlement, not failed-tail tuples or natural 100ms exit. |
| Canonical VST3 Note Runtime | Three owned MNE4 gates pass with the installed Source, original owner and existing Note ledger. At 44.1/48 kHz, partitions `[4]` and `[3,2,4]` each process 1025 pairs over 2050 frames; literal PCM and all 2050 native tuples match independent PPQ/ID expectations in complete same-PID records of 513/684 calls. Successful Off blocks recycle the original 1024 pairing slots. Five ordinary controls cover cold None VST-only/mixed-target refusal before media/factory, first entry, retained-owner repeat refusal and multi-target failure precedence. Actual repeat/seek refusal preserves pending Off, Source, lease and cursors; native failure leaves terminal Poisoned non-reuse. | Some musical reference, scalar Main Event0, direct root and real stereo native audio input. Tests use 32 MiB session scratch and an explicit unbounded aggregate grant. State replacement/re-entry/seek, windowed/precompiled/nested Note execution, true zero-input instruments, third-party VST3 Notes, App Playback/Export, realtime/performance and other platforms remain unqualified. First native failure does not promise whole-call PCM rollback or a complete failed-tail audit. |
| Native-probe Factory preparation | Fresh discovered VST3 and normal CLAP probes move their bounded input-name snapshot into the existing Factory; actual create still validates restored native contracts and names in the original same-child Ready scope. Ten new ordinary controls pass, including 0+1 versus 1+1 launches, keyed name equality/drift cleanup, storage/seed ownership, strict Discovery3/normal Probe4 and the 31-port boundary. Both native sidechain negatives require the exact restored-port error from actual create; original PCM/state controls pass. CLAP seed, overflow and retained controls pass. | Manual/generic and CLAP Note-offer specs retain real validation launch plus strict shutdown. Probe is not proof of start/split/Ready; a name change is explicit unsupported presentation drift. Root-grant coverage of cold probes, complete native heaps, realtime safety and timed overhead improvement are not established. |
| CLAP authored constant Tempo | One exact native gate passed four rate × None/Some Runtime cases at 44.1/48 kHz: two epochs, different partitions, literal PCM, Scope-local gain automation, 3/2 source retiming and six-frame PDC. The same actual native objects survived re-entry and their retained OS handles observed Drop exit0. | Owned stereo/offline fixture; 32 MiB local scratch and explicitly unbounded aggregate grant, not a budget-edge test. No beat/bar/meter/playing, tempo map, selected nested musical execution, third-party, realtime/performance or other-platform qualification. The separate 2s Drop observation does not prove the 100ms graceful budget. |
| Candidate VST3 anchored musical audio | The bounded Worker gate now passes all eight 44.1/48 kHz × None/Some × signed-start cases. The canonical gate again passes Scope-local automation, 3/2 source retiming, two three-frame delays, six-frame PDC and two epochs with independent Q/Tempo bits and PCM. Continuous objects and host-serial seek rebuilds pass. | The previous failing temporary Factory validation instance is removed from the fresh discovered VST3 path; this does not establish a 100ms teardown fix. Canonical control uses 32 MiB session scratch and an unbounded aggregate grant. No full callback-count/order, Note/Event PPQ, realtime/performance or other-platform qualification; overall integration remains HOLD. |
| Owned three-output CLAP | Direct scoped DSP observed 38 same-object SDK Process calls across three partitions and two entries. The matching normal IPC18 Product passed 16 canonical Runtime combinations: 44.1/48 kHz, four Main/Aux/fanout routes, two partitions and two entries each; literal PCM, author-state serialization/restore, NoMedia, Source installation and same-object Drop exit0 passed. | Windows mono/offline owned fixture. Aggregate grant is explicitly unbounded; canonical child Process count is unobserved. Live private-DSP state capture, finite aggregate-grant acceptance, third-party multi-output, stereo native workflow, GUI, realtime/performance and other platforms remain unqualified. |
| Surge XT Effects VST3 1.3.4 | Candidate Some and original None gates each pass two restored Runtime observations plus native bypass: Windows/offline/48 kHz/stereo/block64, 4096 finite frames per Runtime, exact prepared reference, Source closure and same-object/Drop exit0. The original state/input and finite root grant (one occurrence, 32 MiB fixed, 16 MiB prepared) are retained. | These are finite PCM/lifecycle observations, without an independent DSP or Tempo-response oracle. The old post-ACK exit failure remains unresolved; observed Drop exit0 does not prove the 100ms graceful budget. No GUI, realtime/performance or other-platform qualification. |
| Official AGain Sample Accurate VST3 3.8.0.0 | The unmodified MIT SDK 3.8.0 reference passes actual List/Probe v4, capture and fresh restore of its 56-byte state. Three canonical Runtime cases pass 4096 frames each: two fresh Gain ID1=0.5 cases match an independent half-gain PCM reference bit for bit; native Bypass ID0=1 matches unity output. Windows/offline/48 kHz/stereo Float32/block64, absent musical reference, sequence-zero Hold constants, the same finite grant/Source closure/native owner and Drop exit0 checks as Surge. Both original Surge gates pass with the same current Product and fresh-probe Factory path. | The reference was built with the SDK's `/fp:fast`; inputs are exact finite dyadics and the oracle includes canonical Graph zero normalization. This is constant-parameter/reference-plugin qualification, not automation ramps, offset Bypass, nondefault private-state capture, GUI, realtime/performance, other-platform or whole-standard qualification. Original exit failures remain open. |
| Ring Out CLAP | The Audio slice normal Product passed four cold Runtime cases covering native bypass and active control, block64 and 31/33 partitions, 10,240 frames including the actual startup transition and steady range. | Windows/offline and the stated state/parameters; not independent algorithm, GUI, realtime or other-platform qualification. |
| Clack SDK Gain reference | The existing unmodified MIT/Apache-2.0 example source, built through a local standalone manifest, passes actual normal Probe4 with embedded processing3, restored four-byte gain state, parameter and port metadata at 48 kHz/stereo/Realtime/block512. | Contract-probe qualification only. The example is not an oracle for sample-accurate automation; its event batches can apply gain repeatedly to the whole buffer. No DSP/PCM, Factory, upstream binary/build-lock, realtime-performance or platform qualification. |
| CLAP port/config metadata | Optional main input and ordered auxiliary inputs/outputs have owned controls. Actual Surge census reports the default auxiliary input and three outputs; the current host does not expose the configuration extension. IPC18, normal Probe4 with required names and embedded processing3, full processing-offer5 and standalone Note probe2 carry exact identity and order, refusing older incompatible workers/offers. The pinned owned parent-query gate executes full and standalone probes for both default and restored state; original public zero-input Note and canonical three-output gates also pass. | Normal Probe4 is a private envelope, not a revision of Note offers or shared processing metadata. Configuration selection and real auxiliary-output sound remain unqualified. The finite Surge Main result below does not imply active port rescan or a new host configuration service. |
| Surge XT 1.3.4 CLAP instrument | Two Windows/offline Runtime cases pass: original None and Some with exact quarter-note zero -19/7 and duration3/4. Each restores 50,208 bytes with NoteOffer5/notes2, NativeEnd and sixteen channels, then processes 16,384 finite Main frames with nonzero energy at48k/stereo/block256, On127/Off8193 and input cursor16384. Each completes one real nonempty rescan through the Track transaction, original Source installation and Preserved Ack, with same-object/cursor continuity, NoMedia0 and same-handle Drop exit0. Some retains its exact reference through compile, install and Ack. Main1, auxiliary2/3 and zero-filled stereo auxiliary input32768 remain prepared. | Fixed finite cases, explicit unbounded aggregate grant. No independent Tempo/algorithm response, cross-instance PCM equality, release envelope, terminal-Off or auxiliary acoustics, GUI, App Playback/Export, project reload, realtime/performance or other-platform qualification. Drop exit0 does not resolve earlier shutdown failures or prove graceful ACK/100ms. |
| Official mda DX10 VST3 3.8.0.0 | One Windows normal-Product/offline48k/stereo/block256 Some-reference Runtime passes64 distinct output windows/16,384 frames, On127/Off8193, 64 real unchanged-value canonical transactions, original Source install/exact Preserved Ack, NoMedia0 and same-object/cursor/Drop exit0 checks. At least one handoff has actual audible PCM before and after its same-request continuation; the test excludes initially silent prefixes from that acoustic witness while still servicing every hold. An independent SDK-only two-fresh-instance comparison observes On0 audible in its first window, whereas On127 is silent for the first256 frames and audible in the next window. Latest Audio Strict has zero diagnostics. | Fixed default-state case with an explicit unbounded Root grant. First-window silence remains a known limitation; no sample-accurate onset, native Note tuple/FFI-count/voice-state audit, terminal-Off acoustics, broader MDA recovery,100ms graceful-exit,realtime/performance or other-platform qualification. This corrects an unsupported test premise, not production DSP. The original unserviced flags4 gate retains its refusal semantics. |
| OpenFX CPU Filter | The independent BitCrush gate and finite native controls cover the stated current-frame, full-frame RGBA Float32/progressive CPU boundary. Three owned descriptor gates now verify separate Source/Output temporal refusal and disconnected optional admission; six existing capability/parameter regressions pass, including normal resident/Graph execution. The readonly-constructor correction passed 456 checks. | Descriptor admission adds no temporal rendering. Other contexts, temporal/tiles/scale/fields, native GPU interop and other platforms remain outside this qualification. |
| OpenFX metadata notices and lifecycle | Windows: 76 distinct ordinary controls and 22 actual owned/official Basic cases passed with the fresh normal Product. Describe returns six ordered UTF-8 notices without Render; the live Catalog delivers four feed notices without replay or author mutation. Exact 32/33 limits, Fatal/invalid-UTF-8/native-failure prefixes, cancellation, single Load/Describe, failed-Describe Unload and live descriptors during Unload passed. | Schema2 metadata requests only; whole-request context is not emitter or Render identity. Per-request32 notices, per-bundle256/1 MiB text+id, one result slot. Log uses the existing tracing path; sink output is not separately qualified. No whole-heap, realtime, third-party, GUI, GPU, other-platform or comparative-performance qualification. |
| OpenFX instance animation state | Windows normal Product: IsAnimating now derives from the existing Constant/CurrentFrame/Curve source before plugin callbacks. The pre-fix CreateInstance query returns0 instead of1; after the fix, all nine same-current-value query/notification/pixel/PID phases pass. The bounded fixture also checks Destroy; its complete resident record ends with Unload return0 and reports no property mismatch. Four unchanged curve/query/lifecycle gates pass exact1/stderr0 with original180s/2s Job settlement. | The new animation gate still fails its final shutdown deadline and remains HOLD. This does not close earlier exit failures or qualify plugin authoring, third-party, GPU, performance or other platforms. |
| OpenFX primary failure and cleanup | Windows: the pre-fix exact shutdown deadline control reproduces cleanup Unavailable replacing the primary. Three post-fix controls preserve DeadlineExceeded, wrong-response detail and admission Canceled under injected OS query/kill refusal; the same real Child is subsequently reaped. Shutdown uses real controlled TCP; admission first prepares the normal Product and official Basic. Two original owner controls and the unchanged Product instance-reuse/unfetched-output regression also pass. All six exact gates have stderr0 and original180s/2s Job settlement, independently reviewed. | Controlled shutdown exchanges do not qualify native OFX Ready/DSP. Output-allocation, inner-render and GUI policy changes have source review and related regression, without new per-site fault injection. Original fatal exceptions and Retiring/sole-Child semantics remain. App builds pass; strict Clippy fails on ten unchanged diagnostic warnings. No old exit-failure closure, final-Drop bound, cold-construction custody, whole-shutdown100ms, realtime, third-party or other-platform qualification. |
| OpenFX exit observation | Prior child-owner and official Basic controls retain their finite qualification. The current correction passes five deterministic production-helper clock/query controls: pre-expired, query crossing the deadline, timely zero/nonzero exit, pending and I/O error. The fresh normal Product's first readonly native gate fails at shutdown; the remaining seven gates were not run. One separate default-off schema2 diagnostic passes all three readonly sessions, with inner return at 3.5–4.3ms and parent OS-exit observation at 66.7–75.8ms relative to ParentBegin. | The readonly failure and all eleven earlier failures remain open; the diagnostic does not reproduce or explain them. The observed tail does not identify DLL/CRT/OS/scheduling cost or prove natural exit. Original budgets and assertions remain unchanged; final reap has no separate bound. No whole-shutdown 100ms, realtime, comparative-performance, third-party or other-platform qualification. |
| Airwindows metadata | Actual inactive parameter index 6 has current zero and a negative default despite an advertised 0..1 range. It occurs before state restore; the host correctly rejects it. | No clamp or plugin-name exception. Upstream inactive-default initialization is supporting evidence; the actual native writer/source-build binding remains unproven. |

The current local environment uses Windows x64, FFmpeg 8.1.2 and static OCIO
2.5.2. No macOS/Linux runtime or performance result is implied by these checks.
Normal Product retains ten existing warnings; App test typechecking reports
ten library and twenty-five test warnings in unchanged areas. Zero strict Audio
diagnostics is a separate result. OpenFX qualification is stated separately above.

Current IPC18 retains bounded JSON startup metadata followed by the exact opaque adapter
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

The shared author event carries `NoValueRefresh`, `Values` or
`ParameterSetAndValues`; CLAP retains its original raw flags and query facts in
the adapter. That typed migration introduced IPC17; the matching IPC18 regression
above also preserves `RequestOnly` versus the original `Cohort`, and distinguishes
a delivered prefix from a newer live pending cohort. Carrier matching alone does
not authorize raw retirement or Runtime recovery; the collector and child
acknowledgement have their separate finite qualification above.

## VST3 event contracts and Note execution

Required Discovery4/payload3 carries the expected event-bus contract and main-audio shape
through the existing probe, registration and payload owners. The actual restored,
configured, stopped instance is rechecked during preparation, cold rebuilding and
retained configuration. Canonical direct-root VST3 Note now requires the existing
explicit musical reference; None and unsupported entry scopes refuse before
media or native Factory resolution.

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
observe their own seven roles separately. Current IPC18 requires eight fixed owners and
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
The anchored musical SDK slice now passes 342 ordinary controls and sixteen
owned DLL gates. Its validated exact reference projects absolute signed samples
to quarter notes and constant Tempo, with nearest-even binary64 rounding across
native chunks. Independent mode7 DSP controls cover 44.1/48 kHz, local/scoped
roles, None/Some and negative/near-limit positions, including same-object
processing counts and queued-parameter refusal/recovery. Explicit checked Note
batches now project Event PPQ from the same immutable reference, original call
start and event offset, after whole-batch validation. Queued/staged legacy events
and busy/poisoned ingress still refuse before consumption. A real reference-counted
COM list, created during cold preparation even for None, temporarily owns the
original chunk vector backing. Only the native callback exposes its read-only
window; exposure closes before the original inputEvents pointer is restored.
Nonblocking query/cleanup faults prevent later Some and None processing; late
legacy ingress remains in its original owner. Native failure retains priority.
Only the current failed chunk avoids output copy: earlier successful chunks or
an outer cleanup failure can leave a written prefix, without event replay.
New Main-only MNE4 controls verify ten complete actual-PID records against
independent PPQ bits and PCM at both rates, scoped/local roles and signed anchors.
MNE5 tests actual retained COM references, closed views, cold release and its
44-byte component-state round trip without restoring observation counters.
The SDK storage observer reports the complete process Box, concrete COM payload
and unique vector capacity separately. This does not qualify the complete Arc
allocation or nested native owners against the Audio Root grant.
The candidate no-Note VST3 adapter now consumes the same Core reference without
a second clock or state source. Exact preparation/Ready/seed and original native
metadata/rebuild checks remain; selected nested musical execution stays refused.
Canonical VST3 Note has the finite Runtime qualification above. Its initial-entry
guard reuses the existing ledger origin and continuity state before native polls
or Source revocation; a failed target takes precedence over recoverable re-entry
refusals. The owned pure-audio canonical and real Surge controls above pass,
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
It does not roll back an executed prefix. Local Product adaptation now lowers
the existing Note IR through that checked SDK with one admitted conversion Vec.
Its typed zero-capacity layout preserves element alignment; CLAP without this
owner uses its separate neutral layout. The complete Worker header is measured
independently, including when Note input is disabled. Canonical Runtime admission
and early refusal of unsupported re-entry pass the finite owned controls above.
Third-party
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
- The preceding IPC15 consumer run's public CLAP Note gate passes the first two
  44.1 kHz Track-rack partitions, then fails Factory validation for `[1;14]`:
  ACK at 0.325ms, still running at 108.703ms, and late Exit0 after kill/reap.
  It has no child-phase observation or evidence attributing the delay to the
  SDK change. Later partitions and all 48 kHz cases remain untested in this run.
- CLAP native multi-output and the real Surge Main Runtime have the finite
  offline qualification above. Broader instruments, auxiliary sound, product
  output selection and realtime remain open.
- OFX same-handle exit-time observation passed the finite Windows controls and
  official Basic sessions above. The original 100ms policy and sole Child owner
  remain unchanged. The disconnected-optional-clip gate now passes all six
  format/image checks, same-resident bit-exact RGBA half gain and unchanged
  Source, then fails that original shutdown deadline despite native Unload0.
  Five separate temporal-admission/CPU/PAR resident-and-Graph regressions pass.
  This is finite correctness evidence, not a passing lifecycle gate or an
  identified exit root cause. One existing schema2 diagnostic with the newer
  matched Product also fails: native Drop, pump join and inner return finish by
  39.01ms, while OS exit remains unobserved at the 100.47ms deadline. This narrows
  that sample to the tail after inner return; it identifies neither main/CRT/TLS
  nor the old build's failed phase. The original unobserved failure is retained.
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
