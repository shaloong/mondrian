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
$env:MONDRIAN_PERF_OUTPUT='target/perf/project-lifecycle.jsonl'; cargo test -p mondrian-app perf_project_lifecycle_smoke -- --ignored --nocapture
$env:MONDRIAN_PREVIEW_SIM_OUTPUT='target/perf/preview-1080p2997.jsonl'; cargo test -p mondrian-app preview_1080p2997_simulated_perf -- --ignored --nocapture
$env:MONDRIAN_EXPORT_SIM_OUTPUT='target/perf/export-1080p2997.jsonl'; cargo test -p mondrian-export export_1080p2997_simulated_perf -- --ignored --nocapture
$env:MONDRIAN_PERF_OUTPUT='target/perf/preview-media.jsonl'; cargo test -p mondrian-app preview_media_decode_cache_smoke -- --ignored --nocapture
$env:MONDRIAN_PREVIEW_EXTERNAL_MEDIA_PATH='E:\Video Projects\Mondrian Test\HEVC Samples\hevc_4k25P_main10_1.mp4'; $env:MONDRIAN_PERF_OUTPUT='target/perf/preview-external-media.jsonl'; cargo test -p mondrian-app preview_media_external_access_mode_smoke -- --ignored --nocapture
$env:MONDRIAN_PERF_OUTPUT='target/perf/preview-playback.jsonl'; cargo test -p mondrian-app preview_media_continuous_playback_smoke -- --ignored --nocapture
$env:MONDRIAN_PREVIEW_DECODE_FIXTURE='E:\media\sample-4k-hdr.mov'; $env:MONDRIAN_PREVIEW_DECODE_TIMESTAMP='1.0'; $env:MONDRIAN_PREVIEW_DECODE_MAX_WIDTH='1920'; $env:MONDRIAN_PREVIEW_DECODE_MAX_HEIGHT='1080'; cargo test -p mondrian-media preview_decode_fixture_perf_smoke -- --ignored --nocapture
$env:MONDRIAN_RENDERER_GPU_OUTPUT_SMOKE_OUTPUT='target/perf/renderer-gpu-output.jsonl'; cargo test -p mondrian-renderer gpu_output_boundary_runtime_smoke_report_on_real_wgpu_device -- --ignored --nocapture
```

Export simulation reports include a versioned `color_report`. The report embeds
the export color diagnostics summary and adds verdict, fixed checks, root
causes, and actions. The same constructor is used by export job diagnostics and
perf artifacts, so tooling should key off `color_report.verdict`, `checks`,
`root_causes`, and `actions`, not off ad hoc summary interpretation. Default
export smokes fail closed unless export stays fully float/linear, has no GPU
blockers, has no upload/readback transfer stages, reports no structured legacy
RGBA8 reasons, and has at least one diagnosed frame. The old `color_health*`
perf fields are not part of the report contract.

Renderer GPU output smoke reports include a versioned `health_report`. That
report is the sole renderer-side native GPU output contract: tooling should key
off `health_report.verdict`, `checks`, `root_causes`, and `actions`, not off
legacy `health`, `health_failures`, or `passed` fields. Default renderer smoke
expectations are fail-closed: the run must not be skipped, the native GPU stage
sequence must be complete, GPU blockers must be absent, backend runtime and
shader cache must be ready, readback must be byte-complete, and CPU/GPU parity
must stay within tolerance. The schema is owned by
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
together so software decode does not multiply independent thread pools. Override
these only for profiling or platform-specific investigation:

```powershell
$env:MONDRIAN_PREVIEW_DECODE_THREADING='slice' # none | frame | slice
$env:MONDRIAN_PREVIEW_DECODE_THREADS='4'      # FFmpeg decoder threads per worker
$env:MONDRIAN_PREVIEW_DECODE_WORKERS='2'      # app preview decode workers
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
with checks, root causes, and actions. The default slow-frame budget is 50 ms;
over-budget hard failures make the preview media smoke fail and should be
diagnosed from `preview_decode_report.root_causes` before changing
renderer/color code. Access-mode profiles include `queue_wait_max_us` and
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
The same profiles expose session-local seek-index counters:
`seek_index_available_frames`, `seek_index_used_frames`,
`seek_index_keyframes_max`, and `seek_index_observed_packets_max`. Slow scrub
with `seeked_frames > 0` and no `seek_index_available_frames` means the decoder
is seeking without observed keyframe/GOP evidence and should be improved by
index/proxy/hardware-decode work, not by hiding the problem in UI timeouts.
`seek_index_available_frames > 0` with low `seek_index_used_frames` means the
current bounded-any seek window could not use the known anchors; inspect
`seek_us`, `packet_decode_us`, and the seek-window policy before increasing
worker counts.
Cancellation counters are also split inside each access-mode profile; use those
fields to identify whether obsolete, prefetch-deadline,
prefetch-preempted-by-current, still-preempted-by-realtime-current, shutdown,
or unknown cancellations came from playback, active scrub, or still-frame work.
`canceled_playback_deadline_jobs` is different from
`canceled_prefetch_deadline_jobs`: playback-deadline cancellations mean a
visible `PlaybackCursor` current frame missed its display deadline and was
dropped before or during decode, while prefetch-deadline cancellations mean
speculative cache warming exceeded its budget. Treat playback deadline pressure
as a reason to improve proxy/hardware decode/drop policy, not as a reason to
increase speculative prefetch.
Prefetch preemption means the app protected visible current-frame work from
in-flight playback speculation. Still preemption means deterministic still-frame
work yielded to playback or scrub current-frame work. Both are scheduling
pressure signals, not media decode failures.
`preview_media_decode_cache_smoke` intentionally exercises both settled
non-playing seeks (`RandomAccessStillFrame`) and active playhead dragging
(`ScrubCursor`). The active scrub window is controlled by
`MONDRIAN_PREVIEW_MEDIA_SCRUB_READY_MS`; do not replace it with plain
`state.seek(...)`, because that would silently stop profiling the latest-wins
scrub path. The smoke fails closed when either `ScrubCursor` or
`RandomAccessStillFrame` has no successful profile samples; a profile schema
without exercised samples is not acceptable coverage.
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
The decode performance report also carries worker-transport counters. Treat
`queue_full_drops` and `worker_disconnected_drops` as hard failures: they mean
work accepted by scheduler policy did not reach a live preview worker. Treat
`prefetch_skipped_current_pending`, `prefetch_skipped_worker_busy`,
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
from codec, seek, or CPU RGBA boundary cost before tuning worker counts. It also
includes in-flight worker activity (`in_flight_current_jobs`,
`in_flight_prefetch_jobs`, `in_flight_playback_cursor_jobs`,
`in_flight_scrub_cursor_jobs`, and `in_flight_random_access_still_jobs`) so a
slow report can distinguish jobs waiting for a lane from workers currently
occupied by playback, scrub, or still-frame decode.
Lane-level in-flight fields (`in_flight_playback_lane_jobs`,
`in_flight_scrub_lane_jobs`, `in_flight_still_lane_jobs`,
`in_flight_interactive_lane_jobs`, and `in_flight_cross_lane_current_jobs`)
separate healthy visible-work overflow from a worker split that is too tight.
Repeated cross-lane current work is a scheduling-capacity signal, not proof that
the codec, color transform, or GPU upload stage is the primary bottleneck.

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

Performance output should be committed only when it is an intentional benchmark artifact; ordinary runs should leave `target/` ignored.
