# Playback Engine

## Purpose

The Playback Engine owns realtime transport semantics. It converts commands,
monotonic time, clock observations, audio health, and Frame Deliveries into one
authoritative Playback Snapshot plus bounded work directives. UI, Viewer,
decode, renderer, and audio adapters execute directives; none of them may
independently advance, pause, buffer, or end transport.

This is the production boundary. `AppState`, application playback modules, and
Window/Headless UI code adapt commands, observations, media execution, and
presentation around the shared Engine and Preview Production Runtime; none
retains independent transport authority.

## Goals

- Audio-master realtime playback with deadline-based video dropping.
- Synthetic monotonic time when an audio device is unavailable.
- Short, bounded priming after play, seek, device recovery, and disruptive
  quality changes; no per-frame stop/start clock behavior.
- Latest-epoch cancellation and stale-result rejection.
- Explicit degraded and blocked outcomes; no silent proxy or color changes.
- Headless deterministic tests using fake clocks and executors.
- Stable reservations for rational rate and reverse direction without claiming
  those modes are implemented.

## Excluded capabilities

- Reverse decode, J/K/L, variable-rate audio, optical flow, or frame blending.
- Automatic proxy selection or persistent project-setting mutation.
- Cross-process playback workers.
- A public cross-platform decoder trait before two production adapters require
  a real seam.

## Ownership

| Module | Owns | Must not own |
| --- | --- | --- |
| Playback Engine | session epoch, transport state, timeline anchor, rate/direction, Clock Master selection, priming/recovery, deadlines, drop decisions | decode sessions, GPU resources, audio callback buffers, UI state |
| Audio Playback Engine | device/stream lifecycle, rendered PCM queue, consumed-sample observation, preroll, underrun/device evidence | timeline transport decisions |
| Frame Work Broker | bounded semantic admission, queued transport, worker-lane-bound execution leases, latest generation, current/prefetch priority, still preemption, lowered deadlines, first-close timestamp, completion stamps, cancellation, freshness, and class/priority/lane evidence | codec payload interpretation, Clock Master or transport state |
| Monotonic Runtime Clock | process-local lifecycle-age, expiration, and evidence timestamp sampling | Playback Session advancement, authored/media time, wall-clock identity |
| Frame Cancellation Evidence | exact all-run cause/timing aggregates and the shared cancellation acceptance policy | cancellation authority, codec checkpoints, UI presentation |
| Preview Frame Store | physical decoded-media allocation ledger, optional decoded/final-raster residency, exact-current demand protection, current/stale Viewer pin, count/byte/native-resource grants, failure memory | request priority, pending/in-flight scheduling, deadline policy, proxy selection, media/color interpretation, Widget payloads |
| Playback Preview Pump | one pending-demand sample, ordered completion/expiration delivery application, exact-current-demand video-preroll observation, Window/Headless-neutral pump outcome | decode/render implementation, Widget refresh, GPU resources |
| Preview Execution Coordinator | complete generation binding, pending state, executed presentation quality, candidate identity, exact registered output | timeline interpretation, codec payloads, GPU resources, Widget state |
| Preview Output Unavailability | `NoContent`/`Blocked`/`Failed` disposition, owning production stage, stable code, bounded aggregate evidence | scheduler policy, renderer error details, localized UI wording |
| Presentation Adapter | output registration, Viewer handoff, exact presentation-ticket completion, presentation evidence | timeline advancement, GPU color/composite interpretation, scheduler state |
| Playback Evidence | immutable events, aggregates, reports | policy decisions |

`AppState` owns the Playback Engine as project runtime state. It exposes a
snapshot to UI models but does not mirror individual playback booleans.

### Crate placement

The pure state machine lives in the `mondrian-playback` engine crate. It
may depend on `mondrian-core` time/ID/value types, but not on winit, widgets,
wgpu, FFmpeg, CPAL, platform code, `AssetLibrary`, or concrete `Sequence`
internals. App code lowers the active sequence into an immutable playback
timeline descriptor and supplies observations through the Interface.

Media/audio/render implementations remain in their owner crates and enter as
app-level adapters. This direction prevents `mondrian-media` from becoming a
second editor-state owner and prevents the pure Engine from accumulating codec
or UI conditionals. Its public Interface remains limited to transport commands,
observations, snapshots, Frame Demands/Deliveries, presentation tickets,
semantic scheduling classes, and bounded scheduling evidence. Concrete media
keys remain opaque generic Adapter values.

## Runtime flow

```text
TransportCommand ─┐
MonotonicTick ─────┼──> Playback Engine ──> FrameDemand ──> Frame Work Broker
AudioObservation ──┤          │                                  │
FrameDelivery ─────┘          ├──> AudioDirective ──> Audio Engine
                              ├──> PlaybackSnapshot ──> UI
                              └──> PlaybackEvent ──> Evidence

Frame Work Broker ──> media decode ──> render/presentation ──> FrameDelivery
Audio Engine ──> device callback / synthetic availability ──> AudioObservation
```

Each Audio Playback render generation is carried across the PCM Adapter Seam.
Exactly its first admitted window is `Enter(generation)`; every later window is
`Continue(generation)`. Queue admission flips this state only after the Enter
work item is accepted, and reprime always installs a fresh pending Enter. The
Timeline PCM Adapter maps the generation to the audio Runtime continuity epoch
and validates exact next-sample progression. A changed coordinate can never be
treated as an implicit seek/reset.

The same request carries one device-facing semantic `AudioChannelLayout`. The
Timeline PCM Adapter prepares the root Program in the active Sequence's authored
layout, then applies a separate prepared delivery matrix into the requested
device layout. Each decoded source retains its own native layout and crosses its
Component matrix before Sequence processing. Every boundary validates complete
layout identity rather than channel count; an unsupported source/Sequence or
Sequence/device pair invalidates preparation instead of invoking a platform
default downmix.

The Runtime propagates that discontinuity into independent nested-instance
state domains. Child entry is lazy because the parent time map, not the root
sample coordinate, determines the first child sample. Nondecreasing child
demands replay skipped history exactly; generic stateful reverse mapping is
rejected before playback until an explicit reverse/checkpoint/materialization
capability exists. Playback never guesses child coordinates or resets child
state on an ordinary cache miss.

The PCM Adapter also declares whether windows are independent or mutate
generation-owned history. Independent-window failures preserve media duration
with exact silence and evidence. A stateful failure, including an invalid PCM
contract after execution, poisons all later work: Audio Playback cancels and
clears the old generation, captures its final output evidence, begins recovery
preroll at the authoritative Playback position, and admits only a fresh
`Enter`. The application submits the captured final device observation before
using Synthetic Clock Master during recovery. Repeated poisoned generations
consume a fixed recovery budget; exhaustion becomes `RenderBlocked` with no
further scheduling. Explicit reprime/source binding/device open starts a new
attempt cycle, while a generation that completes preroll clears the streak.

The main event thread calls the Playback Engine. Decode/render/audio workers do
not mutate it; they return observations tagged with the session epoch.

## Core model

Concrete Rust names may evolve, but the semantic Interface is fixed by this
document.

### Playback Session

A session contains:

- monotonically increasing `epoch`;
- active `sequence_id` and immutable timeline revision/signature;
- exact grid-aligned anchor in the active Sequence domain, currently stored as
  a validated `FramePosition` whose time base is fixed by the immutable
  Timeline binding and exactly projects to `TimelineTime`;
- signed rational rate and direction;
- Playback Quality Policy revision;
- Clock Master and clock-handoff state;
- Transport State;
- presentation sequence number and last accepted delivery;
- accumulated evidence counters.

Play after stop/end, seek, sequence/project switch, rate/direction change,
proxy/original change, or timeline semantic revision starts a new epoch. A
temporary resolution-scale change may remain in the same epoch but increments
the quality-policy revision, invalidating incompatible demands.

### Commands

- `Play`, `Pause`, `Stop`
- `Seek { position, interaction }`
- `Step { frames }`
- `SetLoopRange`, `ClearLoopRange`
- `SetRate { signed_rational }` (reserved; initially accepts only `1/1`)
- `SetQualityPolicy`
- `ProjectChanged` / `TimelineRevisionChanged`

Commands are synchronous state transitions. They may emit directives but must
not decode, render, block on a device, or wait for a worker.

An App Adapter applies Sequence identity, persisted monotonic
`SequenceRevision`, evaluation time
base, content end, and the requested position as one validated
`PlaybackTimelineBinding`. `play_timeline` and `seek_timeline` commit that
binding and the transport transition atomically. One user Play or Seek intent
rotates exactly one epoch: the Adapter must not compose a public timeline reset,
seek, and play sequence. A seek received during Priming, Playing, or Recovering
preserves play intent and publishes the new bounded Priming demand immediately;
a paused or stopped seek publishes one untimed demand. Pausing an active
transport also atomically supersedes its current deadline-bound demand with an
untimed demand for the authoritative pause position, so a delayed still-frame
presentation cannot be misclassified against the obsolete realtime deadline.
Binding and position time bases must match or the complete transition fails
without partial mutation.
The external Timeline Seek Product payload retains its complete
`FramePosition`; `app::playback` owns the sole Product-to-Playback Adapter. It
converts the input grid to exact nonnegative `TimelineTime`, lowers once onto
the bound Sequence grid, and only then calls the atomic timeline seek Interface.
UI, panels, scripting, and Headless callers do not pre-discard the input time
base or compose reset and seek transitions themselves.
Every public transport operation returns a typed result; dispatch through the
ordinary `Action` envelope and the closed `TimelineProductAction` algebra
preserves that result. Before publishing a new Engine snapshot, the App Adapter
must finish every fallible Sequence binding, duration, audio-program
preparation, and exact audio-anchor validation. The Engine itself evaluates a
candidate Session and installs it only after all checked phase, deadline,
Epoch, and demand-identity arithmetic succeeds. A rejected Play, Pause, Stop,
or Seek therefore cannot report success, rotate transport identity, change the
audio generation, or update UI transport state. Authoring commands that require
transport to stop perform that fallible stop before their author transaction;
runtime reconciliation after an already committed author transaction is
explicitly best-effort and may never relabel the author commit as failed.
The project document's successful-save revision is not accepted as this value:
draft edits, failed saves, Undo, and Redo must still rotate semantic identity.

### Observations

- monotonic tick with a nondecreasing timestamp;
- audio device state and consumed-sample position;
- audio buffer depth, underrun, and preroll readiness;
- terminal Frame Delivery;
- worker/capacity health required for recovery decisions.

Observations with an old epoch or policy revision are recorded as stale and
cannot alter the current session.

The Engine's accepted-observation timestamp is the sole monotonic high-water
mark for Playback execution. App process-monotonic projection, audio snapshots,
presentation completion, preroll, and Evidence all read or advance that one
value; Evidence is never a secondary clock authority. An Adapter must validate
epoch, generation, or demand identity before the Engine accepts its timestamp,
so rejected stale work cannot advance time. After an accepted observation the
App reanchors its one observation-instant-to-Playback projection when that
observation establishes a transport clock anchor. There is no second App
timestamp accumulator. A later event-loop tick maps its absolute instant from
the new anchor, so time before an intervening Viewer, audio, or preroll
observation cannot be counted again.

## Transport state machine

