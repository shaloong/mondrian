# Color commercialization: remaining qualification work

This is a living transfer checklist, not a qualification certificate. Completed
software contracts, passing developer tests, and physical qualification are
different evidence. Update this list after each coherent implementation block;
do not promote an unavailable or failed measurement to a pass.

## Remaining scope at a glance

All eight IDs below remain open as complete deliverables. Historical `done`
labels for a software milestone do not certify native implementation on other
platforms, measured performance, physical I/O, or independent reference frames.
Detailed local subitems and evidence follow below; keep this roster in every
completed-block report.

| Priority / ID | Remaining deliverable | Execution boundary |
| --- | --- | --- |
| P0 COL-010 | Actual HDR/P3/ICC Viewer qualification | Qualified display hardware |
| P1 COL-031 | Cold-start cancellation latency, eligible local diagnostics, complete sealed performance matrix | Local software work; full baseline needs a qualifying machine with at least 32 GiB RAM |
| P2 COL-042 | DeckLink/AJA native bridge and SDI output qualification | Vendor implementation plus physical I/O |
| P2 COL-043 | Genlock and reference-monitor qualification | Physical reference chain |
| P2 COL-044 | ANC/VANC, captions/timecode and broadcast QC chain | Physical ancillary-data capture and independent QC |
| P2 COL-045 | Matching Blender/Resolve/Premiere reference frames | Exact application versions, matching contracts and independent captures |
| P2 COL-046 | Native macOS/Linux implementation and platform/driver/display matrix | Implement and execute on each target OS; Windows passes do not transfer |
| P2 COL-047 | Remaining constructor/callback/Window/Golden ownership, raw receipts, bounded verification support, capsule/loader authority, Windows smoke, route-qualified optimization guidance, 72-hour campaign and replay | Local implementation first; full campaign and independent physical qualification remain separate |

Lifecycle audit caveat: historical clean receipts cover their declared owner
inventory, not every renderer thread. The upload JoinHandle was previously
discarded. The active implementation now retains it behind consuming Renderer
retirement and propagates the actual join receipt through App GPU closure.
Five Renderer protocol/pool regressions and an actual compact-YUV GPU
record/submit/retirement test have passed, followed by 25 App progress tests,
five recovery-receipt tests and one real Headless GPU retirement test. The
strengthened worker-before-submit/map ordering, real DX12 decoder-generation
retirement, Window transfer regression, and six final Renderer unit regressions
also passed. The input test exposed and corrected a stale RGBA16F-only
guard rejecting the product RGBA32F native intermediate. Do not upgrade old
receipts or call this complete startup/normal lifetime qualification yet.

The partial-startup lifetime-safety prerequisite adds one shared GPU startup guard
to Window and Headless: caller-side construction coverage, explicit optional
Renderer ownership, asynchronous retirement on failure/unwind, move-only
activation without residual device/queue handles, and delayed native decode
publication. Device reopen now uses its original validation deadline. This
was only the prerequisite for owning startup errors and terminal receipts.
The new Headless implementation below supplies those seams; Window remains
item 1 below. Real shared-guard protocol tests are distinct from a complete
Window-construction fault-injection or campaign-return regression.
Its local Release verification passed three real-GPU guard tests, 25 progress
protocol tests, one real Headless retirement test and two Window lifecycle tests
(31 focused tests total). The injected constructor panic is intentional and
caught; these tests do not certify complete startup/public-result qualification.

## Local Windows work still in progress

The current COL-031/COL-047 slice isolates native FFmpeg audio startup behind
one bounded Media-owned lane. Its actual-call-site regression first proved a
canceled read was blocked by OS process creation. Owning completions now keep
producer leases through install/retire, including buffered/disconnected results;
teardown first reclaims independent idle Sessions and continues draining while
a native startup is blocked. SourceCache schema 6 includes independent startup
worker, request, join, failure, and unresolved-owner inventory. Startup failure
never falls back to synchronous read-side construction. Focused Media gates
passed62 SourceCache regressions; both real WAV/AAC parity cases passed.
Six actual-native-call samples returned canceled reads in0.98–18.86ms with
complete eventual closure, but one AAC physical observation still held its
permit at50.83ms and failed the unchanged50ms gate. The other five physical
observations were33.25–42.38ms. App validation passed35 executed cases:21
Waveform,2 App shutdown,9 Endurance coordinator,2 explicitly selected real-GPU
cases and1 Project smoke. The smoke's create/open/save maxima were289/78/58ms.
The actual schema6 Project receipt passed the strict PowerShell owner/case/suite
corpus, including missing, mistyped, stale and contradictory startup inventory.
Full workspace/all-target/all-feature Clippy with warnings denied, format and
diff checks passed. App release rebuild took25m13s; that is iteration cost,
not realtime qualification. An initially wrong Waveform GPU filter ran zero
tests and was not counted; the corrected exact test executed and passed.
Physical 50 ms retirement, multiple-source startup pressure and sealed matrix
qualification remain COL-031 work. Do not convert this logical-response fix
or eventual resource closure into physical timing qualification.

