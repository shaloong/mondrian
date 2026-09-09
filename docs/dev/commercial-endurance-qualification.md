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
- an exact bounded schema-2 machine plan whose original file SHA-256 binds the
  canonical `.mdp`, external-source inventory, active Sequence, physical Audio
  and Reference routes, Export plans, recovery targets, verifier tools, and
  timeouts. On Windows `verifier_tools.runtime_files` must be the strictly
  ordered exact set of every `.dll` beside both FFmpeg executables, with a
  lowercase SHA-256 for each; omission or an extra declaration fails closed.
  Its Reference request must explicitly include the ANC policy;
- an external `external-commercial-endurance-authority-v2` manifest with one
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
cargo build -p mondrian-app -p mondrian-platform-core -p mondrian-validation-launcher --release --features mondrian-app/validation -j 1
$replay = "target/release/mondrian-endurance-replay.exe"
$replaySha = (Get-FileHash -LiteralPath $replay -Algorithm SHA256).Hash.ToLowerInvariant()
$profile = "tests/validation/commercial-endurance-qualification.json"
$profileFileSha = (Get-FileHash -LiteralPath $profile -Algorithm SHA256).Hash.ToLowerInvariant()
```

The capture authority must pin those two values plus source, release candidate,
product artifact, runtime image, build provenance, machine report, COL-046
cell, and machine-plan hashes before starting. Its `authority_id` is
`external-commercial-endurance-authority-v2`; it carries a non-placeholder
`single_use_challenge` and one phase binding for each exact workload digest,
producer owner, and verifier identity.
The App parses that complete strict schema before constructing a phase owner;
missing or unknown identity/phase fields fail capture admission. The campaign
then transfers the exact prepared machine-plan object into the runtime once,
and the machine factory must derive preflight and phase construction from the
same borrowed plan rather than a self-reported digest.
Runtime cadence, recovery, Surface, and shutdown deadlines are likewise derived
from that plan; the campaign API has no parallel timeout argument. The external
verifier hashes each bounded JSON input from the same handle bytes it parses and
then requires every such observation to match the immutable replay snapshot.

Prepare campaign input through
`app::endurance_run_request::PreparedEnduranceRunRequest::load`. The strict
schema-1 request must bind absolute regular-file paths and exact SHA-256 values
for the profile, machine plan, capture authority, and every ordered workload.
All path components must use the portable ordinary filename namespace;
relative paths, parent traversal, NTFS alternate data streams, reserved DOS
devices, and trailing-dot/space aliases are invalid.
The identity's profile and machine-plan digests must match those bindings. The
evidence directory must already exist as an empty real directory; the final
manifest must not exist and must be outside the evidence directory. Admission
hashes and parses the capture authority from the same bounded byte read, so a
replacement between separate hash and parse opens cannot be accepted. Only the
prepared request can enter the public product runtime; do not construct a raw
campaign request in a validation binary.

Before creating the fresh `AppState` or any Queue/worker, call
`PreparedEnduranceFfmpegToolchain::prepare_and_install` with that same prepared
machine plan and retain the result in the machine factory. Preparation copies
the exact executable and DLL objects into one private capsule, denies
write/delete sharing on every declared file, retains the capsule directory
against rename/delete, and revalidates the exact entry set before every command.
It uses a capsule-only PATH/CWD for bounded fixed probes, applies the product
semantic runtime gate, and verifies process-loaded runtime identities. Record its
`toolchain_receipt_sha256` alongside the machine-plan digest. A second identity
in one process is invalid. Current macOS/Linux builds return Unsupported and
must remain incomplete until their recorded transfer qualification is done.
This is staged admission infrastructure: the machine-specific factory still
must make successful preparation and retention a construction prerequisite for
the App and every phase owner.
On Windows the in-process check currently proves an exact canonical-path
closure, not independent identity of a module image mapped before its retained
source handle was acquired. The entry-set recheck is also not atomic with
`spawn`. Run only on an access-controlled qualification account, record these
limitations, and do not claim mapped-image/object or hostile-same-user
qualification until the dedicated evidence seam is complete.

For every admitted phase, the machine-specific factory must create a fresh
`AppState` and call `open_endurance_project_fixture` with the exact
`PreparedCommercialEnduranceMachinePlan` before it constructs Playback, Reference, or
Export owners. Do not call ordinary `open_project_file`: ordinary Open may
repair missing base Tracks and therefore changes the fixture being qualified.
The exact seam requires a canonical direct regular file, lowercase SHA-256,
current source Project/Library schemas, matching active Sequence, and pre-authored video plus audio Tracks. It hashes,
parses, and extracts from one retained file object and returns a clean saved
generation-1 Session with empty History. Any failure must return the fresh App
through `FreshEndurancePhaseBuild::Failed` so the campaign still performs its
consuming shutdown. The current non-Windows implementation rejects this call;
add and qualify the recorded immutable-object native Adapter on macOS and Linux
before treating this seam as campaign authority.

Retain the `PreparedEnduranceProjectFixture` returned by that call and pass it
with the same prepared machine plan to
`PreparedEnduranceSourceInventory::prepare`. The complete ordered profile
topology is sealed into that prepared plan at load time; callers cannot supply
a phase subset. The strict schema-1 inventory
binds the Project archive hash, Project/document, root Sequence/revision, exact
per-phase Sequence/media closure, and one canonical path/length/SHA-256 record
for every source in the phase union. The App recomputes Playback, Export, and
Recovery closures through the production Export dependency compiler; the
machine factory must not walk Tracks or Clips. Keep the returned source,
inventory, and parsed Export-preset handles alive until all consuming phase
owners shut down. Reachable proxy-mode Assets, stale probes, changed content,
extra/missing sources, unknown fields, noncanonical paths, or stale Project or
Library evidence are hard invalid-input failures, not `NotRun`. This source
lease is currently Windows-only; macOS/Linux remain fail-closed pending
descriptor-based decode and immutable-object qualification.

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

For Playback/Reference and Concurrent/Recovery, construct
`app::endurance_playback::PersistentTimelinePlaybackPhase` only after the exact
prepared workload and complete capability inventory are admitted. The App must
have one active stopped frame-zero Sequence at the prepared workload's exact
program rate: 60/1 for the original profile, or 60000/1001 for the separately
approved AS-11 profile. Its authored extent
must cover `minimum_playback_presented_frames + 1`: one interval proves the
departed frame before advancing, so the additional terminal guard frame is
required to finish the exact 24-hour count without looping or reaching natural
end. This full extent must still remain after startup Audio alignment; startup
progress cannot consume part of the measured 24-hour window. Pump only through `pump_interval`; every interval must remain in the same
Playback Epoch, advance exactly one frame, prove the departed exact picture, and
retain Audio Device Clock authority. Before a cadence capture call
`settle_window`, collect the owner envelope only after native scheduling has
ended, then call `resume_window`; either operation revalidates the frozen
Sequence ID/revision, Project Author Generation, transport, and coordinate.
Any violation is a permanent phase fault. Close explicitly with `begin_close`
before consuming the complete App owner. A startup error can occur after App
Playback starts, so the runtime must still execute consuming cleanup on the
caller-retained App and execution owners.

This phase owner is not the physical clean-feed producer. The next canonical
Reference pump must branch the full-raster Program Output from the shared
working composite and pair it with the selected public Audio Program; never
submit the monitor/display-transformed Viewer raster to Reference Output.

Construct `EnduranceRunCapture` from the exact profile, release identity, and
capture-authority file. Start phases only through `begin_phase`; it verifies the
raw checked-in workload contract bytes. Submit each owner observation through
`EndurancePhaseCapture::capture_and_push`; direct sample insertion is not a
public producer seam. The campaign coordinator routes sealed Export events into
the crate-private artifact recorder. The recovery recorder accepts only a
sealed canonical receipt (schema 4 for Surface/device reopen, schema 3 for the
other variants) and checks its embedded JSON, SHA-256, exact
cycle/step, and unique operation ID. The seek receipt can only be returned by
`PersistentTimelinePlaybackPhase::recover_seek`, after the real typed product
seek closes one exact Ready target under Audio Device Clock and the frozen
author binding. Caller-authored SHA strings or success booleans are not an
acceptable substitute. Cache pressure can only be returned by
`PersistentTimelinePlaybackPhase::recover_cache_pressure`; it applies real
Critical and Nominal decisions to the settled Preview/GPU pair, requires
nonzero media-cache byte retirement, then proves the same exact Ready picture
and unchanged GPU/Preview/Audio/background/Export failure ledgers. Nominal is
attempted even when Critical application fails. Surface/device reopen is
sealed only by the real winit Window generation owner, and Export cancel/retry
only by the frozen repeated-Export recovery substate; the concrete runtime
forwards those owner receipts unchanged. Surface success additionally requires
the old shutdown identity to equal the receipt's `before` pair and the final
typed Window GPU retirement to be clean and equal its `after` pair; the
independent verifier repeats these checks.
The supervisor
automatically seals and publishes full
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
coordinator takes the final sample. The validation-only concrete three-phase
runtime now composes the existing frozen repeated-Export, persistent Timeline,
canonical Reference, and real Window Surface owners. The remaining thin
validation executable must provide the machine-specific fresh-App factory and
call that runtime; it must not reimplement its loop. The coordinator owns cadence and native
`ProductProcessTree` probing, phase order, and evidence publication. If a
physical provider or required fixture is absent, `begin_phase` must return the
typed `NotRun` receipt created by prepared-workload admission before starting
work; a started phase cannot be downgraded to
`NotRun`. Any `begin_phase` error may follow partial owner creation and therefore
must retain enough state for the supervisor's exactly-once consuming cleanup.
An `Ok` shutdown receipt is still rejected immediately when any software
worker, supervised child, Export worker, pending job, or active job remains;
the coordinator must not take the final sample or admit the next phase.

If a consumed phase later returns an error, inspect
`EnduranceCampaignError::WithTerminalEvidence`: it keeps the original error and
the complete owner-free Realtime, Startup or App-only shutdown receipt, including clean
cleanup followed by publication failure. A receipt is not a successful terminal
sample. Next-phase preparation clears only its error attachment before loading
the next workload, and cannot make a missing terminal sample admissible.
The shared GPU receipt retains both retirement-requested and exact terminal
kind; explicit device destruction fails normal qualification just like loss or
progress failure. Safe resource release is a separate claim. Headless GPU
constructor/bind failures retain their actual owners until consuming cleanup;
Startup receipts name partial Renderer inventory and optional Preview/Waveform.
They never fabricate a normal-session snapshot or turn failed startup into a
qualified phase. Preview/Waveform internal constructor unwinds before returning
an owner remain explicitly unverified. Window partial-start receipts and public
durable raw success reports are still open.

Preview receipt schema 3 explicitly requires the real dependency-observer worker
to join, alongside existing workers and render-cache evidence. Old schema-2
records do not prove this inventory; the strict performance validator rejects
them. Perf startup-error projections are distinct from normal runtime closure,
and successful CPAL/native-surface reports are emitted after closure validation.

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

The run manifest and final report use schema 3. After all phase owners are
terminal, the campaign consumes its selected Surface driver exactly once and
embeds the shared process-local EventLoop owner-closure contract before the
manifest is created. The final report retains the same closure in its evidence
digest. `NotApplicable` is legal only when Concurrent Recovery did not run;
EventLoop Rust-owner return never claims physical native termination. Every chunk, normalized
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
  -ExpectedMachinePlanPath E:/qualification/endurance/machine-plan.json `
  -ExpectedMachinePlanSha256 '<64-hex>' `
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


## Consolidated software regression batch

Prepare related source, receipt-schema and adversarial-test changes before
building the App test target. Run the cheap independent replay first:

```powershell
./scripts/validation/test-window-owner-closure.ps1
cargo test -p mondrian-app --features validation --lib -j 1 -- endurance_ window_outer_receipt surface_reopen_batch_receipt background_runtime preview_render_cache preview_shutdown_evidence --test-threads=1
cargo test -p mondrian-platform-core --test commercial_endurance_qualification -j 1
cargo clippy --workspace --all-targets --all-features -j 1 -- -D warnings
cargo fmt --all -- --check
```

Libtest accepts multiple OR filters after `--`; all selected suites run from
one current-source App binary. Keep Cargo feature/profile/target configuration
fixed, serialize Cargo invocations and retain `target`. A schema-only change
does not require a separate release link. Changed native/GPU lifecycle code
still requires real operation validation. The fast PowerShell test performs
no App or hardware startup.

These gates cover software semantics only. Missing physical phase providers,
exact fixtures or capture prerequisites remain admission-time `NotRun`.
COL-010 HDR/P3, COL-042/043 SDI and reference lock, COL-044 broadcast end-to-end
and wire readback, COL-045 pinned external applications, COL-046 native
platform adapters/matrix and COL-047 actual 72-hour execution remain separate
qualification work. macOS/Linux require the documented native implementation
as well as running tests; copying a Windows report does not qualify them.

## Native pre-loader execution and independent outer closure

Build the FFmpeg-free `mondrian-validation-launcher` package together with the
validation `mondrian-endurance` executable. The approved machine plan's optional
`verifier_tools.preloader` is a `{ path, sha256 }` binding to that exact launcher.
A missing binding or missing authenticated native bootstrap leaves physical phase
admission `NotRun`; no environment flag grants the capability.

Prepare one launch-plan JSON with schema_version 1, launcher/application/request
`{ path, sha256 }` bindings, the same complete ordered runtime_files as the machine
plan, deadline_ms covering the entire campaign and cleanup, and a create-only
report_path. The application digest must equal the run identity's
runtime_image_sha256. Invoke it through
`scripts/validation/invoke-native-endurance-launcher.ps1` with LauncherPath,
ExpectedLauncherSha256, LaunchPlanPath and ExpectedLaunchPlanSha256. The native
launcher owns the fresh App and every native descendant until terminal cleanup.

After the run exits, pass PreloaderReportPath and ExpectedPreloaderReportSha256 to
`verify-commercial-endurance-qualification.ps1` in addition to its existing exact
approval parameters. The outer report hashes the completed child manifest and
contains the native bootstrap, object/mapping facts, Job descendant closure, and
exact namespace cleanup. Its digest must be independently approved as evidence;
it is not an input environment toggle or a replacement for capture authority.
See `docs/architecture/native-validation-launcher.md` for the precise trusted
entrypoint boundary and failure semantics.


## One caption/ANC program for physical output and AS-11 exports

Use `tests/validation/commercial-endurance-qualification-as11-5994.json` and its
three `*-as11-5994-v1.json` workload files for the implemented AS-11 X9
720p60000/1001 row. These are a distinct, externally approvable profile; they do
not alter the previously approved 60/1 workload. Their completed-frame floor is
5,178,821 per playback/reference 24-hour phase. Obtain fresh authority/profile/
workload hashes before launch; a local software test cannot approve them.

Add one top-level machine-plan binding, with actual canonical absolute path and
lowercase SHA-256, for example:

```json
"ancillary_program": {
  "path": "E:/qualification/endurance/captions.frozen-ancillary.json",
  "sha256": "<SHA256 of those exact FrozenAncillaryProgram bytes>"
}
```

Use the ordinary Export panel's validated SCC/CDP/canonical ANC import to obtain
the canonical attachment semantics. The plan file is the strict serialized
`FrozenAncillaryProgram`, not the original SCC text or an alternate cue model.
Its exact source start, cadence and frame count must match both phase Export
selections. Bind the same 60000/1001 physical signal and reserve a nonoverlapping
VANC marker placement; require independent physical ANC readback. Declare and
approve the real BMX runtime bundle for AS-11 through the machine-plan provider
contract. Missing program/provider/device/fixture yields pre-start `NotRun`.
Invalid hashes, unsupported packets or mismatched selections reject admission.

After replacing all approved input bindings, launch through the existing native
pre-loader; do not run the linked application directly:

```powershell
$profile = 'tests/validation/commercial-endurance-qualification-as11-5994.json'
$profileSha = (Get-FileHash -LiteralPath $profile -Algorithm SHA256).Hash.ToLowerInvariant()
$launcher = 'target/release/mondrian-validation-launcher.exe'
$launcherSha = (Get-FileHash -LiteralPath $launcher -Algorithm SHA256).Hash.ToLowerInvariant()
$launchPlan = 'E:/qualification/endurance/launch-plan.json'
$launchPlanSha = (Get-FileHash -LiteralPath $launchPlan -Algorithm SHA256).Hash.ToLowerInvariant()
& scripts/validation/invoke-native-endurance-launcher.ps1 `
  -LauncherPath $launcher -ExpectedLauncherSha256 $launcherSha `
  -LaunchPlanPath $launchPlan -ExpectedLaunchPlanSha256 $launchPlanSha
```

The request inside the launch plan must bind the new profile and authority; the
variables above do not overwrite request JSON or grant approval. Use the full
independent verifier invocation above with the actual new profile/authority and
pre-loader receipt bindings. Preserve all `ancillary_export_artifacts` sidecars
and `wire_journals` at their actual owner-produced paths. The independent verifier
checks their hashes, Export event identities, MXF frame counts, phase identity
and the single program hash across started phases. A frame outside the selected
attachment is intentionally empty on physical output; no replay/looping of cues
is inferred. No-device native bridge tests and software parser tests do not
qualify the physical SDI/Genlock/caption wire or 72-hour campaign.
