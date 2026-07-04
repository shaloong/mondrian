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
```

Viewer GPU-output sessions can persist live health records from the app window:

```powershell
$env:MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT='target/perf/viewer-gpu-output.jsonl'; cargo run -p mondrian-app
cargo run -p mondrian-app --bin viewer_gpu_output_budget -- target/perf/viewer-gpu-output.jsonl --min-ready 1 --max-failed 0 --max-blocked 0 --max-rejected 0 --max-degraded 0
```

The budget command prints a JSON summary and exits non-zero when the health
stream violates the thresholds or when reported cumulative `health_counts` do
not match the statuses replayed from the JSONL records.

Performance output should be committed only when it is an intentional benchmark artifact; ordinary runs should leave `target/` ignored.
