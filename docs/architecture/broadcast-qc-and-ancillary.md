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
