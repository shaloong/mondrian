# Mondrian Editing Context

Mondrian edits time-based audiovisual projects while preserving deterministic timeline semantics and explicit runtime capability evidence.

## Language

**Playback Session**:
One contiguous transport run with a stable epoch, timeline mapping, rate, direction, and clock policy.
_Avoid_: Player instance, preview session

**Transport State**:
The user-visible stopped, paused, priming, playing, recovering, or ended condition of a Playback Session.
_Avoid_: Playback boolean

**Clock Master**:
The single authoritative elapsed-media-time source for a running Playback Session.
_Avoid_: Current video frame, UI timer

**Frame Demand**:
A versioned request for the best frame needed at a timeline position before a presentation deadline.
_Avoid_: Render request, preview refresh

**Frame Delivery**:
The observed outcome of a Frame Demand, including readiness, timing, execution path, degradation, and blocker evidence.
_Avoid_: Preview result

**Playback Quality Policy**:
The allowed temporary preview resolution and user-selected proxy/original policy for a Playback Session.
_Avoid_: Quality flag

**Playback Evidence**:
Structured events and aggregates proving clock, scheduling, delivery, degradation, synchronization, and recovery behavior.
_Avoid_: Debug log

**Preview Frame Store**:
The bounded runtime store for decoded CPU preview payloads, final Viewer rasters, failure memory, the explicitly pinned current/stale Viewer frame, and an oversize current-media exception that remains visible in evidence.
_Avoid_: Entry-count-only cache, unbounded last frame, dropped oversize current delivery

**Viewer GPU Preview Runtime**:
The device-scoped owner of native video import, working-linear compositing, spatial processing, display output, calibration, and current external-texture presentation resources for Viewer execution.
_Avoid_: Window-owned GPU grab bag, separate headless rendering semantics

**Audio Playback**:
The realtime path that owns output-device lifecycle, PCM preroll and consumption, render generations, underrun recovery, and consumed-media-position evidence for a Playback Session.
_Avoid_: Audio clock, UI-owned output stream

**Project Migration**:
An ordered, transactional transformation of one persisted archive, document, or SQLite schema version into the next supported version.
_Avoid_: Best-effort deserialization, ignored ALTER error

## Relationships

- A **Playback Session** has exactly one active **Clock Master**.
- A **Playback Session** has exactly one **Transport State** at a time.
- A **Playback Session** produces zero or more **Frame Demands**.
- Each **Frame Demand** produces at most one terminal **Frame Delivery**.
- A Viewer stale lifecycle state does not terminate a **Frame Demand**; only a deadline/policy decision may emit `StaleAvailable`, while an in-flight worker retains the chance to deliver `Ready`.
- A **Playback Quality Policy** constrains every **Frame Demand** in its Playback Session.
- **Playback Evidence** records state and clock transitions without owning them.
- **Playback Evidence** uses bounded versioned events and aggregates from real Frame Demand, Frame Delivery, Clock Master, seek, and Audio Playback observations; capability probes alone cannot satisfy execution gates.
- A **Preview Frame Store** admits and evicts CPU frames by both payload bytes and entry count; renderer-owned GPU resources remain outside this store and require their own budget evidence.
- A **Viewer GPU Preview Runtime** owns GPU execution resources independently of a Window; production Window and headless validation must adapt the same execution lifetime and must not duplicate color or compositing interpretation.
- Headless validation may report a current **Frame Delivery** as `Ready` only after the shared **Viewer GPU Preview Runtime** records an output and the real GPU submission completes; device capability alone is not delivery evidence.
- **Audio Playback** may offer an Audio Device Clock Master only after stream health, PCM preroll, and media phase satisfy Playback Policy.
- **Audio Playback** records isolated underruns without changing Clock Master; sustained missing-sample evidence enters recovery through a continuous Synthetic handoff and fresh preroll.
- Each persisted archive, document, and SQLite library has an independent version and **Project Migration** chain.
- A **Project Migration** operates on an in-memory value or runtime copy; opening never rewrites the source archive.

## Example dialogue

> **Dev:** “The Viewer missed frame 240. Should it set playback buffering?”
> **Domain expert:** “No. It reports a late **Frame Delivery**. The **Playback Session** decides whether its **Transport State** keeps playing, primes, or recovers according to the active **Clock Master** and quality policy.”

## Flagged ambiguities

- “Buffering” previously meant startup priming, per-frame decode waiting, and sustained recovery. These are distinct Transport States or Frame Delivery outcomes.
- “Audio clock” previously advanced from `Instant` even when it was not proven to represent consumed device samples. Device sample position and synthetic monotonic time are distinct Clock Masters.
