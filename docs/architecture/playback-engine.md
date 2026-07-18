# Playback Engine

## Purpose

The Playback Engine owns realtime transport semantics. It converts commands,
monotonic time, clock observations, audio health, and Frame Deliveries into one
authoritative Playback Snapshot plus bounded work directives. UI, Viewer,
decode, renderer, and audio adapters execute directives; none of them may
independently advance, pause, buffer, or end transport.

This design replaces the current distributed ownership across `AppState`,
`app::playback`, `app_ui::host`, and `app_ui::preview`. Migration is incremental:
existing decode, render, cache, and audio implementations remain usable behind
the new seams.

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

## Non-goals for the first implementation

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
| Preview Frame Store | ready/stale/in-flight identity, source revision, color contract, memory budgets | deadline policy or proxy selection |
| Playback Preview Pump | one pending-demand sample, ordered completion/expiration delivery application, current-epoch video-preroll observation, Window/Headless-neutral pump outcome | decode/render implementation, Widget refresh, GPU resources |
| Preview Execution Coordinator | complete generation binding, pending state, executed presentation quality, candidate identity, exact registered output | timeline interpretation, codec payloads, GPU resources, Widget state |
| Presentation Adapter | GPU import/composite/display, Viewer handoff, presentation evidence | timeline advancement |
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

The eventual Rust names may change during implementation, but the semantic
Interface is fixed by this document.

### Playback Session

A session contains:

- monotonically increasing `epoch`;
- active `sequence_id` and immutable timeline revision/signature;
- exact `TimelineTime` anchor in the active Sequence domain;
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

An App Adapter applies Sequence identity, semantic revision, evaluation time
base, content end, and the requested position as one validated
`PlaybackTimelineBinding`. `play_timeline` and `seek_timeline` commit that
binding and the transport transition atomically. One user Play or Seek intent
rotates exactly one epoch: the Adapter must not compose a public timeline reset,
seek, and play sequence. A seek received during Priming, Playing, or Recovering
preserves play intent and publishes the new bounded Priming demand immediately;
a paused or stopped seek publishes one untimed demand. Binding and position time
bases must match or the complete transition fails without partial mutation.

### Observations

- monotonic tick with a nondecreasing timestamp;
- audio device state and consumed-sample position;
- audio buffer depth, underrun, and preroll readiness;
- terminal Frame Delivery;
- worker/capacity health required for recovery decisions.

Observations with an old epoch or policy revision are recorded as stale and
cannot alter the current session.

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
measurement. The Audio Playback Engine classifies every device clock
observation:

- `DevicePosition`: an OS/backend position correlated to the active stream and
  corrected by measured output latency;
- `CallbackConsumptionEstimate`: cumulative callback-consumed frames plus a
  bounded, recorded latency estimate;
- `Uncertain`: a previously healthy active stream has temporarily stopped
  producing fresh callback evidence, but has not reported loss or failure;
- `Unavailable`: no sufficiently monotonic or stream-correlated observation.

Both usable grades must carry stream generation, sample rate, integer consumed
frames, observation timestamp, latency estimate, monotonicity status, and
uncertainty. Reports preserve the grade. A callback estimate may drive Alpha
playback only when its uncertainty remains inside the configured A/V budget; it
must never be reported as an exact hardware position. `Uncertain` preserves an
already-active Audio Device Clock Master for at most the versioned one-second
grace interval without inventing new consumed samples. Fresh qualified evidence
resumes the same interval; expiry selects Synthetic Clock Master continuously.
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
- No audio tracks may use Synthetic Clock Master directly rather than opening a
  meaningless silent device stream.
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
5. phase error within the hard handoff budget.

Phase qualification compares the audio candidate with the continuous
Synthetic Clock position at the observation timestamp, not with the published
integer video-frame start. The latter is quantized by as much as one frame
(40 ms at 25 fps) and can falsely reject a correctly aligned audio stream when
the handoff budget is smaller. Published video position remains frame-based;
only the cross-clock qualification reference preserves subframe monotonic time.

Small phase error is removed by bounded audio resampling/slew. Error outside the
hard budget keeps Synthetic Master active and reprimes audio; it must not jump
the timeline or duplicate/drop an arbitrary video interval.

### Event-loop clock boundary

The winit adapter samples its wall clock on every `AboutToWait`, including while
transport is stopped or paused. An elapsed interval is forwarded to the
Playback Engine only when transport was running at both ends of that interval.
The first tick after Play or Resume therefore carries zero elapsed time; idle
wall time before the command is never charged to the new playback session.
Audio-device observations remain authoritative after handoff, while this rule
also keeps the Synthetic Clock Master continuous during device absence,
preroll, and recovery.

### Time representation

- Authoritative Timeline anchors use ADR-0004 exact Timeline Time in the active
  Sequence domain. Frame and sample positions are explicit evaluation adapters.
- Monotonic duration is runtime-only and never persisted.
- Video scheduling resolves a Frame Position/evaluation instant; audio
  scheduling resolves an integer sample position carrying its sample rate.
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
Prefetch is advisory, playback-only, slack-only, and cannot displace visible
current-frame work.
When either Synthetic or Audio Device Clock Master reaches natural end, the
Engine invalidates the prior realtime demand and publishes a new untimed demand
for the exact final frame before settling in `Ended`. It must never clear a
terminal marker while retaining the old demand identity: that would revive an
already presented frame and allow redundant timeout work to emit a second
terminal delivery.

## Frame request scheduling

`FrameWorkBroker<K, D, P>` is the Playback Module's codec- and UI-independent
request-lifecycle Interface. `K` is an opaque Adapter key, `D` an opaque
deadline value, and `P` an opaque execution payload; the Broker
interprets only:

- `FrameWorkClass::{Playback, Interactive, Still}`;
- `FrameWorkPriority::{Current, Prefetch}`;
- a monotonically increasing latest-wins generation;
- an optional opaque `FrameDemandIdentity` carried for terminal reporting;
- a strict nonzero pending-request budget.

The Module stores and returns `D` but never compares it directly. Admission
also carries the remaining duration sampled beside `D`; the Broker lowers that
duration once into its own Monotonic Runtime Clock for dequeue, cancellation,
and completion decisions. Each pending entry owns one atomic
`FrameRequestBinding<D>` containing generation, priority, semantic class,
Frame Demand identity, and deadline.

Only playback-class work may prefetch. Current work may evict prefetch; realtime
current work may additionally evict deterministic still work; still work cannot
evict realtime current work. Ordinary frame advance within one Playback Epoch
does not rotate generation: the frame belongs to the opaque request key, while
generation identifies a seek/restart or interpretation discontinuity. Starting a newer generation makes older work
ineligible, but late results are still classified explicitly as `CacheOnly` or
`Stale` rather than being allowed to mutate visible state. Cancellation and
expiration remove pending ownership synchronously; an already executing Adapter
must still poll `execution_cancellation` cooperatively and resolve its execution
lease before publication.

The Broker timestamps pending admission, execution-lease start, and invalidation under that
same lifecycle lock. `execution_cancellation_evidence` atomically classifies broker
closure, supersession, current-over-prefetch preemption, and
realtime-over-still preemption, and carries both the corresponding invalidation or
competing-request age and the lease age from the same clock sample. Starting a generation, canceling/expiring a binding,
changing semantic class, eviction, and completion-driven binding removal all
refresh this state. A compatible same-key request that rebinds in-flight work
clears the temporary invalidation timestamp, so latest-wins reuse is not
mislabeled as cancellation. Adapters may translate the generic disposition and
monotonic age into domain-specific diagnostics, but cannot reconstruct policy
from separate freshness, competing-work, or timestamp queries. The Broker does
not own codec-specific reasons or performance budgets.

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
Every current work class retains semantic lane affinity. Playback runs on
Playback/Any, scrub on Interactive/NonPlayback/Any, and exact still work on
Still/NonPlayback/Any. This prevents one request stream from cold-opening a
decoder/device session on each idle worker and prevents deterministic still
decode from occupying the realtime playback lane. Parallelism remains
available through lanes whose declared acceptance spans the class rather than
through implicit cross-lane stealing.

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

The compact `FrameDelivery` carries only the exact demand identity and terminal
kind. Source time, completion time, decode/import/render/presentation path,
source fingerprint, color contract, reason, and stage durations remain in the
owning media/Viewer execution diagnostics and are correlated by demand and
candidate identity; they are not duplicated into the Engine command payload.

`FramePresentationTicket` is the Playback Module's opaque Interface for final
presentation. It binds the exact demand identity, authoritative
optional `MonotonicTimestamp` deadline, and an allowed quality of Ready or
Degraded. CPU, Window GPU, and headless GPU Adapters call `complete_at` with
their actual completion timestamp; only this Module classifies timed work as
Ready/Degraded versus Late. An untimed paused-seek ticket cannot become Late,
but its exact identity is still invalidated by the next demand. Adapters never
compare or reconstruct deadlines themselves.
The App presentation Adapter does compare the opaque ticket identity with the
Engine's currently pending demand before submitting completion. A GPU result
that finishes after supersession is silently retired: it is neither evidence
for the new demand nor a rejected terminal delivery. This identity guard does
not interpret the deadline or manufacture an outcome.

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
| audio observation invalidity | 100 ms or explicit device error | hand off to Synthetic Master |

Current-frame presentation and media lookahead are independent observations.
The Engine records the terminal current Frame Delivery exactly once, while the
Preview Adapter reports a bounded `VideoPrerollObservation` containing ready
and available future media-frame counts for the same Playback Epoch. Neither
signal can release `Priming` alone. The required lookahead is
`min(policy.minimum_video_preroll_frames, available_media_frames)`, so a frame
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

1. samples `pending_frame_demand` exactly once;
2. asks the Adapter for bounded completion and stalled-current expiration facts
   bound to that identity;
3. applies every exact terminal Frame Delivery through `AppState`;
4. only then samples current-epoch video lookahead and submits one
   `VideoPrerollObservation`;
5. returns visible-change, transport-change, and follow-up-poll facts without
   performing Widget refresh or GPU presentation.

This order prevents completion and expiration from binding against different
demand samples, and prevents a terminal delivery from being followed by
preroll for the demand it just consumed. `AppUiHost` decides only how the pump
outcome affects layout/repaint. The real Headless GPU gate drives the same pump
and therefore cannot maintain a test-only Delivery or preroll policy. The
Preview Adapter still owns concrete decode execution, payload adaptation,
result diagnostics, and lookahead observation; it does not own Playback state
transitions or Viewer candidate lifecycle. `app::preview_execution` owns the
complete output key and GPU execution contract consumed by both Window and
Headless Adapters, and atomically coordinates generation binding, pending state,
executed quality, candidate IDs, and exact registered-output reuse. Cache
residency/eviction remains in the playback-owned `PreviewFrameStore`; GPU/color
mathematics remain renderer-owned. Within the concrete Adapter, timeline
traversal and nested Sequence evaluation live in
`preview::timeline_evaluation`, media-key/path/proxy/decode adaptation lives in
`preview::media_adapter`, decoded/native payload ownership and the single lazy
CPU working-frame adaptation live in `preview::media_frame`, resolved Viewer
identity plus GPU execution-layer lowering lives in `preview::viewer_plan`, and
working-linear CPU composition plus the encoded raster boundary live in
`preview::composite`. Final exact GPU/raster/stale/CPU output arbitration remains
in `preview::presentation`. These are private deep Modules over the existing
request Interface, not new public seams; the parent coordinates them but no
longer owns their color execution implementations or payload conversion state.