```text
Stopped ──Play──> Priming ──minimum readiness──> Playing ──Pause──> Paused
   ▲                │                              │  │              │
   │                ├──fatal blocker──> Blocked    │  └──end──> Ended
   │                └──cancel/Stop────> Stopped    │
   │                                               ├──pressure──> Recovering
   └──────────────────────Stop─────────────────────┴───────────────┘

Paused/Playing/Recovering/Ended ──Seek──> Priming
Blocked ──policy/source/device correction──> Priming
```

State definitions:

- `Stopped`: no running clock; transport position follows stop policy.
- `Paused`: stable position; exact still-frame work is allowed.
- `Priming`: clock is held at an anchor while minimum audio/video readiness is
  acquired within a bounded budget.
- `Playing`: Clock Master advances continuously; late video is dropped.
- `Recovering`: clock continues; speculative work is suppressed and temporary
  preview-resolution reduction may be applied.
- `Ended`: final content frame is stable; the next Play follows restart policy.
- `Blocked`: correctness or required capability prevents playback. It is not a
  synonym for slow decode.

UI may display “缓冲” for Priming, but internal code must not reuse one
`buffering: bool` for Priming, Late, Recovering, and Blocked.

## Clock architecture

### Clock Master kinds

1. `AudioDeviceClockMaster`: authoritative device-consumed sample position,
   not the number of samples queued or rendered.
2. `SyntheticClockMaster`: monotonic elapsed time anchored to exact timeline
   time and rational rate.

Displayed video frames are never a Clock Master. Presentation can be late,
dropped, occluded, or refresh-rate limited and therefore cannot define media
time.

### Audio clock observation quality

CPAL callback activity alone is not automatically a hardware playback-head
measurement. The current portable Adapter therefore publishes only
`CallbackConsumptionEstimate`: cumulative callback-consumed frames corrected
by CPAL's predicted output-playback delay, with a bounded recorded uncertainty.
CPAL backends derive that prediction differently (for example endpoint padding
and stream latency on WASAPI, host time plus configured latency on CoreAudio,
or queue delay on ALSA/PipeWire), so Mondrian must not relabel the portable
result as an exact device position. A future native Adapter may add a stronger
grade only when it supplies a stream-correlated hardware position and a
backend-specific uncertainty contract.

Availability is independent of observation grade:

- `Running`: the observation is fresh, plausible, and inside its uncertainty
  policy;
- `Uncertain`: a previously healthy active stream has temporarily stopped
  producing fresh callback evidence, but has not reported loss or failure;
- `Unavailable`: no sufficiently monotonic or stream-correlated observation.

Every usable observation must carry stream generation, sample rate, an exact
`AudioSamplePosition` media anchor on that same rate, integer consumed frames,
observation timestamp, latency estimate, monotonicity status, and uncertainty.
Rate/anchor mismatch, a negative realtime anchor, or unrepresentable position
fails before Engine state changes. Reports preserve the grade. A callback
estimate may drive Alpha playback only when its uncertainty remains inside the
configured A/V budget; it
must never be reported as an exact hardware position. `Uncertain` preserves an
already-active Audio Device Clock Master for at most the versioned one-second
grace interval. The consumed-frame fact remains unchanged while the phase is
projected at nominal rate and its conservative uncertainty grows by callback
age. Fresh qualified evidence resumes the same interval; expiry selects
Synthetic Clock Master continuously.
`Unavailable` selects Synthetic Clock Master immediately because explicit
device loss/failure is not uncertainty.

Callback consumption is also bounded against monotonic time since the current
output activation. A backend or APO report that advances farther than elapsed
activation time plus the stale-callback and one-buffer tolerance is rejected as
`Unavailable`; it cannot jump timeline position and the Engine remains on the
Synthetic Clock Master until plausible device evidence returns.
Once Audio Device Master is active, its media anchor is immutable for that
consumption interval. A changed anchor or decreasing consumed-sample position
hands off continuously to Synthetic Master before the new observation can move
the timeline. A reprime must therefore pass the normal phase-aligned handoff
gate instead of reusing callback consumption accumulated against an old anchor.

### Master selection

- A healthy audio stream with reliable consumed-sample evidence selects Audio
  Device Clock Master.
- A selected Program Output whose compiler-owned
  `AudioProgramExecutionDemand` is `ProvenSilent` uses Synthetic Clock Master
  directly rather than opening a meaningless silent device stream. Track
  presence or mute is never substituted for that evidence because a pre-mute
  send, Bus processor, tail, or generator may still require execution.
- Device open failure, loss, or an invalid sample position switches to
  Synthetic Clock Master immediately and without discontinuity. A merely stale
  callback first enters bounded `Uncertain`; only sustained uncertainty switches
  to Synthetic.
- An ordinary audio underrun does not instantly replace the master. It first
  enters audio recovery; repeated or unbounded invalidity triggers handoff.

### Handoff invariants

At handoff time `t`, old and new masters are mapped to the same timeline anchor.
The published timeline position must never move backward during forward `1x`
playback. Handoff emits old/new source, phase error, reason, and duration.

Synthetic → Audio requires:

1. device identity/config stable for a configured interval;
2. output stream running;
3. minimum PCM preroll queued;
4. at least one reliable consumed-sample observation;
5. absolute point phase error plus its uncertainty upper bound within the hard
   handoff budget.

Phase qualification compares the audio candidate with the continuous
Synthetic Clock position at the observation timestamp, not with the published
integer video-frame start. The latter is quantized by as much as one frame
(40 ms at 25 fps) and can falsely reject a correctly aligned audio stream when
the handoff budget is smaller. Published video position remains frame-based;
only the cross-clock qualification reference preserves subframe monotonic time.

Handoff preserves monotonic Timeline Time and records signed point error,
uncertainty, and their conservative sum. Sample-duration upper bounds use ceil
or an exact checked rational comparison; floor conversion may not admit a value
one nanosecond beyond policy.
An accepted error may remain within the versioned hard budget until a
separately specified bounded correction Adapter exists. Error outside that
budget keeps Synthetic Master active and reprimes audio. No path may jump
Timeline Time, duplicate or drop an arbitrary video interval, or claim
resampling/slew without an explicit ratio, duration, audibility, and evidence
contract.

### Event-loop clock boundary

The winit Adapter samples one absolute process-monotonic `Instant` on every
`AboutToWait` and forwards only that value. Window code owns no elapsed
accumulator, running-at-both-ends test, or Playback timestamp. `AppState` maps
the instant through its one checked observation projection, takes the maximum
with the Playback Engine's accepted high-water, ticks the Engine, and reanchors
the projection to the same instant and resulting high-water. Mapping fails
closed on a regressing instant or overflow. Saturating timestamp addition is
reserved for diagnostics or explicitly saturating budget aggregation; it
cannot create transport, deadline, or presentation authority.

Play, Pause, Stop, and Seek sample and reanchor this projection at their command
instant. Play/Resume therefore excludes preceding idle time; Pause/Stop/Seek
first account for the final running interval without relying on a later UI
tick. Audio-device observations remain authoritative after handoff, while the
same projection keeps the Synthetic Clock Master continuous during device
absence, preroll, and recovery.

### Time representation

- Authoritative Timeline anchors use ADR-0004 exact Timeline Time in the active
  Sequence domain. Frame and sample positions are explicit evaluation adapters.
- Monotonic duration is runtime-only and never persisted.
- Video scheduling resolves a Frame Position/evaluation instant; audio
  scheduling resolves an integer sample position carrying its sample rate.
- Realtime audio prepare/reprime asks the Engine to lower the authoritative
  phase at one explicit Playback Epoch and monotonic timestamp directly to an
  `AudioSamplePosition` using nearest-sample rounding. It never round-trips
  through the published integer video frame. While Audio Device is master, the
  requested sample rate must equal its active anchor rate.
- Conversion uses checked rational arithmetic at seams. Floating point may be
  used for filters/metrics but is not the authoritative accumulated position.

## Frame Demand

Each demand includes:

- session epoch and quality-policy revision;
- sequence/timeline revision;
- exact target Timeline Time plus the resolved video Frame Position and
  evaluation-grid contract;
- access intent (`PlaybackCursor`, `ScrubCursor`, `StillFrame`);
- optional presentation deadline and demand sequence number; paused seek
  demands are untimed and remain useful until superseded;
- requested output extent/temporary resolution scale;
- proxy/original selection fixed by user/project policy;
- source/color/display contract revision;
- stale-frame permission;
- cancellation token scoped to epoch/demand.

The Engine emits demands based on clock position plus a bounded lookahead.
Priming owns its independent `priming_limit`. Once Running, the current Frame
Demand deadline is the earlier of (a) the target frame's next exact media-frame
boundary and (b) the last instant permitted by the product's conservative
video-presentation phase budget. The latter uses
`abs(clock_phase - target_pts) + clock_uncertainty`; between Audio Device
observations both extrapolated point phase and callback-age uncertainty grow,
while Synthetic Clock has zero uncertainty. A mid-frame or tardy host tick
therefore receives only the remaining proven budget, never `now + one frame`.
The App may cap a distant wake for responsiveness but must preserve zero and
sub-millisecond remaining durations exactly; rounding them up to a nominal UI
timer quantum would knowingly sleep past the Engine's phase deadline.
Reissuing the same target for a quality revision or Clock Master handoff may
conservatively retain an earlier deadline but can never renew it. The same
pending deadline participates in the Engine's next-wake result, so a scheduler
cannot sleep until the successor frame boundary after phase authority has
expired. The next-wake result also retains its typed reason (`PrimingDeadline`,
`PresentationDeadline`, `FrameBoundary`, or `AudioDevicePoll`) so a host may
select a suitable wait primitive without reconstructing Engine timing policy.

Paused and natural-end demands remain untimed. Prefetch is advisory,
playback-only, slack-only, and cannot displace visible current-frame work.

When either Synthetic or Audio Device Clock Master reaches natural end, the
Engine invalidates the prior realtime demand and publishes a new untimed demand
for the exact final frame before settling in `Ended`. It must never clear a
terminal marker while retaining the old demand identity: that would revive an
already presented frame and allow redundant timeout work to emit a second
terminal delivery.
An explicit `Pause` is also an output-settlement command. If the exact
Paused/Ended untimed demand already terminated without leaving a presentable
output, the Engine issues one fresh untimed demand for the same coordinate.
It reuses a still-pending exact demand and never creates an autonomous retry
loop; only a new command or another authoritative transport transition can
establish new presentation authority.

## Frame request scheduling

`mondrian-core::execution_work` defines the small cross-domain Interface used
to compare execution reports: `ExecutionPriority`, an opaque
`ExecutionDeadline<D>` lowered with one admission-time remaining budget, and
`ExecutionTerminalEvidence`. It deliberately contains no task enum, queue,
worker pool, retry rule, or resource budget. The Frame Work Broker remains the
deep realtime lifecycle Module described below; waveform analysis, thumbnail
execution, and proxy generation each independently own bounded admission,
domain scheduling, cancellation, retention, and terminal evidence; Export must
retain its offline resource policy as it adopts the same language. Sharing this
Interface never authorizes background Export to contend with realtime frame or
audio work.

`FrameWorkBroker<K, D, P>` is the Playback Module's codec- and UI-independent
request-lifecycle Interface. `K` is an opaque Adapter key, `D` an opaque
deadline value, and `P` an opaque execution payload; the Broker
interprets only:

- `FrameWorkClass::{Playback, Interactive, Still}`;
- `FrameWorkPriority::{Current, Prefetch}`;
- `FrameInFlightDeadlinePolicy::{Cancel, FinishForLocality}`;
- an optional in-flight cancellation budget measured from execution dequeue;
- `FrameWorkResourceScope::{Shared, Media(MediaWorkReservationIntent)}`;
- a monotonically increasing latest-wins generation;
- an optional opaque `FrameDemandIdentity` carried for terminal reporting;
- a strict nonzero pending-request budget.

The Module stores and returns `D` but never compares it directly. Admission
also carries the remaining duration sampled beside `D`; the Broker lowers that
duration once into its own Monotonic Runtime Clock. Every deadline still owns
queued expiry and completion-time `OnTime`/`Missed` classification. The typed
in-flight policy independently decides whether that same instant requests
cooperative cancellation from a lease that already started. Each pending entry owns one atomic
`FrameRequestBinding<D>` containing generation, priority, semantic class,
physical-resource scope, Frame Demand identity, and deadline. Resource scope is
not a cache-key substitute: it proves that a queued payload or in-flight lease
already owns the exact kind of physical reservation required by the binding.
The separate execution budget is carried by submit and payload-free rebind
contracts, lowered from the lease's original dequeue instant, and requests
cancellation regardless of presentation-deadline locality policy.

Only playback-class work may prefetch. Current work may evict prefetch; realtime
current work may additionally evict deterministic still work; still work cannot
evict realtime current work. Ordinary frame advance within one Playback Epoch
does not rotate generation: the frame belongs to the opaque request key, while
generation identifies a seek/restart or interpretation discontinuity. Starting a newer generation makes older work
ineligible, but late results are still classified explicitly as `CacheOnly` or
`Stale` rather than being allowed to mutate visible state. Cancellation and
expiration remove pending ownership synchronously. Ordinary cancellation
invalidates a matching execution lease; stalled-presentation expiration instead
may detach a same-generation `FinishForLocality` lease from publication
authority while retaining its decoder-locality obligation. An already executing
Adapter must still poll `execution_cancellation` cooperatively and resolve its
execution lease before any result handling.

Submission distinguishes `DroppedObsoleteGeneration` from
`DroppedBackpressure`. The former has no admitted owner and therefore cannot be
projected as pending work or wait for a completion that will never exist. The
latter means the bounded pending/queue window is occupied by admitted work; an
Adapter may defer only while that existing owner has a completion, cancellation,
or lifecycle retry edge. Obsolete-generation diagnostics are not counted as
capacity backpressure.

An Adapter that has not yet acquired a move-only payload calls the Broker's
payload-free `bind_existing` seam first. It may update only same-key,
same-resource-scope queued work while retaining the original payload, or rebind
a compatible in-flight lease. A missing or different scope returns
`NeedsPayload` without changing Broker state; only then may the Adapter acquire
physical capacity and call `submit`. This ordering prevents repeated UI
evaluation from charging the same current demand twice and prevents metadata
promotion from silently dropping a queued physical lease. If a newly acquired
payload loses a later ordinary submit race, move/drop semantics release or
replace that exact lease.

Physical decoded-media capacity preemption is deliberately two-step. A Current
reservation first cancels at most one queued Prefetch payload, whose move-only
work lease is released by that removal, and retries admission. If no queued
Prefetch exists, the Adapter may ask the Broker to mark at most one
not-yet-completed, not-already-preempted in-flight Prefetch for cooperative
preemption. That request never removes its pending/in-flight registry entry and
never releases its physical charge early. The worker observes the typed
cancellation through the persistent logical observer, returns or drops its
payload, publishes normal completion availability, and only then may one pending
current candidate retry. Any concrete codec checkpoint is recorded separately
by the media Adapter.
Repeated polls cannot repeatedly count or preempt the same lease, completed work
is left for the completion pump, and neither path busy-waits on capacity.

`Cancel` is the conservative default and remains mandatory for speculative,
interactive, Still, heterogeneous visual, shutdown, and superseded work.
`FinishForLocality` suppresses `DeadlineExpired` for an already-running lease
and permits stalled-demand expiry to detach that exact same-generation attempt
from presentation. Detachment is permanent: the expired demand is terminal, a
later same-key demand cannot adopt the attempt, and its resolution has no
binding and is at most `CacheOnly`. Close, generation advance, explicit key
cancellation, Prefetch/Still preemption, queue expiry, and exact late evidence
remain authoritative. The media Adapter selects it only for
`Current + PlaybackCursor`: crossing one frame's presentation deadline must
drop that presentation opportunity, not destroy the worker-owned demux/codec
Session needed by the next sequential frame.

Expiration evidence reports actual removed queued payloads separately from
retained in-flight locality attempts. App diagnostics count only the former as
queue cancellation. A detached, deadline-missed completion may preserve decoder
Session locality but cannot publish or enter the Preview Frame Store.

The Broker timestamps pending admission, execution-lease start, and invalidation under that
same lifecycle lock. `execution_cancellation_evidence` atomically classifies broker
closure, supersession, current-over-prefetch preemption,
realtime-over-still preemption, and deadline expiry when the lease policy is
`Cancel`, plus execution-budget expiry; it carries both the corresponding invalidation or
competing-request age and the lease age from the same clock sample. Starting a generation, canceling/expiring a binding,
changing semantic class, eviction, and completion-driven binding removal all
refresh this state. `ReusedInFlight` requires a real locked-registry attempt
with exact generation, demand identity, key, and class. A cross-binding request
queues a bounded fallback; a reusable old completion atomically consumes the
new binding and removes that fallback, while non-reusable completion or failure
leaves it self-progressing. Beginning a generation immediately prunes older
pending/queued fallback work but retains in-flight leases for cancellation or a
later explicitly queued rebound. A compatible same-key request that rebinds in-flight work
clears the temporary invalidation timestamp, so latest-wins reuse is not
mislabeled as cancellation. Adapters may translate the generic disposition and
monotonic age into domain-specific diagnostics, but cannot reconstruct policy
from separate freshness, competing-work, or timestamp queries. The Broker does
not own codec-specific reasons.

Cancellation acceptance measures two non-overlapping intervals from the same
execution: authority request to `LogicalCancellationObserved`, then that
logical observation to worker return. Product policy requires the first
interval to be at most 5 ms for every work class, the second to be at most
50 ms for Playback and Interactive work, and at most 500 ms for deterministic
Still work. The bounds are inclusive. Total execution lifetime remains
diagnostic because work completed before cancellation authority existed cannot
be charged as cancellation latency. A logical observation is scheduler
evidence only; the media Adapter must separately report a concrete codec/demux
checkpoint, and publication remains forbidden until the execution lease has
returned and resolved.

`wait_for_execution_terminal_state` waits under the same lifecycle lock and
Condvar for `Canceled`, `Completed`, `Missing`, or caller-bounded `Timeout`.
Submit/rebind, cancellation, completion, removal, and close publish through
that condition variable, so an execution observer cannot lose a wake between
checking the predicate and sleeping.

Failure has the same atomic terminal-authority rule as successful completion.
An old or preempted execution may publish a failure only when
`fail_execution` consumes its exact latest binding. A queued or already
dequeued fallback for the newer binding is preserved; the old failure produces
no terminal evidence and cannot overwrite generation-scoped failure memory.

Completion resolves against the latest binding in the same lock operation that
removes pending ownership. A reusable result for the same opaque key may adopt
a newer demand identity/deadline; this is required when a seek or refreshed
demand reuses an already decoding frame. A canceled execution is not reusable:
if its generation or demand identity is older, it returns Stale and leaves the
newer binding pending. This prevents the old worker-result identity from being
rejected after it has already consumed the new request.

The Broker owns the blocking queue and execution-lease registry, but not worker
threads, decode sessions, `Instant` to `MonotonicTimestamp` projection, or media access modes. The current App media
Adapter maps `PlaybackCursor`, `ScrubCursor`, and `RandomAccessStillFrame` onto
the three semantic classes and projects generic diagnostics into its report
schema. The Condvar queue, semantic lane selection, capacity reservation, and
deadline dequeue now live behind the playback-owned Interface without importing
FFmpeg types.
The selected worker lane is captured on the execution lease at dequeue. Broker
diagnostics derive in-flight priority, class, lane residency, and cross-lane
invariant evidence from the same locked registry. The App Adapter may rename
those fields for its report schema, but cannot reconstruct worker activity with
atomics or access-mode conditionals.
Every current work class normally retains semantic lane affinity. Playback runs on
Playback/Any, scrub on Interactive/NonPlayback/Any, and exact still work on
Still/NonPlayback/Any. This prevents one request stream from cold-opening a
decoder/device session on each idle worker and prevents deterministic still
decode from occupying the realtime playback lane. Parallelism remains
available through lanes whose declared acceptance spans the class rather than
through implicit cross-lane stealing. One bounded failover is explicit: when a
live Playback-lane execution already has authoritative cancellation evidence,
an idle NonPlayback lane may dequeue one `Current + Playback` replacement.
That authorized replacement outranks ordinary Interactive and Still current
backlog so sustained non-playback demand cannot starve playback recovery. It may
never take Playback Prefetch, never admits a second live cross-lane replacement,
and does not release the old execution's resource lease. The current App deliberately instantiates
only Playback plus shared NonPlayback workers (or one Any worker on a constrained
CPU); dedicated Interactive and Still lanes remain capabilities of the generic
Broker, not additional production decoder pools.

Worker-lane affinity is necessary but does not by itself bound native decoder
residency across a transport transition. The App Preview Runtime therefore owns
two mutually exclusive physical residency families: `PlaybackCursor` belongs
to Playback; `ScrubCursor` and `RandomAccessStillFrame` belong to Interactive.
When the family changes, the Runtime removes only Frame Store entries carrying
nonzero decoder resource units, publishes a worker-lifecycle revision, and
waits for the opposite worker family to destroy its own codec context before
admitting new-family decode. CPU media frames and independently usable final
Viewer outputs remain resident. The Broker owns only the revision and
lost-wakeup-safe Condvar interruption; it does not decide which workers retire
or interpret media access modes. A stale acknowledgement is revision-scoped and
cannot satisfy a later transition. The worker cannot acknowledge even the
current revision until completion, Frame Store, and renderer clones have
released every native-output lease from its context. This bounds production to
one active native decoder family while preserving thread-affine FFmpeg
destruction and bounded transition latency.

Admission across the pending-binding window and worker queue is transactional.
The Broker first computes one eviction that can satisfy every active capacity
constraint—preferring a queued candidate when the same key can open both
windows—and mutates no state until that plan is known to succeed. A rejected
submission therefore cannot consume an in-flight binding, alter an existing
class binding, or partially evict queued work. Realtime-current still
preemption follows the same plan/apply rule rather than acting as a separate
best-effort side effect.

Window and headless Adapters must use this same Interface. Deterministic tests
exercise 100 latest-wins seeks with bounded pending residency and prove equal
admission/completion semantics for both Adapter shapes. This is a structural
test seam, not evidence that licensed 4K Main10 or 30-minute reference-machine
gates have passed.

