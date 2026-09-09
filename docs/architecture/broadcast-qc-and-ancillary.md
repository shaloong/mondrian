# Broadcast QC And Ancillary Data

`mondrian-broadcast` is the deep, platform-neutral Module for broadcaster-profile
picture analysis and registered ancillary-packet construction. It depends only
on `mondrian-core`; Export and Reference Output consume its public Interface.
No caller may maintain a second interpretation of QC thresholds, ST 291 parity,
checksum, packet identity, or inventory ordering.

```text
frozen BroadcastQcProfile             registered ancillary value
           |                                      |
           v                                      v
 post-Legalizer delivery picture       ST 291 Type 2 packet + placement
           |                                      |
           v                                      v
 bounded streaming QC Session          canonical AncillaryFrame inventory
           |                                      |
           v                                      v
 immutable evidence report             Export / Reference Output Adapter
```

## QC profile and execution

A `BroadcastQcProfile` freezes its stable id, edition, source-document SHA-256,
encoded signal identity, exact observation tap, active-picture rectangle,
ordered rules, threshold/severity values, external obligations, and retained-
finding bound. The deterministic fingerprint enters every report. Unknown or
contradictory fields fail closed; broadcaster-specific limits are data, never
hard-coded claims about a generic standard.

The current rules detect sustained black, sustained unchanged pictures,
adjacent-frame mean-luma transition candidates, and RGB signal excursions.
Black/freeze thresholds and duration are profile-specific. `LumaFlashCandidate`
is triage evidence only: it is not an implementation or certification of
ITU-R BT.1702 or a broadcaster-approved PSE tool. Profiles may therefore retain
`RegulatoryPhotosensitiveFlash` as `NotTested`.

The Session accepts contiguous absolute frame coordinates and encoded Float32
RGBA at one declared tap. It evaluates only the explicit active picture, keeps
one prior frame plus bounded findings, coalesces deterministic segments, and
uses O(frame pixels + retained findings) memory. Non-finite pixels, frame gaps,
empty scans, or finding overflow produce `Incomplete`, never a pass. A fatal
finding produces `Fail`; warnings or unresolved obligations produce `Warn`.

Export currently supplies `DeliveryPictureAfterLegalizer`: the real Program
Output after explicit Legalizer and output quantization readback, before the
encoder consumes it. `ProgramSignalBeforeLegalizer` is rejected until Export
can prove that exact tap. `Fail` and `Incomplete` block durable publication;
`Warn` publishes with its immutable report. Smart Render, byte-preserving
Dynamic HDR, and resident encode routes cannot bypass a requested observation.

This scan is not an independent decode of the finished encoded/muxed artifact.
A profile that requires that proof sets
`require_encoded_artifact_revalidation`; its report retains an
`EncodedArtifactRevalidation/NotTested` obligation. Ordinary container probing
and decode validation remain separate Export evidence and must not be presented
as a content re-scan.

## ST 291 packet and frame inventory

`St291Type2Packet` owns the component ancillary data flag `000h 3FFh 3FFh`,
Type 2 DID/SDID constraints, eight-bit data-count limit, per-word parity and
inverse parity, protected code rejection, and checksum. Decode verifies every
structural field. Payload length is bounded to 255 user-data words.

`AncillaryPacket` adds an explicit VANC/HANC, field, line, and horizontal-offset
placement plus validation level. `AncillaryFrame` is canonical, collision-safe,
and bounded to 64 packets and 16,384 encoded words. Its frame identity and
SHA-256 inventory digest let scheduling and future file-carriage Adapters prove
which exact packets they consumed.

Registered constructors currently cover:

- ST 12-2 ATC (`DID/SDID 60h/60h`, 16 user words) from an explicit Core
  `SmpteTimecodeReference`; Viewer/display timecode is never carrier authority.
