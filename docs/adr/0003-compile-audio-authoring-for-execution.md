---
status: accepted
---

# Compile audio authoring into immutable execution plans

Preview, playback, audition, analysis, and export compile the same validated
Sequence author model into an immutable typed audio DAG; they may schedule it
differently but cannot reinterpret routing, processing, automation, transitions,
channel layouts, latency, or nested outputs. Clip source mapping, resampling,
processor calls, fades, explicit two-input transitions, lane/Track summing,
sends, sidechains, delay compensation, and nested boundaries lower to generated
operations with deterministic author origins. Generated operations are never
persistent user-routing entities. Instantaneous routing cycles are invalid;
future feedback requires an explicit stateful delay construct rather than a
general cyclic author graph.

Automation addresses a stable processor instance and stable Parameter ID in its
owner-local time domain. Following ADR-0004, compilation maps shared exact
rational Timeline Time through
clip and nested time transforms once, then emits parameter events at exact
sample offsets independent of execution block partitioning. The legacy
`frame * 1000` keyframe coordinate and string-suffix parameter lookup are not an
acceptable audio contract. Crossfades are explicit relationships between two
contributions and paired gain curves after their clip-local processing, not
unary plugins inferred from overlap. Every summing point aligns parallel paths
using declared processor and nested latency; the mix core neither normalizes,
soft-clips, nor limits samples unless an authored processor requests it.

Execution identity is separated into Consumer Demand Intent, resolved Signal
Closure, an epoch-bound State Domain plus State Entry Plan, and high-frequency
Scheduler coordinates. Only equal resolved closures may share immutable plans;
mutable DSP state additionally requires the same continuity epoch, processor
origin, nested instance path, evaluation/time-mapping context, direction, and
processing-mode contract. Processor capabilities declare latency, tail,
processing quantum, reset, warmup, replay, checkpoint/state-transfer support,
and determinism. Seek, cold entry, graph replacement, exact continuation, and
controlled transition therefore produce explicit state-entry obligations rather
than ad-hoc resets or fingerprint-based state sharing.

Canonical audio fingerprints use a dedicated versioned deterministic binary
encoding of the migrated, validated author model. They are conservative
candidate indexes, not proof of equivalence: reuse also compares normalized
typed descriptors and complete dependency manifests, and mutable state is never
shared by hash alone. Missing/incompatible plugins preserve author state and
produce explicit unresolved or degraded admission; export cannot silently claim
success after bypass, parameter loss, latency mismatch, or substituted silence.
