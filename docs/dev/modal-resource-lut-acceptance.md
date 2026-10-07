# Modal, resource and LUT acceptance

## Captured menu switching and explicit submenu activation

The 2026-10-07 UI follow-up to `2559c41` reproduced three runtime failures:
actual Host hover could not switch File to Help; Enter on a submenu executed its
first descendant; Enter after pointer movement cleared the highlight still
executed the first command. The retained regressions now pass. MenuBar coordinates
captured sibling hover/click through the existing ancestor seam, restricted to
command chrome. Normal focus transitions preserve the original editing owner.
Dropdown and ContextMenu share depth-aware keyboard navigation and activate only
the selected leaf. Mouse-opened context menus require explicit selection;
Enter/Space without a highlight refuses without dispatch. No compatibility
fallback, second clipboard or alternate author transaction was added.

Actual Host tests cover hover, click, keyboard wrapping and exact About dispatch,
plus File-to-Edit Cut with one restoring Undo and cancellation retaining selection.
Shared widget tests cover nested entry, disabled/separator rows, empty/stale paths
and no-highlight refusal. Asset and timeline integration tests explicitly select
rows and retain their exact action, target, selection and rename assertions.
Group interpolation independently passed one-commit and both saved history states.

Final all-feature library gates: App **2601 passed, 0 failed, 68 ignored**
(360.81 s); UI events **77 passed, 0 failed, 0 ignored** (0.01 s); UI widgets
**1105 passed, 0 failed, 0 ignored** (297.80 s). Strict workspace, all-target,
all-feature Clippy with `-D warnings` passed in **2 min 34 s**; fmt and diff checks
passed. App includes both actual modal Tab cases and the real cube resource
transaction/Undo/save/reopen case. The only changes after the full App and events
gates were explicit selection steps in widget tests; production source was frozen.

This follow-up is a UI and authoring-regression qualification. Native plugin and
encoded LUT output evidence below remains bound to clean `ee310cc`, its recorded
binaries and three Golden runs. Submenu pointer/viewport edge cases, audio curve
clipboard, read-only curve interaction, group pointer operations and the broader
vendor/platform/performance matrices remain open in the active overall goal.

## Focused visual curve Cut and atomic clipboard publication

The actual Host/EventRouter Ctrl+A then Ctrl+X regression retained the Clip but
failed to remove its selected curve keys (0 passed, 1 failed, 2.84 s). The closed
curve command now carries Cut with the exact Clip, parameter instance and stable
key IDs. Capture prepares the complete canonical clipboard without publishing;
one author transaction removes the group, then publishes clipboard and selection.
Empty selection and pointer drafts consume the focused chord. The local numeric
curve menu uses the same command. No global Clip fallback or second clipboard
authority was added.

The same Host regression passed after the fix (1 passed, 0 failed, 2.70 s),
including owning Clip retention, complete key removal, one generation advance and
one restoring Undo. Additional closed-command tests cover seven refusal cases
with both old clipboard kinds, preserving project, clipboard, selection and
history. A separate case preserves another parameter's selected key and verifies
Undo, an undone Unicode save, live Redo after save, a redone save and fresh reopen
of both documents with empty transient history and clipboard.

Final App all-feature library suite: **2585 passed, 0 failed, 68 ignored**,
346.89 s. Changed CurveEditor module: **87 passed, 0 failed, 0 ignored**, 2.29 s;
this is the module filter, not the entire widgets suite. Strict workspace,
all-target, all-feature Clippy with `-D warnings` passed in 2 min 32 s.
The clean-source qualification below now includes this Cut implementation.
The root Edit menu follow-up is described below; audio clipboard and read-only
curve interaction remain open in the overall goal.

## Root command menus and text editor ownership

A real Host/EventRouter regression opened Edit after selecting visual curve keys,
clicked Cut and observed the owning Clip disappear (0 passed, 1 failed, 2.39 s).
Root Cut/Copy/Paste now carry typed semantic requests. The router preserves the
original editor while menu chrome receives focus, projects its command readiness,
and executes its owned command after closing the menu. Missing, disabled and
out-of-modal owners refuse without falling through to Clip operations. Global
shortcut overrides share this route; no clipboard payload or author state is
cached in the menu.

The six actual Host cases cover row Cut, standard and customized shortcuts,
Copy/Paste with fresh IDs and exact complete channels, cancellation and empty
selection. Successful edits retain the Clip, advance one author generation and
are restored by one Undo. Cancellation retains selection, clipboard and history.

Two further runtime regressions caught Save before pending text confirmation
(0 passed, 1 failed, 1.84 s) and live IME preedit after menu cancellation
(0 passed, 1 failed, 1.94 s). Non-clipboard requests now confirm the original
editor before dispatch; opening command chrome cancels provisional IME before
disabling it and preserves confirmed draft text. Save and cancellation regressions
use actual text components, Dropdowns and EventRouter.

The actual multiline text regression initially emitted Clip Cut when the platform
clipboard write failed (0 passed, 1 failed, 1.82 s). MultilineTextInput now invokes
its existing text commands, consumes owned refusals and preserves Unicode text on
failed writes. Read-only fields allow Copy, refuse Cut/Paste and keep disabled menu
rows inactive. The four Save/IME/multiline cases passed together (4 passed,
0 failed, 0 ignored, 7.24 s).

The two standalone MenuBar tests now assert the exact typed About request and no
premature dispatch. Their earlier expectation of direct dispatch contradicted the
new confirmation seam; no production compatibility path was added.

Current full source gates: App **2595 passed, 0 failed, 68 ignored** (349.44 s),
UI core **84 passed, 0 failed, 0 ignored** and UI events **76 passed, 0 failed,
0 ignored**. Both actual modal Tab tests passed, including traversal after resource
Apply is disabled and restoration of background focus. Full UI widgets passed
**1100 passed, 0 failed, 0 ignored** (310.02 s). Strict workspace, all-target,
all-feature Clippy with `-D warnings` passed in 2 min 41 s; fmt and diff checks
passed. The clean native/output evidence below belongs to its named historical
source; the clean ee310cc qualification below renews it for this implementation.
Audio curve clipboard, read-only curve selection, group pointer interactions and
the broader professional workflow matrix remain open in the overall goal.

## Clean ee310cc command routing, modal, resource and LUT acceptance

Clean production `ee310cc3868c146e50f2eea07c5122ff6281d3fe` was rebuilt and
qualified on 2026-10-07. The actual product and Golden build passed in **559227 ms**.
Complete Golden passed **3/3**, all seven stages each, distinct project IDs and
natural exit 0 with no forced exits. Process elapsed times were **121370, 124161
and 135105 ms**. Starting and ending source were identical and clean, with stable
attestation and baseline eligibility. Build/process timeouts remained 3600/1800 s.

Every final H.264 export/reimport retained maximum channel error **4/255** and
**0 GPU output CPU fallbacks**. Encoder handoff was `cpu_rawvideo_pipe`; this
qualifies the executed output contract without a wider performance claim.

| Explicit source-bound gate | Executed result |
| --- | --- |
| EventRouter modal containment | 1 passed, 0 failed, 0 ignored; 0.00 s |
| Native resource paths, cancellation, timeout and retry | 5 passed, 0 failed, 0 ignored; 8.31 s |
| App modal Tab and disabled Apply traversal | 16 passed, 0 failed, 0 ignored; 24.45 s |
| Atomic resource drafts, real cube Undo/save/reopen | 103 passed, 0 failed, 5 ignored; 24.78 s |
| Native OpenFX group interpolation/history/queries/output | 1 passed, 0 failed, 0 ignored; 5.39 s |
| Native OpenFX group time/history/queries/output | 1 passed, 0 failed, 0 ignored; 5.60 s |
| Native OpenFX group value/history/queries/output | 1 passed, 0 failed, 0 ignored; 5.53 s |
| Native CLAP/VST3 group interpolation/Undo/Unicode reopen/output | 2 passed, 0 failed, 0 ignored; 37.67 s |
| CLAP sample-accurate event ranges | 1 passed, 0 failed, 0 ignored; 0.89 s |
| VST3 linear parameter queues | 1 passed, 0 failed, 0 ignored; 1.43 s |
| VST3 state restoration and isolated-worker audio | 1 passed, 0 failed, 0 ignored; 1.07 s |
| Native CLAP/VST3 preview execution | 2 passed, 0 failed, 0 ignored; 2.17 s |

The resource gate's five ignored cases were explicitly executed by the native
resource gate. Filters overlap; their counts are not a count of unique tests.
All twelve filters required positive exact execution counts. Product and all five
used plugin binaries retained identical SHA256 hashes through the qualification.
Owned fixtures exercise their documented SDK event/queue contracts; external
examples retain their static reference scope. Broader vendor, bus, editor,
platform and performance matrices remain separate work in the active full goal.

Qualified SHA256 identities:

- Product: `CBD2C6B00A93BF3A863EFDAE602CBFED383BCB4B3B7347F82EC6F21F4FAE0463`
- Golden: `BDF39466A0801F5331533640FC896174E75679712A913FE4CB4762FA72FCAF90`
- clack_plugin_gain.dll: `171222FFE85360DBD5BDC8791073FFE7603D178E93118DFB7E15B42B1891039C`
- mondrian_clap_sample_gain_fixture.dll: `CB2FA1B4956CEDBDA4BBAA4B24EFA2D16924820C7C7FCEB4E4881633F21FD687`
- SessionProbe.ofx: `BA8734FE2FC3CEF93D0973415B8C4FEE64B1A1C0CEA9ECDBD409B2401781978A`
- mondrian_vst3_stateful_gain_fixture.dll: `523E714760F9BC47805B3EDE7C7FC4399AEF7D1BDEEF1DCC3832B6DA50D9907F`
- Gain.vst3: `CBCA9C3E9ED68554A5877889A81264D5C4910DEE38FCF02706272B0BB99F1AF5`

