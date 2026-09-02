# Commercial Endurance Qualification

Commercial endurance is a qualification Module, not a longer benchmark. It
proves that one exact release candidate can continuously play, feed physical
reference output, export, recover, and return all owned work to quiescence
without unbounded resource growth.

## Ownership

The implementation is split at existing authority boundaries:

- `mondrian-platform-core::endurance_qualification` owns the versioned profile,
  strict evidence schema, bounded chunk chain, deterministic evaluator,
  three-state verdict, and self-verifying report. It performs no filesystem or
  product execution.
- `mondrian-platform` supplies complete native `ProductProcessTree` memory
  observations. Windows Private Commit, Linux anonymous resident memory, and
  macOS physical footprint remain non-interchangeable.
- Playback owns `PlaybackEvidenceReport`; Reference Output owns scheduled
  playout/accounting/hardware-clock evidence; Export owns queue/job/publication
  evidence and explicit worker retirement.
- `mondrian-app::app::endurance_qualification` is a validation-only composition
  Adapter and serial evidence supervisor. It copies owner snapshots into the
  neutral sample contract, verifies checked-in workload bytes, buffers at most
  one profile-bounded chunk, bounds both chunk receipts and producer events,
  publishes chunks and the run manifest create-only with file fsync, and
  derives terminal software/Export closure from typed owner receipts and the
  final Reference Output accounting snapshot. It does not execute workloads or
  reinterpret gates. Reference Output now has the consuming Module/Session
  receipt boundary; a real DeckLink/AJA provider implementation and physical
  HITL evidence remain required before that receipt can qualify hardware.
- `mondrian-app::app::endurance_campaign` owns exact serial phase admission,
  monotonic cadence, native process-tree sampling, final-sample order, and
  shutdown-before-terminal capture. It consumes an `EnduranceCampaignRuntime`;
  the concrete runtime remains the authority for pumping real product work,
  coordinator-bounded owner snapshots, typed semantic events, and synchronous
  closure.
- `mondrian-app::app::endurance_workload` owns the bounded regular-file read,
  SHA-256/profile binding, strict phase-specific JSON schema, fixed policy and
  duration/counter validation, plus the exact pre-start capability inventory.
  The runtime receives only `PreparedEnduranceWorkload`, never a raw path.
- `mondrian-app::app::endurance_export` captures one ordinary immutable
  Timeline Export configuration and exclusively drives its fresh Queue through
  strictly serial create-only publication, independent verification, and exact
  terminal-history cleanup. Its explicit recovery substate can cancel exactly
  one running reversible attempt and prove a distinct verified retry without
  changing ordinary Continuous Export cancellation semantics. It does not
  recapture author state between jobs or reinterpret the Export executor.
- `mondrian-app::app_ui::window` is the sole real Surface/Device recovery owner.
  Its validation entrypoint waits for an actual Viewer external-texture batch
  to be presented, consumes the old generation under a bounded whole-queue and
  Adapter-retirement proof, creates a fresh wgpu Device and native Surface, and
  requires the same Timeline/color/display picture contract to be presented
  again before it can return recovery facts.
- the PowerShell verifier owns external trust anchors, link-free file closure,
  immutable-byte checks, a bounded replay process, and create-only output.

This direction prevents a validation harness from becoming a second Playback,
Reference Output, Export, or process-resource implementation.

The coordinator samples every phase at zero and at each profile cadence, pumps
to the exact minimum-duration boundary, requests synchronous product shutdown,
then takes the final sample. A missing physical prerequisite is admitted only
as `NotRun` before any sample; a started phase cannot later relabel itself as
`NotRun`. A failed `begin_phase`, pump, event validation, native probe, or owner
capture triggers one consuming `shutdown_phase` call. Even an `Ok` cleanup
receipt is rejected when workers, children, or Export jobs remain. If cleanup
itself fails, evidence retains both the primary and cleanup failures instead of
detaching phase-owned work. The same closure test is applied to the normal
shutdown path before semantic events, the final sample, or a later serial phase
can proceed.

