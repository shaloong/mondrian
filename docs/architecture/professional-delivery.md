# Professional Delivery

The bounded Export queue also exposes a fixed-size long-duration snapshot with
cumulative activity, rendered-frame, publication, failure, and active-job
facts. Its explicit bounded shutdown terminalizes pending jobs, cancels
reversible running work, waits for worker-loop return, and reports final queue
closure. Shutdown is permanent admission state: later enqueue requests are
rejected instead of creating work after the worker returned. Snapshot
rejections, cancellations, durable publications, and independent verifications
remain separate counters. Durable publication remains distinct from the
independent artifact re-open/content verification required by commercial
endurance qualification; see
[Commercial Endurance Qualification](commercial-endurance-qualification.md).

`mondrian-export` also owns an independent single-file artifact verifier for
that post-publication boundary. It accepts only a direct regular file within an
explicit byte limit, hashes the encoded bytes, derives the ordinary typed
container/stream probe, and launches a separately supervised FFmpeg process
against a verifier-owned immutable byte snapshot. It decodes every advertised
video and audio stream through EOF. The child
has an absolute deadline and strict stdout/stderr byte ceilings. Success
requires a terminal progress record, a nonzero video-frame count when video is
present, decoded duration evidence for a positive-duration artifact, and one
FFmpeg SHA-256 over the decoded stream bytes. Metadata and encoded-byte SHA-256
are rechecked against the published path after decode so a changing artifact
cannot produce a receipt. Bounded snapshot copying reads at most one byte past
the policy ceiling before rejecting a growing or oversized file.

The result is a structured report plus the SHA-256 of its canonical JSON. The
receipt fields are private and bind the caller's stable Export job/artifact
identity, so App capture can construct
`ExportArtifactVerified` only from completed verifier output rather than from
caller-supplied strings. This proves bounded re-open, complete decodability,
and stable content identity; it is not a pixel comparison against the source
Timeline or an independent colorimetric oracle. Audio is covered by the
combined decoded-stream hash and terminal duration rather than by a separately
claimed sample-count qualification. The decoded terminal duration must agree
with the independently probed duration under the validation tolerance. Direct
symlinks and other non-regular
objects are rejected; the admitted output path remains responsible for its
already-canonical parent namespace.

`mondrian-export::professional_delivery` is the deep Module for constrained
IMF, AS-11, and DCP delivery. It deliberately exposes exact qualified product
rows rather than claiming the complete standards families:

| Product row | Picture | Audio | Artifact |
|---|---|---|---|
| IMF Application ProRes RDD 45, 1080p25 Rec.709 | ProRes 422 HQ, 10-bit 4:2:2 | stereo PCM24/48 kHz | immutable IMP directory |
| AMWA AS-11 X9 NABA HD, 720p59.94 Rec.709 | AVC High 4:2:2 Intra, 10-bit | stereo PCM24/48 kHz | one OP1a MXF |
| SMPTE DCP 2D 2K Flat, 24 fps | ST 428-1 XYZ12 JPEG 2000 | stereo PCM24/48 kHz | unencrypted, unsigned immutable DCP directory |

An enum variant is a closed conformance row, not a friendly alias. Admission
requires exact raster, rational cadence, progressive field order, sample
representation, range, chroma, renderer color endpoint, stereo Program Output,
48 kHz audio, and bounded non-empty metadata. A near match fails before
rendering. DCP consumes display-linear Rec.709 from the shared Renderer and the
Adapter alone applies the ST 428-1 transfer and matrix into MSB-aligned XYZ12;
no DCP conversion is hidden in Timeline or the Viewer.

This progressive restriction remains intentional for the current IMF, AS-11,
and DCP catalog; generic interlaced media-file delivery does not widen those
package rows. The separate qualified interlaced matrix is MOV-only: 1920x1080,
25 or 30000/1001 encoded pictures per second, TFF, square-pixel Rec.709 Legal,
10-bit 4:2:2, no alpha, and ProRes 422 LT/422/HQ or uncompressed v210 software
encoding. HEVC/AV1/H.264, DNxHR, AVC-Intra, image/float masters, resident GPU
encoding, Smart Render, BFF output, PsF, telecine, and mixed dominance remain
fail-closed.