Consecutive report:
`E:/dev/mondrian/target/validation/nle-backlog-complete/20261006T230013Z-complete-golden-838960f6/complete-golden-consecutive-report.json`.
This is the latest executed native/output scope and supersedes the historical
26696ef scope below for these cases. The full changed-library and strict Clippy
results above qualify the same production implementation. No push was performed.

## Clean 26696ef modal resource LUT and native plugin acceptance

Clean source `26696ef19c281ee602cac5a94877b83594d55257` was rebuilt and
qualified on 2026-10-07. Build passed in 549962 ms without timeout. Complete
Golden passed **3/3**, all seven stages each, distinct project IDs, natural
exit 0 and no forced exits. Elapsed times were 120404, 120340 and 119887 ms.
Starting and ending source were identical and clean; baseline eligibility was
true. Build/process timeouts remained 3600/1800 s.

Every final H.264 result had maximum channel error **4/255** and **0** GPU
output CPU fallbacks. Encoder handoff was `cpu_rawvideo_pipe`; this qualifies
the executed output contract, with no zero-copy or wider performance claim.

| Explicit current-source gate | Executed result |
| --- | --- |
| EventRouter modal scope, shortcut containment and replacement/empty/removed owners | 1 passed, 0 failed, 0 ignored |
| Native resource helper/Host/OpenFX paths and child cancellation/timeout/retry | 5 passed, 0 failed, 0 ignored; 8.35 s |
| App modal Tab/Shift+Tab, disabled Apply traversal and focus restoration | 16 passed, 0 failed, 0 ignored; 23.65 s |
| Resource drafts, atomic transactions, real cube Undo/save/reopen | 103 passed, 0 failed, 5 ignored; 24.26 s |
| Native OpenFX group interpolation/history/queries/pixels | 1 passed, 0 failed, 0 ignored; 5.42 s |
| Native OpenFX group time/history/queries/pixels | 1 passed, 0 failed, 0 ignored; 5.41 s |
| Native OpenFX group value/history/queries/pixels | 1 passed, 0 failed, 0 ignored; 5.58 s |
| Native CLAP/VST3 group Undo/saved Redo/Unicode reopen/audio | 2 passed, 0 failed, 0 ignored; 37.12 s |
| CLAP sample-accurate event ranges | 1 passed, 0 failed, 0 ignored; 0.85 s |
| VST3 SDK linear parameter queues | 1 passed, 0 failed, 0 ignored; 1.40 s |
| VST3 state restoration and isolated-worker signal | 1 passed, 0 failed, 0 ignored; 0.94 s |
| Real CLAP/VST3 shared preview WAV execution | 2 passed, 0 failed, 0 ignored; 2.25 s |

The five default-ignored resource cases were explicitly executed by the native
resource gate. Filters overlap; the table is not a count of unique tests.
The wrapper required positive exact counts, clean stable source and unchanged
hashes for all six product/plugin binaries. Owned CLAP/VST3 fixtures use the
documented SDK event/queue interpretation; external examples remain limited
static references. Unexecuted vendor, bus, MIDI, editor and platform combinations
remain outside this qualification.

Qualified SHA256 identities:

- Product: `87D8EB8842823A542C4EDD0849DE7E5E22B8439F6F6E4D9E5F6C90CD8F191431`
- Golden: `5D05B2458839D607D8131FB46C6F2FF2D17E2BDA4906342AE7224D6A489077D9`
- OpenFX SessionProbe: `BA8734FE2FC3CEF93D0973415B8C4FEE64B1A1C0CEA9ECDBD409B2401781978A`
- Owned CLAP sample gain: `CB2FA1B4956CEDBDA4BBAA4B24EFA2D16924820C7C7FCEB4E4881633F21FD687`
- Owned VST3 stateful gain: `523E714760F9BC47805B3EDE7C7FC4399AEF7D1BDEEF1DCC3832B6DA50D9907F`
- External Clack static gain: `171222FFE85360DBD5BDC8791073FFE7603D178E93118DFB7E15B42B1891039C`
- External VST3 static gain: `CBCA9C3E9ED68554A5877889A81264D5C4910DEE38FCF02706272B0BB99F1AF5`

Consecutive report:
`E:/dev/mondrian/target/validation/nle-backlog-complete/20261006T211014Z-complete-golden-f3f9d3e3/complete-golden-consecutive-report.json`.
This supersedes b846010 for the executed modal/resource/native/encoded cases.
The App 2585/0/68 ignored, CurveEditor module 87/0/0 ignored, strict workspace
Clippy, fmt and diff gates passed before the production commit. No push occurred.
The complete professional NLE goal remains active.

## Canonical visual clipboard and durable history

The actual focused Copy/Paste regression reproduced narrowing of canonical
`Number(0.2)` through the former `PropertyValue::Float` clipboard: expected bits
`4596373779694328218`, observed `4596373779801702400`. This was an executed
runtime failure (0 passed, 1 failed, 0.22 s); the earlier missing test import was
a separate compilation failure.

The sole transient clipboard now retains complete schema-ordered raw channel
keys. Capture and Copy availability scan the selected property as one group.
Paste renews one identity per complete key, rebases exact owner time and enters
one author transaction. Core validates complete groups, scalar kinds, controls,
unique destinations and identity/time ownership, merges channel lists, then
normalizes constraints against final neighbors once. Occupied times are replaced;
existing identities cannot move through this paste operation. Per-channel
metadata survives without an evaluated-value reconstruction. Unadmitted channel
indices refuse without panic or publication. The project format is unchanged.

Final qualification of this implementation completed with:

- Core library, integration and documentation suites: **496 passed, 0 failed,
  1 ignored**, across 9 result summaries. Five canonical batch regressions ran,
  including exact 0.1..0.2 Float endpoints, wide Hold integers, independent
  channel interpolation, whole-batch refusals and 4096-key capture/publication.
- App all-feature library suite: **2582 passed, 0 failed, 68 ignored**, 318.61 s.
  The actual closed focused command path preserves raw `0.2` bits. Linear,
  Auto Bezier and Continuous Bezier copy/paste cases compare values, controls,
  flags, times and renewed IDs, one Undo, Unicode undone saves, live Redo after
  saving, redone saves and fresh reopen with empty history/clipboard. A separate
  actual 4096-key availability/Copy/Paste case checks every raw value, fresh IDs,
  complete selected group, one generation advance and a single restoring Undo.
- Strict workspace/all-targets/all-features Clippy with `-D warnings`: passed,
  5 min 07 s. The stale production import was removed from production scope;
  no lint allowance was added.

These source tests do not replace the clean-source native/encoded receipt below.
The wider professional goal remains active: audio and focused root-menu
clipboard routing, read-only curve interaction, group pointer workflows and the
remaining plugin/platform/performance/tracking/locale matrices still require
their own implementation and evidence.

## Qualification scope

