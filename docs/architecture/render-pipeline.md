# Render Pipeline

The intended render path is shared by preview and export:

```text
TimelineEvaluationRequest
  -> Timeline Evaluation
  -> FlatActiveClip
  -> TimelineRenderPlan
  -> TimelineRenderPlanElement
  -> Clip Sampling / Generated Source
  -> Effect Graph Evaluation
  -> Layer Composite
  -> Color Transform
  -> Display or Export Encode
```

## Current Implementation

`mondrian-renderer::timeline_render_plan` evaluates one sequence frame through
`evaluate_timeline_render_plan(source, request)`. The request carries:

- render intent: preview, export, thumbnail, or analysis
- render quality: interactive, draft, or final
- color target: display, export, or working space
- scheduler policy: whether frame dropping is allowed
- preview/export resolution scale

The result is a `TimelineRenderPlan` with ordered `TimelineRenderPlanElement`
values plus diagnostics for active clips, emitted elements, zero-opacity skips,
and unrenderable skips.

`collect_timeline_color_diagnostics(...)` reports the clip override, working
space, and output space together with their canonical `ColorEncodingSpec`
values. `collect_timeline_color_diagnostics_with_display_view(...)` attaches
the resolved OCIO display/view for presentation diagnostics. Renderer
diagnostics must consume `ColorSpace::encoding()` rather than duplicating
color-space metadata.

`evaluate_timeline_render_plan(...)` is the render-plan entry point. Callers
must choose an explicit `TimelineEvaluationRequest` intent so preview, export,
thumbnail, and analysis paths cannot accidentally share ambiguous defaults.

`TimelineRenderPlanElement` variants are:

- `Media`
- `Adjustment`
- `SolidColor`
- `NestedSequence`

Each element carries opacity, blend mode, transforms where applicable, effect graph, frame seed, and color/media interpretation data.

`timeline_composite` exposes one color-managed composition contract:
`composite_timeline_elements_color_frame(...)`. It returns a typed
`CpuColorFrame` whose descriptor records domain, encoding, residency, dimensions,
and color space. Viewer preview and export must consume this typed working-frame
contract, then apply their respective working -> output boundary through
`RenderOutputColorBoundary` and renderer color stage execution helpers.
Bare RGBA8 buffers are valid only at source import, debug/golden snapshot, UI
presentation readback, and CPU encoder boundaries. They are not a renderer-stage
exchange format.

The CPU timeline compositor keeps identity-transform media, solid-color layers,
and float-capable adjustment layers in the typed float/linear working frame.
Timeline blend modes, including seeded Dissolve, are implemented by
`mondrian-effects`' float pixel blend contract and must not round-trip through
RGBA8 scratch buffers. Extended working values therefore remain available to the
final output boundary. Float-capable unary effects such as color adjustment and
white balance also run in this path through the same `mondrian-effects` float
contract. Geometric transforms and legacy-only effects currently use the RGBA8
compositor path until their own float/linear execution contracts are
implemented.
`TimelineCompositeDiagnostics` makes that fallback explicit: preview/export
callers can see whether a composite stayed on the float/linear path or fell back
to legacy RGBA8 because of transform or effect support. Blend-mode counters stay
in the diagnostic contract for future unsupported blend contracts, but current
built-in media, solid, and float-capable adjustment blend modes are expected to
remain float/linear. Preview and export diagnostics must aggregate these
counters so performance smoke reports and job panels can identify which legacy
color path blocked a fully float/linear frame. Callers should use
`TimelineCompositeDiagnostics::color_path_summary()` as the renderer-owned
contract for high-level path state and structured legacy reason breakdowns
instead of re-inferring path safety from individual counters.

Decoded media enters the graph as a typed source/import RGBA8 boundary
(`CpuEncodedColorFrame::source_rgba8`). Preview and export must use
`RenderInputTransform` plus `execute_cpu_input_stage(...)` or a future GPU-capable
stage executor to produce a result carrying both `CpuColorFrame` and stage
diagnostics before building `TimelineMediaLayer`. Timeline media layers
therefore carry typed working frames, not naked RGBA slices.

