# Modal, resource and LUT acceptance

## Qualification scope

The latest clean-source native group/resource qualification is
[ce3edfb qualification](#clean-source-native-and-encoded-qualification-for-ce3edfb).
The latest complete encoded Golden is
[ce3edfb qualification](#clean-source-native-and-encoded-qualification-for-ce3edfb).
Source-level tests and native results qualify their named source and binary;
they do not automatically cover later edits.
The complete NLE goal remains active while the listed professional matrices are
open.

## Earlier qualified native-output snapshot

Windows qualification on 2026-10-06 used clean, unchanged production source
`97e82355bb92e4eb81d982c39bfad3628018cf99` and rebuilt product SHA256
`6d208ce39bc84c10e39e4c023eb96faf34591b07b45ac72ca15c8c052456e72e`.
This receipt qualifies that exact clean source, including curve preview admission,
dense temporal editing and canonical key ordering. The accompanying documentation
update does not change production code. Later sections identify subsequent UI
changes that are not covered by this binary receipt.

| Scope | Evidence |
| --- | --- |
| Modal keyboard | 16 App modal tests passed on this commit. Actual EventRouter exercises Tab and Shift+Tab containment, background input blocking and focus restoration. Pending resource confirmation leaves focus order; timeout permits retry. |
| Resource authoring | 103 resource tests passed. Cancelled, edited, stale, replaced and locked drafts cannot commit; Save As changes invalidate the inspected directory. Confirmation enters the author session. Binding a LUT does not import media into the asset library. |
| Process supervision | Both ignored supervision tests explicitly passed: cancellation/timeout reaps the owned child, retry works, and oversized responses leave the service reusable. |
| Native resource execution | Three ignored tests explicitly passed with the rebuilt product: native Unicode paths/helper, actual Host asynchronous confirmation, and native SDK path cancel/Undo/SQLite/Unicode `.mdp` reopen with exact pixels. |
| Undo and durable state | Saving an undone snapshot retains live Redo. Fresh reopen restores saved author state with empty history. The ignored native curve test explicitly passed author edit/Undo/saved Redo/fresh reopen/output checks. |
| Exact integer keys | A current App regression passed three cases: i64 extrema and a value above 2**53. Each case saves and reopens both undone and redone Unicode archives, comparing the entire author Sequence and fresh history. Canonical Integer keys retain all bits; keyboard value increments retain exact owner time, identity and Hold interpolation. |
| Dense temporal keys | Actual Inspector Auto/Continuous Bezier cases qualify exact frame crossing, canonical ID order even with coincident projected coordinates, complete Undo/Redo and Unicode undone/redone saved states. Fresh reopen has empty history. |
| Raw Float keys | The legal canonical Number endpoint 0.2 under hard bounds 0.1..0.2 retains every author bit during time-only editing. Dispatched author gestures returning to origin or cancelling preserve the entire author state and an existing Redo; Unicode undone/redone archives reopen exactly with empty fresh history. |
| LUT pixels | A real Unicode `.cube` swaps red/blue on positive-alpha samples, retains alpha, and preserves zero-alpha hidden RGB. The shared compiled graph returns identical float pixels after durable reopen. |
| GPU and encoded output | Complete Golden passed 3/3, seven stages per round, natural exit 0 each. Source attestation remained clean/stable and baseline eligible. DX12 output had zero CPU fallbacks; final H.264 decoded maximum channel error was 4/255 each round. |

The modal/resource/curve filter counts overlap and must not be added as unique
coverage. All five ignored resource tests and the ignored native curve test were
executed separately; ignored results were never counted as passes.

## Workspace gates and author format

The prior 55cd95c all-feature workspace test run passed 7523 tests, failed zero and ignored
159, including integration and documentation tests across 133 result summaries.
App alone passed 2549, failed zero and ignored 62. Strict workspace
all-target/all-feature Clippy, formatting and diff checks passed before the
production commit. The current 4d2a21e source passed full Widgets 1080 and App 2550 tests with zero
failures, plus strict Clippy, formatting and diff checks. It then passed rebuilt modal, resource,
integer-history, Float-history and all six explicit native tests. Product and
clean source identity remained unchanged throughout qualification.

Schema 31 stores discriminated Number(f64) and Integer(i64) channel keys.
Integer normalization, Hold evaluation, edits and prepared readback preserve
canonical bits. Disabled animation still permits exact author-key lookup.
Floating geometry is an explicit projection; it cannot distinguish adjacent
wide integers. Interpolated integer schemas retain the existing exact f64-domain
restriction. The alpha format rejects prior schemas and primitive numeric key
payloads directly, without migration aliases or a legacy decoder.

Scalar curve action values now use canonical `AnimationChannelValue`, preserving
raw Number precision independently of the evaluated native scalar. Absolute
pointer candidates retain exact original coordinates and origin interpolation
metadata; genuinely sparse direct actions retain current canonical coordinates.
The Core channel setter normalizes final neighbor spans and validates atomically.
This is an alpha action payload change with no previous PropertyValue decoder;
project storage remains schema 31. Core full 463 and App curve 48 regressions
passed; their ignored counts were 1 and 3 respectively.

## Curve preview admission qualification

The curve preview provider is the admission authority: an absent Edit projection
rejects its candidate and author dispatch. Point/handle movement restores local
coordinates, rejected keyboard edits restore metadata, rejected deletion restores
the removed entry, and a refused initial insertion cancels without pointer capture.
Existing capture permits recovery or Escape cancellation. Recognized arrow keys
remain owned when a value is clamped or an edit is refused. Accepted deletion
reveals the actual replacement selection in a zoomed viewport.

Actual Inspector events cover finite author Numbers exceeding native Float range:
the refused candidate leaves the last admitted geometry and entire author Sequence
unchanged; one Undo/Redo restores the complete origin/admitted Sequence. Generic
widget regressions cover point, keyboard, handle, deletion and insertion refusal.
Three genuine REDs preceded the fixes: ghost geometry, leaked handle arrow input,
and an offscreen replacement selection after accepted deletion.

This patch passed the full Widgets library (1080 passed, zero failed) and App
all-feature library (2550 passed, zero failed, 62 ignored), strict workspace
all-target/all-feature Clippy, formatting and diff checks. These counts are not
added to earlier workspace/native results. The first full App run exposed a
retirement-test fixture that consumed queue-empty outcomes during idle polling.
Forcing idle polls reproduced it deterministically. Its replacement models queue
readiness as persistent state and exact submission readiness independently,
keeping nonblocking handoff, independent release, real join and single-drop
assertions. The final full App run and 20 forced-idle repetitions passed; product
retirement logic and deadlines are unchanged.

The committed 4d2a21e source was then rebuilt and qualified by Golden 3/3,
all six explicit native tests and modal/resource/integer/Float history tests.
The source and product hashes remained unchanged throughout. Golden rounds
exited naturally in 124.822, 120.500 and 123.331 seconds.

## Dense temporal editing and exact crossing

The subsequent temporal UI patch passed full Widgets (1083 passed, zero failed,
zero ignored), full all-feature App (2551 passed, zero failed, 62 ignored), strict
workspace all-target/all-feature Clippy, formatting and diff checks. These
results supersede the earlier library counts for this patch. Its clean committed
source was then rebuilt and qualified by the complete native-output gate above.

Transfer-curve display spacing had prevented moving and inserting distinct,
closely spaced temporal keys. A genuine pointer regression reproduced the
failure before separating temporal spacing. Actual Inspector keyboard crossing
also reproduced the exact-time helper's refusal to cross neighboring keys.
Temporal movement now permits crossing inside the owner interval, while the
canonical author model rejects actual collisions. Points and metadata reorder
in place; selected and captured keys retain stable identity. Canonical owner
order distinguishes exact times sharing an f32 projected coordinate. Escape
cancels the draft and retains an existing selection; a subsequent idle Escape
clears it. Cancelled insertions retain no phantom selected key.

Actual Inspector acceptance covers Auto/Continuous Bezier in both an ordinary
dense interval and a wide interval with genuinely coincident projected key
coordinates. Ordinary cases use wheel zoom and pointer events; wide cases select
the stable key through the widget API and then use real exact keyboard intents.
Each accepted step compares exact owner time, canonical identity order and the
entire author Sequence. Undo and Redo compare every snapshot; Unicode archives
save undone and redone states, saving retains live Redo, and fresh reopen restores
the complete saved Sequence with empty history. This is not a physical pointer
precision claim. The committed source additionally passed Golden 3/3, all six
explicit native tests, modal/resource filters and the integer/dense/Float history
regressions. The source and product hashes remained unchanged throughout; rounds
exited naturally in 122.676, 121.348 and 122.433 seconds.

## Crossed gesture cancellation and collision admission

A subsequent actual Inspector regression reproduced a cancellation defect when
three exact owner times shared one f32 position: the complete author Sequence
was restored, but two displayed key identities remained swapped. Cancelling by
sorting projected positions cannot recover exact origin order. Point gestures
now retain the original slot, rotate points and metadata back together in place,
restore selection by target and rebuild the existing ID index. This adds one
slot coordinate to the drag state, not a whole-curve clone per pointer movement.

The real Inspector regression covers six cases: Auto/Continuous Bezier in an
ordinary dense interval, a wide interval with coincident projected coordinates,
and an exact quarter-time neighbor collision. A refused collision retains the
last admitted full author state, geometry, identity order and capture; a valid
candidate recovers. Escape and a finished excursion returning to the exact origin
restore author state and displayed identity order, add no Undo, preserve prior
Redo, and the retained Redo still restores its complete expected Sequence.

This patch passed 84 Widgets curve/model tests, the full all-feature App library
(2552 passed, zero failed, 62 ignored), strict workspace all-target/all-feature
Clippy, formatting and diff checks. These results qualify the changed UI source;
the native binary and Golden report above remain bound to production 97e8235.
Further curve interaction work will be batched before rebuilding that output gate.

## Canonical temporal start and end previews

A subsequent real Inspector regression reproduced a boundary projection defect:
an authored key moved to the exact owner start, but the widget retained both
its real point and the former evaluated virtual point. The rebuilt owner model
correctly contained one endpoint. CurvePreview now explicitly supplies evaluated
virtual start/end points only where no authored key occupies that exact time.
CurveBoundary identifies Start/End independently of display slots. Both visual
and audio adapters use exact owner intervals and already evaluated canonical
samples; no alternate boundary evaluator or legacy slot parser was added.

The widget reconciles at most two boundaries in place. Ordinary value updates
retain the ID index; structural insertion/removal rebuilds it. Captured point
gestures retain at most two original boundary entries. Cancellation restores
origin slots and selection by target, accounting for an uncommitted inserted
point. Additional genuine regressions exposed provisional insertion indices and
Snapshot targets skipped by Keyframe-only restoration; both retain their original
assertions and now pass through the same bounded rollback.

Actual Inspector Auto/Continuous cases cover both owner endpoints, moving away,
retrying and cancelling with complete author-state and live-model comparison.
Actual audio EventRouter Auto/Continuous cases cover both endpoints, moving away,
retrying, identity/capture and a single complete Undo/Redo. The boundary filter
passed 33 tests; these overlap the full App run and are not added to its count.
The final changed source passed 85 Widgets curve/model tests and full all-feature
App (2554 passed, zero failed, 62 ignored), strict workspace all-target/all-feature
Clippy, formatting and diff checks. The native product receipt above remains
bound to 97e8235; it does not qualify this subsequent UI binary.

This alpha widget API change requires CurvePreview providers to supply
virtual_boundaries and changes VirtualBoundary targets to semantic CurveBoundary
values. All in-repository providers and callers were updated; no compatibility
alias or prior action decoder was retained. Project storage remains schema 31.

## Reproduce and locate evidence

Run `cargo test -p mondrian-app --all-features --lib modal -- --test-threads=1`
and the corresponding `resource` filter. Explicitly execute ignored tests with
a freshly built `mondrian` product and the owned SessionProbe fixture; resource
supervision also requires `MONDRIAN_RESOURCE_INSPECTION_TEST_PYTHON`. Use fixture
mode `strings_paths` for resources and `curve_authoring` for the native curve
output test. On Windows include product `target/debug` in `PATH` for native DLLs.
Missing loader setup is an execution environment failure, not a test result.

The exact-source complete output gate is
`scripts/validation/invoke-complete-golden-project-gate.ps1`.
The qualified 97e8235 consecutive report is
`target/validation/nle-backlog-complete/20261005T202700Z-complete-golden-4c57c061/complete-golden-consecutive-report.json`.
It retains per-round reports, process logs, build log, source attestation,
product hashes, pixel errors and encoder diagnostics.

## Execution environment and previous evidence

Product authority locks deliberately live in the fixed LocalAppData namespace,
outside the workspace sandbox; changing TEMP must not split cross-process
exclusion. This clean source/product ran with required native execution
permissions. No lock bypass or alternate product interpretation was added.

A previous c977ce1 attempt failed at startup with Windows access denied before
output. Its retained report is
`target/validation/nle-backlog-complete/20261005T090302Z-complete-golden-4208cbd7/complete-golden-consecutive-report.json`.
The earlier 6fb3c9e qualification passed 3/3 at
`target/validation/nle-backlog-complete/20261005T105524Z-complete-golden-289c938f/complete-golden-consecutive-report.json`.
These receipts describe their own source and product, not the current snapshot.

The previous production 1ade888 qualified Golden 3/3 at
`target/validation/nle-backlog-complete/20261005T133819Z-complete-golden-25a4f96e/complete-golden-consecutive-report.json`.
Its product SHA256 was
`75bac4a3ad892717e68e1a2a2b13f9d1bd3c09eeb5f25249940774def980e1aa`.
That receipt remains historical for this changed production source.

The previous 55cd95c qualification passed Golden 3/3 at
`target/validation/nle-backlog-complete/20261005T165658Z-complete-golden-5f191471/complete-golden-consecutive-report.json`.
Its product SHA256 was
`8a1e0cfab72f1494191fba8d860c38470f08946d1785e9ac8701f59479945c90`.
It remains historical evidence for that source.

The previous 4d2a21e qualification passed Golden 3/3 at
`target/validation/nle-backlog-complete/20261005T190834Z-complete-golden-bd0354d9/complete-golden-consecutive-report.json`.
Its product SHA256 was
`e5d5de9d274c30e38345d55c30e5da6ee7a0e7bd024fb23e51015d85f877a51f`.
It remains historical evidence for that source.

## Remaining qualification

Chooser input uses FakePlatform; keyboard coverage routes actual UI events but
is not a physical keyboard recording. Native helper, SDK, SQLite, project files,
float pixels, GPU output and encoded roundtrips are real. This is Windows
qualification, not macOS or general third-party plugin compatibility.
NVENC uses `cpu_rawvideo_pipe`; this does not establish zero-copy transport.

Full professional curve interaction, Float pointer primitive-range admission,
a dedicated real-owner handle overflow scenario, multi-key editing and long-span
exact pointer input,
a dedicated out-of-soft-range UI-edit native output scenario, plugin author
writes and vendor/bus/platform/localization matrices remain open. The overall
NLE goal remains active.


## Temporal ruler continuation (2026-10-06)

Inspector numeric and Mixer audio curves now display owner-domain seconds and
absolute author value rulers. Both use the same plot geometry as input routing;
zoom/pan change labels only. Theme tokens reserve the gutters. Measured labels
avoid overlap and numeric truncation in narrow panels. Fixed parameters show
one actual value. Scientific formatting stays finite at f64 extremes; extreme
origins that lose visible duration in f64 display use exact rational labels.

Final verification on this production source:

- Curve widget/model/ruler module: **89 passed**, zero failed or ignored.
- App library with all features: **2554 passed**, zero failed, **62 ignored**;
  309.79 seconds. Includes modal Tab/focus, pending resource drafts, atomic
  rejection, constrained Bezier boundaries/cancellation, Undo/Redo and save/reopen.
- `resource_lut_draft_commit_undo_save_reopen_executes_real_cube` passed: actual
  project persistence and real cube execution through the resource author path.
- Strict workspace/all-targets/all-features Clippy passed in 2m33s; fmt and diff
  checks passed.

The initial curve App run had two failing pointer fixtures: full-widget centers
became empty plot space after reserving rulers. Both now target the actual
middle author key, retaining original complete-state and history assertions.
The full regression passed both, and retains the pre-existing full transfer
capacity and audio lifecycle tests. A mistaken broad test replacement was
caught in diff review, restored from HEAD before App validation, and replaced
with three bounded edits. Its interrupted run is not qualification.

No fresh native golden executable was built for this UI source. The historical
97e native/SDK/GPU/encoded evidence above remains identified with that source.
This continuation does not close the overall NLE goal or the remaining plugin,
platform, multi-key, pointer precision and native qualification scope.


## Atomic key-set author command continuation (2026-10-06)

Core bulk deletion validates a unique nonempty stable key set, removes it as one
candidate and normalizes surviving constraints together. Incomplete multi-channel
keys, stale IDs, duplicates and empty sets reject the whole request. Deleting all
keys retains the original evaluated value at an explicit owner time. Clip/Effect
numeric product actions and audio automation actions use existing author
transactions; no loop of independent edits or competing curve evaluator exists.

The actual Clip action test covers Auto/Continuous Bezier, whole-set rejection,
surviving selection, one Undo/Redo, partial and all-key removal, and Unicode `.mdp`
save/reopen for edited, undone and unkeyed states. Fresh readers are closed before
opening another copy of the same logical Project, honoring the product lease.
The audio action test covers both constrained modes, one Undo/Redo, invalid sets,
all-delete retained value and locked-track refusal. A public Core integration
also covers incomplete channel identity, path mapping and codec roundtrip.

Final gates: Core library **464 passed / 0 failed / 1 ignored**, Core integration
**1 passed**, Timeline library **229 passed / 0 failed**, App all-feature library
**2556 passed / 0 failed / 62 ignored** (326.83s), strict workspace/all-targets/
all-features Clippy passed (3m28s), fmt and diff checks passed. The App suite includes
modal focus, resource transactions and actual resource-LUT Undo/save/reopen/cube
execution. A real RED exposed RemoveMany being classified as an ordinary edit,
selecting one removed key; both product handlers now deselect deleted IDs only
within the addressed property. No warnings were suppressed.

This is verified author-command groundwork. CurveEditor multi-selection and
group pointer/time/value/interpolation UI remain pending. No fresh native golden
was built on this source; the prior native report remains source-pinned historical
evidence. Overall NLE goal remains active.


## Visual key-selection UI continuation (2026-10-06)

Visual numeric curves now admit Shift-click selection and Ctrl/Cmd+A, keeping
stable authored IDs rather than display slots. Virtual evaluated boundaries are
excluded, including when zoom hides real keys. Replacement views preserve the
surviving set and recover a primary from survivors when its former ID is absent.
Painting/accessibility expose the full set; Escape clears it without authoring.

Delete sends one complete DeleteMany through canonical preview and the existing
Clip/Effect RemoveMany author transaction. Inspector models supply current exact
Clip author time, so removing all keys retains the original evaluated current
value. A rejected preview restores full local geometry and selection together;
an accepted all-key delete rebuilds canonical evaluated boundary points.

The actual numeric CurveEditor/EventRouter test exercises Shift, Ctrl and Meta
selection, no selection history or capture, all-key retained value, boundary
reconciliation and exactly one complete Sequence Undo/Redo. A real failing
replacement test exposed stale stable-ID selection in set_points; replacement
now clears both that set and its former primary instead of transferring author
selection to unrelated snapshot geometry. Surviving-ID refresh and whole-set
rejection tests also pass.

Final curve-widget tests: **92 passed / 0 failed** (3.96s). Full all-feature App
library: **2557 passed / 0 failed / 62 ignored** (557.71s), including current-source
modal routing and resource_lut_draft_commit_undo_save_reopen_executes_real_cube.
Strict workspace/all-targets/all-features Clippy passed (2m17s); final fmt and
diff checks passed. No warning suppression or source change followed these gates.

Audio UI selection deletion remains disabled until current occurrence time is
explicit for component/shared-Scope bindings. Group movement/value/interpolation
and rubber-band selection remain open; group keyboard edits do not silently edit
only the primary. The corresponding help is localized in English and Chinese.
CurveEdit and CurveGestureEvent carry owned selection IDs and require Clone;
there is no compatibility shim for their former Copy API.

No fresh native golden executable or encoded-output qualification was built for
this UI source. Historical97e evidence stays source-pinned. The 62 ignored tests
are not counted as completed qualification. The full NLE goal remains active.


## Explicit audio occurrence and admission continuation (2026-10-06)

Timeline now resolves domain-qualified Sequence time into Component or Scope
curve time using the addressed occurrence and exact placement arithmetic.
Shared Scopes require an explicit occurrence; absent, stale, mismatched or
unrepresentable contexts fail without author mutation. Mixer and Inspector use
the live AppState coordinate. Selected Clip Scope racks resolve a unique binding;
multiple bindings in one Clip await an explicit product occurrence selector.

Admitted audio curves support whole-selection Delete through one canonical
RemoveKeyframes transaction, retaining the original evaluated current owner
value when every key disappears. The real CurveEditor/EventRouter acceptance
covers Component and Scope owners, one full Sequence Undo/Redo and Unicode
save/fresh reopen. View identity includes the Scope occurrence, never the transient
playhead. An actual failing widget test exposed fallback to singleton deletion
after refresh removed batch admission; refusal now preserves the complete set
and emits no edit.

Curve widgets: **93 passed / 0 failed** (2.26s). Timeline: **231 passed / 0 failed**.
Full App library: **2558 passed / 0 failed / 62 ignored** (315.08s), including modal
Tab scope, pending Apply focus, stale-resource refusal and real cube pixels through
Undo/save/reopen. Strict Clippy initially caught a test-only time constructor;
production callers now share it instead of repeating its fields. Corrected
whole-workspace/all-targets/all-features Clippy passed (3m17s), followed by affected
audio App tests **134 passed / 0 failed / 7 ignored** and final fmt/diff checks.

Fresh native/encoded qualification now passes on production commit
`99db114278986387ec81f227dfd4cb0256f4d5c3`. Group movement/interpolation,
rubber-band selection, ambiguous same-Clip occurrences and broader plugin/platform
qualification remain open; the overall NLE goal is active.

### Native and encoded evidence for 99db114

The first native invocation built successfully (1131214ms) but could not acquire
a production logical Project lease: Windows access denied before rendering.
Production uses the platform-owned LocalAppData authority namespace, independent
of TEMP; the workspace sandbox did not permit this write. No active product
process was present. Retrying with normal host execution permission, unchanged
source and unchanged binaries, resolved admission without weakening any lock,
identity, timeout or output assertion. The failed report remains retained at
`target/validation/nle-backlog-complete/20261006T011959Z-complete-golden-3b0effdc/complete-golden-consecutive-report.json`.

The qualified consecutive report is
`target/validation/nle-backlog-complete/20261006T014046Z-complete-golden-97f32469/complete-golden-consecutive-report.json`.
It records **3/3 passing rounds**, **baseline_eligible=true**, clean/stable source
attestation, three different Project identities and natural exit in every round.
Round durations: 123405, 120954 and 121013ms. Each round executes all seven
contract stages. The color-media encoded/reimport maximum channel error is
**4/255 in every round** and GPU output CPU fallback count is **0 in every round**.
Frame transport is `cpu_rawvideo_pipe`; this is not a zero-copy or general
performance qualification.

Product worker SHA256:
`2fff0b0f2af4775a04a56b3faa7b9e1edafa0a952da13170ac0e7fb6a29c8cf0`.
Golden SHA256:
`109bce8d4ceff6bd1b45aefabb13b4616b7b7072fb2740fa7ab48b06e030fa63`.
Both match the initial successful build. Native follow-up gates: resource worker
**5/0**, effect curve author edit/Undo/saved Redo/reopen/output **1/0**, modal
**16/0**, resource workflow **103/0** (the five separately executed native worker
cases remain ignored in this non-ignored resource filter), plus integer, dense
curve and raw-float author history **1/0 each**. Final native identity attestation
confirms the same source, same product hash and clean/stable worktree.

Physical keyboard/file picker, high-DPI layouts and macOS remain separately
unqualified. This validates the stated Windows contract paths, not the full
vendor/plugin/platform matrix or completion of the overall NLE goal.


## Shared Scope time-reference selector continuation (2026-10-06)

Ambiguous selected-Clip Scope racks now offer a localized time-reference Dropdown.
Options identify the audio Component and exact Scope start; each Scope remembers
its own explicit choice in Session navigation. The choice validates Sequence,
inspected Clip, full Component address and Scope binding before touching state.
It never mutates author generations/history or persistence and does not prepare
an audio execution generation. Unique bindings retain the direct projection path,
without selector-item construction. Locked Tracks may navigate, but their actual
curve Widget emits no author edit.

Actual keyboard routing selects the second occurrence, updates the Inspector,
and deletes all Scope keys through one canonical command. The retained value is
-6 from the second occurrence rather than -24 from the first. Full author Undo/
Redo and Unicode save/fresh reopen pass; no navigation history is restored.
Stale Sequence, Scope and Component intent refusal and locale-independent action
identity pass. An actual failing cross-Track regression exposed complete-address
navigation matching that discarded an occurrence after a move. Navigation now
resolves stable Clip/Component IDs into the current Track address. Scope curve
view identity also excludes Track locator: key-set snapshots survive that move,
but cannot transfer to another occurrence. Queued action admission stays strict.

Three dedicated current-source cases pass, followed by the full App library:
**2561 passed / 0 failed / 62 ignored** (334.71s). Strict whole-workspace,
all-targets/all-features Clippy passed (2m10s), as did final fmt and diff checks.
Public SelectionState now carries occurrence navigation and AudioProductAction
adds a closed navigation variant; no compatibility shim is introduced for these
alpha Rust API changes. The fresh native/encoded report above remains pinned to
99db114, not to this subsequent selector source. Group movement/interpolation, rubber-band
selection and the broader vendor/platform qualification remain open; overall
NLE goal active.


## Retained native window role and resource retirement (2026-10-06)

Opening and closing a Project now changes Startup/Workspace role on the same
native Window and Surface. Ordinary transitions no longer create another Window,
renderer candidate or GPU device-session handoff. Explicit device/surface recovery
keeps its separate replacement and closure protocol. New root layout and display
refresh carry a distinct RoleChanged reason; same-size transitions still layout.
Native IME, eyedropper, tooltip and router focus/capture state are cleared.

Workspace remains resizable with the existing eight-edge hit routing; Startup
has equal minimum/maximum dimensions and leaves minimized/maximized state. The
normal logical Workspace size and maximized flag survive Project close/reopen;
maximized and minimized extents cannot replace the normal restore size.

A real Windows desktop/GPU test creates a Unicode-path Project, opens Workspace,
closes it and reopens from disk. It verifies the identical Window ID, Surface
generation and GPU device generation; resize/maximized contracts, normal-size
retention, continued surface acquisition and complete GPU/UI/App/background
closure. Its nonempty CPU staging fixture uses a solid-color Sequence in the
canonical Project document and the real playback lookahead path. An actual RED
left one CPU-complete ticketless frame after close reached Startup. Role changes
now release that independent CPU staging queue immediately; submitted GPU owners
still stay behind exact completion callbacks. The corrected native test passes
1/0 (9.71s); the failure and correction logs are retained separately.

Final current-source gates pass: App **2562/0/63 ignored** (294.76s), strict
whole-workspace/all-targets/all-features Clippy with warnings denied (2m04s),
default-feature App build (29.46s), fmt and diff checks. The ignored Windows role
case was separately executed and passed. Fresh native resource/Undo/save-reopen/
LUT-output qualification for the clean committed source is recorded below;
the earlier 99db114 encoded Golden proof remains historical. Physical
interaction, macOS and the broad vendor/plugin matrix remain separately
unqualified. Overall NLE goal remains active.


### Clean-source output qualification for fda9b23

The corrected production source is committed as
`fda9b23b2b47c8bcd4ceeceba6ba76f5bb0a70d7` (Shalom Wang as both author and
committer). Fresh complete Golden passes **3/3**, seven stages per round, with
baseline eligibility true. Starting/ending source match that commit, both trees
are clean, timeouts match the unchanged contract, and every process exits
naturally. Three distinct Project IDs and fresh persisted-session identities
are recorded. Native build took 515292ms; rounds took 121890/120957/120283ms.

Report:
`E:/dev/mondrian/target/validation/nle-backlog-complete/20261006T033630Z-complete-golden-1c145a8f/complete-golden-consecutive-report.json`.
Product worker SHA256:
`f79a8c0b1038a44f3ed6de008eede8e9694c5a8d34d51a5dbfa3d623655a75d3`.
Golden SHA256:
`d42c1ea429b590b56e7f964adef230a11863fc294e0fda3ee0d484e45f86208f`.
Color-stage encoded reimport maximum channel error is **4/255** in each round;
GPU-output CPU fallbacks are **0**. Encoder frame transport remains
`cpu_rawvideo_pipe`; this is not zero-copy or a performance qualification.

Independent same-product native follow-up passes: retained Window/nonempty CPU
staging **1/0**; supervised resource worker **5/0**; native effect curve author
edit/Undo/saved Redo/fresh reopen/output **1/0**; modal Tab/resource focus
**16/0**; resource workflows including real Cube LUT commit/Undo/save/reopen/
output **103/0**; integer, dense curve and untouched raw-float history **1/0
each**. The resource filter's five ignored native cases were executed separately.
The final identity receipt confirms the same source, same product SHA and a
clean/stable tree. Earlier failure evidence remains separate from this passing
qualification. Windows contract paths are accepted within this scope; physical
keyboard/file picker, high-DPI layouts, macOS and the broader vendor/plugin
matrix remain unqualified. Key-group move/value/interpolation and rubber-band
selection remain open. Overall goal stays active.


## Exact selected-key time movement (2026-10-06)

Core translates a nonempty unique complete key set by one exact signed author-time
delta. It sorts the final topology before collision admission and normalizes
constraints once, so adjacent selected keys may pass through one another's old
coordinates. Typed property values, stable IDs and tangent modes survive; stale
or incomplete multichannel selections, collisions and arithmetic overflow reject
the whole candidate. A later channel cannot publish earlier channel changes.

Inspector and audio Left/Right use one exact Sequence frame-grid increment;
Shift uses five. Clip preparation checks the owner interval, and audio projection
checks its explicit Component/Scope viewport. Each accepted key group enters one
author transaction. Capability loss, one locked time policy or an active pointer
operation cannot fall back to a primary-only edit. Transfer curves do not acquire
these operations through view restoration.

An actual component regression exposed that Ctrl/Cmd+A was coupled to group
deletion permission: a movement-only owner could not select its keys. Shift-click
and Ctrl/Cmd+A now admit selection when either group operation is supported and
select authored keys independently of deletability. Virtual boundaries remain
excluded. Movement and deletion separately admit the entire set; movement alone
cannot enable group deletion. The original failure and corrected test are kept
as distinct evidence.

Actual production Inspector/EventRouter tests cover Auto/Continuous visual
curves and audio Component11/Scope7 origins, adjacent group movement, boundary
refusal without generation/history changes, one Undo/Redo, saving an undone
snapshot while retaining live Redo, and fresh Unicode project reopening without
persisted history. Early test setup failures (borrowed temporary and audio
continuous control outside its hard gain range) were corrected in fixtures;
production lifetime and gain validation were not relaxed.

Final current-source gates pass: public Core integration **4/0**, full Core
**464/0/1 ignored**, Timeline **232/0**, Widgets **1093/0** (318.93s), and
all-features App **2564/0/63 ignored** (328.04s). Strict whole-workspace /
all-targets / all-features Clippy with warnings denied passes (5m27s); fmt and
diff checks pass. The App run includes actual modal Tab containment/pending
resource focus, cancelled/locked/replaced/Save As resource admission, and the
real Cube LUT commit/Undo/saved Redo/fresh reopen/pixel workflow. Ignored native
cases were not rerun for this group edit source.

The previously recorded native/encoded qualification remains pinned to fda9b23.
This continuation does not claim that historical Golden processes exercised its
new group operation. Dedicated native group-action/output, group value/pointer/
interpolation, rubber-band selection and broad vendor/platform/performance
matrices remain open. The overall NLE goal stays active.


## Definition-backed audio preview admission (2026-10-06)

A reproduced production regression showed that moving selected Continuous Bezier
keys could turn an endpoint into an interior key and generate a new incoming
control below the gain floor. Canonical author admission rejected the edit, but
the UI previously installed the invalid preview. The original failure is kept
separately from corrected results.

Inspection now resolves an immutable `AudioAutomationCurveContract` from the
actual gain, pan or Processor definition. Preview and fresh author admission use
one numeric schema validator for parameter identity, values and absolute
controls, animation permission and interpolation families. Author publication
uses the prepared curve after complete Audio Program validation. UI actions and
geometry come from the same admitted candidate. Schema-free preview APIs and a
redundant parameter-identity diagnostic were removed; no compatibility path was
added. Canonical pan keys and controls now admit the declared [-1, 1] range.

Two Timeline contract cases cover gain, pan, Processing Scope, wrong identity,
integer values, interpolation families and non-animatable parameters. Two App
regressions cover the original lower-bound failure and actual Shift-click group
selection followed by a five-frame move across a neighbor. The latter checks
upper-bound refusal with unchanged geometry, selection, author generation and
history, and legal crossing with stable IDs, one Undo/Redo, saved undone state
with live Redo, and fresh Unicode project reopening with empty history.

Full current-source Timeline **234/0**, audio **105/0/11 ignored**, and App
**2566/0/63 ignored** pass. App runtime is 326.00s. This App run explicitly
passes actual modal Tab/Shift+Tab containment and background focus restoration,
pending resource focus after disabling confirmation, edited/cancelled/locked/
replaced drafts, Save As directory invalidation, and real Cube LUT commit/Undo/
saved Redo/fresh reopen/pixels. Ignored native cases are not counted as passes.

Strict whole-workspace/all-targets/all-features Clippy with warnings denied
passes (4m09s), as do final fmt and diff checks. Native/encoded qualification
remains pinned to fda9b23;
this source-level run does not qualify a new binary. Specific native group
output, group value/pointer/interpolation, rubber-band selection and the broad
vendor/platform/performance matrices remain open. The full NLE goal is active.


## Native group time and constrained curve output

The dedicated `curve_group_authoring` SessionProbe mode asserts exactly three
keys, their key-index/time/value queries, and out-of-range index refusal through
native instance lifecycle callbacks. Derivative queries are compared to symmetric
value samples. Integral queries are compared to independent Simpson quadrature
split at exact keys, including the clamped tail. Native output uses the curve
value at off-key frame 3, compared with canonical author evaluation. Selected
keys move from frames 6/12 to 12/18; queries at frame 12 change from 3 to 2.
The moved author curve must differ from a linear downgrade at frame 3, and
native pixels must match its constrained value.

The App test uses production `ShiftMany` actions for Auto and Continuous Bezier.
A collision with the unselected key refuses the complete group without author
state, generation, history or output changes. Legal movement retains all stable
key IDs in one transaction. One Undo restores the original native output; saving
that state retains live Redo, and a fresh Unicode `.mdp` reopen restores the
original curve with empty history. Redo and a separately saved edited archive
restore the changed native output. SQLite library snapshots remain empty.

Runtime qualification status is recorded only after the explicit ignored tests
run against the rebuilt product. Existing single-key native assertions remain
unchanged and are rerun separately to detect fixture regression. This contract
adds no second authoring or execution interpretation.

The candidate-source explicit ignored group test passes **1/0** (both constraints,
4.71s); the existing single-key native test passes **1/0** (2.18s). Product build
completed in 18m20s; the test executable rebuilt in 2m17s. Candidate source,
product and fixture hashes remained stable. The source was uncommitted during
this run, so it is not represented as a clean-source encoded qualification.
Final strict workspace/all-target/all-feature Clippy passes (1m59s), as do fmt and diff checks. Clean committed-source native attestation follows separately.
Complete Golden remains pinned to fda9b23, and the full NLE goal remains active.


### Clean-source native qualification for 18b3a82

Production and native-test source is committed as
`18b3a82907f3c02080d5431d8e7106195191f381`, with Shalom Wang as both author and
committer. The clean-source product rebuild passes (7m54s). The qualification
starts and ends on this exact commit with a clean tree and stable binary/fixture
hashes. The new group test explicitly executes both Auto and Continuous curves;
its off-key pixels match canonical author evaluation and differ from a linear
downgrade after movement.

| Current-source scope | Executed result |
| --- | --- |
| Group native curve, collision, Undo, both saved states and output | 1/0, 4.33s; both tangent constraints |
| Original single-key native curve and output | 1/0, 2.20s |
| Native retained Window/Surface/device role cycle | 1/0, 10.31s |
| Native resource helper, cancellation/timeout/retry and Unicode persistence/output | 5/0, 8.51s |
| Modal Tab containment and pending-resource focus | 16/0, 22.48s |
| Resource workflows including real Cube LUT commit/Undo/save/reopen/pixels | 103/0, 28.45s; five ignored native cases executed separately |
| Exact integer, dense temporal and untouched Float history | 1/0 each, 1.78s / 4.10s / 0.60s |

Product SHA256:
`c59f8667af36e884cc745c5d7e67e73d5529fca550cf801c8f79f2f0ac878c8f`.
SessionProbe SHA256:
`841520a969b43ba1859a22394f62ccf6aeea62069f41a5c0fbb556ce2be8d456`.
The source/binary/fixture receipt records `CleanStable = true`. Filter counts
may overlap and are not added as unique coverage. Candidate-source compiler
setup failure and precommit runtime results remain separate historical records.

This qualifies the listed Windows native contract paths. It does not qualify a
new complete encoded Golden, physical keyboard/file-picker/high-DPI interaction,
macOS or broad vendor/plugin/performance matrices. Complete encoded Golden
remains pinned to fda9b23. Group value/pointer/interpolation/rubber-band work and
the original full professional NLE scope remain open; the goal stays active.


### Current complete encoded Golden for d2b22be

The complete Golden gate passes **3/3** on clean committed source
`d2b22be0ce80ccbec41a94d4be2af45ab8df7cc8`. Starting and ending revisions match,
both trees are clean, baseline eligibility is true, and the unchanged contract
owns the 3600s build and 1800s process timeouts. Every round executes all seven
stages and exits naturally with code 0; no terminal-report forced termination
was needed. Three distinct Project IDs and fresh durable-reopen Session IDs
are recorded, while Project and authored Sequence identities survive reopening.

Report:
`E:/dev/mondrian/target/validation/nle-backlog-complete/20261006T072224Z-complete-golden-c36aa254/complete-golden-consecutive-report.json`.
Build elapsed time: 26833ms. Round durations: 122068/120518/132848ms.
Golden binary SHA256:
`827638de8b85e867504e4791bb9805e3abdeeabb73ce2a9ff4c32ef25f8fdd55`.
Product worker SHA256:
`c59f8667af36e884cc745c5d7e67e73d5529fca550cf801c8f79f2f0ac878c8f`.

The product worker is exactly the binary from the clean 18b3a82 native group,
resource, modal, retained-window and history qualification. The difference
between those source commits is documentation only. Golden additionally covers
foundation audio authoring, editorial transport, proxy/relink, generated delivery
roundtrip, visual authoring roundtrip, recovery/nesting and color/media roundtrip.
It does not replace the explicit native group action test with a presumed Golden
operation.

Color-stage encoded reimport maximum channel error is **4/255** in every round;
GPU-output CPU fallbacks are **0**. Frame transport remains `cpu_rawvideo_pipe`,
so this is not a zero-copy or performance qualification. Earlier fda9b23 receipts
remain historical. Group value/pointer/interpolation/rubber-band work, native
audio group execution corners and the broad vendor/platform/performance matrices
remain open. The overall NLE goal stays active.


## Complete-key common value displacement: domain qualification

The Core mutation `OffsetKeyframeValues` addresses a nonempty unique set of
complete stable key identities and one declared numeric channel. It derives one
common displacement from the intersection of every selected key's legal motion
interval. Integer source values never pass through floating projection. Floating
boundary displacements are applied directly when finite; scaled step intervals
and fused arithmetic retain support for overflowing finite spans. A subnormal
step fraction cannot suppress a nonzero finite boundary displacement. True
zero-motion groups preserve signed-zero bits.

`AnimatedProperty` and `PropertyBag` publish only an admitted complete candidate,
normalizing Auto/Continuous tangents once. Exact times, IDs and other channel
values survive. The command has path remapping and rejects empty, duplicate,
stale, incomplete-channel and invalid numeric requests even when the requested
increment is zero. Original singleton arithmetic remains unchanged.

Candidate-source evidence: `group-value-core-full.log` passes **469 unit tests**
with one existing ignored test, six public batch tests and nine other integration
tests. The final expanded public batch run `group-value-core-batch-final.log`
passes **8/8**, adding Float inward representable bounds and visual control
admission. These runs cover **486 distinct passing tests** in total. Actual
tiny-range underflow and the subsequent zero-motion guard regression retain their
RED logs and corrected assertions. An initial control test incorrectly assumed
visual controls must remain in key hard bounds; it was corrected against the
existing visual contract. Finite visual Bezier overshoot remains legal, while
nonfinite absolute controls refuse the whole edit. Audio definition admission
continues to own its stricter control-value contract.

This qualification covers the domain command and canonical serialization.
Widget lowering, audio definition integration, one-command Undo/Redo, project
save/reopen and native output for group **value** edits remain open. The earlier
clean native/Golden receipts qualify their recorded group **time** revisions;
they do not qualify these new source changes. The full NLE goal stays active.

Final domain gate: whole-workspace, all-target, all-feature Clippy with warnings
as errors passed in **5m14s** (`group-value-workspace-clippy.log`); formatting and
diff whitespace checks passed. These are local candidate-source gates, not a
remote CI or native production requalification.


## Complete-key common value keyboard authoring and durable history

The actual visual and audio curve controls now admit selected-key Up/Down
increments as one common displacement; Shift requests five increments. Group
value permission is independent of deletion and time movement, and selection
uses stable authored IDs. Whole-set value locks reject before intent dispatch.
Core exact automation uses the same numeric displacement contract as visual
properties; audio supplies the current parameter definition and checks absolute
control bounds before either preview installation or author publication.

A real application RED exposed an unexecuted-action error at the common hard
boundary. The shared visual/audio preview callbacks now compare the complete
candidate with the previous live display source and emit no intent for an equal
candidate. The snapshot precedes pointer-origin reset, so a changed pointer
candidate returning to its original author state still emits a necessary update.
The original failure log `group-value-app-history.log` remains as evidence; the
boundary assertion now requires zero emitted actions.

Candidate-source gates:

- Core public complete-key tests: **9/9**, including exact audio Auto/Continuous
  constraints, identities, rational times, common limits, no-op and codec.
- Timeline full suite: **236/236**, including shared preview/author admission,
  current **+24 dB** gain hard limit, owner locks, complete-set refusal, illegal
  new control bounds and subsequent legal recovery.
- Curve Editor regression suite: **83/83**, including independent group
  capability, selection retention, complete-set value locks, pointer-coordinate
  value locks, singleton value increments, cancellation and exact time behavior.
- Application full all-feature library suite: **2567 passed**, **64 ignored**,
  zero failures, runtime **320.45 seconds**. The ignored native/environment
  gates have not been executed by this run.

The new actual-widget history test covers visual opacity and audio channel fader,
with both Auto and Continuous interpolation: Select All then Shift+Up publishes
one author command and one generation, preserves key IDs and exact times, moves
values to one shared limit without changing group spacing, and refuses to emit
another command at that limit. One Undo restores the complete original sequence;
one Redo restores the complete edit. Separate Unicode `.mdp` archives for both
states are opened in fresh AppStates, with exact sequence/value equality and empty
session histories. Saving the undone state preserves the live session's Redo.

The full application run also passes the actual resource LUT draft, commit,
Undo, save/reopen and real Cube execution test, plus existing pointer-origin,
Escape, late-lock and intervening-author-edit regressions. This is local editor
and persistence evidence, not native plugin pixel or encoded production output
qualification for group value edits. Group pointer/bulk interpolation/rubber-band,
new native group value output and broader vendor/platform/performance matrices
remain open. The full NLE goal stays active.

Final integration gate: whole-workspace, all-target, all-feature Clippy with
warnings as errors passed in **5m12s** (`group-value-integration-clippy.log`).
The tested integration scopes comprise **2895 distinct passing cases**; the
application’s 64 ignored cases require separate native/environment qualification.


## Native group value candidate and shared fixture regression

The rebuilt production worker and SDK SessionProbe pass all three explicitly
executed native tests: group value (5.48s), group time (5.52s), and single-key
history (2.74s), one test per filter, zero failures and zero ignored tests.
These runs used candidate source based on `3e814c7`; clean-source qualification
is a separate gate. Product SHA256 is
`fa43de9bb770faf2725fa71f6e8d21ffbed3a88a1e3962d84bfac18549b3f3d5`;
fixture SHA256 is
`fabf11ea8d81c2bf8f698d854b8bf4568597a4e11b26f5bd45d30bd124cebb5c`.

The shared history runner covers Auto and Continuous curves, complete-selection
atomic rejection, one-command Undo/Redo, saving with live Redo retained, and
fresh Unicode archives for both history states. The new value case preserves
all key times and the unselected first key, and moves the remaining values by
one common offset. Native key count/time/index/value queries, derivative and
integral queries run at lifecycle and render actions. Off-key native HDR and
negative float pixels match the canonical compiled graph exactly after edit,
Undo, Redo and reopen. This fixture remains separate from the official SDK
reference; it does not qualify arbitrary vendor plugins or other platforms.

Whole-workspace all-target/all-feature Clippy with warnings as errors passed
in 2m05s; formatting and diff whitespace checks passed before this commit.


## Clean-source native and encoded qualification for d5ed1ae

Current source `d5ed1ae6865c19cf9ac89af10ef76005f93ec0d8` remained clean and
unchanged throughout native and complete encoded qualification. The product and
SessionProbe hashes match the candidate hashes recorded above. Explicit native
execution passed group value/time, single-key history, actual-widget group
history, window role transitions and integer/dense/Float history (one per
filter), modal focus (16), resources (103), and five additional ignored native
resource cases. The five ignored cases in the ordinary resource filter were
executed separately, and are not counted twice. The resource scope includes
real Cube commit/Undo/save/reopen/pixels, stale and locked drafts, cancellation,
timeout and retry. Modal scope checks Tab/Shift+Tab containment, blocked
background input, disabled pending confirmation and restored focus.

The complete Golden contract `windows-alpha-golden-v14` passed **3/3** rounds,
all seven stages each round, three distinct projects, fresh session reopen and
natural process exit 0 with no forced termination. Round durations were
123097, 118640 and 119020 ms. Every round had H.264 decoded maximum channel
error **4/255** and **zero GPU output CPU fallbacks**. The DX12/NVENC path still
uses `cpu_rawvideo_pipe`; this receipt makes no zero-copy or performance claim.
Source attestation is clean/stable and `baseline_eligible` is true.

Aggregate report:
`E:/dev/mondrian/target/validation/nle-backlog-complete/20261006T095123Z-complete-golden-4b744355/complete-golden-consecutive-report.json`.
Golden harness SHA256:
`fb944d4b5b86d4f574cca6d50cb548101a641be3aba4bd16488b5abd101fe134`.
The packaged product worker SHA256 matches the native qualification product.
These are current Windows contract results; group pointer/bulk interpolation,
audio group native output, arbitrary vendor plugins, macOS and remaining
professional/performance matrices are still open. The full NLE goal is active.


## Complete-selection interpolation domain and preset display

A new Core complete-key interpolation mutation validates nonempty unique stable
identities across all channels, applies the preset to the complete selected set,
and normalizes constraints once before publication. Public tests cover all seven
presets, reversed selection order, raw channel values, exact owner times, stable
identities, idempotence, nested path mapping and serialization. Empty, duplicate,
stale, independent-channel and current-schema-incompatible selections reject
without any channel changing.

A real runtime RED found that correctly stored EaseIn/EaseOut asymmetric sides
were collapsed to Bezier by the shared current-mode resolver. The resolver now
retains the asymmetric preset after Hold and constrained-mode precedence. The
exact-mode assertion was retained. Actual App selection tests verify both ease
modes and exclusion of the current preset from available menu actions.

Core full suite: **489 passed**, zero failures, one ignored performance test;
public complete-key batch tests account for 11 of those passes. Actual App
animation-selection suite: **6 passed**, zero failures/ignored, runtime 0.39s.
This is domain and selection-display qualification. Bulk Inspector menus, audio
batch interpolation admission, one-command batch UI Undo/save/reopen and new
native output remain open; earlier native/encoded receipts qualify their named
source revisions. The complete NLE goal remains active.

Final domain/display gate: whole-workspace all-target/all-feature Clippy with
warnings as errors passed in **5m11s**. No remote CI or current native output
qualification is inferred from this gate.


## Exact audio complete-selection interpolation admission

ExactAutomationCurve now prepares a complete nonempty unique stable-key selection
in canonical owner-time order, applies one supported preset, initializes entering
Continuous constraints and normalizes once before validation/publication. The
singleton API delegates to that primitive. Public batch tests pass **13/13**,
including scalar bit retention (negative zero), stable IDs and exact times,
reversed selection order, idempotence, serialization, singleton equivalence and
atomic rejection of invalid selections/unsupported asymmetric presets.

AudioAutomationEdit::SetInterpolations uses the same current definition-family
admission as singleton edits and checks the entire prepared curve, including
new absolute controls, before either preview or author publication. Timeline
full regression passes **238/238**. New cases check all five supported presets,
preview/author equality, intent codec, complete-set refusal, no-op and owner locks;
integer parameter definitions refuse unsupported batch interpolation families.
A legal manual source can produce a Continuous control below the gain floor:
preview and author both refuse the complete edit with the same InvalidGain error,
retain the whole Sequence, then permit a legal Auto recovery. These are local
candidate-domain results; UI batch context menus, session Undo/save/reopen and
native audio group output still require separate qualification. Full NLE goal
remains active.

Full Core regression after singleton delegation: **491 passed**, zero failures,
one ignored performance test; unit runtime 6.81s, build 1m54s. The public 13-case
batch suite is included in that count. Timeline build was 3m40s.

Final exact-audio domain gate: whole-workspace all-target/all-feature Clippy
with warnings as errors passed in **5m23s**; format and diff checks passed before
commit. This qualifies the current domain source, not end-to-end UI/native audio.


## Actual complete-selection interpolation menus and durable history

Visual and audio curves now build group menus from stable selected keys on
opening. Right-clicking a selected key and Shift+F10 retain the group; Escape
closes the menu without editing. Interpolation selection is independent of
coordinate movement/deletion permissions, so Ctrl/Cmd+A and Shift-click still
work when only menu commands are admitted. Active pointer drafts refuse menu
opening; cancellation restores normal access. Labels and help use existing
localized presets. Checked current presets remain visible and disabled, and
mixed selections have no checked preset.

Visual singleton and group author preparation share one canonical normalized
candidate, replacing application-specific mode equality branches. Both visual
and audio menu adapters preflight current-definition candidates and suppress
no-op actions. Formal dispatch rechecks the current author state.

Verified candidate gates so far: Curve Editor **85/85**, zero failures/ignored,
runtime 2.24s, including independent menu selection and pointer-draft refusal.
The actual App menu-history filter passed **1/1** (3.70s), covering four cases:
visual opacity/audio fader times Auto/Continuous, starting with a mixed Hold/
Linear selection. Real EventRouter Tab/Ctrl+A/Shift+F10/Down/Enter publishes one
intent and author generation, preserves exact key identity/time/value, and keeps
the selection. One Undo and Redo restore complete author snapshots; both Unicode
archives reopen in fresh sessions with empty histories. Saving undone retains
live Redo. A refreshed audio model emits no action for the current group preset.

The current complete App regression passed **2569 tests**, zero failures and
65 ignored, in 321.86s. This includes captured menu intents refused after a late
owner lock or removal of a selected key, with the complete author snapshot,
generation and history unchanged. Ignored native tests are not counted as passes.
Strict whole-workspace, all-target, all-feature Clippy with warnings as errors
passed in **2m24s**.

Both single-key and group menus use the opening-time factory. The production
adapters no longer preflight seven candidates per key during UI reconstruction.
A callback-count test verifies no factory invocation on construction or selection,
then checks keyboard single selection and right-click replacement of the addressed
selection. This removes the introduced quadratic preflight work; no latency or
allocation benchmark is claimed. Current native/plugin/encoded output has not yet been
qualified for this menu source. Group pointer/rubber-band and the broader full
NLE vendor/platform/performance/i18n/tracking matrices remain open.


## Clean-source native and encoded qualification for ce3edfb

On 2026-10-06, source `ce3edfb51ac27476c7b35bd829405cdacb59afa2` was
clean and unchanged through the product rebuild, twelve current acceptance
filters and the complete three-round Golden. Product rebuild took 19m46s.
Both Git author and committer are Shalom Wang; no push was made.

| Identity | SHA256 |
| --- | --- |
| Product `mondrian.exe` | `37be9e8a545c84225b7c1fb43a7d0b7155fb2752ed97b99119e4be1c4e41dcba` |
| Owned SessionProbe fixture | `fabf11ea8d81c2bf8f698d854b8bf4568597a4e11b26f5bd45d30bd124cebb5c` |
| Golden executable | `22ef0cb2aee3a387fdac2f6fc71e232dfa5cb3b1c4db24f5de019404a4c7cb16` |

The twelve filters passed: native group value and time (one each), actual group
interpolation and value menu histories (one each), retained native window role
(one), explicitly ignored resource supervision/native execution (five), native
single-curve Undo/saved Redo/reopen/output (one), modal focus/input (16), resources
(103; five ignored cases already executed separately), exact integer history,
dense temporal history and raw Float history (one each). Filter counts overlap;
they are not a sum of unique tests. These current results include pending Apply
Tab/Shift+Tab containment, cancellation/stale/locked resource refusal, Unicode
resource paths and archives, real LUT float pixels, live Redo after an undone save,
and fresh reopened history. The menu-history filter covers complete selection
interpolation through the actual EventRouter and author transaction; it does not
claim a native interpolation-specific or native audio output probe.

Complete Golden passed **3/3**, seven required stages per round, baseline eligible.
The process elapsed times were 120278, 120271 and 119359ms. All three natural
exits were 0, with no forced exit after the terminal report and distinct project
identities. Source attestation starts and ends at ce3edfb, both clean and stable.
Final H.264 decoded maximum channel error was **4/255** in every round; GPU
output CPU fallback count was **0**. Encoding used the existing rawvideo handoff;
this is not a zero-copy or throughput qualification.

Aggregate receipt:
`E:/dev/mondrian/target/validation/nle-backlog-complete/20261006T120747Z-complete-golden-5da120b3/complete-golden-consecutive-report.json`.
The source-level App 2569, Curve Editor 85 and strict workspace Clippy gates are
recorded immediately above. The complete NLE goal remains active: native group
interpolation/audio, group pointer/rubber-band/copy/reset and wider vendor,
platform, performance, tracking and i18n matrices still require work. An
independent diagnostic on the actual compiled Timeline enum also reproduced
acceptance of omitted native audio binary revision fields; that explicit-null
contract gap is tracked for a source fix, without invalidating the named valid
resource/output acceptance above.


## Required native audio definition revision fields

A diagnostic against the actual compiled Timeline enum reproduced omission of
`binary_sha256` being accepted as an unbound CLAP or VST3 definition. The initial
diagnostic build selected incompatible cached Serde metadata and failed to
compile; fingerprint-matched compilation then succeeded and reproduced both
formats at runtime. The formal crate regressions also failed before the fix:
missing revision fields were inferred, and unknown definition fields disappeared.
Neither compiler errors nor the prior passing explicit-null tests proved this
input contract.

Both native revision fields now use one required nullable decoder, with no
field default or migration alias. Explicit null remains a canonical unbound
state for editing and explicit rebind; valid pinned revisions preserve their
bytes. Built-in, CLAP and VST3 definition objects reject unknown fields. The new
regressions cover both native formats for missing/pinned/null input and all three
definition variants for unknown input, alongside the existing zero-revision
validation and rebind tests.

Current candidate Timeline regression: **240 passed**, zero failures/ignored,
0.75s runtime after 1m50s build. Project persistence regression: **49 passed**,
zero failures/ignored, 1.51s runtime after 2m44s build. Strict whole-workspace,
all-target, all-feature Clippy with warnings as errors passed in **3m57s**.
Final formatting and diff checks precede the source commit.
The ce3edfb native/Golden receipt above qualifies its named earlier source;
these parser tests do not claim a rebuilt native interpolation/audio output
qualification. The full professional NLE goal remains active.


## Native complete-selection interpolation candidate

The new native group interpolation probe passed **1/1** (5.18s), covering Auto
and Continuous from nonuniform Linear keys at frames 0/6/12 with values 1/2.5/3.
One stable-ID complete-selection author request preserves exact IDs, times and
values and sets every constraint intent. A stale member refuses the complete
edit without generation or history changes. One Undo restores the original
curve and pixels; saving undone retains live Redo. Redo and fresh Unicode
undone/redone archive reopening preserve complete author snapshots, native SDK
queries and shared-graph float pixels, with empty history in fresh sessions.

The fixture samples off-key frame 3. Auto must change the nonuniform Linear
shape. Continuous is checked for canonical constraints and matching native
output, allowing legitimate unchanged geometry. SDK key queries retain all
three exact key coordinates; independent symmetric derivative queries and
Simpson integration split at keys/clamped tails keep their original tolerances.
The shared test helper uses a Time/Value/Interpolation enum, and the owned C++
fixture centralizes admission of those modes. Existing native time **1/1**
(4.26s) and value **1/1** (4.83s) regressions passed after the fixture change.

Candidate baseline: `f83d25612a26f1a5b34d5f1b7b6ed6cf0a55ee22`, with test-only
Rust and owned-fixture edits. Official SDK source remained clean at
`e40728885390ec16276d11e00025de9b4282060c`. C++ fixture build passed in 7.07s;
App all-feature test compilation passed in 7m23s; product rebuild was 15m10s.

| Candidate execution identity | SHA256 |
| --- | --- |
| Product | `b3e285576773d238279d5ef9bccfdadeec9f986687d1a7eb7af940da4a57e489` |
| SessionProbe fixture | `ba8734fe2fc3cef93d0973415b8c4fee64b1a1c0cea9ecdbd409b2401781978a` |

Source/product/fixture hashes stayed stable during the run. This was an
**uncommitted candidate**, not a clean-source receipt. Strict whole-workspace,
all-target, all-feature Clippy with warnings as errors passed in **2m03s**.
Final format/diff checks precede commit and clean-source native qualification. No current encoded or native
audio group output is claimed by this probe. Professional group pointer,
rubber-band, copy/reset and all broader original NLE matrices remain open.
