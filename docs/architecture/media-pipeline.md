# Media Pipeline

`mondrian-media` owns FFmpeg-based media inspection, decode support, waveform/proxy/cache primitives, and audio buffers.

## Realtime audio output evidence

`RealtimeAudioOutput` owns the concrete CPAL stream and a fixed-capacity
`ArrayQueue<f32>` PCM queue. The device callback may pop samples, fill silence,
and update atomics only; it does not acquire the former queue mutex. Main-thread
enqueue is bounded to two seconds and evicts the oldest queued sample under
backpressure rather than growing memory.

`RealtimeAudioOutputSnapshot` is the media-to-app Adapter evidence seam. It
reports stream generation, configured sample format, cumulative and
active-interval callback frames, callback count/age/quantum, queued frames,
underrun frames, active state, and asynchronous stream failure. These are media
facts. `mondrian-media` does not select a Clock Master or label this estimate as
an exact hardware playback head; the app lowers the snapshot to a typed playback
observation and `mondrian-playback` applies preroll, uncertainty, epoch,
monotonicity, and handoff policy.

Realtime transport, Clock Master selection, Frame Demand deadlines, and the
interpretation of Frame Deliveries belong to the app Playback Engine described
in [Playback Engine](playback-engine.md). `mondrian-media` executes bounded
decode requests and reports facts; it does not pause or advance transport.

## Bounded audio source windows

Playback and Export do not decode complete audio sources into resident memory.
`AudioSourceCache` opens fingerprinted source readers and supplies exact
interleaved PCM through aligned ten-second windows. One weighted LRU spans all
readers at the prepared sample-rate/channel contract: 128 entries, 256 MiB
payload, and 64 bounded terminal failures. Path + file length + modification
timestamp is the current source revision boundary. Cache diagnostics expose
bytes, entry pressure, hits/misses, decode results, oversize windows,
single-flight leaders, and evictions; an entry-count-only claim is insufficient.

The concrete miss Adapter owns a bounded pool of at most eight persistent
FFmpeg child-process Sessions. A Session is keyed by the complete source
fingerprint plus output sample-rate/channel contract, opens at the first
requested sample, and continuously emits interleaved `f32le`. Consecutive
windows reuse that stream; a non-contiguous miss terminates and reopens only
that source Session using at most ten seconds of input-side coarse preroll plus
output-side exact trim. This preserves the sample coordinates of a sequential
decode instead of trusting codec-dependent input-seek priming. Pool pressure
evicts an idle least-recently-used Session. This is an intentionally isolated process
Adapter, not an in-process FFmpeg claim; a linked FFmpeg Adapter may replace it
behind the same Interface without changing cache or sample semantics.

Stdout has two bounded 64 KiB look-ahead chunks and stderr retains only its
latest 64 KiB while always draining the pipe. Generation cancellation is
polled every 5 ms while waiting for output, then kills, waits, and joins the
child and both pump threads. Partial EOF is accepted only on a complete
interleaved frame boundary. The cache additionally provides single-flight per
complete source-window key, so concurrent consumers share one decode result
instead of serially reopening the Session. None of this work runs in the CPAL
callback or UI thread. The older whole-file helper remains only for
waveform/reference jobs and is not the playback/export PCM source path.

The block contract now carries the generation-owned
`ExecutionCancellationToken` all the way into `AudioWindowDecoder`. Cancellation
is checked before lookup, across concrete decode, and before cache admission.
Canceled results are neither decoded-window entries nor terminal failures, so a
seek cannot poison the same source coordinate for its successor generation.
Session diagnostics separate cold opens, sequential reuse, and random-seek
restarts and expose resident/peak/capacity, evictions, cancellations, and each
class's worst wall duration. Acceptance may constrain steady sequential
latency without falsely relabeling cold-open or random-seek cost.

## Probe

`MediaInfo::probe(path)` uses FFmpeg format/codec metadata without decoding full
media. It extracts:

- container and duration
- file size
- video streams: codec, decoder-proven codec profile, stream-local declared
  duration, dimensions, frame rate,
  pixel format, bit depth, alpha, detected color space, structured color
  interpretation, frame count, and HDR side-data summaries
- audio streams: codec, stream-local declared duration, sample rate, channels,
  layout, bit depth

The probe runs off the UI thread. Once stream/CICP evidence identifies a video
as HDR (or stream metadata declares dynamic HDR/Dolby Vision), the probe opens a
short-lived decoder and reads only the first decoded frame, with a hard limit of
512 target-video packets. This is required because FFmpeg exposes HEVC ST 2086,
MaxCLL/MaxFALL, and HDR10+ metadata as `AVFrameSideData` for common files even
when `AVStream`/`codecpar` side data is empty. The frame facts are merged with
the stream facts by semantic kind; a typed frame payload replaces an unparsed
stream summary but never creates duplicates. SDR and audio-only imports do not
open this metadata decoder. Failure to obtain the optional first-frame evidence
is logged without making otherwise decodable media offline.

Probe absence is explicit. Invalid/zero FFmpeg frame-rate rationals are stored
as unproven rather than silently replaced with 25 fps. Unsupported or unknown
pixel formats retain an unproven marker rather than becoming YUV420P/8-bit.
`VideoCodecProfile::Unknown` is not equivalent to a profile inferred from codec,
bit depth, filename, or extension. A professional Main10 gate requires the
opened decoder context to report `HevcMain10`; persisted records written before
these proof fields default to unproven and must be re-probed before acceptance.
Likewise, a container duration cannot prove that its primary audio or video
stream spans the same interval. Professional acceptance requires a positive
stream-local duration for the relevant primary stream and rejects missing or
shorter evidence. Audio cannot count post-EOF silence as source coverage, and
video cannot count a longer container or unrelated stream as playable frames.

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
  jog, and shuttle. With a probe-backed index it seeks toward the nearest
  keyframe and presents the first valid decoded frame inside the adjacent-GOP
  evidence radius. That temporal approximation is Degraded; the settled request
  then resolves the exact frame. It prioritizes cancellation and visible
  feedback over warming a long forward queue.
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
app-resolved input/source color space plus an authority-aware
`DecodedVideoRangeContract`. In Auto mode CPU/native decode prefers each YUV
frame's explicit range and falls back to the stream probe only when the frame
omits it; Full/Limited user overrides remain authoritative. Matrix is resolved
independently: an explicit decoded matrix controls YCbCr-to-RGB sampling even
when it differs from the resolved RGB source color space. Decode still rejects
unknown facts and unsupported matrices such as BT.2020 constant luminance.
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

