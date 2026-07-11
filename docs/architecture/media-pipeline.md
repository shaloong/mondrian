# Media Pipeline

`mondrian-media` owns FFmpeg-based media inspection, decode support, waveform/proxy/cache primitives, and audio buffers.

Realtime transport, Clock Master selection, Frame Demand deadlines, and the
interpretation of Frame Deliveries belong to the app Playback Engine described
in [Playback Engine](playback-engine.md). `mondrian-media` executes bounded
decode requests and reports facts; it does not pause or advance transport.

## Probe

`MediaInfo::probe(path)` uses FFmpeg format/codec metadata without decoding full media. It extracts:

- container and duration
- file size
- video streams: codec, dimensions, frame rate, pixel format, bit depth, alpha, detected color space, structured color interpretation, frame count
- audio streams: codec, sample rate, channels, layout, bit depth

The probe runs off the UI thread.

Asset registration is separate from metadata probing. `AssetLibrary` can import
a path by calling `MediaInfo::probe`, but callers that already own a bounded
probe result may register the media with that `MediaInfo` directly. This keeps
UI and performance harnesses from blocking on synchronous metadata analysis
when they need to isolate decode/access-mode latency, while preserving one
canonical asset-record write path.

Product media import is an app-level background batch, not a synchronous UI
action. `ImportMedia` and asset-panel import actions validate only cheap
preconditions on the event thread (library availability, target folder
existence), enqueue a `mondrian-media-import` worker, and return immediately.
The worker may call `AssetLibrary::import_media_file(...)`, which performs the
FFmpeg probe and asset-library write off the UI thread. `AppState` then polls
import completions during the normal background-task tick, publishes
`AssetImported`, applies proxy policy, updates status, and saves the project
once per completed batch. UI panels must not call `MediaInfo::probe` or
`AssetLibrary::import_media_file` directly from action handling, drag/drop, or
paint/layout code.

## Decode and Cache

Decoding and frame caching belong to media/renderer/export paths, not UI widgets. UI panels may request thumbnails or waveform data through app adapters, but must not own FFmpeg state.

The media decode layer exposes three access contracts, matching the way mature
NLEs separate playback, interactive navigation, and precise still extraction:

- `PreviewDecodeAccessMode::PlaybackCursor` is for sustained timeline playback
  and forward prefetch. It is mostly-forward, should keep decoder/session
  locality, and is the seam where hardware decode, low-copy P010/NV12
  residency, deadline/drop policy, and GPU input transforms belong.
- `PreviewDecodeAccessMode::ScrubCursor` is for latest-wins playhead dragging,
  jog, and shuttle. It prioritizes cancellation and seek latency over warming a
  long forward queue.
- `PreviewDecodeAccessMode::RandomAccessStillFrame` is for deterministic still
  extraction: thumbnails, poster frames, export fallback, diagnostics, and exact
  one-off requests.
  App thumbnail workers must pass the already-probed `PreviewFileFingerprint`
  into the still-frame request and use the same fingerprint for thumbnail cache and
  failure invalidation, so replaced files cannot reuse stale still-frame UI
  rasters. Decoded RGBA remains source-encoded: the app thumbnail adapter must
  resolve asset input color through the active sequence/project policy, execute
  the renderer source-to-working CPU reference stage, and cross an explicit
  working-to-sRGB display boundary before constructing a UI raster.

Every request also carries a required `PreviewSourceColorContract`: the
app-resolved input/source color space plus the ingest quantization range. CPU
decode resolves each YUV frame's matrix and range from decoder metadata, using
the request contract only for explicitly missing facts, and rejects conflicts,
unknown facts, and unsupported matrices such as BT.2020 constant luminance.
Before `sws_scale`, media configures `sws_setColorspaceDetails` with the exact
matrix, input range, and full-range RGBA output. FFmpeg/swscale defaults are not
part of Mondrian's color contract.

