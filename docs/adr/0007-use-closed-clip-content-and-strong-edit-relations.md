---
status: accepted
---

# Use closed Clip content and Sequence-owned strong edit relationships

## Decision

Timeline placement is the single source of truth. A `Track` owns ordered Clip
placements; a Clip owns exact placement and source-time mapping plus one closed
`ClipContent` variant: Media, Adjustment Layer, Nested Sequence, Solid Color,
or Basic Title. Variant-specific references, generated-source parameters, and
interpretation data exist only inside that payload. Current-schema
deserialization rejects legacy/unknown parallel fields.

Basic Title is Sequence-local generated Clip content. It is neither a fake
Asset nor an Effect: it produces straight-alpha working-linear picture, after
which the ordinary Clip Transform, effect chain, Mask, blend, Transition,
nesting, Preview, and Export semantics apply unchanged. Its definition-backed
Property Bag is a closed author contract evaluated in the same Clip-local
visual author time as Transform, Opacity, Effects, and Masks.
The requested system font family is a concrete recoverable dependency; missing
families, inaccessible face bytes, and undeclared shaping fallback fail
execution closed instead of substituting pixels. The resolved face-byte/index
fingerprint participates in generated output identity.

Relationships that are not intrinsic Clip content are explicit Sequence-local
author entities:

- synchronized editorial selection/editing is a multi-member
  `ClipLinkGroupId` set, not a pair pointer;
- visual Transition is a Sequence-owned two-input entity with strong Clip
  endpoints, exact Sequence-time range, stable identity, type/definition,
  properties, and enabled state;
- audio Components, Scopes, Transitions, routing nodes, and routes remain in the
  Sequence-owned Audio Program defined by ADR 0002.

A visual Transition does not persist `track_id`. Both Track membership and cut
geometry are derived from its endpoint placements. Validation requires two
distinct adjacent Clips on one video Track, one exact shared cut, a non-empty
range covering that cut within the placement union, valid properties, and a
unique endpoint pair. `source_demand` maps the entire Transition interval into
both source domains without clamping; media/nested Adapters provide authoritative
source extents and insufficient handles fail closed.

Every structural command restores link, visual Transition, and audio graph
invariants before the candidate commits. A destructive edit may remove a
relationship whose promise no longer holds; it may not leave a dangling or
misleading reference. Copy, razor, paste, overwrite fragments, precompose, and
Sequence duplication fork all addressable placement/processing/automation
identities and remap internal strong references. Asset Library record
identities are Project-contained strong references. Only their concrete
media/provider bindings are recoverable dependencies: an unavailable binding
retains the Asset record and expected contract for diagnosis and relink. Nested
Sequence IDs cross the duplicated aggregate boundary but must resolve inside
the owning Project.

Removing an Asset from the ordinary Library view retires only its Library
membership. One SQLite transaction preflights the complete selected Asset and
folder batch, retains every Asset row and binding, and hides retired rows from
ordinary listings. It does not edit any Sequence, proxy-mode membership, or
History endpoint. Retirement is not entered into Sequence/Project History:
Undo/Redo continues with the preceding Author Transaction and leaves membership
retired. Reimporting the same concrete path is the current restoration operation
and preserves the same `AssetId`. No record-level physical-purge Interface
exists until a future Project+History+SQLite authority can prove that identity
unreachable; a genuinely missing row is invalid strong-reference state, not
offline media.

Mask scalar Property Bags are persisted with each Mask. Project validation
requires canonical Parameter IDs, valid curves, finite shape geometry, and
strict shape-key order. Runtime defaults are not a substitute for missing
author data.

## Consequences

- Contradictory Clip kinds or fake nested-Clip Asset identities are
  unrepresentable.
- Basic Title cannot drift into a renderer-only `TextLayer`, Asset generator,
  or effect-specific parameter model; it crosses the existing generated-source
  seam and then shares all downstream visual semantics.
- Link groups naturally support more than one video/audio pair and preserve
  relative placement under group edits.
- A Transition cannot disagree with its endpoint Track or silently read beyond
  source handles.
- Library removal cannot produce dangling inactive/nested Sequence references,
  make Undo restore a missing row, partially apply a multi-selection, or erase
  Project proxy intent.
- Cross Dissolve admission requires shared Preview/Export projection and
  execution, exact source-handle diagnostics, strong endpoint persistence, and
  atomic Undo/Redo; insufficient handles or an unavailable endpoint fail
  closed.
- Free-form routing remains in typed audio/visual execution projections; the
  ordinary user author model stays Track-based and does not expose a universal
  untyped DAG.
