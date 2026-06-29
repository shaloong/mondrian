# Audio Pipeline

Audio is currently implemented in `mondrian-media::audio` and orchestrated by `mondrian-app`.

## Core Types

- `AudioBuffer`: interleaved f32 PCM.
- `AudioMixer`: mixes multiple tracks to output sample rate/channel count.
- `AudioSourceCache`: caches decoded audio buffers by path.
- `RealtimeAudioOutput`: cpal-backed output queue.
- `AudioClock`: sample-based clock.
- `AudioSyncController`: soft/hard drift correction.

## Timeline Interaction

Audio clips live on audio tracks. Track mute/solo controls are audio semantics; video track visibility is separate.

## Sync Policy

The sync controller supports audio-master/video-master roles. Soft drift adjusts playback rate within a limited percent; hard drift pads or drops frames; very large drift disables correction.

## Boundaries

UI panels may request waveform or playback actions. They must not decode audio or own audio device state. Export uses timeline/audio data through export orchestration, not UI state.
