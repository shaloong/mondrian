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

The legacy `build_timeline_render_plan` helper delegates to the evaluation API
and exists only for compatibility with code that needs the element list.

`TimelineRenderPlanElement` variants are:

- `Media`
- `Adjustment`
- `SolidColor`
- `NestedSequence`

Each element carries opacity, blend mode, transforms where applicable, effect graph, frame seed, and color/media interpretation data.

`timeline_composite` still supports CPU RGBA compositing and a float-linear path
for simple normal-blend media. Viewer preview and export both enter the
float-linear compositor when that path supports the resolved elements, then
apply the same working -> output color transform contract for their respective
preview/export `ColorContext`. `FrameCompositor` supports GPU batched compositing
with texture pooling and can return either RGBA readback or a GPU texture.

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
