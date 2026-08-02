---
status: accepted
---

# Use exact author time across media

Mondrian persists timeline positions, durations, edit boundaries, automation
keys, and temporal curve handles as one normalized exact rational Timeline Time
interpreted in an explicit owner domain such as Sequence-local,
Clip-local, Component-Edit-local, Processing-Scope-local, Transition-local, or
source-local time. The canonical value
is a checked reduced numerator with a positive denominator; comparison and
arithmetic use checked wide intermediates rather than derived field ordering,
floating-point seconds, a universal fixed tick rate, or assumptions that two
operands share a time base.

The shared value representation does not erase domains. Sequence-local,
Clip-local, Component-Edit-local, Processing-Scope-local, Transition-local,
source-local, and nested-instance times cannot be compared or combined
directly; placement, trimming, speed maps, and nesting provide explicit checked
Time Transforms between them. A Time Transform may be affine or piecewise, but
its mapping and inverse/ambiguity contract are part of author semantics rather
than a caller convention.

Every Clip occurrence owns one stable visual author domain. Transform, Opacity,
visual Effects, Masks, and generated visual content all evaluate in that
Clip-local coordinate. Sequence placement maps it by
`clip_time = clip_time_in + (sequence_time - position)`. An ordinary move,
Slip, or source Speed Map edit preserves `clip_time_in`; an in-edge Trim,
Split, or right-hand fragment advances it by the removed placement duration.
Source-local time remains the independent media/nested sampling coordinate.
This separation is intentional: changing which source pixels are sampled does
not implicitly retime or reverse downstream Clip-owned visual processing.

Video frame positions, audio sample positions, shutter samples, and plugin
parameter-event offsets are derived evaluation coordinates. Conversion happens
at a declared boundary with an explicit rounding/sampling policy and the
resolved integer coordinate is then carried through that execution path. UI
snapping may choose a frame, sample, marker, or free-time grid without changing
the persisted time representation. SMPTE timecode is a separate display
contract containing nominal rate, drop/non-drop rules, and start offset; it is
not the arithmetic timeline value.

Realtime audio prepare, reprime, and device recovery follow the same rule. The
Playback Engine lowers its authoritative phase at one explicit Playback Epoch
and monotonic timestamp directly to an `AudioSamplePosition` on the requested
output rate. A published integer video frame is never an intermediate audio
anchor; rate mismatch, negative realtime anchor, or overflow fails before
execution state changes.

After a video source time is lowered once to an integer stream PTS, exact frame
selection uses the decoded frame's half-open presentation interval
`[start_pts, end_pts)`, never a nominal-frame or nearest-PTS tolerance. A
positive decoded duration supplies a provisional end; an observed successor
PTS conservatively shortens it when earlier. With neither duration nor
successor, only equality with `start_pts` is proven. Playback and deterministic
Still requests fail closed with typed temporal evidence when no interval covers
the request; only explicit Scrub evaluation may publish a nearby non-covering
frame as Degraded. The same duration or successor evidence proves the same
interval for every access mode; access policy changes fallback behavior, never
the meaning of temporal evidence. A valid VFR interior therefore need not equal
the selected frame's start PTS.

Visual and audio automation share the same exact curve primitives and stable
Parameter IDs. They do not share evaluation cadence or processor semantics:
visual consumers usually sample curves at frame or shutter instants, while the
audio compiler emits sample-offset parameter events. Time components of Bezier
handles are exact durations even when parameter values remain floating point.
The legacy `TimeCode`, `TimeTicks = frame * 1000`, string property paths, and
field-derived ordering remain migration inputs only and cannot define the next
project schema.