These contracts are media-layer interfaces. The current in-process adapter can
share the same CPU RGBA FFmpeg implementation while diagnostics and app
scheduling distinguish the requested access mode. Future hardware-resident
decode must specialize behind these contracts instead of adding app-layer flags
or treating playback as repeated random-access still decode. The generic
access-mode router is intentionally media-internal. Public callers enter through
one request seam: `PreviewDecodeRequest`. App preview owns worker lanes,
priority admission, current/prefetch cancellation, queue diagnostics, and
timeout reporting around that media request. Do not add mode-specific public
helpers or a second preview decode pool; they become compatibility debt and
split future hardware/low-copy routing across shallow wrappers.
`PreviewDecodeAccessMode` intentionally has no default value, and serialized
decode diagnostics must include it. Missing access-mode evidence is a diagnostic
coverage bug, not a reason to assume still-frame semantics.
Preview cache and in-flight identities must include the requested access mode,
source media path, file fingerprint, output dimensions, and source time in
microseconds. A bare timeline frame number is not a media identity: the same
frame index can represent different source times under different time bases,
and relink/proxy/source path changes must not reuse stale RGBA frames.
CPU `RgbaFrame` payloads are explicitly source-encoded RGB with straight alpha,
not implicit sRGB or working-linear pixels. Their applied YUV matrix/range and
source contract travel with the payload. Decode sessions, the playback ring,
and the process-global RGBA cache are isolated by that source contract because
they sit downstream of YUV-to-RGB conversion; interpretation changes may reuse
raw/native YUV resources, but must not reuse differently converted CPU RGBA.
Embedded ICC profiles likewise cannot manufacture that source contract. Media
ingest records a mapped ICC identity only when the shared core parser identifies
a supported named standard; generic RGB/GRAY profiles remain unmapped evidence
and enter the explicit missing-metadata policy.
App preview scheduling preserves playback cursor locality without letting idle
workers sit beside visible current-frame work. When more than one preview decode
worker exists, worker 0 has playback affinity and the remaining workers have
interactive affinities for scrub/still work. Those affinities are
work-conserving for `Current` requests: any idle worker may steal the highest
ranked current-frame job before taking lane-local background work. `Prefetch`
remains playback-only and lower priority than every current-frame request. With
only one worker, the lane is `Any` so all modes still make progress. Because
workers filter by lane, enqueue and priority promotion wake all preview workers,
not just one; otherwise a playback-only queue could wake an interactive worker
and leave the playback worker asleep until another request arrives.
Forward prefetch is a playback-only behavior. Settled still-frame preview and
active scrubbing must not enqueue `PlaybackCursor` prefetch work, because that
turns random access or latest-wins interaction into hidden background playback
decode and can keep project shutdown waiting on invisible media. In the app
preview scheduler, `MediaPreviewRequestPriority::Prefetch` is therefore valid
only with `PreviewDecodeAccessMode::PlaybackCursor`; non-playback prefetch
requests are rejected at admission and surfaced as structured diagnostics. The
worker transport queue repeats this invariant and derives priority only from
`MediaPreviewJob::priority`; queue callers must not pass a second priority value
that can drift from the job payload.
Playback forward prefetch is also slack-only. If visible current-frame media is
pending, current-frame work is waiting in the worker queue or already running,
or queued plus in-flight prefetch already covers the configured forward window,
the app must skip that prefetch pass instead of adding more speculative jobs.
Diagnostics report these as `prefetch_skipped_current_pending`,
`prefetch_skipped_worker_busy`, and `prefetch_skipped_prefetch_backlog`. This
keeps first-frame display and dropped-frame recovery ahead of cache warming on
slow or long-GOP media. The configured forward window is derived from a
wall-clock horizon and the active sequence frame rate, then capped before
enqueueing; high frame-rate playback warms more timeline frames than 24/25/30
fps playback without letting speculative work flood the bounded worker queue.
Preview diagnostics expose this playback-clock contract as structured
`playback_schedule` evidence, including the current-frame display deadline
budget, the prefetch horizon/window, and invalid frame-rate counters. Invalid
sequence frame-rate data must warn through diagnostics instead of silently
removing playback deadlines or cache warming.
When the prefetch backlog is below the forward window, scheduling must top up
only the remaining queued-plus-in-flight prefetch job budget across the
evaluated tracks and nested sequences, not enqueue a full new prefetch window
for each future frame offset.
The app preview service owns worker thread lifetimes. Service shutdown must be
non-blocking on the UI/event thread: it sets the shutdown flag, cancels queued
and in-flight scheduler generations, closes the job queue, and moves worker
handles to a background reaper that joins them after FFmpeg exits. A codec,
filesystem, or driver stall inside a preview worker must not prevent pause,
window close, or app quit from being processed. Each worker still explicitly
drops its thread-local media decode sessions before exit; thread-local FFmpeg
decoder state must not be left to implicit TLS teardown at project/app close.
The app scheduler lowers explicit `MediaPreviewAccessIntent` values to media
access modes. Viewer playback lowers to `PlaybackCursor`, active playhead/ruler
dragging lowers to `ScrubCursor`, and settled non-playing viewer frames plus
deterministic one-off work such as thumbnail stills lower to
`RandomAccessStillFrame`. The timeline widget emits explicit seek source
events, including a settled event on drag release, so preview scheduling does
not infer stillness from wall-clock timeouts. Playback pause, stop, project
switch, and natural end-of-playback transitions also settle the preview access
source before the next non-playing viewer request. New UI states must extend
that intent layer instead of passing booleans or strategy flags into
`mondrian-media`.
Playback cursor cancellation is cooperative but non-destructive to the playback
decode session: a prefetch budget miss should not throw away the warmed
mostly-forward decoder stream. Scrub and still-frame cancellation remain
session-destructive because those modes represent latest-wins seeks or exact
random-access work where stale decoder position is more dangerous than locality.
The playback decode session also owns a small forward RGBA ring. Ring hits are
strictly bounded by the same PTS tolerance as the process-global preview frame
cache and are reported as `PlaybackSessionRingHit`; they are not available to
scrub or still-frame requests. This keeps continuous playback locality inside
the media access-mode implementation rather than scattering playback caches
through app UI code.
Access-mode decode behavior is centralized in a media-layer policy, not in app
conditionals or FFmpeg call sites. Playback has the widest mostly-forward
session reuse window, the playback ring, and the exact-path forward decode
budget; scrub has only a very short forward reuse window, a smaller CPU
fallback forward-scan budget, no playback ring, and the low-latency
bounded-any seek strategy; random-access still extraction has no forward reuse
and keeps keyframe-safe exact seeking with the exact-path budget. Scrub
low-latency seek is product semantics, not an opt-in environment variable. This
preserves still-frame correctness while preventing latest-wins scrubbing from
spending the same long-GOP CPU budget as deterministic extraction, and leaves a
clear replacement point for future hardware-resident playback and low-latency
scrub backends.
Every preview decode diagnostic emitted by `mondrian-media` must carry the
resolved access-mode policy contract alongside the observed result:
`seek_strategy`, `forward_reuse_frame_window`,
`forward_decode_budget_frames`, and `any_seek_window_ms`. App/UI performance
reports may aggregate those fields, but must not reconstruct them from app
conditionals. This keeps policy bugs diagnosable: for example, a scrub sample
that reports `BoundedAnyFrame` with a zero `any_seek_window_ms` is a broken
media contract, not a UI presentation issue.
Each in-process preview decode session also maintains a session-local,
incremental keyframe seek index from video packet metadata observed during real
decode work. The index may bound later seeks to an already-known keyframe
anchor, but it must not perform a blocking whole-file scan on first frame or
pretend that unknown GOP structure is known. This is a CPU fallback bridge
toward a real GOP/keyframe map: future probe-backed indexes and hardware
decode session adapters should replace the evidence source behind the media
request boundary, while preserving the same diagnostics for availability,
observed keyframes/packets, and whether a seek actually used an index anchor.
App, export, and thumbnail callers submit a `PreviewDecodeRequest` to the
media preview decode boundary instead of matching on `PreviewDecodeAccessMode`
or calling mode-specific FFmpeg helpers. Access-mode routing, session
retention, cache lookup, playback-ring use, and future hardware/low-copy
backend selection must stay behind the request boundary in `mondrian-media`.
The request may carry a `PreviewHardwareDecodeRequest`, but this is only caller
intent: `Auto` means use the media default, `PreferHardwareDecode` means prefer
FFmpeg hardware decode even when decoded frames must transfer back to CPU RGBA,
`PreferGpuResident` means prefer a native GPU-resident decoded surface when the
access mode/backend can provide one, and `RequireGpuResident` means fail closed
instead of silently returning a CPU RGBA frame. The app playback scheduler may
use `PreferHardwareDecode` for `PlaybackCursor` while renderer native import is
not ready, then upgrade to `PreferGpuResident` only after renderer/platform
native decoded-frame import admission succeeds. Scrub and still-frame work
should remain `Auto` unless a future backend explicitly supports those access
patterns.
Preview decode diagnostics also carry decoder payload sampling facts:
`DecodedVideoSampling` records encoded range, chroma location, and effective
bit depth observed on the decoded FFmpeg frame. These values are media facts,
not color-management interpretation. `mondrian-media` must not translate them
into renderer sampling contracts or infer missing values from platform defaults.
The app layer combines them with the resolved source color space when evaluating
native decoded-frame import readiness; missing or unsupported sampling remains a
structured blocker instead of silently falling back to guessed NV12/P010 shader
constants.
App preview decode execution must run synchronous FFmpeg preview decode on
dedicated preview worker threads, not on the UI/event thread. Current-frame and
prefetch workers pass a cooperative cancellation predicate into
`PreviewDecodeRequest`, and the media loop checks that predicate before
open, seek, packet decode, frame receive, EOF drain, and RGBA conversion. Do
not depend on thread abort to preempt synchronous packet decode.
Viewer preview readiness feeds back into the app playback clock. While the
current playback frame is still `Loading` or only a stale frame can be shown,
the app enters a short buffering state, holds the video clock, and mutes/clears
realtime audio output until the preview becomes current again. This prevents a
slow CPU fallback decode from chasing ever-newer playback frames and dragging
the UI through unbounded obsolete work. Future hardware playback may replace
this with stricter clock-driven frame dropping, but it must preserve the same
readiness feedback contract.
Buffering must not make the UI event loop behave like normal frame playback.
While the clock is held, playback wakeups are throttled to a low-frequency
health tick instead of the sequence frame cadence. Transport actions such as
play, pause, toggle-play, stepping, and seek must refresh controls and timeline
state without synchronously requesting viewer preview/composite work; preview
catch-up is driven by worker completion, cache state, and later render ticks.
This keeps pause, close, and other shell input responsive even when a 4K
Long-GOP decode or GPU-preview blocker is still unresolved.
The app/window layer owns the interactive escape hatch. If the host playback
state is buffering, redraw handling must not synchronously re-enter GPU preview
candidate construction just to repaint controls; it records a loading skip and
lets worker completion or a later state change drive the next preview prepare.
This gate must use the app playback state, not the viewer model, because
lightweight transport refreshes intentionally avoid preview and may not preserve
`viewer_preview_waiting`. The event loop also caps buffering wake delay to an
interactive budget so status ticks and shell input stay responsive while media
workers continue in the background.
Background polling must also enforce a hard escape hatch for the current
realtime playback request. If a scheduler-accepted `Current` request for
`PlaybackCursor` or `ScrubCursor` remains pending past the buffering stall
budget, the app preview service expires only that realtime-current pending work,
removes matching queued worker jobs, records
`playback_current_stalled_expirations`, clears the current-frame pending flag,
and lets the host release playback buffering through the preview-free transport
path. This is not a renderer fallback and must not clear ready/stale frames,
still-frame work, media caches, or external GPU viewer textures. It exists so a
lost, wedged, or pathologically slow current decode cannot hold pause/close UI
behind "preview buffering" indefinitely.
The same counter must appear in the preview decode performance summary/report
even before a worker returns a canceled decode result, because the product
symptom is already user-visible at the moment buffering is released.
Each expired realtime-current request released this way must also count as a
playback late-drop decision and a proxy/hardware recommendation decision so
clock-driven playback diagnostics do not under-report frames that were skipped
before a worker produced a cancellation result.
Consecutive current playback late drops form a sustained playback pressure
state. Once the app preview service observes the pressure threshold, it must
suppress forward playback prefetch and keep worker/queue capacity available for
the visible current frame. The state exits only after a successful `Current`
`PlaybackCursor` result. This recovery path must be structured diagnostics, not
opaque logging: reports include the late streak, pressure entries, recoveries,
and prefetch skips. It must not silently enable proxies, lower decode quality,
or choose hardware decode outside the project/backend policy; those are
separate recovery decisions layered behind the same playback controller.
Performance gates must include real-media playback fixtures, not only synthetic
generated clips. The ignored
`preview_media_external_continuous_playback_smoke` test accepts
`MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_MEDIA_PATH` (or the shared
`MONDRIAN_PREVIEW_EXTERNAL_MEDIA_PATH`) so CI or a local workstation can run
4K HEVC Main10, 4K H.264, HDR PQ/HLG, and Long-GOP camera samples through the
same playback decode/render/color report contract. A sustained playback
pressure root cause is a playback smoke failure, not merely advisory evidence.
The external real-media variant also emits `real_media_gates` and fails on
`PlaybackCursor` decode p95, queue-wait p95, or visible-frame-ratio regression;
those gates must remain separate from broad timeout windows so slow-but-eventual
4K playback is not mistaken for production readiness.
When background preview completion changes the viewer waiting state, the app
host may perform one preview-aware model refresh for the completed work, but
the derived playback-buffering flag must be propagated with a transport/status
refresh that does not request preview again. A buffering-state transition must
not trigger a second full root refresh or layout pass that re-enters preview
interpretation. The same rule applies when playback-clock advancement discovers
that the new visible frame is still waiting on preview decode: the current-frame
refresh may consult preview once, but buffering controls/status must then update
through the lightweight transport path.
The native app event loop also records stage-level responsiveness telemetry for
action draining, redraw, GPU preview preparation, UI refresh, paint/render,
background-task polling, and playback-clock advancement. Any stage that exceeds
the UI responsiveness budget is logged with the stable stage name and elapsed
time. This diagnostic boundary is intentionally in the app layer because it
measures host scheduling and UI-thread residency, not media decode semantics;
media/render changes must preserve it so a future buffering report can identify
whether the stall is decode backlog, GPU preview preparation, redraw/render, or
control dispatch.
Completed preview decode results are also consumed under an explicit UI-thread
budget. The preview service may process only a bounded number of completions per
poll and must yield once the completion-drain time budget is reached, requesting
a follow-up tick instead of monopolizing the event loop. Diagnostics must expose
completion poll calls, drained results, count-budget exhaustions, time-budget
exhaustions, and poll durations. This keeps worker bursts, cache insertion, and
decode diagnostic aggregation from delaying transport controls or close/quit
events during buffering.
The same event-loop rule applies to app-owned thumbnail and waveform completion
queues consumed by `AppUiHost::poll_background_tasks`: they may request another
tick when backlog remains, but they must not drain an unbounded worker burst on
the UI thread.
App preview decode timeout is access-mode-specific, not a single global
playback policy. `ScrubCursor` has the shortest caller-release budget because
interactive latest-wins work must not leave the UI waiting behind pathological
seeks. `RandomAccessStillFrame` may wait longer because exact still extraction
is deterministic one-off work. `PlaybackCursor` sits between those modes and
relies on prefetch cancellation and session locality for sustained playback.
Diagnostic environment overrides may tune or disable these watchdogs, but they
must preserve the per-access-mode structure rather than reintroducing one
opaque timeout for every request.
Timeouts are first-class decode failures, not string-only log messages.
App preview diagnostics emit timeout evidence with the asset id, requested
access mode, timeout budget, frame number, and source timestamp. Diagnostics
must separate `decode_timeouts` from aggregate `decode_failures`, and preserve
timeout counts per access mode. A timeout should therefore point directly at the
failing access contract (`PlaybackCursor`,
`ScrubCursor`, or `RandomAccessStillFrame`) instead of only showing a generic
"decode failed" counter.
Forward-scan budget exhaustion is also a structured decode failure. When a
media-layer access policy hits its forward decode frame budget without finding
an acceptable frame, the media crate must return a typed
`DecodeBudgetExhausted` error containing the requested access mode, decoded
frame count, budget, and target PTS. App preview diagnostics must aggregate
these failures globally and per access mode, and perf reports must surface a
budget-exhausted root cause instead of folding the event into an opaque decode
error or pretending the sample merely timed out. App preview reports must also
carry per-access-mode buckets for decode failures, timeouts, and budget
exhaustion, so engineers can diagnose whether pressure is coming from
`PlaybackCursor`, `ScrubCursor`, or `RandomAccessStillFrame`.
The app preview scheduler stores access mode alongside the media-frame key for
pending/in-flight work. A later scrub/current request for the same media frame
must supersede an older playback/prefetch request instead of letting the older
job complete and remove the pending scrub work.
In the app layer this contract lives in the `preview_access_mode` module:
request admission, latest-generation tracking, access-mode promotion, and
completion classification are localized there. The same module owns the bounded
preview decode job queue and worker-lane selection, because those policies are
defined by access mode. The module must not own render plan evaluation, decode
execution, color interpretation, or GPU/CPU frame conversion; those remain in
the preview orchestrator and media/renderer layers.
Worker-lane selection reserves CPU capacity instead of maximizing raw decode
throughput. The media layer exposes a single `PreviewDecodeCpuBudget` that
coordinates app preview worker count with FFmpeg decoder threads per worker.
The app scheduler uses that budget for lane count, while the media decoder uses
the same budget for its default FFmpeg threading request. This avoids the
dangerous `preview workers * FFmpeg decoder threads` over-subscription pattern
that can make software decode starve UI input, audio, and render submission.
Single-worker systems use one `Any` lane; mid-range systems use separate
`Playback` and non-playback `Interactive` lanes; systems with enough
parallelism split `Playback`, `Scrub`, and `Still` lanes so exact still-frame
requests cannot sit ahead of active playhead dragging, and playback prefetch
cannot consume the only interactive decode lane. Preview diagnostics expose the
resolved CPU budget and the actually started worker count so perf reports can
distinguish codec cost from scheduling over-subscription.
Preview completion has separate display and cache semantics. A decode result is
`Current` only when it still matches pending visible work; same-generation
results whose pending request was canceled or whose access mode has been
superseded may be `CacheOnly`, but must not wake the viewer as the current
frame or remove the newer pending request. Obsolete-generation results are
`Stale` and must not populate success/failure caches.
Queued current-frame work is latest-wins for playback and scrubbing. Before a
new current frame is enqueued, obsolete queued jobs from older generations are
removed regardless of priority so old current jobs cannot fill the bounded queue
and cause the visible current frame to be dropped.
`RandomAccessStillFrame` is the lowest real-time current-frame class. It may
spend more time to produce deterministic still output, but it must not block
active playback or interactive scrubbing when the pending window or worker
transport queue is full. Scheduler admission and the job queue may evict queued
still-frame current work for `PlaybackCursor` or `ScrubCursor` current work;
they must not let still-frame work evict those real-time modes. On a shared
interactive worker lane, `ScrubCursor` jobs are selected ahead of still-frame
jobs even when the still-frame request arrived first. A newly admitted
`PlaybackCursor` or `ScrubCursor` current request also preempts already-pending
still-frame current work before the pending window or worker transport queue is
full; the app must cancel matching queued jobs immediately so deterministic
still extraction cannot occupy decode capacity while realtime interaction is
waiting. If still-frame work is already running in a worker, the app must keep
its scheduler pending entry until the worker observes cancellation; otherwise
the decode would be mislabeled as obsolete instead of a realtime preemption. If
that running still-frame work sees a different realtime current-frame request
pending, the still-frame decode must cooperatively yield and report a structured
still-preempted-by-realtime-current cancellation. Another still-frame request
alone must not trigger that preemption.
If an existing queued prefetch for the same media key becomes current-frame
work, queue promotion must refresh the queued job's access mode, generation,
source timing, and enqueue timestamp. The promoted job should be measured as
current-frame queue wait from the promotion point, not from the earlier
speculative prefetch enqueue.
Scheduler diagnostics keep aggregate skip/drop/stale counters plus reason
breakdowns for missing pending work, access-mode mismatch, obsolete generation,
obsolete request generation, and pending-window backpressure. Access-mode
failures must be diagnosable without inferring from one opaque skipped count.
When scheduler admission evicts prefetch or still-frame pending work to admit
real-time current-frame work, the admission result must return the evicted media
keys. The app layer must immediately cancel matching jobs from the worker
transport queue so already-obsolete work does not sit in the bounded queue until
a worker later discovers the missing pending request. Queue mutations that
remove jobs (`clear`, obsolete-generation pruning, and key cancellation) must
wake waiting lane workers just like enqueue, promotion, and close; otherwise a
worker can remain parked on a stale queue state until unrelated work arrives.
The decode performance summary/report carries the same scheduler diagnostics and
emits stable Scheduling root causes for access-mode mismatch, obsolete
generation churn, and pending-window backpressure.
The app worker transport queue is diagnosed separately from scheduler
admission. `queue_full_drops` and `worker_disconnected_drops` are hard failures
because they mean scheduler-accepted work did not reach a preview worker.
`queue_evicted_prefetch_jobs`, `queue_canceled_jobs`,
`queue_evicted_still_jobs`, `queue_pruned_obsolete_jobs`, and
`queue_promoted_current_jobs` are evidence fields: they should explain how the
system protected real-time current-frame work and kept the worker transport
queue aligned with scheduler cancellation, not be folded into opaque
backpressure. Diagnostics must also expose the current worker transport queue
depth split by priority and access mode (`queued_current_jobs`,
`queued_prefetch_jobs`, `queued_playback_cursor_jobs`,
`queued_scrub_cursor_jobs`, and `queued_random_access_still_jobs`) plus queued
expired playback-current work (`queued_expired_playback_current_jobs`) so a
slow preview report can distinguish active queue backlog, missed display
deadlines, and codec/decode cost without inspecting private queue internals.
Queued expired playback-current work is an active scheduling warning, not just
passive evidence: if a playback frame has already missed its display deadline
while still sitting in the worker queue, the report must point at the
clock-driven decode/drop/proxy decision boundary rather than blaming generic
decode latency. Worker dequeue must not dispatch such expired playback-current
jobs into normal decode; it should emit a structured dropped-expired outcome so
the app can complete the scheduler request as `PlaybackDeadline` while avoiding
decode/session work. Diagnostics must expose both queued expired work and
cumulative dropped-expired work.
The app preview layer must also expose worker-lane eligibility for the same queued jobs
(`queued_playback_lane_eligible_jobs`, `queued_scrub_lane_eligible_jobs`,
`queued_still_lane_eligible_jobs`, and
`queued_interactive_lane_eligible_jobs`) so reports can distinguish a backlog
that has an idle compatible lane from one waiting behind an occupied or missing
lane. The UI/report layer must consume these queue diagnostics rather than
recomputing lane acceptance from access-mode conditionals.
In-flight worker activity is exposed separately and split by the same priority and access-mode
contracts (`in_flight_current_jobs`, `in_flight_prefetch_jobs`,
`in_flight_playback_cursor_jobs`, `in_flight_scrub_cursor_jobs`, and
`in_flight_random_access_still_jobs`) so diagnostics can separate queued backlog
from workers actively occupied by playback, scrub, or exact still-frame decode.
The same activity snapshot must include worker-lane occupancy and
`in_flight_cross_lane_current_jobs`. Cross-lane current-frame work is allowed as
visible-work overflow, such as an idle playback lane helping a scrub current
frame before playback prefetch, but persistent cross-lane evidence means the
worker split or software-decode budget is too tight and should be tuned before
blaming codec throughput, color conversion, or GPU upload.
It also carries per-access-mode decode profiles for playback, scrub, and
random-access still requests: frame counts, cache/ring/source path counts,
end-to-end duration totals/maxima, worker-queue wait totals/maxima, seek counts,
session-local seek-index availability/use, decoded-frame pressure, and
stage-level timings. A slow preview report must identify the slowest access mode
so engineers can distinguish playback locality failures from scrub seek latency,
missing GOP/index evidence, queue-lane contention, or exact still-frame random
access costs.
Slowest-frame evidence must stay frame-local. `max_frame_stage_durations`,
`max_frame_queue_wait_us`, and `max_frame_bottleneck` are captured from the same
successful decode result; `queue_wait_max_us` remains an independent worker
pressure counter and must not be mixed into the slowest-frame bottleneck. This
prevents a codec-bound frame and an unrelated queued frame from being reported
as one impossible root cause.
Each access-mode profile must also carry compact fixed latency buckets for
successful decode duration and worker-queue wait. Reports derive a p95 upper
bound from those buckets and emit per-mode p95 checks. This is intentionally a
bounded diagnostic approximation, not an exact retained sample list: the JSONL
should show whether real-media stalls are sustained across most frames or just
single-frame spikes without growing unbounded UI telemetry state.
The versioned report must emit access-mode-specific latency checks and root
causes, so perf tooling can fail on `PlaybackCursor`, `ScrubCursor`, or
`RandomAccessStillFrame` regressions without reverse-engineering raw counters.
Perf smokes must gate queue-wait regressions for the access modes they exercise,
because queue-lane contention can make the viewer feel stuck even when codec
decode and color/render work are within budget.
Perf smokes that claim access-mode coverage must pass their required access
modes into the preview decode report builder. The report JSON must then include
`required_access_modes` plus pass/fail coverage checks for each required mode,
and the smoke validator should read those checks instead of reimplementing a
parallel coverage model. Playback-specific validators must surface missing
`PlaybackCursor` coverage as a first-class failure, not only as a generic failed
decode report. General UI diagnostics may leave the required list empty; an
idle or partial user session should not fail merely because it did not exercise
every access contract.
Media preview smoke validators must fail on both decode and post-decode render
report failures, and their error codes should include failing check/root-cause
codes before the generic failed-report code. A real sample whose UI cases fit
their broad wall-clock window can still be unacceptable if access-mode p95,
packet decode, or viewer output-boundary diagnostics exceed the frame budget.
The same computed failure-code arrays must be serialized in the smoke JSON so
CI dashboards and manual runs can inspect failures without parsing panic text.
App media preview smokes must generate real samples for both active
`ScrubCursor` playhead dragging and settled `RandomAccessStillFrame` requests;
coverage is incomplete if the report merely defines both profiles. A common
preview media smoke must fail when either access mode has zero successful
profile samples. It must also fail when a required access mode is represented
only by process-global `PreviewCacheHit` samples, because a cross-mode cache hit
does not prove that mode's FFmpeg/session policy actually ran. Playback
session-ring hits count as mode-local playback evidence; the process-global
preview cache does not.
Generated-fixture and external-real-media smokes must share the same
access-mode probe and validation helpers. The external path exists to run 4K
HEVC/HDR and camera-original samples through the exact same `ScrubCursor` and
`RandomAccessStillFrame` gates, not to create a looser ad hoc benchmark.
Playback diagnostics must also expose session reuse and forward reuse evidence.
If playback source decodes repeatedly open sessions or never hit forward reuse,
ring reuse, or cache reuse, the report should flag playback locality separately
from generic codec/GOP pressure.
App-level decode cancellation is also structured before it reaches diagnostics:
workers classify cancellations as shutdown, obsolete pending work, prefetch
deadline, or unknown, and aggregate them by requested access mode. The media
decode predicate remains a boolean so FFmpeg adapters do not learn app/UI
scheduler semantics, but the worker result must preserve the app-level reason
for telemetry and performance reports. Access-mode profiles must carry the
reason breakdown for their own cancellations, so obsolete or unknown cancel
pressure can be attributed to `PlaybackCursor`, `ScrubCursor`, or
`RandomAccessStillFrame` without comparing independent totals.
Canceled decode jobs also carry worker execution duration. A cancellation that
arrives quickly but only returns after an expensive FFmpeg open/seek/decode/copy
step is still a user-visible scheduling failure. Preview reports therefore keep
total/max/last canceled-worker duration globally and per access mode, and emit a
slow-cancellation root cause when the full canceled worker duration exceeds the
slow-frame budget. Workers must also record the elapsed time of the first
observed cooperative-cancellation checkpoint and report the return latency from
that checkpoint to the completed worker result. This is not the true external
request timestamp; it is the first point where decode code proved that the
cancel predicate was observed. A separate slow-cancel-return root cause fires
when this post-observation return latency exceeds the slow-frame budget, which
keeps FFmpeg open/seek/decode work, frame copy work, and cleanup/return work
diagnosable as different scheduling failures. This evidence belongs in the app
scheduler/worker layer because the media layer only owns cooperative
cancellation checkpoints, not UI intent or cancel reasons.
Process-global decoded-frame cache hits are capped to the same strict frame-hit
tolerance for every access mode. Playback performance must come from the
playback cursor's decoder/session locality, ring buffers, hardware decode, and
GPU-resident frame delivery, not from silently reusing adjacent timestamp
requests as if they were the requested frame.