COL-047 default performance owner-closure receipts now cover Project, App UI,
Preview decode/cache, and continuous playback. The 2026-09-04 release run
returned clean Preview/cache/GPU/App closure for all four scenarios, without
the earlier native shutdown crash. It did **not** pass the whole performance
suite:

- Project create/open/save passed; continuous playback returned 60/60 Ready.
- App UI cold root construction measured 14,609 ms against 10,000 ms; its
  remaining six cases passed. An unchanged release-binary repeat measured
  6,673 ms and passed all seven cases. Retain both samples: this is variable
  cold-start cost, not a proven latency fix. A deterministic NumberInput/Label
  regression confirmed two independent widget font-measurement initializations.
  These now share one thread-local owner, with exact-size/shaping parity and
  all 1,003 Widgets tests passing. The modified App release smoke measured
  2,750 ms and passed all seven UI cases. This is the new original-scenario
  observation; the unchanged-binary repeat was not used as fix evidence.
- Decode/cache proved real persistent publication and a new request-local
  lookup/hit. Its render maximum was 64,201 us against 50,000 us: preparation
  2,157 us, composite 4,723 us, CPU output boundary 57,321 us. The boundary
  includes processor preparation, Program Output, monitor adaptation, and
  quantization, not cache disk publication. Keep the required CPU publication,
  output extent, color semantics, and existing budget intact when optimizing.
  Isolated 960x540 boundary diagnostics reproduced 60.3/70.2 ms cold and
  43.9–56.8 ms warm execution. Bounded OCIO scratch and removal of an unused
  retained Program frame now measure 42.2 ms cold and 34.5–39.0 ms warm, with
  identical input/output hashes. Core release regression passed 348 tests;
  the public Viewer allocation regression passed after first proving the
  redundant full-frame allocation. The complete App rerun still **failed**:
  52,872 us against 50,000 us, including preparation 2,124 us, composite
  5,148 us, and CPU output boundary 45,600 us. Persistent publication and the
  new request-local hit passed, as did all six ordinary media cases; owner
  closure remained clean. Next isolate exact RGBA8 quantization rather than
  rerunning unchanged code until a favorable timing sample appears.
  The subsequent shared exact quantization kernel passed four kernel
  regressions and public Viewer presentation parity. Its isolated median was
  0.918 ms versus the original loop's 5.065 ms; complete cold boundary execution
  measured 37.063 ms with the same final RGBA SHA-256 as before. These remain
  diagnostics, not substitutes for the original App gate. The subsequent
  `col047-quantized-final` App gate passed at **44,567 us / 50,000 us**:
  preparation 2,140 us, composite 4,363 us, CPU output boundary 38,064 us.
  All six media cases passed, including real persistent publication and the
  new request-local hit. The CPU optimization block is validated.
- Export/Audio initially stopped at an OCIO dependency-checkout write denial,
  before measurement. After authorized dependency rebuilding, the 1080p29.97
  Export repeat passed: 96 frames, zero budget misses, maximum 22 ms, with
  passing pixel/color evidence. The Audio repeat passed all 12 scalar/SIMD
  load-matrix cells with zero deadline misses. Both reports passed the exact
  suite eligibility validator; the initial build denial was neither a
  performance failure nor a passing skip.
  The subsequent `col047-cpu-final` suite again passed Export (96 frames,
  maximum 20 ms, zero misses) and all 12 Audio cells. Its only failure was the
  decode/cache render budget above. All six reports were copied with matching
  SHA-256 into `.scratch/color-pipeline-commercialization/evidence/20260904-cpu-first-suite/`.

