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
| Audio layout | Mono/stereo main in/out, up to 31 auxiliary inputs of the admitted layout; restored-state native port metadata and causal PDC | Mixed layouts, surround/ambisonic, multiple outputs, zero-input generators/instruments |
| Audio parameters | Canonical author curves and sample offsets; CLAP plain values, VST normalized values; insertion/rebinding/history/archive state | Native display conversion, units/programs/presets, output events, plugin-origin gestures, large state/resources |
| Audio render mode and threads | Explicit VST realtime/offline selection; CLAP requested-mode probe and execution, hard-realtime/declined-mode refusal; instance-local CLAP symbolic thread checks | Dynamic restart/rescan/latency/tail handling, sleep/request-process scheduling and extension event loops |
| Audio transport and events | Canonical signal sample position reaches VST3 ProcessContext and CLAP seconds transport; unavailable tempo/beat/signature/transport facts are not invented | Tempo-map authoring, MIDI 1/2/SysEx, notes/expression/polyphonic modulation and event routing |
| Bounded Note execution infrastructure | Typed carrier/IPC v9, private once-projected score, exclusive Hosted ledger, whole-window admission and restored CLAP NotePorts; finite owned Windows model and direct fixture-DLL controls | Public score admission remains Unsupported; preparation peak/grant, same-instance application processing offer, Parent IPC/DAG native Notes, zero-input instruments, replay/nesting and physical realtime |
| Native editors | Product hosting, isolated editor protocol and observation/author handoff exist; first signal and same-view refresh have dedicated qualification gates | Vendor/platform matrix, resize/DPI/focus/keyboard, remaining GUI/timers/FDs and destructive later-entry continuity |
| OpenFX CPU image execution | Filter Source/Output, current-frame full-frame RGBA Float32, progressive; complete typed Straight/PM Float and Opaque signal, exact Normal Over, representation-aware cache and actual nested/opaque output; resident native instances | Other contexts/required inputs, formats, fields, mixed PAR/depth, tiles/scale/temporal/ordered scheduling; nonlinear/GPU/OCIO consumers retain explicit format admission |
| OpenFX capabilities and bootstrap | Actual Float32/CPU advertisement; optional early host bootstrap; CPU-disabled admission refusal; canonical PAR dimensions and progressive fields | GPU execution and negotiated colour/lifetime/synchronization contracts |
| OpenFX author controls | Numeric tuples/colour/bool/choice/StrChoice/UTF-8 text/resources; canonical curve queries and shared transactions | Spatial semantic types, hierarchy/buttons/custom/parametric/interacts, dynamic descriptors, plugin-origin writes/private data |
| OpenFX product callbacks | Native message/progress/timeline primitives exist | Typed application messages, real cancellation/progress/transport responses and consistent persistence |
| Product interaction | Shared Rack control identity preserves keyboard ownership through canonical refresh; focused text owns clipboard and bounded local Undo/Redo; single-line visual BiDi/EGC navigation and associated caret geometry; exact model ingress with one-line source projection | Remaining control adoption, accessible numeric names, multiline visual navigation and physical/native-editor acceptance |
| Performance | Persistent children, bounded mappings, preallocated native audio buffers and execution budgets | Measured native DSP plus IPC percentiles/deadlines, video frame budgets, warm/cold/memory/overload and vendor/platform matrices |

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