Cross-domain capture is an explicitly bounded envelope, not a fictitious
global linearization point. The supervisor stamps the envelope immediately
before invoking the native memory probe, then collects each domain's internally
consistent projection before the completion stamp. The snapshot path does not
schedule, pump, or poll phase work; individual owner projections may still
refresh bounded diagnostic caches. Export's endurance projection is linearized
by the sole Queue mutex: lifecycle flags, cumulative counters, job gauges, and
activity count are copied while the same lock is held. Job/activity mutations
advance both activity and revision before releasing that lock;
diagnostics-only changes advance their revision in the same critical section.
Every runtime snapshot also carries private phase-kind provenance. The
supervisor rejects an Export-only zero-realtime projection in Playback or
Recovery, so a public snapshot constructor cannot erase required owners by
crossing a phase boundary.

Preview's consuming shutdown receipt inventories media decode, visual
execution, CPU fallback, lazily-started Basic Title, and Timeline render-cache
workers. It distinguishes never-started owners from joined termination, panic,
same-thread detachment, and workers previously transferred to the ordinary UI
asynchronous reaper; only an exact, panic-free, fully synchronous inventory
closure may set the campaign's Playback/Preview worker-return fact.

Audio closure is similarly owner-derived: `AudioPlayback` joins its PCM render
worker and asks the concrete output Adapter to join every device-lifecycle
worker started over its lifetime. Earlier unexpected exits remain visible in
cumulative termination and panic evidence instead of disappearing once their
handles have been consumed by normal polling. When the App terminally replaces
an unexpectedly stopped PCM render owner with `ExecutionUnavailable`, a
validation-only App-lifetime ledger first absorbs that owner's cumulative
render-substitution, generation-recovery, underrun-recovery, backend-loss, and
deactivation-failure counts. Headless endurance capture projects retired plus
current counts, preventing counter regression at the replacement boundary.

The closure consumes schema-3 Playback, schema-2 physical-output, and schema-5
Audio Source receipts. Playback additionally inventories its prebuilt renderer
retirement owner; completion channels retain only PCM or safe value errors, so
an opaque renderer error/destructor cannot migrate onto the coordinator or
campaign caller. Audio Source Session permits remain charged across active,
queued, partial-spawn, terminating, and EOF-finalization states until the child
and both pumps have retired. A resource-free closed cache placeholder replaces
the consumed App field, preventing terminal capture from accidentally starting
a fresh decoder worker.

The App-owned product Waveform service now has its own schema-1 consuming-style
receipt over the analysis worker, request/publication backlog, external cache
references, and nested schema-5 Audio Source closure; its Timeline adapter is
weak and cannot prolong those owners. Normal `AppUiHost` quit consumes that
receipt under a fixed deadline. The concrete three-phase campaign runtime must
instantiate and include the same receipt in its shutdown window before COL-047
can claim complete product-domain closure; the current `AppState`-only owner
group does not fabricate a Waveform owner or infer closure from zero demand.

The validation-only `EnduranceExecutionOwners` group composes the production
Headless Preview and Viewer GPU owners with the exact Audio owner embedded in
the phase's `AppState`. It never starts a sidecar Audio instance. Its consuming
close takes the complete `AppState`, so the Playback binding or an App-owned
auxiliary worker cannot survive a nominally clean terminal projection; a
transport pause failure is latched as fatal evidence while cleanup continues.
The App first closes admission/cancellation seams, then consumes Project and
persistence ownership, Reference Output, Export, Audio playback, the shared
Audio Source Cache (including persistent FFmpeg children and stdout/stderr pump
threads), paused-Audio warmup, Media Import, ordered asset mutation, visual
tracking, Proxy generation, and the lazy native-memory observer. Every join
spends from one App-wide absolute deadline rather than receiving a fresh timeout
after an earlier owner used the budget. The final receipt records configured and
started workers, normal returns, panics, timeouts/detaches, residual queue/work
and resource ownership, cumulative failures, Project Session/library/runtime
lease release, and whether the sole `AppState` owner itself was consumed.
It synchronously retires the same PCM/device workers pumped by Playback before
transferring the complete GPU device-generation retirement envelope to the
existing progress worker. GPU closure is bounded by the same terminal budget
and records worker start/return, panic, timeout, retirement-handoff acceptance,
and exact resource retirement.
A device-loss or progress-failure terminal observed through the final bounded
join is merged into the terminal counters rather than being frozen only before
teardown. A timeout detaches the still-authoritative progress worker so that it can
finish safe retirement, but it is terminal campaign failure evidence: it never
claims that a GPU/native owner returned or that its admission slot was freed.

