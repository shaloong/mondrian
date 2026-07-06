# Media Pipeline

`mondrian-media` owns FFmpeg-based media inspection, decode support, waveform/proxy/cache primitives, and audio buffers.

## Probe

`MediaInfo::probe(path)` uses FFmpeg format/codec metadata without decoding full media. It extracts:

- container and duration
- file size
- video streams: codec, dimensions, frame rate, pixel format, bit depth, alpha, detected color space, structured color interpretation, frame count
- audio streams: codec, sample rate, channels, layout, bit depth

The probe runs off the UI thread.

## Decode and Cache

Decoding and frame caching belong to media/renderer/export paths, not UI widgets. UI panels may request thumbnails or waveform data through app adapters, but must not own FFmpeg state.

Current decode residency is intentionally explicit and fail-closed. The active
preview/media decode path produces CPU RGBA frames and, for legacy YUV callers,
CPU YUV420p frames derived from that CPU RGBA decode. `DecoderMetricsSnapshot`
reports the selected hardware backend, decoded frame residency,
`hardware_decode_active`, `zero_copy_active`, and a stable reason string. Until
DXVA/D3D11VA, VideoToolbox, VA-API, or CUDA/NVDEC hardware frames are actually
exported/imported as renderer GPU textures, `HwAccelBackend::probe()` must
return `None`, `hardware_decode_active=false`, `zero_copy_active=false`, and
`DecodedFrameResidency::CpuRgba`. Platform preference alone is not a valid
hardware decode signal.

The preview decoder's experimental external-process path is named
`PreviewDecodeBackend::ExternalFfmpegCpuRgba` and is enabled only by explicitly
selecting that backend or setting `MONDRIAN_PREVIEW_EXTERNAL_FFMPEG_CPU_RGBA`.
It may ask the `ffmpeg` CLI for platform hwaccel, but its contract is still
`rawvideo` RGBA over stdout, so it is CPU-resident and cannot be reported as
Mondrian hardware decode, zero-copy, low-copy texture residency, or GPU frame
delivery. The previous "GPU assist" terminology is intentionally not used.
Every `RgbaFrame` returned by the preview decode boundary carries
`PreviewDecodeDiagnostics`: concrete path (`InProcessFfmpegCpuRgba`,
`ExternalFfmpegCpuRgba`, or `PreviewCacheHit`), elapsed microseconds, cache-hit
status, external-process status, and CPU-residency evidence. App-level preview
diagnostics aggregate those fields so playback/perf JSON can show whether a
4K/HDR test is decode-bound, cache-bound, or GPU-output-bound.

The renderer now owns a GPU input-stage resource contract for decoded CPU RGBA8
source frames: upload to `Rgba8Unorm`, execute the OCIO GPU input transform, and
produce a GPU-resident linear working frame in a float texture. This is the
bridge for guarded rollout of GPU input transforms. The app viewer uses this
contract for supported media preview layers before GPU working-space
compositing, falling back per-layer to CPU working-frame upload only when the
GPU input stage cannot be recorded. It is not yet a hardware decode or
zero-copy media path because the decoder boundary still hands CPU memory to the
renderer.

## Asset Classification

`mondrian-assets` classifies imported files using `MediaInfo`. Audio-only extensions or media without meaningful video streams become `Audio`; media with video becomes `Video`.

Synthetic assets use `MediaInfo::synthetic_adjustment_layer()` and `MediaInfo::synthetic_solid_color()`.

## Color Metadata

Media probe separates detected metadata from policy assumptions.
`VideoStreamInfo.detected_color_space` is the transform-facing detected-only
index. `VideoStreamInfo.color_interpretation` is the diagnostic/UI-facing
interpretation with confidence, evidence, warnings, and a user-overridable flag.
Evidence records whether a result came from a camera/log metadata hint, exact
CICP tags, partial CICP tags, unsupported CICP tags, or decoder unavailability.
Warnings preserve machine-readable provenance, not only a resolved color-space
enum: multiple-hint warnings keep the selected and ignored metadata keys,
values, and scopes; hint-vs-CICP warnings keep the selected hint and the raw
CICP triplet that conflicted with it. `VideoColorDiagnostic::summary()` is the
stable compact form for logs/export errors and should include those warning
details. `VideoColorDiagnostic::issue_summary()` is the machine-readable
contract for UI, telemetry, smoke JSONL, and export reports; callers must
consume its counters and flags instead of parsing the compact summary string.
`VideoColorDiagnosticIssueAggregate` is the shared rollup for combining many
per-stream diagnostics into one report surface.
Clip-level `MediaInterpretation` can override color space, frame rate, pixel aspect ratio, field order, and alpha interpretation.

Asset library records store persistent user intent separately as
`AssetMediaInterpretation`. Imported media defaults to `Auto`; Auto means "resolve
from current metadata, detector, and project color policy" and must not persist
the currently resolved color space. User changes from the asset-library
Interpret Footage dialog are stored as `Override { color_space }` and must
remain stable across metadata re-probes, relinks, and detector upgrades.
Non-color data is represented as asset payload classification, not as an
Interpret Footage color-space mode. It is reserved for masks, mattes, technical
textures, and advanced utility-channel workflows, not the primary input
color-space picker. UI may display the current resolved result, confidence,
method, and warnings beside Auto, but that resolved value comes from
probe/color-management diagnostics rather than the asset record.
Preview and export resolve input color with the same precedence: non-color
asset payload classification first, then clip-level override, then
asset-library interpretation, then detected metadata, then the sequence
missing-metadata policy.

Unknown/missing metadata policy is resolved at sequence color-management time, not by UI panels.
