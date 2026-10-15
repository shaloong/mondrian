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
- [CLAP 1.2.10](https://github.com/free-audio/clap/tree/1.2.10), additionally checked
  on 2026-10-09 at immutable commit `195b42a004144fab0b3cf95e9c067187d15365b7`.
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
| Audio layout | Mono/stereo main in/out, up to 31 auxiliary inputs of the admitted layout; explicit absent CLAP main input, including ordered auxiliary-only inputs; finite owned mono/offline Note execution; restored-state native port metadata and causal PDC | Mixed layouts, surround/ambisonic, multiple outputs, third-party instrument processing and zero-input stereo DSP |
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

Verified on Windows with owned PCM-input fixture `B239C594...`: canonical authored
two-port scores at 44.1/48 kHz match a literal per-sample oracle across whole,
partitioned and single-frame calls, including release and actual parameter-output
handoffs. The second native case checks uninstalled/invalid entry, zero-frame and
noncontiguous refusal, changed-score refusal and subsequent original Off/release.
Both native cases pass with the final normal Product and Audio executables from
the same source freeze `E3421EAE...` (1949 inputs), images `90136DFB...` and
`AD25DED0...`. Native receipt `8EBAE84F...` retains exact pass sets, exit zero,
no deadline or outliving descendants, and successful reaping. The ordinary Audio
suite passes 557 cases, including seven new preparation controls and all previous
12 pre-clone cases; inventory is 650 total/93 ignored, with the two new native
cases executed separately. Five-crate strict Clippy has zero diagnostics; normal
App check retains the same ten OpenFX warnings. Evidence is under
`E:/codex-artifact-storage/mondrian-3349-plugin-host-sprint/develop-alignment-evidence-r1/public-note-canonical-*`.
This qualifies the finite offline public Runtime path, not App Playback/Export,
zero-input instruments, arbitrary third-party plugins, realtime performance or
other platforms. Gate elapsed times are not DSP performance measurements.

App PCM preparation carries the prepared root score's required origin through
Media's existing continuity model. Explicit fresh entries are checked before
output deactivation and Source/PCM retirement. Pause, end-of-program and denied
recovery suspend PCM without admitting Enter, retaining installed Source/control
ownership and actual callback retirement obligations. A late Preserved Ack cannot
resume suspended PCM. Author State Entry intent updates and nonzero Continue
windows retain their semantics; ordinary stateful plans gain no fixed-origin
restriction. Notes still require entry at zero. Arbitrary seek replay and complete
App Note Playback/Export qualification remain outside this evidence.

Windows host-model regression: Audio 558, Media 582 and App 2874 ordinary tests
pass; focused controls are subsets. Final App/Product share freeze `0B79CDF1...`
(1949 inputs). Audio/Media and strict results retain their unchanged dependency
scope across App-only test-fixture corrections. This adds no new third-party or
realtime qualification.

### Authored content extent and frame ranges, 2026-10-16

`Sequence::total_duration` now takes the maximum end of every Clip and authored
Note, including unordered lanes and legal overlapping Clip placements. Mute,
bypass, plugin availability and tail do not redefine authored duration. App and
Preview share the checked last-occupied-frame projection, `ceil(end)-1`, clamped
to zero. Export retains its exclusive-end ceiling and explicit Out/WorkArea
rules. No project field, cache, playback clock or second duration authority is
introduced. Nested Export controls qualify selected visual ranges and finite
Transition handles; they do not claim rendered transparent pixels or audio DSP.

On source `416EACDE...` (1959 inputs), all 4742 ordinary tests across Core,
Timeline, Audio, Editor State, Project, App and Export pass, including six new
controls. The fresh normal Product `11D54CCD...` and Audio test executable share
that full source. Three existing public Note native tests also pass; the
zero-input case retains six fresh 14-frame Runtime oracles at 44.1/48 kHz across
three partitions, no media resolution/window, actual Source handoffs and owned
process exit. These use the original explicit unbounded root grant. They do
not qualify physical allocation bounds, App terminal Note Off/device consumption,
third-party instruments, realtime performance or other platforms. Evidence is
under `note-content-extent-ordinary-r1/` and `note-content-extent-native-r1/` in
the existing validation directory; this is not a fresh full-workspace result.
Core Strict passes on that source. App/Export Strict passes on `F1620A35...`,
whose only additional source change is an equivalent alignment predicate in
the existing OFX close observer; the executable evidence above remains `416EACDE...`.

This table describes source capabilities. Each qualification below remains tied
to its exact source revision, binary hashes, environment and named tests.
The complete [audio feature matrix](native-audio-standard-features.md) and
[VST3 interface inventory](vst3-interface-inventory.md) distinguish actual product
consumers, vendor primitives, unavailable negotiation and remaining work.

## Required qualification evidence

For each feature: normative contract and negotiation; independent owned oracle;
official reference or validator where applicable; permitted independent vendor
coverage; author Undo/Redo and save/reopen; interruption/error/teardown; supported
platform/device/native UI; measured execution cost. Audio checks use per-sample
references across nonzero starts, partitions and seeks. Image checks include
HDR/negative/alpha, PAR, canonical/pixel coordinate seams and actual encoded output.

Audio performance measures the complete native worker callback after excluded
warmup, at declared rates/layouts/buffer sizes, with p50/p95/p99/max and deadline
misses. The available callback deadline is derived from frames/sample-rate;
crash timeouts are not audio performance budgets. Native root preflight
request-query IPC is included whenever it is part of the production callback;
process-only measurements cannot exempt that round trip or certify the full
root path. Video measures actual resident
worker frame execution against a declared frame budget and image correctness.
Report build profile, CPU/OS, source and fixture hashes, iterations, setup cost,
tail/overload/cancellation and memory. No universal zero-overhead or compatibility
claim follows from a fixture, cache counter or a single machine.

Correctness qualification selects the product and test executables from the same
Cargo test-profile build before starting native gates. The actual product, lib,
integration and plugin binaries are hashed and held under read-sharing file
leases; no Cargo build runs between pinning and direct gate execution. An
immutable content-addressed helper copy is also usable when that harness supports
an explicit helper override. The initial receipt records source and binary
hashes, and completion rechecks them. An unleased shared
`target/debug/mondrian.exe` can be overwritten by a later Cargo selection and is
not a frozen qualification artifact. Test-profile correctness and release
performance remain separate evidence.

## Windows VST3 declared optional consumers (2026-10-15)

One immutable host declaration now gates MIDI/unit/program cache construction,
refresh and consumption, direct unit/program streams and all three generic
note-expression sends. Undeclared consumers return a typed local refusal; core
tracked NoteOff remains available. Standalone defaults are preserved. Local
checked MIDI mapping includes Program Change 130; the legacy remote backend
explicitly refuses 130 without changing its IPC.

Receipt `vst3-cache-policy-actual-r1/gates/receipt.json` records 275 vendor and
558 Audio ordinary passes, three new native controls and seven existing SDK
initialization/Source controls. All six groups have clean owned exits. Final
builds share 1953-input freeze `F4E3FB2A...`; strict checks have zero diagnostics
and normal Product `F0E19F73...` retains the ten-warning baseline and FFmpeg8 ABI.
The controls count optional calls after replacing SDK interfaces; initial
constructor/load query gating is source-reviewed, not instrumented by them.
This qualifies finite owned Windows cache/consumer behavior. At that source,
Source/feedback and restart flags 16/20 remained unconsumed; Surge PCM, other
platforms and realtime performance were open. The test alias build failure and a disqualified stale
Cargo Product are retained; the gates bind the rebuilt Product, while these
SDK controls execute inside the libtests without launching that Product.
That build also contains the separately held OpenFX constructor candidate;
this audio receipt does not accept that change.

## Windows VST3 admitted initialization input handoff (2026-10-15)

The first legal same-object Bind or Enter now refreshes the initial parameter
information/value cohort through the existing Source and input queue. Its bounded
baseline storage request follows the original Session/Root grant. All fallible
checks precede the Source CAS; successful Bind closes the initialization window,
including zero-work Bind. Later callbacks retain runtime author semantics.
Actual input points and pending values protect destructive native operations.
The [lane contract](../architecture/audio-native-lanes.md) records memory coverage
and the remaining dynamic allocation frontier; this is not realtime qualification.

Final freeze `1342129F...` contains 1958 inputs. SDK receipt `C484098E...` under
`vst3-initial-handoff-sdk-actual-r3/gates/receipt.json` has 560 Audio, seven resource
and 14 actual SDK control passes, including unchanged refusal assertions and real
zero-sample processing. Vendor280 is retained from the earlier exact passed set:
its original producer and fixture are reused only after proving the five changed
files are Audio-only and outside those packages' dependencies. Strict checks have
zero diagnostics. Fresh normal Product `B40F3173...` rebuilds all four affected
production targets, uses FFmpeg8 ABI and retains the ten-warning baseline.

The original Surge test now passes in `vst3-initial-handoff-surge-actual-r2`
(gate `C04EEB52...`): two fresh restored instances and one native-bypass instance
each complete 4096 frames, 64 calls and Enter(73) on the same observed object.
Restored outputs match each other bitwise; bypass matches the input bitwise.
All three observed children exit zero after Drop. This is direct Factory
execution with silent auxiliary input, without an independent active-effect DSP
oracle or a graceful-shutdown Ack claim. Nondefault state, active sidechain,
App Playback/Export, GUI, performance and other platforms remain unverified.
The original flags20 failure and the later adapter-only uppercase-case-hash
failure remain retained. No assertion or native deadline was relaxed.

The unchanged two public CLAP Note controls also pass on this exact Audio/Product
pair (`public-note-wire10-native-r1`, gate `C100E86D...`), preserving their literal
PCM, refusal, clock and original-NoteOff checks with fixture `B239C594...`.
All current accepted groups have clean owned exits and unchanged inputs. This
freeze includes a separately tested, still-failing Ring Out consumer candidate
and the held OpenFX constructor candidate; neither is accepted by this slice.

## Windows CLAP true zero-input Note execution (2026-10-16)

CLAP processing now carries explicit native main-input presence through the
processing probe, registration, worker payload and restored-state revalidation.
An absent main input passes zero native audio buffers; the existing Graph output
extent drives processing without a dummy Clip or media source. Inactive shape
changes are refused before reactivation. Port count/get are never queried while
active. The owned restart control first failed with four such queries on parent
`2880647c`, then passed after the lifecycle correction.

Current source `E2D61E32...` binds 1959 build inputs, including Clippy configuration.
Audio and the five-crate strict gate pass; the exact inventory is 673 tests,
104 ignored and 569 ordinary passes. Fresh normal Product `BF219085...` retains
the ten-warning baseline and FFmpeg8.1.2 ABI. No Product source bridge is used.

`clap-zero-input-native-r2/gates/receipt.json` (`1DC24E43...`) records four native
passes: Ring Out's existing four cold Runtime scenarios, the original public
Note positive/refusal controls with DLL `B239C594...`, and the new zero-input
control with separately built DLL `582ADF58...`. The original positive test's
complete 12693-byte source prefix is preserved. All four owners exit zero,
reap descendants and retain unchanged inputs without deadline/cleanup errors.

The new owned fixture qualifies mono/offline Note execution at 44.1/48kHz across
three public render-request partitions: six fresh Runtime objects. Each passes
the independent 14-sample literal PCM oracle, two native Note ports, canonical
save/reopen and real Source handoff/Ack for four parameter callbacks. There are
no media Clips, resolver calls or media-window bytes. Authenticated object facts
remain stable through cold installation, Enter(0), PCM and handoffs; retained
process handles confirm actual Drop exit zero. Fixture ABI controls retain the
original seven tests plus three zero-input controls, passing twice with ten
exact tests per round. The original explicit unbounded Root grant is preserved;
native process callback counts and total physical memory are not instrumented.

Retained failures include the initial five test-DTO migration failures, the
fixture's test-only pointer-initialization lint, and a data adapter's refusal of
one frozen Windows export-library linker message. The native gate above passed;
its adapter subsequently rejected the zero-input test's split `--nocapture`
named-pass line. Closed output evidence is reviewed without rerunning native
tests; that original adapter remains failed. No plugin assertion or native
deadline was relaxed.

Third-party instruments, zero-input stereo DSP, dynamic port rescans, App
note-only playback/export, GUI, realtime/performance and other platforms remain
unverified or separately unsupported. The held OpenFX change is outside this
acceptance.

### Real Surge XT instrument admission, 2026-10-16

The separate instrument test preserves the complete existing FX test and uses
actual List results before noncapture Probe, default-state capture and fresh
restore. Source `BA8C43BD...` has 1959 inputs; fresh Audio and strict Clippy pass,
with 674 tests, 105 ignored and the same 569 ordinary passes. Only a `cfg(test)`
Windows child was appended; the accepted normal Product `BF219085...` is reused
through that exact byte bridge, without claiming whole-source equality.

Windows DLL `6FB598B3...` actually lists Surge XT 1.3.4, ID
`org.surge-synth-team.surge-xt`. The first noncapture Probe fails with the
original audio-port identity/role/layout refusal. The retained native gate
`clap-instrument-probe-actual-r1/gates/receipt.json` (`D94AB202...`) stays failed:
test exit 101, no deadline, owner errors or outliving descendants, and all
descendants reaped. No state capture, restore, PCM or Note processing occurred.
The failed gate and bounded failure artifact qualify a reproducible admission
gap; they do not establish the plugin's actual port topology or a plugin defect.
The next step is to identify the failing native port fact before changing the
mapping contract or any capacity limit.

The bounded same-call refusal follow-up uses source `320D3DD4...` and a fresh
normal Product `2725E9B6...`; Audio, strict Clippy and all 572 ordinary tests
pass (677 total, 105 ignored). The existing error transport retains only fixed
ASCII facts, without native names, custom type strings or extra port queries.
Its owned ABI controls verify the four refusal reasons, original short-circuit
order, query stopping, null/empty normalization and the existing 1024-byte limit.

The real follow-up gate `A766F4B5...` remains failed. Its first input row has
count 1, index 0, ID 32768, known flag bits zero, main false, two channels and
type stereo. The host's index-zero-implies-main policy rejects that row;
this is not evidence of a plugin violation. Output count and the remaining
topology have not been queried. Supporting a native input without a main role
requires an explicit main/auxiliary mapping and preserved native order, rather
than ignoring the flag. State, DSP and instrument compatibility remain open.

The separate default-deactivated census now records the complete bounded
topology on that same DLL: one non-main stereo input (ID 32768), then three
stereo outputs in native order, with only output zero (ID 1) marked main;
outputs one and two have IDs 2 and 3. The input and main output advertise
reciprocal in-place pairs. This adds a second admission gap: the current host
also requires exactly one native output. Fixing input-role inference alone
cannot qualify this instrument.

Source `A005CB9C...` adds only two Windows test-only appendices. Fresh Audio,
strict Clippy, all 572 ordinary tests and the new census pass; inventory is
678 total, 106 ignored. Original Job gate `D67C45BD...` and adapter `57CD038B...`
retain unchanged inputs, exit zero, no deadline/errors/outliving and reaped
descendants. Normal Product `2725E9B6...` serves only List through the exact
two-file prefix bridge (1957 other inputs unchanged); it was not rebuilt.
The fresh census stays deactivated in the Audio test process under the original
180-second outer Job. Instance and library Rust Drop return before completion;
this does not independently count native callbacks or observe OS module unload.
No internal five-second processing deadline, state restore, activation, DSP,
Note, realtime, performance or cross-platform compatibility is qualified.
Evidence: `clap-topology-census-actual-r1/` in the existing validation directory.

### CLAP optional Main input, 2026-10-16

Main input presence now follows `IS_MAIN` at native index zero. Non-main inputs,
including index zero, retain native order and stable ID-based auxiliary keys.
The existing offset and buffer mapping serve both shapes; graph working PCM
does not become an undeclared native input. Admission still requires at most
31 auxiliary inputs and exactly one main output. Inactive restored-state,
render-mode and configuration metadata checks retain their existing contract.

On source `AE956862...` (1959 inputs), all 575 ordinary Audio tests pass,
including three new owned Clack ABI controls for mono/stereo PCM mapping,
disconnected silence, missing PCM and changed/oversized/multi-output refusals.
The same-source five-core Strict check passes with zero diagnostics. The frozen
old-validator control is source evidence, not a run of an old Product binary.
Fresh normal Product `3433BC99...` and Audio `7B1E22D6...` share that source.
Three existing public Note native regressions pass, including six fresh
zero-input Runtime cases at 44.1/48 kHz with literal PCM, Source handoffs and
owned process exit. They retain the original explicit unbounded root grant;
physical allocation bounds and App terminal Note Off remain unqualified.
These regress the existing Note families; auxiliary-only processing has the
inline ABI evidence above. Native evidence: `clap-optional-main-native-r1/`.
Surge XT's observed default three-output topology remains refused; optional
port configuration, third-party processing and realtime performance are not
qualified by these controls. Evidence: `clap-optional-main-ordinary-r1/`.

## Windows Surge Effects through canonical Runtime (2026-10-16)

The public non-RT owner observation returns the actual PID/load facts from the
existing authenticated validation query. Current Source and opaque owner lease
are checked before and after that single native call. Observation neither forces
parameter presentation nor consumes author work; later operations re-admit the
lease. The old unit-result validation API retains the same contract.

`surge-runtime-actual-r2/gates/receipt.json` (`F40E91EB...`) records four exact
native passes on current 1958-input freeze `D48A85B2...`, Audio `61763698...` and
fresh normal Product `BBFD9405...`. This is full source equality, without a
test-only Product bridge. The unchanged vendor dependency is separately bound to
its actual Cargo producer and the exact seven Audio-only source changes.

Surge XT Effects 1.3.4 VST3 follows original 1150-byte state, canonical author
save/reopen, Sequence/Output-rack compilation, finite resource grant, real Source
installation/Bind and Enter(0). Three fresh Runtime instances each complete 4096
frames in 64 render requests at 48kHz/stereo/offline. Two restored outputs match
bitwise (`BE3C9C7A...`); native bypass matches the unity Graph reference
(`9D95B6A2...`); the active control differs in 8176 samples. These counts are
observations, without an independent active-effect or bypass-precision oracle.
All 66 presentation polls per run legally return no parameter batch. The actual
authenticated owner remains unchanged through PCM and each observed child exits
zero after Drop. An unconnected declared auxiliary input uses existing Graph
zero-fill. Reported resource fees cover the existing host model, not total heaps.

The unchanged Ring Out selector also passes all four cold-handoff/partition
scenarios; both original public CLAP Note positive/refusal selectors pass with
owned fixture `B239C594...`. All four gate owners exit zero, reap descendants and
retain unchanged inputs without deadlines or cleanup errors. Fifty case evidence
files are retained. Current ordinary regression has 562 exact passes; strict
checks have zero diagnostics, and Product retains the ten-warning baseline and
FFmpeg8.1.2 ABI. The first adapter's pre-launch JSON object-key-order refusal is
retained; its correction compares the same seven tool paths/hashes structurally.

This qualifies these finite Windows offline Runtime and owned Note regressions.
Nondefault presets, active sidechain, private DSP state, GUI, App Playback/Export,
realtime, performance and other platforms remain unverified. The held OpenFX
constructor candidate is outside this acceptance.

## Windows Surge XT Effects CLAP metadata comparison (2026-10-15)

The real `org.surge-synth-team.surge-xt-fx` module `30845FF7...` completed two
ProcessOwner-backed Probe calls: default capture, then a fresh process loading
the original 972 bytes (`9BFB00CB...`). Both calls returned valid typed metadata
with 13 parameter IDs, zero latency, 96000 tail frames and one stereo auxiliary
input. Twelve names changed from `FX Parameter n` to `- -`; the strict declaration
comparison therefore **failed**. Its original assertion and failed receipt
`22D9A14E...` remain under `clap-surge-probe-actual-r1/gates/receipt.json`.
CLAP's [parameter rescan contract](../../vendor/clap-sys/official/include/clap/ext/params.h)
allows name changes. This strong comparison is a diagnostic expectation, not a
standards-conformance oracle. The production structural matcher already excludes
display names; this failure does not demonstrate a worker-prepare defect.
Capture and restored-state metadata come from different lifecycle stages.

The same-source Audio build and strict checks have zero diagnostics; 558 ordinary
tests pass. The failed native test exits 101 with clean owned reaping and no
deadline or outliving descendants. Test source `53436F80...` has 1954 inputs;
Product `F0E19F73...` retains its 1953-input `F4E3FB2A...` provenance through an
exact test-only parent/child bridge. No successful comparison result was written.
PCM, nondefault state, Project reload and processing compatibility are unverified.
The separately held OpenFX constructor candidate is not accepted by this test.

## Windows Ring Out CLAP Runtime startup gate (2026-10-15)

The real `com.duskaudio.ring-out` module `DF86E10F...` reaches canonical
Sequence/Output-rack compilation, native owner installation and fresh loading
of its original 184-byte state. The new ignored gate intends to render 6144
transition frames and 4096 steady frames, with two partitions and separate
native-bypass/active controls. Its first run instead **fails before PCM** at
Runtime Enter: the installed cold object's Source reports `PendingCallbacks`.
The worker Enter, parameter delivery, PCM comparisons, remaining three runs
and positive Drop assertions are not reached. This is a diagnostic consumer,
not verified processing compatibility or an attributed plugin/host defect.
That version polls after Enter; its next slice must first use the existing
cold installed-owner author poll, canonical transaction, Plan handoff and Ack.
It must preserve the Source rather than clear flags to admit processing.

`clap-ringout-pcm-actual-r1/gates/receipt.json` retains the exact failed selector
and 558 ordinary passes (inventory 657/99). The failed owner exits 101, with
no deadline, outliving descendants or cleanup errors; descendants are reaped.
Audio and five-crate strict builds have zero diagnostics and share 1955-input
freeze `6E11ED58...`. Product `F0E19F73...` is bound through the exact two-child
test-only bridge; its held OpenFX constructor candidate is not accepted here.
Both preliminary compiler failures remain recorded.

The bound raw PCM `0C54BF84...` contains eight negative zeros. Unity Graph
accumulation legally changes their signs; the separate reference `9D95B6A2...`
retains all other bits and still requires every steady sample bit to match.
No tolerance, skipped zeros, original input change or relaxed old generic gate
was introduced. No PCM result or successful run receipt was produced.

The subsequent cold consumer now follows the existing public poll, canonical
author transaction/compile, original resource grant and Source install, prepared
handoff Drop and actual Ack before Enter. One t=0 Hold key preserves the user's
GainOut/Bypass intent across the startup rescan. The first cold attempt completed
that handoff but failed before PCM by requiring another complete parameter
snapshot. Presentation polls are incremental: unchanged revision after Ack may
return `None`. The test now retains the actual cold authenticated PID/serial;
later present batches must still match it. Cold values are explicitly not DSP
target-delivery evidence. Both earlier failed cases remain retained.

`clap-ringout-cold-handoff-actual-r2/gates/receipt.json` (`7686ED5C...`) records
560 exact ordinary passes followed by the unchanged native selector passing.
All four fresh Runtime/Output-rack runs complete 10240 frames: 6144 transition
frames are actually processed, then all 8192 steady sample bit patterns are
compared against the independent unity Graph reference. Native bypass has zero
differing samples for Runtime request partitions 64 and 31/33; active controls
distinguish passthrough. Native callback block sizes are not instrumented.
Every run completes one actual cold proposal handoff, preserves keyed intent
and the installed owner, and observes exit0 after Drop. Its PID originates in
the cold authenticated batch; all four post-Enter parameter-batch counts are
zero. This does not fabricate a missing full snapshot or delivered-value receipt.

New Audio `CE2A6034...` and strict checks bind 1958-input freeze `A5DD12C3...`.
Product `B40F3173...` retains its original `1342129F...` producer through the
exact single-test-child bridge; the other 1957 inputs and actual cfg(test) parent
chain are unchanged. Both gate owners exit0 without deadline, outliving
descendants or cleanup errors; 37 evidence files are retained. This qualifies
this finite Windows offline Runtime bypass/partition/state-handoff scenario.
Transition and active-effect algorithms, private DSP state, nondefault presets,
GUI, App Playback/Export, realtime, performance and other platforms remain
unverified. The separately held OpenFX candidate remains outside this acceptance.

## Windows Surge XT Effects default-state observation (2026-10-15)

Surge XT Effects 1.3.4 VST3 (`3B9F6C9B...`, class
`ABCDEF019182FAEB566D624153465854`) completed default-state capture and loading
of the exact 1150 captured bytes in a second fresh Product process. At 48 kHz,
stereo, offline mode and maximum 64 frames, both probes reported the same 14
parameter descriptors/current values, zero latency, 96000 tail frames and one
auxiliary bus. All six recorded metadata groups matched exactly.

Receipt `21DF420D...` under `develop-alignment-evidence-r1/surge-fx-vst3-capture-restore-r1`
binds source `A4483DF2...`, normal Product `99014BC0...` and state `E24081FD...`.
Both owned processes exited zero without a deadline or surviving descendants;
inputs remained unchanged. This qualifies the bounded capture/load observation.
DSP output, nondefault presets, Project reload, GUI, performance and other
platforms remain unverified for this plugin.

The later PCM/lifecycle gate `2ED65E20...` fails at its first `Enter(73)` with
`ReprepareRequired(Vst3Restart { flags: 20 })`, before any PCM or explicit Drop
oracle. Source `83E20443...`/Audio `554A3A5A...` uses retained Product `8E852B04...`
through an exact test-only source bridge. At that source, the initial consumer
handled only pure value invalidation; parameter-info invalidation was unsupported. The combined
notification is valid and requires parameter-info/value refresh under the
[VST3 contract](https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical%2BDocumentation/Parameters%2BAutomation/Index.html).
Ordinary Audio 558 passes independently; the failed native owner reports exit
101, complete reaping and no timeout or surviving descendants. This is retained
failure evidence, not DSP or Drop qualification.

A subsequent direct-worker startup diagnostic (`02F9128E...`, source
`C60220FC...`) observes the same native object across load, state restore, mode
selection, validation and processing startup. All 14 complete `ParameterInfo`
records and current values remain identical. Flags20 is already pending after
load at serial2; restore advances it to serial3. Every phase retains source
revision10, ten pending events and ten controller-to-DSP feedback values, with
no open edits. Thus metadata equality alone cannot authorize entry: initialization
callbacks and derived-cache obligations also need explicit lifecycle handling.
The diagnostic does not invoke Enter, Process, restart service or a controller
setter; the existing factory retains its own behavior. Its SDK controls and the
original five initial-value controls pass alongside unchanged ordinary Audio558.
This is same-object observation, not Product-front-door or DSP qualification;
the original failed PCM gate remains unresolved.

The follow-up paired-source diagnostic (`4332926C...`, source `7B2F332E...`)
preserves ten ordered Value callbacks whose admitted IDs and exact value bits
match both the complete feedback queue and the controller in every phase.
Unknown IDs and stale values are therefore excluded for this captured default
state only. Three handler controls and an additional SDK pairing control cover
capacity, overflow, reentrancy and stale/unknown observations without consuming
either source. These observations do not authorize initialization retirement:
the pending controller-to-DSP delivery and first-block author-value priority
still require an explicit lifecycle contract.

### Windows OpenFX close observations, 2026-10-15

An opt-in fixed QPC mapping now distinguishes native destruction, complete
Closed packet writes, pump join, resident-inner return and parent OS-exit
observation. It retains the original single 100ms retirement deadline and
owner/cleanup contracts; normal execution leaves observation disabled.

Source `0FC44D25...` (1,952 inputs) produced fresh App tests, integration tests
and normal Product `D74F73FC...`. Four new controls, 46 existing resident tests
with observation disabled, and one original canonical-curves SDK-fixture case
pass. All three observed sessions commit all 16 markers and settle cleanly.
This working baseline still includes the unintegrated readonly-constructor
candidate (`3FE9513B...`); it is not a runtime comparison against the pre-fix SDK.
Native destruction takes 0.7507–0.9742ms, pump join 1.5855–1.6340ms, and the
resident-inner-return to parent OS-exit observation interval is 59.519–69.2271ms;
total observed close duration is 63.6449–73.3416ms. Decode `9E6EBDE0...` retains
the original owner receipts and raw maps under `ofx-close-observation-actual-r1`.

These successful observations do not reproduce or retrospectively locate the
original `D30C487F...` deadline failure, identify a particular OS/CRT/DLL cause,
or qualify third-party compatibility or performance. The readonly-constructor
integration hold and twelve remaining repeat gates stay open.

## Current execution receipts

The full pre-change workspace baseline is documented in
[modal-resource-lut-acceptance.md](modal-resource-lut-acceptance.md).
New plugin and control changes need fresh named qualification and are not covered
by the earlier complete Golden or native Scope receipts.

The new SDK ABI slice has executed independent official C++ layout/signature
checks and all nine C++ calls into real Rust COM interfaces on Windows x64,
including reference lifetime and interface identity. This establishes that slice's
ABI behavior, not implementation of the corresponding optional host services.

The complete-line geometry and bounded local history stage passed all 57 UI
text tests, 244 text-input tests, 32 numeric tests, one cold-owner admission test,
14 actual Host numeric draft/clipboard/history tests and the modal release test.
The Host test includes twenty real 512 KiB Pastes: eviction or exhausting local
Undo does not invoke project Undo, and confirming a fresh numeric edit releases
local ownership. Sources and logs were hashed before/after each frozen gate.

The subsequent complete selection stage reproduced three actual pixel failures
for partial RTL selection, disjoint BiDi spans and IME origin. After repair, all
248 text-input tests, three new renderer contracts and the cold-owner selection
admission test passed. These include reverse-anchor Undo restoration, Arabic
partial clusters and a long contiguous selection without per-glyph rectangles.
At this earlier selection checkpoint, visual BiDi arrow traversal and native/physical IME acceptance remained open.
Strict workspace lint and fresh native product gates are separate requirements.

The frozen foundation native run completed on 2026-10-09 (Asia/Shanghai):
**21 gates, 46 tests, zero failures**. It covers real CLAP/VST3 buses/state/modes/
signed transport; OpenFX bootstrap/API admission/PAR/CPU/limited alpha and the
official Basic filter; five supervised resource workflows; ten native sidechain/
Scope Undo/save-reopen workflows; and eleven portable LUT/media/OCIO output cases.
The test-profile helper SHA-256 is
`BA21F174FC0B7976DCAC25DFB48C0E8E6457FCE64702A094EE133F533A3D4000`.
The receipt SHA-256 is
`FD02B0662CBCDF353067AB870DEE5D9497BE99683BCCEB3EE3C80034A005B74A`
(`.scratch/plugin-standard-audit/foundation-native-pinned-receipt.json`).
All tested source, seven native binaries and all 21 logs were independently
rechecked after completion. Only documentation changed before the checkpoint
commit. The first run's shared-product artifact check had failed; the immutable
helper run resolves that failure without weakening the stability checks.

`cargo fmt --all --check` and strict workspace/all-target/all-feature Clippy with
`-D warnings` passed for the frozen foundation checkpoint. Clippy log SHA-256:
`0099A18186DBB3B075A7E9CCA5A80A2808766E83E55D2677AC62C3BE1259B33B`.
These selected correctness results do not qualify native release performance,
the unfinished standard features, arbitrary vendors or other platforms.

The public SDK reproduction script also completed a fresh end-to-end run from
pinned official archives and the independent generator lock. Its generated
binding SHA-256 is `9cf5371fc7a883a857e4c74d8d4c9de43cdd958e14a73e3158cb568ff6d23940`,
identical to the canonical binding. The generator used LLVM 18.1.8 with its
original Windows GNU target selection; this does not qualify native MSVC,
macOS or Linux ABI behavior.

Packaged distributions copy `LICENSES`, which now retains the exact official
Steinberg SDK MIT notice, the binding runtime's license options, and the
HarfRust notice alongside the existing native-adapter notices. Source and
generation manifests keep their independent license identities.


### Typed CLAP scalar-event carrier qualification (2026-10-09)

The subsequent source-bound worker gate passed 20 tests with four native
installation tests ignored, followed by one typed-storage test. The independent
preparation allocator oracle reproduced 13,547,520 bytes of growth for the old
storage between 64 and 4096 maximum frames, against the derived 580,608-byte
typed-event/ordinal/PCM expectation. The corrected owner passed that same
580,608-byte expectation. A warmed full-capacity/reset control passed exact
native event order and independently calculated PCM with zero measured Rust
allocations/reallocations in realtime and offline mode.

Worker source SHA-256:
`07343967964fad8ce8de92c85c7303845d2459d09f6f6ea99b776137eb1e479b`.
Typed carrier source SHA-256:
`fa0826ace88558a162c41f9c88902a942411685b18ca2ba00c54e06490bf8166`.
Worker log SHA-256:
`f275902bbb66fc740ea4ce5caace6c29f5a7e5a67c9207aee630c04a62ed4376`.
Typed-storage log SHA-256:
`70bf26830f52e95a3cfcb528ce83e394067a34f2e0da2a16e1a25b888d6d0606`.

This qualifies current scalar-event storage capacity/allocation behavior and
the owned control, not native release deadline performance, foreign heap/RSS,
heterogeneous event support or the complete host-owned resource ledger. The
full known-owner, root/IPC callback deadline and native reprepare acceptance
remain open. See [native audio standard features](native-audio-standard-features.md)
and [audio architecture](../architecture/audio-pipeline.md).

## Single-line navigation and source projection qualification

The visual navigation stage reproduced thirteen runtime failures before the
production change, then passed all thirteen regressions and all 64 UI-text
tests. Caret geometry, hit testing, selection and navigation use the sole
complete shaped line with explicit BiDi association. Logical deletion and
Home/End retain their existing contracts; grapheme joins use the complete
result's boundaries rather than separately counted inserted fragments.

A later full input run exposed Unicode-whitespace word-navigation admission.
The initial repair incorrectly treated programmatic model content as user-input
normalization. Its four focused tests and widget/Host gates passed, as recorded
by `8FC211FBE0A079728A5939394419A7A80D7F71F5CDE9B3FEED95C60F2AFD8593`
(`.scratch/plugin-standard-audit/ingress-and-visual-green-result-r3.json`), but the
full panel run then failed the original native long-text property contract:
196 passed and one failed. A native definition's declared single-line control
must preserve its exact authored value even when that value contains separators.
Those prior narrower passes do not qualify model-byte preservation.

The corrected seam preserves raw programmatic source, focus/Undo/cancel baselines
and unchanged author no-op. One complete single-line display projection maps
shaped clusters back to original source EGC/byte endpoints, rather than changing
the author value or inventing another logical-navigation fallback. User input/paste
normalization remains an edit policy. Numeric DTO/history admission still refuses
noncanonical payloads without guessing positions. The original long native text
property and Unicode word-navigation expectations are unchanged; the four tests
based on the incorrect ingress premise are replaced by raw-source/public-geometry
regressions.

The corrected frozen stage reproduced six widget and two public geometry runtime
failures, then passed all eight after the source projection repair. It also passed
the original native long-text test, all 70 UI-text tests, 269 text-input tests,
33 number-input tests, six metrics tests, 14 Host numeric tests, 197 panel tests,
27 localization tests and 128 timeline-view tests. The eleven-gate receipt
SHA-256 is `0d4f02b01f21f43f5d6db8b722f2a10662d0b04afdd16e627cb4839e60d71246`
(`.scratch/plugin-standard-audit/raw-projection-green-result-r10.json`), with
source stability checked even on failure. The associated typed Timeline/Inspector
status localization and unavailable nonempty timeline feedback are included in
the named panel/localization/timeline gates.

The fixed typed CLAP parameter carrier additionally passed actual sample-offset
gain and sidechain/state/delay/partition native DLL gates in realtime and offline
modes using a fresh, immutable product helper. Their source/binary receipt is
`c656e591236daf27ecaefa4f3e3ab334a04688ff4a34bf9f83cbb8229219f548`
(`.scratch/plugin-standard-audit/carrier-native-result-r8.json`). This is native
correctness evidence for these workloads; it does not establish measured
realtime deadline, complete host resource accounting or universal compatibility.

Physical IME, native editors, screen-reader delivery, multiline visual navigation
and complete-line shaping performance remain separate qualification requirements.


## Complete finite native image checkpoint (2026-10-09)

The previous r90 batch reached a genuine nested-compositor failure: legitimate
PremultipliedFloat output was rejected as if every input had to be
StraightCoverage. Its separate dev/test profile artifact replacement also
invalidated the batch lease. Both original receipts and logs are preserved.

The repair uses the sole controlled borrowed storage validator at the timeline
admission seam. Actual complete association remains attached to the raster;
identity, ordinary, fused and transition inputs receive finite/alpha-domain
validation with owner cancellation checkpoints. No admission clone, conversion,
clamp or change to the original raw-cache/nested/output oracle is introduced.
Three old-behavior renderer failures independently reproduced rejection of legal
Opaque, publication of NaN through identity, and missed cancellation while
scanning a raster. Their repaired full-workspace runtime result remains a
separate gate from the native suite.

The current r103 matching-profile native suite executed **20 exact gates,
20 passed, zero failed, zero ignored**. Source and all selected native artifact
hashes remained stable. It includes parent-issued cross-child image namespaces,
plugin-private persisted analysis, FieldNone, signed recursive mutexes and stale
handles, accepted-caller thread completion, bounded mutex capacity, pre-DSP
resource refusal without advancing native work, actual Source/output association,
and public disk-cache → nested Over → real opaque output. Public thread, mutex,
completion and stuck-callback cancellation/reaping also passed.

Actual product SHA-256:
`55692cd29d1154b6ba46dc19da51fbd004f3fe691e9457358d1cdbe02a99da94`.
Native qualification receipt SHA-256:
`6ae04ebad314ce279afbec99ee5ed42ae3cd653c2d67ec2108d167bd82bd1c35`
(`.scratch/plugin-standard-audit/native-float-result-r103.json`).
The same source's r102 full workspace/all-target/all-feature Clippy and fmt gates
passed with warnings denied. These receipts qualify the named source and owned
native workflows; they do not establish all OpenFX formats/contexts, latest
standard feature completeness, native editors or a vendor/platform matrix.

The original release video deadline failures and small-block audio deadline
failures remain evidence. Current bulk transport and complete-float source need
fresh unchanged-policy release measurement, then actual cold factory, whole
compositor/NLE and device acceptance. Correct image pixels and source lint are
not performance acceptance.

## Independent third-party CPU Filter gate (2026-10-11)

The production adapters are unchanged in this slice. The new generic external
OFX integration test executes the acquired purzos-ofx **BitCrush**, publisher
release **v0.2.0**, native identifier `org.purzos.bitCrush`, actual factory/API
version1.0/1. The Windows AMD64 module SHA256 is
`AE266A44A17F4140DCB819DF3AE013B97A9F9E0246EEBD29A95DB7183DA2C3C7`.
Authored-source MIT and packaged OFX Support BSD3 notices were inspected; exact
publisher SDK build commit/complete binary composition remain unknown.
No commercial activation or native editor is involved in this gate.

Source31 has1,787 frozen inputs. Actual fresh Test2 and normal Product13 LAST
pass compilation, and strict Workspace all-target/all-feature Clippy17 passes.
Their producer/source receipts retain the initial E0599 compiler failure;
actual test-binary compilation also overwrote the product path, so the final
helper is bound to Product13 rather than the displaced Product12.

The actual exact ignored List reports one test/zero benchmarks. Two independent
serial native Jobs each report **one passed/zero failed/zero ignored/zero
filtered**, with native exit0, no deadline/errors/outliving descendants and
successful descendant settlement. Each round generates nine actual240-byte
Float32 RGBA rasters: four direct-resident renders, one fresh-instance render
before Drop, and four existing hierarchical-graph renders. Both rounds compare
all raster bytes against the independently frozen asymmetric5x3 endpoint oracle.
Bits8/mix1, bits4/mix1, mix0 and repeat remain distinct checks; probability0
and dyadic straight alpha are explicit input conditions. All six typed numeric
parameter defaults/ranges/animation flags and canonical author IDs are checked,
and full public Describe remains equal before/after registration.

Actual retained process handles are alive after rendering and signaled after
explicit shutdown or Drop. Graph retention is one owner after each call and
zero after retirement. The graph API does not expose a native PID or per-child
shutdown error; its owner count plus outer Job settlement is separate evidence
from direct retained-handle observations. The PID-to-handle acquisition race
remains. The two workflow controller elapsed values are2,151ms and2,091ms;
these include discovery/process startup and are **not** render-cost benchmarks
or measured overhead relative to a reference host.

Current execution receipt is
`.scratch/plugin-host-sprint/ofx-external-source31-native-r2/adapter-receipt.json`,
SHA256 `1E22C70BD9BE6F949DEC920E4060E8ED66B9E9172A8C797AC3901926E9608B86`.
Actual raw rasters, description/inspection, parser controls, author requests and
lifecycle observations are retained under
`ofx-external-source31-cases-r1/round-{1,2}/native-evidence`.
Independent raw-output acceptance is recorded in the sprint coordination record.
This finite CPU Filter gate does not qualify GPU interop, HDR/interior rounding,
temporal access, tile/multires, animation execution, opaque state, GUI,
Preview/Export/application UI, realtime, other platforms or full OpenFX coverage.
## CLAP input-note metadata qualification, source32

On 2026-10-11 the current Windows source32 closure contains1,796 inputs, freeze
SHA256 `EE99CA942A6CB9016960769FA917B9E521B4313334995525D513BEC800E66D3D`.
The new version1 operation inspects inactive plugin input-note metadata using
the existing isolated discovery helper. It preserves raw native IDs, name bytes
and dialect masks, distinguishes absent/empty extensions and refuses malformed
or changing facts. It exposes no HostNotePorts service or input/output note
execution. The discovery path now immediately owns both Child and staging,
retains staging through response decoding and keeps it if termination is not
confirmed. This does not establish bounded successful-kill kernel waits or
permanent orphan cleanup.

Actual Audio16/Fixture1 and normal Product15 producers bind this closure;
Product14's real full-disk link failure remains a failed historical producer.
Current ordinary Audio reports345 passed/zero failed/74 filtered, and strict
workspace Clippy all-targets/all-features passes. Source32 App ordinary runtime
regression is not thereby established; the previously qualified App2778 group
belongs to source29.

Fourteen process-owner controls and two rounds of the original CLAP ABI fixture
each report1 passed/zero failed/zero ignored/418 filtered. Raw Windows Job owners
all exit0 with descendants reaped and no errors or deadline expiry; actual root
wrapper exits0 after cleanup. Independent acceptance receipt SHA256
`510550147B5D25C09E3AE58144A73E2F064A26C851CEDB3E723AC74FC2FFC997`
binds exact producers, current images, all1,796 source inputs and3,744 runtime
bindings. Source inspection proves the32-input policy before allocation/get;
the cap fixture's aggregate refusal does not establish its exact failure branch.

Qualification is **Verified within these owned metadata/ownership controls**.
Third-party note processing, instruments, authored musical events, realtime
deadline safety, performance, other platforms and full CLAP compatibility remain
unqualified. Real Ring Out and shared audio regressions have separate current
records, described below and in the [sprint checkpoint](plugin-host-sprint.md#clap-input-note-metadata-and-discovery-ownership-integration).

After retaining pre-native timestamp/environment preparation failures, corrected
legacy wrapper r3 actually completes10/10 groups: two rounds of owned CLAP/VST3
sidechain PCM/state/partition/lifecycle, canonical signal-time and VST3 repeated
native-object controls. Current Audio16/Product15 and1,796 current sources are
held; historical fixture DLLs and their unchanged compiled input closure remain
separately identified. Aggregate SHA256
`A6AC7C017D1C2612ECF556659C2DCDF8B79DD0E3F417D0BFB0E4EB40FC2BD549`.
Each exact test reports1/zero failed/zero ignored/418 filtered, with actual
exit0/reaped/error-free native Jobs and root terminal0 after cleanup.

Corrected Ring wrapper r3 separately completes2/2 unchanged real Ring Out
metadata/default opaque-load groups at source32. Aggregate SHA256
`A6CEC32937DE16B3174DC507B56BB844B5CE4BC6CA467B87AA875CB30293EB82`.
It checks full11 descriptors/editable9/read-only2, canonical author reopen,
resolver payload, native observed editable values and the unchanged184-byte
default state. Both raw owners and root wrapper exit0 with successful cleanup
and source checks. Native plugin version remains Unknown. No PCM, nondefault
private-state, Note/instrument, realtime or performance qualification follows.

## Finite M2 Note Runtime and direct DLL qualification (2026-10-14)

Final compiled source has 1921 inputs, SHA-256
`87C7F45275B988698BCC6AB83BEE6B92AAD7B8D5A7C4EB2B48B915A46D3613D6`.
The actual Audio inventory is 547 names /84 ignored /463 ordinary, preserving
all previous names. Two ordinary runs each pass463/0 under the unchanged
controller/owner gates; 44 ordinary and three separately selected ignored DLL
controls are new relative to carrier commit1110a611. Strict all-targets/
all-features checks of the five affected model crates have zero diagnostics.

The fixed owned MIT NoteGate DLL B239C594...F32AF1D is actually dynamically
loaded and processed by the CLAP adapter. Inline and borrowed DSP-role controls
compare independent literal PCM, parameters and copied Ends across partitions;
a third control observes native rejection of the wrong original Off after early
End. Each selected control passes1/0. Ordinary receipt is2D44FE16...1C9F8AF;
final DLL receipt is2C9D8F81...D2D8757. Source/inputs remain unchanged, actual
owners exit0 and reap descendants, with no timeout, outliving or cleanup error.

Qualification is **Verified within those owned Windows controls**. The DLL is
a mono PCM-input effect, not a third-party plugin or zero-input instrument.
Its adapter tests do not traverse Parent IPC or with_processing_scope. The
Installed-root model test uses an owned Rust factory, not this native DLL.
Public nonempty Note Score remains allocation-free Unsupported; full processing
offer/resolver, preparation peak admission, actual whole-root capacity refusal,
bad-End poisoning, live all-peer handoff/Ack retry, zero-frame dispatch, replay,
nesting, product author controls, physical realtime, performance and other
platforms still need their respective acceptance evidence. The current normal
Product build passes with FFmpeg8.1.2; it is not a native execution result.
See the [sprint record](plugin-host-sprint.md#a01-m2-exclusive-hosted-ledger-and-native-noteports-2026-10-14)
for full receipts, retained failures, architecture boundaries and review records.

## Coherent CLAP query and prepared logical policy (2026-10-14)

The next compiled cohort has1,925 source inputs, SHA-256
`0096DAF8D5BC3CA90271586F64E1910661E057C14A97B585EB7708845433423A`.
Actual inventory566/85ignored/481ordinary retains every previous547 test name.
Two owned Windows ordinary rounds each pass481/0, including18 new controls.
ReceiptD5CF775A...65E91B binds current retained Audio PE3CCFD387...35A10D,
unchanged sources and actual error-free, deadline-free, reaped process owners.
Five affected model crates pass strict all-targets/all-features checks.

Schema2 ProbeNotes now captures parameters, auxiliary audio layout, latency,
tail, render mode and Notes from one restored, Notes-enabled native instance.
It retains the same services owner through destruction and rejects unsupported
or unserviced observations before publication. Seven raw ABI/lifecycle tests,
one pure source snapshot and two wire controls are **Verified within these
owned controls**. Ordinary probes/schema1 retain their existing behavior.
Application Resolver integration and product Note Score remain **Unsupported**.

The fixed B239C594...F32AF1D owned DLL also passes the actual schema2 parent
query through current normal Product3870C862...635586E, exact1/0 (two scenarios:
without state and with16-byte state restore). Independent literal parameters,
Mono layout, latency0/tail4, mode, high-bit port IDs and complete fingerprint/
finite-policy binding pass. Receipt409B4A6F...DE470C6 has actual0/reaped owners
and unchanged inputs. This is **Verified within this owned Windows metadata
query**, not Note DSP or third-party compatibility. Initial helper startup
failure0xc0000135 remains recorded as0E634EE2...7A74A3; R2 pins and selects the
existing FFmpeg8 runtime environment without changing product/test/fixture bytes.
Normal Product build passes with FFmpeg8.1.2 and ten existing warnings.

Empty-score Runtime preparation now reserves the existing prepared-logical
upper-bound policy before local media/processor creation, retaining ancestor
charges while preparing children. Eight controls verify actual policy parity,
checked accounting and no earlier calls on root/child refusal. This does not
qualify Note IR peaks, author clone/all external prepared-handoff residency,
SDK/Arc allocation ownership, physical memory or realtime safety. Public
nonempty Note Score retains the original allocation-free M1 refusal.

After these gates, only two stale ordinary Rust comment lines are corrected;
complete-file in-memory reversal proves the compiled schedule.rs hash and all
other1,924 compiled input hashes remain unchanged. Later repository documents
are outside the compiled selector. No new build/native or final checkout
byte-identity claim follows from these documentation changes.

See the [sprint record](plugin-host-sprint.md#a01-m2-prepared-policy-and-coherent-clap-offer-2026-10-14)
for current receipts, retained original failures, source review and remaining
dependencies. Query qualification does not establish Note DSP, instruments,
third-party compatibility, comparative performance or another platform.

## Preserved author preparation preflight (2026-10-14)

The Preserved parameter path now checks the existing prepared-logical policy
over the whole retained Runtime tree before its first local deep clone. It
counts the current Root once, adds each actual replacement's requested weight,
and preserves the selected equal-program replacement rule. StateEntry dispatch,
final observed overlap checks, Source publication, CAS, Drop and Ack stay on
their existing paths.

Three owned public-API controls first reproduce allocations before tight-grant
refusal. After the fix, those calls refuse with typed budget errors and zero
requested allocations/reallocations. Three positive controls verify exact-grant
installation, shared Scope materializations and an unchanged nested peer, with
original native object identities, epochs/cursors and literal PCM outputs.
Final sourceADB782DD...562A9C6 /1,927 records passes two ordinary Audio rounds,
each487/0, and strict checks for five affected crates with zero diagnostics.
The same current normal Product build passes with FFmpeg8.1.2/codec62/util60;
its actual product file and generated FFI bindings are retained independently.
Product execution and a current native DLL query are not covered by that build.
All old566 test names and85 ignored names remain. Actual receipts and retained
negative records are in the [sprint record](plugin-host-sprint.md#preserved-parameter-preparation-logical-preflight-2026-10-14).

This is **Verified within the owned Rust model's Preserved logical-policy
preflight and installation controls**. It does not qualify all externally held
handoff residency, physical/SDK/Arc memory, StateEntry peaks, native DLL DSP,
NoteScore execution, instruments, third-party plugins, realtime processing,
comparative performance or another platform. Nonempty public NoteScore remains
Unsupported; separate work is required before that gate can be lifted.

## Repeated parameter capacity accounting (2026-10-14)

The current four-file repair is **Implemented — Not Fully Verified**. It
keeps the initial conservative policy and records each Session's currently
charged parameter component. Ordinary overlap and StateEntry remove that
component consistently; ordinary publication also updates nested Runtime
footprint caches before subsequent preparations consume them.

The unchanged owned Scope control has a qualified actual Red followed by
Green1/0: two real source polls deliver its second scalar proposal, then the
second preparation/install/Ack and literal PCM assertions complete under the
original grant. The same source projection passes strict checks for five
affected crates. A separate nested-materialization regression and independent
review are in progress. Mixed Preserved/StateEntry execution, all-held logical
fees, full current ordinary regression, native SDK/third-party compatibility,
physical memory, realtime and comparative performance are not qualified by
this result. See the [sprint record](plugin-host-sprint.md#charged-parameter-component-original-scope-control-green-2026-10-14)
for exact receipts and preserved failed entry attempts.

The separate nested control now passes together with the original Scope control
on its fresh1,931-source snapshot: two exact groups, each1/0. Three updates
check local charges, both child caches, Root aggregation and literal PCM, and
strict five-crate checks pass on that same source. The full490 ordinary batch
remains failed at489/1, with only the unchanged B all-held desired-policy test
failing. This provides finite owned-model regression evidence for the capacity
repair; it does not qualify all-held lifetime policy or the mixed StateEntry
sequence, nor does it mark the current integration fully accepted.

## Held native parameter Plan roles (2026-10-14)

The original Root now accounts logical pending/retired Plan roles separately
from its published steady footprint, under the same frozen grant. Actual owner
receipts enroll new Plan weight, transfer to old Plan weight on installation,
and refund after the preceding storage fields finish destruction. Ack cannot
refund an externally held Preserved owner; StateEntry fees belong to the actual
Root-owned cohort rather than its external metadata handle.

Source8742C89F...976BDD has actual22 exact passes and503/0 complete ordinary
regression, with all prior names and85 ignored names preserved. The unchanged
B assertion now observes exact required67776/granted45184 refusal while its
old Plan is alive, and actual Drop enables the same-grant retry. This is
**Verified within the owned Preserved logical-policy and private receipt
controls**. A strict layout warning then required a small owning-container
correction; current51F0AB47...688FCB passes the original five-crate strict gate
and fresh default compilation. Its own execution and Product gates are tracked
in the [sprint record](plugin-host-sprint.md#held-plan-logical-roles-owned-policy-qualification-2026-10-14),
without transferring older-source evidence automatically.

Real StateEntry fees, native opaque destructor/Runtime-first lifetimes,
same-Source Superseded CAS, StateEntry before-first-clone, complete physical/
fixed accounting and allocation cost, realtime, third-party/platform and
NoteScore execution remain **Implemented — Not Fully Verified** or unsupported
at their precise existing boundaries. Owned/private green tests do not upgrade
those capabilities or establish production compatibility for all plugins.

Current51F now has its own22 exact passes and two complete503/0 ordinary runs,
under the original gates, independently reviewed alongside same-source strict
checks. Its normal FFmpeg8.1.2 Product build also passes with actual[]/opt1/
debug2/fresh=false, unchanged D55A FFI bindings and separately retained
1CAC2719...54753 Product. Build success does not add Product execution, real
StateEntry fees or native/third-party compatibility to the owned logical slice.

## Real StateEntry logical-role lifetime (2026-10-14)

Source50FF5490...40E71EB /1,936 inputs adds only two Windows test declarations
and two test modules to the accepted5eed production implementation. The old
helpers remain byte-identical. Fresh default Audio compilation and actual
590-test inventory retain all588 previous names;87 tests are ignored, and all
503 ordinary tests pass. The original five-crate strict all-targets/all-features
gate passes without diagnostics. Same-source normal Product6D6E04B4...928AE88
uses the existing FFmpeg8.1.2 SDK and D55A bindings.

Two explicit ignored tests run twice against that actual Product worker and
fresh repository-owned VST3 editor DLL D6D5403F...1D52E6B /288,256 bytes. Its
25-input build, SDK3.8.1 provenance and license sidecars are retained. These are
**Verified finite Windows owned-fixture StateEntry lifetime scenarios**; they
are not independent third-party compatibility or normal product GUI acceptance.

The non-nested scenario observes actual O=33,664 and N=34,688 logical bytes.
Two distinct real plugin objects independently produce typed partial cleanup
failures. Cancellation, retry and metadata-handle Drop retain the same actual
cohort/storage and charge N once. Actual install moves the role to O, preserves
Source authority, and exact native Ack retires the cohort and its old Plan;
H becomes zero. Fresh execution produces the independent stereo PCM oracle
and retains the original native child identities. The nested scenario counts
all three local Sessions, including22,016 logical bytes for the unaffected
root, in N=70,528. Dropping the actual pending Runtime retires the cohort,
releases old Plan Weak witnesses and credits H exactly once.

Both native rounds exit0 with exact one-test passes, unchanged frozen inputs,
no deadline/errors/outliving and reaped descendants under the existing native
Job runner. A rejected generated execution script is preserved; its parse
failure occurred before any native launch. The corrected controller adapter
adds a complete comparison against the old588 names without changing tests,
timeouts or the existing execution engine.

These generous-grant tests do not qualify StateEntry rejection before its first
clone, exact budget boundaries, installed Runtime-first destruction, blocked
opaque native destructors, native Ack refusal or same-Source Superseded CAS.
Fixed/physical/SDK memory, realtime/performance, instruments/NoteScore,
independent third-party plugins and macOS/Linux remain separate open gates.
The next minimal contract adds a borrowed whole-tree logical check before the
first StateEntry clone; it preserves the same grant, fee authority and retry.

## Whole StateEntry logical preflight (2026-10-14)

SourceB5AAEBC8...135366 /1,938 inputs adds one private Scope borrowed traversal
and three real budget tests. It preserves the original allocator, persisted
retry, resource authority and install/Ack tail. Source review A32C787D...6B97F8
verifies R covers all affected locals and the unaffected local Plan roles once;
actual N<=R remains a separate guard before source inspection.

Original sourceDAFB3A36...64AEDA6 has two genuine desired-red tests. The root
case returns exact required68,352/granted68,351 after127 host allocations and20
reallocations; the nested case returns140,032/140,031 after270 allocations and21
reallocations. Actual error, Source, every cursor, windows, no cohort/H0 and
retirement checks pass before the final zero-allocation assertion fails. These
failures and their original artifacts are retained.

On the repaired source, both tight original grants return the same typed errors
with measured [0,0,0,0] host-thread allocation/reallocation/byte counts. A third
real root test freezes G=68,352, observes C=33,664/H=N=34,688 at the exact bound,
and proves partial cleanup retries and metadata Drop keep that same cohort and
charge once. Actual install N→O, exact native Ack, old Plan retirement, epoch702,
known stereo PCM and native child identities pass. These three tests and the
two original cohort-lifetime tests each pass twice on the same-source Product
worker E0042742...5D71F7C and owned editor DLL D6D5403F...1D52E6B. Native owners
exit0 without deadline/errors/outliving and with reaped descendants.

The actual593/90/503 inventory retains previous tests. All503 ordinary tests,
the original22 directed controls, five-crate strict all-targets/all-features
with zero diagnostics and the normal FFmpeg8 Product build pass. This is
**Verified finite Windows owned-fixture logical budget behavior**. The TLS
meter covers this calling host thread, not SDK/worker heaps or process memory.
No realtime benchmark, SDK/IPC zero-call count or physical allocation bound is
inferred. Fixed-budget early refusal, nonzero prior native H, affected equal
peers, native sidechain/Note shapes, installed Runtime-first/blocked destructors,
native Ack refusal, same-Source CAS, independent third-party plugins and other
platforms remain separately qualified or open at their precise boundaries.

### Windows CLAP deactivated parameter observation, 2026-10-14

A Windows test-only consumer now retains owned same-call metadata across both
successful acquisition and the original numeric refusal. It uses the existing
collector once per fresh instance, copies actual descriptors only after their
original construction, and destroys the deactivated instance before exporting
exact bit strings and bounded actual names. False metadata, invalid UTF-8 and
false current-value returns retain their original early stop and unavailable
copied fields. Product discovery, worker processing and public Schema do not
gain a new parameter authority or a changed default-value policy.

Six owned Clack ABI controls pass with exact count/order and destruction
oracles, including success, numeric refusal and the three early-stop branches.
The same-source Audio inventory is 600 tests, 91 ignored and 509 ordinary;
all 509 ordinary tests pass. The same-source five-crate all-targets/all-features
Clippy check with `-D warnings` passes with zero compiler diagnostics. These
controls qualify the diagnostic consumer, not third-party processing or realtime
behavior.

One new supervised experiment used the original Airwindows Consolidated CLAP
binary `6388EA29...4094FAA` and original 314-byte Galactic state
`835DFDE8...02DFC0B`. Both predeclared fresh instances returned 14 owned rows
and completed native destruction: one before state restore, one after loading
that state. Actual names and values identify the Galactic active control
Dry/Wet at index 4 with default/current 1. In both instances inactive index 6
has name `-`, current zero and a negative default with exact bits
`BD327F8000000000` despite an advertised range 0..1. Raw, converted and copied
values agree bit for bit. The anomaly therefore occurs before state restore in
this experiment; it is not introduced by a later Rust scalar conversion.

The native test remains **failed**: test exit 101, runner exit 1, descendants
reaped, no deadline, outliving child or owner error. The original default-range
validator correctly rejects the native metadata. Its stdout contains both
snapshots; a libtest name prefix on the same line initially confused the outer
extractor. The retained closed log was extracted without rerunning the plugin
or altering the failed receipt. Specific raw rescan flags remain unavailable;
revision/count snapshots do not prove them or complete destruction callbacks.

Pinned upstream source has an inactive float-default initialization omission,
and the observed anomalies admit exact Float32-to-Float64 widening. This is
supporting evidence, not a verified writer instruction or atomic source/build
binding for the installed DLL. No epsilon, clamp, parameter-name exception or
external-plugin repair was added. Airwindows processing compatibility, the
normal Product/helper path, native exact callback counts, other platforms and
performance remain unqualified by this experiment.

A subsequent harmless debugger-launch control produced a helper report outside
every Job, with `FILE_TYPE_CHAR` standard handles and no captured markers. The
debugger Job's cleanup therefore cannot qualify target ownership or settlement;
no target exit code was observed. Further writer observation requires a directly
owned target and a separately settled observer, first validated without plugins.