The software owner group starts one paired Headless realtime session without
admitting a phase or entering native playback scheduling. Inside that group,
the session installs the Preview completion waker and binds renderer-qualified
decode admission to the exact GPU device generation. A concrete realtime phase
runtime must open one fresh private coordinator/scheduling residency and close
it at the declared observation boundary. The shared coordinator is compiled
into validation builds and preserves A/V Audio-before-Clock or video-only
Clock/Preview/candidate/successor/lookahead/wait order used by performance
gates; the concrete campaign runtime must call it rather than copying the
former test harness loop. Consuming shutdown best-effort leaves any interrupted
residency before synchronously joining Preview, the App State's actual Audio
owner, and GPU retirement in that order.

The validation-only `PersistentTimelinePlaybackPhase` is the high-level
realtime phase owner over that sealed session. Startup requires an admitted
Playback/Reference or Concurrent/Recovery workload, a fresh stopped frame-zero
transport, one active exact 60/1 Sequence, and enough authored extent for every
required presentation plus one terminal guard frame. It freezes Sequence ID,
Sequence Revision, and Project Author Generation before starting ordinary App
Playback. Each accepted interval pumps the actual App Audio output before the
Clock through the shared coordinator, begins at the frozen expected coordinate,
advances exactly one frame without changing Playback Epoch, proves the departed
exact picture ready, and observes `AudioDevice` as Clock Master. Natural end,
skipped/non-unit progress, picture unavailability, clock fallback, external
transport or author drift, overflow, and owner failure permanently fault the
phase. A cadence observation first finishes the current realtime residency;
resume revalidates the same binding and coordinate before native scheduling is
entered again. Startup failure deliberately leaves `AppState` and the execution
owner group with the caller so their consuming terminal contract can still run.
This is production Timeline picture/audio-path evidence only: canonical
Reference Output is independently driven by the persistent canonical pump
described below and cannot use the monitor-adapted Viewer raster.

The validation-only `PersistentReferenceOutputPump` now supplies that exact
clean-feed producer. It freezes the active Sequence identity/revision, Project
Author Generation, exact-source `TimelineExportSnapshot`, and first physical
frame/cadence phase before opening a device. One persistent Export visual
materializer reuses decoder/Program/Effect/color/composite state and returns an
authored full-raster Float32 working composite; one independent public Audio
Program Runtime uses no audition or standard channel remapping. The pump
derives 48 kHz windows from rational frame boundaries, strictly enters then
continues one Audio generation, lowers picture and Audio through
`ReferenceOutputProgram`, and schedules only a complete A/V bundle. Author
drift or any picture, Audio, packing, provider, continuity, or counter failure
permanently faults the generation. Its close path cancels software work and
hands the ordinary provider stop to App; App-wide consuming shutdown remains
the terminal Reference/Adapter release authority.

This CPU path proves software semantics and simulated scheduling only. It does
not prove GPU/device residency, DeckLink/AJA bridge behavior, SDI wire output,
external lock, reference-monitor behavior, or long-duration performance.

Outside realtime residency, the paired Headless owner exposes one sealed,
fixed-size inventory instead of raw Preview/GPU access. The selected gauges
cover the current Playback binding, Preview scheduler and worker-queue work,
Frame Store aggregate residency and non-evictable Viewer pins, prepared visual
programs, GPU submission/current/prepared/staged owners, Audio render/buffer
ownership, and monotonic worker/device failure counters. The capture does not
claim to enumerate durable Timeline cache files. Clean consuming closure clears
gauges only after the Playback owner is consumed and Preview, the complete App,
and GPU all close; incomplete closure leaves nonzero ownership and fatal
evidence.

