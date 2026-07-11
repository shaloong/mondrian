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
| Preview Scheduler | Frame Demand admission, priority, deadline, cancellation, prefetch, worker capacity | Clock Master or transport state |
| Preview Frame Store | ready/stale/in-flight identity, source revision, color contract, memory budgets | deadline policy or proxy selection |
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
observations, snapshots, Frame Demands, and Frame Deliveries.

## Runtime flow

```text
TransportCommand ─┐
MonotonicTick ─────┼──> Playback Engine ──> FrameDemand ──> Preview Scheduler
AudioObservation ──┤          │                                  │
FrameDelivery ─────┘          ├──> AudioDirective ──> Audio Engine
                              ├──> PlaybackSnapshot ──> UI
                              └──> PlaybackEvent ──> Evidence

Preview Scheduler ──> media decode ──> render/presentation ──> FrameDelivery
Audio Engine ──> device callback / synthetic availability ──> AudioObservation
```

The main event thread calls the Playback Engine. Decode/render/audio workers do
not mutate it; they return observations tagged with the session epoch.

## Core model

The eventual Rust names may change during implementation, but the semantic
Interface is fixed by this document.

### Playback Session

A session contains:

- monotonically increasing `epoch`;
- active `sequence_id` and immutable timeline revision/signature;
- exact `TimeCode` anchor;
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
- `Unavailable`: no sufficiently monotonic or stream-correlated observation.

Both usable grades must carry stream generation, sample rate, integer consumed
frames, observation timestamp, latency estimate, monotonicity status, and
uncertainty. Reports preserve the grade. A callback estimate may drive Alpha
playback only when its uncertainty remains inside the configured A/V budget; it
must never be reported as an exact hardware position. `Unavailable` selects
Synthetic Clock Master.

### Master selection

- A healthy audio stream with reliable consumed-sample evidence selects Audio
  Device Clock Master.
- No audio tracks may use Synthetic Clock Master directly rather than opening a
  meaningless silent device stream.
- Device open failure, loss, invalid sample position, or sustained callback
  failure switches to Synthetic Clock Master without discontinuity.
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

Small phase error is removed by bounded audio resampling/slew. Error outside the
hard budget keeps Synthetic Master active and reprimes audio; it must not jump
the timeline or duplicate/drop an arbitrary video interval.

### Time representation

- Timeline anchors use `TimeCode`/`Rational`.
- Monotonic duration is runtime-only and never persisted.
- Sample positions use integer frames plus sample rate.
- Conversion uses checked rational arithmetic at seams. Floating point may be
  used for filters/metrics but is not the authoritative accumulated position.

## Frame Demand

Each demand includes:

- session epoch and quality-policy revision;
- sequence/timeline revision;
- exact target TimeCode;
- access intent (`PlaybackCursor`, `ScrubCursor`, `StillFrame`);
- presentation deadline and demand sequence number;
- requested output extent/temporary resolution scale;
- proxy/original selection fixed by user/project policy;
- source/color/display contract revision;
- stale-frame permission;
- cancellation token scoped to epoch/demand.

The Engine emits demands based on clock position plus a bounded lookahead.
Prefetch is advisory, playback-only, slack-only, and cannot displace visible
current-frame work.

## Frame Delivery

Terminal outcomes are:

- `Ready`: correct demand result available before its deadline.
- `Late`: correct result completed after its deadline.
- `StaleAvailable`: a previously presented frame may remain visible.
- `Degraded`: an explicitly allowed temporary resolution or HDR-to-SDR path was
  executed and reported.
- `Blocked`: correctness/capability policy forbids presentation.
- `Canceled`: superseded epoch, demand, or latest-wins request.
- `Failed`: execution error not classified as a policy blocker.

Every outcome carries epoch, target position, actual source time, finish time,
decode/import/render/presentation path, source fingerprint, color contract,
quality revision, reason code, and stage durations.

Only the Playback Engine interprets a delivery:

- During Priming, one current Ready/allowed Degraded frame plus required audio
  readiness may start the clock.
- During Playing, Late/Canceled video is dropped while clock time continues.
- StaleAvailable preserves UI continuity but never counts as current readiness.
- Blocked enters Blocked when no allowed path exists.
- Repeated Late/Failed outcomes enter Recovering according to a sliding window,
  not a single-frame boolean.

## Quality and recovery policy

The Engine may automatically lower temporary preview resolution under sustained
pressure. The scale is runtime-only, monotonic within a recovery step, bounded
by a configured minimum, and restored only after a hysteresis window.

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
| minimum video priming | one current frame | stale does not satisfy it |
| normal play priming limit | 500 ms | then start the clock if no correctness blocker; late video may be absent/stale |
| seek priming limit | 750 ms | exact current frame remains highest priority |
| interactive control wake while priming | ≤100 ms | pause/seek/close remain responsive |
| recovery pressure entry | at least 8 late/failed current deliveries in the latest 12 | excludes canceled old epochs |
| resolution ladder | `1`, `1/2`, `1/4` | spatial scale only, working/color semantics unchanged |
| healthy recovery exit | 2 continuous seconds with ≥95% on-time current deliveries | ascend one ladder step at a time |
| audio observation invalidity | 100 ms or explicit device error | hand off to Synthetic Master |