Deterministic Headless fault sequences cover a seek that retires an in-flight
presentation, queued cancellation racing the replacement demand, delayed or
dropped GPU presentation, Audio Device Clock reacquisition, explicit device
loss, and continuous Synthetic fallback. Losing old-epoch facts are retired
before Evidence; only the current demand can consume terminal authority.

All thresholds live in one Playback Policy value, appear in evidence, and may be
tuned by measured reference-machine data. Tests must pass an explicit policy;
environment variables cannot silently redefine product semantics.

## Frame selection and display cadence

Clock time is converted to the exact sequence frame using rational arithmetic
and the sequence's defined frame-boundary rule. When several frame boundaries
are crossed between event-loop ticks, the Engine demands the newest required
frame and records intermediate frames as clock-driven drops; it does not enqueue
obsolete current-frame work merely to preserve a one-request-per-frame count.

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

- One bounded current-demand slot per session; newer current demand supersedes
  older unstarted work.
- Prefetch has a separately bounded budget and is discarded first.
- Worker completion polling has both count and wall-time budgets.
- Cancellation is cooperative, but publishing is guarded again by epoch and
  demand identity.
- Shutdown never joins a potentially stalled codec/device worker on the event
  thread.
- Cache/in-flight keys include access mode, source fingerprint, source time,
  dimensions, color contract, proxy policy, timeline revision, and relevant
  quality revision.

## Evidence and health

Playback Evidence is event-derived and versioned. Minimum events:

- session/state/epoch transition;
- Clock Master selection and handoff;
- Frame Demand admitted/rejected/canceled;
- Frame Delivery terminal outcome;
- video deadline drop and stale presentation;
- quality recovery entry/step/exit;
- audio preroll, underrun, device loss/recovery;
- explicit degraded path or blocker.

Reports must include p50/p95/p99 queue/decode/render/delivery latency, dropped
and stale frames, consecutive pressure, effective preview scale, Clock Master
residency, handoff phase error, audio underruns, A/V drift, cache budgets, and
reason-code counts. Logging alone is not evidence.

`PlaybackEvidenceCollector` is the shared bounded Interface for production UI
and headless/perf Adapters. Schema v2 retains at most 4,096 detailed tail events
and a deterministic 4,096-sample whole-run reservoir per latency/drift metric.
Each metric separately keeps its exact population count and exact all-run
maximum; p50/p95/p99 are explicitly reservoir estimates. Clock Master and
Transport State residency, delivery counts, superseded demand/seek totals, and
underrun totals are streaming aggregates and do not depend on retained detail.
Detailed-event eviction is expected bounded retention, remains reported, and is
not reclassified as observation loss. A gate may use the exact maximum, a
declared reservoir percentile, or aggregate counters, but cannot infer failure
merely because old diagnostic events were intentionally evicted.

The accelerated headless continuity gate drives the public Engine and Evidence
Interfaces for 30 minutes of exact 48 kHz callback time against a 30000/1001
video grid. It completes current Frame Demands, injects one Audio Device to
Synthetic to Audio Device transition, and requires zero delivery-clock drift,
no underrun recovery, exact final rational position, and fixed evidence
residency despite more than 100,000 detailed-event evictions. This proves clock,
demand, and bounded-aggregation semantics; it does not prove CPAL hardware,
codec/GPU throughput, operating-system callback jitter, or reference-machine
memory behavior.

`PreviewFrameStore<MK, M, VK, V, S>` is the Playback Module's payload-opaque CPU
storage Interface. It owns decoded-media payloads, final Viewer payloads,
remembered failures, the explicitly pinned current/stale Viewer payload, and an
oversize current-media pin. Adapters supply stable keys, exact byte
reservations, and an equality-comparable Viewer presentation scope; the Module
does not import media, renderer, UI, or wall-clock types. Media entries
have a 96-entry cap, a 384 MiB pixel-payload budget, and an independent
decoder/GPU resource-lease budget; the generic Store default is four units,
while the App composition root raises it to the sixteen-frame maximum bounded
prefetch window. Viewer raster
entries have a 48-entry cap and a 192 MiB payload budget; failure memory has a
192-key cap. A media reservation includes current linear-float pixels, encoded
source pixels, and the possible lazy working-frame allocation, so a deferred
color transform cannot silently grow beyond its admitted reservation. One
payload larger than its budget remains usable for the current delivery through
the current-media pin but is not admitted to the LRU; an oversize prefetch is
discarded rather than pinned. Both cases produce structured rejection evidence.

The non-evictable current/stale raster and oversize current-media frame are
reported separately from their evictable caches. Project/sequence cancellation
clears media, Viewer, failure, and pinned state through one Interface. External
real-media gates fail when either cache exceeds its byte budget, either pin
exceeds its corresponding budget, or an oversize payload was rejected.
Renderer-owned GPU texture tables remain a separate Module and are not falsely
counted as CPU storage.
The resource-unit budget is intentionally independent of CPU bytes: an FFmpeg
native decoder surface can reserve zero host pixel bytes while still exhausting
the decoder pool. App payload adapters charge one unit for each retained native
surface and zero for CPU frames. LRU eviction enforces count, byte, and resource
budgets together, and diagnostics expose both current resource units and the
configured limit.