The final quantized suite passed Project, media, continuous playback (60/60
Ready), Export (96 frames, maximum 19 ms, zero misses, pixel/color pass), and
Audio (12 cells, zero misses). Its UI case failed **before measurement** because
the PID/counter-based fixture allocator adopted an existing runtime directory
without a durable ownership marker. The same release test executable, using a
fresh exclusive process-local temporary parent, passed all seven UI cases
(root construction 5,068 ms / 10,000 ms) and the original eligibility validator.
Do not call this a single all-green suite run. A controlled injection into a
fresh temporary parent reproduced the exact marker failure (child PID 9152,
first legacy fixture slot, exit 101); the slot did not preexist the injection.
The fixture allocator was corrected separately with an atomically claimed
randomized root and unchanged fixture lifetime. Three regressions first failed
on the old allocator, then passed on both the isolated source harness and the
rebuilt release App test binary. The real App UI test with an injected old PID
slot then passed all seven cases (root construction 2,874 ms / 10,000 ms), left
the old runtime directory unchanged, and passed the original eligibility
validator. The production ownership guard remains fail-closed. The fixed report
and log were SHA-256 verified in
`.scratch/color-pipeline-commercialization/evidence/20260904-fixture-fix/`.
Earlier reports and both failure/repeat logs were preserved
with SHA-256 checks in
`.scratch/color-pipeline-commercialization/evidence/20260904-quantized-suite/`.

These are developer-run observations on an uncommitted tree, not a sealed
COL-031 or COL-047 reference-machine result. Raw local reports are under
`target/perf/col047-final/` and `target/perf/col047-cpu-final/` and may be
removed during a clean rebuild. Preserve reports outside `target` with hash
verification before deleting build output.

The raw-terminal evidence block passed 91 focused tests on 2026-09-04,
including real App clean/incomplete shutdown and one real Headless GPU/native
scheduling shutdown. GPU terminal fault combinations are protocol tests, not
physical fault injection. Full workspace/all-target/all-feature Clippy, format
and diff checks passed. These are developer validation results, not a sealed
realtime or 72-hour qualification baseline.

The subsequent Headless failed-start block passed 115 focused release tests,
including real GPU constructor errors/unwinds, binding errors/unwinds, the
public campaign error receipt, shared startup guards, and a normal active
Headless/native-scheduling shutdown. Preview schema-3 and the strict PowerShell
validator's adversarial fixtures passed; the full workspace/all-target/all-feature
Clippy, format and diff gates passed. This does not close the remaining internal
constructor, Window, durable-success or physical qualification work below.

The internal Preview partial-construction block is implemented and its runtime
gates passed on 2026-09-05. A complete unpublished Runtime retains each returned
cache/visual/fallback/media/observer owner before subsequent construction, and
Headless/Golden/Perf keep its owning failure separate from normal Preview.
The partial schema-1 predicate reconciles exact owner states and outcomes with
the unchanged normal schema-4 receipt. Unknown native construction, opaque
payload abandonment and original-deadline timeouts remain failures. A cache
timeout now correctly counts a started/detached worker rather than NotStarted.

Local release verification passed 57 bounded protocol cases and 41 App cases:
5 Runtime startup, 4 Headless startup, 1 Golden, 2 Perf failures, 15 Preview
shutdown, 9 Endurance coordinator, 2 App shutdown, 2 explicitly selected actual
GPU cases and 1 Project lifecycle smoke. The protocol/App binaries overlap in
three evidence-predicate cases; these are execution counts, not 98 distinct
behaviors. Project create/open/save maxima were 391/92/76 ms with clean raw
receipts, and the strict PowerShell owner/case/suite corpus passed. App lib-test
rebuild took 24m25s; this is iteration cost, not a realtime performance baseline.
Full workspace/all-target/all-feature Clippy with warnings denied passed in
7m03s; final format and diff checks passed. No capacity failure or target cleanup
occurred. This does not close COL-047 as a whole.

The following UI auxiliary-owner slice retains actual Thumbnail and native
audio-device catalog workers, shares ordinary-quit/validation closure under one
absolute deadline, and preserves original Window errors alongside cleanup
failures. Thumbnail closes shared admission and both bounded transports;
activity publication is revoked without hiding current execution. Its original
receipt is immutable, and concurrent closure is explicitly unavailable rather
than a guessed empty inventory. Catalog retains cumulative native startup/join/
panic/timeout facts across refreshes. Common native join behavior is reused by
Preview, Thumbnail and catalog, including opaque panic-payload handling.

