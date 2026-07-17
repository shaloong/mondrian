---
status: accepted
---

# Use exact author time across media

Mondrian persists timeline positions, durations, edit boundaries, automation
keys, and temporal curve handles as one normalized exact rational Timeline Time
interpreted in an explicit owner domain such as Sequence-local,
Component-Edit-local, Processing-Scope-local, Transition-local, or source-local
time. The canonical value
is a checked reduced numerator with a positive denominator; comparison and
arithmetic use checked wide intermediates rather than derived field ordering,
floating-point seconds, a universal fixed tick rate, or assumptions that two
operands share a time base.

The shared value representation does not erase domains. Sequence-local,
Component-Edit-local, Processing-Scope-local, Transition-local, source-local,
and nested-instance times cannot be compared or combined directly; placement,
trimming, speed maps, and nesting provide explicit checked Time Transforms
between them. A Time Transform may be affine or piecewise, but its mapping and
inverse/ambiguity contract are part of author semantics rather than a caller
convention.

Video frame positions, audio sample positions, shutter samples, and plugin
parameter-event offsets are derived evaluation coordinates. Conversion happens
at a declared boundary with an explicit rounding/sampling policy and the
resolved integer coordinate is then carried through that execution path. UI
snapping may choose a frame, sample, marker, or free-time grid without changing
the persisted time representation. SMPTE timecode is a separate display
contract containing nominal rate, drop/non-drop rules, and start offset; it is
not the arithmetic timeline value.

Visual and audio automation share the same exact curve primitives and stable
Parameter IDs. They do not share evaluation cadence or processor semantics:
visual consumers usually sample curves at frame or shutter instants, while the
audio compiler emits sample-offset parameter events. Time components of Bezier
handles are exact durations even when parameter values remain floating point.
The legacy `TimeCode`, `TimeTicks = frame * 1000`, string property paths, and
field-derived ordering remain migration inputs only and cannot define the next
project schema.
