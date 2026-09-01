# Commercial Endurance Qualification Runbook

This runbook produces the COL-047 72-hour commercial stability evidence. A
short test, accelerated frame loop, compilation, or simulated reference-output
Adapter is never eligible evidence.

## Required environment

- one clean, unchanged source revision and exact release-candidate package;
- the executable image hash actually launched, not only an installer/archive;
- an approved COL-046 platform/driver/display cell and unchanged environment;
- a 32 GiB professional reference machine with the required corpus available;
- licensed DeckLink or AJA bridge, physical output hardware, stable external
  reference, and independent output monitoring/capture;
- sufficient create-only evidence storage for 72 hours plus exports;
- an externally approved `mondrian-endurance-replay` binary hash and exact
  checked-in profile-file hash;
- an external `external-commercial-endurance-authority-v1` manifest with one
  single-use challenge, exact release bindings, and exact phase workload and
  producer bindings. Its bytes must be approved and pinned before capture.

If any required hardware, provider, corpus, or trust anchor is absent, record
the phase as `not_run`. The aggregate result is `incomplete`; do not substitute
the simulated Adapter.

## Build before capture

Build the release candidate and replay tool before the campaign. Record hashes
and build provenance outside the evidence bundle. Do not run Cargo during any
measured phase.

```powershell
cargo build -p mondrian-app -p mondrian-platform-core --release --features mondrian-app/validation -j 1
$replay = "target/release/mondrian-endurance-replay.exe"
$replaySha = (Get-FileHash -LiteralPath $replay -Algorithm SHA256).Hash.ToLowerInvariant()
$profile = "tests/validation/commercial-endurance-qualification.json"
$profileFileSha = (Get-FileHash -LiteralPath $profile -Algorithm SHA256).Hash.ToLowerInvariant()
```

The capture authority must pin those two values plus source, release candidate,
product artifact, runtime image, build provenance, machine report, and COL-046
cell hashes before starting. Its `authority_id` is
`external-commercial-endurance-authority-v1`; it carries a non-placeholder
`single_use_challenge` and one phase binding for each exact workload digest,
producer owner, and verifier identity.

## Capture protocol

Use the validation App composition with the production Playback, Preview,
Audio, Reference Output, Renderer, and Export Modules. The
`app::endurance_qualification` converts their owner snapshots and the native
product-process-tree memory result into `EnduranceSample`. The validation
driver must derive recovery/resource/artifact-verification facts from the
production owners; the capture authority pins that producer, and the evidence
supervisor emits typed raw events rather than accepting arbitrary counter/hash
files.

Use the validation-only `EnduranceExecutionOwners` group for the software
Preview/Audio/GPU lifetime. Its consuming shutdown must complete before the
terminal sample and must receive the phase's actual `AppState`; a separately
constructed Audio Playback instance is not closure evidence for workers pumped
by that state. Supply an explicit GPU retirement timeout appropriate to the
approved rig; `timed_out`, a rejected retirement handoff, worker panic, or an
incomplete retirement receipt is a failed closure and must never be rewritten
as quiescence. The detached progress worker remains the resource authority
after a timeout, so the containing validation process must also remain inside
the external process-tree supervision policy until it is reaped.

The concrete endurance runtime inside `mondrian-app` must drive realtime
software work through its crate-private `app::headless_realtime_playback`
composition. Its paired session binds Preview completion wakes, GPU device
generation, and renderer-qualified hardware-decode admission without raising
thread priority. Open a fresh realtime residency only for an admitted
observation window; this enters native playback scheduling and owns the shared
interval coordinator until explicit finish. For realtime intervals and
terminal-demand closure, that coordinator is the only allowed interpreter of
exact candidate identity, queue-visible publication, callback retirement,
successor preparation, lookahead, and bounded waiting. Qualification-specific
counters belong in a `HeadlessGpuExecutionObserver`; do not copy the
coordinator from a performance test or retain scheduling across setup,
diagnostics, or blocking shutdown. The eventual standalone validation binary
must invoke a public high-level App library runner; it must not expose or call
these low-level crate-private owners directly.

Construct `EnduranceRunCapture` from the exact profile, release identity, and
capture-authority file. Start phases only through `begin_phase`; it verifies the
raw checked-in workload contract bytes. Submit each owner observation through
`EndurancePhaseCapture::capture_and_push`; direct sample insertion is not a
public producer seam. Record independently validated artifacts through
`record_export_artifact_verified` and recovery operations through
`record_recovery_step_completed`. The latter enforces the checked-in four-step
cycle order. The supervisor automatically seals and publishes full chunks and
generates the raw producer JSON plus normalized report. Finish executed phases
with typed Reference Output diagnostics and `ExportQueueShutdownEvidence`, or
use `finish_not_run` before any sample when an external prerequisite is absent.
Commit phases in profile order and call `seal_manifest` once.

Before recording an artifact event, call
`mondrian_export::verify_export_artifact` with the stable Export job/artifact
identity and explicit nonzero file-size and decode-time limits, then construct
the App event with
`EnduranceCampaignEvent::export_artifact_verified`. Do not synthesize the
artifact, validator, or report digests from queue state. The verifier reopens
the final regular file into a bounded immutable snapshot, hashes it, probes its
typed streams, fully decodes every advertised video/audio stream under a
supervised deadline, hashes decoded
output, and rechecks the encoded bytes before issuing its sealed receipt. A
decode timeout, cancellation, malformed terminal progress, empty selected
video, changed file, or exceeded evidence-output bound is a verification
failure, never an artifact counter increment. Artifact identities cannot repeat
within one phase.