Packaged applications carry their complete non-system media/color runtime
closure. Windows places vcpkg/`FFMPEG_DIR` DLLs beside the executable, Linux
places collected shared objects under `lib/` with relative RPATH, and macOS
places dylibs in the app bundle's `Contents/Frameworks` with rewritten install
names. Every release stage runs `--verify-runtime` in a sanitized environment;
product startup must not depend on Cargo's test-only search path, Homebrew, or a
developer-specific `PATH`.
`PreviewDecodeAccessMode` intentionally has no default value, and serialized
decode diagnostics must include it. Missing access-mode evidence is a diagnostic
coverage bug, not a reason to assume still-frame semantics.
Current Preview cache and in-flight identities include the requested access
mode, source media path, file fingerprint, output dimensions, and source time in
microseconds. A bare timeline frame number is not a media identity: the same
frame index can represent different source times under different time bases,
and relink/proxy/source path changes must not reuse stale RGBA frames.
Microseconds are the current implementation bridge, not the long-term semantic
key: distinct exact source instants can quantize to the same microsecond. The
target cache identity carries canonical source Timeline Time or exact stream
PTS plus stream time base and the declared rounding/seek contract. A lossy
derived timestamp may remain diagnostic metadata but cannot independently
authorize cache reuse.
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
App preview scheduling preserves decoder-session locality with semantic worker
lanes. When more than one preview decode worker exists, worker 0 has playback
affinity and the remaining workers are assigned scrub/still or shared
non-playback affinity according to the CPU budget. A worker may dequeue current
work only when its lane accepts that work class; allowing an idle Playback or
Scrub worker to steal an exact Still request cold-opens additional FFmpeg hardware
sessions, churns decoder surfaces, and can block realtime work behind a
deterministic seek. `Prefetch` remains playback-only and lower priority than
every eligible current-frame request. With only one worker, the lane is `Any`;
the two-worker fallback uses `Playback` plus `NonPlayback`, so reduced machines
still make progress without a hidden cross-lane exception. Because workers
filter by lane, enqueue and priority promotion wake all preview workers, not
just one; otherwise a playback-only queue could wake a non-playback worker and
leave the playback worker asleep until another request arrives.
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
`prefetch_skipped_current_work`, and `prefetch_skipped_prefetch_backlog`. This
keeps first-frame display and dropped-frame recovery ahead of cache warming on
slow or long-GOP media. The configured forward window is derived from a
250 ms wall-clock horizon and the active sequence frame rate, then capped at
16 frames before
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
non-blocking on the UI/event thread: it first closes the Frame Work Broker to
establish the authoritative cancellation instant, sets the timestamp-free local
stop flag, clears queued/UI generation state, and moves worker
handles to a background reaper that joins them after FFmpeg exits. A codec,
filesystem, or driver stall inside a preview worker must not prevent pause,
window close, or app quit from being processed. Each worker still explicitly
drops its thread-local media decode sessions before exit; thread-local FFmpeg
decoder state must not be left to implicit TLS teardown at project/app close.
An otherwise idle worker performs a timed Broker receive and releases its
thread-local decode sessions after two seconds without work. This preserves
short-gap playback locality while bounding native decoder/device residency
during an open but inactive project; the next request cold-opens normally.
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
Playback and scrub cancellation are cooperative but non-destructive to their
mode-local decode sessions: a prefetch budget miss or superseded pointer target
must not throw away the warmed decoder/device context. Every subsequent seek
flushes and repositions the decoder before reuse. Exact still-frame cancellation
remains session-destructive because stale partial decode state is not useful.
The playback decode session also owns a small forward RGBA ring. Ring hits are
strictly bounded by the same PTS tolerance as the process-global preview frame
cache and are reported as `PlaybackSessionRingHit`; they are not available to
scrub or still-frame requests. This keeps continuous playback locality inside
the media access-mode implementation rather than scattering playback caches
through app UI code.
Forward session reuse must also preserve FFmpeg's send/receive backpressure
contract. A request may return as soon as its target frame is available while a
frame-threaded decoder still has reordered output queued. The next request
drains and considers that output before submitting another packet; it must not
discard those frames or treat `AVERROR(EAGAIN)` from `avcodec_send_packet` as a
terminal media failure. This keeps sequential playback frame-exact and avoids
reopening or seeking a healthy decoder merely to clear its output queue.
Access-mode decode behavior is centralized in a media-layer policy, not in app
conditionals or FFmpeg call sites. Playback has the widest mostly-forward
session reuse window, the playback ring, and the exact-path forward decode
budget; scrub has only a very short forward reuse window, a smaller CPU
fallback forward-scan budget, no playback ring, and the low-latency
probe-backed approximate-first-frame strategy with a bounded-any fallback;
random-access still extraction has no forward reuse
and keeps keyframe-safe exact seeking with the exact-path budget. Scrub
low-latency seek is product semantics, not an opt-in environment variable. This
preserves still-frame correctness while preventing latest-wins scrubbing from
spending the same long-GOP CPU budget as deterministic extraction, and leaves a
clear replacement point for future hardware-resident playback and low-latency
scrub backends.

The normal scrub budget derived from an indexed keyframe includes
decoder-reordering headroom and retains the unindexed safety floor.
Frame-threaded codecs and imperfect container indexes can make the actual
decode distance exceed the simple presentation-frame distance, so that
distance alone is not a safe stopping point. Adaptive hot/slow/recovery modes
may still tighten that floor because newer input cooperatively cancels their
work. Timeout and forward-budget exhaustion are transient scheduling outcomes
and must not enter the terminal media-failure cache; a settled deterministic
request for the same frame must remain eligible to retry exactly.
Likewise, completion of a canceled current scrub or still request schedules one
follow-up render pass so the settled position can submit fresh work after its
scheduler entry is released. Playback deadline cancellations do not use this
rule, because immediate resubmission would create a realtime retry loop. Decode
cancellation is scheduler evidence, not a frame presentation: it records its
reason and return latency but must not emit `FrameDelivery::Canceled` for an
identity already replaced by latest-wins scheduling.