Color-transform executors emit `RenderColorTransformDiagnostics` for input and
output boundaries. Preview diagnostics aggregate transform calls, transformed
pixels, and temporary RGBA8 boundary crossings so performance smoke tests can
catch accidental CPU-bound color work as the GPU path comes online.

`FrameCompositor` supports GPU batched compositing with texture pooling and can
return either RGBA readback or a GPU texture. GPU render graph nodes return
`GpuColorFrameHandle` values carrying the same descriptor contract as CPU
frames, rather than naked textures or byte vectors.

`RenderColorStagePlanner` sits between timeline evaluation/compositing and the
CPU/GPU color executors. It produces ordered stage plans for CPU transforms,
GPU OCIO transforms, upload, and readback. App and export crates should consume
renderer stage plans instead of deciding CPU/GPU/readback behavior locally.
`RenderOutputColorBoundaryPlanner` is the final-output boundary wrapper around
that planner. Its CPU-only mode is the current correctness execution path;
its PreferGpu mode must produce GPU/upload/readback stages plus explicit
blocker diagnostics instead of silently falling back to a CPU output stage.
`RenderOutputColorBoundaryExecutor` is the lower-level CPU final-output
execution boundary: callers choose an explicit strategy at construction time,
and the executor owns the final-output plan/execute sequence instead of
exposing low-level transform executors to app/export code. App/export CPU
reference callers use `execute_cpu_output_boundary_rgba8(...)`, which returns
encoded RGBA8 pixels plus transform/stage diagnostics as one boundary contract;
native GPU app/export callers must use
`RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend(...)`
with `RenderGpuOutputBoundaryRuntimeOwnedBackendContext`. GPU planning,
backend-object preparation, or recording failures are reported structurally and
must not silently run the CPU executor.
App/export integrations should hold a `RenderGpuOutputBoundaryRuntime` per
renderer backend lifetime. The runtime owns the OCIO shader cache, pure backend
prep cache, concrete backend-object cache, GPU frame id allocator, and GPU
frame resource table, and exposes an executor-level record method that accepts
only the per-submission device/queue/encoder/load-op context. Lower-level code
that already owns a prepared pipeline, OCIO bind group, and pass node may still
use `RenderGpuOutputBoundaryBackendContext`.
For native GPU OCIO execution, `RenderGpuColorPassSchedule` is the bridge
between the stage plan and backend recorder: it requires GPU-resident source and
target frame handles, a blocker-free `RenderColorTransformGpuPlan`, and a
matching `OcioGpuWgpuRenderPassNodePlan`. The schedule also owns the renderer
entry points that bind a resolved input frame view into the wrapper bind group
and record the output target through `OcioGpuWgpuRenderPassRecorder`; preview
and export callers must not duplicate OCIO pass assembly. Resolved GPU frame
views must come from `GpuColorFrameResourceTable`, which validates the
`GpuColorFrameHandle` contract before exposing the backend texture/view/sampler
payload. Callers that already have concrete wgpu resources should use
`record_wgpu_from_resources` so resource resolution, wrapper bind-group
preparation, output target construction, and recorder invocation stay in one
renderer-owned path. CPU boundary frames and empty GPU targets must enter that
resource table through `GpuColorFrameUploadPlan` / `GpuColorFrameAllocationPlan`
and `GpuColorFrameUploader`, not ad hoc texture creation in preview or export.
Use `RenderOutputColorBoundaryStagePlan::gpu_resource_plan(...)` to bridge a
blocker-free final-output GPU stage plan into `RenderGpuOutputStageResourcePlan`
so transfer descriptors, frame ids, and the scheduled GPU transform stay
consistent. CPU-only plans and GPU plans with native blockers must fail
structurally at this bridge instead of falling back to CPU execution. Then call
the resource plan's materialization helper to fill the resource table. If the
same stage plan ends in `ReadbackToCpu`,
`RenderGpuOutputStageResourcePlan` carries the `GpuColorFrameReadbackPlan` and
owns resource-table lookup plus readback-copy recording. Preview/export callers
should use
`RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend(...)`;
the runtime owns planning, resource-plan derivation, backend-object
preparation, materialization, schedule validation, pass recording, and optional
readback in renderer-owned order. The returned `RenderGpuOutputStageRecord`
must carry `RenderColorStageDiagnostics` for the recorded upload, GPU color
pass, optional readback, blocker breakdown, and pixel budget so preview/export
telemetry does not infer GPU execution after the fact. The blocker breakdown
must distinguish missing shader modules, OCIO resource bind groups, fullscreen
wrappers, and render pipelines. Lower-level renderer code that already owns a validated
`RenderOutputColorBoundaryStagePlan` or `RenderGpuOutputStageResourcePlan` may
call the matching stage/resource recorder with renderer backend contexts.
GPU-to-CPU output for encode, thumbnails, tests, or debug captures must use one
of those renderer-owned paths; readback is valid only for explicit encoded RGBA8
output contracts unless a future conversion stage says otherwise.
Current CPU preview/export execution uses `execute_cpu_input_stage(...)` for
source boundaries and `execute_cpu_output_boundary_rgba8(...)` for final display
or export output. That CPU reference helper delegates to
`RenderOutputColorBoundaryExecutor::cpu_only()` and returns the encoded RGBA8
boundary result plus diagnostics; app/export code should not instantiate the
final-output executor directly or call `execute_cpu_output_stage(...)` for final
timeline output. Direct
`CpuColorTransformExecutor` usage is limited to renderer internals and its
focused unit tests.

