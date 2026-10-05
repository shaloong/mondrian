# Modal, resource and LUT acceptance

## Qualified snapshot

Windows qualification on 2026-10-05 used clean, unchanged production source
`1ade8887e87ba7c49f2c6bb07c240f7822e65b79` and rebuilt product SHA256
`75bac4a3ad892717e68e1a2a2b13f9d1bd3c09eeb5f25249940774def980e1aa`.
The accompanying documentation update does not change production code.

| Scope | Evidence |
| --- | --- |
| Modal keyboard | 16 App modal tests passed on this commit. Actual EventRouter exercises Tab and Shift+Tab containment, background input blocking and focus restoration. Pending resource confirmation leaves focus order; timeout permits retry. |
| Resource authoring | 103 resource tests passed. Cancelled, edited, stale, replaced and locked drafts cannot commit; Save As changes invalidate the inspected directory. Confirmation enters the author session. Binding a LUT does not import media into the asset library. |
| Process supervision | Both ignored supervision tests explicitly passed: cancellation/timeout reaps the owned child, retry works, and oversized responses leave the service reusable. |
| Native resource execution | Three ignored tests explicitly passed with the rebuilt product: native Unicode paths/helper, actual Host asynchronous confirmation, and native SDK path cancel/Undo/SQLite/Unicode `.mdp` reopen with exact pixels. |
| Undo and durable state | Saving an undone snapshot retains live Redo. Fresh reopen restores saved author state with empty history. The ignored native curve test explicitly passed author edit/Undo/saved Redo/fresh reopen/output checks. |
| Exact integer keys | A current App regression passed three cases: i64 extrema and a value above 2**53. Each case saves and reopens both undone and redone Unicode archives, comparing the entire author Sequence and fresh history. Canonical Integer keys retain all bits; keyboard value increments retain exact owner time, identity and Hold interpolation. |
| LUT pixels | A real Unicode `.cube` swaps red/blue on positive-alpha samples, retains alpha, and preserves zero-alpha hidden RGB. The shared compiled graph returns identical float pixels after durable reopen. |
| GPU and encoded output | Complete Golden passed 3/3, seven stages per round, natural exit 0 each. Source attestation remained clean/stable and baseline eligible. DX12 output had zero CPU fallbacks; final H.264 decoded maximum channel error was 4/255 each round. |

The modal/resource/curve filter counts overlap and must not be added as unique
coverage. All five ignored resource tests and the ignored native curve test were
executed separately; ignored results were never counted as passes.

## Workspace gates and author format

The all-feature workspace test run passed 7521 tests, failed zero and ignored
159, including integration and documentation tests. App alone passed 2548,
failed zero and ignored 62. A subsequent semantics-preserving Clippy cleanup
collapsed one nested `if`; strict workspace all-target/all-feature Clippy,
formatting and diff checks then passed before commit. The current commit's
modal, resource, integer-history and six explicit native tests all passed after
rebuilding its App test target.

Schema 31 stores discriminated Number(f64) and Integer(i64) channel keys.
Integer normalization, Hold evaluation, edits and prepared readback preserve
canonical bits. Disabled animation still permits exact author-key lookup.
Floating geometry is an explicit projection; it cannot distinguish adjacent
wide integers. Interpolated integer schemas retain the existing exact f64-domain
restriction. The alpha format rejects prior schemas and primitive numeric key
payloads directly, without migration aliases or a legacy decoder.

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
The current consecutive report is
`target/validation/nle-backlog-complete/20261005T133819Z-complete-golden-25a4f96e/complete-golden-consecutive-report.json`.
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

## Remaining qualification

Chooser input uses FakePlatform; keyboard coverage routes actual UI events but
is not a physical keyboard recording. Native helper, SDK, SQLite, project files,
float pixels, GPU output and encoded roundtrips are real. This is Windows
qualification, not macOS or general third-party plugin compatibility.
NVENC uses `cpu_rawvideo_pipe`; this does not establish zero-copy transport.

Full professional curve interaction, Float pointer admission and untouched raw
channel precision, rejected-handle draft restoration, rulers/multi-key editing,
a dedicated out-of-soft-range UI-edit native output scenario, plugin author
writes and vendor/bus/platform/localization matrices remain open. The overall
NLE goal remains active.