The App portion adds fixed-shape projections for six background execution
domains: paused-Audio warmup, Media Import, ordered asset mutation, visual
tracking, Proxy generation, and an Infrastructure aggregate over Project
persistence plus the native-memory observer. Each domain supplies exact transport/queue and
physically owned-resource gauges plus monotonic operation-failure and
worker-health counters; conversions and aggregation are checked rather than
wrapped or truncated. Terminal shutdown evidence is merged back into each
corresponding domain independently: terminal gauges replace live gauges, while
cumulative and worker-health failures take the per-domain maximum. This keeps a
failure discovered during shutdown and prevents one domain's terminal count
from masking or double-counting another domain. Persistence uses one shared
monotonic ledger for real worker completion/publication failures. The native
observer retains mutually exclusive queued/running ownership, supported-probe
failures, startup identity, and unexpected exits; unsupported platform probes
remain capability facts rather than fabricated operation failures.

Ordinary `Drop` for the App/Preview/cache/background worker owners covered by
this checkpoint is bounded best-effort cleanup. It may send cancellation or
shutdown and relinquish a still-running handle, so it is never accepted as
worker-return or resource-release evidence. Persistent FFmpeg decoder Sessions
now transfer eviction, random-restart, partial-construction, EOF-finalization,
and last-owner teardown to one prebuilt worker; no App/render caller performs
child wait/kill or pump join. This bounded ordinary path still is not terminal
proof. The explicit consuming Audio Source Cache coordinator remains the sole
deadline-qualified authority, and only consuming qualification paths plus typed
receipts can close a phase.

This checkpoint supplies the serial supervisor, sealed snapshot constructors,
owner-consuming cleanup, typed workload preparation/NotRun admission, the
phase-scoped frozen repeated-Export owner, the persistent production Timeline
picture/audio phase owner, the canonical persistent Reference clean-feed pump,
all four real recovery operation owners (including a narrow real-window Surface
reopen executable), and deterministic software tests. A
valid contract can become `NotRun` only when a
pre-start inventory names one or more missing Timeline/frozen-Export fixtures,
Audio Device, physical Reference provider, external lock, independent verifier,
or exact recovery driver. Bad bytes, wrong phase/kind, unknown fields/policies,
digest drift, duration drift, and counter-policy drift are execution errors,
not absent prerequisites. It does **not** yet supply the concrete three-phase
`EnduranceCampaignRuntime`: real vendor bridge/hardware validation, four-step
orchestration, and the unified high-level validation executable remain explicit
follow-on work. This App owner-closure work is a COL-047 prerequisite, not 72h
execution or hardware HITL evidence. Until that runtime and its physical
providers exist, physical phases must be admitted as `NotRun`; profile prose is not evidence that a runnable
72-hour producer exists.

## Commercial profile

`tests/validation/commercial-endurance-qualification.json` fixes three serial
24-hour phases:

1. playback plus physical reference output;
2. continuous, independently verified Export publication;
3. concurrent playback/reference/export with controlled recovery cycles.

The total is 72 wall-clock hours. This is intentionally distinct from the
Realtime Performance Matrix's 120-minute *Program-scale* authoring workload,
which describes Timeline extent and operation scale rather than a 120-minute
wall-clock soak.

Each profile phase binds one raw checked-in contract under
`tests/validation/endurance-workloads/`, the `mondrian-app` owner, the
`mondrian-app-endurance-capture-v1` supervisor, and producer report schema 1.
The reference-asset validator recomputes every workload file digest, so an
opaque or missing workload cannot be admitted by editing only the profile.
The campaign coordinator independently prepares those exact bytes before
calling the product runtime. Playback/Reference fixes 60/1, Audio Device Clock,
physical output, continuous external lock, every-completion hardware time, and
no Export/recovery; Continuous Export fixes disabled realtime owners, repeated
frozen Sequence export, independent full-content verification, and forbidden
cancellation; Concurrent Recovery fixes all realtime/Export policies plus the
ordered seek, surface/device reopen, Export cancel/retry, and cache-pressure
cycle and exact cycle/cancellation counts.

