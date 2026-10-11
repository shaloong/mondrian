# Plugin host engineering sprint

Updated 2026-10-12 (Asia/Shanghai). This is the recoverable coordination record
for the OpenFX / VST3 / CLAP sprint. Detailed contracts remain in the existing
[plugin architecture](../architecture/plugin-system.md),
[audio pipeline](../architecture/audio-pipeline.md),
[audio standard matrix](native-audio-standard-features.md),
[OpenFX standard matrix](native-openfx-standard-features.md), and
[compatibility evidence](native-plugin-compatibility.md). Source capability and
historical qualification are distinct; stale matrix rows must be checked against
their actual consumer before changing a support claim.

## Recovered baseline and evidence

The original isolated checkout was clean at `a1925ca`, an ancestor of the prior
`a808891c8c91d122798c01ec633202c08ca2d9eb` worktree. The coordinator created
`feature/plugin-host-sprint` at that prior base and restored its retained tracked
and untracked production sources, rather than reimplementing its native adapters.
The source worktree at `E:/dev/mondrian/.claude/worktrees/nle-backlog` remained
read-only. Recovery verified all 2,100 source file hashes; 817 files required
copying and one obsolete file required removal. The original destination is
recoverable from `.scratch/plugin-host-sprint/source-recovery/pre-recovery.zip`.
The manifest and pre/post stability evidence are in that directory's
`receipt.json`. Recovery checkpoint `13fefdf33747a907a25075d7449c1f473a9264e9`
records that snapshot separately from this round's changes. An alternate index
staged only the recovery manifest; the real index was aligned without touching
working files. A pre-existing empty index lock was removed only after checking
the exact path, absence of Git processes and unchanged parent index. All 2,100
original and 1,772 current source hashes remained stable. The checkpoint is not
a native acceptance claim.

Current round evidence:

| Gate | Result | Scope |
| --- | --- | --- |
| Source recovery | PASS | Exact file hashes and stable source tree; no execution claim |
| Cargo metadata | PASS | Workspace/path package resolution; `.scratch/plugin-host-sprint/cargo-metadata.json` |
| CLAP source provenance | PASS | Official generated source/package identity; no runtime claim |
| Clack source provenance | RED then PASS | Audited metadata repair, original checker unchanged, 252 shipped paths |
| Audio baseline | PASS | 331 passed, zero failed, 50 ignored; `.scratch/plugin-host-sprint/audio-baseline-receipt.json` and log |
| Audio after VST3 declaration policy | PASS within library scope | 331 passed, zero failed, 50 ignored; `audio-after-vst-policy-r2.log`, native ignored cases remain separate |
| Shared domain baseline | PASS | Audio scheduling 8, Effects 327, Timeline 247 passed; one scheduling case ignored |
| VST3 consumer policy | PASS within COM scope | Five actual policy/instance/default/builder controls; native vendor initialization remains separate |
| Native lab process owner | PASS within Windows scope | 12 library tests passed, four fixture functions ignored; four new controls execute real owned child/grandchild processes |
| Strict workspace Clippy | PASS | All targets/all features, `-D warnings`, exit zero in 2m06s; 1,772 frozen source hashes stable; `workspace-clippy-r6-receipt.json` |
| Frozen lab controller | PASS within selected scope | Seven tests in three groups, 1,771 source hashes stable; five negative controls refuse wrong count/hash, duplicate ID, timeout and a live descendant |
| DXC build environment / renderer check | PASS within build scope | Original immutable API and SHA-256 checks executed; exact cached archive reused; `renderer-build-environment-r6.log`. No GPU output or performance claim |
| Fresh Basic / SessionProbe binaries | Verified within selected native cases | Fresh build r5 / freeze19: 15 original native groups; no full OFX/GPU qualification |
| Active native catalog shutdown | Verified within two controlled Windows cases | Actual discovery/describe worker retirement and native HANDLE exit; other GUI lifecycle remains separate |
| Independent vendor discovery | Verified within List/Probe/capture only | Airwindows CLAP/VST3 eight runs and Ring Out CLAP four runs; no full compatibility claim |
| Initial VST3 value restart consumer | Verified within owned initial/fresh-object controls | Commit0bd39c4; SDK10/10, prior native10/10, ordinary Audio332 and vendor269, independently accepted source26 |
| Signed-zero buffer fidelity | Verified within owned buffer controls and limited native VST3 replay | Source27 RED270/1 to source28 GREEN271/0; Audio332/0; Airwindows two rounds pass unchanged strict oracle; no SDK violation/performance claim |
| External native PCM | VST3 limited PASS; CLAP RED | Airwindows VST3 default-state offline bypass partitions1/17/64 and distinct finite non-bypass output; CLAP schema drift remains unresolved |
| CLAP read-only metadata / default state load | Verified within Ring Out two-round controls | Source29 Audio336/0, exact native2/2 and independent raw state/author/payload checks; existing core behavior, new verification gate only; no PCM/full-project claim |
| Periodic App ordinary regression | Verified within source29 default libtest scope | 2,778 passed/zero failed; exact ordinary inventory, current producer, 1,786 source hashes and four native Job retirements independently accepted; 124 ignored cases remain separate |
| External OpenFX CPU Filter | Verified within source31 BitCrush finite Windows cases | Two actual rounds, each one passed/zero failed; 18 bit-exact Float32 rasters across direct and existing graph execution, parameter changes and instance retirement; GPU/temporal/full OFX remain unqualified |
| CLAP input-note metadata / discovery staging | Verified within source32 owned metadata and ownership controls | Audio345/0, original11 plus3 new owner controls and two metadata rounds; strict workspace Clippy; no note dispatch/instrument/RT qualification |
| Source32 shared audio and real CLAP regression | PASS within existing finite workflows | Two rounds of five CLAP/VST3 sidechain/PCM/time/object cases and two real Ring Out metadata/default-load rounds; current host, separately retained historical fixture binaries |
| CLAP factory slot admission | Verified within source36 Windows controls | Cached count capped before List/Probe/Note traversal; two actual RED controls, six GREEN ABI-counter controls, ordinary351/0 and current native regressions;4096 is host policy, not a CLAP limit |
| Source36 audio regression | PASS within existing finite workflows | Ordinary351/0; owner14 and metadata2; original CLAP/VST3 cases10; real Ring Out metadata/default-load2; strict workspace Clippy. No new Note DSP, GUI, RT or performance qualification |
| CLAP parameter-refusal detail | Verified within source38 finite Windows controls | Original admission preserved; same rejected snapshot's first reason, unsigned ID/index and direct numeric bits; ordinary355/0 and two real failed Airwindows startup observations, without compatibility/root-cause qualification |
| CLAP startup refusal consumer sites | Implemented — Not Fully Verified | Source40 owned controls exercise three existing consumers; real Airwindows rounds failed without a tagged descriptor refusal, so third-party site capture remains unverified |
| Source40 audio regression | PASS within existing finite Windows workflows | Ordinary359/0; owner14 and metadata2; original CLAP/VST3 cases10; real Ring Out metadata/default-load2; strict workspace Clippy. Original vendor PCM gate remains failed |
| New performance/platform qualification | Not run | Historical results below do not transfer to new changes |

Clack's RED reproduced in both source and destination. The GUI wrapper differed
from its sealed `clap-gui-refusal-v1` preimage only in three method-chain line
wraps. The sealed manifest already identifies the current documentation bytes,
but its local metadata still held an older documentation hash. The note-port
source matched its declared unsigned-carrier delta while `shipped_files` retained
an old hash. Only `vendor/clack/PROVENANCE.json` and
`vendor/clack/LOCAL-FORMAT-CHANGES.json` changed. Runtime, upstream input and checker
bytes are unchanged. Original RED, metadata preimages, sealed witnesses and GREEN
are saved in `.scratch/plugin-host-sprint/clack-provenance-repair/`.
Original LF was restored to avoid a whole-file Windows newline diff; the dependent
raw receipt hash was updated without another semantic change. The unchanged
checker again passed 252 paths. Final provenance SHA-256 is
`e90ace85359e950ed8419cbb9ffdd4228770d4189d4320b24cf8a0bc3c0f53f0`.
This repairs source attribution; it does not qualify runtime compatibility.
Commit `c5337e5` contains only the metadata files. Commit `87db4dd` contains the
separately reviewed VST3 instance policy, and `3572a3c` the Windows lab owner.
The controller's later outer-wait fix remains a separate acceptance slice.

The prior `handoff-6oqHh9.md` records matching r584 strict checks and default-stack
workspace tests: 8,440 passed, zero failed, 303 ignored. Native r561 records 36
owned cases and GPU r562 records 10 portable plus seven actual RTX 3050/DX12
cases. Those results bind their exact source/artifact/environment hashes. Their
ignored, unexercised and platform limitations remain. Fresh compilation and
execution are required for this restored tree and every subsequent patch.

Literal include audit found 118 unique Rust `include_str!`/`include_bytes!`
inputs, all present and none ignored. No tracked symlink/submodule was found.
This is a source audit, not an exhaustive proof of dynamic runtime dependencies.
`.scratch`, `.claude`, `target`, generated DLLs and secrets were excluded from
production recovery. Historical receipts remain in the original evidence store.
Native build prerequisites include OCIO, FFmpeg, MSVC/ATL and pinned static DXC;
the source snapshot does not include their compiled outputs.

## Capability status

`Verified` means the stated boundary has matching executed evidence. `Implemented
— Not Fully Verified` means source exists without the required vendor/platform/
performance evidence. `Partially Supported` means an admitted finite subset.
`Unsupported` means no product execution contract. `Blocked` records a concrete
missing prerequisite, not merely unfinished engineering. No complete standard
has current-round `Verified` status.

| Format / capability | Status | Boundary and acceptance still required |
| --- | --- | --- |
| VST3 discovery, class/fingerprint, supervised worker | Implemented — Not Fully Verified | Repeated actual load/unload, errors and permitted vendor corpus |
| VST3 PCM/buses | Partially Supported | Float32 product, mono/stereo main, up to 31 admitted same-layout auxiliary inputs; mixed/surround and multiple outputs absent |
| VST3 sample-offset parameters | Implemented — Not Fully Verified | Normalized bounded queues; native output, saturation, allocation and full callback deadline gates |
| VST3 state | Partially Supported | Bounded component/controller envelope; arbitrary DSP checkpoint not implied |
| VST3 transport | Partially Supported | Exact signed signal sample/rate; unavailable musical facts remain unavailable |
| VST3 MIDI/notes/instruments | Unsupported | Vendor event APIs exist; product author/event/output routing does not |
| VST3 GUI | Partially Supported | Windows same-object editor path; later destructive entry, DPI/focus/keyboard and vendor matrix pending |
| VST3 dynamic reprepare/PDC | Partially Supported | Cohort/hold/seed controls; canonical installation and retained author acceptance pending |
| VST3 complete vendor/platform certification | Blocked | Matching fresh fixtures/vendor permissions and Linux/macOS execution hosts unavailable in this round |
| CLAP discovery, descriptor/fingerprint, supervised worker | Implemented — Not Fully Verified | Actual repeated lifecycle and vendor controls required |
| CLAP audio ports | Partially Supported | Admitted Float32 mono/stereo main and auxiliary topology; notes/instrument/multi-output contracts absent |
| CLAP parameter events | Implemented — Not Fully Verified | Plain scalar values, ordered bounded carrier; full allocation/deadline and author closure pending |
| CLAP state | Partially Supported | Bounded state/configuration; state seed is not proven DSP continuity |
| CLAP transport | Partially Supported | Exact signed seconds; musical time/tempo-map facts unavailable |
| CLAP note/MIDI/polyphonic modulation | Unsupported | No canonical product event route or note-port execution |
| CLAP GUI/main services | Partially Supported | Windows same-object roles/timers exist; exact callbacks and native platform loops need separate gates |
| CLAP sleep/request-process scheduler | Unsupported | Callback presence alone does not schedule sole Audio DAG |
| CLAP dynamic ports/latency/restart | Partially Supported | Sticky request/control path; actual canonical graph candidate installation unfinished |
| CLAP complete vendor/platform certification | Blocked | No installed independent CLAP binary found; permitted corpus and other platforms still need provisioning |
| OpenFX Filter CPU execution | Partially Supported | Current-frame full-frame progressive RGBA Float32 Source/Output, resident instances and explicit alpha contract |
| OpenFX numeric/text/resource parameters | Partially Supported | Canonical curves/transactions; spatial/hierarchy/custom/dynamic/plugin-origin edits incomplete |
| OpenFX Generator/Transition/General/Retimer | Unsupported | Do not adapt these silently to a single-input Filter |
| OpenFX temporal images, ROI/RoD, tiles/multiresolution | Unsupported | Full-frame current-time subset; scheduling/cache dependencies must be added before advertisement |
| OpenFX components/depth/fields | Partially Supported | Declared CPU Float32/progressive subset; mixed formats/fields/PAR admission remains explicit |
| OpenFX required/aux/thread suites | Implemented — Not Fully Verified | Native ownership/callback evidence finite; full vendor suite conformance is unqualified |
| OpenFX overlay/interact | Unsupported | A descriptor/API presence is not a product interactive workflow |
| OpenFX GPU interoperability | Unsupported | wgpu numeric Mix is not an OpenFX GPU Suite; no native-resource/sync contract |
| OpenFX discovery shutdown in qualified Windows cases | Verified | Actual blocked discovery/describe Host Drop settles child and thread without publication; OS-wide failure/latency guarantees remain unqualified |
| OpenFX full vendor/platform certification | Blocked | Official Basic and owned SessionProbe qualify a finite subset; broader vendor corpus and Linux/macOS/native UI/hardware execution remain unavailable |

The `Verified` category applies to source recovery/provenance and the finite
current Windows COM and process-supervision controls above. It does not upgrade
whole format capabilities. Historical narrowly verified host cases remain recorded
with their original receipts in `native-plugin-compatibility.md`; they are not
upgraded to full third-party compatibility here.

## Shared contracts and ownership

Keep one author Session/transaction/history authority and exact occurrence IDs.
Plugin output observations are execution feedback until an explicit canonical
author transaction accepts them. A state seed, local epoch, Bind or GUI view
does not mint a new installed owner, DSP position or accepted clock anchor.

