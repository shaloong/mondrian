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
- `RealtimeAudioOutputManager`: lazy, non-blocking device lifecycle
  Implementation retained behind Audio Playback's internal output Seam.
- `AudioPlayback`: deep Module owning device lifecycle, integer-sample PCM
  scheduling, render worker, generation invalidation, watermarks, preroll, and
  immutable evidence.
- `AudioPcmRenderer`: narrow render Interface implemented by the app timeline
  Adapter and the headless deterministic Adapter.

## Timeline interaction

Audio clips live on audio tracks. Track mute/solo controls are audio semantics;
video track visibility is separate. The app Adapter renders exact integer-sample
windows requested by Audio Playback; it does not own scheduling. Seek, stop,
stream replacement, or rejected handoff increments the generation. Completion
acceptance checks generation before changing in-flight accounting or output, so
old work cannot become audible or corrupt the current watermark.
The internal queue is bounded by the in-flight policy and reprime synchronously
removes queued old-generation windows. At most one already-executing old window
may finish, after which the generation gate discards it.

## Realtime ownership and lifecycle

The target clock, device-loss, preroll, and handoff semantics are specified in
[Playback Engine](playback-engine.md). Normal playback uses qualified consumed
device samples as Audio Device Clock Master. When output is unavailable,
transport continues on Synthetic Clock Master; displayed video is never master.

Device discovery is lazy: a project without an audible audio clip remains on
Synthetic Clock Master and does not start a meaningless output stream. Device
discovery and stream construction never execute on the UI thread. CPAL
streams are `!Send`, so a named device thread retains concrete stream ownership
for its entire lifetime. It publishes only a sendable handle made of `Arc`,
atomics, and the lock-free PCM queue. Open failure retries exponentially from
250 ms to a 5 s ceiling. An asynchronous stream error removes the handle and
reopens while Synthetic Master continues.

A new stream remains inactive while Audio Playback invalidates old render work,
clears queued PCM, anchors scheduling to the current exact timeline
`TimeCode`, and queues at least 120 ms. Callback consumption is enabled only
after that preroll. Each clock observation carries the exact media anchor for
active-consumption frame zero; the Playback Engine derives candidate media
phase from that anchor and integer consumed frames. Phase error over the 20 ms
policy budget rejects the stream and starts a fresh preroll without moving or
reanchoring the published timeline.

Rendered, queued, callback-requested, latency-adjusted, and consumed positions
are distinct. Current CPAL evidence is explicitly graded
`CallbackConsumptionEstimate`; it is not a backend device position.

Render scheduling accumulates integer sample frames, not floating-point seconds.
The production policy uses 80 ms windows, 120 ms preroll, a 460 ms high
watermark, and at most eight admitted windows. These values are one validated
`AudioPlaybackConfig`, not environment-variable semantics scattered through the
app. A failed or malformed window is replaced with exact-duration silence and
structured evidence; dropping it would shift every later sample against its
declared media anchor and is forbidden.

## Realtime callback contract

The callback may read/write only the preallocated queue and atomics. It does not
allocate, log, decode, inspect the timeline, perform file I/O, publish general
events, or take a contended lock. Inactive output writes silence without draining
the PCM queue. Active starvation writes silence and increments underrun evidence.

## Remaining depth

Device lifecycle, PCM scheduling, generations, watermarks, preroll, and Clock
Master qualification now have narrow Interfaces. Remaining Audio Playback depth
is backend-position evidence, bounded resampling/slew for small non-zero phase
error, explicit underrun recovery policy, and reference-machine drift gates.
Current handoff accepts phase already inside budget; it does not claim to
correct it.

UI Modules may request waveform or transport actions. They never decode audio or
own device state. Export consumes timeline/audio data through export
orchestration, not realtime output state.
