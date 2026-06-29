# Render Pipeline

The intended render path is shared by preview and export:

```text
Timeline Evaluation
  -> FlatActiveClip
  -> TimelineRenderPlanElement
  -> Clip Sampling / Generated Source
  -> Effect Graph Evaluation
  -> Layer Composite
  -> Color Transform
  -> Display or Export Encode
```

## Current Implementation

`mondrian-renderer::timeline_render_plan` builds `TimelineRenderPlanElement` values from `RenderPlanSource`:

- `Media`
- `Adjustment`
- `SolidColor`
- `NestedSequence`

Each element carries opacity, blend mode, transforms where applicable, effect graph, frame seed, and color/media interpretation data.

`timeline_composite` still supports CPU RGBA compositing and a float-linear path for simple normal-blend media. `FrameCompositor` supports GPU batched compositing with texture pooling and can return either RGBA readback or a GPU texture.

## Required Semantics

- Disabled clips and zero-opacity clips do not enter the render plan.
- Clip blend mode overrides track blend mode; otherwise track blend mode applies.
- Adjustment layers operate on lower accumulated pixels, not as standalone media.
- Solid colors are generated sources, not file-backed frames.
- Nested sequences must preserve the configured nested color-processing mode.
- Effect graphs are compiled from clip effects plus masks at the evaluated time.

## Preview vs Export

Preview and export may use different scheduling, cache lifetime, and readback strategy. They must share timeline interpretation, clip ordering, effect evaluation, blend semantics, and color-management decisions.
