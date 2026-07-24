# Timeline Model

Color-context construction delegates target-qualified output View lookup to the
owning Project's exact `ColorEngine`. Sequence preview and export planning never read an
unqualified process-global OCIO default, so a failed ACES or Custom config
cannot inherit a View from the previously active engine or reuse one output
binding under another delivery label.

`mondrian-timeline` owns editorial time and the validated Sequence author model.
`mondrian-editor-state::AuthoringSession` owns project transactions and bounded
Undo/Redo; the Timeline crate has no second mutable document or command history.

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
- Sequence-owned explicit video Transitions
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
advances it without saturation. A Project color-environment replacement
advances the Project `AuthorGeneration`, not the persisted revision of every
unchanged Sequence. Resolved contexts and execution/cache keys carry the exact
Project engine identity, so engine replacement invalidates pixels without
pretending that Sequence author bytes changed.
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

Project validation rejects duplicate Sequence, Track, Clip, Effect, Mask, and
Video Transition identities, invalid strong Transition endpoints, singleton
Clip link groups, duplicate effect-local
Parameter identities, duplicate animation-track identities in one property
owner, and duplicate keyframe identities in one exact automation curve. Audio
Program validation owns the corresponding typed audio-entity uniqueness rules.
`AudioProcessorInstanceId` is unique across all Scope, Track, Bus, and Output
Racks in one Sequence; a shared Scope is referenced once in author data and may
materialize several independent generated execution occurrences.
Copy and razor operations must fork the identities specified by the Sequence
audio ADR; Sequence duplication forks every Sequence-owned identity and resets
only the new Sequence revision.

`AuthoringSession` is the only production owner of a mutable
`ProjectDocument`. An edit operates on a cloned Sequence or Project candidate;
only after the candidate validates is it installed, assigned the next
`AuthorGeneration`/`SequenceRevision`, and recorded in project-wide history.
UI and execution code receive read-only references or immutable snapshots.
There is no production API for “mutate first, record later”, so an edit error,
validation error, history serialization failure, or oversize-history outcome
cannot leak a partial canonical state.

The default history budget is 200 commands and 128 MiB across Undo and Redo.
Sequence and Project snapshots are retained as bounded serialized payloads,
not unaccounted heap clones. Oldest entries are evicted in constant time; a new
branch accounts for discarded Redo entries; an oversize edit remains committed
but is explicitly reported as not retained. Undo and Redo are project-wide and
remain valid while navigating between Sequences. Restored Sequences receive a
new monotonic revision rather than reusing the historical revision. Opening or
replacing a Project creates a new `AuthoringSessionId`, preventing an old
persistence completion or history entry from targeting the new lifetime.
`validate_with_color_environment` additionally validates a Sequence against the
Project's exact `ColorEngine`. Mondrian Standard Sequences use the exact
Linear Rec.2020 working identity pinned by the immutable package; Custom OCIO
Sequences use the exact working space pinned by their Project identity.
Application mutation boundaries call this validator before replacing a
Sequence snapshot. Project-environment replacement validates the complete
future-Sequence template and every existing Sequence as one transaction.
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

The Project owns a complete `new_sequence_defaults` template. Project creation
copies it exactly into the first Sequence; later Sequence creation does the
same. Changing that template affects future Sequences only and never becomes a
runtime fallback for an existing Sequence. The default new Project uses
Mondrian Standard, Linear Rec.2020 and a scene-referred workflow. An explicit
DisplayReferred workflow remains available as a technical colorimetric bypass.
Project `ColorEngine` alone selects Mondrian Standard, ACES, or Custom OCIO;
the Sequence workflow does not duplicate that mode selection.

`SequenceColorManagement.delivery_bit_depth` and `video_range` are the
Sequence-level delivery defaults. They do not force every export of the
Sequence to use one representation: a typed `ExportPreset` may follow either
default or provide an explicit bit depth/range. Export admission resolves that
choice together with codec profile and chroma sampling before execution.
Twelve-bit delivery is currently reserved for ProRes 4444/4444 XQ. None of
these values describes the float working space or renderer-to-encoder pipe
precision; those are derived execution contracts and are not persisted as
editorial pixels.

`SequenceColorManagement` has no engine, override, or inheritance flag. It owns
workflow, input-metadata policy, Program Output color and tone-map intent,
delivery range/bit-depth defaults, and authored HDR metadata. Machine-local
Viewer display policy belongs outside author data. Nested processing belongs to
the parent-to-child Clip placement edge, not either Sequence.