Heterogeneous Preview visual work uses an independent bounded
`FrameWorkBroker` instance rather than a UI task queue or an extension of media
decode ownership. Its opaque key is the complete semantic visual fingerprint;
generation, Playback epoch, demand identity, class, priority, and lowered
deadline remain explicit. A dedicated serial worker executes only the atomic
CPU-prefix batch. Success returns the move-only execution lease with the pixel
result instead of completing it, so the same Broker authority crosses App
polling, Viewer recording, queue submission, and actual GPU completion.
Renderer failure or panic consumes only the latest compatible terminal binding;
deadline expiration becomes `Late` for the exact current Playback demand;
supersession releases the old attempt without consuming a newer binding.
Window completion callbacks are correlated by `FrameExecutionId`, and Headless
retains the exact submitted owner until its callback is observed. Both
therefore resolve at most one terminal outcome through the same visual Broker
lifecycle.

## Frame Delivery

Terminal outcomes are:

- `Ready`: a correct current output became usable through a Presentation Adapter
  before its deadline. Decode/cache readiness alone is not sufficient.
- `Late`: correct result completed after its deadline.
- `StaleAvailable`: at the demand deadline, policy closed the demand while a
  previously presented frame could remain visible.
- `Degraded`: an explicitly allowed temporary resolution, HDR-to-SDR path, or
  adjacent-GOP approximate frame shown during active pointer scrubbing was
  executed and reported, or a correct CPU frame remained presentable after an
  explicitly requested hardware decode path did not actually engage.
- `Blocked`: correctness/capability policy forbids presentation.
- `Canceled`: superseded epoch, demand, or latest-wins request.
- `Failed`: execution error not classified as a policy blocker.

`FrameDeliveryCandidate` carries only exact demand identity and proposed
terminal kind before completion. Terminal `FrameDelivery` additionally binds
the one real `completed_at`; no Adapter may submit a delivery and timestamp as
separate arguments. Source time, decode/import/render/presentation path, source
fingerprint, color contract, reason, and stage durations remain in the owning
media/Viewer diagnostics and are correlated by demand and candidate identity;
they are not duplicated into the Engine command payload.

`FramePresentationTicket` is the Playback Module's opaque Interface for final
presentation. It binds the exact demand identity, authoritative optional
`MonotonicTimestamp` deadline, and an allowed quality of Ready or Degraded. CPU,
Window GPU, and headless GPU Adapters call `complete_at` with their actual
completion timestamp; only this Module classifies timed work as Ready/Degraded
versus Late. An untimed paused-seek ticket cannot become Late, but its exact
identity is still invalidated by the next demand. Adapters never compare or
reconstruct deadlines themselves.
The App presentation Adapter does compare the opaque ticket identity with the
Engine's currently pending demand before submitting completion. A GPU result
that finishes after supersession is silently retired: it is neither evidence
for the new demand nor a rejected terminal delivery. This identity guard does
not interpret the deadline or manufacture an outcome.

Engine application returns one non-cloneable `FrameDeliveryApplication`. Only
the Engine can construct it, and its private fields bind the submitted terminal
delivery, acceptance, post-application snapshot, exact accepted target, and
the optional `PlaybackClockPhaseObservation` sampled at the delivery's same
`completed_at`. A rejected stale application has no target or phase and cannot
advance Engine time. Presentation Adapters must not infer acceptance from
snapshot mutation or secondary Evidence counters: a paused seek can accept a
delivery without changing public transport state, and an allowed software or
quality fallback remains `Degraded`. Headless and Window consumers may treat
accepted `Ready` and allowed `Degraded` as presentable; `Late` remains
non-presentable for the current demand.

Output publication and ticket completion cross one UI-independent App
coordination seam. The Adapter must finish every fallible or potentially
blocking preparation first, then hand the seam a typed
`FramePresentationPublication` carrying only a bounded, infallible visibility
commit. The seam itself samples the monotonic runtime clock at that commit boundary, maps it
once to Playback time, and rechecks the complete ticket identity and deadline.
A `Late` ticket is consumed without running the commit; a stale ticket or
rejected preparation drops the receipt without promoting output. For an exact
`Ready`/`Degraded` ticket, terminal Playback acceptance and the prepared
visibility commit form one synchronous transaction with no fallible operation
between them and therefore require no best-effort rollback. Sampling before
preparation, publishing through a boolean callback, or registering output and
then trying to repair failed evidence are prohibited. A missing ticket is
publishable only when the Engine truly has no active demand. A terminal
delivery consumes presentation authority but does not make the still-active
demand disappear, so an unregistered no-ticket retry cannot overwrite Viewer
output after `Late`, `Failed`, `Canceled`, or `Blocked`. A missing or stale
ticket while an active demand exists is lost authority. The typed result is
`Presented`, `NoDemand`, `DroppedLate`, `OutputRejected`, or `LostAuthority`;
Window and Headless Adapters may not collapse it to a boolean. While transport
is running after a terminal non-presentable delivery, Preview may retain an
already published exact output but must return Loading for any unregistered
no-ticket retry until the Engine issues another demand.

A bounded Headless observation must close the exact sampled demand, not merely
wait for `Ready`. If preflight or completion consumes that demand as `Late`,
the gate records a non-ready terminal sample and applies its unchanged quality
threshold; it must not wait for an impossible no-ticket retry or relabel the
sample as ready. Initial/preroll and paused-still probes still require an exact
usable output. During startup priming, however, the Adapter must keep advancing
the authoritative Clock: expiry of the bounded priming hold permits frame
skips and a later exact-current output, rather than freezing the expired frame
zero demand forever.

Headless readiness uses one candidate rule in startup, realtime, paused-still,
and validation paths. Ordinary queue-ordered publication is usable when its
exact semantic binding and physical output binding both remain current; the
later GPU callback retires the move-only owner and supplies completion
evidence. Consuming the carried demand changes `Some(identity)` to `None` and
is covered only by the same satisfied epoch/quality/frame binding. It cannot
cover a new demand, Epoch, quality revision, coordinate, or missing physical
output.

Every presentable Preview result carries the ticket captured by the same
evaluation that produced it. This includes a new GPU output, a CPU raster, an
already-current registered output, and the semantic transparent canvas.
Window and Headless consumers finalize that carried ticket through the same App
coordination seam; they may not resample `pending_frame_demand` after the result
crosses an Adapter boundary. Window keeps one admitted visible-output state:
Loading, a superseded candidate, and a Late completion preserve that state only
as stale and cannot replace it. Transparent output follows the same
ready/stale lifecycle even though it has no texture payload.

Running Viewer execution additionally owns one capacity-one immediate-successor
slot. Successor work is ticketless and binds the exact Playback Epoch, quality
revision and frame separately from the complete pixel-output identity. This
separation is required because two adjacent coordinates may legitimately
resolve to identical pixels: evaluation of frame N is not proof that frame N+1
was evaluated, while a proved identical successor can safely alias the one
already-visible physical artifact. A different ordinary GPU result owns an
independent prepared physical lease; a blank Program owns an explicit
transparent successor. Neither slot changes the current Viewer, consumes a
future Frame Demand, extends a deadline, nor survives Preview-generation
rotation. Only an exact current-coordinate request promotes semantic and
physical ownership. Late cleanup is artifact/submission-scoped and cannot erase
or revive a newer prepared result.

`app::viewer_gpu_publication::ViewerGpuPublicationSlots` is the sole physical
ownership Module for this contract. It retains one current and one prepared
publication, each binding the semantic output key, process-local submission,
Adapter artifact, and move-only renderer lease. Window supplies an external
texture key and Headless supplies its validation output; neither Adapter owns a
second promotion rule. Exact promotion returns any replaced owner for ordered
Adapter cleanup, exact submission retirement cannot touch a replacement, and
device-generation retirement drains both slots. Accepted Transparent/CPU
output, spatial-presentation change, or semantic/physical disagreement also
drains both slots and clears their exact semantic artifacts before reevaluation;
there is no path that silently drops a prepared lease while leaving its
semantic successor advertised.

Playback acceptance keeps execution conservation and presentation coverage as
two independent invariants. Every completed new GPU execution has exactly one
physical disposition: `PublishedCurrent`, `PreparedSuccessor`, `Released`, or
`TerminalRejected`. A prepared successor is therefore classified execution,
but it is not current presentation evidence. Only exact promotion/alias
completion increments the separate unique `(Epoch, target frame)` presentation
coverage. Qualification fails on either an unclassified execution or
insufficient unique presentation, so successor preparation cannot be mistaken
for a displayed frame and cannot make a correct speculative execution look
leaked merely because it was not published immediately.

The already-visible alias is the sole exception to sampling a new visibility
timestamp: after the Clock crosses its exact frame boundary, Window or Headless
may complete the new ticket at that same boundary observation because the
correct physical pixels were continuously visible and the commit only
synchronizes semantic ownership. A distinct buffer, texture, transparent
transition, or any fallible work must use the ordinary real commit instant;
backdating it is prohibited. Heterogeneous Viewer execution is not silently
treated as prepared by the current ordinary-successor Adapter: it releases the
speculative attempt and executes through its existing current-demand completion
contract until a dedicated prepared heterogeneous ownership protocol exists.

Widget `Ready`, `Blocked`, and other payload-free lifecycle values are
post-admission projections only. They contain no demand identity and therefore
cannot mint, rebind, or consume a terminal delivery during later UI feedback
synchronization. A policy blocker may consume a demand only at a typed Preview
or execution seam that still carries the exact identity; otherwise it remains
diagnostic while the authoritative demand is left for exact completion,
expiration, or supersession.

Terminal authority ends as soon as the Engine accepts one delivery. Preview
Adapters may attach an identity or deadline only from `pending_frame_demand`,
not merely from the last active demand. Scheduler-current work can outlive that
authority when presentation wins a race with redundant decode work. Expiration
must still cancel and release all such work, but may publish at most one `Late`
and only when its identity equals the Engine's still-pending demand. This keeps
the one-terminal invariant at the Playback/Preview boundary instead of treating
an internal queue entry as permission to revive a completed demand.

The active `FrameDemand` is also the sole source of truth for its target.
Synthetic and Audio Device Clock transitions update position and refresh that
demand atomically. The Engine deliberately has no parallel `active_target`
field: such a duplicate once lagged Audio Device Clock updates and rejected an
otherwise exact current `Ready` delivery. Delivery validation now compares the
terminal fact directly with the authoritative demand. When independent bounded
completion and expiry sources report the same identity in one Preview Pump
turn, only the first still-authoritative fact is submitted; losing race facts
are retired before Evidence rather than mislabeled as rejected deliveries.

Only the Playback Engine interprets a delivery:

- During Priming, one current Ready/allowed Degraded frame plus required audio
  readiness may start the clock.
- During Playing, Late/Canceled video is dropped while clock time continues.
- StaleAvailable preserves UI continuity but never counts as current readiness.
- Blocked enters Blocked when no allowed path exists.
- Repeated Late/Failed/Degraded outcomes enter Recovering according to a sliding
  window, not a single-frame boolean. Degraded remains presentable and may
  satisfy Priming, but it is not healthy evidence for resolution restoration.

The Viewer's immediate `Stale` lifecycle state is not itself a Frame Delivery.
It describes the currently visible fallback while current work is still
eligible to complete. Converting that first stale refresh into a terminal
`StaleAvailable` would consume the one-terminal invariant and force a later
presentation arriving before the deadline to be rejected. Payload-free
`ViewerPlaybackFeedback` therefore emits only correctness `Blocked` as a
terminal outcome. A Ready lifecycle must be paired with an exact presentation
delivery carrying the original demand identity. Worker/deadline scheduling owns
Late, Canceled, Failed, and any future deadline-classified StaleAvailable
outcome.