Stage helpers return `RenderColorStageExecution<T>`, not the raw transform
result. App, export, tests, and benches must read frames from `.result` and
aggregate `.stage_diagnostics` where they expose observability. Preview
diagnostics, export job diagnostics, and export performance smoke reports record
stage-plan counts, CPU/GPU stage mix, transfer stages, GPU blockers, and touched
pixels from actual renderer stage executions so later GPU execution work can
prove it removed CPU bottlenecks instead of merely moving code around.
`RenderColorTransformError::ExecutionFailed` must keep the
transform direction plus typed input/output descriptors with the backend reason;
renderer GPU output smoke additionally records a real wgpu upload + GPU color
pass + readback boundary and emits `MONDRIAN_RENDERER_GPU_OUTPUT_JSON` (or JSONL
via `MONDRIAN_RENDERER_GPU_OUTPUT_SMOKE_OUTPUT`) so dashboards can verify the
native final-output path without launching the app window. The smoke report
contains both raw stage/runtime counters and a versioned `health_report` as the
sole high-level contract. That report embeds the derived health summary, fixed
checks, root causes, actions, and evidence so dashboards can distinguish
skipped adapters, incomplete native GPU sequencing, backend-cache/object
preparation failures, readback-size mismatches, GPU blockers, and CPU/GPU
parity failures without reverse-engineering the counters. The renderer owns the
serializable contract types for this layer directly in production code:
`RenderGpuOutputFrameReport`, `RenderGpuOutputStageDiagnosticsReport`,
`RenderGpuOutputRuntimeDiagnosticsReport`, and `RenderGpuOutputHealthReport`.
Smoke tests, perf tooling, and future app/export integrations must build on
those types instead of carrying a test-private schema copy.

## Required Semantics

- Disabled clips and zero-opacity clips do not enter the render plan.
- Clip blend mode overrides track blend mode; otherwise track blend mode applies.
- Adjustment layers operate on lower accumulated pixels, not as standalone media.
- Solid colors are generated sources, not file-backed frames.
- Nested sequences must preserve the configured nested color-processing mode.
- Effect graphs are compiled from clip effects plus masks at the evaluated time.

## Preview vs Export

Preview and export may use different scheduling, cache lifetime, and readback strategy. They must share timeline interpretation, clip ordering, effect evaluation, blend semantics, and color-management decisions.