When `static_hdr_metadata_policy` is `WriteAuthored`, `SequenceSettings`
requires complete typed ST 2086 mastering-display and CTA-861.3 content-light
payloads. `Omit` is the default. The policy never means source passthrough;
these values describe the finished sequence delivery. Validation
rejects invalid rationals, impossible CIE xy coordinates, unordered mastering
luminance, and non-positive or unordered MaxCLL/MaxFALL values at the persisted
domain boundary. Output-View-specific content-peak checks remain an export
responsibility because they depend on the exact Project color engine.

`root_program_color_context(ProjectColorEnvironment)` combines the Project
engine with Sequence semantics and carries the one typed
`OutputTransformIntent` shared by preview program pixels, scopes, and export.
`root_preview_color_context(ProjectColorEnvironment, target)` remains the
explicitly separate local monitor
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

`Clip` is one Timeline placement, never an Asset or Sequence definition. It
owns position/source ranges, a stable `clip_time_in`, transform, speed, visual
effects, masks, optional link-group membership, blend mode, and placement-local
audio Component Edits. Its content is one closed `ClipContent` payload:

- `Media { asset_id, interpretation }`
- `AdjustmentLayer { asset_id }`
- `NestedSequence { sequence_id }`
- `SolidColor { asset_id, color }`
- `BasicTitle { title }`

The variant is the single source of truth. There is no parallel `kind`,
`asset_id`, nested ID, interpretation, or solid-color field that can describe a
contradictory Clip. Current-schema deserialization rejects legacy or unknown
parallel fields. Constructors create only the identity required by the chosen
variant; for example, a nested placement does not manufacture a fake Asset ID.
Callers must also choose the identity they mean: `library_asset_id()` addresses
Media, Adjustment Layer, and Solid Color records in the Asset Library, while
`media_asset_id()` returns only file-backed Media dependencies. There is no
generic `asset_id()` because treating a generated library identity as a file
dependency made valid Solid Color/Adjustment exports fail as offline media.

Every Clip-owned visual processor uses one exact Clip-local author coordinate:
Transform, Opacity, visual Effects, Masks, and generated visual content.
Sequence evaluation maps
`clip_time = clip_time_in + (sequence_time - position)`. Moving the placement,
slipping the source, or changing the source Speed Map preserves
`clip_time_in`; trimming the in edge, splitting, or creating a right-hand
overwrite fragment advances it by the removed placement duration. Source time
is separately derived through `source_in` and `SpeedMap` and controls only
media/nested sampling. This prevents placement, source selection, and visual
processing from becoming three accidental authorities for one property.

`BasicTitle` is a fifth content type, not a synthetic Asset and not an Effect.
Its closed definition-backed Property Bag owns text, exact requested font
family/weight/style, font size, working-linear fill, tracking, line height, and
horizontal/vertical alignment. Font size, fill, tracking, and line height use
the ordinary exact automation model in Clip-local visual time; discrete text,
font, and alignment values remain static until the product defines an explicit
discrete-edit workflow. Unknown, missing, removed, type-divergent, or
out-of-contract properties are rejected before an author snapshot commits.

The Sequence `title_safe_margin` is a total width/height fraction: `0.20`
means 10% per edge. Both action/title safe margins must be finite and in
`[0, 1)`, so persisted settings can never yield NaN layout or consume the
complete canvas. Title creation prefers the selected free unlocked video Track,
then the topmost free unlocked Track; only when none is available does it add a
Track. Adding that Track and placing the title is one author transaction and one
Undo step.

Copy, paste, razor, overwrite-created fragments, and Sequence duplication fork
every addressable placement identity: Clip, Effect, Mask, visual animation
tracks/keyframes, and the corresponding audio aggregate identities. External
references such as Asset ID or nested Sequence ID remain references. Sequence
duplication additionally remaps Track-keyed routes, Transition endpoints, and
link groups, producing a disjoint author graph rather than two aggregates that
share mutable instance identity.

### Link Groups

`Clip.link_group: Option<ClipLinkGroupId>` is set membership, not a pair pointer.
A group may contain two or more video/audio placements and supports imported
multi-component media without inventing chains of pair links. Selection and
structural commands expand from any member to the complete group, validate all
source and destination Track locks before mutation, preserve member-relative
time, and reject a move that would put any member before Sequence zero. Razor
forks one group for the right-hand pieces. If overwrite trimming/splitting
breaks the synchronization promise, affected fragments leave the group rather
than retaining a misleading relationship. Singleton groups are compacted after
structural edits and rejected at the persisted boundary.

### Video Transitions

A visual Transition is a Sequence-owned, typed two-input author entity. It has
a stable `VideoTransitionId`, two strong Clip endpoints, one exact Sequence-time
range, a built-in or plugin definition identity, a complete Property Bag, and
explicit enabled state. It deliberately does not persist `track_id`: endpoint
Track membership is derived from the two Clips, preventing two authorities from
disagreeing.