An active Sequence whose current Program evaluation has no visible elements is
not unavailable: Preview publishes an exact texture-free `Transparent`
presentation. Window and Headless Adapters may complete the current Frame
Demand only with its normal Presentation Ticket. This prevents blank Timeline
regions from expiring demands or accumulating false late-pressure evidence.
`NoContent` remains the typed absence for intents that have no active
Project/Sequence output target; it is not a substitute for transparent Program
pixels.

## Quality and recovery policy

The Engine may automatically lower temporary preview resolution under sustained
pressure. The scale is runtime-only, monotonic within a recovery step, bounded
by a configured minimum, and restored only after a hysteresis window.

The App preview scheduler has a separate, deliberately smaller consecutive-late
decode-pressure guard. After two late current decode outcomes it suppresses
speculative prefetch and avoids submitting duplicate current work while a
realtime request is already queued or executing; the next successful
playback-current decode clears it. This value-state machine lives in
`app::preview_scheduler_policy`. It is an execution backpressure guard, not the
Playback Engine's 8-of-12 Transport recovery policy: it cannot enter
`Recovering`, change Clock Master, or lower runtime presentation scale.

`PreviewExecutionCoordinator` borrows are confined to one state transition and
must end before the Adapter invokes prefetch, Broker pruning, presentation, or
any other operation that can observe execution state. In particular, candidate
selection first materializes an owned `PreviewCandidateDecision`; branches act
on that value only after releasing the coordinator borrow. This keeps the
single-threaded state machine non-reentrant without replacing its explicit
ownership with a lock or duplicating pending/current policy in callers.

Hardware-path recovery signals are likewise pure scheduling policy. For a
playback-current completion, `app::preview_scheduler_policy` compares the
configured hardware request and native-import admission with frame-local decode
provenance, then returns typed `native_import_unavailable` and
`hardware_fallback_not_engaged` facts. The Preview Adapter may count and display
those facts but cannot infer fallback from a capability probe or report schema.

Scrub adaptation is part of the same UI-independent policy Module. Its Adapter
supplies an explicit monotonic observation instant for each request; the policy
classifies a hot source region from request/source locality and combines that
with completed decode latency or forward-decode-budget failure. The resulting
`Normal`, `HotRegion`, `Recovery`, or `SlowLatency` value is only a decoder
strategy hint. It cannot change source time, proxy/original selection, color
interpretation, authored quality, or the requirement that a settled scrub
returns the exact requested frame. Keeping both the thresholds and state out of
the Viewer preserves one deterministic test surface for Window and Headless
Adapters.

The Preview Adapter must execute that policy rather than merely report it. It
uses the single UI-independent `app::preview_quality` contract to normalize the
authored scale to `[0.125, 1.0]` (with a fail-safe `0.5` for non-finite input)
both when applying an author mutation and when consuming persisted state. UI
labels may format this value but cannot define a second clamp or fallback.
It then
multiplies the sequence's user-authored preview scale by the runtime
`Full`/`Half`/`Quarter` divisor before constructing decode, composite, nested
sequence, prefetch, and GPU-output keys. Integer dimensions round upward and
remain at least one pixel. Width/height are part of request identity, while the
quality revision rejects superseded work. The same rule is consumed by Window
and headless paths. Paused and stopped still-frame work uses the authored scale
rather than retaining a recovery reduction. This scale changes spatial work
only: proxy/original media, input interpretation, working/output transforms,
tone mapping, and effect semantics remain unchanged.

Hardware preference is classified from execution diagnostics, never capability
probing. Requested hardware that produces a correct CPU fallback is
`Degraded` and drives the same bounded recovery ladder. An observed hardware
frame transferred to CPU, or a native GPU-resident decoded frame, is `Ready`.
This executed quality is stored on `MediaPreviewFrame`, preserved through
prefetch and Preview Frame Store reuse, and aggregated across every media layer
before the final Frame Presentation Ticket is created. It is not attached only
to a scheduler job or demand identity: an already-running prefetch decode must
not lose its fallback evidence when it later satisfies a current demand.

The Engine must never silently:

- switch between original and proxy media;
- generate or select a proxy;
- change input color interpretation, working/output color space, tone mapping,
  bit depth, alpha semantics, or project settings;
- replace an unsupported effect with an approximate one.

When lower resolution cannot restore deadlines, the session continues dropping
video while audio/synthetic time remains authoritative, and presents a proxy or
hardware recommendation. A correctness blocker stops presentation explicitly.

### Initial policy budgets

These are versioned defaults to validate on the Windows reference machine, not
magic constants embedded in UI code:

| Policy | Initial value | Rule |
| --- | ---: | --- |
| audio preroll target | 120 ms | measured at output sample rate |
| minimum video priming | one presented current frame plus the ready prefix of the bounded future media window (maximum 16 frames) | stale does not satisfy current presentation; pure audio/procedural/end-of-sequence playback has no media lookahead requirement |
| normal play priming limit | 500 ms | then start the clock if no correctness blocker; late video may be absent/stale |
| seek priming limit | 750 ms | exact current frame remains highest priority |
| interactive control wake while priming | ≤100 ms | pause/seek/close remain responsive |
| recovery pressure entry | at least 8 late/failed current deliveries in the latest 12 | excludes canceled old epochs |
| resolution ladder | `1`, `1/2`, `1/4` | spatial scale only, working/color semantics unchanged |
| healthy recovery exit | 2 continuous seconds with ≥95% on-time current deliveries | ascend one ladder step at a time |
| audio callback uncertainty | at most 1 s for an already-active Audio Device Master | retain the consumed-frame fact, project phase at nominal rate while growing uncertainty by callback age, then hand off continuously to Synthetic; explicit device loss/failure hands off immediately |

Current-frame presentation and media lookahead are independent observations.
The Engine records the terminal current Frame Delivery exactly once, while the
Preview Adapter reports a bounded `VideoPrerollObservation` containing the
exact current `FrameDemandIdentity` plus ready and physically preservable
future media-frame counts. Neither
signal can release `Priming` alone. The required lookahead is
`min(policy.minimum_video_preroll_frames, preservable_media_frames)`, where the
second value is the complete immediate future media-bearing prefix proved able
to coexist within the Adapter's physical resource and work-admission grants.
Both observation interfaces carry the monotonic instant at which their fact
became available. The Engine validates the complete demand identity, including
Epoch, quality revision, demand sequence, and target, before accepting that
timestamp, so a late callback from a retired demand cannot move
the current Session's monotonic high-water mark. Whichever independent
observation satisfies the second priming condition supplies the Synthetic
Clock reanchor. Startup work therefore consumes no playback phase: for example,
if the second condition arrives at 17 ms on a 25 fps Sequence, the next frame
boundary is 57 ms, and fractional rates retain the same exact rational boundary
with nanosecond ceiling at the runtime seam.
Thus a frame
at the end of a sequence, a pure-audio sequence, or an immediate procedural
frame does not acquire an artificial delay. A consumed current demand remains
visible to Playback Evidence but cannot issue another Presentation Ticket.

If the priming limit expires with no correctness blocker, the session enters
Playing rather than freezing transport indefinitely. Audio Device Master starts
when audio is ready; otherwise Synthetic Master starts. The Viewer retains a
stale frame or explicit loading presentation until a current delivery arrives.
Blocked color, unsupported required format, or invalid timeline contracts do not
use this timeout escape.

### Production preview execution pump

`app::playback_preview` is the App Module's UI-independent coordinator between
the Playback Engine and the production Preview Adapter. One pump turn:

1. samples `pending_frame_demand` and current transport activity exactly once;
2. asks the Adapter for bounded completion and stalled-current expiration facts
   bound to that identity;
3. applies every exact terminal Frame Delivery through `AppState`;
4. only then captures a fresh borrowed `PreviewExecutionSnapshot`, derives the
   narrow `PreviewVideoPrerollRequest` carrying the Engine's exact active
   demand identity, samples its video lookahead, samples one wall instant after
   that readiness calculation has formed its fact, and submits one timestamped
   `VideoPrerollObservation`;
5. returns visible-change, transport-change, and follow-up-poll facts without
   performing Widget refresh or GPU presentation.

This order prevents completion and expiration from binding against different
demand samples. The preroll request cannot reuse the poll-time transport
snapshot: a terminal delivery may have stopped transport, rotated its Epoch,
or replaced its demand. A presentable terminal delivery during Priming is the
deliberate exception: it consumes the Presentation Ticket while the same active
demand remains the identity of the independent future-lookahead condition.
The fresh request therefore carries `frame_demand`, not
`pending_frame_demand`; the Engine rejects it unless that full identity is
still current. The frame-producing request similarly
captures position, Transport State, Epoch, runtime quality scale, seek source,
Frame Demand identity, and its once-lowered Adapter deadline as one immutable
turn. `AppUiHost` decides only how the pump
outcome affects layout/repaint. The real Headless GPU gate drives the same pump
and therefore cannot maintain a test-only Delivery or preroll policy. The
sampled transport activity also prevents an idle-residency release from using
a stale paused/playing value while a transport transition and result poll race.
The Preview Adapter still owns concrete decode execution, payload adaptation,
result diagnostics, and lookahead observation; it does not own Playback state
transitions or Viewer candidate lifecycle. `app::preview_execution` owns the
complete output key and GPU execution contract consumed by both Window and
Headless Adapters, and atomically coordinates generation binding, pending state,
executed quality, candidate IDs, exact registered-output reuse, and the bounded
transport-qualified successor slot. Cache
residency/eviction remains in the playback-owned `PreviewFrameStore`; GPU/color
mathematics remain renderer-owned. Renderer `PreparedVisualFrameClosure` is the
sole authority for nested time, lookup/cycle/depth, per-Sequence runtime sizing,
color context, Transition/temporal bindings, and instance paths. The
UI-independent `app::preview_timeline_execution` Module only materializes those
typed nodes into nested working-space output, typed media readiness, execution
facts, and mandatory ready-plan identity. The Window
`preview::timeline_evaluation` file is only an Adapter that supplies media
outcomes and records returned execution facts. Prefetch, preroll, and input-color
evidence consume the same Module's read-only media-demand collection instead of
walking nested plans in Window code. Media library/proxy side effects
live in `preview::media_adapter`; decoded/native payload ownership and the single lazy
CPU working-frame adaptation live in `app::preview_media_frame`; resolved Viewer
identity plus GPU execution-layer lowering live in `app::preview_viewer_plan`;
working-linear CPU composition plus Program Output and monitor-adapted raster
execution live in `app::preview_cpu_execution`. Final exact
GPU/raster/stale/CPU output arbitration remains
in `preview::presentation`. These are private deep Modules over the existing
request Interface, not new public seams; the parent coordinates them but no
longer owns their color execution implementations or payload conversion state.

Deterministic Headless fault sequences cover a seek that retires an in-flight
presentation, queued cancellation racing the replacement demand, delayed or
dropped GPU presentation, Audio Device Clock reacquisition, explicit device
loss, and continuous Synthetic fallback. Losing old-epoch facts are retired
before Evidence; only the current demand can consume terminal authority. A
real isolated-demux cancellation probe first establishes the settled output,
retires any queue-published GPU submission, observes Preview work quiescence,
and releases the preceding decoder-family residency. Otherwise the next native
decode may correctly wait for a still-owned output or session lease, and the
probe would be measuring resource backpressure instead of its declared
Seek/PacketRead cancellation seam.

