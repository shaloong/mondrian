# Clip Specification

A Clip is one Timeline placement. It is not an Asset or Sequence definition.
Track membership is owned only by the containing Track.

## Common author state

Every Clip owns:

- stable `ClipId`;
- one closed `ClipContent` payload;
- exact rational `position`, `duration`, and `clip_time_in`;
- one closed exact `ClipSourceTimeMap`;
- built-in visual Transform and opacity;
- ordered visual effects and masks;
- optional `ClipLinkGroupId` membership;
- placement-local audio Component Edits;
- disabled state, optional blend override, and optional label.

Timeline coverage is the half-open range `[position, position + duration)`.
The visible Clip visual-author range is
`[clip_time_in, clip_time_in + duration)`. Transform, Opacity, visual Effects,
Masks, and generated visual content all evaluate in this one Clip-local
domain. Moving the placement, slipping the source, or changing the source time
map preserves `clip_time_in`; an in-edge Trim, Split, or right-hand overwrite
fragment advances it by the removed placement duration.
`timeline_to_source_sample()` subtracts placement position, applies the explicit
source-time map, and returns one `SourceSampleTarget`: the exact source
coordinate plus its half-open sampling ownership. The current constant variant
owns `source_origin`, an exact `TimeScale`, and the sampling boundary retained by
a hold; its terminal boundary is derived by mapping `duration`, never persisted
independently. Positive, negative, and zero scales express forward, reverse, and
hold semantics. A coordinate-only projection must not be used at an execution,
cache, nesting, Preview, or Export boundary because it cannot distinguish a
covering sample from the strict predecessor of an exclusive reverse edge.
Persisted author coordinates are `TimelineTime`; frame numbers are
evaluation/display projections only.

The current product constant-retime contract has two closed operations:

- set a signed, nonzero exact rate while preserving placement, Timeline
  duration, Clip-local visual time, and the existing visible source span. A sign
  change anchors at the old exclusive terminal boundary, so reversal does not
  jump to unrelated media;
- freeze a video Clip at the complete source sample visible at an in-range
  Sequence time while preserving its existing duration and sampling boundary.

Signed rate may explicitly expand the complete Clip link group and rejects any
locked member atomically. Freeze frame never modifies linked audio. Reverse maps
emit `StrictPredecessor`; forward maps emit `Covering`. The target remains intact
through prepared visual/audio schedules and is consumed exactly once at the
physical evaluation grid: video uses `ceil(target)-1` for a strict predecessor,
and audio uses the equivalent exact integer-sample lowering. A hold captured
from reverse content retains `StrictPredecessor`, including after save/reopen.

## Closed content variants

Exactly one variant is present:

- `Media { asset_id, interpretation }`
- `AdjustmentLayer { asset_id }`
- `NestedSequence { sequence_id }`
- `SolidColor { asset_id, color }`
- `BasicTitle { title }`

There is no parallel kind/asset/nested/color/title payload. Unknown or legacy
parallel fields fail current-schema deserialization.

Basic Title is Sequence-local generated content, not an Asset and not an
Effect. Its canonical Property Bag owns text, concrete requested font intent,
size, working-linear fill, tracking, line height, and alignment. Animatable
properties evaluate in the shared Clip-local visual author time.
Missing/extra/schema-divergent
properties make the author snapshot invalid.

## Built-in and effect properties

Built-in Transform, opacity, source-time mapping, Solid Color, and Basic Title
state are not removable. UI may reset or hide them but cannot present them as
deletable effect instances. Visual effects remain ordered instances with their
own stable `EffectId` and definition-backed Property Bags.

## Link semantics

`ClipLinkGroupId` is Sequence-local set membership for two or more placements.
It synchronizes ordinary selection and structural edits; it does not imply
shared effects, Transform, masks, processing state, or media ownership.
Singleton groups are invalid. Structural commands either preserve the complete
group promise or remove membership from fragments for which it no longer
holds.

## Identity copying

Copy, paste, razor-created fragments, overwrite fragments, precompose, and
Sequence duplication fork placement-local Clip, Effect, Mask, animation, and
audio identities. Asset and nested Sequence identities remain external
references. A copied Basic Title retains values and curves while receiving
independent animation/keyframe identities.