Every preview decode diagnostic emitted by `mondrian-media` must carry the
resolved access-mode policy contract alongside the observed result:
`seek_strategy`, `forward_reuse_frame_window`,
`forward_decode_budget_frames`, `any_seek_window_ms`, `requested_pts`,
`selected_pts`, and `temporal_approximation`. App/UI performance
reports may aggregate those fields, but must not reconstruct them from app
conditionals. This keeps policy bugs diagnosable: for example, a scrub sample
that reports `BoundedAnyFrame` with a zero `any_seek_window_ms` is a broken
media contract, not a UI presentation issue.
Each in-process preview decode session also maintains a session-local,
incremental keyframe seek index from video packet metadata observed during real
decode work. The index may bound later seeks to an already-known keyframe
anchor. When bounded scrub has index evidence, it selects the nearest keyframe,
seeks toward that timestamp with keyframe-safe backward semantics, and requests
FFmpeg `NonKey` discard as an optimization. Hardware decoders are not required
to honor that discard hint: the warm phase accepts the first valid decoded
frame within the adjacent indexed-GOP radius, preventing a slow-result feedback
loop from making the same target permanently exhaust a smaller recovery budget.
Diagnostics preserve both requested and selected PTS plus
`temporal_approximation`; presentation policy reports the result as Degraded
rather than claiming frame accuracy. `AVSEEK_FLAG_ANY` is
reserved for the unindexed bounded-window fallback. The index must not perform
a blocking whole-file scan on first frame or
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
`DecodedVideoSampling` records the decoder-reported YCbCr matrix, encoded
range, chroma location, and effective bit depth observed on the decoded FFmpeg
frame. These values are media facts,
not color-management interpretation. `mondrian-media` must not translate them
into renderer sampling contracts or infer missing values from platform defaults.
The app layer combines them with the resolved source color space when evaluating
native decoded-frame import readiness; missing or unsupported sampling remains a
structured blocker instead of silently falling back to guessed NV12/P010 shader
constants.
Absent matrix metadata is distinct from an explicitly unsupported matrix.
Only the absent case may use the resolved source contract's matrix; explicit
BT.2020 constant-luminance, derived, YCgCo, and ICtCp-style matrices remain
fail-closed until their conversion math is implemented.
Complete BT.470BG/gamma-2.8 and SMPTE 170M primaries/transfer pairs resolve to
the PAL and NTSC Rec.601 product color spaces respectively, regardless of
whether the decoded samples are RGB or YCbCr. Matrix remains an independent
sampling fact. Partial matrix-only metadata remains
diagnosed as partial evidence but may still select the matching Rec.601 family;
it must not be relabeled as Rec.709.
Metadata-hint normalization also recognizes exact ACES2065-1, ACEScg, ACEScct,
linear Rec.709/Rec.2020/P3-D65, and Sony S-Log2/S-Gamut identities. These hints
retain the same priority/evidence/warning model as existing camera Log hints;
generic `linear`, `ACES`, or `EXR` text is insufficient. Scene-linear sources
force high-precision proxy admission just like HDR and scene-Log sources.
App preview decode execution must run synchronous FFmpeg preview decode on
dedicated preview worker threads, not on the UI/event thread. Current-frame and
prefetch workers pass a cooperative cancellation predicate into
`PreviewDecodeRequest`, and the media loop checks that predicate before
open, seek, packet decode, frame receive, EOF drain, and RGBA conversion. Do
not depend on thread abort to preempt synchronous packet decode.
Decoded Preview payload ownership is UI-independent:
`app::preview_media_frame` owns decoded CPU/source/native residency, lazy CPU
working adaptation, logical and sampled geometry, presentation quality, decode
provenance, and exact host/decoder reservation. Its closed payload enum admits
exactly one of working CPU, source-domain CPU/GPU-capable, or native decoder
surface residency; no frame can be empty or claim contradictory residency. The
concrete worker loop, cancellation checkpoints, result publication, bounded
shutdown signal, and FFmpeg Preview Adapter live together in the UI-independent
`app::preview_media_task` Module. Window and Headless Adapters consume the same
structured terminal result and may not implement a second decode loop.
`app::preview_media_source` independently resolves an immutable asset record and
complete Viewer intent into one canonical key, explicit color rejection, or
structured unavailable outcome. It owns source/proxy fingerprinting,
color/range/Alpha interpretation, native-surface classification, proxy intent,
and decode-geometry canonicalization. Window code retains only asset-library
lookup, proxy dispatch/deduplication, and evidence projection.
`app::preview_timeline_execution` owns the UI-independent canonical render-plan
traversal, nested Sequence lookup/depth, per-Sequence execution resolution,
nested working-space composition/conversion, typed pending/unavailable
propagation, mandatory ready-plan cache identity, and ordered execution facts.
Window and Headless Adapters supply the same typed media outcome seam and may
project facts; neither may maintain a second recursion or child-sizing rule.
The same Module exposes a read-only media-demand collection over that graph.
Prefetch, preroll readiness, and input-color evidence consume it instead of
re-evaluating nested plans inside the Window Adapter.
The UI-independent `app::preview_viewer_plan` Module owns resolved element
representation, stable cache identity, quality/provenance aggregation, deferred
composite classification, and renderer GPU-layer lowering. Final presentation
arbitration and diagnostics live in separate private deep Modules.
The sibling UI-independent `app::preview_cpu_execution` Module owns source to
working-linear preparation, renderer timeline composition, Program Output,
monitor adaptation, and exact stage durations. It returns pixels plus all input,
composite, output, and monitor facts; Window diagnostics only project that result,
and Headless execution asserts the same result through the shared
`PreviewProductionRuntime`.
`app::preview_runtime::presentation` exclusively
chooses exact registered GPU output, raster cache, scoped stale reuse, deferred
playback composite, or the CPU output boundary; it cannot schedule media work
or mutate transport. CPU raster cache and stale state use the validated,
UI-independent `app::preview_raster_frame::PreviewRasterFrame`; only this final
Window Adapter converts it to a Widget `ViewerFrameImage`, without copying the
shared pixel allocation. Raster extent, encoded color identity, byte reservation,
and stable resource naming therefore remain available to Headless presentation
without importing Widget types. `app::preview_runtime::result_pump` exclusively
performs the bounded foreground drain of completed work, Broker resolution,
deadline expiry, cache admission, and terminal Frame Delivery projection; it may
record facts but cannot invent generation or deadline authority. Window lifecycle
code initiates project/switch shutdown in `service_lifecycle`, while the shutdown
signal, worker observation, and bounded exit behavior stay in
`app::preview_media_task`; teardown cannot become an alternate completion policy.
The sole Frame Store policy Adapter inside the Preview Production Runtime is
`app::preview_frame_store::PreviewFrameStoreAdapter`; its generic storage
and residency algorithm remains owned by `mondrian-playback::PreviewFrameStore`,
while diagnostic aggregation remains in the Preview Adapter. This is a
behavioral module boundary, not a second scheduler: all admission, deadline,
generation, and worker-lane authority still comes from `app::preview_access_mode`
and `mondrian-playback::FrameWorkBroker`.
The cross-Adapter GPU output blocker taxonomy and its aggregate breakdown live
in UI-independent `app::preview_gpu_output_blocker`. Renderer, display-contract,
Window, and Headless Adapters may contribute typed facts to it, but no Widget
module owns or reinterprets those diagnostic identities.
Mutable counter accumulation and point-in-time diagnostics projection are
localized in `app::preview_runtime::evidence`. It records facts selected elsewhere;
it cannot schedule, change pressure state, or derive pass/fail verdicts.
Within diagnostics, the versioned Color Health report model and its
check/root-cause/action derivation live in the private `color_health` deep
Module. It consumes the same immutable aggregate as before and cannot count
execution independently from the Preview Adapter.
The sibling private `performance` Module derives versioned Decode/Render
budget checks, verdicts, bottleneck classification, root causes, and actions
from immutable summaries. Public report builders are re-exported unchanged;
the Module owns no counters, scheduler feedback, or acceptance thresholds.
`app::preview_runtime::request_scheduler` is the concrete Adapter that consumes
canonical current/nested media demands, queries preroll residency, applies the
already selected adaptive hints, and submits Broker work. It does not own pressure
thresholds, access-mode mapping, deadlines, or generation identity; those
remain in the UI-independent policy/Broker Modules.
Viewer lifecycle is adapted through `app_ui::playback_feedback` into typed Frame
Deliveries. `Ready`, `StaleAvailable`, and `Blocked` are terminal observations;
`Loading` is non-terminal pending work. Loading or stale presentation does not
hold the Synthetic/Audio Clock Master and does not mute or clear realtime audio.
Late video is dropped while authoritative media time continues.