## Execution and validation

The author `ExportPreset` is frozen in the ordinary `TimelineExportSnapshot`.
The queue resolves it once, renders picture and PCM through the existing
Preview/Export visual and audio semantics, then enters explicit monotonic
phases:

```text
Preparing -> Rendering -> Encoding -> Packaging -> Validating -> Publishing
```

`Packaging` is a cooperative cancellation boundary. `Publishing` remains the
irreversible namespace boundary. A file deliverable uses
`OwnedPublicationFile`; IMF and DCP use `OwnedPublicationDirectory`. External
writers receive only an identity-bound sibling reservation. The final file or
directory does not exist until validation succeeds and Storage durably
publishes the exact staging object.

The Toolchain Adapter resolves a profile-specific closure and fails closed if
any required executable is missing or cannot return bounded identity evidence:

- IMF: BBC BMX `raw2bmx`/`mxf2raw`, a private Java runtime, and Netflix Photon;
- AS-11 X9: BBC BMX `raw2bmx`/`mxf2raw`;
- DCP: CineCert `asdcp-wrap`/`asdcp-info` and the DCP-o-matic/libdcp package
  verifier.

Tools are searched beside the application and then on `PATH`. Photon is a
private runtime closure under `professional-delivery/photon/{bin,lib}`; the
development-only `MONDRIAN_PHOTON_JAVA` and `MONDRIAN_PHOTON_LIB` overrides
must identify direct files/directories. A release must ship the qualified
versions, transitive runtime libraries, licenses, and checksums as one tested
deployment unit. Merely finding a similarly named executable is not package
qualification; every job probes the selected identities before rendering.

IMF picture and audio Track Files are wrapped separately by BMX and reimported
by BMX. Photon then validates each actual Track File and supplies its bounded
RegXML Essence Descriptor. Those descriptors, actual TrackFile UUIDs, exact
native edit rates, intrinsic/source durations, and SHA-1 digests are the only
inputs admitted to CPL construction. Placeholder descriptors are forbidden.
After the IMP graph is built, Photon independently reimports the complete
directory.

AS-11 X9 is one OP1a file carrying the X9 Specification Identification UL.
BMX reimport must prove OP1a, 60000/1001, AVC High 4:2:2 Intra, 10-bit 4:2:2,
PCM24/48 kHz stereo, MCA labels, complete partitions, and the final sample.
X9 does not invent a proprietary `X9Framework`; optional descriptive XML is a
separate future product row.

DCP picture and sound Track Files are independently wrapped and reimported by
CineCert. The package builder emits SMPTE CPL/PKL/AssetMap documents, then the
DCP-o-matic/libdcp verifier checks the complete directory, assets, hashes, XML,
JPEG 2000, and MXF. The current row targets baseline SMPTE DCP, not the stricter
SMPTE Bv2.1 profile: Bv2.1 findings are retained as a distinct verifier class,
while any ordinary interoperability `Error` fails the export.

## Package graph

`ProfessionalPackageBuildRequest` freezes issue time and all strong document,
composition, resource, virtual-track, descriptor, and Track File identities.
Random identity allocation never occurs while serializing an individual XML
reference. IMF uses a 2067-3 Segment/Sequence/TrackFileResource graph and RDD
45 Application Identification. DCP uses a 429-7 Reel/MainPicture/MainSound
graph. Both include one CPL, one PKL, one AssetMap, one picture Track File, and
one primary audio Track File.