The App's `PreviewCpuFrameStore` is now a thin Adapter and policy composition
root: it computes the
reservation for `MediaPreviewFrame`, maps `ViewerFrameImage` to its encoded byte
size, aligns native-resource residency with the configured maximum prefetch
window, and constructs the `(SequenceId, width, height)` presentation scope. All
residency and failure state lives in the playback-owned Module. A headless
Adapter must instantiate the same Interface rather than reproduce cache policy.

Viewer execution now has one renderer-owned Interface and Implementation.
`ViewerGpuExecutionRequest` supplies working-space layers, the exact output
boundary, crop, output extent, and optional proven calibration;
`ViewerGpuExecutionRuntime` owns native import, input transforms, compositing,
spatial processing, output transformation, calibration, and renderer resource
lifetime. App preview planning produces `ViewerGpuExecutionLayer` directly.
There are no App-named compatibility aliases or App-local execution wrapper.
The Window Adapter separately owns only UI texture registration and published
presentation identity; the headless Adapter separately owns submission and GPU
completion waiting. Both call the renderer Interface directly.
Frame-scoped renderer resources clear through one Interface while pipelines and
device capability state remain resident. On device/display invalidation, the
Window Adapter first unregisters its external texture and then resets renderer
execution resources. Window completion means registration plus ordered queue
submission, not a GPU fence or surface present; headless completion waits for
the submitted GPU work and claims no UI publication. Renderer/platform native
decode admission is consumed by both Adapters and remains separate from media
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

- 30 minutes continuous playback with absolute A/V drift ≤20 ms while Audio
  Master is valid;
- injected device loss: timeline discontinuity ≤1 ms at Synthetic handoff, no
  backward time, controls remain responsive;
- recovery handoff produces bounded phase evidence and no visible timeline jump;
- 100 cross-region seeks with latest epoch only;
- sustained 4K/Long-GOP pressure reaches bounded memory and reports drops;
- Golden/Stress Project runs use real media and structured evidence.

## Migration plan

### Phase 0 — Characterize

Pin existing play/pause/seek/end, buffering escape hatch, audio output, scheduler,
and diagnostics behavior with characterization tests. No semantic refactor yet.

### Phase 1 — Pure Playback Engine

Introduce the headless state machine, exact anchors, Synthetic Clock Master,
commands/observations/snapshots, invariants, and fake-clock tests. Adapt existing
`AppState` methods to delegate while retaining current workers.

### Phase 2 — Frame Delivery seam

Replace `preview_waiting` feedback with typed Frame Deliveries. Move deadline,
drop, Priming, and Recovering decisions into the Engine. Keep current
`AppUiPreviewService` as an Adapter.

### Phase 3 — Audio Playback Engine

Move device/worker/buffer/generation fields out of `AppState`. Add consumed
sample evidence, device-loss detection, Synthetic fallback, preroll, and
controlled handoff.

### Phase 4 — Preview deepening

Consolidate frame-work scheduling in the Frame Work Broker; extract Frame Store
and Evidence from `app_ui::preview` by behavioral ownership, not file size. The
Playback Preview Pump is extracted and Window/Headless duplicate orchestration
is deleted. Timeline evaluation, media adaptation, resolved Viewer planning,
media execution, final presentation arbitration, and immutable diagnostics now
each have private deep Modules.
The concrete media worker, cooperative cancellation, result publication, and
FFmpeg Adapter remain isolated in `app_ui::preview::media_execution`; private
modules deliberately reuse parent payloads rather than exposing a broad
cross-layer Interface. `app_ui::preview::presentation` selects exact registered
GPU output, raster-cache hits, same-scope stale reuse, deferred playback
composites, and the CPU output boundary. It owns no generation, candidate,
cache-residency, scheduling, or transport authority. Retain one public request
seam into media and continue moving only behavior with clear ownership.

### Phase 5 — Realtime policy

Enable deadline-based drops and temporary-resolution recovery on the reference
machine. Preserve explicit proxy recommendation and fail-closed color policy.

### Phase 6 — Advanced transport

Only after forward `1x` gates pass, implement signed rational rate, reverse/J-K-L,
loop playback, variable-rate audio, and corresponding cache/decode policies.

Each phase must leave the product runnable, update or explicitly version evidence
schemas when semantics change, and delete superseded state rather than maintaining
compatibility authorities during Alpha.

## Current integration status

Phase 1 is complete; Phase 2 and Phase 4 are active; and the first Phase 3 Clock
Master slice is integrated through the `mondrian-playback` crate. The pure Engine now owns
the app's transport position/state, Synthetic Clock Master, epoch invalidation,
exact rational clock advancement, stale-delivery rejection, and bounded
temporary-resolution recovery policy. `AppState` projects current frame and
running state from the Engine; the former app-local `PlaybackState`, frame
accumulator, reached-end flag, and misleading audio/video-master diagnostic have
been removed.

`app_ui::playback_feedback` now keeps payload-free Viewer lifecycle separate
from exact Frame Presentation Tickets. Loading and Stale remain non-terminal,
Blocked remains a distinct correctness terminal, and a Ready lifecycle has no
transport authority without a ticket. Duplicate terminal delivery identities
cannot mutate recovery twice. The former
Viewer-owned `playback_buffering` state and its audio mute/clock hold have been
removed. Window redraw may still defer duplicate GPU candidate preparation while
Loading, but that presentation guard has no transport authority.

`app::playback_preview` now owns the production result-to-Playback pump used by
both `AppUiHost` and the real Headless GPU harness. The Preview Adapter returns
only bounded work/preroll facts; it no longer exposes completion, expiration,
and preroll operations for each consumer to compose independently. Window code
retains repaint/layout policy, while Delivery ordering and Transport mutation
are shared and covered by UI-free tests.

