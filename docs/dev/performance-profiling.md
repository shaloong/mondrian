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
$env:MONDRIAN_PERF_OUTPUT='target/perf/preview-playback.jsonl'; cargo test -p mondrian-app preview_media_continuous_playback_smoke -- --ignored --nocapture
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

Preview media smoke reports include `preview_color_report` and
`media_color_issues`. The decode/cache and continuous-playback smokes fail
closed unless preview emits color-health diagnostics, stays fully float/linear,
has no GPU blockers, has no upload/readback transfer stages, reports no
structured legacy RGBA8 reasons, and has no missing-metadata policy rejections.
`media_color_issues` is the machine-readable rollup of active-sequence asset
diagnostics, including missing metadata, decoder-unavailable assets, hint
conflicts, and HDR side-data presence. The old preview `*_budget`, `*_passed`,
and `*_failures` perf fields are not part of the report contract.

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
summary also replays
`display_issue_summary` records by reason and payload-blocker presence, so CI
budgets can fail on display/surface contract problems even when a run allows
some degraded frames for investigation. The same summary now reports
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