The latest clean-source modal/resource and complete encoded Golden qualification is
[26696ef qualification](#clean-26696ef-modal-resource-lut-and-native-plugin-acceptance).
The same clean source also passed the explicitly executed native visual/audio
group, SDK queue, state-restoration and preview cases listed there.
Source-level tests and native results qualify their named source and binary;
they do not automatically cover later edits.
The complete NLE goal remains active while the listed professional matrices are
open.

## Clean b846010 modal resource LUT and native plugin acceptance

Windows qualification on 2026-10-07 rebuilt and tested clean, unchanged source
`b846010209da92753cbb0fac7dcbc7fead092130`, including canonical visual clipboard
capture, atomic complete-key paste, durable history and complete selection
restoration. Both author and committer are Shalom Wang. No push was made.

The complete Golden gate passed **3/3**, each with seven stages and a distinct
project. Each process exited naturally with code 0; none was forced after a
terminal report. Elapsed process times were 130278, 125688 and 122771 ms. Every
round decoded the final H.264 output with maximum channel error **4/255** and
reported **0 GPU-output CPU fallbacks**. Encoder transport was
`cpu_rawvideo_pipe`; this is not a zero-copy or latency qualification. The
source attestation stayed clean/stable and baseline eligible. Product/Golden
build completed in 1153807 ms without timeout; the declared build/process
timeouts remained 3600/1800 seconds.

Aggregate evidence:
`E:/dev/mondrian/target/validation/nle-backlog-complete/20261006T200214Z-complete-golden-1cc7515c/complete-golden-consecutive-report.json`.

| Executed scope | Passed / failed / ignored | Runtime |
| --- | --- | --- |
| Actual EventRouter modal containment, replacement, empty and removed owners | 1 / 0 / 0 | 0.00 s |
| Real resource helper, Host confirmation, native paths/Undo/reopen/pixels and supervised child cancellation/retry | 5 / 0 / 0 | 8.45 s |
| App modal routing and focus restoration | 16 / 0 / 0 | 23.26 s |
| Ordinary resource suite | 103 / 0 / 5 | 26.85 s |
| Native OpenFX group interpolation, Undo, saved Redo/reopen, queries and pixels | 1 / 0 / 0 | 5.76 s |
| Native OpenFX group time, Undo/reopen, queries and pixels | 1 / 0 / 0 | 5.54 s |
| Native OpenFX group values, Undo/reopen, queries and pixels | 1 / 0 / 0 | 5.95 s |
| CLAP/VST3 group interpolation, Undo/saved Redo/reopen and native audio | 2 / 0 / 0 | 38.04 s |
| Owned CLAP SDK event-range oracle | 1 / 0 / 0 | 0.82 s |
| Owned VST3 SDK linear-queue oracle | 1 / 0 / 0 | 1.53 s |
| Owned VST3 state restoration and isolated worker audio | 1 / 0 / 0 | 1.04 s |
| Real installed CLAP/VST3 shared preview execution | 2 / 0 / 0 | 2.26 s |

All five ignored resource cases were explicitly executed in the native resource
row; ignored cases were not counted as passes. The filters overlap and their
counts must not be summed as unique coverage. All six product/plugin binaries
remained unchanged throughout the native run. Product SHA256:
`C3D07CAEC162C7FF21611C55B8A5B39FF3DB6E673FDE00DB1BC954CF2F5313AB`;
Golden SHA256:
`EFED576445AC7D080A85A26EB5DBF5B8E8E221E45FD5AEE52AB1BBBD359024D1`.
The owned OpenFX, CLAP sample-gain and VST3 stateful-gain fixtures and the two
external static references retain their hashes recorded in the fb24582 receipt.
The external gain references remain static-only evidence, not automation oracles.

The source-bound native receipt is
`.scratch/curve-viewport/acceptance-canonical-clipboard-modal-resource-lut.json`,
with per-step `qualified-canonical-clipboard-*.log` files. This supersedes the
older fb24582 receipt for these executed cases. It does not qualify physical
macOS interaction, arbitrary third-party vendors or unexecuted capability and
performance matrices. The overall NLE goal remains active.

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


## Clean-source native group interpolation qualification for c3f5ee8

Source `c3f5ee86a566700eaeca49c04698be8acbadfd45` was clean and unchanged
through rebuilt-product and explicit native Interpolation, Time and Value
acceptance. Product build reused the unchanged compiled inputs and passed in
5.33s. The explicit ignored tests passed **1/1** each, zero failures or ignored:
Interpolation 4.77s (both Auto/Continuous), Time 4.27s and Value 4.32s. Source
files, product and owned fixture remained hash-stable. Both Git author and
committer are Shalom Wang; no push was made.

Product SHA256:
`b3e285576773d238279d5ef9bccfdadeec9f986687d1a7eb7af940da4a57e489`.
Fixture SHA256:
`ba8734fe2fc3cef93d0973415b8c4fee64b1a1c0cea9ecdbd409b2401781978a`.

This promotes the native visual group interpolation scope described above to
a named clean-source receipt. The current parameter author state, complete
constraint intent, independent native calculus and real pixels survive atomic
refusal, one Undo/Redo, saved-undone live Redo and fresh Unicode archive reopen.
Time/value sibling regressions retain their exact coordinates and scalar values.
The earlier ce3edfb encoded Golden remains a separately named receipt; no new
encoded or native audio group output is claimed by this run. Native audio,
professional group pointer/rubber-band/copy/reset, vendor/platform/performance,
tracking and broader i18n acceptance remain open under the complete NLE goal.


## Native audio group interpolation and independent SDK signal oracles

The current candidate adds two end-to-end native audio group tests in
`app::audio_rendering::tests`: CLAP and VST3 each qualify Auto Bezier and Continuous
Bezier through `ProductAction::Audio(EditAutomation)` and the author session.
The test source starts from clean `e20b14130e4e4c557ee810772df50748581f916b`.
Both tests passed (2/0/0 ignored, 38.39s; compile 2m20s). This is candidate-source
execution, using the unchanged clean c3f5ee8 product's native worker helper:
`B3E285576773D238279D5EF9BCCFDADEEC9F986687D1A7EB7AF940DA4A57E489`.
It is not a new clean-product or encoded-output receipt.

The independent native DSP fixtures are:

- Owned CLAP sample gain, plugin ID `org.mondrian.fixture.sample-gain`, adapted
  and attributed from pinned Clack under MIT OR Apache-2.0. It applies gain only
  within each SDK event batch's sample range; upstream source remains unchanged.
  Tested binary SHA256:
  `1CD37EEF0D1CAEE44915654B715911914E6896B9F81D1B098046E4FF6ECC9B26`.
- Owned stateful VST3 gain, now consuming every parameter queue point with linear
  segment interpolation and the implicit previous-block value at sample -1.
  Tested binary SHA256:
  `523E714760F9BC47805B3EDE7C7FC4399AEF7D1BDEEF1DCC3832B6DA50D9907F`.

Actual runtime diagnostics first reproduced whole-block gain defects in the
reference examples: CLAP multiplied both batch gains across all samples; VST3
used only the final queue value. Those examples' static and single-point tests
remain useful, but do not prove multi-point sample accuracy. The owned fixtures
provide SDK signal semantics without implementing a second Mondrian curve model.
VST3 interpolation follows the official
[IParamValueQueue contract](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IParamValueQueue.html).
No production host workaround or relaxed signal tolerance was added.

Isolated-worker qualification passed five explicitly executed ignored filters:
CLAP sample ranges (Realtime and Offline, 0.88s), VST3 linear queues (Realtime and
Offline, 1.70s), VST3 nondefault state restoration (0.99s), external CLAP static
state (0.40s) and SDK VST3 static gain (1.42s), each 1/0/0 ignored. The ordinary
Audio library suite passed 105/0/13 ignored (0.63s); the explicitly run filters are
not added to that suite as unique tests. Reused real-WAV preview fixture tests
also passed 2/0/0 ignored (2.26s).

The group matrix uses a real IEEE Float stereo WAV with opposite-sign channels,
probes it into the asset library, and edits three exact stable keys with one
complete-selection transaction. It checks missing-key and locked-Track atomic
refusal, preserved key identities/times/values, constraint intent, repeated-mode
no-op generation/history, single Undo, live Redo after an undone save, and fresh
Unicode undone/edited archives with empty history and exactly one SQLite asset.
Both runtime modes render every sample of the edited and restored curves with
unequal block partitions and compare native PCM to the canonical admitted curve
at tolerance 1e-6; realtime/offline PCM must also match exactly.

This qualification covers the named stereo Float32 gain instances and native
parameter automation/history chain. Broader vendor plugins, multibus layouts,
MIDI, native editors, platform/performance matrices and professional group
pointer/rubberband/copy/reset remain open in the overall goal. Encoded LUT/modal/
resource acceptance above retains its separately named clean ce3edfb source.


After simplifying the signal test table into named fields and a shared event
runner, both isolated native signal gates were rerun successfully: CLAP1/0/0
ignored (1.20s), VST3 1/0/0 ignored (1.39s). The rebuilt CLAP fixture for this rerun
is `CB2FA1B4956CEDBDA4BBAA4B24EFA2D16924820C7C7FCEB4E4881633F21FD687`;
the earlier end-to-end candidate run above retains its actually tested 1CD37E
fixture identity. Strict workspace/all-target/all-feature Clippy with `-D warnings`
passed (21.26s); standalone CLAP and VST3 fixture all-target Clippy with
`-D warnings` passed (1.47s and 2.03s). No lint suppression was added for the new
complex-type, Range-array or nested-if findings. Clean-source rebuild and native
matrix execution follow the implementation commit and have their own receipt.


## Clean-source native audio group acceptance: 689aae9

Clean and stable source `689aae9605b011fe74e92c32f081e234f91383ac` was rebuilt
and qualified after the implementation commit. The native acceptance script
requires that exact HEAD and a clean Git worktree before and after execution;
it also verifies all 12 committed file hashes and the product/fixture hashes
remain unchanged. Product build passed (8m39s). Every explicitly selected ignored
test executed and passed; no zero-test filter was accepted.

| Executed native gate | Result |
| --- | --- |
| CLAP + VST3 complete-selection interpolation / Undo / saved Redo / fresh reopen / native PCM | 2 passed, 0 failed, 0 ignored; 40.04s |
| CLAP SDK event ranges, realtime and offline | 1 passed, 0 failed, 0 ignored; 0.79s |
| VST3 SDK linear queues, realtime and offline | 1 passed, 0 failed, 0 ignored; 1.42s |
| VST3 nondefault state snapshot and restored worker output | 1 passed, 0 failed, 0 ignored; 0.93s |
| CLAP + VST3 real-WAV preview using the shared media fixture | 2 passed, 0 failed, 0 ignored; 2.33s |

Qualified SHA256 identities:

- Product:
  `B2E9EE66910B513E9746C9249017B36B80157DDD7C86040A156347E6BB2E901D`
- Owned CLAP sample-gain binary:
  `CB2FA1B4956CEDBDA4BBAA4B24EFA2D16924820C7C7FCEB4E4881633F21FD687`
- Owned stateful VST3 gain binary:
  `523E714760F9BC47805B3EDE7C7FC4399AEF7D1BDEEF1DCC3832B6DA50D9907F`

The named matrix includes both Auto and Continuous Bezier, locked/stale-selection
atomic refusal, exact key identity/time/value preservation, no-op history,
single Undo, live Redo after saving the undone state, fresh Unicode undone/edited
archives with empty history and one consistent SQLite source asset, and native
samplewise output under unequal realtime/offline block partitions. It requires
absolute PCM error at most 1e-6 against canonical curve evaluation and exact
realtime/offline PCM equality. The fixtures independently interpret SDK events;
they contain no Mondrian curve evaluator.

Implementation commit author and committer are Shalom Wang; the semantic branch
is `feat/nle-backlog`. Strict workspace/all-target/all-feature Clippy with
`-D warnings`, both standalone fixture all-target strict Clippy, all three fmt
checks and Git diff checks passed before commit. This clean native audio receipt
supersedes the candidate's audio scope, while the encoded LUT/modal/resource and
native visual receipts retain their separately named ce3edfb/c3f5ee8 sources.
No push was performed. The overall NLE goal remains active for the professional
curve interaction, vendor/plugin/platform/performance and other named matrices.


## Clipboard intent refusal and constrained group history

Source baseline is clean docs commit `288421703678576dea3ce96dec3aec0cc38a6353`.
A new actual ProductAction regression first reproduced an unintended author edit:
after copying a Clip and then a key group, clearing the key target and dispatching
Paste inserted the retained Clip (Clip count 2 versus original 1). Runtime RED:
0 passed/1 failed, 0.22s; it was not a compile-only diagnostic.

The fix replaces two clipboard caches plus a mutable kind with one private typed
payload. Kind inspection derives from that payload. Availability and execution
use the same current value; key paste errors have no Clip fallback. Successful
nonempty Copy replaces the value, while failed/empty capture retains it. Cut
publishes its captured value after author removal succeeds. The removed mutable
fields have no compatibility aliases.

Two new actual action regressions passed (2/0/0 ignored, 0.87s): refused key Paste
leaves the Sequence, author generation, selection and history unchanged; constrained
Auto/Continuous key-group Copy replaces the Clip payload, locked Paste refuses
atomically, successful Paste commits once with fresh IDs and preserved values/
constraint intent, single Undo restores the original, saving keeps live Redo,
and fresh Unicode undone/edited archives restore the exact author state with
empty history and an empty transient clipboard.

Full current App all-feature library tests passed **2571/0/68 ignored** (321.57s).
This includes modal routing, resource workflows and author history regressions.
Strict workspace/all-target/all-feature Clippy with `-D warnings` passed (2m27s).
A missing test import was corrected during compilation; it is distinct from the
recorded runtime RED. The 68 explicitly ignored environment-dependent gates are
not counted as executed here. Current encoded LUT and native audio acceptance
retain their named clean ce3edfb/689aae9 source and binary identities; this App
library result is not a new encoded or vendor-plugin receipt.

## Complete key-group Copy refusal

Three actual Copy regressions reproduced runtime failures against bc0d237:
missing key and missing property captures overwrote a two-key clipboard with a
one-key payload, while an entirely stale selection copied a timeline Clip.
All three now pass (3 passed, 0 failed, 0 ignored; 0.39s). Copy chooses its type
from explicit selected-key intent and resolves the entire stable selection against
one current Clip snapshot before publishing. Missing members report typed refusal
and preserve the complete old clipboard, selection, Sequence, author generation
and history. Availability and dispatch share a nonallocating selection predicate.

Full App all-feature library tests passed **2574 passed, 0 failed, 68 ignored**
(570.07s), including clipboard/history, modal routing and resource workflows.
The ignored environment-dependent tests are not counted as executed here.
The focused CurveEditor's local selection and broader professional interaction
matrix remain separate open work; this is not a claim of complete curve UI or
new native/encoded output qualification.

Strict workspace/all-target/all-feature Clippy with `-D warnings` passed (2m06s).


## Clean current modal, resource and encoded LUT acceptance

Clean and stable source `66150cf84ca5613c4b77591eff1fc50762937c2f` was rebuilt
and qualified after the atomic Copy commit. Build passed (504452ms). Complete
Golden passed **3/3** with all seven execution stages, distinct project identities,
exit code 0, natural termination and no forced exit for every pass. Starting and
ending source were clean and identical; the contract timeouts were unchanged.

| Gate | Executed result |
| --- | --- |
| EventRouter modal scope: Tab/Shift+Tab, suspended background shortcut, replaced/empty/removed owner | 1 passed, 0 failed, 0 ignored |
| Real resource helper/Host/native OpenFX and bounded child cancellation/timeout/retry | 5 passed, 0 failed, 0 ignored; 8.87s |
| App modal routing, disabled Apply traversal and background focus restoration | 16 passed, 0 failed, 0 ignored; 24.42s |
| Ordinary resource workflows/transactions | 103 passed, 0 failed, 5 ignored; 23.95s |

The five ignored resource cases in the ordinary filter were explicitly executed
by the real-resource gate above; the skipped count itself is not a passing result.
The wrapper rejects zero or changed executed counts and checks clean source and
stable product/fixture hashes after all gates.

Each Golden pass retained a maximum final H.264 channel error of **4/255** and
**0** GPU output CPU fallbacks. Pass process elapsed times were 120780, 119100 and
118981ms. Encoder handoff was `cpu_rawvideo_pipe`; these receipts do not assert a
zero-copy encoder or platform/vendor/performance coverage outside the named matrix.

Qualified SHA256 identities:

- Product: `448E82EE2789764009141B484A509CC85DE48964E07362A75EB32E5AED59C787`
- Golden executable: `3726BC51F73BD4FEA99FD47A90EB6FE452F5233BDE1D5673A3DD826D741B3A60`
- Native OpenFX fixture: `BA8734FE2FC3CEF93D0973415B8C4FEE64B1A1C0CEA9ECDBD409B2401781978A`

Consecutive report:
`E:/dev/mondrian/target/validation/nle-backlog-complete/20261006T154246Z-complete-golden-0f6053cd/complete-golden-consecutive-report.json`.
This supersedes the older ce3 encoded/modal/resource scope. Native visual and
native audio group receipts retain their separately qualified c3/689 sources;
no newer vendor/plugin acceptance is inferred. Full App2574/0/68 ignored, strict
workspace Clippy, fmt and diff checks passed before the production commit.
No push was performed. The broad NLE goal remains active, including focused
curve clipboard/group pointer/marquee/reset and the other documented matrices.

## Focused visual curve clipboard and durable history

The focused visual curve now owns exact Ctrl/Cmd+C/V chords with its complete
stable key selection. A real EventRouter regression first reproduced the bug:
Ctrl+A selected all three curve keys, but Ctrl+C fell through to global Clip Copy
(0 passed, 1 failed). The focused typed product request now captures the complete
current property selection and pastes through the existing atomic author session.
No global selection mutation, second clipboard, legacy payload decoder or author
state mirror was added.

All-feature App regression: **2578 passed, 0 failed, 68 ignored**, 329.27s.
Curve-editor regression filter: **99 passed, 0 failed, 0 ignored**, 2.38s.
The App run includes the existing actual modal Tab/Shift+Tab, pending resource,
atomic resource and real cube Undo/save/reopen workflows. These source tests do
not replace the separately named clean-source native/encoded receipts above.

New actual focused-control acceptance checks Ctrl+C and Cmd+V, exact key values,
constraint flags and handles, fresh pasted IDs, late Track lock refusal, one Undo,
saving the undone snapshot without losing live Redo, and fresh Unicode `.mdp`
reopens of both undone and edited states. Fresh sessions have empty history and
no transient clipboard. Copy and selection do not create history entries.

The rejection matrix checks missing owner/property/key, duplicate and empty keys,
Clip content, multi-property key content, empty clipboard, a real Playback seek
whose complete pasted extent cannot fit, and a late Track lock. Every refusal
retains the complete clipboard, selection, Sequence, generation and history.
Typed actionable errors resolve through direct and wrapped error transports in
both English and Chinese. Closed Copy/Paste payload decoding rejects extra
operation fields, including the empty Paste object.

Widget acceptance checks exact Ctrl/Cmd modifiers, complete selection, empty
Copy consumption, empty-selection Paste, and pointer-draft/focus ownership.
Audio clipboard, read-only selection, new pasted selection after root rebuild,
root Edit-menu context and group pointer/marquee/reset remain open; this receipt
qualifies focused visual keyboard/local-menu dispatch rather than the complete
professional clipboard workflow.

Strict workspace/all-target/all-feature Clippy with `-D warnings` passed (2m42s).
Formatting and diff checks passed before this implementation was committed.

## Pasted group selection across root reconstruction

A real Host/EventRouter regression reproduced successful Paste publishing three
fresh selected author IDs while the rebuilt curve selected none (0 passed,
1 failed, 3.45s). The visual Inspector now projects selection by exact parameter
instance. A changed projected set replaces the old selection; an unchanged set
preserves current local selection. Set order does not signal a change. Missing
keys are excluded, and another owner or author session cannot inherit focus.

The corrected real Host case passes: Ctrl+A/C/V selects every newly pasted key
after root rebuild, focus stays on that curve, the next keyboard increment changes
all three keys, and Undo/Redo restores complete author state. Generic widget
acceptance additionally covers unchanged local selection, explicit clearing,
missing IDs, projection reordering and different-owner isolation. This adds no
author-state mirror, coordinate-based ID matching or second clipboard.

Final all-feature App after the complete history fixes: **2579 passed, 0 failed,
68 ignored**, 403.10s. The initial projection also passed full Widgets
**1099/0/0 ignored** (323.40s); the revised projection requires its final curve
widget gate below.
The App run includes actual modal/resource and real cube Undo/save/reopen cases.
The older clean native/encoded receipts retain their named source until the
current committed source receives its own rebuilt qualification.


Additional real history regression exposed complete batch edits publishing only
one representative key (0/1 failed, 3.46s). Clip and Effect handlers now retain the
complete admitted value/time/interpolation selection after successful commit.
After that fix, a separate actual Paste Redo regression reproduced three restored
author keys but zero selected UI keys (0/1 failed, 2.99s). Projection comparison
now excludes currently absent IDs, so disappearance and reappearance reproject the
complete group. The final Host test checks these real Undo/Redo routes and the
complete group after keyboard editing. Generic acceptance checks the same stable
identity absence/reappearance without retaining dormant IDs or guessed coordinates.

Final changed-curve widget regression: **100 passed, 0 failed, 0 ignored**, 3.13s.
Strict workspace/all-target/all-feature Clippy with `-D warnings` passed (2m59s).
Final formatting and diff checks passed before commit. The full original goal
remains active; native receipts and remaining professional matrices are separate.

## Clean fb24582 modal, resource, LUT and native plugin acceptance

On 2026-10-07, clean source `fb24582191323569d78e51912e0f7aa3adeaa81a`
was rebuilt and qualified with the current focused clipboard and group selection
fixes. Product/Golden build passed (549361ms). Complete Golden passed **3/3**,
all seven stages each, distinct projects, natural exit 0 and no forced exit.
Source start/end were identical and clean; baseline eligibility was true and
contract build/process timeouts remained 3600/1800s.

Each final H.264 result retained maximum channel error **4/255** and **0** GPU
output CPU fallbacks. Process elapsed times were 123410, 119388 and 120922ms.
Encoder handoff remained `cpu_rawvideo_pipe`; this does not qualify zero-copy
encoding or performance beyond the executed matrix.

| Explicit current-source gate | Executed result |
| --- | --- |
| EventRouter modal scope, suspended shortcut and replacement/empty/removed owners | 1 passed, 0 failed, 0 ignored |
| Native resource helper/Host/OpenFX paths and child cancellation/timeout/retry | 5 passed, 0 failed, 0 ignored; 8.18s |
| App modal Tab/Shift+Tab, disabled Apply traversal and focus restoration | 16 passed, 0 failed, 0 ignored; 23.15s |
| Resource drafts, atomic transactions, real cube Undo/save/reopen | 103 passed, 0 failed, 5 ignored; 24.43s |
| Native OpenFX group interpolation/history/queries/pixels | 1 passed, 0 failed, 0 ignored; 5.41s |
| Native OpenFX group time/history/queries/pixels | 1 passed, 0 failed, 0 ignored; 5.41s |
| Native OpenFX group values/history/queries/pixels | 1 passed, 0 failed, 0 ignored; 5.28s |
| Native CLAP/VST3 constrained group Undo/saved Redo/Unicode reopen/audio | 2 passed, 0 failed, 0 ignored; 38.81s |
| CLAP sample-accurate event ranges | 1 passed, 0 failed, 0 ignored; 0.81s |
| VST3 SDK linear parameter queues | 1 passed, 0 failed, 0 ignored; 1.36s |
| VST3 state restoration and isolated-worker signal | 1 passed, 0 failed, 0 ignored; 0.93s |
| Real CLAP/VST3 shared preview WAV execution | 2 passed, 0 failed, 0 ignored; 2.16s |

The five ignored resource cases above were explicitly executed by the native
resource gate. The wrapper requires positive exact executed counts for every
filter, clean stable source and unchanged hashes for product and all used native
plugin binaries. The owned CLAP and VST3 fixtures retain their documented correct
SDK event/queue interpretation; external examples retain their limited static
reference scope. Native signals/queries qualify the supported contracts exercised
here, rather than unexecuted vendor/bus/editor/platform combinations.

Qualified SHA256 identities:

- Product: `A0389FBA5684DD0BCEA7A663F5D72A22DFAC4686210FA0CB41B7BE9A830F51E5`
- Golden: `D3FB620AABE07DFBC312BF3FF41CC43395189DE7F57EA786A225A437C1D50FCF`
- OpenFX SessionProbe: `BA8734FE2FC3CEF93D0973415B8C4FEE64B1A1C0CEA9ECDBD409B2401781978A`
- Owned CLAP sample gain: `CB2FA1B4956CEDBDA4BBAA4B24EFA2D16924820C7C7FCEB4E4881633F21FD687`
- Owned VST3 stateful gain: `523E714760F9BC47805B3EDE7C7FC4399AEF7D1BDEEF1DCC3832B6DA50D9907F`

Consecutive report:
`E:/dev/mondrian/target/validation/nle-backlog-complete/20261006T183029Z-complete-golden-a86e2348/complete-golden-consecutive-report.json`.
This supersedes the older 661 encoded/modal/resource and c3/689 native-group
scopes for these executed cases. Full App2579/0/68 ignored, final Curve100/0/0
ignored, strict workspace Clippy, fmt and diff passed before the production
commit. No push was performed.

At this historical source, raw canonical clipboard precision still needed direct
channel-bit qualification (subsequently repaired and qualified in b8460102):
current complete-property capture uses `PropertyValue` readback, whose Float
representation is f32 while canonical author channels retain f64. The accepted
focused .25/.5/.75 fixture proves those values and handles, not arbitrary raw
Number bits. A source audit and unexecuted reproduction draft identify this next
qualification; they are not reported as an observed runtime failure.

## Audio automation clipboard and history acceptance

On 2026-10-07, audio curves gained the same focused Copy/Cut/Paste protocol as
visual curves. Capture preserves canonical f64 values, exact time, handles and
constrained tangent intent. Paste generates fresh identities, validates the
complete group against the destination definition and owner time, then commits
one authoring transaction. Cut publishes the clipboard only after a successful
authoring commit. Invalid intents and locked mutations retain clipboard,
document, generations and history.

Actual Host Copy/Cut failures were reproduced (0 passed, 2 failed), then repaired.
A separate actual Paste failure exposed selection retaining the old key after
commit (2 passed, 1 failed). Post-edit selection now joins the existing panel
projection by exact sequence and audio target. The retained Host regressions pass
(3 passed), including subsequent Cut preserving the original key and Undo.

Private App tests exercise Linear, Auto Bezier and Continuous Bezier capture,
shifted paste, raw values and handles, one history entry, Undo, save of the undone
state, Redo, save of the edited state and fresh Unicode-path reopen of both.
Nine invalid intents and locked Copy/Cut/Paste exercise refusal atomicity.

Final gates ran sequentially against frozen source:

| Gate | Result |
| --- | --- |
| Core all-feature library tests | 472 passed, 0 failed, 1 ignored; 6.56s |
| Timeline all-feature library tests | 241 passed, 0 failed, 0 ignored; 0.36s |
| App all-feature library tests | 2606 passed, 0 failed, 68 ignored; 368.48s |
| UI widgets all-feature library tests | 1105 passed, 0 failed, 0 ignored; 301.52s |
| Workspace/all-target/all-feature Clippy, `-D warnings` | Passed; 5m11s |
| Formatting and diff checks | Passed |

App includes the existing real modal Tab/Shift+Tab, disabled Apply traversal,
resource transactions and real cube LUT Undo/save/reopen regressions. Ignored
cases are not counted as executed. These gates do not constitute a new native
plugin or encoded LUT receipt; the source-bound native receipts above remain
historical. Audio Component/Scope/route/rack boundary matrices, root menu and
Undo/Redo selection projection, read-only inspection and newly pasted native
CLAP/VST3 output still require acceptance. The full product goal remains active.

## Clean 3d26dfc modal, resource, LUT and native output acceptance

On 2026-10-07, clean source `3d26dfc5fd4db4f8f0bdb107d67ae37241458262`
was rebuilt after audio clipboard integration. Build passed in 1146306ms.
Complete Golden passed **3/3**, with all seven stages per run and distinct
project/run identities. Source start/end revisions were identical, both clean;
baseline eligibility was true. Contract build/process timeouts remained
3600/1800s. All processes exited naturally with code 0; none required forced
termination. Outer process times were 125069, 121582 and 120984ms.

Each LUT/color-media H.264 roundtrip measured maximum channel error **4/255**
across its five sampled pixels and **0** GPU output CPU fallbacks. The encoder
handoff remained `cpu_rawvideo_pipe`. This qualifies the executed color fixture
and its defined output checks, not zero-copy encoding or arbitrary LUTs.

| Explicit current-production-source gate | Executed result |
| --- | --- |
| EventRouter modal scope and shortcut ownership | 1 passed, 0 failed, 0 ignored |
| Native resource helper/Host/OpenFX and child cancellation/timeout/retry | 5 passed, 0 failed, 0 ignored; 9.63s |
| App modal Tab/Shift+Tab, disabled Apply and focus restoration | 16 passed, 0 failed, 0 ignored; 24.05s |
| Resource drafts, atomic transactions, real cube Undo/save/reopen | 103 passed, 0 failed, 5 ignored; 24.52s |
| Native OpenFX group interpolation/history/queries/pixels | 1 passed, 0 failed, 0 ignored; 5.49s |
| Native OpenFX group time/history/queries/pixels | 1 passed, 0 failed, 0 ignored; 5.42s |
| Native OpenFX group values/history/queries/pixels | 1 passed, 0 failed, 0 ignored; 5.76s |
| Native CLAP/VST3 constrained group Undo/saved Redo/Unicode reopen/audio | 2 passed, 0 failed, 0 ignored; 37.18s |
| CLAP sample-accurate event ranges | 1 passed, 0 failed, 0 ignored; 0.78s |
| VST3 SDK linear parameter queues | 1 passed, 0 failed, 0 ignored; 1.43s |
| VST3 state restoration and isolated-worker audio | 1 passed, 0 failed, 0 ignored; 0.95s |
| Real CLAP/VST3 shared preview WAV execution | 2 passed, 0 failed, 0 ignored; 2.49s |

The five ignored resource cases were explicitly run in the native resource gate.
Filters overlap, so their counts are not summed as unique tests. The wrapper
required positive exact counts and unchanged source and native binary hashes
through all gates. Qualified SHA256 identities:

- Product: `A94F1B5BADC3DD559C054725360E3123E6BDE1700C0BFDA6664A2269B6F6FF1E`
- Golden: `9B8FEEEEB8D0553F5B060E148DECBDB081C05BBE40BC4350E6C20A7B4CF0BE8A`
- OpenFX SessionProbe: `BA8734FE2FC3CEF93D0973415B8C4FEE64B1A1C0CEA9ECDBD409B2401781978A`
- Owned CLAP sample gain: `CB2FA1B4956CEDBDA4BBAA4B24EFA2D16924820C7C7FCEB4E4881633F21FD687`
- Owned VST3 stateful gain: `523E714760F9BC47805B3EDE7C7FC4399AEF7D1BDEEF1DCC3832B6DA50D9907F`
- External CLAP static gain reference: `171222FFE85360DBD5BDC8791073FFE7603D178E93118DFB7E15B42B1891039C`
- External VST3 static gain reference: `CBCA9C3E9ED68554A5877889A81264D5C4910DEE38FCF02706272B0BB99F1AF5`

Consecutive report:
`E:/dev/mondrian/target/validation/nle-backlog-complete/20261007T021257Z-complete-golden-7cf8ed32/complete-golden-consecutive-report.json`.
This is the current receipt for the executed modal/resource/LUT and native
interpolation/time/value/history cases. Newly pasted audio groups still require
their separate native output gate; read-only interaction and wider owner, vendor,
bus, editor, platform and performance matrices remain open. No push occurred.

## Native pasted audio curve history and sample output

The existing owned CLAP/VST3 gain history fixtures now also execute the closed
audio clipboard protocol after fresh edited-project reopen. Both tests explicitly
ran with **2 passed, 0 failed, 0 ignored**, 72.96s, covering Auto Bezier and
Continuous Bezier on each native format's Track pre-fader gain parameter.

Copy is author/history-neutral. A stale complete-selection Cut refuses without
changing author state; subsequent Paste proves the captured group remains usable.
Same-time Paste creates fresh IDs, retains exact times, raw value bits, tangent
intent and handles, advances one author generation and adds one history entry.
Native PCM remains identical before Paste, after Paste, after Undo, after saved
live Redo and after fresh reopening of both Unicode-path snapshots. Each render
uses realtime partitions 64/64/129 and offline partitions 37/59/161, checks all
257 stereo samples against canonical evaluation within `1e-6`, and requires
identical PCM across partitions.

The first fixture attempt compiled successfully but failed both tests at fresh
reopen: it retained the original Session's exclusive logical-project lease while
opening a second Session. This was a fixture ownership error. The corrected
fixture consumes the owning AppState, saves Undo, executes live Redo, saves Redo,
drops that Session, then sequentially opens and closes each saved snapshot.
Production lease rules and all signal/history assertions remain intact.

This extension changes only `#[cfg(test)]` code in `audio_rendering.rs`. It uses
the clean `3d26dfc` product and unchanged native binaries identified above; the
Host test patch starts from documentation-only `364cd070ec3d09b5a27c368f7b7c204888e1f480`.
The gate separately records the test source SHA256 and verifies product/fixture
hashes before and after execution. It does not claim a new production rebuild.
Shifted/native Component/Scope/route and multi-bus matrices remain separate.

Qualified Host test source SHA256:
`AFFEBBF6140E90FD034321C91E05348B6C7ACE1564E52E3A9159DF94C2F98E65`.
Strict workspace/all-target/all-feature Clippy with `-D warnings` passed
(2m07s), followed by formatting and diff checks. No production code changed in
this test extension; the prior full library and clean Golden receipts retain
their named source scope. No push was performed; the overall goal stays active.

## Read-only curve inspection and native window roles

Actual locked Track audio-curve input reproduced a failure: after real Host /
EventRouter click and Ctrl+A/C, the clipboard remained empty (**0 passed,
1 failed**, 3.01s). The backend admitted locked Copy, but the widget was disabled.
CurveEditor now separates author permission from input availability. Read-only
audio, numeric visual and shape curves retain inspection focus, selection and
viewport input. Clipboard-capable curves retain Copy; mutation commands are
owned refusals without local geometry drift or fallback to Clip operations.
Revoking author permission balances draft cancellation and pointer release.
Accessible state marks `read_only` independently from `disabled`.

The original actual Host assertion passed, including Cut/Paste, Delete/Backspace,
arrow and pointer refusal with unchanged curve, clips, generation, history and
clipboard kind. Shared widget cases also check Ctrl/Meta Copy, tangent and point
input refusal, insertion refusal, navigation, zoom/reset and cancellation without
Finish or geometry drift. Core/UI authoring qualification before the final
localized-help and isolated-native-fixture adjustments:

| Gate | Result |
| --- | --- |
| UI-core library | 84 passed, 0 failed, 0 ignored; 0.02s |
| All CurveEditor regression cases | 102 passed, 0 failed, 0 ignored; 3.67s |
| App library | 2607 passed, 0 failed, 68 ignored; 655.96s |
| Strict workspace/all-target/all-feature Clippy | Passed; 2m45s |

Final adjustments add English/Chinese help describing available read-only
operations and isolate native-test preferences from machine configuration.
Final-source gates then passed:

| Gate | Result |
| --- | --- |
| Original locked audio Host Copy/refusal regression | 1 passed, 0 failed, 0 ignored; 3.02s |
| Focused audio Copy/Cut/Paste Host regressions | 3 passed, 0 failed, 0 ignored; 7.42s |
| Localization, catalog and pseudo-locale tests | 26 passed, 0 failed, 0 ignored; 0.52s |
| Explicit native Windows window-role cycle | 1 passed, 0 failed, 0 ignored; 9.57s |
| Strict workspace/all-target/all-feature Clippy, `-D warnings` | Passed; 2m20s |
| Formatting and diff checks | Passed |

The native role cycle uses temporary project/preferences files and the actual
winit window and GPU surface. It verifies fixed Startup size and resize policy,
resizable Workspace, requested 1300x800 normal size, maximize/close/reopen state,
cleared staged GPU successors and retained Window/Surface/Device identities. It
acquires/presents the retained surface and requires complete UI/App/background
resource release. Eight-direction border admission is separately covered by the
App regressions and routes through `drag_resize_window`; this run does not prove
physical pointer dragging or every monitor/platform combination.

Final App test binary SHA256: `D22E5705F15702D1C4C36F7BCDD26B9465E2F97CF65F3DC11CEB7D9CC8CD70B2`.
The prior clean production/native receipts remain tied to their named revisions;
this does not assert a new packaged product build. Wider owner-domain, group
pointer/marquee/reset, plugin/platform/performance and full-goal matrices remain
open. No push was performed.

## Read-only owner replacement and menu acceptance

Continuation from `8161656`: real Host input reproduced two product failures.
After locking a Track during an audio curve drag, late release lost inspection
focus (0 passed / 1 failed / 0 ignored, 2.83s). Preserving inspection identity
then exposed retained old pointer capture (0/1/0, 2.18s). The shared CurveEditor
view snapshot now preserves stable inspection state for the same owner while
excluding author drafts, geometry and callbacks. A deferred capture termination
obligation releases the discarded drag on the next routed event. Another owner
still cannot inherit inspection identity.

Actual locked audio late-release/Copy/Cut/Paste qualification passed 1/0/0 in
2.49s. All CurveEditor module regressions passed 89/0/0 in 3.73s. Real root Edit
menu tests passed 7/0/0 in 15.57s, including locked visual Copy by menu click,
refused Cut/Paste without Clip fallback, and local Shift+F10/Enter Copy. An
initial local-menu fixture sent Escape after the root menu had already closed,
clearing the curve selection by design; correcting that input sequence preserved
all selection and author-state assertions. This fixture mistake is not a product
menu failure.

Full App library qualification passed 2608/0/68 ignored in 372.25s. It explicitly
includes numeric and structured curve owner invalidation, modal Tab containment
and focus restoration, pending resource modal Tab after disabled Apply, stale
resource inspection refusal and real Cube resource Undo/save/reopen pixels.
Ignored native and performance cases are not included in that pass count.

Final strict workspace/all-target/all-feature Clippy with `-D warnings` passed
in 2m19s, followed by formatting and diff checks. Explicit ignored native resource
qualification passed 5/0/0 in 8.24s: actual Host helper completion admission,
native path resource cancellation/Undo/save/reopen/output pixels, bounded helper
failures and supervised child cancellation/timeout/retry reaping. These use the
current test source with the unchanged named `3d26dfc` product helper (SHA256
`A94F1B5BADC3DD559C054725360E3123E6BDE1700C0BFDA6664A2269B6F6FF1E`).
This continuation does not rebuild the packaged product or rerun encoded Golden
exports. Those prior receipts keep their explicit source scope. The full original
goal remains active, including group pointer/marquee/reset and the wider plugin,
platform, performance and product interaction matrices. No push performed.

Current App test binary SHA256: `999993A5A46ADFE40C7B155F2FEAE2BCC9D399B78A121009F5E73A6D868EF5DC`.

## Combined complete-key transform foundation

Continuation from `47fa03a` adds Core's atomic exact-time plus bounded-value group
transform and a single PropertyMutation dispatch. Existing single-axis operations
reuse the same admission; no author compatibility path or second evaluator is
introduced. Four new cases verify constrained output against independent original
curve evaluation, metadata/control retention, joint refusal on invalid selection,
collision/nonfinite/overflow, multi-channel time translation, untouched component
values, integer precision above 2^53, raw signed-zero retention at common limits,
and nested property/bag dispatch. The initial unavailable Hold test constructor
was a compile-only fixture error, corrected to `from_preset(Hold)`.

Full Core qualification: 476 library tests passed, zero failed, one ignored
(6.04s); 26 public integration tests and one replay-tool test also passed.
Zero-test helper/doc targets add no executed-case evidence. Strict full workspace
Clippy with all targets/features and `-D warnings` passed (4m56s); format and diff
checks passed. This is foundation acceptance. App/UI group pointer handling,
marquee/reset, gesture history/reopen and current native encoded output are still
required; no new App runtime or native product qualification is implied.

## Stable group pointer interaction and history

Continuation from `5733f17` connects the atomic Core transform to real visual and
audio input. The initial actual Host RED collapsed a selected group on grab. A
later actual RED exposed another loss after commit: the new action was absent
from the existing selection retention policy. Both paths now reuse the sole
widget/App selection and author-session contracts. Retention/removal policy
matches are exhaustive. Preview sources are published only after complete
candidate admission, including rejected motion followed by origin return.

Four actual input cases passed (9.24s): Linear/Auto/Continuous visual Host groups
and the Auto/Continuous audio control matrix. They cover frozen stable IDs,
origin-relative joint time/value motion, repeats, unselected time/value retention,
collision or owner-bound refusal, return-to-origin cancellation, one pending
history draft, one sealed Undo, Undo/save with live Redo, cancellation preserving
Redo, Redo and fresh sequential Unicode archive reopen. A separate audio model
matrix passed six owner/interpolation combinations: short Component extent uses
the canonical exact author mapper, while Sequence and Scope time is not bounded
by their viewport. EN/ZH help describes actual group input.

Fixture corrections are distinguished from product REDs: an inferred float type
prevented compilation; draft diagnostics legitimately include one prospective
Undo; App identity ordering is normalized rather than click order. The tests
retain complete selection, bounded pending history and full restored diagnostics
assertions. Full App/UI/Timeline/strict Clippy and current encoded product output
qualification are still running or pending; this paragraph does not claim them.

### Final projection and terminal continuation

The actual Scope keyboard projection RED showed a canonical key at normalized
owner coordinate 1.2 remaining displayed at 1.0. Audio key projection now emits
all authored IDs/coordinates; the existing widget updates known keys and clips
painting to the current interval. The independent coordinate assertion passes.
FocusLost follows the existing single-key/handle/value contract and finishes a
changed admitted group once; Escape remains cancellation and preserves prior
Redo. A new test expectation initially treated FocusLost as cancellation; it was
corrected against the existing terminal contract without changing production
FocusLost policy. Tests also require one sealed entry, no pending draft, one Undo
and saved live Redo after completion.

Broad candidate qualification before the four-line audio projection correction:
App 2613 passed/0 failed/68 ignored (661.99s), Timeline 241/0/0 (0.78s), UI events
77/0/0 (0.02s), widgets 1107/0/0 (567.28s), strict workspace/all-targets/all-features
Clippy 4m18s, format and diff checks passed. Final source qualification:
actual group inputs 4/0/0 (8.81s), Scope projection/history/reopen 1/0/0 (0.60s),
all audio-related App cases 148/0/9 ignored (39.28s), strict complete Clippy 2m07s,
format/diff checks passed. Overlapping cases are not added as unique test counts.
Actions pin validation passed. Current product/native encoded output remains to
rebuild and qualify on a clean commit; no inferred native acceptance or push.

## Clean c47b9a5 modal, resource, history and native output acceptance

The group pointer implementation is committed as `ebae929`; `c47b9a5` then merges
develop's community, package metadata and CI changes. Both commits use Shalom
Wang as author and committer. This receipt supersedes the pending native status
above for its explicitly executed scope. The original full NLE goal remains
active, and no push or remote CI result is claimed.

The current product and Golden binaries were rebuilt from clean `c47b9a5`.
Complete Golden passed **3/3** independent runs, each executing all seven required
stages with distinct Project and run identities. Build exit was zero after
1,147,386ms; process durations were 121,353/118,537/117,704ms, all natural zero
exits without forced termination. Source attestation starts and ends at
`c47b9a549935963e08a97df59efe93a3b8901a9f`, clean and stable, with dirty diagnostic
mode disabled and contract timeouts unchanged.

All three color-media H.264 roundtrips checked five decoded sample pixels and
measured maximum channel error **4/255**, with zero GPU output CPU fallbacks.
Encoding uses the qualified CPU rawvideo route; these results do not establish
zero-copy encoding, arbitrary-LUT accuracy or a performance/platform matrix.

| Current-source gate | Executed result |
| --- | --- |
| Actual visual/audio group pointer input, terminal policy, Undo and Unicode reopen | 4 passed, 0 failed, 0 ignored; 9.24s |
| Exact Component extent versus Sequence/Scope viewport admission | 1 passed, 0 failed, 0 ignored |
| Shared audio out-of-view projection, keyboard history and saved live Redo | 1 passed, 0 failed, 0 ignored; 0.58s |
| EventRouter modal replacement, empty scope, removed owner and suspended shortcuts | 1 passed, 0 failed, 0 ignored |
| Actual product resource helper, native paths, cancellation/timeout/retry and child reaping | 5 passed, 0 failed, 0 ignored; 8.43s |
| App modal Tab/Shift+Tab, disabled Apply and focus restoration | 16 passed, 0 failed, 0 ignored; 23.24s |
| Resource transactions, stale/canceled/locked drafts, Undo/save/reopen and real cube pixels | 103 passed, 0 failed, 5 ignored; 25.45s |
| Native OpenFX group interpolation/time/value histories, queries and pixels | Three separate gates, each 1 passed, 0 failed, 0 ignored; 5.46/5.75/5.41s |
| Native CLAP/VST3 group interpolation, Undo/save/reopen and PCM output | 2 passed, 0 failed, 0 ignored; 72.37s |
| CLAP event ranges / VST3 linear parameter queues / isolated state restoration | Three separate gates, each 1 passed, 0 failed, 0 ignored; 0.80/1.49/0.98s |
| Installed CLAP/VST3 Preview execution | 2 passed, 0 failed, 0 ignored; 2.39s |
| Portable LUT/OCIO dependencies, relocation, source removal, tampering, cancellation and real-video rebinding | 11 passed, 0 failed, 0 ignored; 8.39s |
| Native Windows Startup/workspace role cycle, sizing and retained surface/device authority | 1 passed, 0 failed, 0 ignored; 8.99s |

The resource row's five ignored tests were separately executed in the explicit
native-resource gate above; ignored tests are not counted as executed passes.
Filters overlap, so the rows are not added into a unique test total. Resource
coverage includes edit-away-and-back invalidation, duplicate confirmation,
pending IME and a Save As directory change. LUT Undo rejects an unbound resource
instead of reusing stale compiled output; saving preserves live Redo, while a
fresh Session does not resurrect history.

Product SHA-256:
`1c339be231c44981fb25a42b861beb05b0ad5d486c5b20f817dfab4ae916c8ef`.
Golden SHA-256:
`aa48956577255634d62b1917ae8d7f60e67602292f8588f909b2d45825256b1f`.
The complete gate report is
`target/validation/nle-backlog-complete/20261007T072712Z-complete-golden-2fbeaea5/complete-golden-consecutive-report.json`.
Local native and supplemental receipts retain source, product and fixture hashes
and exact logs. All binaries and tracked source remained unchanged throughout
both receipts. Strict workspace/all-targets/all-features Clippy, fmt, cached diff
and Actions pin validation passed before the merge commit; the merged dependency
policy additionally passed `cargo deny --offline check bans licenses sources`.

This scope uses the owned SessionProbe OpenFX fixture and installed reference
gain processors. It does not qualify combined group-pointer native output,
arbitrary vendor plugins, multi-bus/MIDI/editor completeness, all monitor edges
or macOS. Marquee selection and the wider original workflow, platform, i18n and
performance obligations remain open. Subsequent documentation-only commits do
not imply a rebuilt product at a different source revision.
## Stable keyframe marquee selection

Blank dragging in an authored curve now selects stable key IDs. Plain dragging
replaces selection, Shift adds, and Ctrl/Cmd+Shift toggles against the selection
at pointer-down. Ctrl/Cmd clicking a key toggles it; Ctrl/Cmd clicking blank space
explicitly inserts a key. Transfer curves without authored selection retain
point insertion. English and Chinese interaction help describe these controls.

Selection alone never changes project author generation or history. Read-only
curves support the same inspection. Escape restores the original selection;
layout changes and owner replacement cancel transient selection and release the
owned capture. Dense keys with coincident projected centers remain distinct by
ID. Nonfinite motion is ignored, and viewport boundary points are excluded.
Painting consumes semantic theme tokens.

Local Windows validation before commit:

- CurveEditor: 93 passed, 0 failed, 0 ignored (4.38s).
- Actual Host group-pointer routing: 6 passed, 0 failed, 0 ignored (14.71s),
  including Auto and Continuous marquee followed by joint dragging, refusal,
  cancellation, Undo, saved live Redo and fresh Unicode reopen.
- Temporal App tests: 30 passed, 0 failed, 1 performance test ignored.
- Complete App library: 2615 passed, 0 failed, 68 ignored (655.07s).
- Workspace/all-targets/all-features Clippy with warnings denied: passed (2m38s).
- Formatting and diff whitespace checks: passed.

The valid authored fixture reproduced blank insertion before the repair; dense
selection also reproduced capture surviving a resize. Both regressions now pass.
Older insertion tests use the explicit primary-modifier chord and retain their
original author-ID, value and history assertions. Ignored native tests are not
counted as executed by the complete App run. The preceding c47b9a5 native receipt
retains its named source scope; this marquee change has not yet received a fresh
product-binary qualification. Broader plugin, platform, i18n, output and
performance obligations remain open under the original goal.

## Clean a258187 marquee, modal, resource and joint native output acceptance

The product includes marquee commit `3e88d09`; test commit `a258187` adds joint
native curve acceptance. Both author and committer are Shalom Wang; no push was
performed. Current product and owned SessionProbe were rebuilt from clean
`a25818723f9e1d59d1760e2a8c25b7176791e52c` (product build 571524ms).

| Explicit gate | Terminal result |
| --- | --- |
| OpenFX joint time/value transform history and output | 1 passed, 0 failed, 0 ignored; 4.89s |
| OpenFX time, value and interpolation sibling history/output | 1 each passed, 0 failed, 0 ignored; 4.52s / 4.62s / 4.60s |
| Complete Golden | 3/3, all seven stages per pass; natural exit 0, no forced termination |
| App modal Tab/Shift+Tab and disabled Apply routing | 16 passed, 0 failed, 0 ignored; 23.96s |
| EventRouter modal replacement, empty and removed owners | 1 passed, 0 failed, 0 ignored |
| App resources | 103 passed, 0 failed, 5 ignored; 25.83s |
| Explicit native resource/process tests | 5 passed, 0 failed, 0 ignored; 8.56s |
| Portable LUT/OCIO and media dependency tests | 11 passed, 0 failed, 0 ignored; 8.57s |
| Native Windows startup/workspace window role cycle | 1 passed, 0 failed, 0 ignored; 9.40s |
| Actual group-pointer routing, including marquee | 6 passed, 0 failed, 0 ignored; 23.83s |

The five ignored resource tests in the broad filter were explicitly executed;
the rows overlap and must not be added as unique tests. The first supplemental
attempt missed the `strings_paths` SDK mode and stopped at the test's environment
precondition (4 passed, 1 failed). Its failure log is retained. The corrected
script supplies and checks product/fixture paths and mode, validates the existing
exact-source Golden receipt and hashes, and executes the remaining gates. This
was a harness error, not a resource authoring regression; Golden was not rerun.

Golden elapsed process times were 121991 / 121259 / 121035ms, with distinct
Project and Run IDs. Each encoded color-media roundtrip sampled five pixels and
retained maximum channel error 4/255. The tested encoder used the CPU rawvideo
pipe with NVENC; this does not establish zero-copy or performance qualification.
The 3600s build / 1800s process contract timeouts were unchanged. Starting and
ending source revisions were identical and clean, dirty diagnostics disabled,
source stable and the receipt baseline eligible.

The joint native case dispatches the existing TransformMany author action for
Auto and Continuous constraints. SDK queries reject a partial-axis snapshot;
independent finite-difference derivatives and split Simpson integrals retain
their original tolerance. Collision refusal leaves both axes, generation and
history unchanged. Shared production-graph native pixels match canonical curve
samples before/after one Undo, saving the undone state with live Redo, and fresh
Unicode reopen with empty history. Existing time/value/interpolation cases remain
qualified after the fixture extension.

Product SHA-256:
`685cd2bcdb644221adbb8966de06d9cc0333021212fbe708b9275ae4cc129b05`.
SessionProbe SHA-256:
`458766854983fd3152e33570471c633788356756bb8b625f49b7ecff07b8b4b7`.
Golden SHA-256:
`4352a32f6e7fc35ed5d08e719cb956014a6a20fe135119c2982dda5f1fcbc72c`.
Report:
`target/validation/nle-backlog-complete/20261007T093719Z-complete-golden-635a1bc7/complete-golden-consecutive-report.json`.
Local receipts `acceptance-joint-native-history.json` and
`acceptance-marquee-current-modal-resource-lut.json` retain exact source/binary
hashes and logs. All tracked source and native binaries remained unchanged during
these gates. Workspace/all-targets/all-features Clippy with warnings denied,
formatting and whitespace checks passed before both implementation/test commits.

This qualifies the named Windows workflow and owned SDK fixture. Native audio
joint time/value output, complete vendor/bus/MIDI/editor contracts, physical
high-DPI input, macOS, broader i18n and performance matrices remain under the
active original goal. Subsequent documentation commits do not imply a new binary.

## Native CLAP/VST3 joint time/value history and PCM

Test-only commit `e7c7edb2b05ec302a79269bb09ca9573989ce47c` extends the existing
native group/clipboard history cases. CLAP and VST3 explicitly executed: 2 passed,
0 failed, 0 ignored; 122.66s (test-target compilation 2m23s). Both Auto and
Continuous constraints are exercised for each format.

From a fresh reopened constrained curve, one typed TransformKeyframes action
moves the final two stable keys by 32/48000 seconds and 0.05 author value units.
Missing IDs, a collision with the first key and a locked Track reject the whole
request. Unselected author time and raw value remain unchanged; constraint
normalization can reshape neighboring handles. Selected IDs and interpolation
intent are retained. Actual native PCM changes after admission; zero displacement
retains generation/history and the successful request has exactly one Undo.
Undo restores the complete sequence and PCM. Saving the undone state retains
live Redo, and fresh Unicode undone/redone archives have empty history and
clipboard, identical canonical curves/output, and the actual SQLite media asset.

The shared production audio program renders 257 stereo frames for both realtime
64/64/129 and offline 37/59/161 block partitions. Every channel/sample compares
against the canonical exact-time curve multiplied by the probed WAV input,
within the unchanged 1e-6 limit; the two partitions produce identical PCM.
These checks run before/after refusal, edit, Undo, Redo and reopen.

The receipt distinguishes clean test source `e7c7edb` from product-worker source
`a258187`. The qualified product was reused with SHA-256
`685cd2bcdb644221adbb8966de06d9cc0333021212fbe708b9275ae4cc129b05`.
The only changed crate file since that worker build is within the existing
cfg(test) audio_rendering module; its production prefix and all other crate
sources are unchanged. Product and both owned SDK fixture hashes remained
unchanged throughout the gate. This is not a claim of rebuilding the worker at
the test-only revision. Receipt `acceptance-audio-joint-native-history.json`
retains separate source identities, hashes, exact log and terminal result.
Full workspace/all-targets/all-features Clippy with warnings denied, fmt and
whitespace checks passed before the test commit. Shalom Wang is author and
committer; nothing was pushed.

This completes the named stereo Float32 main-bus reference joint-output
history slice. It does not establish arbitrary vendor, multi-bus, MIDI, native
editor, macOS or performance completeness. The original goal remains active.

## Notification history inspection and bounded priority

Two actual runtime regressions preceded the repair: opened history exposed only
the current message to accessibility clients, and a burst of successes evicted
both unexpired errors. The existing StatusBar and toast overlay now own the fix;
no additional authoring or message authority was introduced.

Actual EventRouter tests exercise English, Chinese and expanded Chinese pseudo
copy, full visible-page messages and opaque Unicode paths, newest-first arrow
and page navigation, Home/End, modifiers, boundaries, empty/short/replaced
history, focus preservation and FocusLost/Escape dismissal. Inspection publishes
no editor actions or author generation changes. The narrow painter test uses
custom theme padding and checks elided drawing independently of full accessible
text. The existing two-toast limit preserves error/warning priority during a
1,000-success burst; suppressed facts do not repaint or replay after expiry.

Before the final paint refinement, the full all-features App library suite
executed 2,618 passed, 0 failed, 69 ignored (680.92s); the dedicated modal filter
executed 16 passed, 0 failed (24.05s). After the final clip/theme/paint refinement,
history executed 4 passed (7.75s), notifications 7 passed (0.19s), and the locale
filter 30 passed (9.69s), all with 0 failed and 0 ignored. Full workspace,
all-targets, all-features Clippy with warnings denied passed (2m08s), followed by
formatting and whitespace checks. Counts from overlapping filters are not added.

These are source-level routed and draw-command checks, not a new native worker
build or native screen-reader/physical keyboard/macOS qualification. The prior
source-bound native modal/resource/LUT and joint output results above retain
their original product identities. The original overall goal remains active.

## Declared-default parameter reset and empty-selection menus

Reset enters the existing parameter authoring transaction for Clip intrinsic,
visual Effect/Mask, audio automation and static audio processor parameters.
The current definition supplies the default; reset removes the complete authored
animation, including disabled retained keys, while preserving owner, property
and parameter identities. Removing shared key storage replaces the author list
rather than copying Undo-owned keys merely to clear them.

Actual authoring and routed tests cover vector, scalar, text and integer defaults,
stale and locked targets, no-op generation/history, constrained Auto/Continuous
keys, one Undo restoring exact controls/IDs, saving the undone state with live
Redo, and fresh Unicode undone/redone archives with empty history. Mask shape
keys are retained. English, Chinese and expanded Chinese pseudo Inspector
buttons and audio menus route the existing typed actions; default and read-only
states disable reset.

The audio keyboard case reproduced an actual empty-selection failure: Shift+F10
required a selected point before building parameter commands. The shared curve
now opens its factory at the plot center without keys, and blank right-click
retains inspection state. Empty/virtual-only geometry, read-only commands,
Escape, focus and selection preservation are covered. A separate failed widget
test incorrectly expected an empty constructor input to remain an empty display;
the test now snapshots the constructor's actual virtual endpoints.

The focused four-package union-feature parameter filter passed: App 27 (10.14s),
Core 14 (0.01s), Timeline 7 (0.01s), Widgets 1 (0s), with zero failures or ignored
tests. The subsequent complete all-features library invocation for these four
packages passed its runtime stage; Timeline executed 242 passed (0.75s), Widgets
1,112 passed (593.12s), with zero failures or ignored tests. Its initial strict
Clippy stage rejected a redundant clone of a Copy rack address; that expression
was corrected directly without a lint suppression.
The corrected final parameter filter again passed App 27 (9.62s), Core 14,
Timeline 7 and Widgets 1 (each below 0.01s), all zero failed/ignored. Complete
workspace/all-targets/all-features Clippy with warnings denied passed (4m18s),
followed by formatting and whitespace checks.

These checks qualify source authoring, persistence and routed UI behavior. The
native product and SDK/output receipts above retain their original identities;
this slice does not establish newly rebuilt native reset output, physical input,
screen-reader or macOS qualification. The overall goal remains active.
