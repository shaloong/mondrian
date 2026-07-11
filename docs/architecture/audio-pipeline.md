# Audio Pipeline

Audio decode/mix and realtime output live in `mondrian-media`; the Playback
Engine owns Clock Master qualification and `mondrian-app` is the timeline/render
Adapter.

## Core types

- `AudioBuffer`: interleaved `f32` PCM with explicit sample rate and channels.
- `AudioMixer`: applies timeline track mute/solo, volume, pan, and bounded mix.
- `AudioSourceCache`: caches decoded source PCM by media path.
- `RealtimeAudioOutput`: one concrete CPAL stream with a fixed-capacity PCM queue
  and callback-only atomic telemetry.
- `RealtimeAudioOutputManager`: non-blocking Audio Playback Module for device
  open, stream-failure replacement, and bounded retry.
- `AudioRenderCursor`: render-window cursor only; it is never Clock Master
  evidence.

## Timeline interaction

Audio clips live on audio tracks. Track mute/solo controls are audio semantics;
video track visibility is separate. The app render Adapter produces ordered PCM
windows for one render generation. Seek, stop, stream replacement, or rejected
handoff invalidates that generation so stale worker completions cannot become
audible.

## Realtime ownership and lifecycle

The target clock, device-loss, preroll, and handoff semantics are specified in
[Playback Engine](playback-engine.md). Normal playback uses qualified consumed
device samples as Audio Device Clock Master. When output is unavailable,
transport continues on Synthetic Clock Master; displayed video is never master.

Device discovery and stream construction never execute on the UI thread. CPAL
streams are `!Send`, so a named device thread retains concrete stream ownership
for its entire lifetime. It publishes only a sendable handle made of `Arc`,
atomics, and the lock-free PCM queue. Open failure retries exponentially from
250 ms to a 5 s ceiling. An asynchronous stream error removes the handle and
reopens while Synthetic Master continues.

A new stream remains inactive while the app Adapter invalidates old render
work, clears queued PCM, anchors rendering to the current exact timeline
`TimeCode`, and queues at least 120 ms. Callback consumption is enabled only
after that preroll. Each clock observation carries the exact media anchor for
active-consumption frame zero; the Playback Engine derives candidate media
phase from that anchor and integer consumed frames. Phase error over the 20 ms
policy budget rejects the stream and starts a fresh preroll without moving or
reanchoring the published timeline.

Rendered, queued, callback-requested, latency-adjusted, and consumed positions
are distinct. Current CPAL evidence is explicitly graded
`CallbackConsumptionEstimate`; it is not a backend device position.

## Realtime callback contract

The callback may read/write only the preallocated queue and atomics. It does not
allocate, log, decode, inspect the timeline, perform file I/O, publish general
events, or take a contended lock. Inactive output writes silence without draining
the PCM queue. Active starvation writes silence and increments underrun evidence.

## Remaining depth

Device lifecycle and Clock Master qualification now have narrow Interfaces, but
render-window watermarks/generations remain app-owned. A later Audio Playback
slice will move them behind the same Module, add a backend-position Adapter beside
callback estimates, and implement bounded resampling/slew for small non-zero
phase error. Current handoff accepts phase already inside budget; it does not
claim to correct it.

UI Modules may request waveform or transport actions. They never decode audio or
own device state. Export consumes timeline/audio data through export
orchestration, not realtime output state.