Current playback worker deadlines originate in the Playback Engine Frame Demand.
The app Adapter converts its remaining monotonic lifetime to an `Instant` budget
at enqueue/promotion time. `app::preview_scheduler_policy` owns the pure
worker-deadline eligibility, executed decode-quality classification, and bounded
frame-rate-to-prefetch-window policy;
`app::preview_access_mode` owns Broker admission/job transport,
`app::preview_media_task` owns concrete decode execution and cooperative
cancellation observation, `app::preview_media_source` owns canonical source
interpretation, `app::preview_timeline_execution` owns canonical Timeline and
nested-Sequence execution, while `app::preview_runtime` owns asset-library and
proxy-dispatch side effects plus immutable diagnostics projection. The shallow
`app_ui::preview` Adapter owns only Widget conversion. None of them
duplicate clock math, construct a second media key, or reinterpret nesting.

The same current request carries an opaque Frame Demand identity end-to-end.
The preview queue and worker may transport but must not interpret that identity;
the poll Adapter converts the exact result into a terminal Frame Delivery and
only `mondrian-playback` decides whether it still belongs to the active Playback
Session. Decode generation continues to guard media task/cache relevance, but
cannot authorize a transport transition or replace epoch/demand validation.

The app/window layer retains a separate interaction escape hatch: `Loading`
feedback may defer synchronous reconstruction of the same GPU preview candidate
during redraw. This is a presentation-work guard, not transport buffering, and
cannot advance, pause, or select the Clock Master. Transport actions still
cancel obsolete preview generations and refresh controls without synchronously
requesting new preview/composite work.
Background polling must also enforce a hard escape hatch for the current
realtime playback request. If a scheduler-accepted `Current` request for
`PlaybackCursor` or `ScrubCursor` remains pending past the realtime stall
budget, the app preview service expires only that realtime-current pending work,
removes matching queued worker jobs, and clears the current-frame pending flag.
The scheduler returns the expired key, access mode, and original optional Frame
Demand identity instead of requiring the host to inspect current transport.
Only an expired `PlaybackCursor` carrying that identity records
`playback_current_stalled_expirations` and emits an exact Late Frame Delivery;
an expired `ScrubCursor` releases interactive capacity without mutating the
Playback Session or playback-pressure counters. This
is not a renderer fallback and must not clear ready/stale frames,
still-frame work, media caches, or external GPU viewer textures. It exists so a
lost, wedged, or pathologically slow current decode cannot retain scheduler
capacity indefinitely.
Expiration is transport-only feedback: the host refreshes transport/pending
models without synchronously rebuilding Viewer preview content.
The same counter must appear in the preview decode performance summary/report
even before a worker returns a canceled decode result, because the product
symptom is already user-visible at the moment the demand expires.
Expired playback work counts as one late-drop and proxy/hardware recommendation
only when its identity equals the Playback Engine's still-pending demand.
Redundant jobs collapse to that single decision; work that outlives an accepted
presentation releases capacity without reviving or penalizing the completed
demand. Re-requesting the same media key
refreshes stall age only when generation, access mode, or Frame Demand identity
changes; duplicate redraws for one demand cannot keep stalled work alive.
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
4K playback is not mistaken for production readiness. The generic decode report
still records and diagnoses the slowest individual frame, but the continuous
playback hard-failure selector does not let one session-open maximum override a
healthy p95 plus readiness gate. Timeout, queue loss, invalid access modes,
sustained pressure, missing locality, and p95 regressions remain hard failures.

