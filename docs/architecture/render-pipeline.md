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
values. Renderer diagnostics must consume `ColorSpace::encoding()` rather than
duplicating color-space metadata.

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
contract, then apply their respective working -> output transform through
`CpuColorTransformExecutor` and `RenderColorTransform`.
Bare RGBA8 buffers are valid only at source import, debug/golden snapshot, UI
presentation readback, and CPU encoder boundaries. They are not a renderer-stage
exchange format.

Decoded media enters the graph as a typed source/import RGBA8 boundary
(`CpuEncodedColorFrame::source_rgba8`). Preview and export must use
`RenderInputTransform` plus `CpuColorTransformExecutor::input_to_working(...)`
to produce a result carrying both `CpuColorFrame` and execution diagnostics
before building `TimelineMediaLayer`. Timeline media layers therefore carry
typed working frames, not naked RGBA slices.

Color-transform executors emit `RenderColorTransformDiagnostics` for input and
output boundaries. Preview diagnostics aggregate transform calls, transformed
pixels, and temporary RGBA8 boundary crossings so performance smoke tests can
catch accidental CPU-bound color work as the GPU path comes online.

`FrameCompositor` supports GPU batched compositing with texture pooling and can
return either RGBA readback or a GPU texture. Long-term render graph nodes should
return typed frame handles rather than naked textures or byte vectors.

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
