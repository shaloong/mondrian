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
```

Export simulation reports include `color_health`, `color_health_budget`,
`color_health_passed`, and `color_health_failures`. By default the smoke fails
closed unless export stays fully float/linear, has no GPU blockers, has no
upload/readback transfer stages, and reports no structured legacy RGBA8 reasons.
Use `MONDRIAN_EXPORT_SIM_REQUIRE_FULLY_FLOAT_LINEAR`,
`MONDRIAN_EXPORT_SIM_REQUIRE_GPU_PATH_READY`,
`MONDRIAN_EXPORT_SIM_MAX_GPU_BLOCKERS`,
`MONDRIAN_EXPORT_SIM_MAX_LEGACY_REASONS`, and
`MONDRIAN_EXPORT_SIM_MAX_TRANSFER_STAGES` only when intentionally relaxing the
budget for an investigation.

Viewer GPU-output sessions can persist live health records from the app window:

```powershell
$env:MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT='target/perf/viewer-gpu-output.jsonl'; cargo run -p mondrian-app
cargo run -p mondrian-app --bin viewer_gpu_output_budget -- target/perf/viewer-gpu-output.jsonl --min-records 1 --min-ready 1 --max-failed 0 --max-blocked 0 --max-rejected 0 --max-degraded 0 --max-display-issues 0 --max-display-payload-blockers 0
cargo test -p mondrian-app viewer_gpu_output_budget_smoke -- --ignored --nocapture
```

The budget command prints a JSON summary and exits non-zero when the health
stream violates the thresholds or when reported cumulative `health_counts` do
not match the statuses replayed from the JSONL records. Empty JSONL streams fail
with a `records` budget failure. The summary also replays
`display_issue_summary` records by reason and payload-blocker presence, so CI
budgets can fail on display/surface contract problems even when a run allows
some degraded frames for investigation.

Performance output should be committed only when it is an intentional benchmark artifact; ordinary runs should leave `target/` ignored.
