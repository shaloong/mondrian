# GPU-Native Renderer

This document describes the production GPU direction. Timeline compositing has
one typed CPU/GPU family: scalar CPU paths remain correctness references and
reported fallbacks, while GPU work enters through renderer-owned ColorFrame,
`GpuFrameCompositor`, and Viewer/Export execution Sessions. The removed
untyped RGBA8 `RenderPipeline`/`FrameCompositor` prototypes are not alternate
production paths. UI rendering remains independently owned by
`mondrian-ui-renderer`.

The working compositor clears its accumulation target to transparent black.
Opaque viewer or export backgrounds are explicit downstream presentation or
delivery operations; they are never baked into the shared GPU Program frame.

The compositor records through one checked **Composite Execution Plan**. The
planner is a pure Renderer Module: it owns transformed Layer ROI, conservative
per-Layer damage, unchanged-accumulator preservation, bounded tile draws, and
pass-fusion evidence. The wgpu recorder is its Implementation and cannot derive
a second spatial interpretation. Preview and Export use this same Seam.

A bounded first Layer uses transparent clear plus scissored draws. A bounded
later Layer copies the non-overlapping complement of its damage rectangle from
the previous accumulator, loads that destination, and shades only damage. This
is required because render-pass clear ignores scissor and a ping-pong target's
untouched pixels are otherwise undefined. Full-canvas Layers retain the normal
pass. Tiles are at most 4096 pixels per axis and remain draws within one render
pass. A 4,096-tile hard limit fails before allocation. The plan reports actual
shaded pixels, avoided full-frame shader pixels, preserved-copy work, tile
draws, eliminated Layers, and fused point operations.
Its damage is intra-frame Layer-versus-accumulator evidence only; temporal
damage reuse remains unavailable until a caller can prove exact prior-output
identity and lifetime.

## Target Principle

Old model:

```text
CPU owns frame -> upload for display -> optional encode
```

Target model:

```text
GPU owns frame -> display/export consume GPU result -> readback only at explicit boundaries
```

## RendererContext / GpuContext

`GpuContext` owns shared `wgpu::Device`, `Queue`, and `Adapter`. Higher renderer contexts should own:

- pipeline caches
- bind group layouts
- texture pools
- staging buffers for explicit readback
- renderer execution-Session resource and pass lifetime
- diagnostics/profiling hooks

UI code may use `mondrian-ui-renderer`; timeline/video rendering should stay in `mondrian-renderer`.

## Texture Lifetime

- Temporary textures should come from pools keyed by size/format/usage.
- Textures handed to display/export become caller-owned or reference-counted handles with clear lifetime.
- Readback buffers are transient and must be named in code as readback/export/debug paths.
- CPU RGBA must not be the default exchange type between renderer stages.
- CPU-resident intermediate frames use `CpuColorFrame`; GPU-resident
  intermediate frames use `GpuColorFrameHandle`. The handle carries a
  `ColorFrameDescriptor`, renderer resource id, texture format, and diagnostic
  label; it does not expose CPU pixels or claim ownership of a concrete wgpu
  object outside the renderer resource table.

The texture pool is an idle-residency optimization, not authority to allocate
an unbounded active frame. `ViewerGpuExecutionRuntime` separately holds a
`ViewerGpuExecutionResourceGrant` with a pressure-sensitive idle pool and a
pressure-stable active texture byte/count limit. Before recording,
`estimate_viewer_gpu_active_working_set()` lowers the complete Viewer request
into checked per-stage demand. It includes source upload; native-import
encoded-RGB and working outputs; heterogeneous GPU plan peak
residency; Effect-domain intermediates; Cross Dissolve branches;
Adjustment/composite accumulators; the exact spatial pyramid; Program
Output/scopes; monitor output; and display-calibration output plus its RGBA32F
3D LUT. The media-owned decoder surface remains charged to the Frame Store's
native-resource grant. An idle-pool hit still counts as active while checked
out; a heterogeneous sub-grant does not replace the enclosing Viewer grant;
and returning an intermediate to the idle pool does not authorize the next
frame.

Offline deliverables use the sibling `GpuVisualFrameExecutor` Module. It accepts
only resolved working sources, typed DataTextures, already-resident nested
working handles, procedural solids, adjustments, and Cross Dissolve. The
Export Adapter owns decode and closure traversal but cannot reinterpret those
pixel domains. A whole-closure preflight selects GPU before any GPU pixel work;
nested results remain in the shared frame table, cross-working conversion uses
the same OCIO GPU runtime, and the root passes directly into the output boundary
for one encoder-pipe readback. Temporal and heterogeneous closures currently
select CPU before start.

`GpuVisualFrameExecutionResourceGrant` is independent from idle pooling and the
final output/readback grant. Before each node records, the executor adds exact
current frame-table bytes/count to conservative new upload, external-domain,
Transition, and composite demand. Arithmetic overflow or either exceeded limit
fails before allocating that node. Export freezes the grant per attempt and
reports its active high-water bytes/textures; it may not reduce precision or
fall back after GPU execution has begun.

Software-decoded compact YUV follows that same ownership rule. Its encoded-RGB
intermediate is acquired from the shared exact-contract pool and returned when
the submitted candidate's frame resource table clears on the next record; it
must not be removed and dropped immediately after input-color commands are
recorded. A failed pre-submit candidate returns that resource directly. Only
the compact plane transfer buffers use
the separate bounded asynchronous upload pool. A frame-local intermediate must
not bypass pooling with a direct device allocation, because deferred backend
allocator growth would otherwise re-enter realtime candidate preparation.

If bytes or resource count exceed the active grant, recording returns
`ViewerGpuExecutionError::ActiveWorkingSet` before any texture creation.
Renderer code cannot lower format precision, skip stages, or invoke a hidden
CPU path to satisfy the grant. Public estimate and runtime diagnostics make
the rejection reproducible without constructing a wgpu device. The estimate
uses logical texel bytes; driver padding, swapchain allocations, persistent
pipeline/OCIO resources, decoded CPU caches, and presentation ownership remain
separate resource contracts.

## Native Decoded Frame Import

Hardware decode and renderer texture import are separate contracts. Media owns
the decoder fact (`DecodedGpuFrameHandleKind` and frame residency), while the
device-scoped Renderer runtime owns both import support and the graph contract
for turning that exact decoder surface into a linear working frame
(`GpuNativeDecodedFrameImportSupport` / `GpuNativeDecodedFrameImportPlan`). App
code may schedule or diagnose this path, but it must not translate OS decoder
handles directly into Renderer resources or ask Platform code to probe a second
graphics device.

Native decoded surfaces are not modeled as `GpuColorFrameHandle` values because
they may be multi-plane YCbCr surfaces such as NV12 or P010. The renderer import
contract records the decoder handle family, source texture format, source color
space, target working color space, and a
`GpuNativeDecodedFrameVideoSampling` contract. That sampling contract is the
single place where the renderer learns limited/full range, YCbCr matrix,
transfer characteristic, effective bit depth, and chroma siting. When a
concrete backend reports readiness and support for that handle/format, the plan
allocates a renderer-owned linear `Working` frame handle; the imported decoder
surface remains a backend object consumed by the native sampling/input transform
pass.
The request-only native estimate charges only the renderer-owned encoded-RGB
and working outputs. The already-created decoder surface is governed by Media
Frame Store residency and is not counted again as a renderer texture; direct
import owns no duplicate YUV bridge allocation. D3D12 descriptor validation
still rejects codec-aligned storage above a 2:1 visible-pixel envelope so a
malformed or unexpectedly padded surface cannot escape bounded admission.
Native YCbCr conversion and OCIO input conversion remain two explicit renderer
passes with one color contract. `GpuNativeYuvDecoder` samples the native luma
and chroma plane views into a renderer-owned `Rgba16Float` source frame whose
descriptor is `Source + EncodedFloat`; this frame is encoded RGB in the resolved
source color space, not linear working data. `RenderGpuInputStageResourcePlan`
then consumes that already GPU-resident frame without an upload and executes the
same OCIO source-to-working processor used by CPU-uploaded source frames into a
renderer-owned `Rgba32Float` working texture. Callers cannot lower working
precision. The native import plan owns distinct encoded-source and
linear-working handles so a
backend cannot skip, reorder, or mislabel either pass.

The same YUV shader is also the sole materializer for media-owned compact CPU
YUV. This is not native decode or GPU zero-copy: the Renderer uploads retained
CPU planes before recording the YUV pass. Native NV12/P010 binds interleaved
luma/CbCr views; FFmpeg `YUV422P10LE` binds stride-preserving luma/Cb/Cr views
without expanding or converting them into an RGB staging image. The compact
plane textures survive ordinary Viewer candidate clears. A bounded
renderer-owned upload worker copies visible plane rows into reusable mapped,
256-byte-aligned transfer buffers before realtime candidate recording. The
runtime exposes one command-free preflight over the complete layer/Transition
stack; Window and Headless CPU-complete lookahead call it without reserving a
submission or presentation output. Recording repeats the same preflight and
does not encode any layer until every distinct contributing compact frame is
ready, preventing a multi-layer candidate from partially recording and
thrashing its bounded transfer pool. Until preparation completes, Viewer
execution returns typed backpressure and its payload-free completion edge wakes
the existing Preview retry loop. The realtime caller then records only
`copy_buffer_to_texture` commands in the same command buffer as YUV sampling.
Successful candidates bind buffer remap and pool return to GPU submission
completion; abandoned candidates drop their unsubmitted buffer. This avoids
both `Queue::write_texture`'s per-plane native staging allocation and a
full-frame host memcpy on the transport/UI thread, while keeping upload
ordering, cancellation, and memory ownership inside the Viewer runtime.
The explicit layout contract distinguishes
two-plane from three-plane storage, 4:2:0 from 4:2:2, and most-significant-bit
P010 from FFmpeg's little-endian, least-significant-bit `YUV422P10LE`. Both layouts produce the same typed
`Source + EncodedFloat` intermediate and therefore share color validation,
OCIO execution, spatial scaling, and Viewer composition semantics.