Validation requires distinct endpoints on the same video Track, exact adjacent
edit geometry at one shared cut, a non-empty range that covers that cut and is
contained by the two placement ranges, a unique endpoint pair, valid properties,
non-adjustment endpoints, and non-overlapping Transition ranges on one Track.
The last rule removes otherwise undefined simultaneous three-input evaluation
around a short middle Clip. Structural edits either preserve those facts or
remove the now-invalid Transition before commit; invalid persisted graphs are
rejected instead of repaired during open.

`VideoTransition::source_demand` maps the complete Transition interval into
both source domains without clamping. A media/nested Adapter must supply probed
source extents to `validate_source_extents`; insufficient handles fail closed.
The author model therefore never substitutes repeated boundary frames or reads
outside a source. This is the frozen author/adapter contract. The current
App authoring boundary resolves the primary video stream's declared duration,
single-frame still semantics, or child-Sequence duration before create/range
edits. Container duration is never substituted for a missing video-stream
extent because a longer audio stream could admit nonexistent video handles. Insufficient
handles reject by default. `ShortenToAvailable` is an explicit product command,
intersects both exact source extents while retaining the edit, and commits the
shortened range in the same single Undo transaction. Export snapshot capture
repeats the preflight because relink or child edits can change a recoverable
external dependency without making the author graph structurally illegal.

`RenderPlanSource::flat_visual_items_at` projects ordinary Clips and explicit
two-input Transitions as ordered visual items. During the Transition interval,
one Transition replaces both endpoint placements at that Track stack position.
Both endpoint source times remain unclamped, so Preview/Export either obtain
the requested handles or fail; neither repeats a boundary frame. Cross Dissolve
is executed by the shared Preview/Export CPU working compositor. Product
authoring uses one App-owned command seam: the default is an approximately
one-second range centered on the exact cut and snapped to the Sequence video
grid, clamped only to the endpoint placement union. Real source-handle
admission remains fail-closed and never silently selects the explicit
shortening policy. A selected Transition stores only its stable identity;
Track membership continues to derive from the strong Clip endpoints. Timeline
overlays now create Cross Dissolve at an exact adjacent unlocked video cut,
select/delete by stable identity, and resize either exact range edge through one
typed App action. The view Adapter reports current insufficient or unresolved
source handles without modifying author state. A GPU Transition lowering remains
separate M1 work; until it exists, shared CPU Preview/Export execution remains
the only normative Cross Dissolve backend.

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

Automation authoring uses `AnimationParameterAddress { animation_track_id,
parameter_id }` as the stable property-instance address and `KeyframeId` as the
stable control-point identity. A `PropertyBag` resolves that pair to its current
path only at the mutation boundary. Selection, clipboard, and UI commands never
persist point indices, key times, display labels, or paths as identity; repeated
effects with the same `ParameterId` therefore remain distinct, while a
definition-compatible path alias change does not invalidate selection.

Moving a key is the atomic `EditKeyframe` mutation. It validates the complete
multi-channel key, target-time collision, value schema, and final curve before
publication, while preserving the key ID, interpolation handles, and temporal
flags. Stale or ambiguous identities and collisions reject the detached
candidate without advancing Author Generation, Sequence Author Revision,
dirty state, or Undo history. Insert, move/value edit, and remove are
incremental intents; no editor may implement one gesture by replacing the
whole curve or by exposing a remove-then-insert intermediate.

`ExactAutomationCurve::prepared_segments` validates author order, finite values,
and Bezier time monotonicity once, then returns immutable interpolation segments
whose evaluator reuses the same Hold/Linear/Bezier mathematics as direct curve
evaluation. Execution Modules may lower those segments onto their Evaluation
Grid and advance span cursors; they may not copy the interpolation formulas or
reinterpret the persisted author curve.

Mask scalar properties are part of the persisted Property Bag, not runtime
defaults. Project validation requires the canonical feather, opacity,
expansion, invert, and operation Parameter IDs, valid curves, finite geometry,
and strictly ordered shape keys. Saving and reopening must therefore preserve
the exact evaluated mask rather than silently reconstructing default values.

Every structural Timeline command ends by compacting the three dependent
author graphs as one invariant-restoration step: Clip link groups, visual
Transition strong references, and the Sequence audio program. The resulting
candidate is then fully validated and atomically committed by
`AuthoringSession`.

## Render Projection

Timeline internals are projected into `FlatActiveClip` through `RenderPlanSource`. `mondrian-renderer` consumes that trait and must not depend on `Sequence`, `Track`, or `Clip` internals.