Final local verification on 2026-09-05 passed 91 source-linked protocol tests,
one explicitly selected actual native catalog test, and 57 Preview protocol
tests. A real saturated-result regression first failed because dropped results
retained publication identities; the failure remains archived and that exact
case now passes after the activity revocation fix. App runtime gates passed37
executions:2 Host validation,1 ordinary quit,1 explicit native Host,1 Window
error merge,21 Waveform (14 service and7 startup),9 Endurance coordinator,
1 actual GPU and1 Project smoke. App lib-test rebuild took19m04s. Project
create/open/save maxima were299/69/50ms with clean raw owner receipts; the strict
PowerShell owner/case/suite corpus passed. Final workspace/all-target/all-feature
Clippy with warnings denied passed in2m17s; format and diff checks passed.
The protocol execution counts overlap in shared native-join cases; they are
not independent commercial or physical qualification. No capacity failure or
target cleanup occurred. Another project's Cargo process was observed but not
modified; this is not an isolated performance baseline.

The following Host-only construction slice is now implemented. The deep startup
Module takes the unique App before resolving/loading one default preferences path,
captures audio/display/theme state, and installs Thumbnail, Waveform, Preview, and
catalog owners before later checkpoints. A failed transaction first signals all
owners, consumes partial or complete receipts under one original deadline,
restores and reads back App/process configuration, and returns the same live App
with its primary diagnostic. Opaque payload abandonment fails closed. Complete
Host field order also keeps the App alive until after UI execution services.

The production-linked validation example uses RequiredPackaged Preview rather
than a test Adapter. Local dev qualification on 2026-09-05 passed two normal
routes, all 18 Host checkpoints, three Waveform checkpoints, five Preview
checkpoints including an actual packaged media worker, and one hostile opaque
payload case. The first run exposed that Cargo `examples/` executables resolved
the product worker from the wrong directory; shared profile-root discovery now
handles both `deps/` and `examples/`, and the complete rerun passed. The separate
window-service protocol passed 92/92 with one explicit native test ignored, and
the audio intent getter regression passed 1/1. Final release product build passed
in 13m26s; the same-profile release qualifier built in 13m38s and executed in
1.10s. Full workspace/all-target/all-feature Clippy with warnings denied passed
in 3m03s. Final format/diff checks passed. These link durations are developer
iteration observations, not realtime performance evidence. No capacity failure,
target cleanup, branch switch, or concurrent agent build occurred.

This is not whole Window construction/reopen qualification. Next extend the
Window transaction across old/candidate GPU generation, native event-loop state,
Host success/failure handback, and one final unique App receipt. Durable successful
raw-receipt history remains separate work.

The Window candidate-publication slices now prepare the complete
Window/Surface/frame-renderer/Viewer-runtime candidate before changing Host or
old-Window state. Initial Window startup consumes owning Host failures and
bounded partial-GPU cleanup under its existing deadline. Device reopen validates
candidate identities before retiring the old generation, bounds every candidate
cleanup path, and publishes Host display/native-import state only after clean old
retirement. Same-Device role replacement also prepares its shell before hiding
the old one. The active-session slice makes `run_on_demand` borrow rather than
own Host/session, catches callback panic, and always performs explicit final GPU
then Host/UI closure against one deadline. Validation retains and checks the raw
final progress/Renderer receipt and exact returned App; ordinary Quit only begins
shutdown so it cannot create a nested deadline.

The follow-on publication slice enforces `Prepared -> Activated -> Outer
Installed -> Host Published`. Initial theme/visibility/redraw joins the guarded
publication region; role replacement transfers the complete Device-generation
authority and installs the candidate in the caller-owned Session before any Host
mutation. New-Device reopen also installs its activated candidate first, then
retires the now-local old Session and publishes Host state only after the old
receipt is clean. Retirement error/panic and publication panic therefore retain
the candidate in the outer Session for explicit closure. Publication failure is
terminal; there is no rollback to a partially stale Host. A normal replacement
error is also promoted to an event-loop failure and cannot become normal exit.
Remaining Window work includes EventLoop creation ownership, an
impossible-state-free durable outer Runtime/Host/GPU/native receipt, physical
native termination evidence, and durable old/candidate/final receipt history.
These slices must not be reported as whole Window qualification until complete.

The next runtime-owner slice replaces the bare four-thread Tokio Runtime with a
native supervisor-owned Module. UI execution receives only a Runtime Handle.
Normal active closure, Host-startup failure, and guarded initial-publication
failure signal the supervisor and consume its real thread handle under the
unchanged Window deadline. Only `OwnedWorkerShutdown::Terminated` after Runtime
destruction qualifies; timeout, panic, or current-thread join is dirty, while a
fallback detach is unverified and produces no receipt. This removes unbounded
Runtime Drop from early-return fallback paths without claiming that detached
fallback produced a receipt. The following pre-active construction slice
aggregates Window/Surface/Adapter/Device/partial-GPU failures and construction
panics into one exact App handback; durable aggregation remains separate.