The YUV shader uses unfiltered `textureLoad` operations because YUV plane
formats are not assumed filterable. It performs renderer-defined bilinear
4:2:0 or 4:2:2 chroma reconstruction using explicit Left, Center, or TopLeft sample origins,
expands full or limited range in coded-value space, and applies BT.709 or
BT.2020 non-constant-luminance matrix coefficients. P010 samples are first
converted from normalized 16-bit storage (`code10 << 6`) back to exact 10-bit
code values; treating `R16Unorm` directly as normalized 10-bit data is invalid.
RGB values are not clipped before OCIO, preserving undershoot, overshoot, and
HDR signal precision. A real-wgpu accuracy test covers shader compilation,
plane bindings, range expansion, neutral chroma, and `Rgba16Float` readback.
The contract carries the complete `RenderInputTransform`, not only the target
working color space. OCIO engine selection, tone-map policy, working space, and
the required GPU backend therefore remain explicit through import planning.
Native import rejects CPU OCIO backends so platform adapters cannot substitute
an independent source-to-working transform.
NV12 must validate as 8-bit YCbCr and P010 must validate as 10-bit YCbCr.
Future 12/16-bit paths must add an explicit renderer format such as P016; they
must not reinterpret P010. RGB/BGRA native surfaces must validate with an RGB
matrix. YCbCr surfaces must fail closed when matrix or chroma siting is
unspecified, because silent platform defaults are not acceptable for HDR/PQ/HLG
playback.
The sampling matrix and transfer must also exactly match the resolved source
color space encoding; conflicting source labels and sampling facts are rejected
before backend execution or GPU frame allocation.

The default support contract is fail-closed. A platform without a complete
native-surface sampling and OCIO input backend must use
`GpuNativeDecodedFrameImportSupport::unavailable()` and planning must return
`RendererBackendUnavailable`. Windows DX12 is the first concrete backend: it
advertises `D3D12Resource` plus only the NV12/P010 formats enabled on the actual
wgpu device. A decoder reporting a GPU handle kind, or an OS name implying a
potentially importable family, is not enough by itself to claim hardware decode
playback, zero-copy, or low-copy frame residency. Diagnostics report the
specific missing layer: decoder GPU handle absent, renderer backend not ready,
unsupported handle kind, or unsupported source format.
Legacy DXVA2 and VDPAU can be FFmpeg CPU-transfer fallbacks, but they must not
be presented as the modern GPU-native renderer import path.

The app layer owns the combined readiness report because it is the first layer
that can see media decode facts and device-scoped renderer backend support
together. UI-independent `app::native_video_import` evaluates those facts into
one playback admission and one stable readiness report; Window and Headless
Adapters only project the result into their telemetry. This avoids giving media
a renderer dependency or making a Widget module the owner of execution
admission. CPU-decoded frames remain `CpuDecodedMedia`. Every ready support
contract declares `GpuNativeDecodedFrameImportMode`: D3D12VA, Metal, and Vulkan
direct-texture backends report `ZeroCopy`. A future Adapter that requires one
GPU pixel copy must explicitly report `GpuBridgeCopy`/`ReadyLowCopy`; the
production Windows path does not. A missing mode fails closed; backend construction or a native
handle alone can never imply zero-copy. D3D11VA remains a media
hardware-decode CPU-transfer
fallback; the renderer does not advertise the rejected D3D11-to-D3D12
cross-API sharing experiment.
There is deliberately no independent platform graphics-device probe. Such a
probe can select a different physical adapter and cannot prove feature,
allocation, queue, or synchronization compatibility with the active Renderer
device. The concrete import runtime created from that active Adapter/Device/Queue
is the sole capability authority. Any copy step must be reported from actual
execution evidence rather than a preflight label.
Media may report a platform-preferred hardware decode candidate such as
D3D12VA, D3D11VA, VideoToolbox, or VA-API plus expected NV12/P010 surface
formats, but a candidate is not renderer readiness. Windows candidates must be
ordered D3D12VA, D3D11VA, then legacy DXVA2; Linux candidates must be ordered
VA-API, then legacy VDPAU. Runtime FFmpeg/codec/device failure may fall through
to the next backend. Windows support becomes ready only after
`D3D12NativeVideoImportBackend` binds the active adapter/device/queue and
publishes its renderer-qualified FFmpeg device root; unimplemented platform
backends remain unavailable.
Renderer and product-window device creation request the adapter-supported subset
of wgpu `TEXTURE_FORMAT_NV12` and `TEXTURE_FORMAT_P010` through the shared
`native_video_texture_device_features` contract. P010 is admitted only when
`TEXTURE_FORMAT_16BIT_NORM` is also available and enabled because its luma and
chroma plane views are `R16Unorm` and `Rg16Unorm`. Enabling those features is
only a texture-format prerequisite: it does not prove that a decoder resource can be
shared, synchronized, adopted by the active wgpu device, sampled, or transformed.
Readiness therefore remains fail-closed until backend construction validates
the complete platform import path. Diagnostics distinguish missing device
format features, non-DX12 adapters, and backend construction failures.
Adapter selection enumerates the backends enabled on the wgpu instance. On
Windows it prefers a DX12 adapter exposing native NV12/P010 formats, so the
D3D12VA direct path is not accidentally disabled by selecting a Vulkan
representation of the same GPU. An explicit `WGPU_BACKEND` restriction remains
authoritative because excluded backends are absent from instance enumeration;
if enumeration yields no usable adapter, selection falls back to wgpu's normal
request path.
The Windows renderer backend owns D3D12VA source admission. Before any resource
sharing, it verifies FFmpeg's retained `AVD3D12VAFrame` ABI, the actual D3D12
NV12/P010 resource descriptor, visible-versus-storage extent, single-resource,
single-mip/sample layout, decode-fence device ownership, and exact adapter LUID
equality with the active wgpu DX12 adapter.
Codec-aligned storage dimensions may exceed the visible frame; smaller storage
is invalid. App, Core, and generic Platform code must not duplicate or weaken
these Renderer resource invariants.
Backend construction resolves the active DX12 adapter LUID to the DXGI index
used as typed admission identity, then constructs an FFmpeg D3D12VA device root
over the exact wgpu `ID3D12Device`. App installs that root into the worker-family
pool before it publishes native-decode admission. The pool replacement is
generation-safe: active FFmpeg `AVBufferRef` leases remain valid, but old
Sessions fail current-generation compatibility and App cancels old Broker
bindings plus decoder-resource cache residency. A late old-device result is
therefore stale rather than cache-only.

Every admitted D3D12 surface must report the same raw device pointer and adapter
LUID as the Renderer. The single Renderer queue waits on FFmpeg's per-frame
decode fence without a CPU wait, records `COMMON -> WGPU shader resource`,
adopts the original multi-plane `ID3D12Resource` into wgpu, runs YUV sampling
and OCIO, then records `WGPU shader resource -> COMMON`. It signals one strictly
monotonic renderer-completion fence after the release transition. The Media
frame lease and both transition command allocators/lists remain retained until
a nonblocking fence query proves the final renderer read. There is no shared
handle, renderer-created YUV texture, NT handle, decoder-device queue, or pixel
copy in this path.

At most four source leases may be in renderer flight. Exhaustion returns typed
`GpuNativeDecodedFrameImportError::Backpressure`; presentation keeps the last
completed output and may retry or discard the obsolete candidate without a
surprise CPU transfer. If queue execution or completion signaling becomes
ambiguous, the source and command objects are poisoned and retained for the
backend lifetime rather than being released unsafely. A device-removed
completion-fence sentinel remains a typed physical terminal. Compatibility
diagnostics retain the old `(contract pools, bridge entries)` schema, but the
Windows implementation reports `(1, 0)`; retained-source count is the actual
in-flight lifetime evidence.

Metal and Vulkan use the same shared direct-plane Module. Their platform
Adapters wrap CVPixelBuffer/IOSurface or DMA-BUF planes, while the shared Module
retains the complete Media frame handle through `on_submitted_work_done`; wgpu
texture drop alone is not treated as proof of the final GPU read.

Production-path real-media gates must prove native D3D12VA/P010 residency,
zero bridge-copy/readback/upload counts, native import execution, GPU timestamp
coverage, bounded retained-source reuse, and final zero retained sources. The
device-root integration gate separately proves real DX12 runtime construction,
FFmpeg adoption, idempotent install, replacement generation, `ZeroCopy`, and
zero bridge residency; it does not substitute for the real-media cadence gate.