External smoke media is always registered from one real `MediaInfo::probe`;
the harness does not synthesize codec, profile, resolution, bit depth, duration,
frame count, or color facts from a filename. It builds the sequence at the
probed rational frame rate and advances 1× using a nanosecond frame interval.
The media probe canonicalizes positive FFmpeg average rates that fall within
100 ppm of a standard nominal cinema/broadcast rate. This removes container
time-base quantization (for example, an 11 ppm drift around `24000/1001`)
without rounding genuinely distinct or variable rates into a standard cadence.
The dedicated ignored
`preview_media_professional_4k_hevc_main10_hardware_playback_gate` requires
`MONDRIAN_PREVIEW_PROFESSIONAL_4K_HEVC_MAIN10_MEDIA_PATH` and never reports a
skip when that fixture is absent. Before starting GPU work, the harness rejects
sources shorter than the complete observation window; it must never extend a
short clip beyond source EOF and call that professional evidence. Its
`professional_media_gates` require decoder-proven UHD HEVC Main10 identity,
sufficient duration, 23.976/24/25/29.97/30/50/59.94/60 fps, and at least 90%
actual P010/10-bit hardware provenance on media layers attached to completed
headless Viewer GPU candidates. CPU-transfer hardware decode and retained
native hardware decode are reported separately; both are actual hardware
execution, while candidate/config/device probes are not.
Once a GPU Viewer Adapter has admitted a hardware decode request and device
selector, that decision applies to playback, active scrub, and settled still
access modes. Reverting scrub or still requests to `Auto` would silently move a
4K Main10 interaction from the admitted native P010 path back to CPU decode and
is forbidden. CPU-only preview services retain the default `Auto` policy
because no GPU Adapter admission is installed.
UI-independent `app::preview_hardware_admission` stores the renderer/platform
observation as one Copy snapshot (or the explicit pre-discovery `None` state).
Base request, native-surface-specific downgrade, and device selector are always
projected from that same observation; independently updated booleans cannot
manufacture a mixed admission state. The concrete Preview module only projects
this state into its diagnostics and job fields; it owns no admission rule.
When background preview completion changes Viewer lifecycle, the app host may
perform one preview-aware model refresh, then adapt its payload-free feedback
without requesting preview again. A feedback transition must not trigger a
second full root refresh or layout pass that re-enters preview interpretation.
Completion policy itself is not Window-owned. `app::playback_preview` samples
the pending Frame Demand once, asks the production Preview Adapter to combine
bounded completion drain with stalled-current expiration, applies exact terminal
Frame Deliveries, and only then observes video preroll. The Window host and real
Headless GPU harness call this same pump; neither may reproduce this ordering or
interpret worker results independently. The Window still owns repaint/layout,
and the Preview Adapter still owns decode/cache facts, so this seam does not move
Widget or codec responsibilities into the Playback Engine.
The native app event loop also records stage-level responsiveness telemetry for
action draining, redraw, GPU preview preparation, UI refresh, paint/render,
background-task polling, and playback-clock advancement. Any stage that exceeds
the UI responsiveness budget is logged with the stable stage name and elapsed
time. This diagnostic boundary is intentionally in the app layer because it
measures host scheduling and UI-thread residency, not media decode semantics;
media/render changes must preserve it so a future playback report can identify
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
In the app layer this contract lives in `app::preview_access_mode`:
request admission, latest-generation tracking, access-mode promotion, and
completion classification are localized there. The same module owns the bounded
preview decode job queue and worker-lane selection, because those policies are
defined by access mode. It has no dependency on `app_ui`; thumbnail, Viewer,
frame-store, and scheduler-policy Adapters all depend inward on this one module.
The module must not own render plan evaluation, decode
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
non-playback worker lane, `ScrubCursor` jobs are selected ahead of still-frame
jobs even when the still-frame request arrived first. Newly admitted scrub and
still requests retain their Interactive/Still/NonPlayback/Any lane affinity
instead of cold-opening another thread-local hardware decoder on an
incompatible idle worker. This preserves pointer latency, exact-seek locality,
and realtime playback isolation without serializing same-generation work across
compatible shared lanes. A newly admitted
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
expired work (`queued_expired_jobs`) and its playback-current subset
(`queued_expired_playback_current_jobs`) so a slow preview report can
distinguish active queue backlog, missed display deadlines, and codec/decode
cost without inspecting private queue internals. Expired queued work is an
active scheduling warning, not just passive evidence: dequeue must not dispatch
it into normal decode. The Broker emits one structured `DroppedExpired`
outcome, and the media Adapter maps the binding's priority and work class to the
appropriate cancellation vocabulary without re-evaluating the deadline;
unsupported combinations fail closed as unattributed cancellation evidence.
Diagnostics expose generic and playback-current queued totals plus cumulative
dropped totals; this preserves visibility for prefetch and still work without
misreporting every expiration as a missed playback presentation.
The app preview layer must also expose worker-lane eligibility for the same queued jobs
(`queued_playback_lane_eligible_jobs`, `queued_scrub_lane_eligible_jobs`,
`queued_still_lane_eligible_jobs`, and
`queued_non_playback_lane_eligible_jobs`) so reports can distinguish a backlog
that has an idle compatible lane from one waiting behind an occupied or missing
lane. The UI/report layer must consume these queue diagnostics rather than
recomputing lane acceptance from access-mode conditionals.
In-flight execution-lease residency is exposed in the same Broker diagnostic
snapshot and split by priority and access-mode contracts
(`in_flight_current_jobs`, `in_flight_prefetch_jobs`,
`in_flight_playback_cursor_jobs`, `in_flight_scrub_cursor_jobs`, and
`in_flight_random_access_still_jobs`) so diagnostics can separate queued backlog
from work already owned by playback, scrub, or exact still-frame workers. The
Broker records the selected lane when it creates the execution lease; the App
Adapter must not mirror this lifecycle with atomic activity counters. The same
snapshot includes worker-lane residency and
`in_flight_cross_lane_current_jobs`. In normal multi-lane operation cross-lane
current work is an invariant violation; only lanes whose declared acceptance
already spans a class (`Any` or `NonPlayback`) may share it. Persistent
cross-lane evidence therefore points at an Adapter mapping bug, not spare
capacity that should be exploited before codec, color, or GPU analysis.
An execution remains in flight after the worker sends its result and is released
only when the result consumer resolves freshness/terminal ownership (or the
execution is explicitly abandoned). Worker return alone is not lifecycle
completion and must not make residency evidence disappear early.
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
Completion freshness also gates terminal publication: a result may emit
playback Late/Failed only when it is broker `Current` and its captured identity
equals the Playback Engine's still-pending demand. `CacheOnly`, `Stale`, and
broker-current cleanup for a replaced/completed demand may be cached or
discarded according to their completion contract, but cannot refresh visible
state, create playback pressure, or publish another terminal delivery.
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
Decode cancellation crosses the media boundary as a structured Adapter result,
but its authority and policy do not live in the App. Deadline, generation
invalidation, and preemption arrive from `FrameWorkBroker` as one atomic
disposition carrying the earliest applicable monotonic request instant and its
age. Broker closure follows the same rule: the first close instant is immutable
and `BrokerClosed` carries its age. Process worker-stop/join remains a
media-runtime Adapter concern, but its boolean flag cannot classify or timestamp
cancellation. FFmpeg continues to see only a boolean cooperative predicate derived
from the Broker disposition. Before sending a worker
result through the App channel, the Adapter stamps completion in the Broker;
the UI may resolve freshness later but cannot change whether execution met its
deadline. When the worker returns, the Adapter contributes one
`FrameCancellationObservation` to the playback-owned collector: semantic work
class, structured cause, total execution lifetime,
worker-start-to-first-checkpoint, and request-to-first-checkpoint.