Each phase declares exact minimum duration, warmup, cadence, maximum sample and
per-domain Playback/Reference/Export/recovery progress gaps, native memory
backend/metric, absolute footprint, settled growth, integer least-squares
slope, domain counter budgets, queue bound, terminal quiescence, worker return,
and descendant-process reap. Missing hardware or execution is `Incomplete`;
an executed violation is `Failed`; only complete passing evidence is
`Qualified`.

## Samples and fixed-space capture

Every sample carries three monotonic instants:

- scheduled capture time;
- native probe start;
- complete snapshot publication.

The evaluator rejects missing/duplicate/out-of-order sequences, impossible
profile capacity, excessive
sample gaps or probe latency, counter regression, process-tree inventory
failure, backend/metric drift, arithmetic overflow, Reference Output imbalance,
and Export job/publication imbalance. It also requires per-sample physical
provider and external-lock facts, normalized hardware-clock gaps, and terminal
Export worker shutdown. UTC may be retained by raw producer reports for
audit, but it is never duration authority.

Samples are written in create-only bounded chunks. Each chunk hashes its exact
contents and the preceding chunk digest; the manifest fixes file name, index,
sample count, and first/last sequence. The checked-in commercial profile keeps
at most 120 samples per chunk, 48 chunks per phase, and 256 typed producer
events per phase. App memory therefore has an exact profile-derived ceiling
rather than retaining an unbounded sample, receipt, event, or channel tail.

The evaluator replays chunks one at a time. It retains only the previous
sample, cumulative maxima, first/last settled values, and checked integer
regression sums. No raw-sample vector, GPU timestamp vector, or unbounded event
tail is introduced.

## Domain closure

Reference Output now preserves explicit provider hardware-clock ticks plus tick
rate, adjacent maximum gap, regressions/rate changes, callback count, reference
lock-loss transitions, current outstanding depth, and aborted frames. Its
invariant is:

```text
scheduled = completed + late + dropped + flushed + aborted + outstanding
```

Stop, block, and post-consume ANC/hardware-time failure classify every remaining
frame instead of silently clearing it.

The terminal contract now consumes the Reference Output Module and provider
Session and independently records playback stop, callback-execution
termination, device/profile ownership release, unresolved resources, accounting
closure, and provider/module failure. `Stopped` plus zero outstanding frames is
still insufficient. The simulated provider validates this fail-closed contract;
a real DeckLink/AJA bridge must produce the same receipt before physical closure
can be qualified.

Export publishes a constant-size `ExportEnduranceSnapshot` with cumulative
admissions, failures, cancellations, rendered frames, durable artifacts,
activity events, active gauges, worker lifecycle, and job-scoped decoded-audio
owner started/closed/failure/active facts. The App treats a dirty owner closure
as a fatal-error increment and a live owner as queue depth. The App signals every
owner first, then passes its unchanged absolute monotonic deadline to
`RenderQueue::shutdown_until`. The Queue cancels reversible work, terminalizes
pending jobs, consumes its retained worker handle, and returns schema-4 facts
for normal join, outer panic, deadline timeout, handle detach, and retained-owner
abandonment after spawn failure or an opaque panic payload with an unsafe
destructor. A worker joined after its completion deadline is terminated but
still timed out and therefore never clean.
Started/closed audio-owner equality, zero dirty closures, and zero active audio
owners are additional terminal conditions; an empty job queue alone is not
closure evidence.
Lifecycle flags, counters, activity, and gauges come from one Queue-lock
critical section rather than several independently timed observations.
Independent finished-artifact re-open/validation remains an App capture fact;
durable namespace publication alone is not relabeled as content verification.
The supervisor accepts only typed artifact receipts containing the artifact,
independent validator, and validator-report digests. Concurrent recovery must
record `seek -> surface_device_reopen -> export_cancel_retry -> cache_pressure`
for every complete cycle. Event order, time, count, and terminal counters close
twice: before App publication and again in the external PowerShell verifier.
Each event embeds a bounded canonical schema-3 operation receipt plus the
SHA-256 of its exact UTF-8 bytes. App publication reparses the receipt,
reserializes it to
reject non-canonical bytes, validates the step-specific before/after
relationships, and rejects operation-ID replay. The external PowerShell
verifier independently rehashes and reparses the embedded JSON and replays the
same exact property sets, relationships, cycle order, and replay rejection.

