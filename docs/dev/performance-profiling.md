# Performance Profiling

## Areas

- UI layout/event/paint command count
- text layout and atlas churn
- vector icon raster/cache behavior
- timeline render-plan construction
- media decode/cache hit rate
- effect graph compilation/cache behavior
- GPU texture allocation and pooling
- compositor submissions/readbacks
- export queue throughput

## Rules

- Measure before optimizing.
- Track cache keys and invalidation.
- Avoid hidden GPU readback.
- Prefer batched submissions where semantics allow.
- Keep UI hover/focus updates local and cheap.

## Existing Smoke Tests

```powershell
$perfRun = "target/perf/manual-$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())"
New-Item -ItemType Directory -Path $perfRun | Out-Null
$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'project-lifecycle.jsonl'; cargo test -p mondrian-app --release -j 2 --lib perf_project_lifecycle_smoke -- --ignored --nocapture --test-threads=1
$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'app-ui-scale.jsonl'; cargo test -p mondrian-app --release -j 2 --lib app_ui_scale_smoke -- --ignored --nocapture --test-threads=1
$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'preview-media.jsonl'; cargo test -p mondrian-app --release -j 2 --lib preview_media_decode_cache_smoke -- --ignored --nocapture --test-threads=1
$env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'preview-playback.jsonl'; cargo test -p mondrian-app --release -j 2 --lib preview_media_continuous_playback_smoke -- --ignored --nocapture --test-threads=1
$env:MONDRIAN_EXPORT_SIM_OUTPUT=Join-Path $perfRun 'export-1080p2997.jsonl'; cargo test -p mondrian-export --release -j 2 --lib export_1080p2997_simulated_perf -- --ignored --nocapture --test-threads=1
$env:MONDRIAN_AUDIO_LOAD_MATRIX_OUTPUT=Join-Path $perfRun 'audio-load-matrix.jsonl'; cargo test -p mondrian-audio --release -j 2 --test load_matrix dense_schedule_multitrack_load_matrix -- --ignored --nocapture --test-threads=1

$env:MONDRIAN_PREVIEW_EXTERNAL_MEDIA_PATH='<external-media-path>'; $env:MONDRIAN_PERF_OUTPUT=Join-Path $perfRun 'preview-external-media.jsonl'; cargo test -p mondrian-app --release -j 2 preview_media_external_access_mode_smoke -- --ignored --nocapture --test-threads=1
$env:MONDRIAN_PREVIEW_DECODE_FIXTURE='<external-media-path>'; $env:MONDRIAN_PREVIEW_DECODE_TIMESTAMP='1.0'; $env:MONDRIAN_PREVIEW_DECODE_MAX_WIDTH='1920'; $env:MONDRIAN_PREVIEW_DECODE_MAX_HEIGHT='1080'; cargo test -p mondrian-media --release -j 2 preview_decode_fixture_perf_smoke -- --ignored --nocapture --test-threads=1
$env:MONDRIAN_RENDERER_GPU_OUTPUT_SMOKE_OUTPUT=Join-Path $perfRun 'renderer-gpu-output.jsonl'; cargo test -p mondrian-renderer --release -j 2 gpu_output_boundary_runtime_smoke_report_on_real_wgpu_device -- --ignored --nocapture --test-threads=1
$env:MONDRIAN_COLOR_VIEW_GPU_PERF_OUTPUT=Join-Path $perfRun 'color-view-4k.jsonl'; cargo test -p mondrian-renderer --release -j 2 --test color_view_gpu_perf standard_views_4k_gpu_timestamp_meet_budget_and_beat_aces2 -- --ignored --nocapture --test-threads=1
$env:MONDRIAN_COLOR_TRANSFORM_GPU_PERF_OUTPUT=Join-Path $perfRun 'color-transform-4k.jsonl'; cargo test -p mondrian-renderer --release -j 2 --test color_view_gpu_perf standard_input_transforms_4k_gpu_timestamp_meet_budget -- --ignored --nocapture --test-threads=1
```

Every smoke must use a fresh report path. The two short Preview smokes generate
their own copyright-free media and run the production media path; they are not
substitutes for the reference-validation runner's real 4K HEVC Main10, device
clock, long A/V synchronization, memory, and GPU-presentation gates. A test
process that exits successfully after reporting `skipped` has not produced
eligible performance evidence.

The reference audio gate uses report profile
`cpal_av_48khz_30min_recovery_v2` and must be built and tested with the
`validation` feature. It qualifies one real CPAL generation with one second of
stable Audio Device Clock/active-callback residency, then begins Playback
Evidence before requesting an exact-current controlled recycle. The gate
requires one retained `ControlledRecycle` loss with a post-drop frozen inactive
callback snapshot and exact 48 kHz media anchor, Synthetic Clock fallback
within one second, one newer opened generation, and Audio Device Clock/active
phase handoff within five seconds followed by one stable second. Lifecycle
deltas must be exactly one open, one loss, and one controlled recycle;
backend-loss and deactivation-failure deltas must remain zero. Only after that
handoff does the gate reset its Viewer counters and measure 30 continuous
minutes on the final generation, so recovery work cannot satisfy the final
callback or headless-GPU duration requirement.

Playback Evidence schema 4 replaces the old target-versus-latest-clock drift
aggregate with completion-time phase evidence partitioned by Clock Master. The
CPAL recovery gate requires Audio Device and Synthetic point-error,
uncertainty, and conservative proven-error samples, a proven maximum of at most
20 ms for both masters, and zero running presentable deliveries without a
proven phase. `phase_not_applicable` remains diagnostic for paused/still
deliveries and does not fail this gate. The bounded event tail is not lifecycle
authority: stream-loss/reopen qualification comes from retained aggregate
counters and the frozen last-loss record.