Production orchestration should normally enter through
`run_endurance_campaign`. Its concrete `EnduranceCampaignRuntime` must pump the
actual owners until each absolute campaign deadline, return an atomic snapshot,
and synchronously close Playback/Preview/Audio/GPU/Reference/Export before the
coordinator takes the final sample. The coordinator owns cadence, native
`ProductProcessTree` probing, phase order, and evidence publication. If a
physical provider or required fixture is absent, `begin_phase` must return
`NotRun` before starting work; a started phase cannot be downgraded to
`NotRun`.

At every profile cadence:

1. record scheduled monotonic offset;
2. start the native product-process-tree memory query on the evidence worker;
3. atomically snapshot Playback Evidence, Reference Output diagnostics, Export
   endurance diagnostics, outstanding leases/queues, and independent artifact
   verification totals;
4. record completion offset and append the exact next sequence;
5. when the 120-sample buffer is full, the supervisor seals it, publishes it
   create-only, fsyncs, and retains only its receipt and digest before
   continuing; the 48-chunk phase limit is enforced before another file is
   written.

The supervisor must keep the root process and every child under its process
containment policy. A daemonized/reparented encoder, inaccessible child, PID
reuse, inventory race, or failed member query invalidates that sample.

Run phases serially in profile order:

- `01-playback-reference-24h`: exact 60 fps program, Audio Device Clock,
  physical scheduled output, required reference lock, and hardware timestamp on
  every completed output frame;
- `02-continuous-export-24h`: repeated frozen exports, durable publication,
  independent re-open/content verification, and queue continuation;
- `03-concurrent-recovery-24h`: playback/reference output while exports run,
  plus the workload-defined seek, surface/device reopen, Export cancel/retry,
  and cache-pressure recovery cycles.

Each phase ends by stopping product work, explicitly retiring Export, draining
Reference Output, releasing Viewer/decoder/resource leases, reaping supervised
children, and then taking the final complete memory sample. The final sample
and terminal record must agree exactly.

## Manifest and directory closure

The run manifest uses `EnduranceRunManifest` schema 1. Every chunk, normalized
owner report, and raw evidence receipt names one distinct leaf file. The
evidence directory must contain exactly those files—no unrelated logs,
subdirectories, links, partials, or extras. The supervisor derives producer
file hashes from the actual regular files; callers do not supply those hashes.
Each normalized producer report must echo its phase and run IDs, the authority
challenge, workload digest, producer owner/verifier identity, raw-evidence
digest, terminal status, event count, independently verified Export count, and
complete recovery-cycle count. Raw evidence contains contiguous typed events.
The independent verifier derives those summaries and compares them with both
the report and terminal owner counters rather than accepting manifest hashes as
self-attestation.

The run clock is monotonic. `started_at_run_us` and `completed_at_run_us` define
serial order; the final sample completion must close exactly against the phase
duration. Wall-clock/UTC fields in external logs are audit metadata only.

## Independent verification

Run the checked-in verifier from a private immutable copy of the evidence. The
report path and its parent must already be chosen; the report itself must not
exist.

```powershell
pwsh -File scripts/validation/verify-commercial-endurance-qualification.ps1 `
  -ProfilePath tests/validation/commercial-endurance-qualification.json `
  -RunManifestPath E:/qualification/endurance/run-manifest.json `
  -ChunkDirectory E:/qualification/endurance/evidence `
  -ReplayBinaryPath target/release/mondrian-endurance-replay.exe `
  -ReplayBinarySha256 $replaySha `
  -ExpectedProfileFileSha256 $profileFileSha `
  -ExpectedCaptureAuthorityPath E:/qualification/endurance/capture-authority.json `
  -ExpectedCaptureAuthoritySha256 '<64-hex>' `
  -ExpectedSourceRevision '<40-hex-clean-source>' `
  -ExpectedReleaseCandidateId 'mondrian-0.2.0-rc1' `
  -ExpectedProductArtifactSha256 '<64-hex>' `
  -ExpectedRuntimeImageSha256 '<64-hex>' `
  -ExpectedBuildProvenanceSha256 '<64-hex>' `
  -ExpectedMachineReportSha256 '<64-hex>' `
  -ExpectedPlatformCellSha256 '<64-hex>' `
  -OutputPath E:/qualification/endurance/report.json
```

`ChunkDirectory` is the legacy parameter name for the complete evidence
directory: chunks, normalized owner reports, and raw evidence all live there.
The verifier rejects non-regular inputs, extra/missing evidence, hash mismatch,
environment drift, changed evidence during replay, an unapproved tool/profile,
timeout, and every non-`qualified` result. Preserve the profile, manifest,
chunks, owner reports, raw evidence, trust anchors, replay executable, and final
report together as release evidence.

## Software versus HITL

Ordinary tests may use synthetic time and fake counters only to exercise
deterministic protocol logic. This machine's green software gates establish no
commercial stability claim. COL-047 closes only after an approved rig finishes
all 72 wall-clock hours with physical Reference Output and a qualified report.
