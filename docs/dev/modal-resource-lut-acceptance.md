# Modal, resource and LUT acceptance

## Latest qualified native-output snapshot

Windows qualification on 2026-10-06 used clean, unchanged production source
`4d2a21e420a99620a2a66279d4ed83e3503f9ef6` and rebuilt product SHA256
`e5d5de9d274c30e38345d55c30e5da6ee7a0e7bd024fb23e51015d85f877a51f`.
This receipt qualifies that exact clean source, including curve preview admission.
Later temporal UI changes are qualified separately below; this native receipt
does not qualify a later production binary.

| Scope | Evidence |
| --- | --- |
| Modal keyboard | 16 App modal tests passed on this commit. Actual EventRouter exercises Tab and Shift+Tab containment, background input blocking and focus restoration. Pending resource confirmation leaves focus order; timeout permits retry. |
| Resource authoring | 103 resource tests passed. Cancelled, edited, stale, replaced and locked drafts cannot commit; Save As changes invalidate the inspected directory. Confirmation enters the author session. Binding a LUT does not import media into the asset library. |
| Process supervision | Both ignored supervision tests explicitly passed: cancellation/timeout reaps the owned child, retry works, and oversized responses leave the service reusable. |
| Native resource execution | Three ignored tests explicitly passed with the rebuilt product: native Unicode paths/helper, actual Host asynchronous confirmation, and native SDK path cancel/Undo/SQLite/Unicode `.mdp` reopen with exact pixels. |
| Undo and durable state | Saving an undone snapshot retains live Redo. Fresh reopen restores saved author state with empty history. The ignored native curve test explicitly passed author edit/Undo/saved Redo/fresh reopen/output checks. |
| Exact integer keys | A current App regression passed three cases: i64 extrema and a value above 2**53. Each case saves and reopens both undone and redone Unicode archives, comparing the entire author Sequence and fresh history. Canonical Integer keys retain all bits; keyboard value increments retain exact owner time, identity and Hold interpolation. |
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
results supersede the earlier library counts for this patch, not the native
product identity above. Ignored native tests require rebuilt-product execution.

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
precision or native output claim for the changed source.

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
The qualified 4d2a21e consecutive report is
`target/validation/nle-backlog-complete/20261005T190834Z-complete-golden-bd0354d9/complete-golden-consecutive-report.json`.
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

## Remaining qualification

Chooser input uses FakePlatform; keyboard coverage routes actual UI events but
is not a physical keyboard recording. Native helper, SDK, SQLite, project files,
float pixels, GPU output and encoded roundtrips are real. This is Windows
qualification, not macOS or general third-party plugin compatibility.
NVENC uses `cpu_rawvideo_pipe`; this does not establish zero-copy transport.

Full professional curve interaction, Float pointer primitive-range admission,
a dedicated real-owner handle overflow scenario, rulers/multi-key editing,
a dedicated out-of-soft-range UI-edit native output scenario, plugin author
writes and vendor/bus/platform/localization matrices remain open. The overall
NLE goal remains active.