The independent internal reimport is not an XML well-formedness check. It
rejects DTD/entity declarations, files above 2 MiB, symlinks, nested or
case-aliasing paths, duplicate IDs, extra filesystem objects, multi-chunk or
non-zero-offset AssetMap objects, non-positive sizes/durations, wrong media
types, unsupported hash algorithms, malformed SHA-1 values, broken
AssetMap/PKL/CPL closure, CPL/PKL hash disagreement, wrong profile signaling,
or picture/audio duration disagreement. IMF `SourceEncoding` references must
close over exactly the included Essence Descriptors. Every declared size,
path, and hash is recomputed from the staged files before publication.

## Product and performance boundary

The App Export panel edits the same `ExportPreset`: constrained codec/raster/
cadence controls are read-only, while title, issuer, creator, and RFC 5646
language remain editable. Availability resolves the complete request against
the selected Sequence; a mismatched DCP or AS-11 request is disabled before
dispatch, and queue admission repeats the same validation. File and directory
suffixes are `.mxf`, `.imf`, and `.dcp` respectively.

The Module has no second renderer, audio mixer, Timeline walker, or worker
pool. It reuses the frozen visual/audio Programs and the bounded Export worker.
External processes have bounded output capture, a six-hour attempt deadline,
cooperative cancellation, and profile-local invocation. XML inventory is
bounded to 32 assets and two MiB per document. Directory publication performs
one durable tree synchronization after complete validation; it does not copy
the package into the final namespace progressively.

Real qualification tests generate essence with the production FFmpeg
contracts, wrap actual MXF Track Files, reimport them through the independent
readers, and validate the complete IMF/DCP package through Photon or
DCP-o-matic. These ignored tests are deployment gates because their tools are
licensed runtime artifacts rather than Rust test dependencies; ordinary unit
tests retain deterministic negative coverage for graph, timing, digest, path,
and XML attacks.

## Broadcast QC publication gate

An optional immutable `BroadcastQcProfile` is frozen with the ordinary Export
snapshot. Export observes each real post-Legalizer, output-quantized delivery
picture before handing it to the encoder and streams it through the shared
Broadcast Module. `Fail` or `Incomplete` is terminal before durable publication;
`Warn` publishes with the versioned report in job diagnostics. A requested scan
disables Smart Render, byte-preserving Dynamic HDR, and GPU-resident encoding
routes that cannot expose the exact observation tap.

This gate proves the in-process delivery-picture sequence, not the final
encoded/muxed artifact. Profiles may retain an explicit independent artifact
revalidation obligation, just as regulatory flash analysis remains an external
obligation. AS-11 ST 436 carriage and caption semantics are not inferred from
the existing AS-11 picture/audio package. See
[Broadcast QC And Ancillary Data](broadcast-qc-and-ancillary.md).

## Dynamic HDR delivery qualification

Dynamic HDR is a separate delivery Module from the fixed IMF/AS-11/DCP rows.
Its immutable queue contract resolves one of three Sequence-authored paths:

- `Omit` renders ordinary output and deliberately publishes no dynamic
  metadata;
- `PreserveSourceExact` requires a video-only complete-source identity, no
  authored static-metadata rewrite, no Legalizer, and copies the whole file
  byte-for-byte. Source/output SHA-256 equality and output metadata re-probe are
  mandatory evidence. Any failure blocks publication and cannot select render;
- `Remake` requires progressive Rec.2100 PQ Legal HEVC Main10 10-bit 4:2:0 plus
  authored ST 2086/MaxCLL/MaxFALL for the first qualified row, and also requires
  a licensed/adopter-qualified generator, independent validator, and human
  HDR/SDR QC evidence. No such runtime Adapter is currently installed, so this
  path fails closed before execution.

ST 2094-40 Application #4 syntax is named as such in product state and evidence;
Mondrian does not turn detection into an HDR10+ certification claim. Dolby
Vision CM version, metadata levels, bitstream profile/level, licensed tooling,
and delivery profile are retained as distinct qualifications. Open syntax tools
or FFmpeg/x265 parameter availability alone do not establish either branded
workflow.