All thresholds live in one Playback Policy value, appear in evidence, and may be
tuned by measured reference-machine data. Tests must pass an explicit policy;
environment variables cannot silently redefine product semantics.

## Frame selection and display cadence

Clock time is converted to the exact sequence frame using rational arithmetic
and the sequence's defined frame-boundary rule. When several frame boundaries
are crossed between event-loop ticks, the Engine demands the newest required
frame; Playback Evidence records a one-frame advance separately from a
multi-frame observation and counts every skipped intermediate target. The
Engine does not enqueue obsolete current-frame work merely to preserve a
one-request-per-frame count.

Display refresh cadence is an observation, not timeline time. A 24 fps sequence
on a 60 Hz display may present a frame more than once. A 60 fps sequence on a
slower display necessarily records presentation drops while the Clock Master
continues. VSync, occlusion, minimized windows, and monitor migration therefore
cannot change the playback position.

## Audio Playback Engine

The Audio Playback Engine owns device discovery/open/reopen, callback-safe PCM
consumption, render worker generation, buffer watermarks, preroll, underrun, and
consumed-sample evidence. Its Interface accepts directives and returns immutable
observations.

The app Clock/Audio Adapter maps Transport State to one typed permission:

- `Stopped`, `Paused`, `Ended`, and `Blocked` → `Idle`;
- `Priming` → `Preroll`, which fills PCM while keeping callback consumption
  inactive;
- `Playing` and video-pressure `Recovering` → `Consume`.

This mapping is intentionally not a `playing: bool`. Reaching the PCM preroll
watermark during slow first-frame or seek Priming cannot start audio before the
Engine releases the transport anchor. The later `Preroll` → `Consume`
transition activates the same render generation and queue; it does not reprime,
clear PCM, or create a phase correction caused by video readiness. Frame
Delivery pressure never changes this permission while transport remains
`Playing`/`Recovering`; only audio device/underrun evidence may initiate audio
recovery.

Playback prefetch normally keeps its 50 ms cooperative decode budget so
speculative work cannot monopolize the playback worker. During `Priming`, only
prefetch work carrying the active Frame Demand's absolute deadline may use the
remaining startup window, never more than the 500 ms session limit. Its decode
and queue timings are reported as startup-preroll evidence and excluded from
steady-state access-mode latency budgets; this preserves cold device/session
startup cost without misclassifying it as a continuous-playback regression.

The realtime callback may only read/write preallocated lock-free or proven
bounded structures and atomics. It must not allocate, log, decode, access the
timeline, lock a contended mutex, perform filesystem I/O, or publish general
events.

Rendered, queued, submitted, and consumed sample positions are distinct. Only
consumed sample position can drive Audio Device Clock Master. Queue depth and
callback counters remain evidence.

## Scheduling and backpressure

- One active Playback current-demand identity per session; the same demand may
  own every media key in a multi-layer frame, while a newer demand atomically
  removes all older unstarted `Current/Playback` payloads across keys. An older
  in-flight execution may finish for decoder locality, but it no longer owns
  presentation or failure-publication authority.
- Prefetch has a separately bounded budget and is discarded first.
- Worker completion polling has both count and wall-time budgets; its producer
  transport holds at most eight queued results plus one pending publisher per
  decode worker.
- Cancellation is cooperative, but publishing is guarded again by epoch and
  demand identity.
- Completion-transport backpressure remains interruptible by shutdown and
  decoder-residency revisions; a retired-family result is resolved
  non-reusable before its payload and worker-owned surface pool are released.
- Shutdown never joins a potentially stalled codec/device worker on the event
  thread.
- Cache/in-flight keys include access mode, source fingerprint, source time,
  dimensions, color contract, proxy policy, timeline revision, and relevant
  quality revision.

## Evidence and health

Playback Evidence is event-derived and versioned. Minimum events:

- session/state/epoch transition;
- Clock Master selection and handoff;
- single-frame Clock Master advancement and skipped intermediate frame targets;
- Frame Demand admitted/rejected/canceled;
- Frame Delivery terminal outcome;
- video deadline drop and stale presentation;
- quality recovery entry/step/exit;
- audio preroll, underrun, device loss/recovery;
- explicit degraded path or blocker.

Reports must include p50/p95/p99 queue/decode/render/delivery latency, dropped
and stale frames, consecutive pressure, effective preview scale, Clock Master
residency, uncertainty-inclusive handoff phase error, audio underruns,
per-master delivery phase error, cache budgets, and reason-code counts. Preview reports additionally include the fixed Playback and
NonPlayback worker execution-progress snapshots so an outstanding Broker lease
can be attributed to the exact media Adapter call without reading UI state or
parsing logs. A progress snapshot is diagnostic evidence only: quiescence still
requires the Broker lease to return and all native-output leases to retire.
Logging alone is not evidence.

`PlaybackEvidenceCollector` is the shared bounded Interface for production UI
and headless/perf Adapters. It consumes only Engine-created
`FrameDeliveryApplication`; callers cannot separately supply acceptance,
snapshot, target, or timestamp. Every fallible timestamp/phase calculation is
preflighted before collector mutation. Schema v4 retains at most 4,096 detailed
tail events and a deterministic 4,096-sample whole-run reservoir per
latency/phase-error metric.
Each metric separately keeps its exact population count and exact all-run
maximum; p50/p95/p99 are explicitly reservoir estimates. Clock Master and
Transport State residency, single/multi-frame advancement and skipped-target
counts, delivery counts, superseded demand/seek totals, and underrun totals are
streaming aggregates and do not depend on retained detail.
Detailed-event eviction is expected bounded retention, remains reported, and is
not reclassified as observation loss. A gate may use the exact maximum, a
declared reservoir percentile, or aggregate counters, but cannot infer failure
merely because old diagnostic events were intentionally evicted.

For an accepted `Ready` or `Degraded` delivery, the exact target PTS and active
Clock phase use the same checked floor-nanosecond grid. Evidence records
`point_error = abs(phase - target)` and
`proven_error = point_error + uncertainty`, partitioned into Audio Device and
Synthetic summaries. Audio uncertainty includes its callback estimate plus age
since the last reliable observation; Synthetic uncertainty is zero within its
model. Presentable phase evidence is explicitly tri-state. A `Playing` or
`Recovering` delivery that should have a running Clock phase but lacks one
increments `unproven_presentable`. A Paused, Stopped, Ended, or otherwise
non-running still/seek delivery without a phase increments
`phase_not_applicable`; it does not contaminate the running-interval gate.
Nanosecond thresholds are compared before report quantization, and microsecond
reports use ceil, so 20,000,001 ns is 20,001 us and cannot pass a 20 ms gate.
The real Windows 4K playback gate declares a 20 ms whole-run P95 contract for
both Synthetic and Audio Device delivery phase, alongside independent zero
missed-opportunity, zero skipped-frame, and zero unproven-delivery invariants.
It reports exact maxima but does not treat one non-real-time OS preemption as
sustained cadence failure. Deterministic accelerated clock tests and the
Audio-Master handoff contract retain their stricter maximum-error assertions.

The accelerated headless continuity gate drives the public Engine and Evidence
Interfaces for 30 minutes of exact 48 kHz callback time against a 30000/1001
video grid. It completes current Frame Demands, injects one Audio Device to
Synthetic to Audio Device transition, and requires non-empty Audio Device phase
evidence, zero unproven or not-applicable delivery inside that continuous
interval, proven phase error within 20 ms, no underrun recovery, exact final
rational position, and fixed evidence residency despite more than 100,000
detailed-event evictions. This proves clock, demand, and bounded-aggregation
semantics; it does not prove CPAL hardware, codec/GPU throughput,
operating-system callback jitter, or reference-machine memory behavior.

`PreviewFrameStore<MK, M, VK, V, S>` is the Playback Module's payload-opaque CPU
storage and physical-allocation-ledger Interface. It owns decoded-media and
final Viewer residency, remembered terminal failures, and the explicitly pinned
current/stale Viewer raster. Adapters supply stable opaque keys, conservative
pre-work reservations, measured post-work charges, and an equality-comparable
Viewer presentation scope; the Module does not import media, renderer, UI, or
clock types. The generic standalone `PreviewFrameStoreConfig::default` is
96 optional media entries/384 MiB/four native resource units, one exact-current
demand grant of 16 entries/1 GiB/eight native resource units, 48 Viewer
entries/192 MiB, and 192 remembered failures. Those values are
fallback/reference defaults, not a fixed product budget.

Every admitted Broker attempt receives an independent move-only
`MediaWorkResourceLease`, including concurrent attempts for the same semantic
key. That lease follows the job through Broker queue, in-flight worker,
completion channel, and foreground result consumption. Replacement,
cancellation, queue rejection, stale completion, worker failure, or dropped
result releases only that physical attempt through RAII; no semantic-key
reconciliation may decrement the ledger. Successful Store admission validates
the measured charge and consumes the work lease into one cloneable
`MediaFrameResourceLease`. Store ownership and all external frame clones then
share one allocation identity until the last owner drops. The Adapter strips
lease/protection fields from the cached media payload itself, so Store-exclusive
ownership is observable and evictable. A late same-key completion preserves the
already resident allocation instead of replacing an object that a visual
candidate may still use.

The App therefore performs exact same-key/same-`MediaWorkReservationIntent`
rebinding before requesting this lease. Independent charging remains mandatory
for genuinely new attempts or a different demand scope; it is not permission
to double-charge an already queued or compatible in-flight producer.

Reservation accepts the complete
`MediaWorkReservationIntent::{Current(MediaWorkDemandId), Prefetch}` rather
than a priority plus optional identity, so a Current request without exact
working-set authority is unrepresentable. Admission separately reports
`RejectedCurrentDemandGrant` and `RejectedAggregateCapacity`. The first is
permanent for the unchanged exact Viewer closure. The second says only that
other physical ownership currently fills the aggregate ledger; the App may
project it as pending only when it can identify an admitted owner or lifecycle
transition that will publish a retry edge. Otherwise it is a typed aggregate
resource blocker.

Decoded-media policy has three distinct limits:

- the **optional residency limit** bounds trim-responsive Prefetch/cache
  entries, host bytes, and native resource units;
- the untrimmed **current-per-demand limit** bounds the unique allocations in
  one exact typed Viewer closure;
- the **aggregate hard limit** is the component-wise maximum of those two
  limits, not their sum, and bounds all Store, external, queued, in-flight, and
  unconsumed-completion ownership across every demand.

A Current completion that fits the aggregate and its exact demand but not the
optional LRU enters a bounded multi-key current-working-set overflow LRU. It is
not an unbounded oversize pin. `MediaFrameProtectionLease` counts each
allocation at most once for a demand even when the guard is cloned, and keeps
that input charged through asynchronous CPU-prefix, GPU candidate, and
presentation handoff. A seventeenth distinct input therefore cannot enter a
16-entry demand by multiplying guard clones, while multiple demand identities
cannot bypass the aggregate hard limit. Protection or measured-completion
refusal is a typed resource blocker; it must not be converted into a source
decode failure, remembered in terminal decode-failure state, or retried as an
`AlreadyResident` loop.