Current decode residency is intentionally explicit and fail-closed. CPU paths
produce `RgbaFrame`; admitted in-process FFmpeg D3D11 playback may instead
produce `PreviewNativeDecodedFrame`. Legacy unowned YUV preview surfaces remain
removed: native output is an owned decoder-resource lease, not a raw plane
container or a handle-kind diagnostic.
`PreviewHardwareDecodeDecision` records the media-layer selection for each
request. A GPU preference must resolve to a structured CPU RGBA reason such as
`CpuRgbaHardwareUnavailable`, `CpuRgbaAccessModeUnsupported`,
`CpuRgbaBackendUnavailable`, `CpuRgbaCodecUnsupported`,
`CpuRgbaBackendBoundary`, or `HardwareDecodeCpuTransfer` until the selected
decoder actually produces a native surface admitted by the caller's combined
renderer/platform capability check.
`HardwareDecodeCpuTransfer` means in-process FFmpeg hardware decode was
configured and hardware frames are transferred back to CPU before RGBA preview;
it is active hardware decode, but it is not zero-copy, GPU texture residency, or
a native renderer import contract. `GpuResidentNative` is valid only when the
decoder probe reports active hardware decode, zero-copy/GPU texture residency,
and a native handle kind. Renderer/platform readiness is a separate app
admission fact. External FFmpeg CPU RGBA
is always a backend boundary, even if the CLI used platform hwaccel internally.
`HwAccelProbe` reports the selected hardware backend, decoded frame residency,
`hardware_decode_active`, `zero_copy_active`, optional
`decoded_gpu_frame_handle_kind`, and a stable reason string. It also reports the
platform candidate backend list in priority order,
the candidate backend selected for the current stream plan, the candidate native
handle kind, and preferred native surface formats such as P010/NV12. Candidate
fields are planning evidence only. On Windows the ordered list must prefer
D3D12VA/ID3D12Resource before D3D11VA/ID3D11Texture2D, with legacy
DXVA2/IDirect3DSurface9 only as a lower-priority CPU-transfer fallback. If
FFmpeg, the codec, or the local device cannot use D3D12VA, playback admission
may fall through to D3D11VA and then DXVA2. On macOS the candidate is
VideoToolbox/CVPixelBuffer. On Linux the ordered list must prefer
VA-API/DMABUF-style surfaces before legacy VDPAU. Until D3D12VA/D3D11VA,
VideoToolbox, VA-API, VDPAU, DXVA2, or CUDA/NVDEC hardware frames are actually
exported/imported through the renderer native decoded-frame import contract,
`HwAccelBackend::probe()` must keep `selected_backend=None`,
`decoder_adapter_available=false`, `hardware_decode_active=false`,
`zero_copy_active=false`, `DecodedFrameResidency::CpuRgba`, no active GPU
handle kind. Platform preference alone is
not a valid hardware decode signal. The first concrete media adapter supports
FFmpeg's preferred `AV_PIX_FMT_D3D11` frame ABI. It does not make D3D12VA,
legacy `AV_PIX_FMT_D3D11VA_VLD`, DXVA2, VideoToolbox, VA-API, VDPAU, or CUDA
native automatically; GPU-resident planning skips backend candidates without
a matching concrete media adapter.
For a concrete video stream, media may also run a read-only FFmpeg hardware
codec config probe with `avcodec_get_hw_config`. That probe records whether the
linked FFmpeg build lists the candidate hardware device type, whether the
stream codec has a decoder, whether that decoder advertises a matching hardware
config, the advertised hardware pixel format, and the setup methods
(`hw_device_ctx`, `hw_frames_ctx`, `internal`, or `ad_hoc`). This probe must not
create an `AVHWDeviceContext`, change decoder format negotiation, allocate
hardware frames, or report active hardware decode. Its purpose is to separate
"FFmpeg/codec cannot use this backend" from "Mondrian has not connected the
decoder adapter yet".
When the codec config is present and hardware decode was requested for playback,
media may run the cached FFmpeg hardware device-context probe. That probe calls
`av_hwdevice_ctx_create`, immediately releases the returned `AVHWDeviceContext`,
and records whether device creation was attempted, succeeded, or returned an
FFmpeg error code. It must be cached per backend for the process lifetime so
session planning does not repeatedly initialize GPU drivers. Playback sessions
may attach a fresh retained `AVHWDeviceContext` to an unopened FFmpeg decoder
and install a get-format callback that accepts only the advertised hardware
pixel format. `PreferHardwareDecode` always permits materializing hardware
frames through `av_hwframe_transfer_data` into CPU frames before RGBA scaling.
`PreferGpuResident` permits the same diagnosed fallback if native
materialization fails. `RequireGpuResident` configures the hardware decoder but
does not permit CPU transfer or software-frame fallback. A failed device-context
probe is `CpuRgbaHardwareUnavailable`; a
successful hardware decode session that still transfers frames to CPU is
`HardwareDecodeCpuTransfer`; a future zero-copy adapter that cannot import into
the renderer should use renderer/platform import diagnostics instead of this
CPU-transfer state.
The temporary FFmpeg hardware CPU-transfer fallback must report a structured
`PreviewHardwareDecodeCpuTransferStatus`: `NotAttempted`,
`ConfiguredAwaitingFrame`, `SetupFailed`, `DecoderOpenFailed`, or `Observed`.
Setup/open failures must not disappear into trace logs or generic software
decode counters. They remain a diagnostic fallback state only; `Observed` means
hardware frames were transferred back to CPU, not that Mondrian achieved
GPU-resident playback.
Playback hardware-decode admission is an app-layer aggregation contract, not a
media, renderer, or platform responsibility. The host combines renderer native
decoded-frame import support, OS native texture import probing, and preview
scheduler policy into `AppUiPreviewHardwareDecodeAdmissionDiagnostics`. That
diagnostic must include the playback request, whether renderer support is
known/ready, renderer-supported handle and source-format counts, platform
discovery/zero-copy/low-copy facts, and a stable `admission_blocker` enum such
as `RendererImportUnavailable`, `PlatformDiscoveryUnavailable`,
`PlatformCopyPathUnavailable`, or `PlatformHandleUnsupported`. The scheduler
may request `PreferGpuResident` only when renderer import and platform import
are both ready for a shared handle family. Otherwise playback may request
`PreferHardwareDecode` to use FFmpeg hardware decode with CPU-transfer fallback,
and the performance report must still identify the precise native-import
admission blocker. Do not report FFmpeg hardware CPU-transfer as the production
GPU-resident playback path.

