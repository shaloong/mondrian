# Color commercialization: remaining qualification work

This is a living transfer checklist, not a qualification certificate. Completed
software contracts, passing developer tests, and physical qualification are
different evidence. Update this list after each coherent implementation block;
do not promote an unavailable or failed measurement to a pass.

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

## Local Windows work still in progress

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

Remaining local COL-047 implementation/validation blocks:

1. Preserve the exact Preview/GPU owners on campaign startup/bind failure and
   consume them through the shared shutdown deadline, rather than returning
   only an error and later reporting App-only closure. Preserve typed startup
   failure evidence past the product entrypoint, even when cleanup succeeds.
   Cover partial Window/Headless GPU construction and preserve normal raw
   terminal receipts as well as the optional campaign snapshot. The now-verified
   Renderer retirement/progress protocol is the shared prerequisite, not proof
   that these constructor/public-result paths already close.
2. Extract cohesive performance owner-closure support from the large test
   module and tighten validation-only module/cfg boundaries without broad
   warning suppression.
3. Make qualified FFmpeg command rejection a typed error; a command pointing
   at an assumed-nonexistent sentinel executable is not fail-closed admission.
4. Close the capsule lifecycle: sealed namespace, spawn-time admission,
   retained child leases, explicit Windows access-control evidence, and
   fallible process-owner cleanup. Static owners do not run TempDir cleanup
   at process exit. Never recover orphans by deleting a filename-prefix glob.
5. Add pre-loader authority and post-load image/object attestation. Current
   loaded-module canonical paths do not prove the identity of an image mapped
   before the retained source handle was acquired. Do not substitute a partial
   PE hash for complete image identity.
6. Run the locally executable real Windows campaign smoke after those
   boundaries close. A short smoke cannot certify the physical 72-hour run.
7. Qualify the CPU-output bottleneck recommendation: the current generic
   `move_preview_output_boundary_to_gpu` action is not universally applicable.
   Diagnostics must preserve mandatory CPU cache publication, respect route
   requirements, and distinguish processor/memory optimization from legal GPU
   output admission (including a hybrid path if actually supported).

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