Production Window and Headless Preview always apply the current immutable App
resource decision. Before pressure trimming, the below-minimum, 8 GiB,
16 GiB, and 32 GiB-or-larger classes respectively receive media
entry/byte/native-unit limits of 24/96 MiB/4, 48/192 MiB/6,
96/384 MiB/8, and 128/512 MiB/12, plus Viewer entry/byte limits of
12/48 MiB, 24/96 MiB, 48/192 MiB, and 64/256 MiB. Unknown capacity uses the
conservative 8 GiB policy while remaining distinct evidence. Their exact-current
per-demand grants are respectively 4/256 MiB/4, 8/512 MiB/8,
16/1 GiB/16, and 24/2 GiB/24. Speculative and Aggressive pressure trim divides
only optional cache counts, bytes, and native units. It never reduces an
in-progress correctness grant or the 192-key failure bound.

The maximum temporal prefetch candidate depth remains eight frames; byte and
native-unit admission may shorten it. The 250 ms target is fully represented
through 30 fps; at higher frame rates the speculative horizon is capped (about
133 ms at 60 fps) to preserve decoder DPB/import headroom. A media reservation
includes current linear-float pixels, encoded source pixels, and the possible
lazy working-frame allocation, so a deferred
color transform cannot silently grow beyond its admitted reservation. One
payload larger than its budget remains usable for the current delivery through
the bounded current-working-set overflow only when it fits both the exact-demand
and aggregate grants. A Prefetch that exceeds optional policy is discarded.
Before speculative scheduling, the Store projects typed entry/byte/resource
headroom after subtracting physical non-releasable ownership and treating each
Store-exclusive LRU allocation as releasable. Prefetch and preroll consume this
through one near-to-far future-media prefix planner. That planner retains a
short-lived clone of every accepted nearer resident allocation while it
recomputes headroom and transfers planned missing work into Broker ownership;
the clone's physical allocation lease makes that resident non-releasable for
the complete planning/admission transaction. Pending Broker work is already
charged and is not reserved a second time. A media-bearing frame advances the
available prefix only when its complete prepared visual dependency closure is
resident, pending, or admitted. At most one deterministically ordered partial
frontier may be warmed, and no farther frame may bypass it. Actual Store
admission remains authoritative.

The planner separates immutable future-frame lowering from physical admission.
One small bounded sliding window retains the canonical prepared media closure
and its fully lowered immutable decode keys, reservations, and hardware request
contract. Its identity includes the open Authoring Session and Author
Generation, root Sequence identity/revision, Effect Registry revision, runtime
scale, target extent, complete Program Color Context, Asset Library revision,
Proxy configuration, and the coherent device-scoped Renderer hardware-admission
observation. An identity change clears the complete window; lifecycle and
hardware-admission edges also clear it eagerly. Sequential playback therefore
evaluates and lowers at most the newly entered frame after the initial window
instead of rebuilding every overlapping frame (`N + window`, not
`N * window`). A seek may reuse only an exact frame under that complete
identity.

The cached value contains no resident/pending/failure observation, allocation
guard, Broker ownership, retry state, or resource headroom. Those facts are
re-read for every planner pass and remain authoritative. Proxy artifact
arrival has no independent durable revision; a retained future key is instead
bound to its complete immutable file fingerprint, revalidates that filesystem
revision during every planning turn, and has both bounded entry count and
bounded reuse. Revalidation uses a turn-local memo keyed only by physical path:
all retained frame contracts for the same file share one live observation
within that turn, while the next turn must open and observe the file again.
Partial observations never authorize reuse. The window exposes cumulative
production evidence for reusable hits, semantic evaluations, media lowerings,
identity invalidations, and live source-fingerprint observations so performance
claims cannot hide repeated filesystem work. It can remain a correct
original-source optimization miss or fail closed on object drift, while
current-frame source resolution remains fresh and can immediately select the
new proxy. It can never authorize pixels from an unproved replacement.

`PreviewFrameStoreConfig` is a persistent runtime policy, not merely a
construction default. `reconfigure` replaces media count, byte, and
native-resource-unit limits, Viewer count and byte limits, and failure
retention, then immediately
evicts Store-exclusive ordinary/overflow LRU allocations against the prospective
limits before installing the policy. Thus a transient trim cannot create a
false overcommit transition. External frames, live work, and protected
candidates are never revoked; if they make the new policy irreducible, aggregate
and per-demand overcommit state/events remain explicit until their real owners
drop. High-water marks begin a new policy epoch after that atomic trim. All
later admission uses the new limits until another configuration arrives.
`PreviewProductionRuntime` applies the same decision to Window and Headless
paths. A stronger decoder-residency trim is a separate operation and cannot be
approximated by a transient cache clear.

Diagnostics separately expose optional residency, current overflow, outstanding
work, protected ownership, physical aggregate/high-water, per-demand
high-water, both limits, admission rejections, and overcommit transitions.
Current media admission also retains its last exhaustive result, keeping
decoder-family retirement, aggregate physical capacity, and Scheduler/realtime
execution pressure distinct instead of collapsing them into generic Loading.
Qualification requires optional residency within trim policy, aggregate and
per-demand high-water within their own grants, no overcommit event, and no
outstanding work lease. Renderer-owned import tables, bridges, textures, and
presentation resources remain a separate Module and need their own evidence.
Native resource units are intentionally independent of host bytes: an FFmpeg
surface can reserve zero host pixels while still retaining a decoder pool.
Adapters charge one unit per retained native surface and zero for ordinary CPU
frames.

One retained native `AVFrame` may keep its decoder's complete hardware-surface
pool resident, so entry/byte/resource-unit limits are necessary but not a
transport-idle release boundary. Once transport is stopped, Broker
pending/queued/in-flight work is zero, the current intent is not pending, and a
final registered GPU Viewer output has been proved for the active Preview
generation, the Production Runtime clears Store-owned ordinary and
current-working-set overflow residency while preserving that Viewer output, its
stale-presentation pin, and terminal-failure memory. External frame/protection
leases remain charged until their holders retire. The release also advances the worker-owned decoder
residency revision; each worker clears its explicitly owned
`PreviewDecodeSessionContext` before returning to `Idle`. Acceptance polls the
worker stage, active-helper count, and launch-to-post-reap accounting rather
than sleeping for an assumed timeout. Together these two ownership releases let
the driver surface pool disappear without blanking the paused Viewer. Any
active or unresolved work makes the release fail closed.

The Headless lifecycle converges this condition in ownership order: establish
the exact stopped-frame Viewer output, retire its exact GPU submission and
move-only presentation/media-protection owners, wait for Preview
pending/queued/in-flight work to reach zero, then request decoder-residency
release and observe worker/session reap. Releasing before output establishment
or GPU retirement is an ordering error; weakening the release predicate is not
an allowed recovery. Qualification consumes the final settled Runtime snapshot:
its counters and high-water marks retain the continuous-window evidence, while
its instantaneous Broker, worker, and helper fields alone prove post-stress
ownership closure. A mid-window helper snapshot must never be relabeled as
post-stress lifecycle evidence.

Headless realtime validation waits against absolute monotonic deadlines. On
Windows it uses a thread-local high-resolution waitable timer for a bounded
1 ms poll after checking the shared Preview work revision; it does not depend
on the commonly 15.6 ms-quantized condition-variable timeout and does not
change the process-wide timer period. A raced work notification can therefore
add at most one poll interval, while deadline cadence remains precise enough
to enforce the half-frame presentation-phase contract. Other platforms retain
the same revision predicate and bounded wait Interface.

The concrete product Window coordinator and the Headless realtime validation
coordinator share one platform scheduling seam. During active playback, Windows
joins the MMCSS `Playback` task at critical relative priority, macOS applies
thread-local user-interactive pthread QoS, and Linux requests a bounded
per-thread nice improvement without moving the fallible UI/event loop into a
realtime scheduling class. Pause/Stop/End and owner destruction restore the
captured thread-local state. Activation failure is explicit (and fails the exact
native scheduling qualification profile); an unavailable privilege or unknown
platform enters stable `PortableFallback` for the current residency and retries
only after that residency ends. This seam does not elevate decode/analysis
workers, alter process priority, or change the system timer period, so resource
arbitration and bounded domain queues remain authoritative.

Each coordinator turn applies the newest coherent Audio Device callback
observation before advancing the video Clock and deriving a new Frame Demand.
This ordering is part of the Clock-Master contract: deriving the demand first
would charge callback age that was already resolved by a queued observation,
mint an artificially early deadline, and then retain it under the correct
no-deadline-renewal rule. The post-pump transport tick uses a newly sampled
monotonic instant and still runs after a pump error, because a fail-closed audio
event may already have handed authority continuously to Synthetic Clock.

Long-run acceptance telemetry obeys the same realtime boundary. Native
product-process-tree memory samples are captured by a dedicated evidence
worker on a fixed monotonic cadence and accumulated only after the measured
playback window closes. Transport advancement, audio pumping, Preview result
draining, and Viewer submission therefore never execute Tool Help enumeration
or per-process memory queries inline. A separate post-stress sample may run
synchronously only after realtime playback and GPU ownership have settled.

The worker/session retirement boundary owns codec, DPB, hardware-frames context,
and native surface-pool destruction; it does not own the physical decoder
device. `mondrian-media` retains one immutable FFmpeg `AVHWDeviceContext` per
backend/renderer-adapter key and gives every unopened codec an independent
`AVBufferRef`. This avoids a playback-to-paused-exact race with driver device
teardown while preserving mutually exclusive Playback/Interactive frame-pool
residency. Device loss must later invalidate that exact cache key and create a
new device generation; it must never mutate the retained device in place.

GPU output publication and Broker lease retirement are separate observable
events. Publication may therefore attempt this release while the worker lease
is still completing. Before a stopped Preview binds a different generation,
the Production Runtime retries the release while the previous output is still
exact and can prove that its decoded sources are no longer needed. Typed
transport synchronization performs the same retry before retiring a
stopped-to-stopped discontinuity, because that retirement otherwise
invalidates the proof before Viewer generation binding runs. A play/pause
family transition, or any discontinuity without an exact output, only cancels
obsolete work and retains independently reusable CPU media frames. This is the
last safe point: after rotation the retained output is stale by definition and
must not authorize source-residency decisions. The retry remains non-blocking
and fail-closed when any old work is actually still queued or in flight. Once
an active candidate is denied by this retirement barrier, the Runtime records
its exact access family and rechecks the barrier before consuming one retry
edge. A final worker acknowledgement that races ahead of waiter registration
therefore remains actionable; an acknowledgement without a denied candidate
does not manufacture work. Once the source lease is released, a compatible Interactive decoder may apply the
current seek/flush policy for the new generation only when its packet-source
execution family also matches; decoder reuse does not grant the old media frame
publication authority or make canceled partial output cacheable.