The Engine now emits a Frame Demand containing epoch, quality revision, demand
sequence, sequence/timeline revision, exact target, preview scale, and an
optional monotonic deadline. Paused seek emits an untimed current-frame demand;
playback/priming emits a timed demand. Its opaque Adapter identity projects epoch, quality revision, demand
sequence, and target frame. Preview workers consume an absolute wall projection
of the demand deadline instead of independently reconstructing frame duration.
Same-epoch completions for a superseded demand are rejected before target
validation.

Playback-current preview jobs carry the opaque identity projection (`epoch`,
quality revision, demand sequence, target frame) through queue, worker, result,
and app polling. Media workers do not interpret playback policy. Polling returns
exact terminal deliveries only for Late or Failed frame work. Decode-job
cancellation remains broker/media evidence and does not masquerade as a frame
presentation for an identity that latest-wins scheduling may already have
replaced. A successful Ready/Degraded decode is staged as nonterminal readiness; its classification is
attached as a Frame Presentation Ticket to the exact GPU candidate or CPU
presentation and terminates the demand only after that Adapter produces a usable
output. The ticket's final completion timestamp, not decode completion, decides
whether the result is Ready/Degraded or Late. The Playback Engine
performs the final identity check, so an old candidate cannot terminate a newer
demand. Media generation remains a decode/cache cancellation mechanism and is
not a substitute for Playback Session identity. Payload-free Viewer lifecycle
feedback cannot synthesize Ready without this exact presentation identity.
Scheduler pending state retains the same identity until completion or
expiration. A stall expiration returns that stored identity and never asks the
host to synthesize a delivery from whichever demand is current at poll time.
Scrub expiration is capacity/cancellation evidence, not a playback Late Frame
Delivery.

Phase 4 now uses one `FrameWorkBroker` in `mondrian-playback`. It atomically owns
semantic class validation, pending and queued capacity, generation invalidation,
current-over-prefetch and realtime-over-still preemption, worker-lane dequeue,
in-flight execution leases, deadline dequeue, completion freshness,
cancellation/expiration, and stable diagnostics. `app::preview_access_mode` is
the UI-independent application Adapter that owns the media key, explicit access
intent, bounded job transport, worker-lane mapping, and wall-deadline projection.
Thumbnail, Viewer, frame-store, and scheduler-policy code depend on this module;
it has no dependency back on `app_ui`. The
former `FrameRequestScheduler` and App-local Condvar queue authorities were
deleted rather than retained as compatibility paths. App-local worker-activity
atomics were also deleted: windowed reports and Headless verification share the
Broker diagnostics Interface for execution-lane residency.

`app::preview_scheduler_policy` owns the corresponding pure decision boundary:
Frame Demand deadline completion, real decode execution quality, bounded
frame-rate-derived prefetch depth, frame-local hardware recovery signals,
consecutive-late pressure, and timestamped scrub adaptation. Moving it out of
`app_ui` prevents a Viewer Adapter from redefining Ready/Degraded/Late,
hardware-fallback, or decode-strategy semantics.

Time-sensitive Broker transitions use the `MonotonicRuntimeClock` Interface.
Production adapts Rust's monotonic `Instant`; Headless tests inject an exact
manual clock. The Broker samples it under the lifecycle lock exactly once per
atomic operation, so every mutation in that operation observes one instant and
concurrent lock-acquisition order cannot manufacture a regression. An Adapter
must therefore be non-blocking and may not perform I/O or re-enter the Broker.
If a sample still moves backward, the Broker clamps it to the last observation,
increments structured regression evidence, and every preview/professional gate
fails closed. This runtime clock measures lifecycle intervals only: it is not a
Clock Master, Timeline Time, device-consumption clock, or displayed position.

Worker cancellation now crosses that same Interface as atomic
`FrameExecutionCancellationEvidence`: disposition, request age, and execution-
lease age share one Broker-clock sample. `app::preview_access_mode` is the
single application lowering point from that evidence plus request
priority/access mode into media cancellation reason, Playback work class, and
request-to-checkpoint attribution. It combines Broker evidence only with the
separate steady-state prefetch decode budget; the concrete worker polls this
policy, while `app_ui::preview` only records and projects its result. Absolute
Frame Work Deadline comparison is not repeated in the App. The Broker evaluates deadline, generation invalidation,
preemption, and closure together and returns authoritative request and lease ages, so
one cause cannot hide slower observation of an earlier cause. The first close
records an immutable Broker-clock instant; repeated close cannot renew it.
App worker-stop flags remain lifecycle signals only and cannot timestamp or
classify cancellation.

`FrameCancellationEvidenceCollector` is the single aggregation Module after
that seam. The App media Adapter records one completed observation containing
semantic work class, authoritative cause, total execution-lease lifetime,
lease-start-to-first-checkpoint, and request-to-first-checkpoint. Codec-function
entry is intentionally not an execution origin because dequeue and cancellation
can occur before it. The Module
derives checkpoint-to-return, preserves exact all-run counts/maxima, and
partitions Playback, Interactive, and Still without App-local counter families.
`FrameCancellationPolicy` is shared by windowed diagnostics, Headless tests,
and professional acceptance: request-to-checkpoint is at most 5 ms; Playback
and Interactive checkpoint-to-return are at most 50 ms; deterministic Still is
at most 500 ms. Unknown causes, missing request/checkpoint attribution, and
impossible timestamp ordering fail closed.
Total execution lifetime remains diagnostic because work performed before the
authority request is not cancellation latency.

