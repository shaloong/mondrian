# Timeline Model

Color-context construction delegates config-default display/view lookup to the
selected `ColorEngine`. Sequence preview and export planning never read an
unqualified process-global OCIO default, so a failed ACES or Custom config
cannot inherit a view from the previously active engine.

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
`validate_with_project_color_management` additionally validates the effective
inherited/overridden `ColorEngine`. Mondrian Standard v1 sequences use the exact
Linear Rec.2020 working identity pinned by the immutable package; Custom OCIO
sequences use the exact working space pinned by their project identity.
Application mutation boundaries call this validator before replacing a
sequence snapshot, and new sequences adopt the selected engine's pinned space.
Editing-mode presets preserve that space: DCI raster dimensions do not imply a
P3 working-space change.
The persisted `working_color_space` is a `WorkingColorSpace`, distinct from
external input and output `ColorSpace` values. Root color contexts carry a
display-referred output identity, while nested contexts carry their parent working
identity so render recursion cannot mistake an internal handoff for a delivery
boundary.
Sequence validation rejects scene-linear and scene-Log source identities as
presentation outputs even if a project file is authored outside the UI; the
sequence output must be one of the display-referred SDR/HDR identities. This
does not remove export's separate, explicit professional Log intermediate path.

New sequences default to the Mondrian Standard v1 working identity, unbounded
scene-linear Rec.2020, and a scene-referred workflow. Their initial SDR program
boundary therefore executes the version-pinned Mondrian Standard View. An
explicit DisplayReferred workflow remains available as a technical colorimetric
bypass. Project `ColorEngine` alone selects Mondrian Standard, ACES, or Custom
OCIO; the sequence workflow does not duplicate that mode selection.

`SequenceColorManagement.delivery_bit_depth` is the encoded deliverable sample
depth and currently permits 8-bit, 10-bit, or 12-bit output. Twelve-bit output
is reserved for ProRes 4444/4444 XQ. It does not describe
the float working space or the renderer-to-encoder pipe precision. Those are
renderer/export implementation contracts and are not persisted as editorial
intent.

`root_program_color_context` resolves the effective color engine from the
sequence/project inheritance rules and carries the one typed
`OutputTransformIntent` shared by preview program pixels, scopes, and export.
`root_preview_color_context` remains the explicitly separate local monitor
presentation request until the renderer applies it as monitor adaptation after
Program Output; it must never be used as the scope or delivery identity.
An ordinary display-referred SDR context remains `Colorimetric`; a
scene-referred or explicitly tone-mapped Mondrian Standard boundary resolves to
the fully pinned `MondrianStandard { package }` product intent. An explicitly
configured delivery display/view resolves to `OcioDisplayView` only while tone
mapping is active. No resolved display/view strings are stored beside the
intent, so contradictory context state is unrepresentable. This selection is
independent from the renderer's CPU/GPU execution backend so a backend change
cannot silently change project color science.

Standard output resolution is target-specific. SDR sRGB/Rec.709/P3 contexts
select `Mondrian Standard SDR v1`; Rec.2100 HLG and PQ contexts select
`Mondrian Standard HDR 1000 nits v1` under their respective OCIO displays.
Core resolves that target-specific view from the typed intent when renderer
constructs a Program Output boundary, so the target transfer function
changes only the display encoding and never selects a second HDR picture
formation.

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