Use `scripts/perf/run-perf-suite.ps1` for the generic suite rather than treating
the direct commands above as one coordinated gate. It applies a 2,700-second
wall-clock deadline to every Cargo child by default (override only with the
positive `-ProcessTimeoutSeconds` parameter), drains both output streams, and
terminates the complete descendant process tree on timeout. Per-child logs are
retained under the run's `cargo-logs` directory, and a timeout is always an
explicit suite failure rather than a missing report interpreted as success.
Long Preview and CPAL reports use `product_process_tree_private_commit_v2`.
Their JSON evidence must identify `product-process-tree` scope, the Windows
Tool Help/Process Status Backend, a non-zero observed member-count range, and a
complete inventory for every cadence and post-stress sample. This intentionally
charges isolated demux and FFmpeg audio children to the same 4 GiB/plateau
contract. `CurrentProcess`, an unsupported Adapter, a changing inventory after
bounded retries, or one failed member query is a failed gate—not a zero-byte or
smaller sample. Working Set and summed member lifetime peaks remain diagnostic;
aggregate Private Commit is the acceptance metric.
For `app_ui_scale`, both `preview_color_report` and
`preview_playback_color_report` must be present and carry a recognized non-Fail
verdict; the Rust producer also writes both structured reports before rejecting
a missing-evidence or Fail result.

The color-View gate requires a timestamp-capable real adapter. It rotates 60
warm 4K samples each for Mondrian Standard SDR, PQ, HLG, and the official ACES
2 1000-nit PQ preset over the same non-empty GPU-resident working frame. All
three Standard Views must satisfy the absolute p95 budget; the PQ result is also
compared with the like-for-like ACES PQ reference. Its JSONL record
contains GPU and CPU-record p50/p95/p99, cold initialization, raw OCIO shader
bytes, LUT dimensions/interpolation, pass/write/upload/readback counts, adapter
identity, and relative performance. Schema 4 also snapshots runtime counters
immediately before and after the measured samples. Its warm-path gate requires
zero measured shader extraction, static-pipeline preparation, concrete backend
object preparation, wrapper input bind-group creation, texture allocation, or
pool eviction; wrapper-binding and output-texture pool hits must cover every
measured View sample. This distinguishes a fast steady-state pass from a run
that hides recurring GPU object creation behind acceptable timestamp results.
Defaults are
`MONDRIAN_STANDARD_HDR_4K_P95_US=5000`,
`MONDRIAN_STANDARD_HDR_TO_ACES_P95_RATIO=0.8`, and 60 samples; sample count may
be set with `MONDRIAN_COLOR_VIEW_GPU_PERF_SAMPLES` (20..500). The synchronous
timestamp mapping is part of this offline gate only and is outside the measured
GPU interval; production presentation continues to use the asynchronous query
ring.

The separate color-transform gate rotates Identity, Linear Rec.2020 to encoded
Rec.709 (the matrix + OETF class), encoded Rec.709 to the project working
space, and Sony S-Log3/S-Gamut3.Cine to the working space. It uses the
production intermediate and GPU-resident input-stage recorders rather than a
benchmark-only shader. Source textures are allocated and initialized before
measurement; each timestamp contains exactly one OCIO fullscreen pass and no
upload or readback. The schema-1 report records p50/p95/p99, cold CPU record
cost, shader/LUT resources, exact processor identities, and measured cache
deltas. Every transform has a default 4K p95 budget of 5 ms, configurable with
`MONDRIAN_COLOR_TRANSFORM_4K_P95_US`; sample count is controlled by
`MONDRIAN_COLOR_TRANSFORM_GPU_PERF_SAMPLES` (20..500). Its warm-path gate also
requires every measured output to come from the texture pool and every wrapper
input binding to be reused. Both 4K gates are ignored manual hardware tests;
ordinary `cargo test -p mondrian-renderer --test color_view_gpu_perf` runs only
their fast report and gate-logic coverage.

Export simulation reports include a versioned `color_report`. The report embeds
the export color diagnostics summary and adds verdict, fixed checks, root
causes, and actions. The same constructor is used by export job diagnostics and
perf artifacts, so tooling should key off `color_report.verdict`, `checks`,
`root_causes`, and `actions`, not off ad hoc summary interpretation. Default
export smokes fail closed unless export stays fully float/linear, has no GPU
blockers, has no upload/readback transfer stages, reports no structured legacy
RGBA8 reasons, and has at least one diagnosed frame. The old `color_health*`
perf fields are not part of the report contract.

These simulations are hot-Session CPU Float32 compositor kernel gates, not
end-to-end Export throughput claims. They compile the immutable identity Effect
program once and retain one job-local `TimelineCompositeScratch`; decode,
Prepared Visual closure construction, root output conversion, encode, and
publication require separate production-loop evidence. A reported
`passthrough_frame` is counted only from
`TimelineCompositeExecutionDiagnostics::zero_copy_identity_passthroughs`.
Layer count, opacity heuristics, or pixel equality do not prove that the
zero-copy production route executed. The named `export-4k60-simulated` gate
likewise fixes its workload at 3840×2160, 60 fps, two Normal/identity media
layers, and at least 16 measured frames. Environment variables may lengthen the
sample window or tighten TTFF/FPS thresholds, but cannot substitute a smaller
frame, one layer, or a lower target rate under the same scenario name. Release
execution keeps the complete output observable through `black_box`, requires
fusion evidence for every frame, and compares deterministic output samples with
the canonical scalar source-over oracle; both execution and pixel evidence must
pass. The suite parser independently rejects a missing pixel oracle, non-Pass
color report, altered 4K workload identity, or incomplete fusion count, even if
an artifact claims `passed=true`.