The Playback Module derives checkpoint-to-return and owns exact all-run
aggregation for Playback, Interactive, Still, and their rollup. UI diagnostics
only project that immutable report into legacy decode fields and access-mode
views; they do not keep parallel counters or choose thresholds. The shared
fail-closed policy rejects unknown causes, missing request/checkpoint
attribution, impossible timestamp ordering, request-to-checkpoint above 5 ms,
Playback/Interactive return above 50 ms, and
Still return above 500 ms. Total worker lifetime remains diagnostic only:
expensive work completed before cancellation was requested is not evidence of
slow cancellation. This separation keeps authority propagation, codec
checkpoint placement, and cleanup/return independently diagnosable without
teaching the media layer UI intent.

The Broker obtains request ages, expiration, and completion timestamps from
playback's injected `MonotonicRuntimeClock`, with one sample per atomic
lifecycle operation. Immediately before admission the media Adapter pairs its
opaque absolute wall deadline with the remaining duration. The Broker lowers
that duration into its clock once; queueing does not renew it, and the App does
not compare or reconstruct it afterward. Rebinding the same in-flight key
replaces the lowered deadline with the latest binding, while the once-only
worker completion stamp prevents delayed UI polling from inventing lateness.
Broker clock regressions are clamped, counted by regression episode, projected
by UI diagnostics, and rejected by both decode-performance and professional
playback gates.
Process-global decoded-frame cache hits are capped to the same strict frame-hit
tolerance for every access mode. Playback performance must come from the
playback cursor's decoder/session locality, ring buffers, hardware decode, and
GPU-resident frame delivery, not from silently reusing adjacent timestamp
requests as if they were the requested frame.
Container PTS quantization is not itself temporal degradation. Exact playback
and still decode treat a selected frame inside the session's half-frame hit
tolerance as the requested frame; many valid CFR files cannot represent every
ideal rational frame timestamp exactly in their stream time base. A selected
frame outside that tolerance remains degraded. Keyframe-only scrub policy is
stricter: any non-exact keyframe selection is an intentional temporal
approximation and must retain degraded presentation quality.

Current decode residency is intentionally explicit and fail-closed. CPU paths
produce `RgbaFrame`; admitted in-process FFmpeg D3D12VA or D3D11VA playback may
instead produce `PreviewNativeDecodedFrame`. Legacy unowned YUV preview surfaces remain
removed: native output is an owned decoder-resource lease, not a raw plane
container or a handle-kind diagnostic.
`PreviewHardwareDecodeDecision` records the media-layer selection for each
request. A GPU preference must resolve to a structured CPU RGBA reason such as
`CpuRgbaHardwareUnavailable`,
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
exported through a concrete media adapter and imported through the renderer
native decoded-frame import contract,
`HwAccelBackend::probe()` must keep `selected_backend=None`,
`decoder_adapter_available=false`, `hardware_decode_active=false`,
`zero_copy_active=false`, `DecodedFrameResidency::CpuRgba`, no active GPU
handle kind. Platform preference alone is
not a valid hardware decode signal. The Windows media adapter supports FFmpeg's
`AV_PIX_FMT_D3D12` and preferred `AV_PIX_FMT_D3D11` frame ABIs. It does not make
legacy `AV_PIX_FMT_D3D11VA_VLD`, DXVA2, VideoToolbox, VA-API, VDPAU, or CUDA
native automatically; GPU-resident planning skips backend candidates without a
matching concrete media adapter.
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
media, renderer, or platform responsibility. UI-independent
`app::native_video_import` combines renderer native decoded-frame import
support and OS native texture import probing into one
`PlaybackHardwareDecodeAdmission`; Window and Headless composition roots pass
that same value to the concrete Preview Adapter. Preview diagnostics project,
but do not own, its playback request, renderer readiness and supported handle/
source-format counts, platform discovery/zero-copy/low-copy facts, and stable
`PreviewHardwareDecodeAdmissionBlocker` variants such as
`RendererImportUnavailable`, `PlatformDiscoveryUnavailable`,
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
`PreviewDiagnostics`. Export continues to use the source/export contract;
proxy selection is a preview playback scheduling decision, not media color
interpretation.
Alpha-bearing sources bypass the current opaque proxy profiles and GPU-native
YUV preview surfaces. They decode from the source into the typed RGBA input
boundary until a proxy/native format has an explicit alpha-capable contract;
the app must not generate or reuse an opaque proxy for such an asset. Hardware
decode preference is downgraded to the CPU RGBA path for these requests, and an
unexpected opaque native surface fails closed instead of silently discarding
coverage.
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
probed assets propagate `Limited` or `Full` into the proxy contract unless an
asset range override replaces that probe fact. Because range is part of the
proxy contract and sidecar identity, changing the override selects a distinct
proxy path and invalidates reuse of the old artifact. `Unknown`
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
This path remains limited to `RandomAccessStillFrame` requests. It runs the
child with independently drained stdout/stderr pipes, polls the request probe,
and kills, waits for, and joins pipe readers on cancellation; a 4K rawvideo pipe
therefore cannot hide an unbounded child-process wait. `PlaybackCursor` and
`ScrubCursor` stay on in-process decode/session paths for locality and native
residency rather than using the process boundary as a realtime shortcut.
Every frame returned by the preview decode boundary is a
`PreviewDecodeOutcome`: `Frame(RgbaFrame)` for CPU encoded RGBA8 payloads,
`FloatFrame(FloatRgbaFrame)` for CPU scene-linear RGBA f32 payloads, or
`NativeGpuFrame(PreviewNativeDecodedFrame)` for GPU-resident decoder payloads.
Native FFmpeg results use `PreviewDecodePath::InProcessFfmpegNative`; they are
never labeled as the in-process CPU RGBA path.