The initial pre-active construction follow-on retains the complete raw Host until
Host publication succeeds. A single caught construction scope records the last
installed Window/Surface/Adapter/DeviceQueue/GPU-progress/waker/Prepared/Activated
stage. Error or panic first consumes any partial Viewer GPU owner, then closes the
complete Host and returns the exact App, then consumes the background Runtime,
all against the original absolute deadline. The stage evidence states only that
Rust native authority left the event-loop-thread scope; it does not claim OS or
driver termination. Production-seam tests prove ordinary-error scope destruction
precedes evidence construction and panic diagnostics preserve the last stage.
EventLoop creation and durable typed outer receipt aggregation remain open.

Final gates for this slice passed: default and validation checks, two focused
construction error/panic regressions, App and full-workspace all-target/all-feature
Clippy with warnings denied, and full-workspace format. The final-source release
validator rebuilt in 15m26s and real Win32/winit/DX12 cycles 15-16 both passed
with distinct Surface/Device generations, terminated old progress workers,
complete retirement, returned YUV workers, no native device removal, actual
presentation, and matching original/reopened picture hashes. Binary SHA-256 is
`84D784A2AE082489416B72AAC5F37B6B983A479A33BD818AC3C9846E9422EB80`;
report SHA-256 is
`8D81BBF6CE2ADB0E65D035E8BAFC73344FD538E333753CEF2F5E93AC6A27106C`.
Observed 166-213ms Preview preparation remains COL-031 failure evidence rather
than realtime qualification. No capacity failure or `target` cleanup occurred.

Local final-source verification built the release
`mondrian-surface-reopen` binary in 12m19s and ran two real Win32/winit/DX12
cycles (7-8). Both old generations recorded terminated progress workers,
completed retirement, and returned Renderer YUV workers; the endurance Adapter
also required each final active generation's raw receipt to qualify before the
run could succeed. The report remains schema 1 and serializes only the reopen
operation's old-generation receipt, so durable final-generation history is
still COL-047 item 4. Three focused Window regressions, default/validation
checks, full workspace/all-target/all-feature Clippy with warnings denied, and
format/diff gates passed. No capacity failure or `target` cleanup occurred.

The candidate-before-publication follow-on rebuilt that release binary from
final source in 15m04s and passed real Win32/winit/DX12 cycles 9-10. Both old
generations again recorded a terminated progress worker, accepted and completed
retirement, a returned Renderer YUV worker, and no native-device removal. Six
focused ownership/error regressions passed, including an injected retirement
failure that leaves the candidate in the outer Session. Default and validation
checks, final-source focused test, workspace/all-target/all-feature Clippy with
warnings denied, and full-workspace format check passed. The observed
188-227ms `prepare_viewer_gpu_preview` responsiveness warnings are not accepted
as realtime qualification and remain COL-031 evidence. No capacity failure or
`target` cleanup occurred.

The extracted background-runtime Module passed four focused tests: exact worker
configuration and clean supervisor return, fail-closed timeout evidence, a real
blocking task that returns `TimedOutDetached` within the 25ms deadline, and
expired-deadline rejection before supervisor spawn. Default/validation checks,
workspace/all-target/all-feature Clippy with warnings denied, and full format
passed. The final-source release validator rebuilt in 13m21s; real
Win32/winit/DX12 cycles 13-14 both completed with distinct Surface/Device
generations, terminated old progress workers, complete retirement, returned YUV
workers, and no native device removal. The in-memory Window return gate also
required a clean background supervisor return, but schema-1 does not yet
serialize that receipt. The binary SHA-256 is
`4932E667324418F61DF443E6AC2CC54026D6265AE29C6DC9DAF3D2D18032DBEE` and the
report SHA-256 is
`E263CC32ED0D27DDAB7059F53564CA7333A564D0D30B58CFC642498223F2C535`.
The observed 156-229ms Preview preparation warnings remain COL-031 evidence,
not a realtime pass. No capacity failure or `target` cleanup occurred.

Remaining COL-047 checklist (ten subitems; retain every item in block reports):

