# Audio Pipeline

Audio is currently implemented in `mondrian-media::audio` and orchestrated by `mondrian-app`.

## Core Types

- `AudioBuffer`: interleaved f32 PCM.
- `AudioMixer`: mixes multiple tracks to output sample rate/channel count.
- `AudioSourceCache`: caches decoded audio buffers by path.
- `RealtimeAudioOutput`: cpal-backed output queue.
- `AudioClock`: current provisional monotonic/sample-rate clock; it must not be
  treated as proof of device-consumed samples.
- `AudioSyncController`: soft/hard drift correction.

## Timeline Interaction

Audio clips live on audio tracks. Track mute/solo controls are audio semantics; video track visibility is separate.

## Realtime playback ownership

The target clock, device-loss, preroll, and handoff semantics are specified in
[Playback Engine](playback-engine.md). Normal playback uses actual consumed
device samples as Audio Device Clock Master. When the device is unavailable,
transport uses Synthetic Clock Master based on monotonic time; displayed video
frames never become the master.

`RealtimeAudioOutput` and the app-owned render worker are provisional
implementations. They will move behind Audio Playback Engine directives and
observations. Rendered, queued, submitted, and consumed positions must remain
distinct.

## Legacy sync policy

The current sync controller exposes audio-master/video-master roles and a
monotonic `AudioClock`. This is implementation evidence, not the final contract:
the future engine removes Video Master from realtime clock selection and uses
the explicit Audio Device/Synthetic Master policy.

## Boundaries

UI panels may request waveform or playback actions. They must not decode audio or own audio device state. Export uses timeline/audio data through export orchestration, not UI state.
