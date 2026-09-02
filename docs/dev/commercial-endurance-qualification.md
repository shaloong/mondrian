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
files. Each returned snapshot is privately bound to its phase kind; the
supervisor rejects an Export-only zero-realtime snapshot in Playback or
Recovery.

Use the validation-only `EnduranceExecutionOwners` group for the software
Preview/Audio/GPU lifetime. Its consuming shutdown must complete before the
terminal sample and consumes the phase's complete `AppState`, proving that its
Playback binding cannot survive terminal projection. A separately
constructed Audio Playback instance is not closure evidence for workers pumped
by that state. Supply an explicit GPU retirement timeout appropriate to the
approved rig; `timed_out`, a rejected retirement handoff, worker panic, or an
incomplete retirement receipt is a failed closure and must never be rewritten
as quiescence. The detached progress worker remains the resource authority
after a timeout, so the containing validation process must also remain inside
the external process-tree supervision policy until it is reaped.

An unexpected PCM render-worker exit replaces the App Audio owner only after
its cumulative failure counters are absorbed into the validation ledger.
Endurance capture reads the ledger plus the current owner; never read the new
`ExecutionUnavailable` snapshot alone or historical counters can regress.

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
public producer seam. The campaign coordinator routes sealed Export events into
the crate-private artifact recorder. The recovery recorder and counter remain
unavailable to production callers until typed, externally recomputable
before/after operation receipts exist; caller-authored SHA strings are not an
acceptable substitute. The supervisor automatically seals and publishes full
chunks and generates the raw producer JSON plus normalized report. Finish
executed phases with final Reference Output accounting and one clean schema-4
`ExportQueueShutdownEvidence`; a joined-late, panicked, timed-out, detached, or
owner-abandoned worker receipt is terminal failure even when its pending/active
gauges are zero. Started/closed decoded-audio owner counts must also match, with
zero dirty closures and zero active owners. Use `finish_not_run` before any
sample when an external
prerequisite is absent. Reference diagnostics do not yet prove vendor
callback-thread/device-session consumption. Commit phases in profile order and
call `seal_manifest` once.

Before creating a fresh `AppState` or any phase owner, let
`run_endurance_campaign` compile the checked-in leaf into
`PreparedEnduranceWorkload`. Concrete runtimes receive that prepared value, not
the contract path. They must obtain `EndurancePhaseAdmission` through its
capability inventory: Timeline playback fixture, frozen Export fixture, Audio
Device, physical Reference provider, external lock, independent Export
verifier, and each of seek/surface-reopen/export-retry/cache-pressure recovery
are distinct capabilities. A non-empty missing list is the only legal
`NotRun`; parse/hash/identity/policy/duration/counter mismatches abort the
campaign as invalid input.

For Continuous Export, construct one
`app::endurance_export::FrozenRepeatedExportPhase` after prepared-workload
capability admission and before the phase starts. Use a fresh `AppState` whose
Queue has no retained jobs, a single-file media preset, an existing real output
directory, a bounded ASCII artifact prefix, and explicit nonzero independent
verification limits. The owner captures the ordinary Timeline Export snapshot
and delivery configuration once. Call `poll` from the runtime pump; it admits
strictly one unique `CreateNew` artifact at a time and returns only sealed
`ExportArtifactVerified` events after exact durable publication and independent
full decode. Forward those events unchanged to the campaign coordinator. Never
enqueue UI or recovery jobs into this Queue, mutate the Timeline expecting a
later artifact to observe it, or synthesize artifact/validator/report digests
from Queue state.

At phase close call `begin_close`, keep polling until the current artifact is
published and verified, require `is_quiescent`, then drop the phase owner before
the complete `AppState` consuming shutdown. Do not cancel the active attempt:
the Continuous Export workload explicitly forbids cancellation. A decode
timeout, cancellation, malformed terminal progress, empty selected video,
changed file, Queue contamination, mismatched durable path, or non-exact history
cleanup is a terminal phase failure, never an artifact counter increment. The
separate Concurrent Recovery runtime must own its typed cancel/retry protocol;
it cannot reuse this non-cancelling owner.

