# Mask Spec

Masks are clip components. Data lives in `mondrian-core::mask_data`; rasterization/evaluation lives in `mondrian-effects`.

## Shapes

Supported `MaskShape`:

- Rectangle
- Ellipse
- Bezier path

Bezier paths store position, incoming control, and outgoing control per point.

## Scalar Properties

Mask scalar properties:

- feather
- opacity
- expansion
- invert
- mask_op

These are stored in a `PropertyBag` and exposed through clip property paths:

```text
mask.<mask_id>.<property>
```

## Shape Animation

Shape keyframes are stored separately as `(TimeTicks, MaskShape)`. When `shape_animation_enabled` is false, the first shape keyframe is used.

## Operations

`MaskOp` supports Add, Subtract, Intersect, and Difference. Masks compile into effect graph `MaskSource` and `Mask` nodes.