1. Other callbacks/GPU closure and native wake health.
2. Window initial/reopen: Host handback, candidate-before-revoke ordering,
   bounded partial candidate cleanup, and explicit final active-session
   event-loop panic/GPU/UI/App closure are complete. Candidate installation
   before fallible Host publication and post-handoff publication panic ownership
   are complete. The background Runtime now has a bounded supervisor owner and
   clean join predicate. Initial native/GPU construction errors and panics now
   have a unified bounded close path. EventLoop creation, impossible-state-free
   durable Runtime/Host/GPU/native evidence, physical native termination, and
   durable old/candidate/final receipt history remain.
3. Golden whole-operation closure.
4. Successful raw receipt retention/history/durable serialization, including Export.
5. Performance-support deep Module extraction and bounded production-linked tests.
6. Capsule namespace/spawn authority/child leases/ACL/fallible cleanup.
7. Pre-loader authority and mapped-object identity.
8. Actual Windows campaign smoke after the ownership prerequisites.
9. Route-qualified CPU/GPU optimization advice preserving CPU publication and
   valid hybrid routes.
10. Physical 72-hour campaign and independent replay.

Detailed implementation history and remaining boundaries:

1. Preview/Waveform ownership history; other callbacks/GPU and Window remain.
   The Waveform follow-on
   below has passed its focused ownership regressions. A constructor
   unwind before returning an owner is now explicitly unverified, not a clean
   NotStarted inventory. Include every Preview worker (including dependency
   observation) and preserve the original caller deadline. Preview worker joins
   now share a consuming Module that never destroys opaque unwind payloads on
   the shutdown stack and retains separate abandonment counts. Dependency-worker
   health is also projected without waiting for evaluation/result polling.
   The linked App block passed 113 focused release tests, including five actual
   Headless GPU startup/shutdown cases. After adding failure-safe test-gate cleanup,
   the separate actual-source protocol harness passed all 20 cases; the two
   corrected disconnect/Drop cases also passed 16 repeated executions. Its
   dependencies and native search paths came from the freshly built Cargo graph.
   Final strict PowerShell, format/diff, ordinary App library Clippy and full
   workspace/all-target/all-feature Clippy gates passed with warnings denied.
   That earlier block did not yet close internal construction or every other-owner/GPU
   callback panic path. The subsequent Work Watch block now owns bounded
   registrations and off-producer retirement, opaque payload/capture abandonment,
   original-deadline immutable receipts, and rejected reentrant/concurrent
   consuming calls. Observer termination stores unhealthy before publishing its
   terminal hint. Preview schema 4 requires the complete raw callback receipt;
   Runtime, Headless and Perf preserve its independent failures and inventory.
   Strict Rust/PowerShell validation rejects missing nullable fields, scalar-enum
   shape errors and contradictory named/aggregate worker counts. GPU progress
   callback/join closure and native event delivery health remain separate work.
   The following Waveform startup slice prepares a complete unpublished owner,
   installs SourceCache and analysis handles before later startup steps, and
   retains exact partial-stage failure inventory through Endurance shutdown.
   Production SourceCache construction no longer performs redundant erased
   initialization callbacks after decoder startup. Normal one-worker closure
   remains mandatory; partial startup uses a separate predicate and the same
   consuming implementation. Ordinary Window construction preserves its existing
   degraded/panic policy. Waveform/SourceCache validation on 2026-09-04 passed
   53 Media source tests, 21 App Waveform tests, nine Endurance coordinator
   tests and three explicitly selected real-GPU tests covering two partial startup
   stages, active-session closure and the public failed-binding error receipt.
   Both WAV/AAC window-parity gates passed. Full workspace/all-target/all-feature
   Clippy, format/diff and strict PowerShell owner/case/suite checks passed;
   the latter used the explicitly selected archived Project report. Default
   ignored tests were not counted. The supplemental App rebuild took 26m09s;
   this is iteration cost, not realtime qualification. Supplemental real-file
   cancellation testing exposed a separate cold-start latency defect: physical
   permits are released asynchronously, not necessarily at read return, and
   both observations can exceed the unchanged 50 ms requirement. Actual child,
   pump and shutdown-owner receipts closed completely; this is not evidence of
   a leak or a stable timing qualification. Final direct-FFmpeg WAV/AAC samples
   passed at approximately 34 ms read return and 36 ms physical release, but
   earlier repeated failures remain recorded. Isolate cold-start cancellation
   and qualify controlled-load WAV/AAC behavior; preserve typed command-admission failures, physical
   capacity and retained owner evidence rather than moving waits onto callers.
   Final local Work Watch validation on 2026-09-04: 46 actual-source protocol
   tests and 59 production-linked App targeted tests passed, including the
   explicitly selected real-GPU Headless startup/binding failure and panic case.
   Full workspace/all-target/all-feature Clippy with warnings denied, format,
   diff, and strict PowerShell owner/case/suite checks passed. A new test's
   incorrect worker-total assumption was corrected against the real factory's
   baseline; the production closure predicate was not weakened. The final App
   lib-test rebuild took 26m01s: this is developer iteration cost, not realtime
   performance qualification. No capacity failure or target cleanup occurred.
   Finish Window initial/reopen candidate
   owning failures and their separate old/candidate/UI receipts, preserving the
   original error and unique returned App owner.
   Headless GPU construction/binding now retains exact partial/complete owners;
   Endurance installs failed startup before error propagation and returns a
   dedicated Startup receipt. Perf factory and direct GPU/CPAL paths consume
   failures; Golden uses the same binder. Live DeviceDestroyed counting now
   shares terminal classification. Preview schema 4 requires the actual
   dependency-observer join and callback inventory; historical schemas 2/3
   cannot prove the new inventory.
   The current raw-terminal block preserves complete Realtime/App-only receipts
   separately from the optional snapshot and attaches them to public campaign
   failures, including failures after clean shutdown. Next-phase preparation
   clears stale error attachments before workload loading. It also replaces
   Endurance's lossy GPU projection with the exact shared receipt and one
   Endurance/Perf/Window normal qualification predicate, rejecting
   `DeviceDestroyed`. This is a protocol finding, not an observed hardware fault.
   Public successful campaign returns still expose the manifest only; raw
   success reporting and Window/internal-constructor inventories remain incomplete.
   Golden's normal whole-operation closure must also be covered, not only
   returned-owner bind failures. Preview-first outer constructor unwinds now
   consume the existing App and retain unverified internal-inventory facts.