Frame completion now resolves an atomic Frame Request Binding. The App Adapter
submits an absolute wall deadline together with the remaining duration sampled
immediately before admission. The Broker lowers that duration into its own
Monotonic Runtime Clock without interpreting the absolute value. Rebinding the
same in-flight key refreshes the latest demand identity and lowered deadline.
Before a worker publishes its result channel message, it stamps completion in
the Broker exactly once; a later UI poll atomically resolves freshness against
the latest binding while deadline status remains tied to worker return. Canceled
old work cannot remove a newer binding, and main-thread load cannot turn an
on-time decode into false Late evidence.

The playback-owned `PreviewFrameStore` now also contains the CPU residency
Implementation formerly local to App UI. Count and byte budgets, LRU eviction,
failure retention, current/stale Viewer pinning, and the oversize-current media
exception therefore have one test surface for both windowed and headless
Adapters. App UI retains only payload sizing and presentation-scope mapping.

The app Clock Adapter maintains a wall-`Instant` to Playback
`MonotonicTimestamp` mapping. Worker deadlines are projected as one absolute
wall deadline at the actual sampling instant. Immediately before Broker
admission the Adapter pairs that value with its then-current remaining duration;
the Broker lowers it once, so queueing cannot renew time or ask UI code to
compare clocks. Presentation completion uses the same
mapping, and Playback Evidence records that completion timestamp behind a
monotonic high-water mark. Demand latency therefore includes final CPU/GPU
presentation work rather than stopping at decode readiness.
Before play, pause, stop, or seek mutates the transport, the App Adapter advances
its runtime timestamp to that high-water mark. A paused GPU presentation can
therefore never make the next playback epoch begin with a partly expired
preroll deadline.

Play and running seek now remain in bounded Priming. Ready or allowed Degraded
delivery starts Synthetic Master immediately; after the 500 ms policy deadline,
Synthetic Master starts at the deadline anchor and catches up to the current
monotonic time without adding another priming interval of drift.

The CPAL Audio Playback Adapter now publishes a typed
`CallbackConsumptionEstimate`: stream generation, active-interval callback
frames, sample rate, callback age/quantum, estimated latency, uncertainty,
underrun frames, and stream failure. The callback counters are atomic and PCM
uses a fixed-capacity lock-free queue, so the realtime callback no longer takes
the former contended `Mutex<VecDeque<_>>`. The Audio Playback Module requires
120 ms PCM preroll,
a callback no older than 100 ms, and at least one active-interval callback
before offering Audio Device Master. The Engine accepts callback estimates only
within its versioned 50 ms uncertainty budget. This admits common host callback
quanta such as 1024 samples at 44.1/48 kHz while still rejecting stale or
coarsely buffered observations. The Engine anchors the first qualified
observation without a position jump, advances exact callback observations from
integer sample deltas, and interpolates the continuous audio phase from the
monotonic interval between callbacks. Stream failure, stale evidence, excessive
uncertainty, an impossible consumed-sample slope, or decreasing sample position
hands off continuously to Synthetic Master. A stale callback from an otherwise
healthy active stream instead enters `Uncertain`: the Engine preserves the
existing Audio Device Master for one second, accepts a fresh observation from
the same interval without reprime, and falls back continuously only when the
grace expires. Explicit stream failure/device loss remains immediately
`Unavailable`; repeated unavailable observations while already Synthetic do
not reanchor or freeze transport.

The grade remains explicitly `CallbackConsumptionEstimate`, not exact hardware
`DevicePosition`. The former app-owned `AudioClock`/`AudioRenderCursor` no
longer participates in realtime scheduling and cannot be exposed as Clock
Master evidence.

Device open/reopen now sits behind a non-blocking media Module. Because CPAL
streams are `!Send`, a named device thread retains each concrete stream and
publishes only its lock-free handle. Open failures retry from 250 ms to a 5 s
ceiling while Synthetic Master continues. Every new generation invalidates old
audio render work, clears queued PCM, remains inactive through 120 ms preroll,
and only then offers callback evidence.

Synthetic-to-Audio qualification is phase-controlled. An observation carries
the exact media `TimelineTime` at active-consumption frame zero; the Engine compares
latency-adjusted integer consumption with the continuous active Clock Master
phase at the same monotonic timestamp. Error beyond the independent versioned
20 ms handoff budget records `PhaseRejected`, keeps Synthetic authoritative
without reanchoring, and asks the app Adapter to reprime. Accepted/rejected
generation and signed phase error are immutable snapshot evidence. Small-error
resampling/slew and backend-native hardware position remain Phase 3 work.

The same Audio Playback Module now owns the PCM render worker, current
generation, integer-sample next-window position, bounded in-flight admission,
460 ms steady sequential high watermark, and preroll activation. `AppState` supplies only an
immutable timeline `AudioPcmRenderer` Adapter and consumes snapshots/events. A
headless fake output plus fake PCM renderer exercise the same Interface,
including exact window order, preroll activation, malformed-buffer silence
substitution, synchronous cancellation of queued old work, and rejection of the
at-most-one executing completion from an invalidated generation.
Environment variables no longer alter realtime audio watermark semantics.

Generation invalidation now has one cancellation authority in addition to its
publication check. Audio Playback cancels a monotonic core token before creating
the next generation; executing work observes that token through timeline DSP,
nested audio, decoded-source, and media-window seams. Unit tests hold a render
and a source decode in flight, reprime/cancel them, require observation within
50 ms, and verify canceled work creates no cache entry or remembered failure.
Generation comparison still rejects a completion even if a faulty Adapter
ignores cancellation; cancellation responsiveness and publication safety are
independent invariants.