Audio uses the existing typed Routing DAG, prepared PDC, sample-rate/block/mode
admission and signed signal time. VST normalized values and CLAP plain values
remain distinct adapters. Note events require one future canonical time/bus/
payload contract before either format introduces product event routes. Main/UI
affinity, symbolic audio role and physical scheduling registration are distinct.
Realtime/offline requests must reach actual native preparation.

Video uses the compiled Effects graph and existing Renderer/Cache authority for
both Preview and Export. Render requests carry exact occurrence, time, scale,
extent, pixel/alpha identity, generation and cancellation. Native image ownership,
temporal requests, ROI and cache invalidation must extend that authority. Native
GPU handles require an independently negotiated resource and synchronization
contract. CPU transfers may be measured; zero-copy must never be inferred.

Parent Session owns resource grants and root checkpoint/deadline. Known native
buffers/SDK owners are measured under that grant; arbitrary plugin heaps, OS/CRT,
thread bookkeeping and unmeasured SDK ownership remain explicit frontiers.
Operation timeouts protect recovery; they do not certify audio deadlines.

| Owner | Exclusive responsibility | Concurrent write exclusions |
| --- | --- | --- |
| Coordinator | Git, source recovery, Cargo/native execution, shared contracts, integration | All writes paused during source/artifact qualification leases |
| Audio slice agent | VST3 instance-local immutable `IPlugInterfaceSupport` policy, loader propagation and independent COM tests | No Audio DAG/Session/protocol/SDK rewrite |
| OpenFX slice agent | Cancelable discovery/describe, correct packaged helper, catalog worker/child settlement, owner-thread definition registration and controls | No Render Graph/pixel/suite-domain redesign |
| Independent verification agent | This record; Clack provenance metadata repair; read-only review of final slice patches | No runtime source changes or Cargo execution |

This round uses disjoint module ownership within the already isolated worktree;
it does not claim per-agent worktrees were created. Contract changes require the
coordinator to approve scope, dependent consumers, migration and regression gates.
Expensive builds are serialized by the coordinator.

```mermaid
flowchart TD
  A[Recovered source and immutable provenance] --> B[Metadata and baseline tests]
  B --> C[VST3 advertised-consumer policy]
  B --> D[OFX discovery lifecycle]
  C --> E[Independent patch review]
  D --> E
  E --> F[Targeted tests and fresh native fixtures]
  F --> G[Workspace regression and recorded slice commit]
  G --> H[Shared musical event contract]
  H --> I[VST3 and CLAP note routing / instruments]
  G --> J[OFX temporal / ROI / context contract]
  J --> K[Cache, Preview and Export integration]
  I --> L[Independent vendor and measured performance lab]
  K --> L
```

## Compatibility lab

The initial inventory is `.scratch/plugin-host-sprint/plugin-inventory.json`.
The installed Coda VST3 shell was found, but version/authorization/class inventory
and execution remain unverified. Duplicate package copies are one binary/class
identity, not extra test coverage. Owned fixture DLLs and the SDK Basic OFX sample
exist under the prior target directory; their historical hashes do not prove
fresh matching build selection.

The following 24 planned objects prioritize different boundaries. They are a
candidate queue, not 24 installed or compatible plugins. Owned/SDK objects remain
separate from independent vendor counts. Each format build and plugin class must
be inventoried before execution; external candidates are not silently installed.

| Object | Kind | Primary purpose / entry dependency |
| --- | --- | --- |
| CLAP sample gain | Owned fixture | Scalar events and range/partition PCM |
| CLAP sidechain | Owned fixture | Auxiliary mapping, delay, state and partitions |
| CLAP output probe | Owned fixture | Bounded output parameter/event observations |
| CLAP process author | Owned fixture | Native-origin author transaction closure |
| VST3 sidechain | Owned fixture | Bus mapping, state/delay and partition PCM |
| VST3 editor | Owned fixture | Same-object HWND and first-signal state |
| Stateful VST3 gain | Owned fixture | Component/controller state round-trip |
| VST3 Gain bundle | Reference fixture | Bundle resolution and repeated lifecycle |
| OpenFX SDK Basic | Official sample | Declared Filter processing and image law |
| OpenFX contract fixtures | Owned fixture family | Error/ownership/callback/teardown boundaries; individual IDs must be recorded |
| Coda VST3 shell | Installed independent candidate | Shell classes, discovery and permitted state/GUI execution |
| Surge XT instrument, VST3 | Independent candidate | Requires musical events and instrument output routing first |
| Surge XT instrument, CLAP | Independent candidate | Note lifecycle and polyphonic modulation after shared contract |
| Surge XT Effects, VST3 | Independent candidate | Effect chain, parameters/state and GUI |
| Surge XT Effects, CLAP | Independent candidate | Adapter comparison, parameters/state and GUI |
| Chow Tape Model | Independent candidate | Nonlinear PCM, state and automation; inspect actual format build |
| BYOD | Independent candidate | Complex state and processing graph; VST3/CLAP artifact checks |
| DPF example effect | Independent framework reference | Second implementation stack; choose actual built descriptor |
| DPF example instrument | Independent framework reference | Zero-input/event admission after shared contract |
| openfx-misc Blur | Independent candidate | Spatial/ROI/tiles and exact image comparison |
| openfx-misc ColorCorrect | Independent candidate | Float HDR/negative/alpha and colour contract |
| openfx-misc TimeBlur | Independent candidate | Temporal image requests and caching |
| openfx-misc Merge | Independent candidate | Required multi-input General context |
| openfx-misc Transform | Independent candidate | Bounds/ROI/PAR/multiresolution and interaction |