Scene-linear FFmpeg output must not pass through swscale's RGBA8 boundary.
`GBRPF32LE/BE` and `GBRAPF32LE/BE` frames are unpacked directly from their
declared planar byte order into interleaved `FloatRgbaFrame` storage. Negative
values, values above one, and straight alpha are preserved. Preview resize uses
the float path, and both the playback ring and process-global frame cache retain
the payload kind. Unsupported scene-linear decoder formats fail closed instead
of silently quantizing. App preview, thumbnails, and export route this outcome
through the renderer's `LinearFloatSource` input contract.
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
`av_frame_free` when the final lease drops. D3D12 residency reads FFmpeg's
documented `AVD3D12VAFrame` descriptor from `AVFrame::data[0]`: its borrowed
`ID3D12Resource`, decode-completion `ID3D12Fence`, and fence value remain owned
by the retained AVFrame. A renderer must wait for that value before reading the
resource; media never performs a CPU fence wait. Preferred D3D11 residency
reads only FFmpeg's documented `AV_PIX_FMT_D3D11` ABI: `AVFrame::data[0]` is the borrowed
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
For D3D12 and preferred D3D11 frames, media reads `AVFrame::hw_frames_ctx` and
accepts only explicit `AV_PIX_FMT_NV12` or `AV_PIX_FMT_P010LE` software layouts. Planar
`AV_PIX_FMT_YUV420P` / `AV_PIX_FMT_YUV420P10LE` descriptions are not silently
reinterpreted as two-plane GPU textures. A GPU-preferred CPU fallback records
`PreviewNativeDecodeFallback` as `SoftwareFrame`,
`ResourceAdapterUnavailable`, `SurfaceFormatUnavailable`,
`SamplingMetadataIncomplete`, or `ResourceRetentionFailed`. A required-GPU
request returns a decode error for the same condition instead.
On multi-adapter Windows systems, a native-import admission attaches the typed
`D3D12VaAdapterIndex` selector derived from the renderer's physical DXGI
adapter. Media includes it in decoder-session identity and passes its decimal
index only to FFmpeg's D3D12VA `av_hwdevice_ctx_create` call; backend-specific
selectors cannot silently select a different hardware API. D3D11VA remains
available when no renderer-native selector is installed, primarily as a safe
hardware-decode CPU-transfer fallback. Device probes are cached by backend plus
selector. The renderer still validates every decoded D3D12 resource's LUID, so
selection prevents accidental cross-adapter creation without weakening the
native resource boundary.
GPU-resident decoder setup reserves thirty-two FFmpeg `extra_hw_frames` before
`avcodec_open2` because native frames remain leased after the receive call.
This is decoder-pool headroom, not application cache capacity; CPU-transfer
decode leaves the setting at zero because it exports no hardware surfaces.
GPU-resident requests bypass the process-global CPU RGBA cache and the
session-local RGBA playback ring. Native decoder surfaces are not inserted into
either media-owned CPU cache. They may enter the App's playback-owned Preview
Frame Store as opaque leases charged one decoder-resource unit each; the App
composition root sets that resource-unit budget to at least the maximum bounded
prefetch window. This permits useful forward residency without treating a
zero-host-byte surface as free or allowing the Store to exhaust the decoder
pool. CPU fallback payloads remain eligible for the existing CPU cache policy.
CPU consumers such as thumbnails and current RGBA fallback paths must explicitly
match `Frame(RgbaFrame)` and fail closed on `NativeGpuFrame`; they must not
reinterpret a native decoder surface as RGBA or silently force a CPU transfer.
The app viewer preview path preserves `NativeGpuFrame` as a native source
payload and passes the complete frame, including its opaque handle token and
residency/sampling facts, into GPU preview admission. App adapters must not
flatten that payload into diagnostics and discard the token. On Windows DX12,
admitted D3D12VA NV12/P010 resources remain GPU-resident through the renderer's
same-API shared-texture bridge and OCIO input stage. Other native handle
families remain renderer-readiness blockers unless their concrete backend is
implemented; they are not media decode failures or implicit CPU fallback
frames.
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
Every Viewer GPU Adapter must preserve those media facts in its frame-residency
telemetry. Media decode diagnostics feed the renderer-owned
`ViewerGpuMediaSource` contract, while a retained native payload feeds
`ViewerGpuNativeSource`. App product admission combines decoder
residency/handle/format with the platform import probe and renderer import
support. This does not make CPU RGBA preview hardware
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
`ScrubCursor` derives its effective selection/decode budget per request from
that evidence: probe-backed anchors use bounded approximate-first-frame selection,
missing or session-only evidence gets a bounded responsiveness-first budget, and exact
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
generations before enqueueing the current work. One continuous Playback Epoch
retains its generation across ordinary frame advances: frame identity belongs
to the media request key, while seek/restart or another interpretation
discontinuity rotates the generation. Fresh same-generation prefetch
remains eligible only after the current frame is not pending, no current-frame
job is already queued or running, and queued plus in-flight prefetch is below
the forward window. It then tops up only the unfilled queued-plus-in-flight
prefetch budget while traversing tracks and nested sequences. That lets playback
warm nearby frames without stealing first-frame or recovery budget. This worker
pool and pruning are scheduling guardrails; hardware-resident decode remains a
separate execution concern.
Preview decode exposes a cooperative cancellation boundary for every access
mode: app workers pass a generation-aware request probe to the media decoder.
Each thread-local FFmpeg session installs that probe as an
`AVIOInterruptCB`, including before input open and stream discovery, and keeps
it active across seek and packet I/O. The media loop also checks it before
opening, seeking, packet decode, frame receive, EOF draining, hardware transfer,
and RGBA conversion. If cancellation fires, the decoder returns a
typed canceled outcome rather than a media failure. `PlaybackCursor` and
`ScrubCursor` cancellation preserve their thread-local FFmpeg sessions so
sustained playback, forward prefetch, and pointer dragging retain decoder/device
residency; `RandomAccessStillFrame` cancellation discards only its mode-specific
session. The experimental external-process CPU RGBA path follows the same
policy and terminates/reaps its child when the probe fires. This keeps stale
work from being cached or marked as a failed source while preserving independent
playback, scrub, and still-frame session state for subsequent requests.
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
arrive while playback or current-frame work is active, the app host must cancel obsolete
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
service from the coherent `app::native_video_import` snapshot. The media
request default remains `PreviewHardwareDecodeRequest::Auto`; Window or
Headless composition code may raise playback jobs to `PreferHardwareDecode` for
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
`RgbaFrame` and `FloatRgbaFrame` store their payloads in shared immutable memory
so cache hits can adjust per-request diagnostics without deep-copying a 4K
frame. Callers that need ownership must request it explicitly through the frame
consumption API; renderer color-frame boundaries should prefer shared payloads
where their typed input contract permits it.
Execution truth is separate from per-request cache diagnostics.
`PreviewDecodeExecutionPath` is assigned only from an observed FFmpeg hardware
CPU transfer or a validated native GPU payload and remains unchanged when the
request becomes `PreviewCacheHit` or `PlaybackSessionRingHit`. App prefetch and
the Preview Frame Store retain this provenance, aggregate it across nested/media
layers, and bind it to the exact GPU candidate. A cache hit therefore describes
the current request without pretending another hardware decode occurred.
`PreviewNativeDecodedFrame` is the separate GPU-resident payload contract and
must flow toward renderer native decoded-frame import rather than the RGBA cache.
For an admitted opaque NV12/P010 native decode, cache and in-flight identity use
the probed source raster, not the current Viewer presentation scale. Native
surfaces are source-sized and Half/Quarter quality is a later Viewer spatial
operation; including that output extent in the decode key would invalidate
useful prefetch whenever adaptive presentation scale changes. CPU decode keeps
its requested decode extent because scaling is part of that media operation.
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
Packaged runtime verification also checks FFmpeg's public decoder registry for
PNG and OpenEXR. Windows CI, release, and developer setup must install
`ffmpeg[zlib]`; a `libavcodec.pc` file alone is not evidence that these decoders
were compiled. The vcpkg step is idempotent and uses `--recurse` so an older
cache with the default component set is upgraded instead of silently reused.
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
interactive lane workers. Thread-local preview decode sessions are kept alive
across short gaps for playback locality, released automatically by the app
worker after two seconds idle, and also released through
`clear_thread_local_preview_decode_session()` at explicit lifecycle boundaries
such as perf probes, media/project shutdown, or tests that open threaded
software decoders. The idle release is resource policy, not a cache-key or
generation change.
Codec safety policy may narrow these diagnostic overrides. OpenEXR contexts are
always serial (`None`, one decoder thread): FFmpeg's frame-threaded EXR path can
hold the single image until EOF and deadlock during codec-context destruction
on Windows. Independent image requests remain parallel at the app decode-pool
level, so this does not serialize the media pipeline globally.

