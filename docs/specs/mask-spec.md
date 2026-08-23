# Mask Spec

Masks are clip components. Data lives in `mondrian-core::mask_data`; rasterization/evaluation lives in `mondrian-effects`.

## Shapes

The current authoring model and float mask rasterizer execute these `MaskShape`
variants:

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

Shape keyframes are stored separately as `(TimelineTime, MaskShape)` in the
owning Clip's authoring time domain. When `shape_animation_enabled` is false,
the first shape keyframe is used. Evaluation maps that exact author time onto
the requesting render grid; mask storage never adopts a frame or fixed-tick
time base.

## Operations

`MaskOp` executes Add, Subtract, Intersect, and Difference. Masks compile into
effect graph `MaskSource` and `Mask` nodes. This execution statement does not
claim every UI editing workflow or GPU backend is product-verified.