Preview path resolution is proxy-aware but does not synchronously generate
proxy media. `mondrian-media::ProxyGenerator` owns the shared proxy freshness
contract through `ProxyStatus` (`Missing`, `Fresh`, `Stale`). If project proxy
playback is enabled for an asset and the expected proxy file is `Fresh`, app
preview decodes that proxy path. If the proxy is `Missing` or `Stale`, preview
falls back to the source path and records proxy hit/miss/stale counters in
`AppUiPreviewDiagnostics`. Export continues to use the source/export contract;
proxy selection is a preview playback scheduling decision, not media color
interpretation.
Newly imported video assets enter proxy playback and start background proxy
generation only when the project `ProjectSettings.proxy_enabled` policy is on.
The project policy is the scheduling source of truth; app/UI preferences must
not independently enable proxy generation against a project that has disabled
proxy workflows.
Proxy generation and preview proxy-path resolution must use the same
project-derived `ProxyConfig`: `ProjectSettings.proxy_resolution` selects the
proxy height preset, and `ProjectSettings.cache_dir` places proxy media under
that cache root's `proxy/` directory when configured. Callers must not use
`ProxyConfig::default()` for project media scheduling because that would split
generation and playback lookup across different cache roots or resolutions.
They must also use the same versioned `ProxyColorContract`. The app resolves
that contract from asset interpretation, ingest detection, and the active
missing-metadata policy before it asks media code to locate or generate a
proxy. A rejected input-color decision rejects proxy generation; it must not
be replaced with an implicit sRGB or Rec.709 assumption.
Proxy artifacts are source-referred optimized media. Their identity includes
the effective source color space, source precision, encoded range, spatial
settings, quality setting, and concrete encoding profile, but excludes
timeline working space and monitor/display transforms. Every completed proxy
has a versioned `.color.json` sidecar containing that contract and an exact
source file fingerprint. `ProxyStatus::Fresh` requires both the proxy and an
exactly matching, parseable sidecar; file modification ordering alone is not
proof of color or source identity.
`ProxyCodec::Auto` selects H.264 High 8-bit only for ordinary 8-bit SDR and
selects H.265 Main10 for HDR, camera-log, or greater-than-8-bit sources.
Explicit H.264 requests for high-precision sources fail closed. H.265 Main10
and DNxHR HQX preserve a 10-bit proxy boundary, while standardized source
spaces emit canonical FFmpeg primaries/transfer/matrix tags. Camera-log spaces
do not receive guessed delivery tags: their sidecar contract remains
authoritative. Unknown source range remains an explicit `Unknown` contract
at ingest, but it is not a valid proxy-generation contract. `ProxyColorContract`
is valid by construction only for explicit `Limited` or `Full` range and a
supported 8-16 bit source precision; deserialized contracts are revalidated at
every generator entry point. Proxy generation therefore fails closed instead
of allowing FFmpeg to infer range.
Ingest persists decoder `color_range()` on `VideoStreamInfo`, so ordinary
probed assets propagate `Limited` or `Full` into the proxy contract. `Unknown`
is reserved for genuinely unspecified metadata, decoder-unavailable records,
and older serialized asset records loaded through the explicit serde default.
The FFmpeg proxy filter graph declares frame metadata with `setparams`, then
uses matching `scale` `in_range`/`out_range` values. The encoded output also
carries an explicit `color_range` plus canonical CICP tags when the source
identity has trustworthy standardized tags. Contract/manifest v2 invalidates
older artifacts generated under implicit range behavior.
Generation must also use `ProxyStatus`: a `Fresh` proxy is reused, while a
`Stale` proxy is regenerated in the background. Failed regeneration must not
delete the previous proxy file, because preview can keep falling back to source
until a fresh proxy is finalized.
`ProxyConfig.concurrent_jobs` is an execution contract, not a UI preference:
`mondrian-media` must limit expensive FFmpeg proxy transcodes per proxy cache
root before launching the transcode work. Fresh proxy reuse does not consume a
transcode slot. App code may schedule proxy requests, but it must not bypass the
media-layer limiter when starting background generation.
The app layer must enqueue proxy generation requests onto a shared background
dispatcher instead of creating one OS thread/runtime per asset. Dispatcher
workers are allowed to keep proxy requests moving, but expensive transcode
parallelism remains owned by the media-layer `ProxyConfig.concurrent_jobs`
limiter so batch imports cannot starve preview playback, UI, or export work.
`MultiLevelCache` must not weaken this contract: L1 memory hits and L2 proxy
index hits are valid only while the referenced proxy still resolves to
`ProxyStatus::Fresh`. A cached source fallback must be re-evaluated when a
fresh proxy later appears so proxy generation can actually improve playback
without requiring an app restart or manual cache clear.

