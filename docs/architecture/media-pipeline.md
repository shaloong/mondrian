# Media Pipeline

`mondrian-media` owns FFmpeg-based media inspection, decode support, waveform/proxy/cache primitives, and audio buffers.

## Probe

`MediaInfo::probe(path)` uses FFmpeg format/codec metadata without decoding full media. It extracts:

- container and duration
- file size
- video streams: codec, dimensions, frame rate, pixel format, bit depth, alpha, color space, frame count
- audio streams: codec, sample rate, channels, layout, bit depth

The probe runs off the UI thread.

## Decode and Cache

Decoding and frame caching belong to media/renderer/export paths, not UI widgets. UI panels may request thumbnails or waveform data through app adapters, but must not own FFmpeg state.

## Asset Classification

`mondrian-assets` classifies imported files using `MediaInfo`. Audio-only extensions or media without meaningful video streams become `Audio`; media with video becomes `Video`.

Synthetic assets use `MediaInfo::synthetic_adjustment_layer()` and `MediaInfo::synthetic_solid_color()`.

## Color Metadata

Media probe assigns a stream color space. Clip-level `MediaInterpretation` can override color space, frame rate, pixel aspect ratio, field order, and alpha interpretation.

Unknown/missing metadata policy is resolved at sequence color-management time, not by UI panels.