2. Extract cohesive performance owner-closure support from the large test
   module. The validation-only lifecycle/cfg boundary is now closed: ordinary
   App library and binaries, validation App library, and the required
   workspace/all-target/all-feature Clippy each passed with warnings denied.
   Only qualification wrappers/imports/read-only diagnostic retention were
   gated; ordinary Drop, worker accounting and shutdown primitives remain.
   No broad warning suppression was added. Keep bounded protocol-test execution distinct from
   unnecessarily repeated release linking of every product/tool executable;
   preserve the real public-interface compile checks and test coverage.
   `preview_visual_protocol` now has a validation-only Cargo example/libtest
   entry, not a top-level integration target. The original 26 private tests across
   the real worker-lifecycle, notifier, visual-task and dependency-observer
   Modules live in independent `tests/protocol/` files, excluded from ordinary
   library source inputs. One additional test covers already-terminal string
   and opaque panics at an expired join deadline. Run `cargo test --release -p
   mondrian-app --features validation --example preview_visual_protocol -j 1 -- --test-threads=1` with
   `CARGO_INCREMENTAL=0`. Cargo owns library/native/profile/loader selection;
   no one-off manually linked runner is required. Neither a product binary nor
   an alternative Runtime was introduced. Local release evidence: initial
   migration26/26 passed (build6m34s); adding only the new case27/27 passed
   (build1m08s). Both Cargo graphs contained no companion binaries or App lib-test;
   the second kept the ordinary App library fresh. The non-test example returned
   the expected usage failure. These are iteration observations, not a controlled
   performance baseline. Cold or changed ordinary App library builds remain
   expensive. Performance evidence/owner support extraction and
   bounded production-linked owner tests are still unfinished; source protocol
   tests do not replace App Runtime/GPU/physical closure qualification.
3. Close the capsule lifecycle: sealed namespace, spawn-time admission,
   retained child leases, explicit Windows access-control evidence, and
   fallible process-owner cleanup. Static owners do not run TempDir cleanup
   at process exit. Never recover orphans by deleting a filename-prefix glob.
4. Add pre-loader authority and post-load image/object attestation. Current
   loaded-module canonical paths do not prove the identity of an image mapped
   before the retained source handle was acquired. Do not substitute a partial
   PE hash for complete image identity.
5. Run the locally executable real Windows campaign smoke after those
   boundaries close. A short smoke cannot certify the physical 72-hour run.
6. Qualify the CPU-output bottleneck recommendation: the current generic
   `move_preview_output_boundary_to_gpu` action is not universally applicable.
   Diagnostics must preserve mandatory CPU cache publication, respect route
   requirements, and distinguish processor/memory optimization from legal GPU
   output admission (including a hybrid path if actually supported).