GPU-resident scrub and exact Still decode share one Interactive session. Each
native output carries a lease through the Frame Store and renderer source copy;
the session is ineligible for seek/flush until the final clone retires. While it
is leased, the worker remains at a cancellable `OutputLease` backpressure point
and records `output_lease_wait_us` rather than opening a spare surface pool or
blocking inside the codec. This bounds discontinuous decoder residency without
assuming that generation count, elapsed time, cache eviction, or
`avcodec_flush_buffers` alone proves downstream surface release. The access
mode still selects independent scrub versus exact precision and seek policy on
every request; session sharing does not relax exact-Still correctness.
A released session may cross from Scrub to exact only when its packet-source
execution family, conservative source revision, and codec/output contract also
match. Production Scrub and exact use the same reusable isolated demux
family. The worker still waits for the final output lease before seek/flush,
and a successful exact request alone does not authorize destruction of a
healthy codec/DPB/frames context. Teardown occurs for a changed contract,
poisoned helper, residency-family retirement, idle retirement, or shutdown.
This preserves one native surface pool without opening an alternating pool
while the old output is leased. The immutable device cache and bounded seek
index survive mutable-session retirement because neither owns codec continuity
or decoder surfaces.

The Frame Store release above is necessary but not sufficient for native video.
The renderer's D3D12 bridge temporarily retains the imported source resource
through its GPU copy. That command-lifetime lease is governed by the bridge's
copy-ready fence, not by Preview generation, cache membership, decoder-session
lifetime, or visibility of the final Viewer texture. Headless presentation
retires completed source leases immediately after its completion wait; the
Window presentation loop retries the same non-blocking retirement before every
candidate request. A completed Headless output is invalid evidence if any
decoder source remains retained. This keeps the independently copied Viewer
output and reusable renderer bridge resources resident while returning decoder
surfaces early enough for the next exact seek.

The exact-output shortcut is generation-safe rather than cache-presence based.
The Preview generation includes Sequence identity/revision, Project Author
Generation, playback epoch or stopped frame, output extent, seek intent, the
exact resolved Project engine plus Sequence Program Color Context, effective
display color identity, and display-contract generation. Process-global OCIO
reload generation is operational evidence and is not part of semantic output
equality. `PreviewExecutionCoordinator` records which generation proved the
registered output. A generation rotation retains the old output only as stale;
only full output-key resolution or a newly registered presentation promotes it
to exact. Repeated stopped-Viewer queries may therefore reuse the already
proved output after media release, while any authoring, display, color-config,
seek, or transport discontinuity must re-evaluate and cannot revive stale
media semantics.

Regression coverage for play/pause generation rollover establishes a paused
transport at an exact frame before sampling the evaluation working set. A
Stopped-to-Playing action intentionally restarts at frame zero, so it is a
semantic coordinate change and must not be used as evidence that scheduling
generation alone preserves a frame evaluation. Transport actions in this
coverage must succeed explicitly; an ignored action failure is not a valid
cache-reuse observation.

`app::preview_frame_store::PreviewFrameStoreAdapter` is the sole Frame Store
policy Adapter inside the Preview Production Runtime. It computes exact
`MediaPreviewFrame` host-byte/native-resource reservations, admits the validated
UI-independent `PreviewRasterFrame` by its exact encoded byte size, and
constructs the `(SequenceId, width, height)` presentation scope. All residency
and failure state lives in the playback-owned Module. Decoded media with absent
or incomplete source revision evidence is never admitted. Entry count, host
bytes, and decoder-resource units remain independent budgets; the prefetch
window consumes those budgets but does not define or inflate them. The Store
never contains a Widget payload. `app::preview_raster_frame` owns RGBA8 validation, encoded color
identity, stable presentation-resource naming, and the CPU raster output
contract. The Window presentation Adapter alone maps that value to
`ViewerFrameImage`, reusing the same `Arc<[u8]>`; a headless Adapter can consume
the application contract directly and must instantiate the same Store Interface
rather than reproduce cache policy.

Viewer execution has exactly one renderer-owned Interface and Implementation.
`ViewerGpuExecutionRequest` supplies working-space layers, the exact output
boundary, crop, output extent, and optional proven calibration;
`ViewerGpuExecutionRuntime` owns native import, input transforms, compositing,
spatial processing, output transformation, calibration, and renderer resource
lifetime. App preview planning produces `ViewerGpuExecutionLayer` directly.
There are no App-named compatibility aliases or App-local execution wrapper.
The Window Adapter separately owns only UI texture registration and published
presentation identity. The UI-independent App Headless Adapter owns the real
no-Surface device, full-frame spatial request, asynchronous submission
lifecycle, and exact completion observation; it cannot import Widget geometry
or Window publication contracts. Both Adapters call the renderer Interface
directly.

The product policy gives the 8 GiB and 16 GiB classes pressure-stable Viewer
active grants of 768 MiB/64 textures and 2 GiB/96 textures respectively. A
pure policy assertion proves both grants cover the renderer's conservative
steady-state identity path for UHD P010 Main10 input, Float16 Program Output,
and the capacity-one current-output replacement reserve. This is a correctness
floor, not an 8 GiB realtime-4K promise; monitor conversion, calibration,
scopes, transitions, spatial filtering, and non-identity Effects enter their
larger explicit working-set contracts and may require quality/proxy fallback.

Frame-scoped renderer resources clear through one Interface while pipelines and
device capability state remain resident. On device/display invalidation, the
Window Adapter first unregisters its external texture and then resets renderer
execution resources. For an ordinary complete-GPU Viewer candidate, Window
completion still means registration plus ordered queue submission, not a GPU
fence or surface present; Headless uses the same queue-ordered usability rule
and retains the submitted owner until its later callback. A heterogeneous
candidate is stricter because its visual Broker lease is still active: Window
and Headless publication both wait for the exact queue-completion callback and
completed-batch validation. A Headless completion deadline revokes publication
authority without releasing the submitted owner; the capacity-one lifecycle
quarantines it until the exact late callback retires its frame and
media-protection leases. Headless Preview state retains only cloneable semantic
output metadata; the Adapter alone owns the move-only presentation lease.
Successful ordinary publication moves that lease synchronously from the
submission owner to the shared current physical slot, while heterogeneous publication
moves it only after callback/Broker validation. `Current` and queued-Ready reuse
requires the exact semantic key and unique submission resource key on both
sides. Timeout, cancellation, and late completion may clear Preview metadata
only after exact submission identity removed that same physical artifact;
same-semantic replacement is never clear authority. Accepted Transparent and
CPU Raster presentation clear both physical GPU slots in the same publication
commit. Once a presentable publication has consumed its Frame Demand, later
Headless probes may observe the already-published output without inventing a
replacement ticket, but only through a read-only predicate that requires both
the active Preview generation and the Adapter's unique physical resource key.
This observation cannot reactivate a stale generation, schedule work, or run a
publication callback; every new artifact still requires preflight and the
exact pending ticket. A later callback that proves only a stale, released, or terminal GPU
artifact must explicitly reopen one reconciliation of the current Headless
consumer intent; it cannot preserve an `Attempted` binding as though that
artifact had satisfied the current output, and it cannot relabel the artifact
`Ready`. Aggregate media pressure is transient only while the Scheduler or
worker Broker still exposes queued, executing, or completed-but-unconsumed work
that will publish a retry edge. The Runtime records one generation-scoped
current-candidate waiter and consumes it on either a result that releases
physical work ownership or settlement of every observable Scheduler/Broker
owner (including queued cancellation); this preserves exactly one
reconciliation edge even if generic candidate-pending state retired
concurrently. A background or canceled completion with no such waiter cannot
retry an unrelated pending Viewer intent; presentation-current results use
their `visible_change` edge, and exact reused work uses the separate binding
below. An external lease with no observable owner remains a typed
Blocked result rather than an unbounded Pending state. Rebinding required media
to existing queued or in-flight work uses a separate exact waiter set keyed by
media identity and the complete generation/class/resource-scope/demand
binding. A Timeline candidate may request multiple media inputs, so this is not
a single global bit. Broker state is the level predicate: unrelated work cannot
keep a waiter alive, and an orphaned pending binding is not an execution owner.
The Frame Work Broker enforces the stronger invariant under its lifecycle lock:
every retained pending binding has at least one exact queued payload or
compatible in-flight lease. Completion, failure, and abandonment remove the
last owner and its binding atomically, but preserve the binding when a real
queued/in-flight fallback exists.
If candidate-local Prefetch planning cancels or supersedes a rebound owner after
Timeline evaluation, the Runtime publishes a payload-free work revision while
retaining the waiter; the next ordinary completion poll consumes it into one
candidate-retry request. That request is level-triggered until Window or
Headless actually enters candidate evaluation and acknowledges it; merely
polling work cannot lose retry authority during a same-turn Transport change.
Generation rotation, invalidation, and transport retirement clear all such
waiters and retained requests. Headless otherwise claims no UI publication.
Device-scoped Renderer native decode admission is consumed by both Adapters and
remains separate from media
decode capability probes.

## Required invariants

1. Exactly one Clock Master is authoritative in Playing/Recovering.
2. Forward `1x` published timeline time never moves backward.
3. Old epoch/revision results never become visible or audible.
4. A demand has at most one accepted terminal delivery.
5. Pause/Stop/Seek/UI close never wait for decode, GPU, or audio workers.
6. Late video cannot pause Audio/Synthetic Master during stable playback.
7. Priming is bounded and has an explicit timeout outcome.
8. Automatic recovery may change only temporary preview resolution.
9. Blocked color/capability paths cannot be relabeled as ordinary buffering.
10. Preview and export retain identical timeline/effect/color interpretation;
    only realtime scheduling, resolution, and presentation may differ.

## Verification

### Deterministic contract tests

- all legal and illegal state transitions;
- epoch invalidation across seek/project/timeline revision;
- fake Audio/Synthetic clocks including jitter and backward observations;
- device loss continuity and Synthetic → Audio controlled handoff;
- 19,999,999/20,000,000/20,000,001 ns phase thresholds, callback uncertainty,
  59.94 target lowering, arithmetic overflow, and transactional rejection;
- priming timeout, late frames, stale frame, recovery hysteresis;
- no automatic proxy/color/project mutation;
- rational frame/sample conversion at 23.976/24/25/29.97/50/59.94;
- end, loop, zero-duration, empty sequence, and very short clip behavior.

### Integration tests

- fake decode/presentation/audio adapters through the same production Interface;
- slow Long-GOP decode with responsive pause/seek/close;
- worker completion after cancellation cannot publish;
- device loss during Priming, Playing, Recovering, Paused, and shutdown;
- GPU/import blocker remains distinct from decode lateness;
- preview resolution recovery preserves exact color/timeline semantics.

### Reference-machine gates

- 30 minutes continuous playback with non-empty Audio Device phase evidence,
  zero running-interval unproven presentable delivery, and maximum
  uncertainty-inclusive phase error ≤20 ms while Audio Master is valid;
- paused/still seek deliveries retain `phase_not_applicable` diagnostics without
  failing the running-interval phase gate;
- injected device loss: timeline discontinuity ≤1 ms at Synthetic handoff, no
  backward time, controls remain responsive;
- recovery handoff produces bounded phase evidence and no visible timeline jump;
- 100 cross-region seeks with latest epoch only;
- sustained 4K/Long-GOP pressure reaches bounded
  `product_process_tree_private_commit_v2` memory and reports drops; every
  cadence sample covers the App plus demux/FFmpeg descendants with complete
  scope/count/inventory evidence, so current-process-only data fails closed;
- every planned Headless observation advances the complete execution-resource
  cycle before GPU completion, Prepared Successor, or exact-current reuse can
  return; fewer resource-policy applications than planned observations fails
  closed even when all frames were otherwise presented;
- Golden/Stress Project runs use real media and structured evidence.