Renderer GPU output smoke reports include a versioned `health_report`. That
report is the sole renderer-side native GPU output contract: tooling should key
off `health_report.verdict`, `checks`, `root_causes`, and `actions`, not off
legacy `health`, `health_failures`, or `passed` fields. Default renderer smoke
qualification is fail-closed: the report must not be skipped, the native GPU
stage sequence must be complete, GPU blockers must be absent, backend runtime
and shader cache must be ready, readback must be byte-complete, and CPU/GPU
parity must stay within tolerance. The ignored Rust test is a report producer:
when no adapter is available it deliberately emits a structured `skipped`
report and exits successfully. Therefore raw `cargo test` status is not a GPU
qualification verdict; a gate runner must parse the requested output and reject
missing or skipped evidence. The schema is owned by
`mondrian-renderer::RenderGpuOutputHealthReport` and its companion
frame/stage/runtime report types, so perf tooling should not redefine the JSON
shape locally.

Preview media smoke reports include `preview_color_report` and
`media_color_issues`. The decode/cache and continuous-playback smokes fail
closed unless preview emits color-health diagnostics, stays fully float/linear,
has no GPU blockers, has no upload/readback transfer stages, reports no
structured legacy RGBA8 reasons, and has no missing-metadata policy rejections.
`media_color_issues` is the machine-readable rollup of active-sequence asset
diagnostics, including missing metadata, decoder-unavailable assets, hint
conflicts, and HDR side-data presence. The old preview `*_budget`, `*_passed`,
and `*_failures` perf fields are not part of the report contract.

`preview_decode_fixture_perf_smoke` is a focused media-layer probe for a real
file. It bypasses app UI scheduling and reports one
`MONDRIAN_PREVIEW_DECODE_PERF_JSON` record with the in-process FFmpeg preview
decode path, output dimensions, RGBA byte count, elapsed time, cache-hit flag,
CPU-residency flag, whether the request performed a decoder seek, and how many
frames FFmpeg decoded before selecting the output frame. It also reports the
in-process FFmpeg decoder threading mode/count. Use it when a real 4K/HDR file
feels slow to distinguish long-GOP seek/decode/scaling cost from later app UI,
OCIO, compositor, or viewer-output cost.

Preview software decode defaults to the coordinated `PreviewDecodeCpuBudget`:
the app preview worker count and FFmpeg decoder threads per worker are sized
together so software decode does not multiply independent thread pools. Worker
count is always derived from that budget. Only FFmpeg's threading mode and its
bounded per-worker decoder-thread count can be overridden for profiling:

```powershell
$env:MONDRIAN_PREVIEW_DECODE_THREADING='slice' # none | frame | slice
$env:MONDRIAN_PREVIEW_DECODE_THREADS='4'      # FFmpeg decoder threads per worker
```

App preview diagnostics also report proxy path resolution:
`media_proxy_path_hits`, `media_proxy_path_misses`,
`media_proxy_path_stale`, and `media_proxy_path_bypasses`. A 4K/HDR clip that
must produce single-frame preview in tens of milliseconds should either hit a
fresh proxy path or a real hardware-decoded GPU residency path; repeated source
decode with `seek_performed=true` and high `decoded_frame_count` is expected to
remain CPU-bound.

`decode_stage_durations` breaks software preview decode time into session open,
cache lookup, seek, packet/decode, FFmpeg software scale, RGBA copy, and
external-process wait time. Use these counters to classify slow frames before
changing color/render code: high `packet_decode_us` usually points at codec/GOP
or hardware-decode work, high `seek_us` points at random-access/indexing/proxy
work, and high `swscale_us`/`rgba_copy_us` points at the CPU RGBA boundary.
Preview media perf artifacts also include a versioned `preview_decode_report`
with checks, root causes, actions, and the exact serialized policy. Schema 33
does not gate one mixed access-mode latency aggregate. It first records the
Session disposition (`Opened`, `Replaced`, `Reused`, or `BypassedCache`) and
then partitions every successful result into exactly one work class:
`CacheHit`, `SessionOpened`, `SessionReplaced`, `ForwardSteady`, `ReusedSeek`,
`ReusedOther`, or `Unclassified`. Missing/double accounting and
`Unclassified` samples fail capture integrity.

The default recurring worker-execution budget is 50 ms for cache, forward
steady, and non-seeking reuse; reused seek is bounded separately at 500 ms.
Session open/replacement has a 5 s absolute fail-safe but is not included in
steady p95. Required Playback profiles need at least four real forward-steady
samples, while required Scrub and Still profiles need at least one reused-seek
sample. Repeated opens/replacements are constrained by the report's session
churn policy even when each open is individually below 5 s. A histogram p95
whose rank lands in the open greater-than-5-second bucket fails; reports never
invent a finite upper bound.