The product `AudioPcmRenderer` and Export now bind the same block-oriented
`AudioDecodedSource` Interface. `mondrian-audio` keeps only an aligned
4,096-frame hot window; `mondrian-media` owns ten-second FFmpeg decode windows
in a cross-source 128-entry/256 MiB weighted LRU keyed by file fingerprint.
This removes whole-file PCM residency from playback and makes arbitrary seek
memory independent of source duration. The concrete media Adapter now owns at
most eight persistent FFmpeg child Sessions with bounded stdout look-ahead and
stderr retention. Sequential windows reuse one continuous decode stream;
non-contiguous misses restart only that source revision/output contract.
Generation cancellation is observed while waiting for output and performs
kill/wait/join. The PCM cache single-flights identical misses. Cold-open,
steady-sequential, and random-restart latency remain separate evidence classes;
the physical-device 30-minute gate is still required.

The physical-device gate is now an implemented, ignored reference-machine
Adapter rather than an unwritten requirement. `cpal_av_48khz_30min_v1` runs the
ordinary App transport for real wall time, consumes the production bounded
audio source through a concrete CPAL stream, completes a generated video layer
through the real headless Viewer GPU Adapter, and evaluates the shared Playback
Evidence plus native process-memory facts. It fails on a changed CPAL stream
generation, stale/failed callback, non-48 kHz stereo output, callback cadence
rate error above 1,000 ppm after a 100 ms minimum allowance, any PCM silence
substitution, any underrun recovery,
delivery-clock drift above 20 ms, less than 99.5% current video readiness,
source-window decode failure/oversize, missing/broken Session-pool or sequential-
reuse evidence, steady sequential latency above 460 ms, cache budget
overflow, or failure of `whole_process_private_commit_v1`. CPAL callback
consumption is explicitly OS-output evidence, not acoustic loopback evidence.
Before beginning the observation, the Adapter requires the same healthy CPAL
stream, fresh callbacks, Audio Device Clock Master, and Active Audio Playback to
remain qualified for one continuous second. A bounded 30-second tail may extend
wall execution only until both Playback Evidence and the current uninterrupted
callback interval each cover the required 30 minutes; it does not reduce either
threshold. Whether that tail succeeds or expires, evaluation writes the full
structured report rather than discarding the terminal snapshot behind an early
assertion.
On 2026-07-18 the full gate passed once on the local Windows development
machine: 86,474,752 callback-consumed frames over one 48 kHz stereo stream,
168,926 callbacks, 53,964/53,964 current-video Ready samples, 107,924 completed
headless GPU presentations, zero underrun/substitution/recovery, zero delivery-
clock drift, and zero rejected/failed/blocked terminal deliveries. Source-cache
residency was 264,960,000 of 268,435,456 bytes, the maximum steady sequential
ten-second decode was 79,973 us, settled Private Commit growth was 90,004,849
bytes, and the post-stress sample did not grow. This is local development
evidence, not a checked-in fixed-reference baseline or acoustic loopback.
Container duration alone cannot admit this run: the media probe must prove the
primary audio stream itself spans the complete observation, so automatic
post-EOF silence cannot masquerade as long-source evidence.
The local short CPAL/A/V smoke has reached the production stream and headless
presentation path with zero underrun and zero observed delivery-clock drift.
Its earlier ~1.02 s process-per-window miss was a cold-path observation that
motivated persistent Session reuse. It neither proves nor disproves the new
steady-sequential bound; only the complete reference-machine run can close the
gate.

Audio transport permission is now explicit. Priming renders into the bounded
queue under `AudioPlaybackMode::Preroll` without enabling consumption;
Playing/Recovering switches to `Consume` and activates that same generation.
This removes the former path where a slow video first frame allowed audio to
consume during Priming and then triggered a phase-rejected reprime/PCM clear.

Underrun recovery is now sample-budgeted inside Audio Playback. An isolated
callback shortage emits evidence and preserves Audio Device Master. Missing
frames accumulated to 20 ms in one active interval trigger a final valid device
position observation followed at the same monotonic timestamp by an unavailable
observation, so Synthetic handoff includes all consumed device time. Output then
re-enters explicit Recovering/preroll state with a new render generation; video
lateness and Viewer state remain unrelated to this decision.