Production orchestration will enter through a public high-level App runner that
uses `run_endurance_campaign` internally. Its concrete
`EnduranceCampaignRuntime` must pump the actual owners until each absolute
campaign deadline, return one coordinator-bounded capture envelope, and
synchronously close Playback/Preview/Audio/GPU/Reference/Export before the
coordinator takes the final sample. That concrete three-phase runtime and thin
validation executable are not implemented at this checkpoint; its Continuous
Export leaf must compose the existing frozen repeated-Export owner rather than
reimplementing the loop. The coordinator owns cadence and native
`ProductProcessTree` probing, phase order, and evidence publication. If a
physical provider or required fixture is absent, `begin_phase` must return the
typed `NotRun` receipt created by prepared-workload admission before starting
work; a started phase cannot be downgraded to
`NotRun`. Any `begin_phase` error may follow partial owner creation and therefore
must retain enough state for the supervisor's exactly-once consuming cleanup.
An `Ok` shutdown receipt is still rejected immediately when any software
worker, supervised child, Export worker, pending job, or active job remains;
the coordinator must not take the final sample or admit the next phase.

At every profile cadence:

1. record scheduled monotonic offset;
2. stamp the capture-envelope start immediately before the native
   product-process-tree memory query;
3. collect internally consistent Playback Evidence, Reference Output
   diagnostics, Export endurance diagnostics, outstanding leases/queues, and
   independent artifact-verification totals without claiming one cross-thread
   linearization instant; snapshot adapters may refresh bounded diagnostics but
   must not schedule, pump, or poll phase work;
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

## macOS / Linux transfer record

The current checked-in endurance profile is explicitly Windows-only: it
requires `windows_private_commit`. Do not relabel that profile or compare its
byte thresholds directly with another operating system. Before a macOS or
Linux campaign, approve a distinct profile whose process-tree backend and
metric match the native Adapter:

- macOS: `MacOsLibprocProcessTree` with `mac_os_physical_footprint`;
- Linux: `LinuxProcfsProcessTree` with `linux_anonymous_resident`.

The following work is intentionally recorded as **not executed on the current
Windows host** and must be transferred to each physical machine after the local
software backlog is complete:

1. Use Rust 1.97.1, the exact clean source revision, and a target-local empty
   `target` directory. Run `cargo fmt --all -- --check`, then serial
   `cargo check -p mondrian-app --features validation -j 1`, affected tests,
   and `cargo clippy --workspace --all-targets --all-features -j 1 -- -D warnings`.
2. Prove the native product-process-tree inventory with descendants present.
   Retain backend, metric, observed-process count, inventory attempts, and
   completeness; a current-process fallback is not endurance evidence.
3. Execute the matching COL-046 physical row first: Metal plus CoreGraphics /
   AppKit EDR on macOS; Vulkan plus the active Wayland color-management or X11
   ICC path on Linux. Follow
   `docs/dev/platform-driver-display-qualification.md` and preserve the exact
   target package/runtime-image/build-provenance hashes.
4. Exercise the platform-specific media/output paths used by the workload
   (VideoToolbox and macOS playback QoS, or VA-API and Linux scheduling/audio),
   then verify that ordinary teardown returns promptly and the consuming App
   owner receipt reports no timeout, detach, child process, or residual native
   surface.
5. Only after those software and physical gates pass, run the real 72-hour
   campaign with the separately approved platform-native memory budget and the
   required DeckLink/AJA/reference-monitor rig. Preserve `not_run` rather than
   substituting simulated output when any prerequisite is absent.

Results from macOS and Linux remain independent platform rows. They may be
aggregated only through the existing cross-machine manifest rules; logs or
receipts from one operating system cannot fill another row.