Access-mode profiles include `queue_wait_max_us` and
`queue_wait_total_us`; high values there point at worker-lane contention or
stale prefetch/current admission before codec, color, or render work. The
preview media smokes fail when the access modes exercised by that scenario have
queue waits over the decode slow-frame budget.
For slowest-frame bottleneck attribution, use `max_frame_queue_wait_us` together
with `max_frame_stage_durations` and `max_frame_bottleneck`; those fields come
from the same decoded frame. `queue_wait_max_us` may come from another job and
is worker-pressure evidence, not automatically the slowest frame's bottleneck.
Access-mode profiles also include seek-strategy counters. `ScrubCursor` samples
must use `bounded_any_seek_strategy_frames`; if scrub frames show
`keyframe_seek_strategy_frames`, the access-mode routing is wrong and the test
should fail before anyone tunes codec threads, proxy thresholds, color, or
renderer code.
`preview_decode_report` schema 37 records the media-layer policy contract
observed by each access mode: `forward_reuse_frame_window_max`,
`forward_decode_budget_frames_max`, and `any_seek_window_ms_max`. A healthy
`ScrubCursor` sample must have a non-zero `any_seek_window_ms_max`; otherwise
the report fails with `preview_decode_scrub_cursor_any_seek_window_ms` because
the UI would be claiming low-latency bounded-any seeking without carrying the
actual bounded seek window through diagnostics.
For `ScrubCursor`, `forward_decode_budget_frames_max` is the maximum effective
per-request budget observed after media adapts the base scrub policy from
probe-backed/session-observed seek-index evidence. It is not a static constant:
low values on successful near-keyframe scrub frames are healthy, while budget
exhaustion counters mean the scheduler needs proxy, hardware decode, or better
GOP/index evidence rather than more UI patience.
The same profiles expose session-local seek-index counters:
`seek_index_available_frames`, `seek_index_used_frames`,
`seek_index_keyframes_max`, `seek_index_observed_packets_max`,
`seek_index_probe_backed_frames`, and `seek_index_session_observed_frames`.
Slow scrub with `seeked_frames > 0` and no `seek_index_available_frames` means
the decoder is seeking without keyframe/GOP evidence and should be improved by
index/proxy/hardware-decode work, not by hiding the problem in UI timeouts.
`seek_index_probe_backed_frames > 0` means media seeded the session from the
container/probe stream index or the media-layer path/fingerprint/stream cache.
`seek_index_session_observed_frames > 0` means keyframe anchors were learned
only after decoding packets; this is useful evidence, but not a substitute for
fast open-time probe-backed GOP maps on scrub-heavy media.
`seek_index_available_frames > 0` with low `seek_index_used_frames` means the
current bounded-any seek window could not use the known anchors; inspect
`seek_us`, `packet_decode_us`, and the seek-window policy before increasing
worker counts.
Access-mode profiles also aggregate the hardware decode contract:
`hardware_decode_active_frames`, `zero_copy_active_frames`,
`gpu_texture_resident_frames`, `decoded_nv12_surface_frames`,
`decoded_p010_surface_frames`, and
`hardware_decode_texture_residency_blocker_frames`. Renderer/platform native
import readiness is reported by app playback admission diagnostics, not by
media access-mode profiles. For current alpha builds, a
healthy honest CPU fallback will usually show zero hardware/zero-copy frames,
non-zero texture-residency blocker frames, and possibly NV12/P010 surface
candidate counts. Do not interpret NV12/P010 candidates as hardware playback;
they only identify sources that are good targets for the native GPU residency
path.
Cancellation counters are also split inside each access-mode profile; use those
fields to identify whether obsolete, prefetch-deadline,
prefetch-preempted-by-current, still-preempted-by-realtime-current, shutdown,
or unknown cancellations came from playback, active scrub, or still-frame work.
`canceled_playback_deadline_jobs` is different from
`canceled_prefetch_deadline_jobs`: playback-deadline cancellations mean a
visible `PlaybackCursor` current frame missed its display deadline and was
dropped before or during decode, while prefetch-deadline cancellations mean
speculative cache warming exceeded its budget. The playback deadline is derived
from the active sequence frame duration, with conservative min/max bounds, so
slow playback at 24 fps and 60 fps playback are judged against different
budgets. Treat playback deadline pressure as a reason to improve proxy/hardware
decode/drop policy, not as a reason to increase speculative prefetch.
`preview_decode_report` schema 37 also includes `playback_schedule`, the
app-owned playback-clock contract used by the scheduler. It records the last
current-frame deadline budget, the dynamic forward-prefetch horizon/window, and
invalid frame-rate counters. It also records `current_decode_decisions`,
`current_drop_late_decisions`, and
`current_proxy_or_hardware_recommended_decisions` so playback can be analyzed as
a clock-driven scheduler instead of a best-effort decode queue. Checks
`preview_decode_playback_deadline_invalid_frame_rate` and
`preview_decode_prefetch_window_invalid_frame_rate` warn when a sequence frame
rate prevents playback deadlines or prefetch cache warming from being derived.
Prefetch preemption means the app protected visible current-frame work from
in-flight playback speculation. Still preemption means deterministic still-frame
work yielded to playback or scrub current-frame work. Both are scheduling
pressure signals, not media decode failures.
During playback, Viewer `Loading` and stale-frame states are nonterminal
presentation evidence. They never create UI-owned buffering or stop the Audio
device Clock Master; after device loss the Synthetic monotonic Clock Master
continues instead. Video scheduling follows that clock, drops late work, and
accepts presentation only for the current exact ticket. Persistent
Loading/Stale, late drops, or `Priming`/`Recovering` should be diagnosed
separately from `preview_decode_report`, `preview_render_report`, transport
evidence, and Viewer state counts; do not hide them by widening timeouts or
reintroducing a Viewer-to-clock control path.
`preview_media_decode_cache_smoke` intentionally exercises both settled
non-playing seeks (`RandomAccessStillFrame`) and active playhead dragging
(`ScrubCursor`). The active scrub window is controlled by
`MONDRIAN_PREVIEW_MEDIA_SCRUB_READY_MS`; do not replace it with plain
`state.seek(...)`, because that would silently stop profiling the latest-wins
scrub path. The smoke fails closed when either `ScrubCursor` or
`RandomAccessStillFrame` has no successful profile samples; a profile schema
without exercised samples is not acceptable coverage.
For real 4K HEVC/HDR decode fixtures, run the ignored
`preview_decode_fixture_sequence_perf_smoke` with
`MONDRIAN_PREVIEW_DECODE_FIXTURE`, `MONDRIAN_PREVIEW_DECODE_SEQUENCE_FRAMES`,
and `MONDRIAN_PREVIEW_DECODE_P95_BUDGET_US`. The JSON report includes
`p95_us` and `uncached_p95_us`; when the budget variable is set, the test fails
if `p95_us` exceeds that gate.
Pass an exact rational such as
`MONDRIAN_PREVIEW_DECODE_FRAME_RATE='60000/1001'` for frame-by-frame codec
comparisons. A decimal remains supported for approximate diagnostics, but its
microsecond input quantization can select duplicate or skipped source samples
and must not be used to compare decoder/demux implementations.
Set `MONDRIAN_PREVIEW_DECODE_COMPACT_YUV=1` to retain the same planar CPU YUV
payload used by the production GPU upload path instead of expanding each frame
to CPU RGBA. To measure only the process-isolated format-I/O boundary, also set
`MONDRIAN_PREVIEW_DEMUX_WORKER_PATH` to a freshly built packaged Mondrian
executable; leave it unset for the otherwise identical in-process
`AVFormatContext` baseline. The report identifies `demux_mode`,
`representation`, and the isolated worker's lifecycle/packet evidence so these
two runs cannot be mistaken for different output contracts or a stale helper.
`preview_media_external_access_mode_smoke` runs the same access-mode probe and
gates against a caller-supplied real media file via
`MONDRIAN_PREVIEW_EXTERNAL_MEDIA_PATH`. Use it for 4K HEVC/HDR, camera originals,
and other slow-path samples; tune only its `MONDRIAN_PREVIEW_EXTERNAL_MEDIA_*`
thresholds so the generated fixture smoke remains a stable fast regression gate.
The external access-mode smoke registers the file with lightweight caller-owned
`MediaInfo` instead of running synchronous import metadata probing; this keeps
the benchmark focused on decode scheduling, preview readiness, color/render
handoff, and access-mode diagnostics. Metadata probe/import latency needs its
own bounded smoke and must not be hidden inside this access-mode gate.
`MONDRIAN_PREVIEW_EXTERNAL_MEDIA_TOTAL_TIMEOUT_MS` bounds the preview/access-mode
portion of the external probe once lightweight registration has completed, so
a pathological decode or scheduler path fails with preview diagnostics instead
of leaving the test process running for many minutes.
Continuous playback smoke also fails on playback-locality root causes such as
`preview_decode_playback_session_not_reused` and
`preview_decode_playback_without_locality`, even when the wall-clock window
still passes. Those indicate `PlaybackCursor` is behaving like repeated random
access instead of a warm mostly-forward decode stream. It also fails when
`PlaybackCursor` queue wait exceeds the decode budget, because playback must
not sit behind still-frame or scrub work.
The real external continuous-playback smoke additionally emits
`real_media_gates` and fails when those gates fail. Defaults are deliberately
closer to production playback expectations than the broad wall-clock timeout:
`MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_P95_US=60000`,
`MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_QUEUE_WAIT_P95_US=10000`, and
`MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_VISIBLE_PERCENT=95`. These gates are
intended for 4K HEVC/HDR/Long-GOP fixture runs: a failure should drive
hardware decode, proxy, scheduler, or renderer-residency work, not timeout
widening. Override them only when documenting a different fixture class.
The interaction suffix also resizes the production Viewer geometry while the
transport remains clock-driven. Intermediate coordinates may be skipped when a
native resize stalls the UI thread; forcing one Timeline frame per resize would
slow transport and invalidate realtime evidence. The gate preserves authored
Full output, limits any single displacement to the four-frame CPU staging
horizon, and permits at most one isolated retained-frame hold per eight resize
observations. Loading, blank/unavailable output, consecutive stale holds, or a
second stale observation still fails. Ordinary continuous playback retains its
exact-ready requirement. The twelve-observation pause/resume window applies the
same four-frame displacement and no-blank/no-consecutive-stale rules, allowing
at most one isolated retained-frame hold while GPU completion callbacks settle
after resume.
The 60 ms general real-media decode bound and its explicit 60 ms histogram
boundary match the measured tail of direct FFmpeg software decode without
rounding a 50-60 ms result up to 80 ms. It accommodates bounded frame-threaded
software-decode bursts only when the independent exact Ready/publication,
zero-clock-skip, queue-wait, and bounded-tail gates also pass. The same
scenario-specific budget feeds both the nested decode-health report and the
outer real-media gate; they never evaluate one sample set against conflicting
50 ms and 60 ms policies. Its recurring-tail ceiling is derived from that same
budget (300 ms for the general 60 ms fixture). The
professional hardware qualification retains its stricter 40 ms p95 contract.
The current-ready ratio uses the maximum of accepted Engine `Ready` deliveries
and unique exact-current Viewer GPU publications. Both evidence streams
de-duplicate by playback epoch and frame, and GPU publication coverage is also
checked separately. This avoids charging a clock-boundary supersession as a
miss after the exact frame was already visible without allowing cached stale or
merely prepared successor work to count as presentation.