The production seek receipt is available only from the persistent Timeline
Playback owner. That owner exits native scheduling at a cadence boundary,
invokes the typed settled Timeline product action, proves a strictly newer
Playback Epoch and exact target coordinate, re-enters the same paired
Preview/GPU/Audio owners, closes the target's exact Ready delivery, verifies
Audio Device Clock and exactly one new accurate-seek latency observation, and
rechecks the frozen author binding before sealing the receipt. The sequence
binding digest and operation identity are derived inside the owner; callers do
not provide success booleans or hashes.

Cache-pressure receipts are likewise available only from the persistent
Timeline owner while the paired Headless session is outside native realtime
residency. The owner requires nonzero optional decoded-media bytes, applies the
real Manual `Critical` decision to both Preview and the idle Viewer GPU owner,
proves exactly one policy application and actual byte/entry/resource-unit
retirement, and unconditionally applies `Nominal` before evaluating the
Critical result. It then re-enters the same owners, proves the exact current
picture Ready under Audio Device Clock, returns to a settled boundary, and
requires unchanged GPU-device-loss, aggregate fatal, and Export-failure
counters. Decision digests bind the exact coordinator revision, pressure
source, trim, Frame Store budgets, and Viewer idle-release request.

The receipt schema and verifier deliberately recognize all four ordered steps.
All four now have production constructors that accept only opaque facts
returned by their real operation owners. Surface/device reopen can only enter
through the winit Window event loop. Process-local Surface and Device
generation identities are nonzero and monotonic; same-generation reuse is
rejected. The old-generation receipt embeds canonical JSON proving the progress
worker started and returned without panic/timeout, accepted the consuming
retirement handoff, completed Adapter retirement, and had no loss/failure
terminal. Clean retirement additionally requires one successful whole-queue
wgpu wait; Adapter-local readiness alone is insufficient. The reopened
contract embeds the exact active Sequence/frame, Program and monitor color
spaces, display/view and display-contract digest, executed GPU residency, and
an actual Surface-present observation. The owner seals the original presented
picture digest beside the reopened picture's canonical JSON and digest; they
must match exactly. Those canonical bytes and SHA-256 values are retained, so
the independent verifier can reject either a changed picture or a semantically
dirty nested receipt even when every outer digest is recomputed.

`mondrian-surface-reopen --self-test` provides a narrow local executable that
authors a Basic Title through the ordinary ProductAction path and exercises
this real window seam. CPU-upload or procedural content may carry overall
Viewer health `Degraded` while still proving Surface recovery; the operation
therefore accepts `Ready` or `Degraded` only when GPU working composition was
actually executed, an external texture batch was submitted, no external
texture failed, and the exact picture contract matches after reopen. This does
not claim native decoder-surface residency, physical Reference Output, final
72-hour campaign, or hardware qualification. The Window owner now returns the
same `AppState` after bounded Window Preview and Waveform closure. The narrow
runner consumes that state through the full App endurance shutdown and rejects
the otherwise valid Surface receipt if any UI or App owner remains detached;
the concrete campaign runtime can instead resume the same settled Timeline
owner after the Window operation.

Continuous Export now has a validation-only product owner instead of a loop in
the campaign harness. Start requires a fresh empty Queue, a supported
single-file media preset, an existing canonical output directory, and a bounded
link-free ASCII artifact prefix. The owner freezes the complete ordinary Export
configuration once and changes only the monotonically numbered output path for
later clones. Every output uses `CreateNew`; the Queue must contain exactly the
one phase-owned Job while it is active and no Job between attempts. Completion
is eligible for verification only when the Queue reports `Completed`,
`Published`, and exact path-matching `Durable` evidence. The owner then invokes
the independent full-decode verifier, emits its sealed campaign event, and
requires removal of exactly one terminal history record before another Job can
be admitted.

Closing the owner stops admission but allows the current attempt to publish and
verify, because the Continuous Export workload forbids cancellation. Job
failure/cancellation/disappearance, publication/path mismatch, Queue
contamination, verification failure, cleanup mismatch, or checked counter
overflow permanently faults the phase. The concrete runtime must hold the owner
only for the Export interval and drop it before consuming the complete
`AppState`; otherwise its extra Queue `Arc` is residual ownership, not clean
shutdown evidence.

