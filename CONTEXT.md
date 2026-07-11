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
- A **Playback Quality Policy** constrains every **Frame Demand** in its Playback Session.
- **Playback Evidence** records state and clock transitions without owning them.
- **Audio Playback** may offer an Audio Device Clock Master only after stream health, PCM preroll, and media phase satisfy Playback Policy.
- Each persisted archive, document, and SQLite library has an independent version and **Project Migration** chain.
- A **Project Migration** operates on an in-memory value or runtime copy; opening never rewrites the source archive.

## Example dialogue

> **Dev:** “The Viewer missed frame 240. Should it set playback buffering?”
> **Domain expert:** “No. It reports a late **Frame Delivery**. The **Playback Session** decides whether its **Transport State** keeps playing, primes, or recovers according to the active **Clock Master** and quality policy.”

## Flagged ambiguities

- “Buffering” previously meant startup priming, per-frame decode waiting, and sustained recovery. These are distinct Transport States or Frame Delivery outcomes.
- “Audio clock” previously advanced from `Instant` even when it was not proven to represent consumed device samples. Device sample position and synthetic monotonic time are distinct Clock Masters.
