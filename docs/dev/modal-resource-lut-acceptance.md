# Modal, resource and LUT acceptance

## Qualified snapshot

Windows qualification on 2026-10-05 used clean, unchanged source
`c977ce1a7da6cc59de3bb7316aaf78d66544eb26` and the rebuilt product SHA256
`9ca0e1136d6278fc762af798fda94f8f2894974f6ff9cd3d182f66294b6e4956`.
The accompanying documentation update does not change production code.

| Scope | Evidence |
| --- | --- |
| Modal keyboard | 16 App modal tests passed. Actual EventRouter exercises Tab and Shift+Tab containment, background input blocking and focus restoration. Pending resource confirmation leaves focus order; timeout permits retry. |
| Resource authoring | 103 resource tests passed. Cancelled, edited, stale, replaced and locked drafts cannot commit; Save As changes invalidate the inspected directory. Confirmation enters the author session. Binding a LUT does not import media into the asset library. |
| Process supervision | Both ignored supervision tests were explicitly run and passed: cancellation/timeout reaps the owned child, retry works, and oversized responses leave the service reusable. |
| Native resource execution | Three ignored tests explicitly passed using the rebuilt product: native Unicode paths/helper, actual Host asynchronous confirmation, and native SDK path cancel/Undo/SQLite/Unicode `.mdp` reopen with exact pixels. |
| Undo and durable state | The real cube test saves an undone snapshot, retains live Redo, and restores identical pixels after Redo. Fresh reopen restores saved author state with an empty history. A fourth native test explicitly passed curve author edit/Undo/saved Redo/fresh reopen/output checks. |
| LUT pixels | A real Unicode `.cube` swaps red/blue on positive-alpha samples, retains alpha, and preserves zero-alpha hidden RGB. The shared compiled graph returns identical float pixels after durable reopen. |
| GPU and encoded output | Complete Golden passed 3/3, seven stages per round, natural exit 0 each. Source attestation remained clean/stable and baseline eligible. DX12 output had zero CPU fallbacks; H.264 decoded maximum channel error was 4/255 each round. |

The modal and resource filter counts overlap and must not be added as unique
coverage. All five ignored resource tests were executed separately; ignored
results were never counted as passes.

## Reproduce and locate evidence

Run `cargo test -p mondrian-app --all-features --lib modal -- --test-threads=1`
and the corresponding `resource` filter. Explicitly execute the ignored tests
above with a freshly built `mondrian` product and the owned SessionProbe fixture;
resource supervision also requires `MONDRIAN_RESOURCE_INSPECTION_TEST_PYTHON`.
When launching an App test executable directly on Windows, include the product
`target/debug` directory in `PATH` for its native DLLs. Missing loader setup is
an execution environment failure, not a test result.

The current App test binary separately passed 16 modal tests and 103 resource
tests; the five ignored resource tests and the ignored native curve test were
explicitly run and passed. These checks include the exact curve insert request
contract and subsequent test-helper module relocation.

The exact-source complete output gate is
`scripts/validation/invoke-complete-golden-project-gate.ps1`.
This run's consecutive report is
`target/validation/nle-backlog-complete/20261005T091432Z-complete-golden-662d02a7/complete-golden-consecutive-report.json`.
It retains per-round reports, process logs, build log, source attestation,
product hashes, pixel errors and encoder diagnostics.

## Execution environment

The first current-source Golden attempt built successfully but failed at Project
startup with Windows access denied before any output stage. Product authority
locks deliberately live in the fixed LocalAppData namespace, outside the
workspace sandbox; changing TEMP must not split cross-process exclusion.
Running the identical clean source/product with the required native execution
permissions passed all three rounds. No lock bypass or alternate product
interpretation was added. The failed attempt remains recorded in
`target/validation/nle-backlog-complete/20261005T090302Z-complete-golden-4208cbd7/complete-golden-consecutive-report.json`.

## Remaining qualification

Chooser input uses FakePlatform; keyboard coverage routes actual UI events but
is not a physical keyboard recording. Native helper, SDK, SQLite, project files,
float pixels, GPU output and encoded roundtrips are real. This is Windows
qualification, not macOS or a general third-party plugin compatibility claim.
NVENC uses `cpu_rawvideo_pipe`; this evidence does not establish zero-copy
transport. Full professional curve interaction, plugin author writes and
vendor/bus/platform/localization matrices remain on the roadmap. The overall
NLE goal remains active.