Native-import GPU attribution is independent from the semantically fixed
Viewer suffix timer because native import submits its YUV/input-color prefix
before the caller-owned Viewer command buffer. Timestamp capability and
activation are separate: the default
`NativeVideoImportGpuTimingPolicy::Disabled` allocates nothing even on a
capable device; an evidence owner must explicitly select `Enabled { capacity }`
within the hard `1..=256` slot range. An activated D3D12 backend owns that
bounded asynchronous ring with exactly three ordered points:
post-acquire/start, after YUV-to-encoded-RGB, and after the source-to-working
input color stage. Every completed sample carries both a backend-runtime-local
Viewer-candidate token and import token, plus an optional lock-free observation
of whether the decoder fence was already complete before the queue
wait/copy/acquire chain. A report spanning multiple renderer runtimes must add
its own execution-session identity instead of treating either token as
process-global. The two reported deltas are
`yuv_decode_marker_bracket_us` and
`input_color_marker_bracket_us`; they must never be inferred from CPU recording
time or the later Viewer suffix.
Because the start counter is written in the wgpu import command buffer, these
deltas intentionally exclude decoder execution, decode-fence wait latency, and
the raw acquire transition that orders that command
buffer. Each bracket can still contain implicit barriers, scheduler gaps, and
backend command placement/reordering within its marker boundaries; neither
field claims pure shader time. The optional fence-ready fact helps classify
upstream readiness but is not a duration measurement.
The execution owner polls the device for its own lifecycle reasons;
`ViewerGpuExecutionRuntime` only exposes a collect-after-device-poll seam that
performs `try_recv`/mapped-readback work and never initiates polling. Cumulative
diagnostics separate `capability_supported`, `activated`, and
`inactive_reason`, then report `samples`/`pending`/`missing`/`dropped`. Those
accounting categories are disjoint and sum to valid-output imports.
Here `submitted_imports` means imports that returned a valid renderer working
frame. A native-import failure after ambiguous queue acceptance is excluded even
though the GPU may have accepted its command buffer; timing diagnostics are
usable-output coverage, not an inventory of all possible GPU work.
Ring exhaustion drops only telemetry, and disabled policy, unsupported
timestamp features, or a readback/runtime failure leave timing inactive without
failing, waiting, spinning, or changing the rendered frame. Ambiguous failure
after queue submission disables and retires the whole timing lifecycle instead
of recycling possibly in-flight query resources.
Viewer recording captures its candidate token locally at entry, carries that
exact value through every import, and ends the candidate scope on every ordinary
`Ok`/`Err` return. When timing is active, a successful
`ViewerGpuExecutionRecord` owns one move-only
`NativeVideoImportCandidateTimingReceipt`; the presentation Adapter may move it
out exactly once with `take_native_video_import_timing_receipt`. Its
`submitted_imports` is produced only by terminal probe consumption inside that
exact candidate scope and satisfies
`submitted_imports = scheduled_samples + missing_samples + dropped_samples`.
`scheduled_samples` means asynchronous readback was successfully registered,
not that a hardware sample has completed. A successful active candidate with no
native imports therefore returns an explicit all-zero receipt. Disabled or
unsupported timing returns no receipt, and a failed Viewer record suppresses
its receipt even if an earlier import already scheduled a sample.

A completed sample from such a failed candidate retains its original token and
remains deliberately unmatched, so report validation can classify it instead
of attaching it to a later frame. An internal renderer panic is not converted
into recoverable timing evidence; the outer panic-isolation seam must drop and
rebuild that renderer runtime. If a caller nevertheless starts another
candidate while a prior scope remains active, timing fails closed and disables
itself rather than rebinding later imports to the new token.

The media layer's FFmpeg hardware codec config probe is also only planning
evidence. It can prove that the linked FFmpeg decoder advertises a backend
config for H.264/HEVC/etc., but it does not create an OS device, expose a
native surface handle, or satisfy renderer import support by itself.
The cached FFmpeg hardware device-context probe goes one step deeper by creating
and releasing an `AVHWDeviceContext`, but it is still not a decoded-frame
residency contract. Renderer readiness requires an actual decoded NV12/P010
surface handle plus a platform import path that can sample that surface.
`HardwareDecodeCpuTransfer` is also not renderer readiness: it proves FFmpeg
hardware decode can be configured and hardware frames can be transferred back to
CPU RGBA, but the compositor still receives CPU-uploaded RGBA rather than a
native decoder surface.

Viewer GPU output telemetry now preserves decoder residency and payload
sampling facts through both renderer-owned source contracts. `ViewerGpuMediaSource`
carries CPU RGBA source pixels plus decoder diagnostics for the GPU OCIO upload
path; `ViewerGpuNativeSource` carries a native decoder surface contract
without CPU pixels. It also carries the exact aspect-fitted materialization
extent resolved from the Preview request, distinct from the physical native
surface extent. It retains the complete media-owned
`PreviewNativeDecodedFrame`, including the opaque process-local handle token,
instead of flattening the payload into diagnostic facts; this preserves the
resource identity and lifetime required by the renderer import backend. Both
preview source contracts expose `DecodedGpuFrameHandleKind`,
`DecodedVideoSurfaceFormat`, and `DecodedVideoSampling` (range, chroma location,
and effective bit depth) when the decoder reported them. The window layer maps
GPU-resident NV12/P010/RGBA facts into `GpuNativeDecodedFrameTextureFormat` and
combines decoder sampling with the resolved source color space into
`GpuNativeDecodedFrameVideoSampling` only at the app readiness seam. Unknown
range, unsupported chroma siting, bit depth mismatches, or RGB surfaces whose
resolved source color space does not have an RGB matrix fail closed before
readiness can report its declared transfer mode. Frame residency diagnostics can therefore
distinguish CPU-decoded media, native GPU-decoded media, native media blocked
by missing sampling facts, mixed CPU/native stacks, and procedural GPU-native
content across the concrete D3D12, VideoToolbox/Metal, and VA-API/Vulkan
Adapters.
The renderer owns `ViewerNativeVideoImportRuntime` independently from the
swapchain UI renderer. On Windows it constructs the concrete renderer
backend from the active adapter/device/queue and publishes that backend's support
contract; construction failure remains unavailable with its exact reason. The
runtime survives surface-format/UI-renderer rebuilds so display changes do not
discard the device-bound import runtime. A Renderer device rebuild publishes a
new decoder-device generation and retires old native decode/cache authority. It
allocates native import frame ids from the same
`RenderGpuOutputBoundaryRuntime` namespace that will receive the returned
working resources, preventing resource-table id collisions.
Renderer readiness must remain in
`PreviewHardwareDecodeAdmissionDiagnostics`. It must not be copied into
media `HwAccelProbe`, `PreviewDecodeDiagnostics`, hardware-decode decisions, or
media blocker enums. Media reports whether decode produced a retained native
surface; the app separately reports whether the active Renderer capability
allowed requesting that surface. Only the app admission boundary combines
those facts.
Concrete import execution belongs behind
`GpuNativeDecodedFrameImportBackend`. The shared
`PreviewNativeDecodedFrame` payload implements the renderer-owned
`GpuNativeDecodedFrameImportSource` contract directly. Conversion from
`DecodedVideoSurfaceFormat` to `GpuNativeDecodedFrameTextureFormat` is also
renderer-owned and returns `GpuNativeDecodedFrameSourceFormatError` for
non-native media formats. App code may consume that conversion for readiness
diagnostics, but it must not maintain a parallel renderer-format mapping.
The media payload's `PreviewNativeDecodedFrameHandle` retains an opaque
backend-owned resource lease. A matching import backend may downcast that lease
to its concrete resource type while the frame is borrowed; app/platform
schedulers may inspect kind/id diagnostics but must not reinterpret them as OS
handles. This keeps native resource release tied to the last frame/handle clone
instead of cache or window timing.
The first production media lease is `FfmpegNativeDecodedFrameResource` for
FFmpeg's D3D12VA ABI. It retains the source `AVFrame`/`AVBufferRef` and exposes
the borrowed `ID3D12Resource`, decode fence, and fence value only to the
matching renderer import backend. Media returns this payload as
`InProcessFfmpegNative` after app admission; renderer support becomes ready only
after `D3D12NativeVideoImportBackend` validates and imports that exact lease
into the planned float working-space resource. Media also understands FFmpeg's
preferred D3D11 frame ABI for ownership and CPU-transfer fallback diagnostics,
but no D3D11 native renderer backend is advertised.
The shared
`execute_native_decoded_frame_import(...)` helper owns support validation,
working-frame plan creation, backend invocation, and returned-resource contract
verification. Platform/window/app code must not fabricate
`GpuColorFrameResource` entries directly from decoder handles; a real D3D12,
D3D11, VideoToolbox, VA-API, or CUDA adapter must return the exact planned float
working frame or fail with a structured backend error.
Native NV12/P010 sampling carries the decoder matrix independently from the
encoded RGB source color-space identity. The GPU YCbCr pass supports BT.709,
FCC, BT.470BG/625-line BT.601, SMPTE 170M/525-line BT.601, SMPTE 240M, and
BT.2020 non-constant-luminance coefficients. Matrix identity is never rebuilt
from RGB primaries; explicit unsupported matrices must be rejected before GPU
resource import.

