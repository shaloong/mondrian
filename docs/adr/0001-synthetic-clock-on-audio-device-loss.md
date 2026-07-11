---
status: accepted
---

# Use a synthetic monotonic clock when the audio device is unavailable

During normal realtime playback the consumed audio-device sample position is the Clock Master. If the output device is lost or cannot be opened, Mondrian continues the Playback Session using a monotonic `SyntheticClockMaster`; it does not make displayed video frames authoritative and does not stop transport merely because audio is unavailable. Returning to Audio Master requires device stabilization, audio preroll, and a phase-controlled handoff. This preserves real-time transport while preventing render latency from becoming timeline time.

Automatic temporary preview-resolution reduction is permitted under sustained pressure, but proxy/original selection, color processing, and project settings never change silently. The architecture reserves rational playback rate and direction, while the first implementation exposes only forward `1x` playback.