The renderer now owns a GPU input-stage resource contract for decoded CPU RGBA8
source frames: upload to `Rgba8Unorm`, execute the OCIO GPU input transform, and
produce a GPU-resident linear working frame in a float texture. This is the
bridge for guarded rollout of GPU input transforms. The app viewer uses this
contract for supported media preview layers before GPU working-space
compositing, falling back per-layer to CPU working-frame upload only when the
GPU input stage cannot be recorded. It is not yet a hardware decode or
zero-copy media path because CPU RGBA8 and scene-linear float outcomes still
hand CPU memory to the renderer. The float outcome currently enters the CPU
OCIO input stage; a future GPU float-source upload contract must remain distinct
from the RGBA8 encoded-source upload.

## Asset Classification

`mondrian-assets` classifies imported files using `MediaInfo`. Audio-only extensions or media without meaningful video streams become `Audio`; media with video becomes `Video`.

Synthetic assets use `MediaInfo::synthetic_adjustment_layer()` and `MediaInfo::synthetic_solid_color()`.

## Color Metadata

Media probe separates detected metadata from policy assumptions.
`VideoStreamInfo.detected_color_space` is the transform-facing detected-only
index. `VideoStreamInfo.color_interpretation` is the diagnostic/UI-facing
interpretation with confidence, evidence, warnings, and a user-overridable flag.
Evidence records whether a result came from a camera/log metadata hint, a
complete file-name pair, complete CICP colorimetry, partial CICP tags, ICC,
unsupported CICP tags, or decoder unavailability.
`interpret_video_color_metadata(...)` owns the selection policy so decoder
integrations do not duplicate it.
Camera/log hints resolve only when they identify both the transfer curve and
the associated camera gamut. For example, `S-Log3 / S-Gamut3.Cine` is a
supported exact identity, while bare `S-Log3` remains unresolved. The same rule
applies to ARRI, Canon, Panasonic, RED, Blackmagic, DJI, Apple, and DaVinci
camera families. File names participate only when they contain the complete
pair, and remain low-confidence descriptive evidence; bare names such as
`Slog3` or `Log3G10` do not invent a gamut. This prevents a plausible-looking
but incorrect primary conversion from being hidden behind a generic log label.
ICC profile names are display-profile evidence and are never promoted to camera
input identities.
Selection is deterministic: declared stream metadata outranks declared
container metadata; either is high confidence and may override conflicting
CICP while retaining a warning. Exact CICP outranks free-form/container comments
and file-name inference. Partial CICP and mapped ICC are medium confidence;
descriptive text and complete file-name pairs are low confidence and are used
only when no stronger usable evidence exists. An unmapped ICC profile is
retained as evidence and warning but does not suppress a complete low-confidence
hint.
Warnings preserve machine-readable provenance, not only a resolved color-space
enum: multiple-hint warnings keep the selected and ignored metadata keys,
values, and scopes; hint-vs-CICP warnings keep the selected hint and the raw
CICP triplet that conflicted with it; lower-priority warnings record the method
that won and every conflicting descriptive hint it rejected.
`VideoColorDiagnostic::summary()` is the
stable compact form for logs/export errors and should include those warning
details. `VideoColorDiagnostic::issue_summary()` is the machine-readable
contract for UI, telemetry, smoke JSONL, and export reports; callers must
consume its counters and flags instead of parsing the compact summary string.
The summary schema is strict: new evidence counters are required fields rather
than serde-defaulted compatibility values, so stale reports fail visibly.
`VideoColorDiagnosticIssueAggregate` is the shared rollup for combining many
per-stream diagnostics into one report surface.
Clip-level `MediaInterpretation` can override color space, frame rate, pixel aspect ratio, field order, and alpha interpretation.

Asset library records store persistent user intent separately as
`AssetMediaInterpretation`. Imported media defaults to `Auto` for both color
identity and encoded signal range. Color Auto means "resolve from current
metadata, detector, and project color policy" and must not persist the currently
resolved color space. Range Auto follows the current probe result. User changes
from the asset-library Interpret Footage dialog are stored independently as
`MediaColorInterpretation::Override { color_space }` and
`MediaRangeInterpretation::Override { Full | Limited }`; both remain stable
across metadata re-probes, relinks, and detector upgrades.
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
Preview, thumbnail, proxy, and export decode contracts resolve encoded range
with one separate precedence rule: explicit asset range override, then probed
range, otherwise `Unknown`. `Unknown` continues to fail closed at YUV conversion
or proxy-generation boundaries.

Unknown/missing metadata policy is resolved at sequence color-management time, not by UI panels.