## Effect Integration

`mondrian-effects` lowers supported compiled graphs to `CompiledEffectGpuPlan`,
a graphics-API-neutral contract. The preview scheduler carries that plan and
the timeline frame seed across the app/window boundary; it does not interpret
effect math or own wgpu resources. `GpuFrameCompositor` consumes the plan in its
`Rgba32Float` working-space pass. Media and solid plans apply before
straight-alpha source composition; adjustment plans apply to the lower
accumulator and blend the processed result back. The retired RGBA8 global
executor/upload/readback path must not be reintroduced.
Media affine transforms (translate, scale, rotate, and non-singular shear) are
inverse-sampled in that same GPU pass for both uploaded and native-decoder GPU
working frames, so ordinary clip transforms do not introduce a readback.

Typed two-input visual Transitions are not decomposed into ordinary GPU layers.
`ViewerGpuExecutionLayer::CrossDissolve` owns two
`ViewerGpuTransitionInput` branches, and every non-transparent branch reuses
the same `ViewerGpuSourceLayer` preparation contract as an ordinary Timeline
source. The complete two-branch payload has one boxed owner so the ordinary
Source/Adjustment enum variants do not inherit both endpoints' inline size;
this indirection changes storage only, not endpoint identity or execution.
Each branch therefore completes source color conversion, affine
transform, Clip opacity and effect-domain processing before a dedicated
working-linear pass interpolates premultiplied coverage and restores the public
straight-alpha contract. Transparent endpoints reduce to one correctly
weighted source without inventing pixels. Preview execution records a distinct
`gpu_cross_dissolve_passes` fact, while a real-wgpu readback test compares the
shader with the Export/CPU reference formula. An ordinary source-over opacity
pair remains an invalid lowering.

The native point subset is a bounded single-source chain of ColorAdjust,
creative LUT, White Balance, Primaries, ASC CDL, RGB/YRGB and secondary Color
Curves, Vignette, deterministic Grain, and Crop. Effects compiles white balance into a working-space Bradford matrix
and supplies already validated Primaries/CDL vectors; the renderer only lowers
those immutable values into the common point uniform and WGSL. One pass accepts
at most sixteen operations, remains far below the 64 KiB uniform limit, and
splits longer admitted tails through the heterogeneous execution plan.
Unsupported topology, neighborhood spatial sampling, and custom operations
remain explicit lowering blockers until dedicated renderer graph passes provide
their resource and alpha contracts.

Qualifier is a dedicated non-preserving-domain GPU graph path. It is not
inserted into the point-operation uniform: the heterogeneous suffix lowers the
Effects-owned `PreparedQualifier` as `SceneLinearRgb -> AlphaMask`, then records
the existing GPU Mask pass or a dedicated Matte Preview pass. HSL and 3D
Include/Exclude sampling, Clean Black/White, inversion, box denoise, and
Gaussian feather mirror the CPU Float32 algorithm. The qualifier owns exactly
one pass without refinement, two passes with one separable refinement, or four
passes with both; every output/private RGBA32F texture is retained through the
submission and enters physical byte/texture admission. A real-wgpu parity gate
covers HSL, 3D, negative/HDR input, refinement, Mask output, and opaque
grayscale preview at `1e-4` maximum per-channel error, and proves the runtime
materialization table is empty afterward.

Power Window is a separate grade-matte GPU graph path. `MaskSource` geometry is
rasterized once in the cancellable CPU Float32 prefix and uploaded as
`AlphaMask + NonColorData`; `MaskCombine` reduces multiple Window textures with
Add, Subtract, Intersect, or Difference, and `MatteMix` combines an ungraded
base with its graded branch. The dedicated WGSL pass writes
`mix(base.rgb, graded.rgb, matte.a)` and copies `base.a`, so it cannot turn a
Window into programme transparency. The plan, token/live-set validator,
physical texture/byte admission, and runtime materialization table cover both
passes. A real-wgpu parity gate exercises the complete route, HDR/negative RGB,
and alpha preservation against the CPU Float32 reference at `1e-4` maximum
per-channel error.

Compiled curves do not consume per-operation uniform capacity beyond their
small location/mode/luminance descriptor. The compositor's device-resident
RGBA32F grade-resource atlas packs each curve set as a `256 x 3 x 1` slab beside
creative LUT cubes and keys residency by complete semantic fingerprint. WGSL
uses explicit texel loads and linear interpolation, including endpoint-slope
HDR extrapolation. A neutral-secondary flag bypasses the HSV stage exactly.
The real-wgpu point-chain parity test includes non-neutral Hue/Luma/Saturation
curves plus negative and greater-than-one values under the same `3e-5` Float32
channel budget.

Non-scene-linear point plans carry their exact `EffectColorDomain` into the
renderer. `RenderEffectColorDomainGpuPlanner` resolves that declaration into a
paired stock-OCIO identity route (`Working -> Effect -> Working`) using the
project color engine. `ColorFrameDomain::Effect` and
`RenderGpuColorTransformResourcePlan` describe the GPU-resident floating-point
intermediate and allocate it from the shared texture pool without upload or
readback nodes. `RenderGpuOutputBoundaryRuntime` records the two prepared OCIO
passes and a standalone `GpuFrameCompositor::record_point_effect_pass` into the
same caller encoder. The point pass retains the `Effect` descriptor, preserves
alpha, performs no blend, and returns a GPU working frame only after the second
stock-OCIO pass. Scene-linear effects omit this route entirely; data and alpha
domains remain non-convertible. The ordinary working compositor still rejects
an unmaterialized external-domain plan rather than evaluating encoded/log math
on working-linear samples.
App preview diagnostics count internal transforms separately from source input
and display/output transforms, preserving per-pass call and pixel evidence
without misclassifying them as transfer boundaries.

A real-wgpu readback test compares the fused media and adjustment outputs
against `apply_compiled_effect_graph_rgba_f32(...)` and
`apply_compiled_effect_graph_pass_rgba_f32(...)` per channel. This is the
numerical contract for extending the GPU subset; shader parsing or successful
command recording alone is not sufficient evidence of effect correctness. The
qualified chain includes working-space White Balance, Primaries, and no-clamp
ASC CDL and holds the same `3e-5` maximum per-channel Float32 budget while
retaining extended values.
An additional real-wgpu round-trip test compares
`Working -> OCIO -> display-encoded point effect -> OCIO -> Working` against the
stock-OCIO CPU processor with a `3e-5` maximum channel budget and asserts zero
upload/readback nodes around the effect route.

## Viewer Working-Linear Spatial Processing

`viewer_execution.rs` is the renderer-neutral entry boundary for Viewer GPU
inputs. `ViewerGpuExecutionLayer`, `ViewerGpuSourceLayer`,
`ViewerGpuTransitionInput`, `ViewerGpuMediaSource`, and
`ViewerGpuNativeSource` contain only media payloads, color transforms, effect
plans, typed graph relationships, compositing parameters, and native-resource lifetime. They contain no
Window registration key, playback ticket, cache key, or headless completion
policy. App code consumes these contracts directly; it must not re-export them
under App-owned aliases.

Native decoded-frame format and sampling resolution is Renderer policy and
therefore lives beside `ViewerNativeVideoImportRuntime`. Unknown range,
bit-depth mismatch, unsupported chroma location, or an RGB/YUV matrix mismatch
fails closed before backend execution. Product admission explanations remain an
App Adapter responsibility: they may combine exact media facts with the
device-scoped Renderer support snapshot, but cannot reproduce import format
policy, probe another graphics device, or construct Renderer resources.

`viewer_runtime.rs` owns the complete device-scoped Viewer execution lifetime.
Its immutable `ViewerGpuExecutionRequest` is independent of Window widgets,
texture registries, playback tickets, cache identities, and headless gate
policy. One `record` call executes native/CPU input preparation, working-linear
effects and compositing, Viewer crop/resize, the display boundary, and optional
proven calibration; `ViewerGpuExecutionRecord` returns the retained output plus
stage, compositor, spatial, residency, and fallback evidence. Window and
headless Adapters resolve the texture view from this same runtime, then perform
their distinct registration or completion obligations themselves. The
headless Implementation lives in the UI-independent App layer, uses a
full-frame spatial contract, and imports no Widget or Window publication type.
The Adapter also supplies a typed `ViewerGpuOutputPrecision`: SDR without
calibration may use the lower-bandwidth `Encoded8` carrier, while HLG, PQ,
high-bit validation, and any display-calibration route use `EncodedFloat16`.
The renderer no longer guesses output precision from calibration presence, and
rejects a calibration request paired with an 8-bit carrier before recording
frame commands. This prevents an HDR View from being silently quantized before
the presentation boundary without forcing every ordinary SDR frame through a
half-float target.
When recording fails, the Window Adapter may unwrap a composite-graph error only
to preserve the renderer's typed first-blocker telemetry; it must not reproduce
graph scheduling or collapse effect-domain failures into a generic CPU fallback.