Preview media decoding must convert source media into the sequence working
color space before compositing. The source color space resolves from clip
override first, then explicitly detected media metadata, then the configured
missing-metadata policy. Resolved or assumed probe values are not media metadata;
callers must use `VideoStreamInfo.detected_color_space` and
`MissingColorMetadataPolicy::resolve_input_decision(...)` so preview and export
share the same `InputColorResolution` branch diagnostics. When preview/export
logs or UI need to explain why metadata was rejected or unsupported, they should
attach `VideoStreamInfo.color_interpretation` and `VideoStreamInfo.color_metadata`
raw CICP tags rather than rebuilding diagnostics from path or decoder text.
Export jobs carry this as `TimelineExportInput.asset_color_diagnostics`, and
preview emits the same diagnostic summary when missing-metadata policy rejects a
media asset. Preview also stores the latest viewer-request rejection as
`AppUiPreviewColorRejection` so panels, diagnostics, and automated smoke tests
can inspect the rejected asset id, path, missing-metadata policy,
`InputColorResolution` branch, working color space, and media diagnostic summary
without scraping logs. Playback prefetch must not overwrite this viewer-facing
snapshot.
Preview perf smoke reports and export frame diagnostics expose the same
per-branch input color-resolution counters, including override, detected
metadata, missing-policy assumptions, and missing-policy rejects. Data/non-color
texture resolution is reported as its own branch instead of being merged into
overrides, and it must come from asset payload classification rather than the
Interpret Footage color-space picker. These counters are part of the render-path
health contract: preview can remain real-time while still reporting whether it
was driven by authoritative media interpretation, non-color asset
classification, or by project policy. Aggregated report fields such as
`policy_assumptions` and `explicit_metadata_or_override` must be derived from
`InputColorResolutionSourceCounts` in `mondrian-timeline`, not hand-maintained
in preview/export reporting code.
Media metadata quality must travel through the same reporting path: export job
diagnostics carry `asset_issue_summary`, and preview media smokes serialize
`media_color_issues`, both derived from `VideoColorDiagnosticIssueAggregate`
rather than from parsed warning strings. The aggregate scope is the referenced
timeline asset set, not the whole asset-diagnostics cache, so preview/export
parity cannot be skewed by unrelated assets loaded in the project.
Timeline export writes accumulated render-path color diagnostics into
`RenderJob.diagnostics.color`. The worker updates this snapshot while frames are
actually rendered. App panels, logs, and JSONL reports should consume
`ExportJobDiagnostics::color_report()` or
`ExportJobColorDiagnostics::health_report()` as the stable semantic contract
instead of re-evaluating timeline state or rebuilding derived counters in UI
code. The embedded summary carries the same health concepts used by preview
reports:
explicit metadata/override totals, policy assumptions/rejections, data-texture
bypasses, legacy RGBA8 reason totals, float/linear completeness, GPU blockers,
GPU blocker breakdowns, legacy RGBA8 reason breakdowns, and GPU path readiness.
Export simulation perf JSONL includes this evidence only through the versioned
`color_report`; `color_health*` fields are not a supported external report
surface. Export simulation `passed` must include that report verdict: default
runs require diagnosed frames, fully float/linear composites, GPU path
readiness, zero GPU blockers, zero upload/readback transfer stages, and zero
structured legacy RGBA8 reasons.
Preview perf JSONL follows the same pattern with `preview_color_report` and,
for app-scale playback probes, `preview_playback_color_report`. The app preview
service owns the derivation from raw counters to report summary fields so perf
tests do not hand-maintain color health semantics. Preview media decode/cache
and continuous-playback smokes fail closed on those reports: default runs
require a present health summary, fully float/linear composites, GPU path
readiness, zero GPU blockers, zero upload/readback transfer stages, zero
structured legacy RGBA8 reasons, and zero missing-metadata policy rejections.
Live app-window viewer output diagnostics can additionally be persisted with
`MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT`. That JSONL stream records the last viewer
GPU output attempt, including the derived health summary, display/presentation
readiness, native GPU stage sequence, blocker breakdown, output texture state,
external texture registration outcome, and frame context needed to correlate
failures with a concrete sequence id, timeline frame, preview size, external
texture key, output target/color space, tone-map flag, optional display/view,
and the latest structured viewer color rejection when preview metadata policy
rejects an asset. The stream now also carries renderer-owned structured stage
evidence through `RenderGpuOutputStageDiagnosticsReport`, so downstream budget
or triage tools can migrate away from app-local flat stage counters without
redefining the renderer schema. It also carries the latest renderer runtime
snapshot through `RenderGpuOutputRuntimeDiagnosticsReport`, so viewer triage can
see shader-cache and backend-object failures from the same JSONL record stream.
The same records include cumulative ready/degraded/blocked/failed/rejected/waiting
health counts so smoke tooling can enforce viewer GPU-output budgets directly
from the JSONL stream. `viewer_gpu_output_budget` consumes this stream, emits a
versioned health report, and fails closed when ready/failed/blocked/rejected/
degraded thresholds are not met or when the reported cumulative `health_counts`
disagree with the statuses replayed from the JSONL records. Empty streams fail
explicitly with a `records` budget failure instead of being reported only as
missing ready frames. The report's embedded summary replays
`display_issue_summary` records into reason counts and payload-blocker counts,
and it replays viewer color rejections into a
`VideoColorDiagnosticIssueAggregate`, allowing smoke runs to fail on HDR,
wide-gamut, unsupported presentation, UI payload blockers, or metadata-policy
rejections even when a temporary health budget permits degraded frames.
Per-reason display thresholds are part of the contract, so CI can relax one
failure class for investigation without silently tolerating the rest, and
unknown future display reasons still fail closed instead of disappearing inside
an aggregate display-issue allowance. The same evaluator lives in
`app_ui::viewer_gpu_output_budget` so Rust smoke tests and the CLI share one
budget implementation; the ignored `viewer_gpu_output_budget_smoke` test reads
`MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT`, writes the same health report into
`MONDRIAN_PERF_OUTPUT` when configured, and fails the test on budget violations.
Export diagnostics may expose additional preflight helpers, but final job-level
counters must be produced from the frame render path.
Preview exposes the same frame-level input color-resolution source counts from
its preview-intent evaluation path. Tests compare preview and export source
counts for the same timeline frame so display/output differences cannot hide a
divergence in media input interpretation.
Nested-sequence recursion depth is a timeline-level contract exposed as
`MAX_NESTED_SEQUENCE_RENDER_DEPTH`; preview, export video, export audio, and
diagnostic source-count paths must all use that same limit instead of local
hard-coded values.
Preview final-frame cache keys must include the effective color context so a
monitor/output transform change cannot reuse stale pixels from a previous view.
For OCIO-backed preview this includes the resolved display and view names, not
only the output color-space enum, because two views on the same display can
produce different presentation pixels.