`DecodedGpuFrameHandleKind` belongs to media because it describes the decoder
surface family that FFmpeg/hardware decode produced, such as D3D12 resource,
D3D11 texture, legacy DXVA2 surface, CVPixelBuffer, VA-API surface, legacy
VDPAU surface, or CUDA device memory. It does not imply that the renderer can
import or sample that handle. Platform capability discovery is reported
separately by `mondrian-platform-core` as native texture import support, and
renderer readiness is reported by
`mondrian-renderer` through
`GpuNativeDecodedFrameImportSupport` / `GpuNativeDecodedFrameImportPlan`. App
code must not infer zero-copy playback from the media handle kind alone.
On Windows, platform capability discovery performs real D3D12 and D3D11 device
probes and reports `D3D12Resource` and/or `D3D11Texture2D` with a low-copy
staging fallback when those device probes succeed. That is OS/device evidence
only: zero-copy remains false until the renderer exposes a native import
backend, and CPU-transfer hardware decode must still report CPU RGBA residency.

The preview decoder's experimental external-process path is named
`PreviewDecodeBackend::ExternalFfmpegCpuRgba` and is enabled only by explicitly
selecting that backend or setting `MONDRIAN_PREVIEW_EXTERNAL_FFMPEG_CPU_RGBA`.
It may ask the `ffmpeg` CLI for platform hwaccel, but its contract is still
`rawvideo` RGBA over stdout, so it is CPU-resident and cannot be reported as
Mondrian hardware decode, zero-copy, low-copy texture residency, or GPU frame
delivery. The previous "GPU assist" terminology is intentionally not used.
Because the CLI process boundary is not cooperatively cancellable, this path is
limited to `RandomAccessStillFrame` requests. `PlaybackCursor` and `ScrubCursor`
must stay on in-process decode/session paths where scheduler cancellation can be
observed between open, seek, packet/decode, hardware-frame transfer, scale, and
copy stages.
Every frame returned by the preview decode boundary is a
`PreviewDecodeOutcome`: `Frame(RgbaFrame)` for CPU RGBA payloads or
`NativeGpuFrame(PreviewNativeDecodedFrame)` for GPU-resident decoder payloads.
Native FFmpeg results use `PreviewDecodePath::InProcessFfmpegNative`; they are
never labeled as the in-process CPU RGBA path.
`PreviewNativeDecodedFrame` must carry a
`PreviewNativeDecodedFrameHandle` minted by the media backend that owns the
native decoder resource. The handle is a shared lease over an
`Arc<dyn PreviewNativeDecodedFrameResource>`; cloning a frame retains the
backend resource, and dropping the final clone releases it through the concrete
resource implementation. Handles are neither `Copy` nor serializable. Their
process-local kind/id values are diagnostics and backend-routing evidence, not
OS handles or resource ownership by themselves. Equality and hashing use the
lease object identity so recycled diagnostic ids cannot alias live resources.
The in-process FFmpeg lease is `FfmpegNativeDecodedFrameResource`. It retains
the decoder frame with `av_frame_clone`, thereby retaining the frame's
`AVBufferRef`-owned hardware surface, and releases that reference with
`av_frame_free` when the final lease drops. Preferred D3D11 import reads only
FFmpeg's documented `AV_PIX_FMT_D3D11` ABI: `AVFrame::data[0]` is the borrowed
`ID3D11Texture2D` pointer and `data[1]` is the array-texture slice. Legacy
`AV_PIX_FMT_D3D11VA_VLD`, missing texture pointers, and slice-width overflow
must fail with structured resource errors rather than being reinterpreted as
the preferred layout. Establishing this resource ownership contract does not
by itself report active hardware decode, native output, or zero-copy; those
states remain observed-result facts.
Forward frame selection must retain before/after FFmpeg candidates with
`av_frame_clone` as well. Do not use `ffmpeg-next::Video::clone()` for decoder
candidates: that wrapper allocates an image frame and invokes pixel-copy APIs,
which is neither the correct hardware-surface lifetime operation nor a checked
failure boundary. The media-internal retained candidate type owns the cloned
`AVFrame` until selection/conversion finishes.
A handle-kind-only payload is invalid because it can masquerade as GPU
residency without an importable resource. Native payloads are
limited to renderer-importable surface families such as NV12, P010, RGBA8, and
BGRA8; unknown or planar CPU formats must fail closed before reaching the app
or renderer. They must also carry `DecodedVideoSampling` at construction time:
range must be explicit, bit depth must match the native surface contract, and
subsampled NV12/P010 payloads must have explicit chroma location. This remains
media payload evidence, not color interpretation; unsupported-but-explicit
chroma siting can be rejected later by the app/renderer admission boundary, but
missing sampling facts must not escape the media native-frame constructor.
For D3D11 frames, media reads `AVFrame::hw_frames_ctx` and accepts only explicit
`AV_PIX_FMT_NV12` or `AV_PIX_FMT_P010LE` software layouts. Planar
`AV_PIX_FMT_YUV420P` / `AV_PIX_FMT_YUV420P10LE` descriptions are not silently
reinterpreted as two-plane GPU textures. A GPU-preferred CPU fallback records
`PreviewNativeDecodeFallback` as `SoftwareFrame`,
`ResourceAdapterUnavailable`, `SurfaceFormatUnavailable`,
`SamplingMetadataIncomplete`, or `ResourceRetentionFailed`. A required-GPU
request returns a decode error for the same condition instead.
GPU-resident requests bypass the process-global CPU RGBA cache and the
session-local RGBA playback ring. Native decoder surfaces are not inserted into
either cache because retaining them there can exhaust the decoder surface pool;
their bounded lifetime belongs to the returned current/prefetch payloads and
the app scheduler. CPU fallback payloads remain eligible for the existing CPU
cache policy.
CPU consumers such as thumbnails and current RGBA fallback paths must explicitly
match `Frame(RgbaFrame)` and fail closed on `NativeGpuFrame`; they must not
reinterpret a native decoder surface as RGBA or silently force a CPU transfer.
The app viewer preview path may preserve `NativeGpuFrame` as a native source
payload and pass the complete frame, including its opaque handle token and
residency/sampling facts, into GPU preview admission. App adapters must not
flatten that payload into diagnostics and discard the token. Until renderer
native import execution is connected, the retained payload is a renderer
readiness blocker, not a media decode failure and not a CPU fallback frame.
The renderer owns the fallible mapping from media `DecodedVideoSurfaceFormat`
to its native texture-format contract and implements its source-descriptor
trait for `PreviewNativeDecodedFrame`. App readiness code delegates to that
mapping instead of copying renderer format policy back into the media/app
layers.
When native payloads reach the renderer import contract, renderer-side video
sampling metadata is mandatory. `Nv12` is 8-bit YCbCr and `P010` is 10-bit
YCbCr; 12/16-bit hardware surfaces require a distinct future format such as
P016 rather than overloading P010. The app/readiness layer must pass explicit
limited/full range, YCbCr matrix, transfer characteristic, and chroma-location
facts resolved from media metadata / user interpretation. Platform adapters for
D3D12/D3D11, VideoToolbox/IOSurface, and VA-API/DMABUF must import handles
only; they must not silently decide Rec.709 vs Rec.2020, SDR vs PQ/HLG, or
left vs center chroma siting.
Both payload kinds carry `PreviewDecodeDiagnostics`: concrete path
(`InProcessFfmpegCpuRgba`, `InProcessFfmpegNative`,
`ExternalFfmpegCpuRgba`, `PlaybackSessionRingHit`, or `PreviewCacheHit`),
elapsed microseconds, cache-hit status, requested access mode, external-process
status, CPU-residency evidence, seek status, requested seek strategy,
session-local seek-index availability and source (`None`, `SessionObserved`, or
`ProbeBacked`), anchor-use evidence, decoded frame count, in-process FFmpeg
decoder threading mode/count, and stage-level wall-clock timings for session
open, cache lookup, seek, packet/decode, FFmpeg hardware-frame transfer back to
CPU, software scaling, RGBA copy, and the experimental external-process path.
The same diagnostics carry the current hardware decode contract:
`hardware_decode_request`, `hardware_decode_decision`, `hw_accel_backend`,
`hardware_decode_candidate_backend`, `hardware_decode_candidate_handle_kind`,
`hardware_decode_adapter_available`,
`hardware_decode_ffmpeg_device_type_available`,
`hardware_decode_ffmpeg_codec_config_available`,
`hardware_decode_ffmpeg_hw_pixel_format`,
`hardware_decode_ffmpeg_device_context_attempted`,
`hardware_decode_ffmpeg_device_context_created`,
`hardware_decode_ffmpeg_device_context_error_code`,
`hardware_decode_cpu_transfer_configured`,
`hardware_decode_cpu_transfer_observed`, `hardware_decode_active`,
`zero_copy_active`, `decoded_frame_residency`, `gpu_frame_handle_kind`,
`hardware_decode_blocker`, and `native_decode_fallback`, plus
`decoded_surface_format` for the decoder output
format before CPU RGBA conversion. `Nv12` and `P010` are the primary GPU-native
YUV/P010 residency candidates; they are media facts, not renderer import claims.
These fields are fail-closed; until a real hardware-frame decoder and renderer
import path are connected they must report CPU RGBA residency with
`TextureResidencyNotConnected` and a CPU RGBA `PreviewHardwareDecodeDecision`.
When FFmpeg has no matching codec/backend hardware config, the decision should
be `CpuRgbaCodecUnsupported`. When FFmpeg advertises the config but cannot
create the hardware device context, the decision should be
`CpuRgbaHardwareUnavailable`. When FFmpeg can create the device context and
hardware frames are observed but Mondrian still transfers them to CPU RGBA, the
decision should be `HardwareDecodeCpuTransfer`; this should improve decode CPU
pressure but is not the final residency model. Only after an adapter produces
native GPU residency should later readiness failures move to renderer or
platform import diagnostics.
The app-window GPU preview path must preserve those media facts in its frame
residency telemetry. Media decode diagnostics feed the
`AppUiGpuPreviewMediaSource` contract, and window-side native video import
readiness combines decoder residency/handle/format with the platform import
probe and renderer import support. This does not make CPU RGBA preview hardware
decoded; it prevents the future hardware decoder adapter from being hidden
behind a generic "GPU input upload" label once it starts producing NV12/P010
native surfaces.
Preview sessions seed their seek index from FFmpeg's container/probe stream
index when available, using a small media-layer LRU cache keyed by
path/fingerprint/video-stream. That first production path gives scrub and still
decode real keyframe/GOP evidence without a full packet scan before first frame.
When a container exposes no usable index, sessions continue to learn keyframe
anchors from decoded packets and report `SessionObserved` instead of pretending
the source was probe-backed.
`ScrubCursor` derives its effective forward-decode budget per request from that
evidence: probe-backed anchors close to the target get a tight budget, missing
or session-only evidence gets a smaller responsiveness-first budget, and exact
playback/still requests keep their larger deterministic budget. The budget
reported in `PreviewDecodeDiagnostics.forward_decode_budget_frames` is the
effective budget that was actually used for that request.
App-level preview diagnostics aggregate those fields so playback/perf JSON can
show whether a 4K/HDR test is decode-bound, long-GOP seek-bound, cache-bound,
single-thread decode-bound, software-scale/copy-bound, worker-queue-bound, or
GPU-output-bound. The same diagnostics must also preserve the stage timings from
the slowest single decode frame and slowest single post-decode render frame, and
track how long decoded jobs waited in the preview worker queue before decode
started. Performance reports classify their primary bottleneck from max-frame
stage timings plus max queue wait, while aggregate stage totals remain trend
evidence. This avoids blaming a cumulative stage total when an interactive stall
came from one pathological seek, decode, software-scale/copy, composite,
output-boundary frame, or current-frame job waiting behind other decode work.
The in-process preview decoder uses bounded frame threading by default. This is
the product default because 4K HEVC Main10/Long-GOP preview seeks are commonly
packet-decode bound, and frame threading is the safer general FFmpeg software
decode default than slice threading for this class of media. The app
preview service runs a conservative decode worker pool from
`PreviewDecodeCpuBudget`: one worker on small CPU budgets, two on common
mid-range machines, and three only on larger workstations. FFmpeg decoder
threads per worker are computed from the same budget, leaving explicit
interactive headroom so current-frame decode can make progress while another
worker is occupied by prefetch or a long-GOP seek without oversubscribing the
UI, renderer, audio, or FFmpeg's own codec threads. When a current-frame request
is scheduled, the job queue also prunes obsolete prefetch jobs from older render
generations before enqueueing the current work; fresh same-generation prefetch
remains eligible only after the current frame is not pending, no current-frame
job is already queued or running, and queued plus in-flight prefetch is below
the forward window. It then tops up only the unfilled queued-plus-in-flight
prefetch budget while traversing tracks and nested sequences. That lets playback
warm nearby frames without stealing first-frame or recovery budget. This worker
pool and pruning are scheduling guardrails only; they are not a substitute for
future cancellable decode sessions or hardware-resident decode.
Preview decode also exposes a cooperative cancellation boundary for interactive
work: app workers pass a generation-aware predicate to the media decoder, and
the media loop checks it before opening, seeking, packet decode, frame receive,
EOF draining, and RGBA conversion. If cancellation fires, the decoder returns a
typed canceled outcome rather than a media failure. `PlaybackCursor`
cancellation preserves its thread-local FFmpeg session so sustained playback
and forward prefetch can retain decoder residency; `ScrubCursor` and
`RandomAccessStillFrame` cancellation discard only their own mode-specific
session because their packet/frame state may be mid-stream and should not poison
subsequent precise or latest-wins requests. The experimental external-process
CPU RGBA path must follow the same session-retention policy even though the
child process itself cannot be interrupted mid-run. This keeps stale work from
being cached or marked as a failed source while preserving independent playback,
scrub, and still-frame session state for subsequent requests.
Speculative prefetch decode is also bounded by a short app-level wall-clock
budget. Current-frame decode is not canceled by this budget, and the deadline
applies only to playback prefetch work. Scrub and still-frame requests are
latest-wins/current-frame work; if future callers try to submit them as
prefetch, scheduler admission must reject them instead of letting the worker
deadline silently reinterpret their access mode. A playback prefetch that is
already in a worker must also yield immediately when another current-frame
request is pending, unless that prefetch is for the same media key that was
promoted to current-frame work. This cancellation is reported separately as
prefetch-preempted-by-current rather than as obsolete work or a deadline miss,
so diagnostics can distinguish intentional current-frame protection from
expired speculative work.
Playback current-frame decode has a separate display deadline. The app layer
assigns that deadline when a `Current + PlaybackCursor` job is admitted or
promoted, because only the app owns viewer/playback-clock intent. The budget is
derived from the active sequence frame duration and clamped to a conservative
interactive range, so 24/25/30/60 fps playback does not all inherit one opaque
timeout. A playback current job that reaches a worker after its deadline is
canceled before FFmpeg work begins. Expired playback-current jobs must not
block fresher current-frame work in worker queue selection, but they must remain
observable long enough to emit a structured deadline cancellation instead of
disappearing as an opaque queue drop. A job that crosses the deadline while
decoding is cooperatively canceled through the same media predicate. This is
intentionally not a media crate concept: `mondrian-media` still receives only
an access-mode request and a cancellation predicate. Diagnostics must report
playback-deadline cancellations separately from prefetch-deadline cancellations
so late visible frames can drive drop/proxy/hardware-decode work instead of
being hidden as generic obsolete work.
Completion polling repeats the same deadline contract as a final guard. If a
`Current + PlaybackCursor` result reaches the app after its display deadline,
the app may still record decode diagnostics and success/failure telemetry, but
it must not cache the frame, mark it visible, or reset sustained-pressure
recovery as a successful current playback frame. This protects the viewer from
decode backends that return after missing a cooperative cancellation check.
Repeated playback-current deadline misses put the app scheduler into sustained
pressure recovery. In that state, a fresh `Current + PlaybackCursor` request
must not pile onto the decode queue while another current or playback decode is
already queued or in flight; it is skipped as a clock-driven drop/proxy/hardware
decision. When no realtime decode work is pending, the scheduler must admit one
current playback request so the viewer has a chance to recover instead of
staying permanently stale.
User transport and close/quit actions are interactive escape paths. When they
arrive while playback or buffering is active, the app host must cancel obsolete
preview generations and queued jobs before dispatching the state mutation, and
it must refresh transport controls without synchronously requesting a new
preview frame. A visible pending-close confirmation must be repainted
immediately; an invisible modal or a decode worker that is still finishing an
old frame must never make pause, close, or quit feel locked.
Diagnostics for this path must separately count escape-path requests, the
scheduler pending requests they canceled, and the worker-queue jobs they
cleared. Those counters are distinct from generic obsolete-generation churn so
reports can distinguish healthy user-driven preemption from unstable playback
rescheduling.
Schedule diagnostics also count current playback decode decisions, late-frame
drop decisions, and proxy/hardware recommendations. Those are app scheduler
facts, not media decoder facts, and they are how perf tooling distinguishes
clock-driven playback from best-effort frame extraction. The broad
proxy/hardware recommendation counter must not be the only signal for recovery
policy: current playback frames blocked specifically by renderer/native GPU
import readiness are counted separately so automated fallback can distinguish a
missing GPU-resident path from deadline pressure or proxy-generation pressure.
Preview decode performance reports must surface that case with a specific check,
root-cause evidence, and an action to connect renderer native video import.
Playback hardware-decode admission is also runtime-gated by the app preview
service. The media request default remains `PreviewHardwareDecodeRequest::Auto`;
window/renderer code may raise playback jobs to `PreferHardwareDecode` for
FFmpeg hardware CPU-transfer fallback, and to `PreferGpuResident` only after the
renderer native decoded-frame import contract reports ready and the platform
probe supports at least one renderer-supported native handle family with
zero-copy or declared low-copy import. This prevents CPU-transfer playback from
being mistaken for native GPU residency while still avoiding a pure software
decode default for sustained playback. The same admission state must be
serialized in preview diagnostics and performance reports as separate renderer,
platform, and final-admission facts; a known native-import blocker must produce
a specific gated-admission root cause instead of disappearing as a generic
media-layer software decode.
Playback reports must also distinguish hardware-decode intent from effective
hardware decode. If `PlaybackCursor` requests `PreferHardwareDecode`,
`PreferGpuResident`, or `RequireGpuResident` but the observed result is neither
GPU-resident native decode nor an observed FFmpeg hardware CPU-transfer frame,
the report must surface a `preview_decode_playback_hardware_fallback_not_engaged`
root cause with backend, codec, device-context, decoder-open, setup, and native
import evidence. This is a recovery signal for proxy/optimized-media or backend
repair, not proof that the hardware path is active. The app playback scheduler
records the same condition as
`current_hardware_fallback_not_engaged_decisions` and increments the broad
proxy/hardware recommendation counter at most once for a completed current
playback frame, even when the same frame also reports a native-import blocker.
This keeps automated recovery policy clock-driven without inflating pressure
metrics from overlapping hardware diagnostics.
When playback pressure resolves an asset that is already in proxy mode but the
proxy is missing or stale, the app preview service may request proxy generation
through the shared app-layer proxy dispatcher. The request is deduplicated by
asset, source fingerprint, and missing/stale reason so a late playback frame
does not enqueue proxy work every refresh. Preview must not spawn FFmpeg
directly, change media color interpretation, or silently enable proxy mode; the
media crate still owns only proxy file generation and status probing.
Interactive scrub uses app-selected adaptive hints rather than a separate decode
API. The app preview service observes recent scrub seek locality and scrub
decode latency, then tags `PreviewDecodeRequest` with a
`PreviewScrubAdaptiveClass`. The media layer preserves the requested frame
semantics but may tighten bounded-any seek windows and forward-scan budgets for
hot or slow scrub regions. This avoids long UI-blocking scrub attempts while
keeping settled still-frame requests exact. Do not implement scrub speedups by
silently changing color interpretation or by shrinking decoded media geometry
unless the renderer has an explicit source-sample extent versus layout extent
contract.
`RgbaFrame` stores its RGBA8 payload in shared immutable memory so cache hits can
adjust per-request diagnostics without deep-copying a 4K frame. Callers that
need ownership must request it explicitly through the frame consumption API;
renderer color-frame boundaries should prefer the shared payload constructor.
`PreviewNativeDecodedFrame` is the separate GPU-resident payload contract and
must flow toward renderer native decoded-frame import rather than the RGBA cache.
Preview decode session reuse is isolated by `PreviewDecodeAccessMode`, and each
session slot plus the process-global preview frame cache must be keyed by a
media file fingerprint, not by path alone. Proxy regeneration finalizes fresh
media at the same proxy path, so same-path cache hits or reused FFmpeg sessions
are valid only while file length and modification timestamp still match the
fingerprint captured when the session/cache entry was created.
FFmpeg's default app log level is fatal for product preview decode. Codec-level
warnings and recoverable decoder errors, such as HEVC reference-frame messages
during aggressive seek/scrub, must not leak directly to the user terminal as the
primary diagnostic channel. Developers can opt into noisier FFmpeg output with
`MONDRIAN_FFMPEG_LOG_LEVEL`; product health should use structured decode
diagnostics and explicit frame failure/cancellation reasons instead.
Preview path resolution already probes the source/proxy file identity; app
workers must forward that `PreviewFileFingerprint` into the media decode
boundary instead of making the decode worker repeat the filesystem metadata
lookup. `mondrian-media` may capture the fingerprint itself only for lower-level
callers that do not already have one.
`MONDRIAN_PREVIEW_DECODE_THREADING`, `MONDRIAN_PREVIEW_DECODE_THREADS`, and
`MONDRIAN_PREVIEW_DECODE_WORKERS` are diagnostic overrides, not separate decode
semantics. `THREADS` means FFmpeg decoder threads per app preview worker;
`WORKERS` means the app preview worker budget used for access-mode lanes. The
app viewer preview service uses the resolved budget directly for playback and
interactive lane workers. Thread-local preview decode sessions are
intentionally kept alive for playback locality and must be released through
`clear_thread_local_preview_decode_session()` at explicit lifecycle boundaries
such as perf probes, media/project shutdown, or tests that open threaded
software decoders.

