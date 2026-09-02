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
owner-consuming cleanup, and deterministic software tests. It does **not** yet
supply the concrete three-phase `EnduranceCampaignRuntime`: repeated frozen
Export plus independent verification, persistent Timeline clean-feed/audio
pumping, the real vendor Reference Output bridge and hardware validation, typed
recovery receipts, and the high-level validation executable remain explicit
follow-on work. This App owner-closure work is a COL-047 prerequisite, not 72h
execution or hardware HITL evidence. Until those owners exist, physical phases
must be admitted as `NotRun`; profile prose is not evidence that a runnable
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
The recovery event payload currently has no production constructor, and the
low-level hash recorders are crate-private: recovery cannot begin until each
operation issues a typed receipt whose before/after facts can be independently
recomputed. A caller-supplied SHA string or bare failure counter is not evidence.

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
leak detection, chunk tamper rejection, synchronous Preview/Audio/GPU owner
closure, bounded GPU-timeout failure, and worker retirement. They do not
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