Preview callers must use `TimelineEvaluationRequest::preview(...)`. Export
callers must use `TimelineEvaluationRequest::export(...)`. Any future thumbnail,
analysis, AI, or cache-warm path should add an explicit intent instead of
reinterpreting sequence state directly.

The renderer test suite contains an explicit preview/export semantic-signature
contract. It allows request settings such as intent, quality, color target, frame
drop policy, and resolution scale to differ, but requires media source timing,
nested-sequence source timing, element order, blend/opacity, transforms, clip
interpretation, and nested color-processing mode to remain identical for the
same sequence frame. Renderer and app tests also pin the final color boundary:
for the same working frame, output color space, tone-map flag, and engine,
preview display and export delivery targets must produce identical RGBA pixels
while preserving distinct output domains in diagnostics. App-level parity tests
also compare `TimelineCompositeColorPathSummary`, the shared fields of the
preview/export color-health summaries, and the normalized versioned report
verdict/check/root-cause/action signatures for the same timeline frame, so
preview and export cannot silently diverge in float/linear versus legacy RGBA8
composite routing, stage scheduling, GPU blocker accounting, health booleans, or
diagnostic interpretation. The renderer golden suite includes a stable RGBA hash for
this preview/export Rec.709 boundary parity contract; intentional color-pipeline
changes must update that hash with the same care as image golden references.
The app preview suite also pins a stable Rec.2020-working to sRGB-output
multilayer preview/export RGBA hash, so preview and export cannot drift together
without an explicit golden update.
