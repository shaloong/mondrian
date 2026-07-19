# Keyframe Spec

Keyframes use the shared parameter and automation contracts in
`mondrian-core`. A keyframe is author data; frame and sample positions are
consumer-specific execution projections.

## Time and ownership

- Every persisted keyframe and temporal handle uses canonical rational
  `TimelineTime`.
- The property owner declares the `AuthoringTimeDomain`: Clip/component-local,
  Processing Scope-local, or Sequence-local. A bare time value never implies a
  domain.
- Times from different domains are mapped only through an explicit validated
  `TimeTransform` before comparison or evaluation.
- Video frames, audio samples, motion-blur instants, and parameter-event batches
  are `EvaluationGrid` projections with a declared rounding policy. They are
  not stored as the keyframe coordinate.
- Negative and sub-frame times are valid when the owning domain admits them.
  Floating-point seconds and fixed ticks are not persistence or interchange
  formats.

## Parameter identity and values

Every property definition carries a versioned `ParameterSchema`. Stable
`ParameterId` identifies the definition; a property path is only the current
instance address used by UI and command routing.

`PropertyValue` supports Bool, Int, Float, Double, Vec2, Vec3, Vec4, Color,
Enum, Resource, and Text. The Schema owns the definition default, animation
capability, unit, hard/soft numeric bounds, enum keys, resource intent, allowed
execution interpolation, localization message identity, and cache impact.
Discrete values use Hold execution semantics unless a future Schema revision
defines another mathematically valid rule. Vector and color values are
evaluated channel-by-channel while retaining one logical keyframe identity at
one exact author time.

## Interpolation

Only three execution semantics are persisted:

- Hold: retain the left key value until the next key.
- Linear: interpolate against the exact rational interval.
- Bezier: evaluate explicit temporal/value handles whose time coordinates are
  monotonic and contained by the segment.

Auto Bezier, Continuous Bezier, Ease In, and Ease Out are editor presets. They
author concrete Bezier handles and temporal editing flags; they are not extra
runtime algorithms or Parameter Schema capabilities. Persisted curves reject
non-finite values, duplicate key identities, non-increasing times, unsupported
interpolation, and invalid Bezier time handles before entering an immutable
Sequence snapshot.

`ExactAutomationCurve::prepared_segments` is the shared lowering seam for
numeric execution. Video and audio runtimes may prepare span cursors or dense
event batches for their own Evaluation Grids, but may not copy or reinterpret
Hold/Linear/Bezier mathematics.

## Mutations and transactions

Product edits flow through `PropertyMutation` and `PropertyHost`, then through
the owning Sequence command transaction. Constructors and narrowly scoped
domain helpers may initialize fields directly; UI panels, renderers, audio
runtimes, and plugins may not bypass validation or create a second static value
beside the curve default.

Animation enable/disable, key insertion/removal/move, interpolation changes,
handle edits, channel edits, and reset must preserve stable Parameter,
animation-track, and keyframe identities according to the command's semantic
operation. Preview, export, cache invalidation, save/reopen, and Undo/Redo all
consume the same validated author snapshot.
