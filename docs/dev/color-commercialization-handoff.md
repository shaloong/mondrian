# Color commercialization: remaining qualification work

This is a living transfer checklist, not a qualification certificate. Completed
software contracts, passing developer tests, and physical qualification are
different evidence. Update this list after each coherent implementation block;
do not promote an unavailable or failed measurement to a pass.

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
  cold-start cost, not a proven fix. Two independently initialized widget font
  measurement owners are a concrete candidate; separate their initialization
  from model/layout and machine-load effects before qualifying an optimization.
- Decode/cache proved real persistent publication and a new request-local
  lookup/hit. Its render maximum was 64,201 us against 50,000 us: preparation
  2,157 us, composite 4,723 us, CPU output boundary 57,321 us. The boundary
  includes processor preparation, Program Output, monitor adaptation, and
  quantization, not cache disk publication. Keep the required CPU publication,
  output extent, color semantics, and existing budget intact when optimizing.
- Export/Audio initially stopped at an OCIO dependency-checkout write denial,
  before measurement. After authorized dependency rebuilding, the 1080p29.97
  Export repeat passed: 96 frames, zero budget misses, maximum 22 ms, with
  passing pixel/color evidence. The Audio repeat passed all 12 scalar/SIMD
  load-matrix cells with zero deadline misses. Both reports passed the exact
  suite eligibility validator; the initial build denial was neither a
  performance failure nor a passing skip.

These are developer-run observations on an uncommitted tree, not a sealed
COL-031 or COL-047 reference-machine result. Raw local reports are under
`target/perf/col047-final/` and may be removed during a clean rebuild.

Remaining local COL-047 implementation/validation blocks:

1. Resolve the two observed performance problems above; retain failed samples
   alongside subsequent measurements.
2. Preserve the exact Preview/GPU owners on campaign startup/bind failure and
   consume them through the shared shutdown deadline, rather than returning
   only an error and later reporting App-only closure.
3. Extract cohesive performance owner-closure support from the large test
   module and tighten validation-only module/cfg boundaries without broad
   warning suppression.
4. Make qualified FFmpeg command rejection a typed error; a command pointing
   at an assumed-nonexistent sentinel executable is not fail-closed admission.
5. Close the capsule lifecycle: sealed namespace, spawn-time admission,
   retained child leases, explicit Windows access-control evidence, and
   fallible process-owner cleanup. Static owners do not run TempDir cleanup
   at process exit. Never recover orphans by deleting a filename-prefix glob.
6. Add pre-loader authority and post-load image/object attestation. Current
   loaded-module canonical paths do not prove the identity of an image mapped
   before the retained source handle was acquired. Do not substitute a partial
   PE hash for complete image identity.
7. Run the locally executable real Windows campaign smoke after those
   boundaries close. A short smoke cannot certify the physical 72-hour run.

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

## Physical and external-application work

- P0 COL-010: real HDR/P3/ICC Viewer display qualification.
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