GPU-resident media layers with log or display-encoded effect domains are
preprocessed through the runtime-owned `OCIO -> point effect -> OCIO` route
before working-linear affine/compositing. Procedural solids with those external
effect domains first materialize their unblended, untransformed, full-precision
color into a pooled working GPU frame, then use that same route; layer opacity,
affine transform, and blending remain after the effect-domain round trip in
authored order. The consumed plan
is removed from the working compositor request, so an otherwise eligible
single layer can retain its passthrough path. CPU working-frame fallbacks use an
explicit pooled RGBA32F upload node before the same effect-domain route; this
reports exactly one upload stage and no readback, while GPU/native media retain
their transfer-free path. The upload plan retains the CPU frame's shared
immutable RGBA32F payload instead of repacking another full-frame host buffer.
Scene-linear solids retain the direct procedural-uniform fast path for every
invertible affine transform. The composite shader applies inverse-affine source
bounds and evaluates fused position-dependent effects in source coordinates,
so transformed solids need neither a full-frame materialization pass nor a
second RGBA32F texture. Singular transforms remain typed blockers.
External-domain adjustments are scheduled by the renderer-owned composite graph:
it finalizes the lower accumulator, executes the stock-OCIO round trip, blends
the processed frame back with the authored adjustment opacity/blend mode, and
continues upper layers from that working accumulator. Contiguous ordinary layers
remain batched rather than forcing one compositor submission per layer. The
adjustment merge is a dedicated single-pass node that samples the original
accumulator directly; it does not build a two-layer composite or allocate/copy
an extra accumulator first.

Layers whose clamped opacity is exactly zero are non-contributing graph nodes.
The Viewer removes them before native import, CPU upload, source color
conversion, procedural materialization, or effect-domain preparation. The
shared composite graph also ignores zero-opacity external adjustments, and the
low-level compositor excludes all zero-contribution layers from capability
validation, passthrough selection, and pass recording. Diagnostics therefore
describe only executed work, and a visible GPU layer beneath an invisible
adjustment remains a zero-pass GPU passthrough.

`GpuCompositorTextureBindingDiagnostics` separately counts texture bind-group
creation and cache hits. This keeps backend object churn observable without
conflating it with uniform-buffer writes or render-pass counts.
The compositor uses separate sampled-layer and accumulator layouts so each
binding depends on one texture rather than a per-pass texture pair. Every
pooled `GpuColorFrameWgpuResource` carries a bounded eight-entry LRU of bindings
keyed by compositor/layout identity; the cache therefore survives exact-contract
pool reuse but is dropped with the texture when the pool evicts it. Procedural
dummy bindings are constructed once with the compositor. Hot-frame recording
clones lightweight wgpu handles and never extends a global cache that could keep
otherwise-evicted textures alive.

Every layout identity stored in that resource-owned LRU is allocated from one
renderer-global, strongly typed key namespace. OCIO wrappers, the working
compositor, spatial processing, and future consumers must not maintain separate
numeric counters: equal integers from different subsystems can otherwise return
a cached bind group created for an incompatible `BindGroupLayout` after pooled
texture reuse. The key identifies the concrete layout lifetime, not a frame,
texture, pass, or subsystem-local ordinal.

OCIO fullscreen wrapper inputs use the same resource-owned cache rather than
creating a texture/sampler bind group for every color stage on every frame.
Each prepared wrapper layout receives a device-runtime identity key; its
creation/hit counters are aggregated into
`OcioGpuWgpuBackendObjectRuntimeDiagnostics::wrapper_input_bindings`. Because
the cached binding lives on `GpuColorFrameWgpuResource`, an exact-contract pool
hit remains warm even though the new frame handle has a different ID, while a
pool eviction drops both texture and binding together.

GPU shader extraction keys include the exact `ColorEngine`, endpoints/view,
language, extracted processor cache ID, and the core OCIO cache-revision field.
Immutable Mondrian/ACES packages and validated Custom engines currently use
revision zero because their complete package/config/resource/processor digests
are already part of `ColorEngine`; changed Custom bytes produce a different
engine key. The process-global OCIO selection generation is operational reload
evidence and never participates in shader-cache equality, matching the CPU
Processor Session contract.

`viewer_spatial.rs` owns Viewer-only crop and resize processing. Its typed plan
accepts and produces only GPU-resident `Working + LinearFloat + Rgba32Float`
frames, so it cannot be scheduled after an OCIO display/output transform or an
ICC device transform. Strong downscales first build full-frame 2x box-prefilter
levels in working-linear light until the final reconstruction footprint is
bounded, then use two separable Lanczos3 passes over the requested visible
source region. Filtering is performed on premultiplied RGB and alpha and the
public output is restored to the compositor's straight/opaque-alpha contract.
The horizontal reconstruction texture is a typed
`PremultipliedCoverage` frame; pass uniforms are derived from frame descriptors
rather than axis-specific assumptions, so a resource cannot be premultiplied in
the shader while remaining mislabeled in the renderer resource table.
Plans reject more than 2:1 scale anisotropy, and prefiltering continues while
either axis exceeds the bounded 4x reconstruction footprint, so the shader
never silently truncates taps for malformed non-Viewer requests.

`GpuViewerSpatialRuntime` owns its pipelines, private prefilter/intermediate
textures, typed output resource, frame IDs, and cumulative pass/pixel
diagnostics. Private pyramid levels never enter app or UI caches. Real-wgpu
readback tests prove 1:1 sample preservation, linear-light checkerboard
downscaling, normalized crop coordinates, and mandatory RGBA32F working input.
The app/UI integration must provide presentation geometry; it must not
reinterpret this spatial filter or resample display/device code values.

## OCIO GPU Integration

`mondrian-renderer::OcioGpuShaderCache` is the renderer-side cache for OCIO GPU
shader extraction. It accepts color-space and display/view requests, calls the
core OCIO extraction boundary, and returns a shader plan with shader text length,
shader hash, texture/uniform counts, and the OCIO processor cache id.

This is the stable boundary before concrete backend execution. The cache must
not create device objects itself; shader modules, bind groups, LUT textures,
fullscreen wrappers, pipelines, and render-pass nodes belong to the backend
prep/object runtimes. Renderer diagnostics should track cache hits, misses, and
extraction failures so preview performance work can distinguish shader planning
cost from actual frame execution cost.

`OcioGpuShaderCache::prepare_wgpu_execution(...)` validates the shader-side
wgpu resource contract and returns a blocker-free execution plan when that
contract is internally consistent. Concrete backend failures must surface later
from `OcioGpuWgpuBackendPrepRuntime` or `OcioGpuWgpuBackendObjectRuntime`, not
as stale "not prepared" blockers in the planner.

The same preparation step also returns an `OcioGpuWgpuResourcePlan`. This plan
is the renderer contract for the future native pass: the OCIO binding contract
reported by the GPU shader descriptor, the separate Mondrian fullscreen wrapper
contract for input sampling/output location, input/output frame resource counts,
LUT texture counts, uniform buffers, samplers, bind group entries, bind groups,
and stable resource/pipeline-layout hashes.
`OcioGpuWgpuResourcePlan::for_shader_plan(...)` is a fail-closed validation
boundary. It rejects OCIO descriptor inconsistencies such as mismatched
texture/uniform counts, texture bindings before OCIO's `texture_binding_start`,
texture bindings overlapping the reserved uniform binding, missing uniform
buffers, duplicate resource indices/bindings, empty resource names, zero
texture extents, or empty LUT payloads before backend layout planning begins.
It is not a fake resource allocation layer; concrete `wgpu::ShaderModule`,
`wgpu::Texture`, `wgpu::BindGroupLayout`, `wgpu::BindGroup`, and pipeline
objects must still be created by the backend compiler/upload layer before
the Viewer or Export renderer execution Session records a native pass.

`OcioGpuWgpuBackendPrepRuntime` is the renderer-owned pure-preparation runtime
for this path. It owns the resource-layout cache and wrapper Naga artifact
cache, and turns one `OcioGpuShaderPlan` into an
`OcioGpuWgpuPreparedStaticPipeline`: prepared OCIO resource layout, wrapper
input binding plan, pipeline layout plan, wrapper link/source artifacts,
validated stage-split Naga wrapper modules, and the render-pipeline descriptor.
It creates no concrete wgpu objects; backend object caches consume this static
pipeline afterward.

`OcioGpuWgpuBackendObjectRuntime` is the renderer-owned concrete-object runtime
for that static pipeline. It uploads OCIO LUT and uniform payloads, validates
the packed bind-resource contract, creates the OCIO bind group, creates a
stable wrapper input bind-group layout, prepares wrapper shader modules,
pipeline layout, render pipeline, and render-pass node, then caches the whole
object bundle by resource/layout/module/output-format contract. Per-frame
wrapper input bind groups must be created from the stable
`OcioGpuWgpuPreparedWrapperInputLayout` so the bind group remains compatible
with the render pipeline layout.

`OcioGpuWgpuResourceCache` prepares and caches the backend binding-layout
contract derived from that resource plan. OCIO bind-group entries use the
descriptor set, uniform-buffer binding, texture binding start, and LUT binding
indices reported by OCIO rather than inferred WGSL text or count-based slot
assignment. Because wgpu separates texture-view and sampler bindings,
`OcioGpuWgpuSamplerBindingPolicy` derives deterministic sampler bindings after
the OCIO texture/uniform range and keeps the mapping by OCIO texture index,
dimension, sampler symbol, and interpolation policy. Mondrian wrapper resources
stay in a separate wrapper contract. This cache reports hits/misses
independently from OCIO shader extraction. It is the handoff point for backend
layout/object creation; it still creates no concrete device objects.

