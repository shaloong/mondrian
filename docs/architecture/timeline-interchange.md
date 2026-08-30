# Timeline Interchange

`mondrian-interchange` is the app-independent Module for qualified editorial
timeline interchange. It reads and writes declared subsets of OpenTimelineIO,
CMX 3600, Final Cut Pro 7 XML, and AAF without creating a second public
Timeline model. `mondrian-timeline::Sequence` remains the only authoring
authority.

## Ownership and Flow

Each native format is a private Adapter. Every Adapter lowers into one bounded,
exact, crate-private `InterchangeTimeline`; only the common materializer may
construct a detached `Sequence`:

```text
native bytes -> format Adapter -> exact private model -> inspection report
                                                    -> Asset bindings
                                                    -> detached Sequence
                                                    -> one App Project transaction

canonical Sequence + immutable Asset snapshots -> exact private model
                                                -> preservation analysis
                                                -> format Adapter -> bytes + report
```

Import inspection never mutates Project or Asset state. Foreign locators are
presentation hints only. Before materialization, the product must bind every
foreign `InterchangeMediaKey` to an existing strong `AssetId`; missing,
duplicate, or conflicting bindings fail closed. The App verifies those records
against the canonical Asset Library and appends the candidate through exactly
one reversible Project snapshot transaction. That commit does not silently
change open-Session Sequence navigation.

Export takes an immutable canonical Sequence and immutable Asset Library
snapshots. It returns a `PreparedInterchangeArtifact` that keeps bytes and its
conformance report coupled. Filesystem selection and durable publication are
product/storage Adapter responsibilities, not format semantics.

## Qualified Profiles

| Profile | Declared subset | Color behavior |
|---|---|---|
| `otio_json_v1` | OTIO native JSON, pinned 0.18.1-compatible core schema map | Mondrian metadata extension preserves explicit input color identity and an exact static `ASC CDL -> LUT3D` Clip Grade |
| `cmx3600` | strict A-mode picture conform, one enabled video Track, explicit caller-owned rate, at most 999 events, 8-character reel identities, cuts/dissolves | color and grade are omitted only with explicit loss findings |
| `fcp7_xml_v5` | `xmeml version=5` Sequence subset, external media, supported exact rates, cuts/dissolves | color and grade are omitted only with explicit loss findings |
| `aaf_edit_protocol_v1` | metadata-only external-media Edit Protocol through a qualified isolated helper | no generic color extension; omissions are reported |

The OTIO grade extension is deliberately narrow. It accepts one active linear
Grade Graph containing an optional static ASC CDL followed by an optional
static LUT3D. Automation, parallel/layer graphs, unsupported nodes, and extra
versions are never silently reinterpreted. A LUT reference must carry its URI,
processing space, finite intensity, and a 64-hex SHA-256 content binding; a
missing digest or processing identity is a publication blocker. Imported
extensions create Sequence-owned Grade Definitions and registered Effect
nodes, not an interchange-only grade model.

## Source Identity and Exact Time

`MediaInterpretation::editorial_source` persists optional reel name, exact
source SMPTE reference, and an external item identity. These are Clip/media
interpretation facts and do not replace Asset Library identity. Display
timecode parsing and formatting share the exact Core
`SmpteDisplayTimecodeContract`; drop-frame skipped labels, unsupported rates,
signs, overflow, and frame-grid misalignment fail closed. CMX cannot infer a
trustworthy frame rate and therefore requires one explicitly.

## Conformance and Loss Evidence

Every successful inspection or preparation returns report schema v1 with:

- pinned format profile and direction;
- SHA-256 source, semantic, and capability fingerprints;
- stable owner-addressed finding codes;
- severity and exact disposition (`preserved`, `normalized`,
  `represented_by_extension`, `omitted`, and related closed values);
- stable aggregate counts and one overall outcome.

Blockers always reject. `RejectUnpreserved` also rejects every approximation,
flatten, omission, or relink requirement. `AllowWithReport` admits only
non-blocking losses and returns the complete evidence for explicit product
approval. It can never override malformed input, missing strong bindings, or a
blocker. Policy/blocker rejection carries the complete report inside the typed
error, so a fail-closed product path can still present and serialize the exact
reason instead of reducing it to an error string.

## Untrusted Input and AAF Isolation

All profiles enforce caller-supplied nonzero limits for bytes, tracks, items,
metadata strings, and structural nesting. OTIO JSON and FCP XML have explicit
depth bounds. Runtime XML rejects DTD and entity declarations and never performs
network or external-entity resolution.

AAF binary interpretation is not linked into the Mondrian process. A release
Adapter must first qualify an adjacent helper by exact implementation, helper
version, bridge-contract version, and underlying engine identity. Each
operation uses bounded temporary files, bounded stderr/output, a wall-clock
deadline, no shell, and process failure containment. Import and helper output
must be Compound File Binary artifacts with the AAF magic; the helper exchanges
only the versioned exact JSON bridge with this Module. The helper seam is not
evidence that a platform has a qualified pyaaf2 build.

## Qualification Boundary

Native fixture round trips, failure cases, and App transaction tests qualify
the declared Mondrian subsets. Actual Resolve, Premiere, Pro Tools, and other
application round trips, reference-frame comparison, version matrices, and
vendor-specific extension qualification require separate evidence. The
Renderer-owned Cross-Application Color Qualification can qualify exact
file-output frames from Blender, Resolve, and Premiere, but it does not prove
that OTIO/AAF/XML/EDL structures or vendor extensions round-trip. Conversely,
interchange conformance cannot substitute for pixel qualification. Until each
evidence class exists, reports must not claim generic application compatibility
beyond the declared profiles above.
