---
status: accepted
---

# Own Dynamic HDR at Program Output

Dynamic HDR metadata describes the final composited picture. Mondrian therefore
stores one Dynamic HDR Program Module directly on `Sequence`, independently of
`SequenceSettings`, Tracks, Clips, and export presets. A Program contains
bounded format-specific ST 2094-40 Application #4 or Dolby Vision shot metadata
on exact Sequence time, immutable analysis Adapter/schema identity, the complete
Prepared Visual author fingerprint analyzed, and a canonical payload digest.
Any picture edit makes that analysis stale. Program and Shot IDs are strong
author identities and are rekeyed by Sequence duplication.

Delivery has three closed intents. `Omit` deliberately renders without dynamic
metadata. `PreserveSourceExact` requests only a detectable metadata family and
is valid solely when the Prepared Visual identity proves one complete,
unmodified source file. Export revalidates the captured file revision before
and after the copy, hashes the exact bytes while reading them, verifies the
output SHA-256 and byte length, and independently re-probes the output. This is
neither remux nor Smart Render and any failure blocks publication without a
render fallback. `Remake` projects one analyzed Program over the exact export
range and requires a qualified runtime Adapter for generation, independent
technical validation, and human HDR/SDR QC.

Public syntax support is not a branded-delivery claim. ST 2094-40 Application
#4 detection does not establish HDR10+ adopter status or certification. Dolby
CM version, metadata levels, bitstream profile/level, licensed/approved tools,
and delivery profile are separate facts. Executable paths, licenses,
entitlements, and adopter evidence remain machine-local and never enter Project
JSON.

The first Remake delivery row is progressive Rec.2100 PQ Legal HEVC Main10
10-bit 4:2:0 with authored ST 2086 and MaxCLL/MaxFALL. Mondrian currently has no
qualified/licensed Remake Adapter, so that path fails closed before tool or
encoder execution. This honest blocker is preferable to treating FFmpeg/x265 or
open metadata utilities as qualification evidence.

## Normative and qualification references

- [SMPTE ST 2094-40:2020, Dynamic Metadata for Color Volume Transform — Application #4](https://pub.smpte.org/pub/st2094-40/st2094-40-2020.pdf)
- [Dolby Vision Content Creation Best Practices](https://professionalsupport.dolby.com/s/article/Dolby-Vision-Content-Creation-Best-Practices-Guide?language=en_US)
- [Dolby professional licensing](https://professional.dolby.com/en-gb/licensing/)
- [HDR10+ Technologies](https://hdr10plus.org/)