Primary project sources checked for candidate selection:
[Surge](https://github.com/surge-synthesizer/surge),
[Surge CLAP capabilities](https://surge-synthesizer.github.io/clap/),
[ChowDSP products](https://chowdsp.com/products.html),
[DPF format build targets](https://github.com/DISTRHO/DPF/blob/main/cmake/DPF-plugin.cmake),
[openfx-misc](https://github.com/NatronGitHub/openfx-misc).
These sources establish candidate projects, not local binaries or compatibility.
Choose immutable release/commit/license inputs when provisioning; no third-party
core code or license constraint is imported by this candidate list.

Every result records binary/class/version/SDK, source and artifact hashes, license
and activation, OS/CPU/GPU, input/parameters/rate/block/pixel format, selectors,
actual output, repetitions, failure/cleanup and performance. Native tests use
matching fresh helper/fixture artifacts, run serially and explicitly select
ignored cases. Ordinary unit success does not execute these cases.

## Acceptance gates and priority risks

Each slice needs a reviewable diff, independent semantic oracle, matching build,
targeted regression and explicit unresolved boundary. Preserve RED failures;
do not refresh identity metadata until each source delta has independent evidence.
Before integration, test cross-module lifecycle, state and Preview/Export contracts.
Run periodic workspace fmt, strict Clippy and all-feature tests; ignored cases
remain separately reported. Finish a verified slice with an auditable commit.

Highest priority risks are truthful capability advertisement, OFX discovery
children/dispatch, incomplete native author/reprepare installation, and absent
musical event routing. Follow with GUI thread/lifetime controls and callback
allocation/IPC deadline measurements. Existing zero-feedback budget controls do
not certify the known nonzero-feedback capacity-growth frontier. GPU Mix only
admits its documented Coverage/Opaque targets; Float alpha refusal remains.

Historical CPU HD/UHD Mix misses frame deadlines. Cold GPU pipeline creation is
material; 16/64-size eight-sample costs do not qualify full-frame playback/export.
Audio MMCSS admission is not callback deadline evidence. Measure actual whole-root
DSP, IPC, preflight, handoff and copies at declared sample rates/blocks with
p50/p95/p99/max and misses. Compare matched plugin/input/hardware/quality against
the restored baseline and a permitted mature host; a 10% target is provisional,
not a universal certification line.

Windows source and owned native evidence do not qualify Linux/macOS. The existing
environment has FFmpeg 7/libavcodec 61; release guard requires FFmpeg 8/62 and must
remain enforced. OCIO native prefix and ATL/pinned DXC downloads are build
prerequisites, not product semantic fallbacks. Physical DAC, full modal/save/reopen
workflows, whole-App RSS and commercial activation remain separate evidence needs.

## Recoverable 30-day cadence

Day 1–3: recover/audit real sources, reconcile provenance, baseline tests, freeze
shared contracts and complete independent lifecycle/capability slices. Day 4–12:
prioritize shared event/transport contract, audio notes/instruments/buses/state and
native author/reprepare closure. Day 13–21: contexts, temporal images/ROI/tiles and
Preview/Export/cache integration; independent OFX work may start earlier. Day
22–26: permitted vendor corpus, repeated lifecycle, platform verification and
measured bottlenecks. Day 27–30: stop discretionary feature expansion, regressions,
risks, evidence and final five-state matrix.

These are engineering phases, not a claim of continuous unattended execution.
Each resumed cycle reads this record, receipts, Git status and the last accepted
slice before selecting the next dependency-ready task. No recurring automation
has been created by this document. Coordinator updates evidence/status when a
gate actually finishes; pending work must never become `Verified` by elapsed time.

## Independent review record

The initial Clack recovery RED is independently explained by sealed source
preimages, not a new semantic source delta. `clack-provenance-repair/` retains
source/destination RED, both old metadata files, sealed GUI/document/manifest
witnesses and a GREEN receipt. GUI preimage SHA-256 is
`58c4e6de6964c2021a5a464df2577ab992b5026ced04b7e80c9eed4de20935b3`;
current unchanged runtime SHA-256 is
`3e240c8241083251efc521bb00f7186b2dbdc650f4dfe716fcb423fbf3652efb`.
The diff has only three whitespace line-wrap changes. Current document hash
`bc1801148ce69b8d375fd07c00ded9fa248f242c98499e3b46e41918855e5158`
matches the sealed package manifest. Note-port hash
`306ac1f8042b2d42d6a138b9ef3827b8be4cc11def6d530f6a7d03cca613d20f`
already matched its declared source delta. The immutable input receipt/tree and
checker hash are recorded unchanged in GREEN. The initial repaired format receipt is
`00a7eebb54007548132996a6ec7320b31aaac7262bc9e9a26b613086b946cf83`.
Preserving baseline LF yields final raw receipt hash
`616b67d39bbd90ef55eacfc6a577a07959e035084e452097cbf1df241ed0ec1f`;
independent JSON-leaf review confirms the same approved semantic changes.

The first read-only lab-runner review found two pending issues. Its parent
`WaitForExit` deadline did not bound subsequent asynchronous output drain when
an already-exited parent leaves inherited pipe handles in a child. Parent exit
and complete output drain require separate bounded outcomes; killing a live
parent tree does not prove normal-exit child settlement. Its final manifest hash
also reread the mutable input file rather than hashing the frozen evidence copy.
The controller now delegates to the existing validation launcher's native Job
owner: suspended spawn, assignment before resume, kernel active-process counts,
termination even after parent exit, and bounded cleanup. Logs are create-only
files. Frozen manifest bytes define the receipt digest. Independent follow-up
also found and corrected late-exit acceptance and duplicate group overwrite.
The matching `lab-frozen-r2/receipt.json` passes three groups/seven tests with
1,771 source hashes stable. `lab-negative-controls-r2.json` records actual refusal
of wrong counts/hashes, duplicate IDs, a deadline and a parent that exits with a
live child. The last two include actual native Job settlement evidence. These
controls qualify Windows test supervision; they do not establish full native
plugin compatibility, arbitrary OS failure recovery or real-time performance.

A later audit found two indefinite waits in the PowerShell fallback for a hung
runner. The follow-up removes them, uses one bounded observation budget, preserves
errors and requires the native Job receipt for descendant settlement. Killing
the runner does not prove tree settlement or impose a hard bound on the operating
system's kill operation. Actual `controller-followup-r3/summary.json` passes
nine positive controls (four process-owner controls and five COM controls) and
rejects wrong counts, a live descendant, a deadline and a deliberately hung
outer runner. The last case failed after 11,088 ms, observed runner exit during
seven ms of cleanup and retained the missing native-owner receipt as a failure.
Independent review checked receipt hashes and raw native flags. These results
qualify this finite Windows supervision behavior, not a hard OS kill deadline.

VST3's five real COM/policy controls passed after the original declaration RED.
OpenFX's four direct-child controls passed after the original cancellation RED.
Independent review also found and corrected a missing checkpoint argument in
the resident preparation caller; the first workspace Clippy run was canceled
before product checking to preserve a single-source build. This is a canceled
gate, not a passing check or an executed compiler failure. App/native Basic and
active catalog shutdown still require the integration gate.

## Current integration and next contract

The second and third strict workspace Clippy attempts stopped in the external
DXC build script: system curl could not acquire Schannel credentials. They did
not diagnose product code and are not passing gates. An actual Rust 1.97.1
resolution probe confirmed that the build executable directory precedes the
system directory when no child PATH is explicitly supplied. A task-local curl
launcher forwards literal arguments to installed Git OpenSSL curl. The live
release API still reports an immutable release. For the one exact archive URL,
an explicit cache binding copies the actual existing 26,115,841-byte archive;
the unchanged build script verifies its SHA-256
`2db15ab14e6eee9b9931d75d1690b4f7d81cad446fecccfe4e36e218dee25cce`
before extraction/linking. This round did not download a new complete archive.
The slow renderer attempt was canceled before the cache-backed successful check.
Registry sources and upstream checks were not patched. Task-local launcher,
probe, cache preflight and independent review receipts remain in `.scratch`.

The fourth strict Clippy attempt exposed an omitted resident `std::thread`
import and a test's invalid `PropertyBag::new` call; both were repaired without
changing their contracts or assertions. The fifth rejected the deliberately
orphaning negative fixture. A scoped `expect(clippy::zombie_processes)` documents
that exact control's external Job owner and retains strict enforcement for the
rest of the code. The sixth workspace attempt passed. The owner suite then
passed again: 12 tests, zero failures, four explicitly ignored fixture functions.
Neither compilation nor these supervision controls qualify native plugin output.

The OpenFX fixture now has explicit test-only modes that block inside actual
`OfxSetHost` or `Describe`, after validating host capabilities and recording a
native PID. The Windows AppUiHost tests dispatch the real install action, retain
the native child process and catalog worker thread handles, and invoke the real
Host destructor once. Both objects must signal exit; registry revision and an
isolated preferences file must remain unchanged. A five-second Drop gate is
below the original discovery/describe timeouts and distinguishes cancellation
from merely waiting for them. It is a broad integration gate, not a real-time UI
guarantee. Failed tests require the external lab Job owner; cleanup cannot turn
a failure into a pass. Fresh native controls passed: first-run discovery and
describe Host Drop took 19 and 22 ms respectively, with actual process/thread
handles signaled and no registry/preferences publication. Repeated frozen gates
also passed. These observations do not establish a universal latency bound.

App/native build r2 completed successfully in 13m05s. The first native execution
passed 12 of 15 selected groups before an inherited NaN test expected the wrong
error variant. A diagnostic-only rerun retained that failing assertion and
captured `Frame(InvalidSample { index: 7, alpha: StraightFloat })`. Independent
review proved the canonical validator and error variant unchanged from recovery.
The test now checks that exact typed refusal; no production validator changed.
Native execution r2 passed 13 groups, including that strict NaN control, then
exposed a real resident lifecycle defect: admission returned `Canceled` without
retiring the instance, so the following render still succeeded. The original
retirement assertion remains; the local fix retires on admission cancellation or
deadline and adds an equivalent deadline regression. Invalid authored frame
errors, output resource refusal and user-blocked semantics retain their existing
handling. Native execution r3 passes all 15 groups across eight exact selectors,
with 1,773 frozen source hashes stable and every Job report proving zero exit,
no errors or outliving descendants and complete settlement. Independent
acceptance is `ofx-native-r3-independent-acceptance.json`. Both failed native
runs remain preserved under `.scratch/plugin-host-sprint`. Strict Clippy must be
rerun after the new admission change.

The ordinary App library regression first exceeded a 15-minute serial budget.
The second run completed in 691.70 seconds with 2,836 passing tests, four failures
and 121 ignored tests. All four failures were the new direct-child supervision
controls; the Job reported exit 101, no deadline, no errors and complete cleanup.
The same App executable starts both ignored child fixtures correctly when launched
directly. A minimal Rust reproduction subsequently proved that the inherited
Windows environment contains different `PATH` and `Path` values: rebuilding that
environment for a child folds the keys and loses the prepended FFmpeg DLL path.
Changing output redirection or process creation flags did not resolve it.
Normalizing the outer launch environment with `ProcessStartInfo.Environment`
then passes all four original controls in 0.95 seconds, without changing source,
assertions or their five-second readiness budget. A frozen lab repeat also passes
four controls with zero ignored tests. This finite result does not substitute for
a fresh complete App regression. Raw RED, environment folding and GREEN evidence
are retained in `app-child-entry-diagnosis-r1` and
`ofx-child-supervision-frozen-r2`. Separately, 18 ordinary OFX module tests pass;
their selectors did not include these four new child controls.

Fresh audio fixture compilation exposed a non-Send error returned from a scoped
CLAP background log thread. The fixture now transports an owned error string and
converts it back on the caller; SDK and host threading remain unchanged. Both
native fixture builds and the audio library build completed. The first selected
CLAP native PCM lifecycle gate nevertheless fails because `start_processing`
returns false. The original predicate required audio callbacks to also be main
callbacks. A complete diagnostic record now proves the first failure was
`start_processing`: main=false, audio=true, prior validity=true and actual DLL
ThreadId(1) -> ThreadId(3). The initial teardown-only publication produced empty
files; a later run also failed earlier at bounded validation shutdown. Both
remain preserved. Temporary fault-path file instrumentation was used only for
diagnosis and has been completely removed from the current fixture.

The corrected fixture checks main identity against its own initialization
ThreadId, accepts audio on either physical thread and accepts main or audio for
Tail. All original PCM/state/bus algorithms and sticky refusal remain. Its one
table test covers 54 role/identity assertions and passes. Native execution r2
then passes four groups: CLAP and VST3 PCM lifecycle and signed signal time. The
fifth VST3 object-binding group fails because it assumes the very first Enter
reloads the initialized object, whereas the worker explicitly preserves that
object on first entry. The second round is unexecuted. A separate CLAP native log
gate fails an assertion that object observation is unavailable, although the
current adapter returns an actual object. These tests require contract review
and fresh strict replacements, not removal of their binding/log/PCM assertions.
The complete audio matrix remains unqualified. Diagnostic builds and these
functional timing observations are excluded from realtime/performance acceptance.

Independent review also confirmed a separate retained risk:
`ProcessOwner::terminate` ignores a kill error before waiting unconditionally.
An OS refusal while the child stays alive can therefore strand a resident
caller. The external lab Job bounds test recovery, not the product method.
This requires its own settlement slice and is not qualified by successful
normal-OS cancellation tests. No full production lifecycle claim is made.

The next audio contract is canonical Note routing, followed by a real input-free
instrument graph role. A native zero-input admission change alone would still
inherit the current main-PCM contribution and its latency and cannot qualify an
instrument. Note author data must remain in the sole Sequence AudioProgram,
with exact TimelineTime, target instance and definition-owned note-input identity;
compiled/prepared sample schedules are derived execution state. Event capacity,
ordering, continuity epoch, seek/preroll and releasing voices need explicit
admission and independent PCM oracles before adapters are expanded.

CLAP second-based Note routing can be verified as a first finite slice. VST3's
event PPQ position has no unavailable flag and requires a real musical projection;
the standalone vendor Timeline or an invented 120 BPM must not become product
authority. VST3 separate parameter queues and event lists also cannot promise
CLAP's cross-kind FIFO. The common contract can preserve same-sample application
and stable order within each stream. No project schema, musical map or shared
event API change is approved by this investigation alone. The detailed coordinated
change request is `.scratch/plugin-host-sprint/note-routing-contract-proposal.md`.

## Integration checkpoint: accepted slices and outstanding native shutdown

Accepted commits are `ae00fa2` (exact typed NaN refusal assertion), `04ae31a`
(catalog cancellation and worker settlement), and `c4debc6` (resident admission
cancellation/deadline retires the instance). The normalized full App regression
now passes **2,840 / 0 failed / 121 ignored**, in 726.87 seconds. Its source,
artifact and normalized Windows environment binding are preserved in
`.scratch/plugin-host-sprint/app-lib-regression-r3`. The earlier four failures
and serial timeout remain recorded; environment normalization is part of the
lab launch, not a product compatibility workaround. Workspace strict Clippy r7
and audio strict Clippy r8 both pass.

Two stricter audio test corrections are independently reviewed:
VST3 first Enter retains the initial object and second Enter must replace it;
CLAP log observations retain the actual initial native object identity. All
original PCM, stale-binding and complete log assertions remain. Eight earlier
finite PCM/signal-time groups passed across two rounds. The fresh r3 matrix
passes seven of ten groups, including the corrected VST3 object test, before
CLAP factory validation shutdown fails again. The corrected CLAP log gate also
fails at factory creation. Neither run qualifies the complete matrix. Temporary
parent-libtest diagnostics preserve the original shutdown budgets and observe
ACK versus process exit; diagnosis and any lifecycle contract change are pending.

The finite fixture and test-oracle slices are subsequently committed as
`b882d53` and `70522b3`; temporary supervisor diagnostics are excluded. Eight
serial diagnostic groups pass, with 56 normal shutdown records and actual exit
observed in 57.634–79.390 ms. A separate two-stream native stress run completes
18 groups (17 passing, one failing during factory validation). Three complete
records show `Completed` ACK in approximately 0.3–0.4 ms followed by the original
100 ms post-ACK exit deadline and forced cleanup. The failed factory's Job still
settles its process tree. Passing groups can contain failing Drop shutdowns and
are not graceful-lifecycle acceptance. The child subphase and natural exit time
after the original cap are unknown because forced termination interrupts them.
A second diagnostic-only shadow observation retains the original failure
verdict, delays cleanup by at most an extra second and records natural exit
separately; it is not a production budget change or realtime qualification.

Real third-party Airwindows Consolidated **1.2026.285**, publisher commit
`8a67b80e04230a5cf40e84d6e8b9a2fd70b13a75`, completes two List and two Probe
runs for each of CLAP and VST3 on Windows x64. All eight native Job reports
have zero exit, no deadline/errors/outliving descendants and complete settlement.
Each format reports 14 parameters, latency zero and tail 96,000 samples. Opaque
default state capture is 314 bytes for CLAP and 373 bytes for VST3. Evidence is
`.scratch/plugin-host-sprint/airwindows-native-discovery-r1/receipt.json`
(SHA256 `5C9216E16338D49C14B0A21B4A7E820B51612736F6AF996BF03ACCAF951902E9`).
Only these finite discovery/initialization/capture cases are verified. PCM,
state restore, GUI, project integration, realtime safety, comparative performance
and complete format compatibility remain unverified. The package is a private
external lab input; its complete binary license composition remains unverified.

The OFX kill-refusal fix is implemented only in an isolated worktree and awaits
independent review, fresh builds and seven Job-contained native controls. It
separates Running / Retiring / Settled, retains the Child for cleanup retry, and
prohibits wait after an unconfirmed kill refusal. No native result is yet claimed.

## Current checkpoint: terminal reader and process retirement

The temporary parent Shutdown diagnostics were removed before freeze19. The
original 100 ms acknowledgement/exit policy remains. Two diagnostic shadow runs
observed four children naturally exiting with code zero at approximately
171–184 ms after the unchanged original failure verdict. These are diagnostic
observations, not altered production acceptance, realtime or performance results.

An independent actual TCP regression demonstrates a separate terminal-reader
bug: after acknowledged Shutdown, the original reader attempts another decode
while the connection remains open. The original behavior fails the new test;
the corrected reader returns after terminal acknowledgement and all eight reader
module controls pass. The scoped-thread cleanup guard closes slots and socket
before join on assertion/unwind paths. It proves loop completion, not an OS
thread-exit deadline, and does not explain every observed slow teardown.
Evidence and independent review are in `native-reader-terminal-r1` and
`native-reader-terminal-independent-review.json` under the sprint scratch store.

The OFX owner slice is now integrated. Its private Running / Retiring / Settled
state revokes admission before cleanup, never waits after an unconfirmed kill
refusal, and retains the same Child for retry. Freeze19 fresh App build r5 passes
15 original native OFX groups and seven new Job-contained retirement controls.
Independent source/producer/artifact/Job acceptance is recorded in
`ofx-freeze19-normal-and-retirement-independent-review.json`. The affected
ordinary OFX modules also pass 18 tests and the child supervision module passes
four controls. Kill-refusal controls inject errors around actual children; they
do not prove an actual OS ACL refusal. Successful OS kill/wait latency is not
universally bounded; permanent refusal can leave a live process after finite
Drop attempts. GPU interop and complete OFX compatibility remain unqualified.

Freeze19 Audio build r6 passes the full ten selected native audio groups and two
strict CLAP log groups. These retain PCM, bus/state, signed signal time, actual
object identity, complete callback logs and the original factory-shutdown gate.
Independent ten-group receipt is
`audio-freeze19-ten-gates-independent-review.json`. Passing processing groups
still cannot prove each ignored Drop shutdown returned true.

The ordinary audio regression on the same build is **RED: 327 passed / five
failed / zero ignored / 50 explicitly skipped**, in 20.18 seconds. The first
paused GUI test observes Active=1 and AcceptedEvents=0 instead of three; four
later tests fail from its poisoned fixture serialization mutex. An isolated
repeat passes without changes. Read-only review identifies a valid completion
race in the embedded fixture: the audio callback publishes ACTIVE_FLUSHES before
sending events, and main inspects the later event counter immediately. This
embedded path does not use the native reader or external worker. The correction
publishes that existing SeqCst count after exchange, preserving all event, PCM,
thread, Process-count and two-second assertions. Original RED and independent
source-bound review remain in `audio-ordinary-execution-r6` and
`clap-paused-flush-race-review-r1`. Fresh full regression is pending.

Strict workspace Clippy r9 refuses one nested-if warning in the new OFX fixture;
a semantically equivalent let-chain resolves it. Fresh freeze21 Clippy r10
passes all targets/features with `-D warnings` in 1m38s. Historical App full
2,840-pass regression predates these slices and must not be reported as a fresh
full regression of the current tree.

The audio process-owner r2 and generic external-vendor PCM/state test slices
remain isolated, independently reviewable deliveries. Neither has root build
or native qualification yet. The vendor test uses opaque captured default state,
canonical author parameters and plugin-owned Bypass for bit-exact PCM, with a
non-bypass discrimination control; default state alone cannot prove nondefault
state restoration. Upcoming admission must preserve the existing factory
shutdown failure rather than bypassing it. Current fresh binary bytes are saved
in `native-artifact-archive-r6` before further rebuilds.

Freeze21 Audio build r7 subsequently completes the full ordinary regression:
**332 passed / zero failed / zero ignored / 50 explicitly filtered**, 30.44s.
The fresh Job exits zero with no deadline/errors/outliving descendants and full
settlement. `audio-ordinary-execution-r7/receipt.json` binds the exact successful
summary, 1,773 source hashes and current Cargo-produced audio executable.
This supersedes the ordinary-test pending status above; the original r6 RED
remains. It does not execute the 50 native/installed ignored cases or qualify the
isolated audio owner/vendor additions.


The three accepted slices are committed independently as `afa6614` (terminal
reader), `9cff2bf` (OFX owner) and `586acde` (fixture completion). Subsequent root
integration changes audio supervisor/Cargo and adds the reviewed external vendor
test module; an offline metadata resolution adds only the existing workspace
tracing dependency to mondrian-audio's Cargo.lock entry. Freeze22 explicitly
includes the new source file (1,774 inputs). Strict build and all new native
qualification remain pending. Freeze21 gates are historical accepted artifacts,
not a claim that this newer tree matches their hashes. Exact existing App and
Audio executables were archived before rebuilding.

## Latest checkpoint: Audio owner and external vendor admission

Freeze23 contains 1,774 selected repository inputs (SHA256
`97A8F42DD3BA581EDD4291F6BFF8E6139C9026360DAF2079E22DF8DC1492CB69`).
Fresh default-feature Audio build r8 and App build r6 succeed. Strict workspace
Clippy r12 passes all targets/features with `-D warnings` in 1m40s. These replace
the build-pending status above; they do not certify plugin compatibility.

The integrated audio owner has private Running / Retiring / Settled states and
a startup guard immediately after spawn. It revokes admission before cleanup,
preserves the primary startup error, retains the same Child after unconfirmed
kill refusal, and forbids wait on that refusal path. Eleven actual Windows
Job-contained controls pass and have independent source/artifact/output review:
`audio-owner-native-eleven-independent-review-r1.json` (SHA256
`069C3D5CBC2D15721F2D67A7C791D946414ED6678CA21FE9F4BE2D51EFD8A6E3`).
Those controls inject operation errors around actual children; they do not
establish actual OS ACL refusal, bounded successful kill/wait, whole-tree product
containment, real SDK teardown, or realtime safety. Permanent kill refusal may
leave a live process after finite Drop attempts. The Running shutdown policy
and existing native factory-shutdown assertion remain unchanged.

Fresh `audio-native-execution-r7/summary.json` records **10/10 original native
audio groups passing** on this owner, across two rounds of CLAP/VST3 PCM,
signal/time and actual VST3 object lifecycle controls. Independent review is in
`audio-freeze23-ten-native-gates-independent-review-r1.json` (SHA256
`3302EFF8B9A1343B300873D6EF66044E05EA8BD739645B7A8EF858D4BABB7217`).
Current ordinary Audio subsequently passes **332 / zero failed / zero ignored /
64 explicitly filtered**, 30.38s, with all actual test names matching the derived
ordinary set. Independent receipt
`source23-audio-owner-ordinary-and-inventory-independent-acceptance-r1.json`
has SHA256 `650174277E49458F3FBEF3259EF16A57AE22A045B2561940219FF1F1BE273C47`.
The owner slice is committed as `9ebc35c`. The generic external lab tests are
separately committed as `21f1827`, retaining actual vendor RED results. Full
current App regression is pending.

The generic external-vendor test uses actual opaque state capture, canonical
author parameters, native discovery/resolve, plugin Bypass and asymmetric PCM
with multiple block partitions. Its current four-case plan binds exact Airwindows
binaries and fresh product helper. Actual first CLAP execution is **RED: zero
passed / one failed**, before DSP: the resolver reports
`authored CLAP parameter contract differs from the plugin`. The Job exits 101
without deadline, owner errors or outliving
descendants; the failure is preserved under
`external-vendor-native-frozen-r1/round-1-clap-controller-evidence`.

The independent first VST3 case subsequently reaches factory validation and a
fresh native instance, but is **RED before PCM** at state entry:
`ReprepareRequired(Vst3Restart { flags: 4 })`. Current bindings identify this as
`kParamValuesChanged`. Its native Job also settles with exit 101 and no deadline,
errors or outliving descendants. Evidence is in the same generation directory's
`round-1-vst3-controller-evidence`; both second-round cases remain unexecuted.
The required parameter-authority/reprepare integration is under investigation;
the flag has not been ignored or reclassified as a successful entry.

The saved initial and bypass author snapshots differ only in Bypass automation
constant, zero to one; schema and opaque state are identical. Publisher source
contains an uninitialized default-value array with entries written only for
active algorithm parameters. This is a candidate explanation for fresh-instance
schema drift, **not yet confirmed by the actual failing descriptor**. Strict
schema admission remains intact. No plugin-name exception, default normalization,
fabricated state or relaxed output assertion has been introduced. Vendor PCM,
restore, GUI, performance and full compatibility are still unqualified.

Exact current Audio/App/OFX executable bytes are retained in
`native-artifact-archive-r8/receipt.json` before diagnostic rebuilds. These PE
archives do not establish the entire runtime DLL dependency closure. Raw logs,
plans, source hashes and independent reviews remain in the sprint scratch store;
this document is the Git-persisted coordination index.

Fresh App build r6 also passes all 15 original OFX native groups under the
unchanged controller (`ofx-native-execution-r7/summary.json`). Each actual exact
selector produces the required successful test summary and Job settlement.
No separate fresh selector-list preflight was executed in this run. The retained
old plan was correctly refused for stale source/marker bindings; a new plan only
rebinds the current source and fresh catalog marker paths. Independent review of
this new execution is recorded in
`ofx-freeze23-fifteen-native-independent-review-r1.json` (SHA256
`F9276459F19FE55E3E4F50CC0DC7EF9354E879FA66C8DA8271B2EBEBA8BFC4A3`).
Complete current App regression remains pending.

## Actual vendor diagnosis and next isolated slice

A temporary test-only same-refused-probe diagnostic was independently reviewed
and compiled as Audio build r9 / freeze24. The existing helper remains App6,
built from source23; it was not relabeled as a source24 product build. Actual
diagnostic attempts fail at different native gates: startup metadata admission,
discovery/restore, and then the existing strict restored-author assertion.
A third-case preparation error (missing required empty evidence directory) is
retained separately from its corrected continuation. No attempt passes PCM.

The corrected third continuation provides actual metadata drift: after one
successful restored author snapshot with default zero, a second fresh restore
reports parameter `2944917345` with default `4.5906537691281007e-41` instead of
`0.0`. The schema equality compares allocation contents, so differing diagnostic
allocation UUIDs do not explain the refusal. The test-only resolver diagnostics
did not print: this failure occurs earlier in the existing assertion. The value
is consistent with the publisher's uninitialized inactive default slot; complete
binary/source cause attribution remains an inference. Keep the strict refusal
and do not suppress inactive parameters or normalize the default.

Exact diagnostic executable bytes are archived in `native-artifact-archive-r9`.
Temporary diagnostics were removed and all 1,774 source23 hashes restored.
Cargo build r10 was **rejected as restoration evidence**: Copy-Item preserved an
older source timestamp, so Cargo reported fresh=true and reused diagnostic r9
bytes. Touching only the restored file timestamp forced an actual r11 compile
(fresh=false), producing SHA256
`8A7CE34E4E839533177A0A0365E1D7A205BA2765D2617741494F629FF40704AE`.
Original r10 logs/cache refusal and restoration proofs are retained. No restored
source claim relies on its stale executable.

Actual default App6 inventory contains 2,778 ordinary and 124 ignored tests;
historical 2,840/121 used the validation feature. Inventory alone executes no
tests. Bulk inventory tool r1 failed before child launch on a scalar --list JSON
argument; r2 corrects array serialization and provides the successful actual
inventory/Audio run. Tool r3 additionally guarantees cleanup attempts when
evidence publication fails; its syntax and extracted-finally fault controls pass.
These scratch tool changes do not alter production budgets or assertions.

VST3 restart flag4 has a documented initial control-consumer gap, distinct from
latency flag8. The existing latency service cannot consume it before first DSP.
Its reviewed CCR is in `vst3-param-values-restart-independent-ccr-r1`. A new
isolated worktree at `.scratch/agent-worktrees/vst3-initial-value-restart`
(base `9ebc35c`) owns only the VST3 adapter, narrow bounded controller-query
support and a dedicated SDK fixture. The accepted design supports pure4 only on
proven initial/fresh-reload control phases, preserves canonical author and
last-DSP authority, and retires only the exact observed cohort after all reads.
Runtime/mixed/newer/exhausted requests remain pending. Implementation, native
controls, independent review and actual vendor retest are still required before
integration. No shared domain, graph, schema or global time contract is changed.

## Restored executable regression and second corpus checkpoint

The actual source23 Audio r11 executable passes all 332 ordinary tests again,
with zero failures/ignored and 64 filtered native gates (26.60 seconds). Evidence
is `audio-ordinary-execution-r9/receipt.json`; actual four-Job inventory r3 is
`libtest-inventory-execution-r3/inventory-receipt.json` (SHA256
`1305CC6DD98881EAFC07B48EE9F498D05D5045B6E61953B529F68B5A115ECE6A`).
This binds the restored r11 producer and executable, rather than the rejected
fresh=true r10 build. Independent acceptance is pending. The current default
App6 ordinary regression is running in `app-lib-regression-r4`, against its
actual inventory of 2,778 ordinary and 124 ignored tests; no result is claimed
before its final output, source checks and native Job settlement.

The App r4 run subsequently completed successfully: 2,778 passed, zero failed,
124 ignored, zero filtered, 677.99 seconds. The runner and outer controller
exit codes are zero, native owner errors are empty, no deadline or outliving
descendants were reported, and descendants were reaped. All source checks passed
under 1,853 read leases. `app-lib-regression-r4/receipt.json` binds App6 SHA256
`68E8CB85254E5791F666247BD542AB45F3809B958C3863BC755E909799562D47` and source23;
ignored native tests were not executed by this ordinary regression. Independent
review is pending. Exact restored Audio11 bytes are now preserved separately in
`native-artifact-archive-r10`; accepted App6/product/OFX bytes remain in archive r8.

The isolated VST3 initial-value service is now frozen for review in
`vst3-initial-value-service-r1/delivery-receipt.json`. Its relative seed patch
SHA256 is `66944FF29064F29F0F6C02F0584EF25F3E9A3D1B6E2750B7A90DD45D16C99B6B`.
There are 15 owned paths, including five ignored actual SDK fixture controls.
Rustfmt parsing and pinned SDK provenance checks passed; no Cargo/native test
or root integration has happened for this slice.

The second real CLAP corpus is DuskAudio RingOut v1.0.0. Official ZIP SHA256 is
`9DEA8EC30F9F69362BF95F7FDD1108870F2982423953C9A46748C7E7DE8AC3E4`;
the extracted x64 CLAP SHA256 is
`DF86E10F2B0AADA4A3EB5F4F0BDA315A941960B587B4B9513CD7BA8A9F8BD99B`.
Static acceptance evidence is `second-clap-corpus-r1/static-candidate-acceptance.json`
(SHA256 `9041967F04688860B0B5EE62CB303CC7B57AE15894605048E2C91BB9892D7277`).
Only CLAP and actual license/notices were extracted; no installer or plugin
execution occurred. GPL-3.0-or-later plus ISC/MIT notices are recorded, while
complete binary composition remains unverified. Its 11 parameters include two
read-only meters, bypass smooths over 10 ms, and default processing can be
identity. These legitimate semantics make the existing first external PCM
slice ineligible. Preserve that gate and design separate read-only authoring,
controlled nondefault state and bypass-settling oracles before runtime claims.

## Initial VST3 value consumer candidate: actual first gates

Independent acceptance of the previous source23 ordinary Audio/App runs is now
recorded in `source23-audio-app-ordinary-independent-acceptance-r2.json` (SHA256
`ADE048B3048391F79BEF99D794DBAB3FF642C12E73E4A425F9AFE710B69D340E`).
Its 28 checks include exact executed names versus actual List inventories,
producer/profile identity and Job settlement. The source23 timestamped hash
snapshot was retained before later candidate writes.

The initial-value candidate is statically accepted in
`vst3-initial-value-service-independent-review-r2.json` (SHA256
`9ED4F81F4F0AD9D28644FCE8D2C96FEA22511A9239F3AF67C78475AB2FE03BFF`).
Root preserved LF in two existing files; other 13 snapshots remain identical.
Integration is a candidate, not an accepted feature commit. Freeze25 covers
1,785 inputs, including every owned fixture/license/provenance path; SHA256
`A03B96AB419323063AD2E80CD04B15D2CBE1D4D1D55606CCAF3ECD83BC83FAC7`.
Strict workspace Clippy r13 passed (2m11s), actual new SDK fixture build r1
passed, and Audio12/App7 builds passed with all 1,785 source read leases held.
Their `*-source-receipt.json` records producer/source checks. None alone proves
plugin compatibility.

Actual five fresh exact ignored List Jobs passed in
`vst3-initial-values-native-preflight-r1.json`. Actual positive SDK control1
passed: current values observed without setters/synthetic Process, followed by
expected PCM and sample-offset automation. Control2 is **RED**, preserved in
`vst3-initial-values-native-execution-r1-2`: Enter correctly returns the exact
pending restart request, but a subsequent direct Process assertion at line231
expects ReprepareRequired. The worker still has no accepted continuity anchor;
the existing signal path checks that anchor first. Independent diagnosis of this
new test's contract assumption is pending. No production error-order change or
test weakening has been made. Native owner exit101 is settled with no deadline,
outliving descendants or owner errors. Remaining controls, second round and the
external real VST3 retest have not executed; initial-only compatibility remains
Implemented — Not Fully Verified.

## Initial-value slice: source26 accepted SDK gates and external boundary

The source25 negative control exposed an incorrect direct-worker test assumption,
not a new production error-order defect. The existing signal contract refuses
Process without an accepted continuity anchor before inspecting restart flags.
Independent reports `vst3-initial-values-direct-process-contract-independent-review-r1.json`
and `vst3-initial-negative-test-source26-independent-review-r1.json` accepted the
test-only correction. Exact Enter refusal, pending cohort, author source,
query/setter counts, object identity and untouched PCM assertions remain; direct
Process now asserts the existing typed continuity refusal and retained state.
The original RED receipt and Audio12 executable remain preserved.

Freeze26 SHA256 is
`203C8D3777CB8E64A8919BEC4BCB85D0DD3BE6CB29EBBC487FD6E0C1FD005562`.
Only the new test changed from freeze25; the other 1,784 inputs are identical.
Strict workspace Clippy r14 passed. Actual Audio13/App8 builds passed under all
1,785 source read leases, with completed Cargo JSONL and source receipts.
App8 also compiled default integration targets; compilation does not qualify
their runtime, GPU, UI or release behavior.

Five fresh exact SDK List Jobs and all five controls in two rounds passed:
`vst3-initial-values-native-execution-r2/summary.json` records **10/10**.
Independent acceptance `vst3-initial-values-ten-native-independent-acceptance-r2.json`
SHA256 `824C854E3F42440A13D33F6EF9AD83DAF419F02D713FECE6E562C9EBCDCA1B0B`
checks actual producers, source/artifact bindings, exact selectors and settled
owners. This verifies the owned DLL's initial/fresh-only pure4 observation and
negative controls; runtime pure4, wider controllers, UI, real-time guarantees
and full third-party compatibility remain outside that scope.

Actual fresh inventory r4 contains Audio401 tests /69 ignored /332 ordinary,
and App2902 /124 ignored /2778 ordinary. Source26 ordinary Audio r10 passed
**332/0/0**. The original five native controls passed twice in
`audio-native-execution-r9/summary.json` (**10/10**). Their generator first
rejected a failed historical CLAP fixture producer before launch; regeneration
explicitly used the accepted CLAP r3 producer. That refusal is retained.
The previous full App ordinary run remains source23 evidence, not source26.

The unchanged strict Airwindows VST3 case now reaches actual first Enter and
DSP, and its native child exits0. It is still **RED**:
`external-vendor-native-vst3-initial-execution-r2-round1` records eight PCM bit
differences, all right-channel `-0.0` to `+0.0` in partition1. All nonzero samples
are bit-identical; partitions17/64, non-bypass discrimination and round2 were
not reached. Raw output/state/author/owner evidence and the bit analysis
`external-vst3-first-pcm-eight-differences-r1.json` are retained. The host's
numeric-zero silence optimization can canonicalize signed zero; pinned SDK
silence flags are optional and do not promise signed-zero preservation, so this
is not classified as a demonstrated SDK violation. A generic conservative
silence-detection fidelity change is under review; no normalization, plugin-name
special case, deleted assertion or compatibility PASS has been introduced.

Ring Out List/Probe preparation r1 remains unexecuted. Independent review found
whole-file hashing before response/owner size admission. A separate scratch r2
will repair that bounded-input guard before actual product discovery; Ring Out
PCM, state restore, authoring and GUI are still unverified.

Source26 standalone `vst3-host` no-default-feature ordinary regression passed
**269/0/0**, after actual fresh all/ignored inventories under native Job owners:
`vst3-vendor-ordinary-execution-r1/ordinary/receipt.json`. Build r2 used the
actual standalone manifest and all 1,785 source read leases. A preceding script
derivation accidentally retained the App command and was interrupted/rejected;
`vst3-vendor-build-r1-rejection.json` records it, and no vendor qualification is
derived from that output.

Ring Out staged preparation r2 received independent acceptance
`ring-out-discovery-plan-r2-independent-review-r1.json`, SHA256
`5CF44529CA7543F5C504CFC431A75288D48E7792FA77D0CC46FB3D1455774B85`.
Root then executed **two List and two Probe Jobs**, all passing with actual
App8/source26 and the accepted runner. The actual sole identity is
`com.duskaudio.ring-out`; discovery version is null and package version remains
the separately evidenced1.0.0. Probe retains all 11 parameters, including the
two read-only meters, and captured184 bytes of default opaque state. Round1
state SHA256 is `1CC8E19194558D2B72358549FDF9516FB41631A58B1D784EB95E70036095B7C1`.
Raw responses, states and owner receipts are in `ring-out-list-native-r1` and
`ring-out-probe-native-r1`; independent actual-run review is pending. This is
Verified only for those discovery/default-capture controls, not state restore,
PCM, bypass settling, project workflows, GUI, performance or full compatibility.

The finite initial-value slice's integrated gates received independent acceptance
in `initial-values-integrated-gates-independent-acceptance-r1.json`, SHA256
`144E44D8AC0ED78ECB8BE8C060A7A31EF43AD774FCAC71527308D78C3B074180`.
It binds1,913 evidence/source files, the unchanged source26, exact332 ordinary
Audio names,269 vendor names, both native control groups, and the earlier SDK
acceptance. The actual Airwindows signed-zero RED is explicitly retained; this
acceptance permits the limited initial/fresh-object slice commit and does not
qualify third-party compatibility or source26 full-App runtime regression.

Ring Out's four actual discovery/default-capture Jobs subsequently received
independent limited acceptance in `ring-out-native-independent-final-receipt-r2.json`
(SHA256 `49B783932E2A1389CDEAA40F2023E3FBB8DF40FC0D58B8798308D94AED2EF3A4`).
Both raw List responses and both raw Probe responses are identical; both opaque
states have the same184-byte length/hash. All four native owners exit0, report
no deadline/errors/outliving descendants and report descendants reaped. The
review binds the earlier exact source26 timestamped snapshot; subsequent
source27/28 buffer-test/policy edits are explicitly outside that qualification.
Root-observed controller exit0 returns are recorded separately from the
independently rehashed native owner proof. State restore and PCM remain untested.

The signed-zero regression is now experimentally established. Source27 adds
only two owned buffer tests. `vst3-signed-zero-red-execution-r1` records
**270 PASS /1 FAIL**, with flags7 versus expected1 on the preceding numeric-zero
policy; native owner exit101 is fully settled. The RED executable is retained in
archive r12. Source28 changes the same private scan's predicate to positive-zero
bits in all four f32/f64 input/output consumers, preserving native-declared
flags and existing missing/inactive behavior. Both new tests and the original
269 pass (**271/0/0**) in `vst3-signed-zero-green-execution-r1`. This demonstrates
the owned buffer fidelity policy, not full third-party or performance acceptance.
Audio/product rebuilds and unchanged real-vendor PCM regression are pending.

## Source28 signed-zero native acceptance and resource checkpoint

The actual source28 App9 product build completed in22m15s with exit0 and
all1,785 source read leases held. Its actual compiler-artifact is the Windows
`mondrian` binary, features[], test=false, fresh=false, SHA256
`2A60BF23C236F9BC0F9B9520D237057165D153D96F788DEC80066E399A77DB56`.
`app-native-build-r9-source-receipt.json` binds the actual producer and frozen
source28. Audio14 ordinary regression passes332/0/0 against fresh actual
all401/ignored69 Job inventories in `audio-ordinary-execution-r11`.
Workspace strict Clippy15 passes all targets/all features with `-D warnings`,
exit0 and all1,785 source read leases; its scope is the workspace check.

Two actual Airwindows VST3 Jobs now pass the unchanged strict oracle:
`external-vendor-native-signed-zero-execution-r1-round1` and `-round2`.
Each executes4 actual processing instances. The six bypass outputs for
partitions1/17/64 are all exactly32,768 input bytes, including8 negative zeros,
SHA256 `0C54BF849A05BEA1F80494BA50ADDD3828C31736DA3C179A5CC735CB2450BB2`.
Both non-bypass64 outputs are finite and differ in all8,192 sample bit patterns.
Actual restored default opaque state, author/native-bypass distinctions,
instance serials and observed post-Drop child exits are retained. Both outer
Jobs exit0 without deadlines, errors or outliving descendants; all8 processing
children have observed normal exit0. Independent limited acceptance is
`airwindows-vst3-signed-zero-two-rounds-independent-acceptance-r1.json`, SHA256
`4CE1D486422F6C7C1B19C67487164A51B80AC51307EDF2457C6FE3859AADD587`.
This verifies only Windows48kHz/stereo/offline/default-state processing under
these inputs. It does not qualify GUI, realtime safety, project reload,
nondefault-state discrimination, graceful Shutdown ACK, performance or full
standard compatibility. Historical RED evidence remains intact.

An extra standalone `vst3-host` lib/tests strict Clippy check fails with78
reported diagnostics in `vst3-vendor-strict-clippy-r1.log`; its exit101 receipt
is retained. Workspace Clippy success does not cover this separate check.
Independent diagnostic-to-baseline review maps all78 diagnostics to unchanged
source lines at0bd39c4:72 unnecessary casts,3 manual is-multiple-of checks,
one borrowed box, one missing transmute annotation and one vector initialization.
None originates in the new predicate or signed-zero tests. Mapping receipt
`vst3-vendor-strict78-independent-baseline-mapping-r1.json` has SHA256
`AF48B7807DFDF6296EC231F0912B0B7A537A843019825711C89344B48E55EB3B`.
This is static baseline attribution, not a claim of an actual baseline Clippy
execution. No warning suppressions, assertion deletions or broad vendor cleanup
have been made to force a PASS. The source28 SDK initial-value replay passes
10/10 and the original CLAP/VST3 native replay passes10/10 in
`vst3-initial-values-native-execution-r3` and `audio-native-execution-r10`.
Both use current Audio14/App9 and freeze28. Legacy selector admission verifies
exact membership in the earlier actual current all401/ignored69 Job inventories;
SDK admission executes five new exact selector Job Lists. Historical source26
results remain separately bounded.

The cold product/check builds reduced free disk space to about256MiB. Root
applied reversible NTFS LZX compression to21 historical executable copies and
the completed product debug-symbol file (two paths refer to the same stored
file). Before/after raw SHA256 agrees for every compressed file; no file was
deleted and no source or active executable was changed. Receipts are
`native-archive-lossless-compression-r1.json` and
`product-debug-symbol-lossless-compression-r1.json`; after debug-symbol
compression the measured free space is1,267,945,472 bytes. This is a storage
maintenance action, not a processing performance claim.

The isolated CLAP read-only metadata/default-load test slice received static
acceptance after removing an unused, unavailable Serialize derive; original
r1 remains rejected and immutable. Delivery r2 SHA256 is
`DF67F8F4D21EB812118AD31FA1710F6DFF695702BCB886FE335799C9026167A3`;
independent static review SHA256 is
`F3AB7A5ADF7B4F846F7BC54D144C3DC14636D1C5CBDA379F09E4DE2FB2109F70`.
It adds a separate lab gate while preserving the existing strict PCM gate.
Core CLAP already retains11 metadata descriptors and filters the two read-only
meters from the9 writable author values. No second parameter model is needed.
This candidate is not yet integrated, built or natively executed.

Surge XT1.3.4 remains a proposed instrument corpus member, not an acquired or
compatible plugin. Official checksum asset185292289 was obtained, with MD5
`d318e95ca79a5ee70ddafec04313a00c` for the49,093,875-byte Windowsx64 plugins-only
ZIP. Two finite whole-file attempts and two bounded range batches failed under
actual HTTPS timeout/reset conditions. Only about7.4MB of partial evidence is
retained; no complete archive, binary hash, package-license inventory, executable
extraction, installation or native result exists. Evidence is sealed in
`third-corpus-acquisition-r2` (delivery receipt SHA256
`F307D063039C89E933178936608E4F83400D657D94E240E513537A52A4EDAE77`).
Acquisition is Blocked by observed transfer reliability. Any retry must use
fresh finite receipts and complete length/official-MD5/local-SHA validation;
instrument admission requires musical events, bus and audio-output oracles.

## Next CLAP metadata/default-load slice (candidate)

The signed-zero slice is committed as `cb48b86`. Its comprehensive independent
acceptance is `signed-zero-source28-integrated-independent-acceptance-r1.json`,
SHA256 `3B21C997EB421A2A0F3F5B91C89F3A8FA7B9FA60CE8DEF7F3AD357131F5A5DF7`.
The separate source28 root-terminal observation supplement binds actual
controller returns without treating them as substitute native-owner proof.

Root then integrated the independent r2 CLAP test-only candidate. The existing
`external_vendor.rs` changes only by a three-line module declaration, retaining
all old PCM code. The new `external_vendor/readonly.rs` was formatted with
Rust2024 rustfmt; its current SHA256 is
`3F43305CD0CCCDE214322A08A2E6E2C57B910D9C6E5B94C9D9DC9F8660CA27A9`.
The original isolated r2 remains unchanged. Freeze29 binds1,786 sources, SHA256
`5431C3AAC636DBBF087C2876F56979A3AE4184D258D8FDCEB365DE967BDC9F0B`.
At that source29 capture, the four ordinary tests and single native ignored gate
had not yet executed and central Audio15 compilation was in progress. Its status
then was Implemented — Not Fully Verified; the accepted gates below supersede
that candidate state. No production CLAP interface changed.

The actual readonly native case/manifest preparation is assigned to the Audio
Agent in scratch, with fresh current compiler producers, the admitted original
Ring Out List/Probe/state and two bounded root-owned execution rounds. A source
file's mtime was invalidated with identical raw bytes to force an actual new
product build despite this being a test-only addition; that action is separately
recorded in `clap-readonly-product-force-rebuild-r1.json`. No old helper is
relabeled as a fresh current producer and no declared test count is acceptance.
Source29 centralized gates now pass: Audio15 actual fresh build exit0;
App10 actual fresh product build exit0 (5m36s), helper SHA256
`7B8FD326F0EC6E2033666FE1C7DB5F8D4695E21070CAAADEBD0FCAA48B9B5B05`;
actual all406/ignored70 native Job inventories admit exactly336 ordinary names,
all passing including the four new negative controls. Workspace strict Clippy16
passes all targets/all features `-D warnings` with all1,786 source read leases
held (1m42s). Existing standalone vendor strict RED debt is unchanged, not rerun
or suppressed by this test-only slice.

Independent format review confirms the r2 child changed only whitespace,
19 optional trailing commas and three return-false semicolons; the raw captured
JSON and all other protected tokens are retained. Its receipt SHA256 is
`232A81B96F5D25D4730232A319E8400C386A1F7450E0FDF618C84B6564E9D224`.
The data-only preparation packet has independent static acceptance SHA256
`C2A143C8DBF3225D1E16D468B4C5E9EDA5C56C831B51BB54974839536F1A516C`.
It generated actual current cases/manifests under `clap-readonly-native-frozen-r1`.
Its first attempt refused nonexistent historical PATH directories before any
output creation or native launch; explicit existing runtime directories then
passed without weakening the admission rule.

The new preflight r1 was never executed. Independent review found missing full
freeze-path admission and possible original-error masking during receipt write.
A separate scratch r2 reuses the existing full Lease-Freeze admission and
retains original errors while always releasing leases. It preserves the existing
30-second Job/40-second outer wait/two-second cleanup budgets and validates
bounded held-handle owner/stdout inputs before hashing. Exact r2 SHA256 is
`6E4010DAFDC5E5E82BFA48172B1A8D70BFB6B81CDE49459B9C555CBC95F9171C`.
Root executed its actual sole ignored-test List Job successfully; no List output
was counted as a positive test. The original scripts/old preflight readers were
not modified.

Root then executed two real Ring Out default-load Jobs, both passing exactly
one test/zero failed/zero ignored/405 filtered, with native owners exit0,
no deadline/errors/outliving descendants and descendants reaped. Evidence is
`clap-readonly-native-frozen-r1/round-{1,2}/controller-evidence` and
`test-evidence`. Actual default opaque load, serde reopen, nine writable author
values, full11-descriptor resolver payload and refusal of RO IDs9/10 are checked.
The184-byte opaque state agrees with the original admitted raw default state.
The runtime discovery version remains null. This establishes validation of
existing production CLAP behavior; the code addition itself is only a lab gate.
Raw test output does not certify its own outer Job. Independent final native
review accepts the three-file verification slice in
`clap-readonly-source29-two-rounds-independent-acceptance-r1.json`, SHA256
`DF9D77C07C3668FF0F49D47C16FA47B166EC88959EE5DC060C9C89E939A2C12E`.
It checks all1,786 source hashes before/after,1,820 generation bindings and
3,614 preflight bindings, both actual Jobs, raw state/author/native descriptors
and resolver payload. No PCM/DSP, nondefault-state discrimination,
project archive, actual author gestures/Undo, GUI, realtime, performance,
Preview/Export, instrument/events, other-platform or full-standard acceptance
is inferred. Root terminal observations are separately sealed in
`clap-readonly-root-controller-observations-r1.json`.

## Periodic App regression and next independent slices (in progress)

The source29 CLAP verification slice is committed as `1080db0`; the accepted
source28 signed-zero change remains `cb48b86`. Central App11 library-test
compilation actually completed with exit0 in7m25s, holding all1,786 source29
read leases. Its sole current Cargo libtest artifact has empty features,
`fresh=false`, and PE SHA256
`4C840D336FE8175F3C39B99EDB64166D283BAB9A5633126534D880A8AD50A501`.
Producer `app-native-build-r11.jsonl` SHA256 is
`DE911BB7681AA13199AC6291882B16E5A71812A8C4EEE93B90E760D311604F80`.
The old source23 App2778-pass result is historical evidence, not a source29
regression qualification.

Independent static review rejected the unexecuted generic ordinary-runner r3
for inherited unbounded tree cleanup, possible substring skips and insufficient
current producer binding. An App-only runner uses exact current package/lib/src,
producer/source receipt and full frozen-source leases with the existing bounded
native Job owner. Its early r3 attempt stopped before output/native launch on a
PowerShell null-extension filename error. The corrected r4 executed successful
all/ignored List Jobs (2,902/124), then stopped before the ordinary suite because
`viewer_gpu_output_budget_smoke` is a substring of the ordinary
`viewer_gpu_output_budget_smoke_report_serializes_health_report`. Both attempts
are retained; neither is an ordinary-suite pass or native-plugin failure.
The next candidate adds `--exact` and a third actual filtered List Job whose
entire name set must equal the2,778 ordinary names before executing them.
The installed Rust1.97.1 primary libtest source confirms exact matching also
applies to skip filters. Current full App regression remains pending.

The accepted r6 runner actually admitted three Lists, including an exact-skip
name set equal to all-minus-ignored, and then ran the entire ordinary suite.
Its first full result is retained RED:2,775 passed/3 failed/0 ignored/124 filtered
in656.94s. All three failures involve unavailable media CLI under the narrow
five-directory PATH reused from the Ring Lab. The packaging/tracking failures
explicitly report ffmpeg spawn NotFound; the invalid-media verifier never
obtained its required real ffprobe cleanup receipt. This is not a passing App
regression or evidence of a plugin-source fix. Root corrected only the admitted
environment, adding the existing actual FFmpeg/ffprobe6.1.1 tool directory with
both executable hashes held; source, assertions, inventory, threads and budget
remain unchanged. The original three exact failed selectors then all pass in
`app-media-cli-recovery-r1/execution`, native owner exit0/descendants reaped,
3.426s controller elapsed. The complete serial r7 outcome is recorded below.
The CLI version is separate from the product's FFmpeg8/avcodec62 qualification
guard; no guard, product linkage, license or test assertion changed.

Current App11 libtest is archived in `native-artifact-archive-r15`, raw SHA256
unchanged after NTFS LZX compression (144,880,640 logical/42,762,240 stored bytes).
Only its archive copy is compressed, preserving the active executable.

The new independent OFX corpus is **acquired, not compatibility verified**:
official purzos-ofx v0.2.0 Windowsx64 asset472588157,3,436,652 bytes, publisher
and local SHA256
`EC707E12E6474E5512ECC3935ABC233EC4D5F205D42D14DEC5424D0CADC18625`.
Bounded archive inventory admits131 members/8,641,200 unpacked bytes. The selected
BitCrush module is130,560 bytes. Actual MIT and static OFX Support BSD3 notices
match pinned source after newline normalization; the exact SDK build commit and
complete linked composition remain unknown. Acquisition receipt is
`ofx-third-party-corpus-acquisition-r1/delivery-receipt.json`, SHA256
`D403EF40046A53C5037D8F51E12D0EC2FBB28B674E0ECECD71884D20231B0302`.
No native descriptor, DSP, GUI, GPU, other-platform or performance result is
claimed. The next finite slice must use the existing isolated CPU Filter path
and an independently fixed numeric image oracle; the plugin's own normalized
clamp must not be described as a host precision conversion.

The audio prerequisite audit finds no canonical note producer in AudioProgram
or process/wire consumer, and production clack dependencies do not currently
enable note-ports. Existing execution demand already conservatively executes
active processors on silent PCM and must be preserved. The proposed next
vertical boundary is authored sample-time notes through the existing Audio DAG
to a CLAP note-controlled effect; zero-input/multi-output instruments and VST3
quarter-note project projection require separate contracts. The first candidate
is opt-in, versioned note-input metadata in the existing discovery worker,
preserving old List/Probe/payload shapes and advertising no executable note
dialect. It is a prerequisite proposal, not implemented event or instrument
support. All production/shared changes remain centrally coordinated; agents
only own isolated proposals/candidates and root alone runs expensive builds.
For Surge XT,185292432 identifies the plugins ZIP;185292289 identifies its
checksum text asset. The incomplete transfer remains acquisition Blocked.

The selected BitCrush module has now been safely extracted and statically read:
SHA256 `AE266A44A17F4140DCB819DF3AE013B97A9F9E0246EEBD29A95DB7183DA2C3C7`,
AMD64 PE32+ DLL, standard OFX entry exports. Static imports and local runtime-file
presence do not certify dynamic loading. Its frozen normalized5x3 image oracle
uses bits8/mix1, bits4/mix1, mix0 and repeat, probability0, exact Float32 endpoint
quantization/alpha bits; no tolerance, HDR or temporal claim. The assigned agent
may implement only a generic external OFX test candidate in its isolated scratch
directory. No native execution or root-source integration has yet occurred.

Independent review conditionally accepts the metadata-only CLAP R2 candidate:
checked native count must distinguish absent function from valid zero; raw getters
must start with initialized scratch, preserve unsigned IDs and unknown dialect
bits, reject missing/failed functions, and cap counts before allocation. The
agent owns the isolated candidate including its minimum vendor accessor; root
owns integration/builds. Broad NoteGate implementation remains unapproved until
quantization, ordering, occurrence/epoch voice lifetime, atomic window admission
and NoteEnd matching are fully specified. NoteEnd matches the original input
note tuple and its timestamp is ignored; current parameter-output time ordering
must not be blindly reused. A separate inherited discovery risk is also explicit:
kill failure currently precedes unconditional wait. The5s work deadline therefore
does not prove bounded fault cleanup. A focused reuse/ownership CCR must address
it without creating a second process-management authority.

The complete source29 App r7 ordinary regression is now independently accepted:
2,778 passed/zero failed/zero ignored/124 filtered in648.87s (649,360ms controller).
All2,902 names,124 ignored names and the exact2,778 ordinary name set agree;
the ordinary prefix-neighbor test remains included. All four actual native Job
owners exit0, with no deadline/error/outliving descendants and successful
retirement. The current fresh App11 producer and PE, all1,786 source hashes and
1,807 input bindings were rechecked. Execution receipt SHA256 is
`AA5C26A55CBF47555471574D3515ABB4EB9D00F31566A3414417124409531BEE`;
independent final receipt is
`app-ordinary-source29-r7-independent-final-acceptance-r1.json`, SHA256
`8F0FD2EA36F5D9B31B70822AD31FDF49305A2F12ED50EA4FA78A62E820C8C9A6`.
The original r6 RED and exact-three recovery remain retained. This is a current
App default-feature ordinary regression, not full Workspace/all-feature runtime,
ignored native-plugin, realtime, GPU/UI or performance qualification.

The OFX external lab r1 candidate is statically conditionally accepted, with
actual API signatures checked. Execution is withheld until a fresh r2 candidate
adds exact first-description animation/numeric metadata expectations, precise
fresh product producer/package/source/terminal validation and local FFI safety
invariants. Existing bit-exact image and primary-failure checks remain required.
No OFX candidate compilation or actual BitCrush invocation is claimed yet.
## External OFX finite test integration (build checkpoint)

Root integrated only the independently accepted generic Windows integration
file `crates/mondrian-app/tests/openfx_external_vendor.rs`; production adapters,
core graph and SDK inputs remain unchanged. Root rustfmt is independently
confirmed equivalent to the sealed r2 formatter output. Source30 Test1 actually
failed with E0599: the graph execution error implements Debug, not Display.
That compiler RED and producer remain retained. Source31 changes only the test's
single error-format expression to Debug; it preserves all numeric/image,
lifecycle and timing assertions. The independent correction receipt SHA256 is
`DFA0BD10E6258784EC8F8997ADF6D19EAB48CE739A6D902378552369F3395035`.
Current freeze31 contains1,787 sources, SHA256
`B2F12C0FFCA0D7994FD33129E8C437D74F4FA3FA3FC34921446DDF46A8501DB7`;
all1,786 previous source29 hashes remain unchanged.

Actual Test2 compilation passes in10.90s, fresh=false/default features,
profile test=true/opt0/debug0; integration PE SHA256
`65E82A22A9A6DF12206BFF0C7DE60A46E274AA620A328A39F09974D0848FA401`.
Cargo actually overwrote the main executable while compiling the integration
target, so root rebuilt Product13 LAST under the same freeze. That actual
cargo-rustc build passes in18.24s, fresh=false/default features, dev opt1/debug2;
the redundant explicit debug2 argument is recorded, not described as cargo-build.
Final helper SHA256 is
`AB67319ACE58D5704EF711CC68BD4CD1FFE374B645CABFA8DC5B93D630372763`.
Both builds hold all1,787 sources plus their metadata inputs. Actual strict
Workspace Clippy17 all-targets/all-features/-D warnings also passes in16.59s,
holding both selected executable bytes unchanged; its source receipt SHA256 is
`04C18C806F66C56FBB52E5094DB679A62E22C4D81A469B62A7C369B26F190BC9`.
These are build/check gates, not native BitCrush compatibility.

Two actual data-only cases were generated under all1,787 source and15 other
input read leases. Generation receipt SHA256 is
`74FA416E9DE938A0C971E5526EA7CE6B4920B0557A45C27AD23024D7E2A3BB5F`.
No actual List, plugin invocation or native test is counted yet. Independent
review refuses execution-adapter r1 until profile matching, primary-error
preservation/final cleanup acceptance and unique evidence-directory checks are
fixed in a new sealed r2. No native deadline or pixel oracle is relaxed.

The CLAP metadata/lifecycle candidate r3 is independently statically accepted:
15 candidate files,7 root preimages and31 packet artifacts match;14 files are
identical to sealed-r2, while supervisor only gains two local SAFETY comments.
It remains outside root production sources until the OFX slice settles.
Independent r3 review SHA256 is
`CF3EC241CA4E37A98453C6F7F0C7E4A0C72513A8D57FE2769502724B55E0B7B8`.
Its9 ordinary/four ignored proposed additions and prior11 owner controls still
require actual new compilation/execution. A pre-existing provenance metadata
length debt remains explicit: MONDRIAN.md hash is correct, but recorded3274
bytes differs from actual4071. This candidate does not silently revise that
historical length. No Note execution, instrument, realtime or complete CLAP
qualification is inferred from static acceptance.

## External OFX actual execution and retained failures

The preparation r2 actually refused before native launch because PowerShell
bound an Object[] to HashSet[string].SetEquals. That data-preparation RED is
retained in `ofx-external-source31-plan-r1/adapter-receipt.json`; it is not a
plugin failure. Sealed adapter r3 changes only the explicit string-array argument.
Its isolated controls actually pass nine/zero and independent static acceptance
SHA256 is `43E07AAC62BAAE5612A64828FE662E9D5571F24C0A03A04EEE6E8F553DFD3EFA`.
Root r3 Prepare and Execute both actually exit zero; no pixel assertion, native
budget, profile, producer or cleanup gate is relaxed.

Actual Execute receipt `ofx-external-source31-native-r2/adapter-receipt.json`
SHA256 is `1E22C70BD9BE6F949DEC920E4060E8ED66B9E9172A8C797AC3901926E9608B86`.
It binds all1,787 source hashes and1,866 inputs. The actual exact ignored List
contains one test/zero benchmarks; both independent native Jobs report exactly
one passed/zero failed/zero ignored/zero filtered, empty stderr, owner exit0,
no deadline/error/outliving descendants and successful retirement. The preserved
source30 compiler RED remains distinct from this source31 native acceptance.

Each round contains nine240-byte rasters: four resident direct calls, one fresh
instance Drop call and four existing graph calls. All18 files equal the frozen
neutral/posterized Float32 golden bytes, including alpha. Actual native factory
ID/version/API and six typed parameter descriptors, canonical identities,
registered Describe equality and all four graph authoring/timing steps are
checked. Direct instances are observed alive after rendering and their retained
native handles signal after shutdown/Drop. The narrow PID-to-OpenProcess race
remains disclosed; graph owner-count retirement and outer Job settlement are
separate evidence, not graph PID or per-child shutdown-error observations.

Rounds take2,151ms and2,091ms including worker startup and lab work; these are
workflow timings, not host overhead benchmarks. This acceptance is Windows CPU
Filter normalized5x3 endpoint/dyadic-alpha Float32 only. No HDR/interior rounding,
GPU interoperability, temporal/tile/multiresolution, animated execution, GUI,
opaque state persistence, realtime, other platform or full Preview/Export UI
qualification is inferred. Publisher SDK commit remains Unknown; selected
authored MIT and static SDK BSD notices are recorded without claiming complete
linked-composition license verification.

Independent final acceptance
`ofx-external-source31-native-independent-final-acceptance-r1.json` SHA256
`AD4F93DF594BB51B9FA4FD069792FB1FF0B883920AF5BC17068019D03DD501D1`
rechecks current source/producer/input lengths and hashes, raw Job outputs,
all18 rasters and native schema/author/lifecycle evidence. Its verdict permits
only the generic integration test and two evidence documents; production core
changes and later source closures require their own gates.

## CLAP input-note metadata and discovery ownership integration

Root integrated the reviewed r3 packet after independently checking all31
packet artifacts, seven root preimages and all1,787 source31 hashes. The15
selected files add a versioned inactive input-note metadata query, minimum raw
Clack accessors, original ABI fixture and focused discovery process ownership.
The existing owner now adopts Child and staging immediately after spawn, holds
staging through response decoding, preserves actual exit status and retains
staging before diagnostics when termination is unconfirmed. Refused kill does
not call unconditional wait; successful-kill kernel wait and permanent orphan
cleanup remain limitations, not bounded-cleanup guarantees.

Metadata distinguishes absent extension, present zero inputs, missing functions
and failed getters. It preserves unsigned IDs, raw name bytes and unknown dialect
bits, caps this query at32 inputs before allocation, checks binary/state identity
and restart requests, and never advertises HostNotePorts or event execution.
The cap is this operation's policy, not a claimed CLAP standard limit. Existing
List/Probe and author/DSP payload shapes remain unchanged; an older helper
refuses the new operation rather than fabricating an empty result. No authored
note timeline, instrument, dispatch, note output or realtime support is added.

Root formats seven Audio/fixture Rust files and generates the fixture's offline
lock with only two path packages. The installed fixture provenance separately
binds formatted root-relative source bytes and this lock; historical candidate
bytes stay immutable. Source32 contains1,796 inputs, freeze SHA256
`EE99CA942A6CB9016960769FA917B9E521B4313334995525D513BEC800E66D3D`.
Independent post-format review SHA256
`C09EB8B1C6EB26FF0EB3F6853145BD4896965EEC0C524A7F3B93A4792CD87D2B`
verifies1,780 unchanged/source31 inputs, seven changed/nine added/no deletion,
actual formatter output and all original11 owner test bodies/assertions unchanged.
The unchanged Clack provenance checker passes252 shipped paths; the inherited
MONDRIAN.md length metadata debt is still disclosed separately.

Actual source32 Fixture1 builds in2.20s and Audio16 libtest builds in1m21s;
both hold all1,796 inputs. The original ordinary preparer r1 refuses before
native launch because its64KB capacity is smaller than the unchanged322,393-byte
historical Audio budget manifest. That admission RED is retained. New r2 bounds
only that same data input at1MB; native work remains180s. Actual all/ignored/exact
ordinary Lists contain419/74/345 unique names, and the actual ordinary group
passes345/zero failed/zero ignored/74 filtered in26.42s (26,627ms controller).
All four observed native Job owners exit0 and retire successfully; root wrapper
session85320 also actually exits0. Execution receipt SHA256 is
`6B8ECC31BF908D1718652E78AA6E8154DE3564451F40893013131F0172FF05B6`.
The inherited ordinary wrapper writes its receipt before releasing leases and
has a final Dispose error-preservation boundary; this successful actual terminal
exit is evidence, not proof of all future cleanup-failure behavior.

Normal Product14 LAST, new ignored native metadata/staging controls, old owner
and real CLAP regression, strict Clippy and final independent acceptance remain
pending at this checkpoint. Owned ABI fixture success will remain distinct from
third-party compatibility. This slice is not integrated as a completed commit
until its runtime gates settle.

The next actual Product14 build fails with linker LNK1318 FILE_SYSTEM(3) while
C: has zero free bytes. Its failed producer/source receipt remains retained and
is not admitted as a helper. Root copies the exact failed partial PDB to
`E:/codex-artifact-storage/mondrian-3349-plugin-host-sprint/`, verifies both hashes
and deletes only that generated C: partial file. Five completed App library
cache files are compressed with all raw hashes unchanged. This recovers build
space without deleting source or changing the release FFmpeg guard. Product15
then actually succeeds in18.29s as the normal LAST helper, SHA256
`65DBD3464ED4EE7B8CE6D390ED470B9EDFE24FDD91E22043080598DD1D1D7C86`.
Strict `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
r18 passes in3m53s with all1,796 sources and the three selected native images
unchanged. These build timings are not host processing benchmarks.

Root's cleanup-first native wrapper r4 actually exits0 (session28403,
chunkabeef7). Original11 process-owner controls, three new discovery/staging
controls and two owned CLAP metadata rounds all pass: each exact selected test
reports1 passed/zero failed/zero ignored/418 filtered. All16 raw Windows Job
owners exit0, report descendants reaped and no errors/deadlines/outliving
descendants. Execution receipt SHA256
`E8FF253234A7376B1370A96961FF1640BDB1CAC9A224F8F62EC5FC703712281F`
binds3,744 inputs and records completed source postchecks and lease cleanup.
Independent actual acceptance SHA256
`510550147B5D25C09E3AE58144A73E2F064A26C851CEDB3E723AC74FC2FFC997`
rechecks the exact actual producer identities, current source/PE hashes and raw
outputs. The ordinary345 gate is separately independently accepted, SHA256
`F660BA714FF84753B353C6A4C8351EF2F9E9FDD5027561340AA65C6A976D7B47`.

Metadata coverage includes high unsigned IDs/unknown dialect bits, fresh zeroed
ABI getters, supplied-state index reordering with stable IDs, absent versus
empty inputs, malformed records, and restart/state refusal. Aggregate refusal
for the cap-exceeding fixture does not attribute its precise child failure;
cap-before-get is established by source inspection. The existing audio-effect
probe still refuses this no-audio fixture. No actual old helper binary,
third-party note processing, PCM, instrument, realtime or performance conclusion
follows from these tests. Real Ring Out and shared VST3/CLAP legacy gates remain
separate pending regression evidence at this checkpoint.

Current-source regression admission also retains two actual pre-native tool
failures. Legacy wrapper r1 exits1 before any controller group because the
pinned PowerShell JSON reader converts ISO timestamps to DateTime and implicit
string conversion produces a culture-dependent value. New r2 reads the original
UTC strings from the same bounded held JSON using JsonDocument, preserving the
exact command/completed chronology/Product-LAST checks. Ring wrapper r2 exits1
with zero groups and an observed cleanup failure because environment deletion
with ordinary `$null` instead supplies an empty string under this PowerShell
binding/runtime. A root-owned isolated environment-key control actually proves
old empty-key retention, explicit `[NullString]::Value` deletion and cleanup;
it does not mutate PATH or run a plugin. Both new r3 wrappers change only the
deletion value and retain bounded environment snapshot checks, original oracles,
native budgets and cleanup precedence. Failed directories and raw terminal
observations remain immutable. These are preparation/environment failures, not
plugin incompatibility or evidence of native test execution.

The corrected legacy r3 actually finishes all10 native groups and exits0
(root session69736, chunkcb808f). Each exact test reports1 passed/zero failed/
zero ignored/418 filtered; raw Job owners exit0 with descendants reaped and no
errors, deadlines or outliving descendants. Aggregate execution SHA256
`A6AC7C017D1C2612ECF556659C2DCDF8B79DD0E3F417D0BFB0E4EB40FC2BD549`
binds1,953 inputs, records all1,796 sources unchanged and completed cleanup.
The current Audio16/Product15 host is exercised with independently source-bound
historical sidechain DLLs, not fixture binaries relabelled as newly built. These
are two rounds of PCM/state/partition/lifecycle, canonical signal-time and VST3
repeated actual-object controls; they do not qualify musical event transport,
UI, realtime or performance. The original four180s and one60s budgets per round
remain unchanged.

Corrected Ring r3 then actually passes both180s native groups and exits0
(root session68297, chunk15d710). Aggregate SHA256
`A6CEC32937DE16B3174DC507B56BB844B5CE4BC6CA467B87AA875CB30293EB82`
binds1,854 inputs and records successful source postchecks, environment
restoration and lease cleanup. Both raw Job owners exit0 and retire descendants
without errors. Full11 native descriptors, editable9/read-only2, canonical
author serialization/reopen, resolver payload and original184-byte default
state are checked with the unchanged corpus/oracle. Plugin version remains
Unknown where the native descriptor supplies none. Group times2,035ms/2,131ms
include discovery and lab work, not host processing overhead. This is real
third-party metadata/default-state regression, not Note processing, PCM,
nondefault private-state fidelity, realtime, performance or full CLAP support.

The next planned investigation was the cfg(test)-only same-registration schema-refusal
diagnostic candidate for the preserved Airwindows CLAP RED. It must retain
original short-circuit behavior, native call count, schema/error semantics and
all old assertions; Stage B callback instrumentation remains unapproved. Shared
note event/time/voice and zero-input producer/output contracts remain separate
prerequisites for instrument execution. No new event model or production
compatibility exception is authorized by metadata qualification.

## Source33 diagnostic observations and exact source34 restoration

The temporary Stage A parent-libtest candidate was centrally compiled against
source33/1797, freeze SHA256
`94D7AA0E979BA6BE2701841016A87D78FACDA9C03F950D0C38682D4CFC632071`.
Actual Audio17 producer and source receipt, fresh native Lists427/74/353,
ordinary353/0 and all8 diagnostic controls passed. The exact ignored external
CLAP selector also passed its original30-second List Job preflight. Independent
ordinary acceptance SHA256 is
`EDD0BEE756B1F47749014E8CD36367059A022B314D2A01B28F7DE2B1219E7B7F`.
These are diagnostic controls, not a new production capability.

Two serial Airwindows CLAP observations retained the original selector,
expected1PASS oracle,180-second budget, plugin6388ea29...,314-byte state835dfde8...,
captured List/Probe and32768-byte asymmetric stereo input. The parent was the
fresh diagnostic Audio17; the normal helper remained separately bound historical
source32 Product15, with byte-equal release-code projection. Neither original
compatibility gate passed. Both failed at unchanged external_vendor.rs:709,
native factory startup validation: `CLAP parameter metadata is invalid or
ambiguous`. Each completed block-size1 bypass output identical to the admitted
input, then failed during the block-size17 startup. Neither emitted the resolver
schema-refusal sidecar. Absence supplies no failed-field, callback, or failure-stage
diagnosis. The primary logs establish this later startup failure; it is not proof
that the older resolver-schema RED has the same cause. The publisher's placeholder
initialization remains a hypothesis about the shipped binary, not a confirmed fix.

Both original tests have0passed/1failed/426filtered. Raw native Job owners have
exit101, no deadline, errors or outliving descendants, and reaped=true; the outer
runner correctly returns1 for its Result::Err. The diagnostic wrapper's terminal0
means both bounded failed observations and cleanup were recorded, not compatibility
success. Actual plan-r2 receipt SHA256 is
`25C6149BC6CE32B25AA875735B07B9907324C70EC13774799510BFCDF4463FDF`;
execution-r2 receipt is
`26A96E6A330CE0F487CBE9B52F9B22A18A57B3CAE9BAB0F5EFDDF03E0F993783`.
ObservationComplete=true, DiagnosticCaptureComplete=false.
Independent actual observation receipt SHA256 is
`FD5DC5A23EA5042EEC467E43EB486767E37F9505EA007CD0D6B5C19DEB4E89C4`.
Initial execution-r1
remains retained: its wrapper incorrectly expected runner101 instead of1 and
stopped after a valid raw native RED. Its receipt is D60877F3...; corrected r4
preserves the original tests and separately records runner1/native101. The root
preflight-r1 missing-workspace-variable refusal also remains retained, before
native execution; preflight-r2 supplies the actual successful gate.

All1796 source32 bytes were restored, worker40309C8C... restored exactly, and
only the newly owned diagnostic module was removed. Source34 restoration freeze
SHA256 is `C7A8791BD395015939C85DB11109F2F1CA847621D29D38543A3EEC013B95104C`.
Fresh Audio18 rebuild passed; ordinary345/0 passed with the restored419/74/345
inventories, receipt SHA256
`6F87E7C5C1B47AD47122B5C4A8836C81A975546962A8DDDBEAF83F65A1EDA973`.
At this restoration checkpoint, Audio18 occupied the parent executable path. Audio17 was held against writes
through the diagnostic runs and their postchecks, but no separate executable
archive was retained before rebuilding; historical producer/image hashes and
the sealed diagnostic source packet remain. Reproduction requires rebuilding
that packet rather than treating the current executable as Audio17.

At that checkpoint, the next independent slice was cached factory-slot count admission before descriptor
iteration in existing List, Probe and Note queries. The same existing4096 host
capacity applies; it is not a CLAP standard limit. The scratch candidate has6
prospective ABI-counter controls and a three-guard RED variant; independent
static review SHA256 is
`8650225C02E327C1C020631D8C2A471CA53E1C932DC537E50B7053AE33D4D542`.
It had not yet been integrated, compiled or runtime-qualified. Successful-kill
kernel wait and permanently unconfirmed child retirement remain disclosed
ownership limitations. Shared Note/instrument contracts remain separate;
their scratch proposals do not add executable notes, GUI, realtime safety,
performance, other-platform or complete-standard qualification.

## Source36 — bounded CLAP factory traversal (2026-10-12)

The existing emitted-descriptor cap did not bound factory slots: Clack's iterator
skips null descriptors, and filter/take(2) bounds matching results rather than
native descriptor requests. List, Probe and the additive input-note metadata
query now check the iterator's cached initial count before traversal, using the
existing4096 host capacity. This reads no additional native count/get callback.
Normal admitted factories retain ordering, duplicate/missing-ID behavior and
the existing error/lifecycle/state channels. No audio/render graph, author model,
worker protocol or vendor-specific compatibility branch changed.

Two owned source files changed: `processor_isolation/clap_discovery.rs` and
`processor_isolation/clap_note_input_metadata.rs` in mondrian-audio. Six ABI-counter
controls cover oversize4097/u32::MAX refusal before any get, exact4096 capacity,
empty/null slots, duplicate/missing IDs, original request order and no unintended
plugin creation. Owned immutable fixture lifetimes and callback safety invariants
are documented next to the test ABI. Source35 RED removed only the three new
guards; its two exact negative controls really failed the unchanged positive
test oracle (runner1/native101), not a deadline or cleanup gate. Source36 GREEN
minus those guards reproduces the formatted RED bytes exactly. All old and new
test assertions remain intact. The Audio19 RED PE and historical Product15 helper
were copied with exact-byte verification before the new builds.

Source36 freezes1796 inputs, SHA256
`F13673499C5255F0D95C571666235401E92CA80ABCDED2744AF0DACAC1E840DC`.
Fresh Audio20 and normal Product17 were produced from that closure; Product17
was built last with the original command/profile. Actual all/ignored/exact-skip
Lists425/74/351 and their settled Job receipts determine424 filtered tests for
each selected native case. The fixture binaries retain their historical producer
identity; their actual independent compiled/support subsets match current bytes.
An old full host freeze is not imposed on changed source36 or relabelled fresh.

| Actual source36 gate | Result | Execution receipt under sprint scratch |
| --- | --- | --- |
| Audio ordinary, including six new controls |351 passed/0 failed/74 filtered,26.71s | `audio-ordinary-source36-r1/execution-receipt.json`,8D6B8F07... |
| Process ownership14 + CLAP input-note metadata2 |16 selected cases passed; original60s/180s budgets | `clap-noteport-source36-native-execution-r1/execution-receipt.json`,87B13F3F... |
| Original CLAP/VST3 PCM/bus/state/time/object cases |10 passed in two rounds, original oracles/budgets | `audio-legacy-source36-execution-r1/execution-receipt.json`,CCAA4AD2... |
| Real Ring Out read-only metadata/default opaque load |2 passed;11 descriptors/9 editable/RO IDs9,10/184B state | `clap-readonly-source36-native-execution-r1/execution-receipt.json`,8EB59A7E... |
| Strict workspace Clippy | `--locked --workspace --all-targets --all-features -- -D warnings` passed | `workspace-clippy-r20-source-receipt.json`,8F831CC0... |

All selected native Job owners exit0, report no deadline/reaping errors and
confirmed descendant settlement. Root tool terminal exits0 separately confirm
the wrappers returned after cleanup. Each wrapper holds source/image/input read
leases and performs postchecks. Ordinary and preflight inherited receipt-before-
cleanup boundaries remain limited by these separate terminal observations.
The owner/metadata dedicated wrapper normalizes inherited PATH but does not
claim caller restoration or a last-round in-process environment observation.
Identical raw List content originally made the new Ring plan's hash-only input
lookup ambiguous: unexecuted r1 was refused by review; sealed r2 deduplicates
consistent canonical paths, rejects conflicting hash/length, and binds all four
files to the actual all-stdout directory. Original native oracles are unchanged.

Product16 was a real failed build: Cargo101/linker1318, PDB LIMIT(12), with an
invalid442585088B partial PDB. It remains archived and hash-verified on E; only
that generated workspace PDB was removed after copying. Five completed App rlibs
retained their raw hashes; compressing the new453393402B rlib stored78897152B.
Product17 then passed the same command/configuration in19.55s. This recovery
does not establish the underlying linker cause or a processing performance gain.
Product16 is never admitted as the helper. Original failures and earlier evidence
remain retained, not overwritten.

This slice qualifies finite Windows factory admission and the listed regressions.
A native count/get callback can still hang or violate pointer contracts; the
slot cap is not complete native-call safety. Existing successful-kill kernel-wait
and permanently unconfirmed child-retirement limitations remain. Real Ring Out
reports no native version string; it does not qualify PCM, nondefault state,
GUI, full-project reload or Preview/Export. Other platforms, GPU interop,
allocation/deadline performance and full-standard compatibility remain unverified.

At the source36 checkpoint, the next slice was generic parameter-refusal detail
in the existing CLAP error channel, using
the same rejected descriptor's direct numeric bits and first unchanged predicate;
no extra native query, normalization, sidecar or Stage B instrumentation is
approved. Separately, proposed Note author M1 needs an archive boundary decision:
old nested AudioProgram readers ignore new fields, so document31 must not carry
note_lanes. Using the existing version gate to reject32 in old readers plus an
explicit strict31-to32 note-free migration is the proposed minimum; container1/library6 need
not change. No Note author/model/schema migration has been implemented or
approved here, and ordinary transactions/Undo cannot recover data discarded
during old deserialization.


## Source38: CLAP parameter-refusal diagnostics and actual failure evidence

The existing shared parameter validator now appends the rejected slice index,
unsigned native parameter ID, first failed predicate and direct `f64::to_bits`
hex values for minimum/maximum/default/current to its original InvalidContract
prefix. Every original predicate, short-circuit order, `seen.insert` effect,
inclusive range, stepped-field order and1024 host capacity remains. No plugin
name, native reread, normalization, new state/protocol or DSP logging was added.
Formatting occurs only on refusal; this is not a real-time safety certification.

Only `mondrian-audio/src/processor_isolation/clap_discovery.rs` changed in
production. Four additive controls cover ordered and overlapping refusals,
unchanged admission/count error, full-width unsigned IDs, signed zero and direct
NaN payload bits. Source37 used the complete original validator with the same
new tests. Two actual RED controls failed their original positive oracle,
runner1/native101, and settled without deadline/reaping errors. Its Audio21 PE
is retained before replacement. Restoring the original validator in formatted
GREEN exactly reproduces formatted RED, including every old/new test.

Source38 freezes1796 inputs, SHA256
`C1C97090D7E490953BCEC1D90E5452E0FB70A90D8FFF80A54F99EAC32B259FE0`.
Fresh Audio22 and normal Product18 use that closure, original command/profile;
Product18 is last. Actual all/ignored/ordinary Lists429/74/355 determine428
filtered tests for the selected native cases. Historical fixture binaries retain
their producer identity; independent compiled support subsets match current
bytes. Their old full host freezes are not relabelled current.

| Actual source38 gate | Result | Execution receipt under sprint scratch |
| --- | --- | --- |
| Audio ordinary, including four new diagnostic controls |355 passed/0 failed/74 filtered,26.99s | `audio-ordinary-source38-r1/execution-receipt.json`, BFD0D28D... |
| Process ownership14 + CLAP input-note metadata2 |16 selected cases passed; original oracles/budgets | `clap-noteport-source38-native-execution-r2/execution-receipt.json`, 3206E817... |
| Original CLAP/VST3 PCM/bus/state/time/object |10 selected cases passed; original oracles/budgets | `audio-legacy-source38-execution-r2/execution-receipt.json`, FB059392... |
| Real Ring Out metadata/default opaque load |2 selected cases passed; original oracles/budgets | `clap-readonly-source38-native-execution-r2/execution-receipt.json`, ED723E2E... |
| Strict workspace Clippy | All targets/all features, `-D warnings`, jobs1; exit0 | `workspace-clippy-r22-source-receipt.json`, E548BC0E... |

Two real AirwindowsConsolidated CLAP1.2026.285 rounds retain the original plugin,
314B Galactic state, input PCM, automation, selector, partitions1/17/64,
positive `expectedPassed=1` and180s budgets. Both remain FAILED at the first
partition1 startup validation, before PCM creation. Both reject index8,
native ID2944917348: round1 `default_out_of_range`, default bits
`C4DD647200000000`; round2 `default_nonfinite`, bits`FFFFFFFFE0000000`.
Minimum/current are`0000000000000000`, maximum`3FF0000000000000`.
The same raw test stderr contains these fields, not a separately captured Probe
or reconstructed decimal. Each native owner exits101/runner1, with confirmed
reaping and no deadline/settlement errors. Diagnostic capture is verified;
compatibility remains RED. Source33's earlier completed PCM is not inherited.

The existing startup rejection channel covers decoded payload validation,
expected output construction and native metadata after state/render-mode setup.
No actual child payload snapshot was retained; the message alone does not
identify its originating call site or prove publisher/native callback fault.
Pinned publisher source is a hypothesis lead, not shipped-binary proof.
The two-round receipt is
`clap-parameter-refusal-source38-native-execution-r1/execution-receipt.json`,
SHA256257385DD18B3704CA8FCD36F229F2126E9AF866B0F715FB632F49955FB595F42;
independent runtime acceptance is39690739... under sprint scratch.

Normal App library build completed first; lossless generated-rlib compression
preserved453387582 raw bytes and hash, then Product18 linked normally in19.81s.
Audio22/Product18 and615927808B PDB were copied/hash-verified in the existing
E artifact store; only two exact generated local PDB hard links were released.
Strict21 genuinely failed with PowerShell OOM and allocation errors; its raw log
and missing final source receipt remain recorded. Strict22 passed in3m01 with
the same argv and jobs1. This does not identify the resource root cause or
establish processing performance. Failed data caller r1 is also retained:
metadata16 prepared, then an incidental requirement for unselected PDB presence
stopped Legacy preparation. Sealed generator r2 canonicalizes only filenames
membership; selected PE existence/hash/profile/producer/oracles stay strict.
Data caller r2 uses fresh output paths and succeeds; no original native test was
weakened. Pre-native relative-output refusal and root install hash-format/path
guard mistakes remain separate observations rather than execution claims.

This slice qualifies bounded diagnostics and the listed existing Windows
regressions. Full Note DSP/instruments, GUI, Preview/Export equivalence,
allocation/deadline performance, GPU interoperability, other platforms and
complete standard compatibility remain unverified. Next is a separately
reviewed minimal classification of the three existing startup rejection sites;
it may identify host consumption location, not prove a publisher defect. No
site classification implementation has been integrated in this checkpoint.

Surge XT1.3.4 corpus acquisition has now completed separately: official Windows
plugins-only asset185292432,49093875B, publisher MD5
`d318e95ca79a5ee70ddafec04313a00c` and local SHA256
`564E162C560AF07AD4ED47FE1BFCD827CF97A575DE30D06C48249AAD2E7C35E6`.
The first finite tail request timed out after28127602B and remains failed; its
partial was unverified until a second exact206 remainder and complete-archive
MD5 established the full byte sequence. Original partials and both attempt
receipts remain. `surge-acquisition-resume-r2/archive-admission.json` records
acquisition only: no extraction, installation or native/plugin compatibility
result is established by it. Static package and license inventory follows.

## Source40: CLAP startup refusal consumer position

`mondrian-audio/src/processor_isolation/clap_worker.rs` appends one fixed
`validation_site` only to the existing detailed parameter-validation refusal:
`decoded_payload`, `expected_output_contract`, or
`native_after_state_render_mode`. These identify the host consumer, without
attributing the descriptor to a writer or a publisher defect. The three existing
validation calls, native query count, state/render-mode setup and error categories
remain. Count, payload-decoding, structural-change and unrelated errors retain
their existing details. Successful validation adds no formatting allocation.
No parameter authority, payload schema, state source or execution path was added.

Four additive controls exercise the real missing-library factory boundary, the
existing loaded-output consumer, native metadata after an owned ABI state
restore, and preservation of valid state/count/other errors. The output consumer
is redundant behind validation of the same immutable public-factory payload;
its direct existing loaded seam does not imply that ordinary factory requests
can reach this refusal. Source39 retains the original three calls with the same
new tests: three precise missing-suffix controls failed and one preservation
control passed. Its formatted source and Audio23 executable were retained before
GREEN. Restoring those three calls in formatted GREEN reproduces the whole RED
source. Non-Windows `Result<Option<_>>::expect` is statically compatible with
`Ok(None)`; actual runtime evidence here remains Windows only.

Source40 freezes1796 inputs, SHA256
`01AE6CD93EDAE390ED500C6C3508C08DD930EE0C9D4A53FCD2EF80BCDB8D3126`.
Fresh Audio24 and normal Product19 share this closure, with original commands
and profiles. Normal App library and current metadata capture precede Product19.
Actual all/ignored/ordinary Lists433/74/359 establish432 filtered tests for each
sole selected native case. Historical fixtures retain their separate producers;
current compiled support subsets match their bytes.

| Actual source40 gate | Result | Execution receipt under sprint scratch |
| --- | --- | --- |
| Audio ordinary, including four new consumer controls |359 passed/0 failed/74 filtered,26.51s | `audio-ordinary-source40-r1/execution-receipt.json`,13FFB750... |
| Process ownership14 + CLAP input-note metadata2 |16 selected cases passed | `clap-noteport-source40-native-execution-r1/execution-receipt.json`,E7EAF96F... |
| Original CLAP/VST3 PCM/bus/state/time/object |10 selected cases passed, two rounds | `audio-legacy-source40-execution-r1/execution-receipt.json`,DD0E82E0... |
| Real Ring Out metadata/default opaque load |2 selected cases passed, two rounds | `clap-readonly-source40-native-execution-r1/execution-receipt.json`,73FD98FF... |
| Strict workspace Clippy | All targets/all features, `-D warnings`, jobs1; exit0,3m25s | `workspace-clippy-r23-source-receipt.json`,2BA80569... |

The original two AirwindowsConsolidated CLAP1.2026.285 PCM cases remain FAILED.
They keep the same314B Galactic state, authored values, inputs, partitions1/17/64,
positive expectedPassed1 and180s budgets. Round1 completed the4096-frame,
32768B partition1 output with input/output byte equality and native bypass,
then failed at partition17 startup with
`CLAP parameter metadata changed since contract probe`. Round2 failed during
opaque native state restore with `CLAP discovery worker failed with status exit
code: 1`, before PCM output. Both native owners exit101/runner1 and settle
without deadline/reaping errors. Neither contains a detailed descriptor refusal
or a `validation_site`; descriptor ID/bits and native origin are not inferred
from Source38 or a separate Probe. ObservationComplete is true,
DiagnosticCaptureComplete and CompatibilityQualified are false. The raw receipt
is `clap-parameter-refusal-site-source40-native-execution-r1/execution-receipt.json`,
SHA25660EB2D176D1EC31ECAD95F493028641A23B3C04C749D2F91338BC91063A784AD.
Owned ABI consumer behavior is verified within the controls; actual third-party
site capture remains Implemented — Not Fully Verified.

Engineering resource records retain the normal library build6m06 and Product19
link46.73s. Lossless compression of the completed453390758B generated RLIB
preserves SHA256D754FDF1... and stores78893056B. These are build/disk observations;
processing performance, deadlines, allocations and mature-host overhead remain
unmeasured. Static review rejected two unexecuted stale-freeze helper scripts and
a self-referential packet manifest. New immutable revisions correct only those
preparation defects; original selectors, budgets and native assertions remain.

Surge XT1.3.4 now has four contained local module extractions from the admitted
archive: instrument/effects CLAP and VST3,61463552B total. Independent CRC, SHA,
x64 PE and export checks passed. Native discovery, roles, resolved dependencies,
state, DSP and GUI remain unverified. The two-format Effects List adapter is a
scratch candidate; independent controls exposed rejection of legal empty Lists
and inconsistent case-sensitive ID ordering. Its separate correction passed
finite static and pure-control review; actual generation and execution remain
pending.

OpenFX current-Source IsIdentity remains a scratch candidate. Independent review
found that recoverable persistent Error could mask native fatal/error status;
the revised candidate fixes this precedence and adds conjunction/retirement
controls, with actual RED/GREEN pending. The existing Render/ROI/EndSequence path
has a separate statically identified version of this risk and is unchanged.
It needs its own reproduction and fix before the identity feature is integrated.
Note DSP/instruments, GUI, Preview/Export equivalence, other platforms, native GPU
interoperability and full standard compatibility remain outside these verified
finite gates.