For compressed-wall-clock isolation of long-lived native decoder state, use
the ignored `preview_media_external_accelerated_native_surface_endurance_probe`
with `MONDRIAN_PREVIEW_EXTERNAL_PLAYBACK_MEDIA_PATH` and
`MONDRIAN_PREVIEW_ACCELERATED_ENDURANCE_FRAMES`. Set
`MONDRIAN_PREVIEW_ACCELERATED_ENDURANCE_SEEK_PROBES` to append up to 200
cross-region warm/exact seek probes after continuous decode. This diagnostic
does not replace cadence, whole-product process-tree memory, or reference-machine gates; it
only separates accumulated decode/surface state from real-time scheduling. Set
`MONDRIAN_PREVIEW_DECODE_EXECUTION_OUTPUT` to capture the same independently
flushed progress journal during this compressed run.

Every production Preview diagnostic snapshot now contains
`decode_worker_execution` for the bounded `any`, `playback`, and `non_playback`
workers. `stage` names the concrete media operation currently entered;
`request_sequence` proves which request generation the observer has admitted,
and `progress_sequence` changes on every stage or interrupt-poll publication.
`interrupt_poll_sequence` proves FFmpeg actually invoked the installed callback;
`interrupt_cancel_sequence` and `interrupt_last_cancel_request_sequence` prove
which request's probe returned cancellation. These facts deliberately stop
short of proving the FFmpeg call returned. When a timeout leaves a Broker
execution lease in flight, capture this
snapshot before terminating the process. A stable `codec_send_input`,
`codec_receive_frame`, `codec_open`, `hardware_device`, or `session_retire`
stage identifies the blocking call family; `output_lease_wait` instead means
the worker is cooperatively waiting for App/renderer ownership to retire. These
values are evidence, not a watchdog: do not mark the Broker lease complete or
restart a codec merely because the stage stopped changing.

