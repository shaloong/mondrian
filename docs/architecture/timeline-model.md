# Timeline Model

Color-context construction delegates target-qualified output View lookup to the
selected `ColorEngine`. Sequence preview and export planning never read an
unqualified process-global OCIO default, so a failed ACES or Custom config
cannot inherit a View from the previously active engine or reuse one output
binding under another delivery label.

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
inherited/overridden `ColorEngine`. Mondrian Standard sequences use the exact
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

New sequences default to Mondrian Standard working-space identity v1, unbounded
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

When `static_hdr_metadata_policy` is `WriteAuthored`, `SequenceSettings`
requires complete typed ST 2086 mastering-display and CTA-861.3 content-light
payloads. `Omit` is the default. The policy never means source passthrough;
these values describe the finished sequence delivery. Validation
rejects invalid rationals, impossible CIE xy coordinates, unordered mastering
luminance, and non-positive or unordered MaxCLL/MaxFALL values at the persisted
domain boundary. Output-View-specific content-peak checks remain an export
responsibility because they depend on the effective inherited color engine.

`root_program_color_context` resolves the effective color engine from the
sequence/project inheritance rules and carries the one typed
`OutputTransformIntent` shared by preview program pixels, scopes, and export.
`root_preview_color_context` remains the explicitly separate local monitor
presentation request until the renderer applies it as monitor adaptation after
Program Output; it must never be used as the scope or delivery identity.
An ordinary display-referred SDR context remains `Colorimetric`; a
scene-referred or explicitly tone-mapped Mondrian Standard boundary resolves to
the fully pinned `MondrianStandard { package }` product intent. ACES carries its
preset and Custom OCIO carries only a target-qualified `CustomOcio` intent; the
engine identity owns the corresponding display/view/output-endpoint binding.
No resolved display/view strings are stored beside the intent, so contradictory
context state is unrepresentable. A Custom target absent from the pinned output
binding set fails sequence validation before render planning. This selection is
independent from the renderer's CPU/GPU execution backend so a backend change
cannot silently change project color science.

Standard output resolution is target- and package-specific. Current-package SDR
sRGB/Rec.709/P3/Rec.2020 contexts select `Mondrian Standard SDR v2`; a project
that explicitly pins the legacy v2 package continues to select
`Mondrian Standard SDR v1`. Rec.2100 HLG and PQ contexts select
`Mondrian Standard HDR 1000 nits v1` under their respective OCIO displays.
Core resolves that target-specific view from the typed intent when renderer
constructs a Program Output boundary, so the target transfer function
changes only the display encoding and never selects a second HDR picture
formation.

The SceneReferred Standard Program Output selector exposes only the six targets
with exact versioned package Views: sRGB, Rec.709, Display P3, Rec.2020 SDR,
Rec.2100 HLG, and Rec.2100 PQ. Rec.601 PAL/NTSC remain supported source
interpretations and explicit DisplayReferred colorimetric outputs; they are not
misrepresented as Standard rendering Views. Domain validation resolves the
effective typed output intent against the selected package, so an externally
authored unsupported Standard target fails before render planning.

ACES uses the same typed-intent validation boundary. A sequence carries the
immutable ACES preset until its requested output target is resolved, rather
than materializing the preset's default Rec.709 View for every target. The
official Studio preset supports sRGB, Rec.709, Display P3, HLG, and PQ; the CG
preset omits HLG, and both omit Rec.2020 SDR. Unsupported combinations fail
sequence validation instead of producing a differently encoded signal with
contradictory delivery metadata.

## Track

`Track` owns an ordered `Vec<Clip>` and track-level state:

- `track_type`: Video, Audio, Subtitle
- `height`
- `is_muted`, `is_locked`, `is_visible`
- `blend_mode`
- animatable `track.opacity`

Video tracks use visibility and opacity. Audio tracks persist mute; solo is a
transient audition overlay outside the canonical author snapshot. UI must not
show speaker controls for video tracks or visibility controls for audio tracks
unless a future explicit domain feature is added.

Audio authoring keeps the Timeline Track as the editorial container and keys
its Track Mixer Channel state by the same `TrackId` inside the Sequence-owned
Audio Program. An audio Clip owns placement-local `AudioComponentEdit` values;
their Track and Sequence range are always derived from the owning Track/Clip.
They bind to Sequence-owned `AudioProcessingScope` values for Clip-level
processor continuity. The visual `Clip.effects` vector is not an audio rack.
Track creation/removal updates the keyed mixer state and default route in the
same Sequence mutation. Validation rejects a snapshot when the two sets differ.
See [Audio Pipeline](audio-pipeline.md) for the author/compiler boundary.

## Clip

`Clip` is the timeline instance, not the asset itself. It references `asset_id`,
carries timeline/source ranges, transform, speed, effects, masks, linked clip,
blend mode, kind-specific data, and placement-local audio component edits.

Supported `ClipKind`:

- `Media`
- `AdjustmentLayer`
- `NestedSequence`
- `SolidColor`

Transform, speed, blend mode, solid color, masks, and effects are currently
exposed through `PropertyHost`/`PropertyBag`. Every product definition carries
a versioned `ParameterSchema` with an address-independent `ParameterId` and
typed unit, numeric/enum/resource, interpolation, message, and cache-impact
contracts. Effect execution resolves the stable ID exactly; the
instance-qualified string path remains only an authoring/UI address alias and
cannot define execution identity. `PropertyBag::validate` rejects malformed
persisted descriptors, values, channel layouts, keyframe order, enum indices,
and interpolation before the project enters execution. Visual and audio
automation share exact curve primitives and stable Parameter IDs; audio
Processor definitions still need to publish the same parameter-description
contract before the M0 parameter milestone is closed.

`ExactAutomationCurve::prepared_segments` validates author order, finite values,
and Bezier time monotonicity once, then returns immutable interpolation segments
whose evaluator reuses the same Hold/Linear/Bezier mathematics as direct curve
evaluation. Execution Modules may lower those segments onto their Evaluation
Grid and advance span cursors; they may not copy the interpolation formulas or
reinterpret the persisted author curve.

## Render Projection

Timeline internals are projected into `FlatActiveClip` through `RenderPlanSource`. `mondrian-renderer` consumes that trait and must not depend on `Sequence`, `Track`, or `Clip` internals.