The typed FFmpeg command-construction boundary now distinguishes absent,
admitted and rejected authority. Only absence may resolve packaged/PATH tools;
rejection returns the original typed cause without a sentinel command. Audio
admission releases unused physical permits, avoids string-only failure caching,
and retains rejection even against a concurrent cancellation after decode.
Preview cannot treat it as recoverable codec failure. Export hardware and Smart
Render do not choose a fallback route after rejection; ordinary unsupported
hardware/output mismatch still can. Public artifact validation and independent
verification retain typed causes, and Golden/CLI fixtures use the same resolver.
Private stem/DPX validation retains its terminal-only string diagnostics; some
terminal paths prioritize concurrent cancellation, never success or fallback.
Fixture encoding failures after successful admission retain their prior skip
policy. These are command-construction contracts, not spawn-time/loader or
physical qualification. Export's standalone validation feature explicitly enables
the Media validation contract rather than relying on workspace unification.
Local validation on 2026-09-04: Media library433 passed/8 ignored and Export
library255 passed/8 ignored, both with validation enabled and serial test
execution. Export includes real independent full decode, pipe layouts,
high-precision image/mezzanine/stem/H.264 delivery, Smart Render and HDR10
metadata checks. This is developer-run regression evidence, not a sealed
performance or 72-hour qualification baseline. Ordinary Media/Export library
Clippy and full workspace/all-target/all-feature Clippy passed with warnings
denied; format and diff checks passed as well.

A same-user filesystem race and an attacker able to inject into the process
are distinct threat models. Any solution requiring a new privileged broker or
independent security principal needs an explicit deployment decision.

## Transfer to macOS and Linux

COL-046 requires native implementation **and** execution, not merely rerunning
Windows tests. Qualified FFmpeg preparation/revalidation, immutable source
inventory installation, and exact Project fixture installation currently
reject unsupported non-Windows platforms.

On each target OS implement and verify descriptor/immutable-executable
authority, loaded-library provenance, loader search/injection environment,
file-object identity across spawn, child inheritance, and consuming teardown.
Then run the native build/test matrix, real decoder/encoder and audio-device
paths, GPU/driver/display cells, process-tree memory capture, and endurance
capture. Preserve exact source/package/runtime hashes and the same sealed
workload contracts. A Windows pass is not evidence for another platform.
The CPU terminal RGBA8 kernel uses baseline SSE2 on x86_64 and canonical scalar
code elsewhere; ARM performance must be measured on the target, not inferred
from the local x86_64 optimization.

## Physical and external-application work

- P0 COL-010: real HDR/P3/ICC Viewer display qualification.
- P1 COL-031: execute the complete sealed realtime performance matrix for the
  current release candidate: 4K60/8K30 HDR/effects/scopes, real dual-layer
  Main10 Preview, 30-minute video/audio, and 5/30/120-minute authoring gates.
  The software matrix exists, but the issue records no executed sealed
  reference-machine baseline. Run locally eligible cells after owner closure
  is complete; retain unmet reference-machine requirements for transfer.
  Local read-only inventory on 2026-09-04 reports 15.86 GiB physical memory,
  12 logical processors, Windows build 26200, and an NVIDIA RTX 3050 Laptop GPU
  (WMI-reported adapter RAM approximately 4 GiB; driver 32.0.15.9159). No
  hardware serials were collected. This machine does not meet the matrix's
  32 GiB `professional-large-project` minimum; transfer the sealed full-matrix
  baseline to a qualifying machine. Locally runnable smaller diagnostics are
  still useful but cannot be relabeled as that baseline. GPU timestamp/HDR
  admission remains execution evidence, not inferred from this inventory.
- P2 COL-042: DeckLink/AJA vendor bridge and physical output qualification.
- P2 COL-043: Genlock and reference-monitor qualification.
- P2 COL-044: physical ANC/VANC, captions/timecode, and broadcast QC chain.
- P2 COL-045: exact-version Blender, DaVinci Resolve, and Premiere reference
  frames with the declared matching color/alpha/export contracts.
- P2 COL-046: the complete platform/driver/display matrix, including the native
  implementation work above.
- P2 COL-047: the complete physical 72-hour campaign and independent replay.

Use the [endurance runbook](commercial-endurance-qualification.md),
[platform matrix](platform-driver-display-qualification.md), and
[cross-application capture procedure](cross-application-color-capture.md).
Missing devices, licenses, native adapters, or independent reference captures
remain explicit incomplete qualification; simulators are contract tests only.