`OcioGpuWgpuBindGroupLayoutDescriptorPlan` turns the pure OCIO and wrapper
layout contracts into wgpu-ready bind-group layout descriptors. OCIO LUT
textures are declared as non-filterable 32-bit float sampled textures and their
paired samplers are non-filtering for portability; hardware linear filtering of
`R32Float` / `RGBA32Float` LUT textures must be enabled only through an explicit
device-feature policy. The fullscreen wrapper shader therefore remains
responsible for honoring OCIO's nearest/linear/tetrahedral/cubic interpolation
semantics, either manually or through a feature-gated hardware-filtering path.

`OcioGpuShaderTranslationCache` is the next boundary toward native execution.
It parses OCIO GLSL-family shader text through Naga, validates the module, and
caches successful Naga IR artifacts by source shader hash, OCIO binding-contract
hash, stage, source language, and target language. Optional WGSL is emitted only
as debug/diagnostic output; it is not the canonical execution artifact. Backend
module caches must create shader modules from `wgpu::ShaderSource::Naga` so
Mondrian avoids a GLSL-to-WGSL text round trip in the execution path.
Translation failures remain structured diagnostics, not fallbacks.

`OcioGpuWgpuLutUploadPlan` is the texture-upload handoff. It carries OCIO LUT
payloads copied from the GPU shader descriptor as-is, including texture/sampler
symbol names, binding indices, dimensions, channel packing, interpolation
policy, value hashes, and the raw `f32` values. Upload code must use
this plan rather than reconstructing LUT values from shader text or generated
WGSL.

`OcioGpuWgpuPackedLutUploadPlan` validates payload lengths against OCIO
metadata, preserves single-channel LUTs as `R32Float`, and expands RGB LUTs to
`Rgba32Float` with alpha set to `1.0` because wgpu has no portable RGB32Float
sampled texture format. `OcioGpuWgpuLutUploader` is the concrete backend
boundary that creates `wgpu::Texture`, `wgpu::TextureView`, and
portable non-filtering `wgpu::Sampler` objects from that packed plan while
preserving OCIO interpolation metadata in the resource plans. Upload success
alone still does not make the color pass executable.

Monitor ICC calibration uses a separate typed processor after OCIO display/view
output. `GpuDisplayCalibrationPlan` accepts only an encoded float frame in the
same standard source space used to build `DisplayCalibrationLut3d`, and produces
a `Device(calibration_key)` / `DeviceFloat` frame. That calibration key is the
complete 32-byte ICC identity and is carried through the frame descriptor and
every `GpuColorFrameHandle`; LUT and pass bindings retain complete sampled-LUT
identity as well. The derived `u64` diagnostic projection cannot authorize
equality, a cache hit, or a pass. The backend uploads the cube as RGBA32F and
performs explicit trilinear interpolation with
`textureLoad`; this avoids requiring `FLOAT32_FILTERABLE` and keeps CPU/GPU
sampling rules identical. Real-wgpu tests read back the pass and compare it
against the core CPU reference. `GpuDisplayCalibrationRuntime` caches pipelines
by target format and LUTs by the complete fingerprint plus edge size, while
retaining only the current device-RGB output frame. Runtime diagnostics count
pipeline builds, LUT uploads, and records. The app combines this processor proof
with the explicit opaque sRGB-surface code-value carrier before unblocking ICC.

`OcioGpuWgpuUniformUploadPlan` is the uniform-buffer handoff. It carries OCIO
uniform names, types, offsets, value counts, and copied values. The packed
uniform buffer uses OCIO's reported `buffer_offset` and `uniform_buffer_size`
rather than inferred shader layout, and `OcioGpuWgpuUniformUploader` creates the
matching `wgpu::Buffer`. Unsupported uniform payloads, offset overflows, and
out-of-bounds writes fail closed before a GPU object is created.

`OcioGpuWgpuShaderModuleCache` is the first concrete backend-object boundary.
It validates that an `OcioGpuTranslatedShader` and `OcioGpuWgpuResourcePlan`
share the same shader hash and expanded OCIO binding contract before creating a
`wgpu::ShaderModule` with `wgpu::ShaderSource::Naga`. It deliberately does not
create bind groups or the final fullscreen render pipeline; those remain
explicit native blockers until the wrapper shader contract and render pass are
implemented and verified.

`OcioGpuWgpuBindResourcePlan` is the bind-resource validation boundary between
packed OCIO payloads and concrete bind-group creation. It validates that packed
LUT textures and packed uniform buffers share the resource key, binding indices,
texture/sampler symbols, extents, dimensions, and source-value hashes required
by `OcioGpuWgpuResourcePlan`. The plan also keeps Mondrian's fullscreen wrapper
input texture/sampler bindings in a separate wrapper bind group.

`OcioGpuWgpuBindGroupPreparer` is the concrete backend-object boundary for bind
groups. It validates uploaded LUT textures, uploaded samplers, and uploaded
uniform buffers against `OcioGpuWgpuBindResourcePlan` before creating the OCIO
resource `wgpu::BindGroup`, and it creates the separate fullscreen wrapper
input bind group from a GPU frame texture view and sampler. Creating these bind
groups is necessary but still not sufficient for native execution:
`can_execute()` must remain false until the fullscreen wrapper shader and render
pipeline/render-pass node consume those bind groups and prove preview/export
consistency.

`OcioGpuWgpuPipelineLayoutPlan` composes the OCIO resource bind group and the
wrapper input bind group into a stable pipeline-layout contract, and
`OcioGpuWgpuPipelineLayoutPreparer` can create the concrete
`wgpu::PipelineLayout` once both bind groups have been prepared.
`OcioGpuWgpuFullscreenShaderContract` records Mondrian's fullscreen vertex and
fragment entry points, output location, and draw topology.
`OcioGpuGeneratedProgramContract` analyzes the OCIO-generated source using the
function name, pixel variable, and resource prefix configured by
`mondrian-core`; it distinguishes callable OCIO programs from complete fragment
shaders or unknown source shapes. `OcioGpuWgpuWrapperLinkPlan` combines that
program contract with Mondrian's fullscreen wrapper contract and reports
structured blockers such as missing function names, missing pixel variables, or
complete fragment shaders that must be split before wrapping.
`OcioGpuWgpuWrapperShaderSourceArtifact` is the next boundary in that chain: it
generates stage-split GLSL source artifacts from a linkable OCIO callable
program plus Mondrian's wrapper contract, strips duplicate GLSL version
directives from the fragment source, carries stable stage/link hashes, and fails
closed when the link plan still reports blockers. The combined source is
diagnostic text only. Because these are stage-split GLSL artifacts parsed by
Naga, the backend entry point for each stage is `main`; vertex and fragment
stages live in separate modules.
The wrapper snapshots incoming alpha before calling the OCIO-generated RGB
program and restores it afterward. CPU execution uses OCIO's strided RGB
processor on interleaved RGBA for the same contract. Program color transforms
therefore cannot perturb straight or premultiplied coverage; alpha and data
semantics stay owned by the compositor rather than the display processor.
`OcioGpuWgpuRenderPipelineDescriptorPlan` consumes the wrapper-link plan and
captures the output target format plus render-pipeline descriptor hash.
`OcioGpuWgpuWrapperShaderModuleArtifactCache` translates the wrapper source
artifact into validated vertex and fragment Naga modules, keyed by wrapper
source hash, link hash, pipeline-layout hash, render-descriptor hash, and output
format. WGSL remains diagnostic output only.
`OcioGpuWgpuWrapperShaderModuleCache` creates the concrete vertex/fragment
`wgpu::ShaderModule` pair from those Naga artifacts, and
`OcioGpuWgpuRenderPipelineCache` validates the descriptor, concrete pipeline
layout, and wrapper module metadata before creating the fullscreen
`wgpu::RenderPipeline`. `OcioGpuWgpuRenderPassNodePlan` and
`OcioGpuWgpuRenderPassRecorder` are the final backend-node boundary: they
validate the pipeline, OCIO bind group, wrapper input bind group, target format,
and target resource key before recording a fullscreen draw into an existing
command encoder. `RenderGpuColorPassSchedule` is the renderer-facing execution
binding above that recorder. It validates concrete input/output
`GpuColorFrameHandle` values, creates the wrapper input bind group from the
scheduled transform's wrapper contract, converts the output frame view into the
OCIO render-pass target, and records through the shared recorder.
`record_wgpu_from_resources` is the resource-table-backed entry point for
preview/export frame evaluation. App and export code must not reassemble OCIO
bind groups or render-pass targets independently.

`GpuColorFrameResourceTable` is the shared resolver for GPU-resident color
frames. It maps typed `GpuColorFrameHandle` ids to backend payloads, and every
lookup revalidates the handle's descriptor and texture format against the stored
entry. This prevents stale texture reuse when a resource id is recycled across a
different color space, domain, encoding, extent, or target format. The concrete
wgpu payload is `GpuColorFrameWgpuResource`, which owns the texture, default
view, and sampler used by fullscreen color passes.

