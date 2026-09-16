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

`MaskOp` executes Add, Subtract, Intersect, and Difference. Each enabled Window
rasterizes to an `AlphaMask` through `MaskSource`; an ordered Window stack is
reduced explicitly through `MaskCombine`, then `MatteMix` applies that matte to
the ungraded base and graded result:

```text
output.rgb = mix(base.rgb, graded.rgb, matte.a)
output.a   = base.a
```

A Power Window is therefore a grade matte, not Clip transparency. Inversion is
part of the `MaskSource` boundary, all four operations are shared by CPU RGBA8,
CPU Float32, temporal/ROI execution, and the admitted CPU-to-GPU route, and HDR
or negative working RGB remains unclamped.

## Product Authoring

The Viewer edits Rectangle translation/corners, Ellipse translation/X-Y radii,
and Bezier anchors/incoming/outgoing controls. Pointer motion is widget-local
preview state; pointer-up commits exactly one complete shape action. Playback,
Track lock, or Window edit lock makes the overlay read-only. Inspector creation
supports all three shapes and exposes the same ordered Window stack and scalar
properties.

Undo/Redo and `.mdp` save/reopen preserve `MaskId`, shape `KeyframeId`, complete
Bezier controls, scalar animation, and Window ordering. Preview and Export both
evaluate the same `PreparedEffectProgram -> CompiledEffectGraph`; neither owns a
second Window interpretation.

## Motion Tracking

Every Mask may retain one `MaskTrackingRecipe` with a stable `TrackingId`,
object-translation or eight-degree-of-freedom planar model, forward/backward/both
direction, exact Clip-local anchor and generated range, bounded analysis
settings, physical video-stream index, complete source fingerprint, and quality
evidence. Generated shapes remain ordinary shape keys. Object tracking preserves
Rectangle/Ellipse/Path topology; planar tracking canonicalizes primitives once
to a fixed-topology Bezier path and projectively transforms anchors and both
control endpoints.

Tracking is an App-owned background analysis, not render-time evaluation. One
immutable request freezes Authoring Session, Sequence revision, Clip retime and
source-sample mapping, Asset/source revision, stream and color interpretation.
The dedicated bounded worker decodes exact still frames, runs deterministic
Effects-owned feature matching and robust translation or homography fitting,
and publishes only a complete result through one Sequence transaction. Cancel,
decode/quality failure, changed Session/Sequence/Clip/Mask/lock/source, or an
invalid transform publishes no shape key. A matching completed request may use
the service's bounded exact-result cache; explicit Recompute bypasses it.

Manual shape writes and disabling shape animation clear tracking provenance.
Applying a new completed result atomically replaces only its exact range,
preserves an existing exact-time `KeyframeId`, and keeps shape animation enabled.
The recipe and generated keys persist in `.mdp`; decoded rasters, features,
pyramids, worker state, and cache entries never do.
