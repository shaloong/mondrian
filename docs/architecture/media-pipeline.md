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
`hardware_decode_active`, `zero_copy_active`, optional
`decoded_gpu_frame_handle_kind`, `renderer_import_ready`, and a stable reason
string. Until DXVA/D3D11VA, VideoToolbox, VA-API, or CUDA/NVDEC hardware frames
are actually exported/imported through the renderer native decoded-frame import
contract, `HwAccelBackend::probe()` must return `None`,
`hardware_decode_active=false`, `zero_copy_active=false`,
`DecodedFrameResidency::CpuRgba`, no GPU handle kind, and
`renderer_import_ready=false`. Platform preference alone is not a valid
hardware decode signal.

Preview path resolution is proxy-aware but does not synchronously generate
proxy media. `mondrian-media::ProxyGenerator` owns the shared proxy freshness
contract through `ProxyStatus` (`Missing`, `Fresh`, `Stale`). If project proxy
playback is enabled for an asset and the expected proxy file is `Fresh`, app
preview decodes that proxy path. If the proxy is `Missing` or `Stale`, preview
falls back to the source path and records proxy hit/miss/stale counters in
`AppUiPreviewDiagnostics`. Export continues to use the source/export contract;
proxy selection is a preview playback scheduling decision, not media color
interpretation.
Newly imported video assets enter proxy playback and start background proxy
generation only when the project `ProjectSettings.proxy_enabled` policy is on.
The project policy is the scheduling source of truth; app/UI preferences must
not independently enable proxy generation against a project that has disabled
proxy workflows.
Proxy generation and preview proxy-path resolution must use the same
project-derived `ProxyConfig`: `ProjectSettings.proxy_resolution` selects the
proxy height preset, and `ProjectSettings.cache_dir` places proxy media under
that cache root's `proxy/` directory when configured. Callers must not use
`ProxyConfig::default()` for project media scheduling because that would split
generation and playback lookup across different cache roots or resolutions.
Generation must also use `ProxyStatus`: a `Fresh` proxy is reused, while a
`Stale` proxy is regenerated in the background. Failed regeneration must not
delete the previous proxy file, because preview can keep falling back to source
until a fresh proxy is finalized.
`ProxyConfig.concurrent_jobs` is an execution contract, not a UI preference:
`mondrian-media` must limit expensive FFmpeg proxy transcodes per proxy cache
root before launching the transcode work. Fresh proxy reuse does not consume a
transcode slot. App code may schedule proxy requests, but it must not bypass the
media-layer limiter when starting background generation.
The app layer must enqueue proxy generation requests onto a shared background
dispatcher instead of creating one OS thread/runtime per asset. Dispatcher
workers are allowed to keep proxy requests moving, but expensive transcode
parallelism remains owned by the media-layer `ProxyConfig.concurrent_jobs`
limiter so batch imports cannot starve preview playback, UI, or export work.
`MultiLevelCache` must not weaken this contract: L1 memory hits and L2 proxy
index hits are valid only while the referenced proxy still resolves to
`ProxyStatus::Fresh`. A cached source fallback must be re-evaluated when a
fresh proxy later appears so proxy generation can actually improve playback
without requiring an app restart or manual cache clear.

`DecodedGpuFrameHandleKind` belongs to media because it describes the decoder
surface family that FFmpeg/hardware decode produced, such as D3D11 texture,
CVPixelBuffer, VA-API surface, or CUDA device memory. It does not imply that
the renderer can import or sample that handle. Platform capability discovery is
reported separately by `mondrian-platform-core` as native texture import support,
and renderer readiness is reported by `mondrian-renderer` through
`GpuNativeDecodedFrameImportSupport` / `GpuNativeDecodedFrameImportPlan`. App
code must not infer zero-copy playback from the media handle kind alone.

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
status, external-process status, CPU-residency evidence, seek status, decoded
frame count, in-process FFmpeg decoder threading mode/count, and stage-level
wall-clock timings for session open, cache lookup, seek, packet/decode,
software scaling, RGBA copy, and the experimental external-process path.
App-level preview diagnostics aggregate those fields so playback/perf JSON can
show whether a 4K/HDR test is decode-bound, long-GOP seek-bound, cache-bound,
single-thread decode-bound, software-scale/copy-bound, worker-queue-bound, or
GPU-output-bound. The same diagnostics must also preserve the stage timings from
the slowest single decode frame and slowest single post-decode render frame, and
track how long decoded jobs waited in the preview worker queue before decode
started. Performance reports classify their primary bottleneck from max-frame
stage timings plus max queue wait, while aggregate stage totals remain trend
evidence. This avoids blaming a cumulative stage total when an interactive stall
came from one pathological seek, decode, software-scale/copy, composite,
output-boundary frame, or current-frame job waiting behind other decode work.
The in-process preview decoder uses bounded slice threading by default. The app
preview service runs a conservative decode worker pool: one worker on small CPU
budgets and at most two workers on wider machines, so current-frame decode can
make progress while another worker is occupied by prefetch or a long-GOP seek
without letting preview decode oversubscribe the UI, renderer, or FFmpeg's own
codec threads. This worker pool is a scheduling guardrail only; it is not a
substitute for future cancellable decode sessions or hardware-resident decode.
`RgbaFrame` stores its RGBA8 payload in shared immutable memory so cache hits can
adjust per-request diagnostics without deep-copying a 4K frame. Callers that
need ownership must request it explicitly through the frame consumption API;
renderer color-frame boundaries should prefer the shared payload constructor.
Preview decode session reuse and the process-global preview frame cache must be
keyed by a media file fingerprint, not by path alone. Proxy regeneration
finalizes fresh media at the same proxy path, so same-path cache hits or reused
FFmpeg sessions are valid only while file length and modification timestamp
still match the fingerprint captured when the session/cache entry was created.
`MONDRIAN_PREVIEW_DECODE_THREADING` and `MONDRIAN_PREVIEW_DECODE_THREADS` are
diagnostic overrides, not separate decode semantics. Thread-local preview decode
sessions are intentionally kept alive for playback locality and must be
released through `clear_thread_local_preview_decode_session()` at explicit
lifecycle boundaries such as perf probes, media/project shutdown, or tests that
open threaded software decoders.

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