`GpuColorFrameAllocationPlan` creates empty GPU color targets/intermediates
with a shared usage contract for upload, sampling, rendering, and readback.
`GpuColorFrameUploadPlan` packs CPU boundary frames for upload: linear
`CpuColorFrame` data is explicitly packed as `Rgba32Float`, while encoded
`CpuEncodedColorFrame` data is packed as `Rgba8Unorm`. `GpuColorFrameUploader`
materializes those plans into `GpuColorFrameWgpuResource` entries. Preview and
export frame evaluation should fill the shared resource table through these
plans before calling `record_wgpu_from_resources`.
`RenderGpuOutputStageResourcePlan` is the bridge from `RenderColorStagePlan` to
those concrete resource plans: it consumes the explicit
`UploadToGpu -> GpuColorTransform -> optional ReadbackToCpu` stage shape,
allocates stable GPU frame handles, creates the CPU upload and output target
allocation plans, and produces a transfer-resolved GPU transform for
`RenderGpuColorPassSchedule`. Its materializer uploads and allocates wgpu
resources through `GpuColorFrameUploader`, validates both planned table slots,
and inserts the resources into `GpuColorFrameResourceTable` without leaving a
partially materialized stage when a contract conflict exists.
When the source stage shape includes `ReadbackToCpu`, the resource plan keeps
the matching `GpuColorFrameReadbackPlan` and exposes renderer-owned helpers to
resolve the output frame from `GpuColorFrameResourceTable` and record the copy.
`RenderGpuOutputBoundaryRuntime` owns this path's state for exactly one
execution owner. The Window Viewer retains it for one live GPU Session. Each
Export attempt constructs a different runtime inside its job-local visual
Session, reuses it only across that attempt's frames, applies attempt-local
backoff after failure, and releases it at the terminal gate. It combines the
OCIO shader cache, pure backend prep runtime, concrete backend-object runtime,
GPU color frame id allocator, and
`GpuColorFrameResourceTable<GpuColorFrameWgpuResource>` so callers do not split
those contracts across unrelated services or accidentally create process-wide
Viewer/Export residency.
`RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend(...)`
is the app/export-facing entry point that plans the boundary, prepares or reuses
the static/backend OCIO objects, derives the resource plan, materializes the
planned frames, schedules the matching `RenderGpuColorPassSchedule`, records
the OCIO fullscreen pass, and records the optional readback copy. Lower-level
renderer code may still call the stage or resource recorder with explicit
backend contexts after it already owns the validated backend objects.
The returned `RenderGpuOutputStageRecord` carries the same
`RenderColorStageDiagnostics` shape as the planned stage graph so app/export
telemetry can prove whether a frame used upload, native GPU OCIO, readback, and
which pixel budget was touched without reconstructing the plan externally. The
diagnostics include native blocker breakdowns for shader module preparation,
OCIO resource bind groups, fullscreen wrapper generation, and render-pipeline
preparation, so GPU readiness reporting does not collapse into a single opaque
blocker count.
Renderer owns a manual ignored smoke for this exact boundary:
`cargo test -p mondrian-renderer gpu_output_boundary_runtime_smoke_report_on_real_wgpu_device -- --ignored --nocapture`.
It requests a real headless wgpu adapter, records a working -> display output
boundary through `RenderGpuOutputBoundaryRuntime`, reads back the encoded
texture, compares it with the CPU reference path, and emits
`MONDRIAN_RENDERER_GPU_OUTPUT_JSON`. Setting
`MONDRIAN_RENDERER_GPU_OUTPUT_SMOKE_OUTPUT` appends the same JSON as a JSONL
record for perf dashboards. The JSON includes a versioned `health_report` as
its sole high-level contract. That report embeds the derived summary from the
raw stage/runtime diagnostics: native GPU output readiness, upload + GPU OCIO +
readback stage completeness, blocker-free execution, shader/backend runtime
readiness, readback byte completeness, expected readback bytes, and CPU/GPU
parity within tolerance, then adds fixed checks, root causes, actions, and
evidence. The renderer exposes that contract through
`RenderGpuOutputHealthReport` plus the serializable frame/stage/runtime report
types, so smoke output and downstream tooling share one schema owned by
`mondrian-renderer`. Schema v2 names the boundary input explicitly as
`working_color_space`; an output-boundary report must never relabel its
scene-linear input as an external camera/display `ColorSpace`. This smoke proves
renderer-side upload + native GPU OCIO + readback sequencing; it does not prove OS
swapchain/display-management correctness, which remains the app-window display
contract's responsibility.
The app UI wgpu window session owns one `RenderGpuOutputBoundaryRuntime` for
the surface/backend lifetime and traces its cache/resource diagnostics with the
frame renderer diagnostics. Viewer GPU output telemetry also derives a
per-attempt `health` summary from the last output-boundary outcome, final stage
diagnostics, display-boundary blockers, and presentation readiness. That summary
separates ready native GPU output, blocked display contracts, degraded
presentation readiness, missing output textures, record failures, and external
texture registration rejection without making dashboards reconstruct the state
machine from counters. Setting `MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT` appends those
viewer diagnostics as JSONL from the live app window, including sequence id,
timeline frame, preview dimensions, external texture key, output target, output
color space, tone-map flag, optional OCIO display/view, frame residency
diagnostics, and cumulative health counts for
ready/degraded/blocked/failed/rejected/waiting outcomes. Frame residency reports
whether the candidate was CPU-decoded media, procedural GPU-native content, or a
mixed stack; whether working composition was CPU or GPU resident; whether input
transform was CPU OCIO or GPU-native; upload/readback/native bridge-copy counts;
and whether the frame is truly zero-copy/low-copy. The same
records also carry the latest structured viewer color rejection and its machine
issue summary when missing-metadata policy rejects preview media. This gives
playback/scrubbing sessions a persistent health stream that can be budgeted and
correlated back to specific frames in addition to trace logs; the
`viewer_gpu_output_budget` developer binary consumes that JSONL and exits
non-zero when the configured health thresholds or display-issue thresholds are
violated. It emits a versioned health report for CI walls and issue
attachments. That report carries the diagnostics stream source path and keeps
the raw budget summary intact, including
structured display issue reason counts, payload-blocker counts, aggregated
media issue counts, stage counters, and last health flags, then adds fixed
diagnostic checks, root causes, actions, and evidence across capture integrity,
viewer output state, GPU color path, display contract, display capability
drift, and media color policy. Display capability drift must not collapse into
one opaque bucket: reports should distinguish refresh churn, issue-after-refresh
correlation, HDR tone-map headroom drift, surface-format set drift, per-format
color-space capability drift, present-mode drift, and alpha-mode drift. Reports
must use this single structured shape
instead of parsing trace text, reconstructing readiness from ad hoc counters,
or depending on a legacy summary-only output. The same session also owns
the display-output contract for the current wgpu 30 surface and monitor:
selected sRGB surface
format, selected `SurfaceColorSpace`, SDR/HDR mode, available surface formats,
per-format surface color-space capabilities, `display_hdr_info` and tone-map
headroom diagnostics, present modes, alpha modes, and monitor fingerprint.
Viewer preview evaluation now splits at the correct boundary:
`app::preview_runtime::PreviewProductionRuntime` resolves the timeline and
composites a working-space
`CpuColorFrame`, while the app window validates the requested display boundary
against that contract and records the display/output boundary through the
session-owned GPU runtime. Unsupported presentation requests, such as HDR output
on an SDR-only surface, Display P3 output on an sRGB surface, Rec.2020 SDR output
with no direct wgpu presentation color space, or camera-log output treated as a
display space, are structured blockers rather than implicit SDR or OS/backend
fallbacks. Window resize, scale-factor, and move events refresh the
display-output contract. When that contract changes, the window unregisters the
previous external preview texture, clears output-runtime frame resources, and
marks the viewer frame dirty; if the selected surface format changes, it also
reconfigures the surface and rebuilds the frame renderer for the new format. A
selected surface color-space change follows the same rebuild path. The preview
service may keep a CPU `RasterImage` as the correctness/fallback path, but it
does not own wgpu objects and must not create short-lived GPU output runtimes
inside CPU media workers. After a concrete Window recording/device-generation
failure, the App switches the current semantic generation to a dedicated
one-worker, one-queued-request CPU fallback Adapter. It requests
CPU-addressable media, runs canonical basic effects/composition plus Program
Output and monitor color transforms off the Window thread, publishes a
validated sRGB raster through the existing Frame Store and presentation
ticket, and drops late generation/epoch results. This is real execution rather
than a diagnostic-only `CpuFallbackRequested` state. Creation of a replacement
healthy GPU generation explicitly exits fallback and invalidates the old
execution generation before GPU admission resumes.
That Adapter's single scheduling worker is not the pixel-kernel parallelism
authority. Its retained `TimelineCompositeScratch` owns the bounded CPU Visual
Execution Module used by Export as well: full-frame Normal blends and Cross
Dissolve may dispatch to that owner-local pool, opaque Dissolve may use
runtime-selected SIMD, Adjustment passes reuse the transferred base allocation,
and Transition endpoints reuse grant-bounded scratch. The pool, SIMD choice,
and scratch Implementation remain behind the compositor Interface so Window,
media, and Export Adapters cannot acquire a second execution interpretation.
Preview resolution is sampling density, not timeline geometry. Source and
sequence transforms are evaluated in their full authoring extents, then
`project_affine_to_sampled_extents` projects that affine into the decoded and
output sample extents (`S_output * T_authoring * inverse(S_source)`). A half-size
decode therefore preserves the authored fit instead of applying scale twice;
non-uniform sampled extents project both axes and translation independently.
CPU composition, nested sequences, and the GPU candidate handoff share this
contract. Code that changes preview scale must not rewrite clip transforms or
pretend sampled pixels are the source's logical dimensions.
The persisted `display_issue_summary` is not just a reason string: it carries
the active display target fingerprint plus current/selected/desired surface
format, color space, encoding, HDR mode, payload blocker, and support evidence
so smoke tooling can diagnose the exact presentation contract mismatch from the
JSONL report alone.
Display-contract refreshes are part of the same persisted evidence: the session
records structured resize / scale-factor / window-move events with previous and
next contract snapshots, allowing multi-monitor or swapchain reconfiguration
problems to be reconstructed from JSONL without depending on trace log
retention. When a subsequent display issue is emitted, it should preserve the
preceding correlated refresh event so diagnostics can attribute the blocker to
the contract transition that introduced it. Those refresh snapshots must carry
capability-difference evidence too, including available formats, per-format
color-space support, present modes, alpha modes, and HDR tone-map headroom, so
JSONL reports can show not only that the contract changed but why it had to.
The window session also owns viewer GPU output telemetry next to the runtime:
each preparation attempt records whether it skipped because the external
texture was already current, media was loading, the preview was unavailable, the
display contract blocked presentation, wgpu recording failed, the output
texture was missing, or the external texture was registered. Successful and
rejected registrations accumulate the actual `RenderColorStageDiagnostics`
returned by input and output color-stage records, so logs and smoke tests can
prove the main preview path used CPU source upload + GPU OCIO input + GPU
working composite + GPU output transform rather than inferring it from model
state. The frame-residency payload distinguishes `GpuOcio`, `CpuOcio`, and
mixed input-transform paths; `GpuOcio` still means low-copy until media decode
exports real GPU hardware frames.
The same JSONL also records preview preparation timing
(`prepare_attempts_timed`, accumulated/max/last microseconds). These timings
measure the window scheduling + wgpu recording boundary, not decoder latency;
decode/cache telemetry remains in the preview/media diagnostics.
Successful native frames also attach the cumulative
`GpuViewerSpatialRuntimeDiagnostics` snapshot (pipeline builds, records,
prefilter/Lanczos passes, and output pixels) to the Viewer GPU JSONL report;
spatial execution must not be inferred only from an external texture success.
Headless smoke tests cannot create this window/session boundary, so
`PreviewProductionRuntime::diagnostics()` separately reports GPU preview candidate
requests, ready/current/loading/unavailable outcomes, candidate pixels, and
external-frame handoff counters. Those counters prove the service produced a
working-frame candidate for the window path; they do not replace the
window-session telemetry for actual wgpu output recording.
The self-hosted UI renderer has an external GPU texture plane for that preview
path. `DrawCommand::ExternalTexture` carries only a stable renderer-owned key,
bounds, UVs, and tint; widgets and panel models do not own wgpu objects.
`ViewerFrameContent` is the widget/app-model boundary and can carry either a
CPU `RasterImage` reference or a GPU `ViewerExternalTextureFrame` key.
Workspace redraw preserves GPU preparation before the dirty model refresh so a
native candidate does not first force the CPU Viewer adapter to composite the
same frame. Zoom, dock, and surface-lifecycle handlers synchronously rebuild or
lay out the active widget, so preparation can read their current physical-pixel
geometry. The following refresh makes the registered texture visible in the
same redraw. If another model refresh changes geometry afterward, the widget's
strict presentation identity rejects the stale draw and the next redraw
reschedules the spatial/display tail.
`ViewerExternalTexturePresentation` is the stable producer/consumer identity
for a spatially prepared frame. It includes the visible output pixel extent and
a quantized normalized source region. Layout changes clear the advertised
external frame before the preview service evaluates its `Current` state, and
the widget refuses to draw a spatial texture whose identity does not exactly
match its current geometry. The app transfers the spatial output resource into
the OCIO runtime's shared frame table using the same frame-ID allocator; it
does not copy the texture or create a second ID namespace.
`AppUiFrameRenderer::register_external_texture_view` exposes the renderer
registry to the app window/runtime layer, while `UiRenderer` owns the concrete
bind group for the current backend lifetime. The window unregisters the
previous viewer GPU texture key and clears obsolete output-frame resources when
recording a new preview output, so playback does not leak per-frame texture
bindings. Missing external keys render a visible diagnostic fallback while
incrementing `failed_external_textures`. Viewer preview GPU output registers
the final display-encoded texture through this plane instead of converting it
into a CPU `RasterImage` or placing full-frame video into the raster image
atlas.
`GpuColorFrameReadbackPlan` records the matching GPU-to-CPU boundary for final
encoded output. It aligns copied rows to wgpu's copy-buffer requirement and
unpacks padded mapped bytes into `CpuEncodedColorFrame`. Only
`Rgba8Unorm` / `EncodedRgba8` readback is defined here; float or half-float
targets require explicit conversion before readback.

