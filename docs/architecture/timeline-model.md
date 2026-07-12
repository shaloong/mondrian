# Timeline Model

`mondrian-timeline` owns editorial time, tracks, clips, and timeline commands.

Persisted positions, ranges, automation keys, and temporal handles use canonical
exact rational `TimelineTime`. `FramePosition` is an evaluation/display adapter,
not an author coordinate, and the former `TimeTicks = frame * 1000` path has
been deleted. Sequence frame rate remains a
video evaluation/snap grid and display-timecode input, not the universal storage
time base; audio edits may therefore retain sample-accurate boundaries without
creating a second Timeline model.

## Sequence

A `Sequence` contains:

- `id`, `name`, `role`
- `settings`
- ordered video and audio tracks
- playhead
- optional exact in/out range
- a Sequence-owned `AudioProgram`

Default sequences create `V1..V3` and `A1..A3`. `SequenceSettings` validates resolution, frame rate, audio sample rate/layout, preview settings, and color-management constraints.
The persisted `working_color_space` is a `WorkingColorSpace`, distinct from
encoded input and output `ColorSpace` values. Root color contexts carry an
encoded output identity, while nested contexts carry their parent working
identity so render recursion cannot mistake an internal handoff for a delivery
boundary.

`SequenceColorManagement.delivery_bit_depth` is the encoded deliverable sample
depth and currently permits 8-bit, 10-bit, or 12-bit output. Twelve-bit output
is reserved for ProRes 4444/4444 XQ. It does not describe
the float working space or the renderer-to-encoder pipe precision. Those are
renderer/export implementation contracts and are not persisted as editorial
intent.

Root preview/export color contexts resolve the effective color engine from the
sequence/project inheritance rules. When the effective engine is
`MondrianSmart` or explicit OCIO, the context carries the loaded OCIO config's
default display/view if one is available.

## Track

`Track` owns an ordered `Vec<Clip>` and track-level state:

- `track_type`: Video, Audio, Subtitle
- `height`
- `is_muted`, `is_locked`, `is_solo`, `is_visible`
- `blend_mode`
- animatable `track.opacity`

Video tracks use visibility and opacity. Audio tracks use mute/solo semantics. UI must not show speaker controls for video tracks or visibility controls for audio tracks unless a future explicit domain feature is added.

Audio authoring keeps the Timeline Track as the editorial container and keys
its Track Mixer Channel state by the same `TrackId` inside the Sequence-owned
Audio Program. Clip audio components are independently processable Audio
Contributions; the visual `Clip.effects` vector is not the audio processor rack.
Track creation/removal updates the keyed mixer state and default route in the
same Sequence mutation. Validation rejects a snapshot when the two sets differ.
See [Audio Pipeline](audio-pipeline.md) for the author/compiler boundary.

## Clip

`Clip` is the timeline instance, not the asset itself. It references `asset_id`, carries timeline/source ranges, transform, speed, effects, masks, linked clip, blend mode, and kind-specific data.

Supported `ClipKind`:

- `Media`
- `AdjustmentLayer`
- `NestedSequence`
- `SolidColor`

Transform, speed, blend mode, solid color, masks, and effects are currently
exposed through `PropertyHost`/`PropertyBag`. Long-term visual and audio
automation share exact curve primitives and stable Parameter IDs; string paths
remain UI aliases and migration inputs rather than identity.

## Render Projection

Timeline internals are projected into `FlatActiveClip` through `RenderPlanSource`. `mondrian-renderer` consumes that trait and must not depend on `Sequence`, `Track`, or `Clip` internals.
