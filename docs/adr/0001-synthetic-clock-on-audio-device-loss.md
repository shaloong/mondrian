---
status: accepted
---

# Use a synthetic monotonic clock when the audio device is unavailable

During normal realtime playback the consumed audio-device sample position is the Clock Master. If the output device is lost or cannot be opened, Mondrian continues the Playback Session using a monotonic `SyntheticClockMaster`; it does not make displayed video frames authoritative and does not stop transport merely because audio is unavailable. Returning to Audio Master requires device stabilization, audio preroll, and a phase-controlled handoff. This preserves real-time transport while preventing render latency from becoming timeline time.

Logical Audio Render Admission and physical Monitor Sink Admission are
independent. Device failure affects the user/workspace Monitor Path, listening
availability, Clock qualification, and evidence, but never rewrites or
invalidates a Sequence Audio Program. Render failures, sink underruns, and clock
synchronization failures remain distinct outcomes; exact-duration silence may
protect realtime sample position but is evidence of degradation, never proof of
successful rendering.

Automatic temporary preview-resolution reduction is permitted under sustained pressure, but proxy/original selection, color processing, and project settings never change silently. The transport contract reserves rational playback rate and direction; the supported realtime mode is forward `1x` until additional modes satisfy the same Clock Master, continuity, and recovery invariants.
