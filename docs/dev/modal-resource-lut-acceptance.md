# Modal, resource and LUT acceptance

## Qualification scope

The latest recorded clean-source native/encoded result is
[fda9b23 qualification](#clean-source-output-qualification-for-fda9b23).
Subsequent sections record newer source-level tests separately. Native results
qualify the named binary and source; they do not automatically cover later edits.
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