The versioned Reference Playback runner sets
`MONDRIAN_PREVIEW_DECODE_EXECUTION_OUTPUT` for the Video gate. A sampler that
owns only a clone of the read watch writes versioned JSONL beside the normal
report. The current schema is v3. Every record carries `schema_version`,
`scenario`, monotonic process-local `observed_at_us`, `terminal`, and a
`workers` object split into `any`, `playback`, and `non_playback` lanes. A
present lane contains `stage`, `request_sequence`, `progress_sequence`, the
`interrupt_poll_sequence`, `interrupt_cancel_sequence`, and
`interrupt_last_cancel_request_sequence` fields, plus `isolated_demux`. That
object contains `session_launches`, `ready_sessions`,
`cross_request_reused_sessions`, `completed_seeks`, `completed_reads`,
`packet_responses`, `end_responses`, `clean_closes`,
`cancellation_terminations`, `failure_terminations`,
`forced_close_terminations`, `active_sessions`, and `peak_active_sessions`.
These cumulative demux lifecycle facts are the schema-v3 addition; absent
worker lanes remain JSON `null`.

The sampler runs at 100 ms, writes on progress changes or a five-second
heartbeat, and flushes every record so an externally terminated gate retains
its last observation; a normal finish adds `terminal: true`. The gate plan also
defines a 45-minute external process deadline. On expiry, the parent runner
terminates the complete Cargo/test descendant tree, records
`process_timed_out`, and retains the Cargo log and journal even though the
normal performance report may be absent. Inspect the last non-terminal journal
record to identify the request and call family that stopped progressing. This
mechanism closes the diagnostic feedback loop only; it is not codec recovery or
proof that an in-process FFmpeg call is cancellable.

Preview media perf artifacts also include `preview_render_report`, which covers
post-decode viewer work: sequence/media resolution, final-frame cache lookup,
working-frame preparation, CPU timeline composition, CPU output/color boundary,
and final raster packaging. Use it with `preview_decode_report` as a two-part
slow-frame diagnosis: decode-bound frames should drive codec/proxy/hardware
decode work, while render-bound frames should drive GPU composite, output
boundary, or raster-packaging work. In particular,
`preview_render_cpu_output_boundary_bound` means the viewer is spending budget
after decode in the CPU display/output transform, not in FFmpeg.
`preview_render_frame_packaging_bound` should never be caused by hashing the full
RGBA payload for an atlas key; viewer raster keys are expected to come from the
resolved render-plan identity so large frames do not add another full-frame CPU
scan after rendering.
Preview media path resolution also captures the resolved file fingerprint while
checking source/proxy freshness; repeated source/proxy metadata probes in the
viewer hot path should show up as resolve cost and should be collapsed instead
of hidden behind cache-key construction.

Viewer preview scheduling treats current-frame media requests as higher
priority than forward prefetch. When the pending decode window is full, a
current-frame request may evict a pending prefetch request; prefetch requests
must not evict current-frame work. The worker transport queue follows the same
rule and pops current-frame jobs before prefetch jobs so FIFO prefetch backlog
cannot hide a newly requested viewer frame; an already-queued prefetch job for
the same media key is promoted when it becomes current-frame work. Track this with
`scheduler.evicted_prefetch_requests`, `scheduler.dropped_backpressure_requests`,
`queue_evicted_prefetch_jobs`, `queue_canceled_jobs`,
`queue_promoted_current_jobs`, and `scheduler.skipped_decode_jobs` before tuning
queue sizes or decode worker counts.
Playback prefetch depth is frame-rate aware. The scheduler converts a bounded
wall-clock horizon into sequence frames, capped at eight native-resource units.
The full 250 ms horizon is retained through 30 fps; a 60 fps profile uses the
eight-frame cap (about 133 ms) so speculative residency leaves hardware-decoder
DPB/import headroom. `prefetch_skipped_prefetch_backlog` means
queued plus in-flight prefetch already covers that dynamic window, not a fixed
two-frame constant.
The decode performance report also carries worker-transport counters. Treat
`queue_full_drops` and `worker_disconnected_drops` as hard failures: they mean
work accepted by scheduler policy did not reach a live preview worker. Treat
`prefetch_skipped_current_pending`, `prefetch_skipped_current_work`,
`prefetch_skipped_prefetch_backlog`, `queue_pruned_obsolete_jobs`,
`queue_evicted_prefetch_jobs`, `queue_canceled_jobs`, and
`queue_promoted_current_jobs` as scheduling evidence that explains whether the
queue protected visible current-frame work and removed canceled queued work
before any codec or color/render optimization is attempted. Nonzero prefetch
skip counters are intentional slack-only prefetch behavior: the app did not add
speculative playback work while the visible frame was still waiting on media,
current-frame work was already queued or running, or queued plus in-flight
prefetch already covered the forward window. If queued plus in-flight prefetch
is below the forward window, the scheduler only tops up the remaining
queued-plus-in-flight prefetch budget across the evaluated tracks and nested
sequences; repeated playback ticks should not add full new speculative windows.
The same report includes current worker-queue depth split by priority and access
mode (`queued_current_jobs`, `queued_prefetch_jobs`,
`queued_playback_cursor_jobs`, `queued_scrub_cursor_jobs`, and
`queued_random_access_still_jobs`) so active transport backlog can be separated
from codec, seek, or CPU RGBA boundary cost before tuning worker counts. Schema
v20 also includes worker-lane eligibility for queued jobs
(`queued_playback_lane_eligible_jobs`, `queued_scrub_lane_eligible_jobs`,
`queued_still_lane_eligible_jobs`, and
`queued_non_playback_lane_eligible_jobs`) so a slow report can distinguish
generic queue depth from work that a particular lane is allowed to take. It also
includes Broker-owned in-flight execution-lease residency (`in_flight_current_jobs`,
`in_flight_prefetch_jobs`, `in_flight_playback_cursor_jobs`,
`in_flight_scrub_cursor_jobs`, and `in_flight_random_access_still_jobs`) so a
slow report can distinguish jobs waiting for a lane from workers currently
occupied by playback, scrub, or still-frame decode.
Schema v31 places lane-level in-flight fields in that same `worker_queue`
snapshot (`in_flight_playback_lane_jobs`,
`in_flight_scrub_lane_jobs`, `in_flight_still_lane_jobs`,
`in_flight_non_playback_lane_jobs`, and `in_flight_cross_lane_current_jobs`)
and removes the App-local worker-activity counter family. Repeated cross-lane
current work is a Broker/Adapter contract violation, not healthy overflow or
proof that the codec, color transform, or GPU upload stage is the bottleneck.

