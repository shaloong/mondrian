---
status: accepted
---

# Model interlace as exact progressive field samples

Mondrian models one interlaced encoded picture as two full-raster progressive
evaluations at exact display-field instants. Interlace is not an encoder flag
and woven scan lines are not an Effect or compositor value domain.

Media owns source field interpretation. Probe retains exact FFmpeg transport
order (`tt`, `bb`, `tb`, `bt`) separately from display dominance. A decode
Session owns one temporal BWDIF `send_field` graph and resets it on seek,
cancellation recovery, flush, or replacement. Unknown, mixed, or changing scan
evidence fails closed; field processing is part of decode/session/cache
identity and currently requires CPU-addressable frames.

Renderer owns Program picture sampling. A progressive Sequence evaluates once
on its frame grid. An interlaced Sequence evaluates twice on the exact doubled
field grid, preserving display order and row parity. Existing Effects,
Transitions, nested Sequences, temporal demand, seeds, and caches keep one
progressive Float32 Interface and independently observe each field time.

Export alone owns interlaced delivery pixels. It vertically prefilters each
full-raster sample, extracts the qualified parity, atomically weaves one encoded
picture, lowers exact field flags, and validates both stream-level field order
and decoded-frame dominance before publication. Smart Render and resident GPU
encoding remain progressive-only.

The first qualified output matrix is deliberately closed: 1920x1080 at 25 or
30000/1001 pictures per second, TFF, square-pixel Rec.709 Legal, 10-bit 4:2:2,
no alpha, MOV ProRes 422 LT/422/HQ or uncompressed v210 software encoding. BFF
output, UHD interlace, PsF, telecine, mixed dominance, interlaced image/float
masters, professional packages, and hardware output are unsupported until each
has its own execution and validation evidence.

This preserves ADR-0004 exact time across media and prevents Preview/Export,
CPU/GPU, or codec-specific interpretations from diverging.