The app now feeds the collector after transport, clock, demand, delivery, seek,
and underrun observations and exposes one serializable report. The real-media
continuous playback harness uses one Play plus monotonic Engine ticks and sends
exact deadline/failure worker deliveries and presentation deliveries through
the same Adapter ordering as the production Host. CPU/GPU Ready presentation
can close cache-hit outcomes, Viewer Blocked closes correctness outcomes, and
Viewer Stale remains nonterminal. The harness no
longer fakes continuous playback by seeking and restarting every frame.
External-media gates consume the same report and fail on delivery-clock drift
above 20 ms, any sustained-underrun recovery, or fewer than 90% current Ready
samples even when stale frames keep 95% of samples
visible. The headless harness now pre-rolls and executes only through the real
GPU Adapter; it no longer invokes the CPU raster Viewer in parallel. For every
fresh output, the Adapter retains the exact submission index and waits until
that submission completes before it marks the output presentable or completes
the Frame Presentation Ticket. Cached outputs may reuse only an output whose
original submission was already observed complete. Its report contains both
the count of completion-observed fresh outputs and per-output GPU completion
latency, cache reuse, color-stage evidence, explicit fallback reasons, and the
distinct extents actually submitted to the shared runtime. External gates fail
when real GPU execution coverage or exact completion coverage is missing,
playback GPU completion p95 exceeds one frame interval, a readback appears, or
a GPU blocker is reported. Pre-roll pipeline warm-up is reported separately
and cannot contaminate the steady-playback p95.
The professional 4K HEVC Main10 gate now has a fail-closed input and execution
contract (`uhd_hevc_main10_hardware_1x_v4`). It uses real FFmpeg decoder
profile/format/rate evidence, primary-video stream duration and any declared
frame count rather than container duration alone, the probed
rational cadence, frame-local decode provenance carried through caches and
prefetch, the exact Viewer candidate, and a completed headless GPU submission.
Its Adapter derives a non-overridable minimum frame count from 30 minutes and
the probed rational cadence, uses Playback Evidence's bounded whole-run
aggregates, pauses transport, completes 50 approximate warm plus 50 exact
cross-region seeks through the same GPU presentation path, and then schedules a
100-seek latest-wins burst. The gate
requires warm p95 at or below 200 ms, accurate p95 at or below 500 ms, at least
99 superseded-seek observations, no rejected old terminal delivery, and zero
Broker pending/queued/in-flight residency after the burst.
The same Adapter takes a native whole-process memory sample at a fixed one-second
cadence. `mondrian-platform-core::ProcessMemoryProbe` defines the OS-neutral
fact boundary and the Windows implementation uses `GetProcessMemoryInfo`.
Private Commit is the acceptance metric; current/peak Working Set remains
diagnostic because OS paging is not application ownership. The versioned
`whole_process_private_commit_v1` profile requires at least 240 valid samples
in both minutes 5–10 and minutes 25–30, no incomplete/native-probe samples, a
4 GiB absolute Private Commit high-water limit, and no more than 256 MiB growth
from the early settled-window average to the final-window average. After each
gate's declared terminal stress reaches verified worker quiescence (the 4K
video profile uses 50 warm seeks, 50 accurate seeks, and a 100-request
latest-wins burst), one further sample must remain within the same
absolute cap and within 256 MiB of the final playback average. The collector
stores scalar counts/sums/high-water facts only; duration cannot make evidence
memory grow. Unsupported platform probes fail the professional contract rather
than reporting zero. These are initial reference-profile thresholds and may be
changed only by a reviewed profile revision backed by reference-machine data.
The report projects playback-owned cancellation evidence separately for
Playback, Interactive, and Still work and the professional gate evaluates the
same shared policy directly. Realtime cancellation is not allowed to hide a
slower deterministic still decoder. Any Broker runtime-clock regression also
fails the gate because clamped timing remains diagnosable but is not valid proof.
In-process FFmpeg sessions now keep
the request probe installed as an `AVIOInterruptCB` across open, stream-info,
seek, and packet I/O; the optional external still backend kills and reaps its
child while draining both pipes. The product gate therefore evaluates each
class against its fixed return budget rather than deriving a threshold from UI
slow-frame settings.
Steady playback is evaluated by p95 plus at least 99.5% current-frame readiness;
the slowest single decode remains explicit diagnostic evidence but one
session-open outlier cannot independently fail a 30-minute run whose sustained
distribution and presentation hit rate pass.
The deterministic rules and versioned structured failure codes live in the
deep, UI-independent `app::playback_acceptance` Module; the perf harness is an
Adapter that projects UI execution diagnostics into acceptance evidence and
collects real probe, playback, and completed-presentation observations. The
professional profile fixes cadence, 30-minute coverage, timeouts, decode/queue
latency, visibility, readiness, and hardware-execution thresholds; developer
environment variables apply only to non-professional smoke runs and cannot
weaken these values.
It cannot pass from filename labels, hardware candidates/device contexts, or
PlaybackCursor aggregates containing speculative prefetch. The repository does
not contain the licensed/reference 4K Main10 fixture, so a successful
execution on one development machine. The repository still does not contain a
licensed/reference 4K Main10 fixture, fixed-machine result baseline, driver
matrix, or Golden Project identity; those remain required before release-level
professional playback acceptance can be claimed.

On 2026-07-18, a 2.88-second decoder-proven 3840×2160 25 fps HEVC Main10 HLG
sample completed the external continuous-playback smoke on an NVIDIA RTX 3050
Laptop GPU: 60/60 current frames were Ready, all 60 media layers used retained
D3D12VA P010 hardware surfaces, all 60 Viewer composites remained native GPU
work, and upload/readback/fallback counts were zero. GPU execution p95 was about
1.8 ms and the real-media gate passed. This proves the short production
decode→native-import→GPU-presentation path on that machine; it does not satisfy
the 30-minute duration, repeated seek, physical audio-device, driver-matrix, or
bounded whole-process-memory acceptance run. The source was an unverified-rights
local download used only by an ignored manual diagnostic. It is not a canonical
fixture and cannot support release or professional acceptance evidence.

Later the same day, an unverified-rights local 30m39s 3840×2160 59.94 fps HEVC
Main10 HDR10 source completed the full video v4 Adapter: 107,894 cadence frames,
107,850 current Ready observations and 44 stale-visible observations, 100
completed seeks plus latest-wins supersession, zero unavailable/invalid terminal
facts, complete GPU timestamp evidence, maximum request-to-checkpoint
cancellation latency of about 2.14 ms, and passing Private Commit plateau plus
post-idle convergence. This closes the combined long-run contract on that one
development machine only. The source and report remain local and cannot become
canonical release evidence.

The generated-media gate additionally requires execution—not merely policy
state—when at least two pressure thresholds of requested-but-unengaged hardware
decode are observed: Playback Evidence must contain the corresponding Degraded
deliveries, and the headless GPU report must contain exact Full, Half, and
Quarter extents. This prevents a recovery state transition from passing while
decode/composite work remains at the original size.

The generated playback fixture declares limited-range BT.709 primaries,
transfer, and matrix metadata. A fixture without a quantization range must fail
the same closed color-contract boundary as user media; the harness must never
obtain a passing frame from an implicit swscale range guess.