Viewer GPU-output sessions can persist live health records from the app window:

```powershell
$env:MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT='target/perf/viewer-gpu-output.jsonl'; cargo run -p mondrian-app
cargo run -p mondrian-app --bin viewer_gpu_output_budget -- target/perf/viewer-gpu-output.jsonl --min-records 1 --min-ready 1 --max-failed 0 --max-blocked 0 --max-rejected 0 --max-degraded 0 --max-display-issues 0 --max-hdr-output-requires-hdr-surface 0 --max-output-color-space-requires-surface-color-space 0 --max-reconfigure-blocked-by-payload 0 --max-unsupported-presentation-intent 0 --max-unsupported-surface-contract 0 --max-unknown-display-issues 0 --max-display-contract-refreshes 999999 --max-display-issue-refresh-correlations 0 --max-display-tone-map-headroom-changes 0 --max-available-surface-format-changes 0 --max-format-color-space-changes 0 --max-present-mode-changes 0 --max-alpha-mode-changes 0 --max-display-payload-blockers 0 --max-color-rejections 0
$env:MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT='target/perf/viewer-gpu-output.jsonl'; cargo run -p mondrian-app --bin viewer_gpu_output_budget -- target/perf/viewer-gpu-output.jsonl --preset display-baseline
cargo test -p mondrian-app viewer_gpu_output_budget_smoke -- --ignored --nocapture
$env:MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT='target/perf/viewer-gpu-output.jsonl'; cargo test -p mondrian-app viewer_gpu_output_display_baseline_smoke -- --ignored --nocapture
```

The budget command prints a versioned health report and exits non-zero when the
health stream violates the thresholds or when reported cumulative
`health_counts` do not match the statuses replayed from the JSONL records.
Empty JSONL streams fail with a `records` budget failure. The report's embedded
summary now fails closed on missing renderer-owned stage/runtime evidence and on
preview-candidate discontinuity unless explicitly tolerated:
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_MISSING_RUNTIME_REPORTS`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_MISSING_STAGE_REPORTS`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_READY_RECORDS_MISSING_PREVIEW_CANDIDATE_CONTEXT`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_PREVIEW_CANDIDATE_ID_REGRESSIONS`.
A `Ready` record should carry preview-candidate identity/state and stable
`accumulated_stage_report`/`runtime_report` fields for CI-grade traceability.
The report also replays `display_issue_summary` records by reason and
payload-blocker presence, so CI budgets can fail on display/surface contract
problems even when a run allows some degraded frames for investigation. The same
summary now reports
`color_rejections`, the last structured viewer color rejection, and aggregated
`media_issues`; `MONDRIAN_VIEWER_GPU_OUTPUT_MAX_COLOR_REJECTIONS` keeps viewer
GPU-output smokes fail-closed when metadata policy starts rejecting media.
The summary preserves the last display issue's structured surface-contract
evidence as well: display target fingerprint, current/selected/desired surface
format and color space, SDR/PQ/HLG encoding, HDR mode, payload blocker, and
whether the required target surface color space was actually supported.
It also replays recent display-contract refresh events and reports the last
refresh event, so resize, scale-factor, and window-move monitor transitions can
be diagnosed from the same JSONL summary instead of only from trace logs.
When a display issue follows a recorded refresh, the summary preserves that
preceding refresh on the issue itself and reports correlation counts by refresh
reason, so a smoke report can show that a blocker started only after a monitor
move or scale-factor transition.
Refresh events now also preserve capability-difference evidence: available
surface formats, per-format color-space support, present modes, alpha modes,
and tone-map headroom summaries before and after the transition. The viewer
health report turns those into separate root causes/actions for refresh churn,
issue-after-refresh correlation, HDR headroom drift, surface-format drift,
per-format color-space drift, present-mode drift, and alpha-mode drift.
Use the per-reason viewer display thresholds
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_HDR_OUTPUT_REQUIRES_HDR_SURFACE`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_OUTPUT_COLOR_SPACE_REQUIRES_SURFACE_COLOR_SPACE`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_RECONFIGURE_BLOCKED_BY_PAYLOAD`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNSUPPORTED_PRESENTATION_INTENT`, and
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNSUPPORTED_SURFACE_CONTRACT`, plus
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_UNKNOWN_DISPLAY_ISSUES`, when an investigation
needs to relax one specific display contract failure without masking the others
or silently accepting a new reason emitted by the app. Capability drift now has
its own gates too: `MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_CONTRACT_REFRESHES`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_ISSUE_REFRESH_CORRELATIONS`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_TONE_MAP_HEADROOM_CHANGES`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_AVAILABLE_SURFACE_FORMAT_CHANGES`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_FORMAT_COLOR_SPACE_CHANGES`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_PRESENT_MODE_CHANGES`, and
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_ALPHA_MODE_CHANGES`.
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_MISSING_RUNTIME_REPORTS`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_MISSING_STAGE_REPORTS`,
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_READY_RECORDS_MISSING_PREVIEW_CANDIDATE_CONTEXT`, and
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_PREVIEW_CANDIDATE_ID_REGRESSIONS`
are part of the same per-reason gate style.
`viewer_gpu_output_budget_smoke` uses the fully environment-driven budget, while
`viewer_gpu_output_display_baseline_smoke` keeps the same fail-closed display
issue and capability-drift gates but defaults
`MONDRIAN_VIEWER_GPU_OUTPUT_MAX_DISPLAY_CONTRACT_REFRESHES` to `2` so a normal
window bring-up can tolerate a small amount of display-contract settling
without hiding real monitor, surface, or payload drift.
The CLI now exposes the same baseline through `--preset display-baseline`, and
explicit threshold flags still override the preset afterwards.
The health report is the only external report shape: fixed area checks, verdict,
root causes, actions, compact evidence, and the raw budget summary in one JSON
object.