`RenderColorTransformGpuPlanner` is the renderer color-boundary planner that
connects typed frame descriptors and `RenderInputTransform` /
`RenderColorTransform` requests to the OCIO shader cache. It produces a
descriptor-level GPU plan with the OCIO request, prepared wgpu resource
contract, native blockers, transform diagnostics, and explicit CPU
upload/readback boundary flags. This is the only place color-transform
scheduling should ask whether a GPU OCIO path is ready; CPU execution remains
the correctness executor until the plan reports no native blockers and the
renderer execution Session owns GPU-resident frame handles.

`RenderColorStagePlanner` is the scheduling layer above CPU and GPU color
executors. It emits an ordered `RenderColorStagePlan` containing CPU transform,
GPU color transform, upload, and readback nodes. Preview and export scheduling
should consume this stage plan instead of branching independently on CPU/GPU
state, so transfer cost and GPU blockers remain visible to diagnostics and
performance budgets. Once a planned GPU stage has concrete source/target
`GpuColorFrameHandle` values and a matching backend render-pass node,
`RenderGpuColorPassSchedule` validates those pieces, prepares the wrapper input
bind group, and records the fullscreen pass into the caller's command encoder.

## Allowed Readback

Readback is allowed for:

- final CPU encode paths until GPU encode exists
- thumbnails or cached previews that explicitly require CPU pixels
- tests/golden images
- debug/profiling captures
- interoperability with CPU-only plugins

Readback is not allowed as an invisible hop between ordinary render passes. All
readback callers must use `GpuColorFrameReadbackPlan`; ad hoc
`copy_texture_to_buffer` logic belongs in lower-level experiments only.

## Display and Export Boundary

Display owns swapchain/surface presentation. Export owns encode scheduling and file
output. Neither should reinterpret timeline state; both consume the same evaluated
render plan and color-management context.

The renderer-owned `RenderGpuOutputBoundaryRuntime` is the execution boundary for
native GPU final-output boundaries in both display and export paths. Export code
must call the shared runtime with an export boundary, then serialize explicit
`gpu_output_attempts`, `gpu_output_cpu_fallbacks`, and
`gpu_output_fallback_reasons` into its color report instead of hiding fallback as a
local "works well enough" path. A successful export path must not assume that a
scheduled GPU plan executed if the runtime returns no materialized readback handle or
no recorded stage/runtime evidence.

Exporting still reads back encoded pixels for encoder interoperability today, but
that readback must remain explicit in the plan and report. A future encoder path
may move readback behind an API that still reports parity or staged transfer
intent. Until that exists, readback reasons and blockers stay part of the
export color health contract.

## Viewer Resource Reuse

Viewer spatial prefilter, separable Lanczos, working composite, OCIO output,
fused signal-monitoring output, and optional ICC display-calibration output
textures share one device-scoped, exact-contract, byte-bounded resource pool.
Signal monitoring adds at most one active output texture, retains its pipeline
and uniform buffer, updates the uniform payload through `Queue::write_buffer`,
and performs no pass or texture allocation when every warning is disabled.
Renderer caches must use a security-supported `lru` dependency. Dependency
upgrades must preserve VRAM budgets, eviction order, and exact cache-key
semantics; renderer tests and `cargo deny` jointly guard that contract.
The working compositor also owns one fixed 128-slot, dynamically-offset uniform
arena. Per-pass uniforms use ordered `Queue::write_buffer` writes into that
persistent buffer and one persistent bind group; frame cleanup resets only the
slot cursor after submission, eliminating steady-state uniform-buffer/bind-group
creation without a device poll or CPU wait. Arena capacity, writes, high-water
mark, resets, and fail-closed exhaustion are exposed as runtime diagnostics and
persisted in both Window GPU-output JSONL and the existing headless performance
summary, so steady-state reuse regressions do not require a new stress harness.
Frame-local handles remain strongly typed and monotonic, while submitted texture
storage is returned to the pool without a CPU completion wait and reused only
through ordered queue semantics. Device reset first returns every stage's frame
resources and then clears the shared pool, preventing stale-device reuse.
