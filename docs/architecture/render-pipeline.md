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
`RenderOutputColorBoundaryExecutor` is the CPU final-output execution boundary:
callers choose an explicit strategy at construction time, and the executor owns
the final-output plan/execute sequence instead of exposing low-level transform
executors to app/export code. CPU callers use `cpu_only().execute(...)`; native
GPU app/export callers must use
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
readback in renderer-owned order. Lower-level renderer code that already owns a
validated
`RenderOutputColorBoundaryStagePlan` or `RenderGpuOutputStageResourcePlan` may
call the matching stage/resource recorder with renderer backend contexts.
GPU-to-CPU output for encode, thumbnails, tests, or debug captures must use one
of those renderer-owned paths; readback is valid only for explicit encoded RGBA8
output contracts unless a future conversion stage says otherwise.
Current CPU preview/export execution uses `execute_cpu_input_stage(...)` for
source boundaries and `RenderOutputColorBoundaryExecutor::cpu_only()` for final
display or export output. The compatibility helper
`execute_cpu_output_boundary(...)` delegates to that executor; app/export code
should not call `execute_cpu_output_stage(...)` directly. Direct
`CpuColorTransformExecutor` usage is limited to renderer internals and its
focused unit tests.

Stage helpers return `RenderColorStageExecution<T>`, not the raw transform
result. App, export, tests, and benches must read frames from `.result` and
aggregate `.stage_diagnostics` where they expose observability. Preview
diagnostics and export performance smoke reports record stage-plan counts,
CPU/GPU stage mix, transfer stages, GPU blockers, and touched pixels so later
GPU execution work can prove it removed CPU bottlenecks instead of merely
moving code around.

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
override first, then media metadata, then the configured missing-metadata policy.
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
same sequence frame.
