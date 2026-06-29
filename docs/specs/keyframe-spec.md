# Keyframe Spec

Keyframes use the shared automation system in `mondrian-core::automation`.

## Time

Automation time uses `TimeTicks`.

```text
SUBFRAME_TICKS_PER_FRAME = 1000
timecode_to_ticks(frame) = frame * 1000
```

Persisted timeline structure remains frame-based; ticks allow subframe automation and future audio-rate precision.

## Values

Supported property values:

- Bool
- Int
- Float
- Double
- Vec2
- Vec3
- Vec4
- Color
- Text

Text is not interpolated.

## Interpolation

Supported intent/UI modes:

- Hold
- Linear
- Bezier
- AutoBezier
- ContinuousBezier
- EaseIn
- EaseOut

Persisted interpolation uses `KeyframeInterpolation` and temporal flags. UI modes map to concrete handles/flags.

## Multi-Dimensional Properties

Vector/color values interpolate component-wise. Spatial editing may use UI metadata (`supports_spatial`) but must still mutate the underlying property path.

## Mutations

All property edits should flow through `PropertyMutation` and `PropertyHost`. Direct field edits are acceptable only for constructors, migrations, or tightly scoped domain helpers.