If the priming limit expires with no correctness blocker, the session enters
Playing rather than freezing transport indefinitely. Audio Device Master starts
when audio is ready; otherwise Synthetic Master starts. The Viewer retains a
stale frame or explicit loading presentation until a current delivery arrives.
Blocked color, unsupported required format, or invalid timeline contracts do not
use this timeout escape.

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

Extract Preview Scheduler, Frame Store, and Evidence from `app_ui::preview` by
behavioral ownership, not file size. Retain one public request seam into media.

### Phase 5 — Realtime policy

Enable deadline-based drops and temporary-resolution recovery on the reference
machine. Preserve explicit proxy recommendation and fail-closed color policy.

### Phase 6 — Advanced transport

Only after forward `1x` gates pass, implement signed rational rate, reverse/J-K-L,
loop playback, variable-rate audio, and corresponding cache/decode policies.

Each phase must leave the product runnable, preserve report compatibility or
version it, and delete superseded state rather than maintaining two authorities.

## Current integration status

Phase 1 is complete, Phase 2 is active, and the first Phase 3 Clock Master slice
is integrated through the `mondrian-playback` crate. The pure Engine now owns
the app's transport position/state, Synthetic Clock Master, epoch invalidation,
exact rational clock advancement, stale-delivery rejection, and bounded
temporary-resolution recovery policy. `AppState` projects current frame and
running state from the Engine; the former app-local `PlaybackState`, frame
accumulator, reached-end flag, and misleading audio/video-master diagnostic have
been removed.

`app_ui::playback_feedback` now adapts Viewer lifecycle into typed terminal Frame
Deliveries. Loading remains non-terminal, Stale and Blocked remain distinct, and
duplicate terminal delivery identities cannot mutate recovery twice. The former
Viewer-owned `playback_buffering` state and its audio mute/clock hold have been
removed. Window redraw may still defer duplicate GPU candidate preparation while
Loading, but that presentation guard has no transport authority.

The Engine now emits a Frame Demand identity containing epoch, quality revision,
demand sequence, sequence/timeline revision, exact target, preview scale, and
monotonic deadline. Preview worker deadline budgets consume that demand instead
of independently reconstructing frame duration. Same-epoch completions for a
superseded demand are rejected before target validation.

Playback-current preview jobs carry the opaque identity projection (`epoch`,
quality revision, demand sequence, target frame) through queue, worker, result,
and app polling. Media workers do not interpret playback policy. Polling returns
an exact terminal Frame Delivery for Ready, Late, Failed, or Canceled work, and
the Playback Engine performs the final identity check. Media generation remains
a decode/cache cancellation mechanism and is not a substitute for Playback
Session identity. Viewer lifecycle feedback remains the Adapter for cache hits
and presentation-only outcomes that do not originate from worker completion.
Scheduler pending state retains the same identity until completion or
expiration. A stall expiration returns that stored identity and never asks the
host to synthesize a delivery from whichever demand is current at poll time.
Scrub expiration is capacity/cancellation evidence, not a playback Late Frame
Delivery.

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
within its versioned 20 ms uncertainty budget, anchors the first qualified
observation without a position jump, advances exact timeline frames from integer
sample deltas, and ignores monotonic UI ticks while Audio Device Master is
active. Stream failure, stale evidence, excessive uncertainty, or decreasing
sample position hands off continuously to Synthetic Master; repeated unavailable
observations while already Synthetic do not reanchor or freeze transport.

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
the exact media `TimeCode` at active-consumption frame zero; the Engine compares
latency-adjusted integer consumption with the published timeline. Error beyond
the versioned 20 ms budget records `PhaseRejected`, keeps Synthetic authoritative
without reanchoring, and asks the app Adapter to reprime. Accepted/rejected
generation and signed phase error are immutable snapshot evidence. Small-error
resampling/slew and backend-native hardware position remain Phase 3 work.

The same Audio Playback Module now owns the PCM render worker, current
generation, integer-sample next-window position, bounded in-flight admission,
460 ms high watermark, and preroll activation. `AppState` supplies only an
immutable timeline `AudioPcmRenderer` Adapter and consumes snapshots/events. A
headless fake output plus fake PCM renderer exercise the same Interface,
including exact window order, preroll activation, malformed-buffer silence
substitution, synchronous cancellation of queued old work, and rejection of the
at-most-one executing completion from an invalidated generation.
Environment variables no longer alter realtime audio watermark semantics.
