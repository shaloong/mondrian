# UI Renderer Primitive Policy

Mondrian UI primitives should use CPU geometry for batching and coarse coverage, then GPU analytic
distance fields for edges that users can see.

## Lines

- Build line quads on CPU from the requested segment, stroke width, round-cap radius, and conservative
  AA padding.
- Shade line coverage on GPU with a pixel-space segment SDF. Do not rely on fixed-function thin
  triangle rasterization for 1px or near-1px editor strokes.
- Clamp logical hairlines to at least 1 physical pixel before generating geometry.
- Keep line local coordinates in pixel space so AA width is independent of line angle.
- Regression tests must cover 45-degree and non-cardinal angles, subpixel start/end phases,
  positive and negative slopes, representative DPI scales, round caps, and visible connected
  coverage from start cap to end cap.

## Rounded Rects And Circles

- Render rounded rectangles and circles with GPU analytic rounded-box SDFs.
- Treat circles as square rounded rectangles with radius equal to half the side.
- Tests must verify opaque centers, transparent corners, finite boundary SDFs at primary angles, and
  visible AA coverage at edges across representative DPI scales.

## Triangles

- Normalize triangle winding on CPU before batching.
- Keep GPU pipeline culling disabled for UI primitives so vertex ordering mistakes do not disappear
  on one backend and fail on another.
- Reject non-finite or degenerate triangles before they reach the GPU.
- Tests must verify stable winding, subpixel vertex preservation, filled interior coverage, and
  transparent exterior samples.

## Raster Images And Icons

- Use raster atlas sampling for bitmap assets and CPU prefiltered rasterization for small SVG icons.
- Keep vector-like editor primitives as analytic geometry unless an asset has already been authored
  as an image.