- ST 2016-3 AFD and bar data (`41h/05h`, eight user words).
- ST 334-2 Caption Distribution Packet transport (`61h/01h`) after CDP magic,
  declared length, frame-rate code, and modulo-256 checksum validation.

CDP support is deliberately transport-only. It does not author subtitle Tracks,
convert text to CEA-608/708, validate service semantics, or prove receiver
interoperability. The dormant `TrackType::Subtitle` vocabulary is not used as
evidence because Timeline does not yet own a caption authoring model.

## Integration and qualification

Reference Output schedules video, the exact 48 kHz Audio Program interval, and
one `AncillaryFrame` as an atomic bundle. Mode admission explicitly declares
ancillary support/readback. Nonempty ancillary is rejected when disabled, and
required readback cannot be inferred from ordinary packet scheduling. The
completion callback must return the actual inventory digest and the Module
fails on omission or mismatch. The simulated Adapter proves Module ordering
and accounting only.

Export freezes the profile in `TimelineExportSnapshot`; the App external
`ExportProductAction` preserves the complete tagged profile, and terminal job
diagnostics expose profile version, verdict, analyzed range, findings, overflow,
and obligations. The local UI does not invent a broadcaster default profile;
deployment/profile management must supply an approved versioned contract.

Commercial qualification still requires real DeckLink/AJA line/field capture,
CEA-608/708 authoring and receiver tests, ST 436 MXF carriage/reimport, an
approved current photosensitive-flash corpus/tool, independent final-artifact
content re-scan where required, and broadcaster conformance fixtures. These are
HITL facts, not consequences of passing software unit tests.

Normative and qualification references:

- [SMPTE ST 291-1 ancillary packet format](https://pub.smpte.org/latest/st291-1/st0291-1-2011.pdf)
- [SMPTE ST 12-2 ancillary time code](https://pub.smpte.org/pub/st12-2/st0012-2-2014.pdf)
- [SMPTE ST 334-1 VANC caption mapping](https://pub.smpte.org/latest/st334-1/st0334-1-2015.pdf)
- [SMPTE ST 334-2 Caption Distribution Packet](https://pub.smpte.org/latest/st334-2/st0334-2-2015.pdf)
- [SMPTE ST 2016-3 AFD and bar data](https://pub.smpte.org/latest/st2016-3/st2016-3-2009.pdf)
- [SMPTE ST 436-1 MXF VBI/ANC mapping](https://pub.smpte.org/latest/st436-1/st0436-1-2013.pdf)
- [EBU QC Items](https://qc.ebu.io/) and [EBU R 103](https://tech.ebu.ch/publications/r103)
- [ITU-R BT.1702-3](https://www.itu.int/rec/R-REC-BT.1702-3-202311-I/en)

### Finished encoded artifact stream

`BroadcastArtifactQcSession` accepts independently decoded GBR Float32
little-endian bytes under an exact frozen frame count and caller raster bound.
It assembles arbitrary pipe fragments with one-frame storage, rejects the first
byte beyond the frame inventory, and seals incomplete scans on truncation,
decode/identity failure, or nonfinite content. The receipt retains encoded
object SHA-256/size, ordered decoded-byte SHA-256, and the original frozen
profile's content report. Only a complete scan resolves encoded-artifact
revalidation. Regulatory PSE obligations remain independent and unresolved.
The decoder uses FFmpeg's documented `gbrpf32le` format and passthrough frame
cadence; no color view or frame duplication may enter this observation.

`wire_qualification` owns explicit validation-only ST291 nonce/frame marker
construction and independent capture reconstruction. Captured packet structure,
placement, inventory and on-wire frame/campaign identity must match before
canonical provenance is joined to actual decoded words. Missing, duplicate,
shifted, corrupted, stale-campaign or extra packets fail closed. This owner is
shared by the validation Timeline pump and the physical AJA receiver adapter;
ordinary playback does not inject qualification packets.
### ST436 exact carriage and final MXF reimport

`FrozenAncillaryProgram` owns one selected Timeline source start, resolved
output frame rate, bounded duration and sparse canonical `AncillaryFrame`
inventory. Export admission binds all three coordinates; absent packets mean
an explicit empty ANC element for that frame, never an omitted frame or retime.

The Broadcast ST436 codec follows SMPTE 436M-2006 sections 4.4.4 and 6: luma
10-bit ANC words (including the actual checksum) are packed into the high 30
bits of each big-endian UInt32. The strict decoder also accepts standard 8-bit
luma ANC, regenerating the parity/checksum excluded by that coding. It rejects
unsupported wrapping/coding, malformed arrays, padding, checksum/parity,
truncation and trailing data. ST436 does not represent arbitrary horizontal
word offsets, so this exact carriage admits offset zero only. It does not
silently discard or invent the position of an SDI packet.

AS-11 uses the same owner to generate standard KLV input for BMX `--klv s
--anc`. After wrapping, a separate streaming reader scans the actual final MXF
ANC essence elements, checks every packet word and placement, exact frame
count, and single ANC track, then restores canonical provenance only after
the observed data matches. Missing, extra, altered and truncated elements
block publication. The pure codec tests and native AJA raster tests establish
software boundaries; only actual tool execution and physical receiver records
can qualify a delivery or wire run.

## External regulatory PSE ownership and approval (2026-09-06)

`mondrian-broadcast::regulatory_pse` owns the strict external approval and result
contract. The initially supported frozen edition is ITU-R BT.1702-3 (11/2023).
This is an external qualification boundary: Mondrian does not approve itself,
ship an approved default, or promote `LumaFlashCandidate` into regulatory PSE.
The commissioning organization must independently establish the authenticity and
scope of `approval_authority`, `approval_id`, and the original approval-document
SHA-256. Provider id/version, approved executable SHA-256, native profile SHA-256,
and the delivery QC profile fingerprint are explicit immutable trust anchors.
Synthetic tests use conspicuous `SYNTHETIC TEST ONLY` authority names; their
success proves protocol/ownership behavior and never hardware or PSE compliance.

`mondrian-export::regulatory_pse` admits this third-party installation before any
encoding. Windows retains read-only, deny-write/deny-delete native handles for
all three files until the job consumes the prepared provider. Missing provider,
approval, native profile, or native identity adapter is typed `NotRun`; changed
hashes or malformed configuration are failures. Other OS adapters deliberately
remain NotRun until their equivalent physical identity qualification is supplied.
`RenderJob` owns the prepared provider while queued, executing, canceled or being
dropped. A rejected enqueue has no native process to close. Export also checks
that the QC raster, encoded color and observation tap match the frozen delivery.

The only native invocation is the pinned executable with
`--mondrian-regulatory-pse-v1`. It receives one strict `RegulatoryPseRequest` JSON
on stdin and must return one strict `RegulatoryPseOutput` JSON on stdout (2 MiB
maximum); duplicate/unknown fields and trailing objects fail. The request has a
new random nonce, the entire approval, immutable final-file snapshot path,
provider-native profile path, and the same-run final-artifact QC report. The
output echoes these bindings and reports exact contiguous full-frame coverage,
Pass/Fail/Incomplete, plus actual native vendor-report bytes and their SHA-256.
An installed vendor protocol adapter is responsible for calling its approved
analyzer and preserving its original report; arbitrary stdout assertions are
not a substitute for an independently approved installation.

Artifact hashing, bounded snapshot copying, native stdin/stdout/stderr workers,
post-analysis identity verification and consuming cleanup share the original
monotonic deadline and cancellation token. The shared Media supervisor retains
original native cleanup, including failure to reap or join. The receipt retains
request, bounded original output, parsed response, exit code, stderr truncation,
typed failure, original cleanup and independent snapshot-disposal failure. It
never reconstructs a clean owner from a zero exit status. Only clean execution
can resolve an obligation; Failed and Incomplete PSE results block publication.
The pure resolver requires equality with the original artifact scan and its
digest, so another invocation/artifact/profile cannot discharge this obligation.
It returns derived QC evidence while preserving the original artifact receipt.

App exposes explicit import/clear of a bounded JSON configuration containing
`provider` and `qc_profile`; import freezes configuration, not qualification.
Typed export requests and phase-scoped repeated exports carry the same provider.
Commercial campaign admission reads the machine-plan-bound QC object and holds
its lease plus the prepared provider before any phase factory opens product
owners. Missing required external PSE produces the explicit
`RegulatoryPseProviderPrepared` missing capability and NotRun before phase start;
these preparation leases live through run-owner close. Each export attempt also
retains its own admitted provider until its actual final-artifact invocation.

Focused tests cover changed invocation/digest/provider/profile/coverage,
integer-overflow coverage, absent approval, unknown/duplicate/trailing protocol,
empty/oversized input, typed cancellation/deadline, Windows replacement denial,
real native invalid-protocol cleanup, and rejection before a queue job exists.
They do not claim regulatory qualification without a commissioned third-party
provider, approval document, approved profile and representative physical fixture.
The `queue::final_broadcast_qc` gate is shared by the generic media-file path
and the reclaimed final AS-11 MXF, after independent ST436 readback and before
publication. It preserves earlier render/audio diagnostics, decoded final-file
QC, native PSE evidence and the derived final QC separately. One deadline covers
the final scan and PSE invocation. AS-11 expectations come from the resolved
professional contract: 1280x720 at 60000/1001, ten-bit High 4:2:2 Intra level 4.1,
Rec.709 legal-range picture and stereo 48 kHz PCM24, exact duration and all-intra
progressive decoded coverage. Its picture encoder now uses the shared signal
conversion/tag arguments before producing the separately wrapped essence.
IMF/DCP directories, image sequences and audio-stem packages reject an attached
Broadcast QC profile at admission until their own final-artifact scan is
implemented; no attachment can silently bypass a missing publication gate.
### Broadcast caption import, version 1 (COL-044)

`captions` is the only SCC/CDP import authority. App resolves the selected output
through Export's `resolve_ancillary_export_selection`, reads a bounded file, and
freezes the resulting `FrozenAncillaryProgram`. No timeline edit or second caption
authoring model is introduced. Both inputs reach the existing canonical packet,
ST436 encoder, native BMX wrapper and independent final MXF word-by-word rescan.

The implemented source rows are explicit:

* Scenarist SCC literal `Scenarist_SCC V1.0`, UTF-8/ASCII with optional BOM, Field 1
  CC1/CC2 at 30000/1001, one parity-bearing pair per input frame, mapped exactly to
  60000/1001 output. The selected Timeline origin and sequence timecode origin
  are subtracted with rational arithmetic. Drop/non-drop labels use Core's sole
  SMPTE parser. Skipped drop labels, mixed counting modes, overlapping/reordered
  rows, data before the selection, off-grid words and out-of-range data reject.
  SCC is re-carried as Derived CDP; transmitted repeated controls remain intact.
* Raw `.cdp` contains concatenated complete data-only ST334-2:2015 CDPs, exactly one
  per selected output frame. No proprietary MCC expansion is guessed. Optional
  CDP timecode/service-info sections, unknown versions and unsupported rates fail
  closed. Transport supports 25/29.97/30/50/59.94/60; App's implemented AS-11 row
  selects 59.94. The 23.976/24 variable 608 allocation is explicitly unsupported.

The CDP checker validates complete section extents, fixed cc_count by rate, marker
and reserved bits, 608-before-708 ordering, high-rate field alternation, header/
footer identity, checksum and wrapping 16-bit sequence. Valid 608 pairs require
odd parity, explicit channel selection and supported caption-mode initialization.
It distinguishes CC1..CC4, PAC rows/indentation, pop-on/paint-on/roll-up controls,
repeated controls, basic/recognized special/extended characters, replacement
characters, tabs and the 15x32 row boundary. XDS, text services, alarm/reserved
commands and unsupported state transitions reject. It does not decode display
memory into rendered text, normalize characters, or qualify typography.

708 uses bounded 2..128-byte DTVCC packet assembly, modulo-four sequence continuity,
explicit start/continuation types, complete service-block boundaries, standard and
extended services 1..63, legal null padding and complete supported C0/C1/G0/G1
command lengths. Pen/window extents and reserved fields are checked. Unsupported
EXT1/reserved command forms and unsupported P16 encodings reject; unknown commands
are never skipped to claim success. Packet assembly must finish at end of input.
This establishes transport/control-subset validity, not 708 window rendering,
complete CTA semantic conformance or accessibility quality.

Every imported packet remains `Transport`, never `Semantic`. Source provenance
retains format/version, importer version 1, exact source SHA-256, output duration,
608 channel/pair inventory and completed 708 packet/service inventory. Deserialized
caption-source receipts revalidate all canonical CDPs and compare that inventory;
JSON cannot change counts or upgrade the packet qualification. The original source
hash is provenance, not independent certification of a supplied JSON document.
Input is limited to 8 MiB / 100,000 output frames to bound import work and materialized
CDP storage. Import uses explicit progressive VANC line 20 at offset zero; other
placements remain available through validated canonical ANC import.

Boundary coverage includes bad parity, missing initialization, duplicate controls,
column overflow, skipped drop labels, exact origin offsets, overlapping rows,
truncation at every CDP byte, header/footer/field-phase corruption with a repaired
outer checksum, orphan/missed/unfinished DTVCC packets, invalid extended services,
service-command overflow, maximum packet size and forged receipt inventory. The
ignored `caption_scc_and_708_cdp_survive_official_bmx_st436_final_mxf_rescan` test wraps
both SCC and 708 programs using official BMX 1.6 and then reads actual final MXF
ANC words through the shared production scanner. This is software carriage evidence;
no SDI receiver, caption renderer or hardware qualification is inferred.

Primary source record (consulted 2026-09-06):

* [SMPTE ST334-2:2015](https://pub.smpte.org/pub/st334-2/st0334-2-2015.pdf), CDP syntax,
  table 3 cadence/cc_count, section markers, footer/checksum and sequence rules.
* [FFmpeg n8.0 SCC demuxer](https://github.com/FFmpeg/FFmpeg/blob/n8.0/libavformat/sccdec.c),
  literal SCC header and Field 1 608 byte-pair carriage. Its approximate millisecond
  timestamp conversion is not copied; Mondrian uses Core exact rational timecode.
* [FFmpeg n8.0 CEA-608 decoder](https://github.com/FFmpeg/FFmpeg/blob/n8.0/libavcodec/ccaption_dec.c),
  parity, channel/control/PAC classification and caption-grid behavior.
* [CCExtractor v0.96.5 CEA-708 decoder](https://github.com/CCExtractor/ccextractor/blob/v0.96.5/src/lib_ccx/ccx_decoders_708.c),
  DTVCC packet/service structure, command extents and window coordinate fields.

The implementation is an independently written strict supported subset. The cited
open-source decoders establish operational byte handling; they do not substitute
for licensed CTA conformance certification or caption-monitor HITL.


### Endurance shared attachment admission

`FrozenAncillaryProgram::nonempty_frames` exposes a borrowed sparse inventory
for provider admission; it adds no alternate caption semantics. The campaign
retains one exact source file/hash and canonical program across physical output,
repeated AS-11 export and retries. The final independent MXF rescan calls the
same `verify_mxf` parser used by ordinary delivery and checks exact origin/rate/
duration against Export's canonical selection resolver. Physical correlation
markers are added only to the physical validation output, never silently merged
into the author's exported program. This implementation does not infer Semantic
caption qualification from transport checks or claim missing receiver hardware.