Preview decode report schema v36 evaluates an explicit access-mode × work-class
matrix. `SessionOpened`/`SessionReplaced`, steady forward reuse, reused seeks,
reused ambiguous work, and decoder-bypassing ring hits are non-overlapping
cells; contradictory lifecycle/path/seek facts enter `Unclassified` and fail
capture integrity. Each cell's frame count must equal its histogram sample
count, successful lifecycle totals must equal successful frames, and queue
histogram totals must equal explicit queue sample counts. Required playback
profiles must contain steady-forward samples; required scrub and exact-still
profiles must contain a real reused seek after lane warm-up.

The >5 s work-latency bucket and >80 ms queue bucket are open intervals and have
no fabricated quantile upper bound. A quantile in either bucket retains the
measured maximum only as evidence. Missing queue/decode p95 checks are not zero:
Headless acceptance consumes them as optional evidence and fails when absent.
Session churn includes successful and canceled opens/replacements, while
canceled work remains outside successful-frame latency profiles.

Native D3D12VA Viewer runs must keep two hardware-timestamp domains distinct.
The ordinary Viewer ring measures the caller-owned composite/spatial/output
suffix. The renderer native-import ring measures the preceding YUV and
source-to-working color submission and correlates its deferred samples through
backend-runtime-local candidate/import tokens plus the gate's execution-session
identity. A report must carry `yuv_decode_marker_bracket_us`,
`input_color_marker_bracket_us`, `capability_supported`, `activated`,
`inactive_reason`, and cumulative `samples`/`pending`/`missing`/`dropped`
coverage; it must not relabel Viewer suffix duration as native-import work.
Those two deltas begin inside the wgpu import command buffer and do not include
decoder execution, the cross-device copy, queue-wait latency, or the raw
acquire transition. They can include implicit barriers, scheduler gaps, and
backend command placement/reordering inside the markers, so tooling must call
them bracket attribution rather than pure shader time. Capability alone does
not activate timing: gates explicitly enable a bounded `1..=256` ring, while
normal runtime defaults to disabled. Disabled policy, unsupported capability,
or readback failure is explicit inactive evidence, not a zero-duration sample,
and ring pressure drops telemetry instead of delaying the measured playback
scheduler. Collection follows an execution-owner device poll and must not add
another poll or wait.
`submitted_imports` is usable-output coverage: it counts only imports that
returned a valid working frame. A failed bridge may have ambiguously submitted
GPU commands, but that attempt is not a successful import and must not be
treated as covered output.

Only the full professional 4K HEVC Main10 playback gate activates this prefix
timing contract. Ordinary playback smokes and the short isolated-demux
qualification select `Disabled` explicitly. The professional probe derives a
fixed observation-buffer capacity from its frozen frame/seek workload, enables
the renderer ring at its bounded maximum, and fails on either renderer-ring
drops or App observation-buffer overflow; profiling never backpressures frame
publication. Each successful Viewer record moves its renderer receipt exactly
once into App evidence qualified by one process-local Headless session ID.
Preroll and measured-playback candidates retain separate ownership until all
deferred samples are collected after the existing suffix-timing device wait,
then one linear reconciliation assigns every receipt and sample exactly once.
A uniquely owned successful candidate may be released, superseded, or retire
late without becoming an orphan; only candidates that actually completed
publication contribute `published_native_candidates` and
`published_native_samples`.

Professional acceptance requires the renderer and receipt accounting
identities, zero pending/missing/dropped/overflow evidence, unique
session/candidate/import identities, and exact sample reconciliation. It fails
on duplicate receipts, duplicate samples, multiply owned samples, unmatched
samples, orphan receipts, or any candidate whose observed sample count differs
from that exact receipt's `scheduled_samples` even when run-wide totals happen
to balance. The serialized report also publishes
p50/p95/mean/max for both marker brackets and ready/not-ready/unknown decoder
fence counts. These statistics describe only uniquely reconciled samples;
untrusted samples remain visible through failure evidence instead of
contaminating the stage distributions.

Performance output should be committed only when it is an intentional benchmark artifact; ordinary runs should leave `target/` ignored.
