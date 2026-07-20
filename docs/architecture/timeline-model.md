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

- stable `id`, monotonic author `revision`, `name`, `role`
- `settings`
- ordered video and audio tracks
- playhead
- optional exact in/out range
- a Sequence-owned `AudioProgram`

Default sequences create `V1..V3` and `A1..A3`. `SequenceSettings` validates
resolution, frame rate, audio sample rate/layout, preview settings, and
color-management constraints. Audio layout is the sole persisted channel
authority; no parallel channel-count field can diverge from it.

### Position Display

`SequenceSettings.timeline_display` is the sole persisted position-display
choice. It contains a `TimelineDisplayFormat` and a signed actual-frame offset
for the Sequence timecode origin. Resolving it with the Sequence video
evaluation rate produces one validated `TimelineDisplayContract`; Viewer and
Timeline ruler consume that same Interface and may not reconstruct counting or
origin arithmetic.

`Frames` displays signed Sequence-relative evaluation-frame offsets and ignores
the retained timecode origin. `Timecode` projects exact `TimelineTime` onto the
Sequence grid using an explicit rounding policy, then applies the origin and
formats SMPTE NDF or DF labels. Drop-frame is accepted only for exact
30000/1001 and 60000/1001 rates. Negative positions remain signed; conventional
SMPTE labels wrap after 24 hours while author time and frame display remain
exact and unbounded within their integer contract.

Display settings never change Clip placement, automation, nesting transforms,
or audio sample positions. Unsupported formats such as Feet+Frames are absent
from the persisted enum and product menu until their film gauge, footage
counting, origin, parsing, and reference fixtures are implemented as one real
contract.

## Author Identity, Revision, and Undo

`SequenceId` is stable identity; `SequenceRevision` is the persisted, nonzero,
monotonic author-transaction generation for that identity. A new or duplicated
Sequence starts at revision 1. Every committed author edit, Undo, and Redo
advances it without saturation. A Project color-engine replacement advances
every inheriting Sequence because its effective render semantics changed.
`ProjectDocument.document_revision` is only the generation of successful file
saves and is never a Playback or render-cache revision.

Track, Clip, Effect, Mask, animation-track, keyframe, audio edit/scope/processor,
Route, Bus, Output, Transition, and Role identities remain stable entity IDs.
They deliberately do not each own a second revision counter. The owning
Sequence revision is the conservative invalidation contract; definition,
processor, resource, color, media-source, and prepared-plan fingerprints provide
the narrower cache keys where retaining unaffected work matters. This avoids a
family of counters whose atomic agreement would be harder to prove than the
author transaction itself.

Project validation rejects duplicate Sequence, Track, Clip, Effect, and Mask
identities, invalid strong linked-Clip references, duplicate effect-local
Parameter identities, duplicate animation-track identities in one property
owner, and duplicate keyframe identities in one exact automation curve. Audio
Program validation owns the corresponding typed audio-entity uniqueness rules.
`AudioProcessorInstanceId` is unique across all Scope, Track, Bus, and Output
Racks in one Sequence; a shared Scope is referenced once in author data and may
materialize several independent generated execution occurrences.
Copy and razor operations must fork the identities specified by the Sequence
audio ADR; Sequence duplication forks every Sequence-owned identity and resets
only the new Sequence revision.

`mondrian-timeline::CommandHistory` is the deep Module behind the active
Sequence Undo seam. Commands declare their target `SequenceId` and exact
command-owned retained bytes. The default hard budget is 200 commands and
128 MiB across Undo and Redo. Complete Sequence snapshots are stored as bounded
serialized byte payloads rather than unaccounted cloned heap graphs. Oldest
entries are evicted in constant time, a new branch accounts for discarded Redo
bytes, an oversize edit is not retained, and immutable diagnostics expose all
three outcomes. Target mismatch and snapshot failure fail closed; failed
Undo/Redo returns the command to its original stack. App author mutations
propagate history errors and restore the preceding Sequence rather than saving
a partially recorded transaction. Sequence switching, project replacement and
a successful project color-engine replacement establish a new active-history
scope instead of replaying snapshots against another aggregate or inherited
color contract. A failed color-engine replacement preserves the prior state and
history.
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
Rack order and Processor Instance identity are author semantics and cannot be
collapsed into an aggregate gain or array-index address during compilation.
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
a versioned `ParameterSchema` with an address-independent `ParameterId`, value
type, definition default, automation capability, typed unit,
numeric/enum/resource, Hold/Linear/Bezier execution semantics, message, and
cache-impact contracts. Effect execution resolves the stable ID exactly; the
instance-qualified string path remains only an authoring/UI address alias and
cannot define execution identity. `PropertyBag::validate` rejects malformed
persisted descriptors, values, channel layouts, keyframe order, enum indices,
and interpolation before the project enters execution. Visual and audio
automation share exact curve primitives, stable Parameter IDs, and the same
Parameter Schema language. Audio Processor instances capture a schema snapshot
beside the exact curve so missing plugins remain editable; a known built-in must
still match its canonical definition schema exactly before compilation. Numeric
audio schemas may use exact integer sample-frame units; values must remain
exactly representable in the shared curve domain, and a parameter that changes
storage or continuity topology is non-animatable and forces plan re-preparation
rather than a live callback event.

`ExactAutomationCurve::prepared_segments` validates author order, finite values,
and Bezier time monotonicity once, then returns immutable interpolation segments
whose evaluator reuses the same Hold/Linear/Bezier mathematics as direct curve
evaluation. Execution Modules may lower those segments onto their Evaluation
Grid and advance span cursors; they may not copy the interpolation formulas or
reinterpret the persisted author curve.

## Render Projection

Timeline internals are projected into `FlatActiveClip` through `RenderPlanSource`. `mondrian-renderer` consumes that trait and must not depend on `Sequence`, `Track`, or `Clip` internals.