Concurrent Recovery enters an explicit substate on that same frozen owner; no
ordinary poll can infer or request cancellation. The substate waits for the
exact owned Job to report `Running`, `executed`, and `Reversible`, snapshots the
cumulative Queue counters, and accepts only `ExportCancelOutcome::Requested`.
Pending work is not cancellation evidence and a race into `Committing` fails
closed. It then requires the same Job generation and route to terminate as
`Canceled` + `NotPublished`, with user-initiated terminal evidence, no artifact
publication evidence, exactly one new cancellation, no too-late increment, and
exactly one terminal-history removal. The retry must have a different Job ID,
strictly newer generation, different monotonically numbered `CreateNew` path,
and complete as exact durable publication. Only the independent full-stream
EOF verification receipt can close the retry and seal both the artifact event
and Export recovery receipt. Retry failure/cancellation, verifier failure,
identity drift, residual Queue ownership, or counter drift permanently faults
the owner. A close request received before cancellation authority is asserted
withdraws the recovery request and lets the original attempt finish; after an
accepted cancellation, close must finish the distinct retry rather than leave
the system degraded.

The independent Export receipt is produced only after a bounded regular-file
check, encoded-byte hash, typed container/stream probe, and a separate FFmpeg
decode of every advertised video/audio stream through EOF from an immutable
verifier-owned snapshot. Its report binds the Export job/artifact identity,
final
video-frame and duration progress, a combined decoded-stream SHA-256, the
stable verifier identity, and unchanged before/after artifact size and digest.
App copies event fields from the sealed receipt and rejects repeated artifact
identities within a phase; a runtime cannot promote
durable publication or an opening-frame probe into `ExportArtifactVerified`.
This integrity/decodability evidence remains distinct from source-pixel or
colorimetric reference comparison.

## Identity and replay

One run binds:

- canonical profile digest;
- clean source revision;
- release candidate, package, and actually executed runtime image;
- build provenance and machine report;
- one admitted COL-046 platform/driver/display cell;
- one externally hash-approved single-use capture-authority manifest;
- equal before/after environment identity;
- exact workload, normalized owner-report, and raw-evidence files for every phase.

`mondrian-endurance-replay` reads bounded regular files, replays the exact chunk
closure, requires a complete `Qualified` report, and creates a new report file.
The external verifier independently pins the replay binary, profile bytes, and
capture authority; checks its challenge and release/phase bindings; parses and
re-derives typed producer-event summaries; requires
the evidence directory to contain exactly every declared chunk, owner report,
and raw evidence file; rejects links/extra files/hash drift; re-enumerates the
directory after replay; and rehashes both inputs and output around inspection.
It also rejects reparse-point ancestors and applies explicit JSON size bounds.

## Qualification boundary

Fast synthetic tests prove schema, hashing, deterministic verdicts, accounting,
leak detection, chunk tamper rejection, frozen repeated-Export ordering and
fault latching, persistent Timeline rate/extent/guard-frame admission and exact
interval policy, synchronous Preview/Audio/GPU owner closure, bounded
GPU-timeout failure, and worker retirement. They do not
prove 8/24/72-hour stability, physical reference lock, DeckLink/AJA callback
cadence, monitor behavior, or a platform/driver campaign. Those facts remain
HITL and must be captured on the approved physical rig; `NotRun` can never be
promoted to success.

The current Windows development machine can close source, unit, integration,
and headless GPU software gates only. Release transfer must separately execute
the native `cfg` build/test matrix and endurance capture on physical macOS and
Linux hosts, preserving each platform's scheduler, filesystem identity,
process-tree memory metric, audio-device Adapter, decoder/encoder runtime, GPU
driver, and display behavior. The same transfer package must carry the real
HDR/P3 display, DeckLink/AJA SDI loopback, Genlock/reference monitor, ancillary
data, external Blender/DaVinci Resolve/Premiere reference-frame, and final
72-hour rig gates. A Windows success, simulator receipt, or `NotRun` result is
not transferable evidence for any of those cells.