The renderer now owns a GPU input-stage resource contract for decoded CPU RGBA8
source frames: upload to `Rgba8Unorm`, execute the OCIO GPU input transform, and
produce a GPU-resident linear working frame in a float texture. This is the
bridge for guarded rollout of GPU input transforms. The app viewer uses this
contract for supported media preview layers before GPU working-space
compositing, falling back per-layer to CPU working-frame upload only when the
GPU input stage cannot be recorded. It is not yet a hardware decode or
zero-copy media path because the decoder boundary still hands CPU memory to the
renderer.

## Asset Classification

`mondrian-assets` classifies imported files using `MediaInfo`. Audio-only extensions or media without meaningful video streams become `Audio`; media with video becomes `Video`.

Synthetic assets use `MediaInfo::synthetic_adjustment_layer()` and `MediaInfo::synthetic_solid_color()`.

## Color Metadata

Media probe separates detected metadata from policy assumptions.
`VideoStreamInfo.detected_color_space` is the transform-facing detected-only
index. `VideoStreamInfo.color_interpretation` is the diagnostic/UI-facing
interpretation with confidence, evidence, warnings, and a user-overridable flag.
Evidence records whether a result came from a camera/log metadata hint, exact
CICP tags, partial CICP tags, unsupported CICP tags, or decoder unavailability.
Warnings preserve machine-readable provenance, not only a resolved color-space
enum: multiple-hint warnings keep the selected and ignored metadata keys,
values, and scopes; hint-vs-CICP warnings keep the selected hint and the raw
CICP triplet that conflicted with it. `VideoColorDiagnostic::summary()` is the
stable compact form for logs/export errors and should include those warning
details. `VideoColorDiagnostic::issue_summary()` is the machine-readable
contract for UI, telemetry, smoke JSONL, and export reports; callers must
consume its counters and flags instead of parsing the compact summary string.
`VideoColorDiagnosticIssueAggregate` is the shared rollup for combining many
per-stream diagnostics into one report surface.
Clip-level `MediaInterpretation` can override color space, frame rate, pixel aspect ratio, field order, and alpha interpretation.

Asset library records store persistent user intent separately as
`AssetMediaInterpretation`. Imported media defaults to `Auto`; Auto means "resolve
from current metadata, detector, and project color policy" and must not persist
the currently resolved color space. User changes from the asset-library
Interpret Footage dialog are stored as `Override { color_space }` and must
remain stable across metadata re-probes, relinks, and detector upgrades.
Non-color data is represented as asset payload classification, not as an
Interpret Footage color-space mode. It is reserved for masks, mattes, technical
textures, and advanced utility-channel workflows, not the primary input
color-space picker. UI may display the current resolved result, confidence,
method, and warnings beside Auto, but that resolved value comes from
probe/color-management diagnostics rather than the asset record.
Preview and export resolve input color with the same precedence: non-color
asset payload classification first, then clip-level override, then
asset-library interpretation, then detected metadata, then the sequence
missing-metadata policy.

Unknown/missing metadata policy is resolved at sequence color-management time, not by UI panels.
