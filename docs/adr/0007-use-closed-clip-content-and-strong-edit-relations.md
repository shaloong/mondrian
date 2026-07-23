---
status: accepted
---

# Use closed Clip content and Sequence-owned strong edit relationships

## Decision

Timeline placement is the single source of truth. A `Track` owns ordered Clip
placements; a Clip owns exact placement/source ranges and one closed
`ClipContent` variant: Media, Adjustment Layer, Nested Sequence, Solid Color,
or Basic Title. Variant-specific references, generated-source parameters, and
interpretation data exist only inside that payload. Current-schema
deserialization rejects legacy/unknown parallel fields.

Basic Title is Sequence-local generated Clip content. It is neither a fake
Asset nor an Effect: it produces straight-alpha working-linear picture, after
which the ordinary Clip Transform, effect chain, Mask, blend, Transition,
nesting, Preview, and Export semantics apply unchanged. Its definition-backed
Property Bag is a closed author contract evaluated in Clip source-local time.
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
identities and remap internal strong references. Asset and nested Sequence IDs
remain external references.

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
- The typed Transition author foundation does not claim renderer support.
  Cross Dissolve becomes supported only when shared Preview/Export projection,
  backend execution, source-handle diagnostics, persistence/Undo, and Golden
  Project evidence all pass.
- Free-form routing remains in typed audio/visual execution projections; the
  ordinary user author model stays Track-based and does not expose a universal
  untyped DAG.
