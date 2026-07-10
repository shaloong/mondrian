# GPU-Native Renderer

This document describes the target direction. The codebase is currently transitional: `FrameCompositor` and UI rendering are GPU-backed, but timeline compositing/effects still have CPU RGBA paths.

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
- render graph resource lifetime
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

## Native Decoded Frame Import

Hardware decode and renderer texture import are separate contracts. Media owns
the decoder fact (`DecodedGpuFrameHandleKind` and frame residency), platform
owns OS/backend capability discovery (`NativeVideoTextureImportProbe`), and
the renderer owns the graph contract for turning a decoder surface into a
linear working frame (`GpuNativeDecodedFrameImportPlan`). App code may schedule
or diagnose this path, but it must not translate OS decoder handles directly
into renderer resources.

Native decoded surfaces are not modeled as `GpuColorFrameHandle` values because
they may be multi-plane YCbCr surfaces such as NV12 or P010. The renderer import
contract records the decoder handle family, source texture format, source color
space, target working color space, required float working texture format, and a
`GpuNativeDecodedFrameVideoSampling` contract. That sampling contract is the
single place where the renderer learns limited/full range, YCbCr matrix,
transfer characteristic, effective bit depth, and chroma siting. When a
concrete backend reports readiness and support for that handle/format, the plan
allocates a renderer-owned linear `Working` frame handle; the imported decoder
surface remains a backend object consumed by the native sampling/input transform
pass.
Native YCbCr conversion and OCIO input conversion remain two explicit renderer
passes with one color contract. `GpuNativeYuvDecoder` samples the native luma
and chroma plane views into a renderer-owned `Rgba16Float` source frame whose
descriptor is `Source + EncodedFloat`; this frame is encoded RGB in the resolved
source color space, not linear working data. `RenderGpuInputStageResourcePlan`
then consumes that already GPU-resident frame without an upload and executes the
same OCIO source-to-working processor used by CPU-uploaded source frames. The
native import plan owns distinct encoded-source and linear-working handles so a
backend cannot skip, reorder, or mislabel either pass.

The YUV shader uses unfiltered `textureLoad` operations because NV12/P010 plane
formats are not assumed filterable. It performs renderer-defined bilinear 4:2:0
chroma reconstruction using explicit Left, Center, or TopLeft sample origins,
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
advertises D3D11Texture2D plus only the NV12/P010 formats enabled on the actual
wgpu device. A decoder reporting a GPU handle kind, or a platform probe reporting
a potentially importable OS family, is not enough by itself to claim hardware
decode playback, zero-copy, or low-copy frame residency.
Diagnostics should report the specific missing layer: decoder GPU handle absent,
platform import unsupported/missing, renderer backend not ready, unsupported
handle kind, unsupported source format, or unsupported working texture format.
Legacy DXVA2 and VDPAU can be FFmpeg CPU-transfer fallbacks, but they must not
be presented as the modern GPU-native renderer import path.

The app layer owns the combined readiness report because it is the first layer
that can see media decode facts, platform probes, and renderer backend support
together. `app_ui::native_video_import` evaluates those facts into a stable
viewer telemetry payload without giving media a renderer dependency or giving
the renderer a platform dependency. CPU-decoded frames remain
`CpuDecodedMedia`; retained D3D11 decoder surfaces can report `ReadyLowCopy`
only when platform probing, renderer support, sampling metadata, and actual
backend construction all agree.
On Windows, `mondrian-platform` performs lightweight D3D12 and D3D11 device
probes by loading `d3d12.dll`/`d3d11.dll` and calling
`D3D12CreateDevice`/`D3D11CreateDevice`. Successful results prove only that the
OS/device layer can support the `ID3D12Resource` and/or `ID3D11Texture2D`
handle families and a declared low-copy staging path; it still reports
zero-copy as unsupported until the renderer backend can import and sample the
decoder surface directly.
Media may report a platform-preferred hardware decode candidate such as
D3D12VA, D3D11VA, VideoToolbox, or VA-API plus expected NV12/P010 surface
formats, but a candidate is not renderer readiness. Windows candidates must be
ordered D3D12VA, D3D11VA, then legacy DXVA2; Linux candidates must be ordered
VA-API, then legacy VDPAU. Runtime FFmpeg/codec/device failure may fall through
to the next backend. Windows support becomes ready only after
`D3D11Dx12NativeVideoImportBackend` binds the active adapter/device/queue;
unimplemented platform backends remain unavailable.
Renderer and product-window device creation request the adapter-supported subset
of wgpu `TEXTURE_FORMAT_NV12` and `TEXTURE_FORMAT_P010` through the shared
`native_video_texture_device_features` contract. Enabling those features is only
a texture-format prerequisite: it does not prove that a decoder resource can be
shared, synchronized, adopted by the active wgpu device, sampled, or transformed.
Readiness therefore remains fail-closed until backend construction validates
the complete platform import bridge. Diagnostics distinguish missing device
format features, non-DX12 adapters, and backend construction failures.
The Windows renderer backend owns D3D11 source admission. Before any resource
sharing, it verifies the retained FFmpeg texture ABI, actual DXGI NV12/P010
format, visible-versus-storage extent, array-slice bounds, single mip/sample
layout, and exact adapter LUID equality with the active wgpu DX12 adapter.
Codec-aligned storage dimensions may exceed the visible frame; smaller storage
is invalid. App, core, and generic platform probes must not duplicate or weaken
these renderer resource invariants.
Validated D3D11 sources can enter a reusable low-copy bridge entry. Each entry
owns a single-slice NV12/P010 texture created with the Windows NT-handle sharing
contract, a D3D11/D3D12 shared timeline fence, two reusable DX12 barrier command
lists, and one wgpu multi-plane texture with explicit luma/chroma views. D3D11
waits for the prior renderer completion before overwriting, copies the decoder
array slice, and signals `copy_ready`; DX12 waits, transitions `COMMON ->
RESOURCE`, and only then exposes plane views. The renderer submit is followed by
`RESOURCE -> COMMON` and `renderer_complete`. Fence values are strictly
monotonic, command allocators are reset only after completion, busy entries fail
without a CPU wait, and any partially submitted failure permanently poisons the
entry. The complete import backend pools entries by source device, storage,
color, and sampling contract; it grows the pool for bounded in-flight work,
returns busy at the configured limit, and evicts poisoned entries before reuse.
The bridge never relies on `Flush`, implicit sRGB, or an undocumented
resource-state assumption. A real-GPU ignored smoke test exercises NT-handle
creation/opening, both API devices on the same adapter, fence transfer, resource
barriers, wgpu adoption, and completion.
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
sampling facts through both preview source contracts. `AppUiGpuPreviewMediaSource`
carries CPU RGBA source pixels plus decoder diagnostics for the GPU OCIO upload
path; `AppUiGpuPreviewNativeSource` carries a native decoder surface contract
without CPU pixels. It retains the complete media-owned
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
readiness can report zero-copy. Frame residency diagnostics can therefore
distinguish CPU-decoded media, native GPU-decoded media, native media blocked
by missing sampling facts, mixed CPU/native stacks, and procedural GPU-native
content across the concrete Windows backend and still-unimplemented
VideoToolbox/VA-API import adapters.
The app window owns `AppUiNativeVideoImportRuntime` alongside, but independently
from, the swapchain UI renderer. On Windows it constructs the concrete renderer
backend from the active adapter/device/queue and publishes that backend's support
contract; construction failure remains unavailable with its exact reason. The
runtime survives surface-format/UI-renderer rebuilds so display changes do not
discard decoder bridge pools. It allocates native import frame ids from the same
`RenderGpuOutputBoundaryRuntime` namespace that will receive the returned
working resources, preventing resource-table id collisions.
Renderer/platform readiness must remain in
`AppUiPreviewHardwareDecodeAdmissionDiagnostics`. It must not be copied into
media `HwAccelProbe`, `PreviewDecodeDiagnostics`, hardware-decode decisions, or
media blocker enums. Media reports whether decode produced a retained native
surface; the app separately reports whether renderer and platform capabilities
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
The first concrete media lease is `FfmpegNativeDecodedFrameResource` for the
preferred FFmpeg D3D11 ABI. It retains the source `AVFrame`/`AVBufferRef` and
exposes a borrowed `ID3D11Texture2D` pointer plus array slice only to the
matching renderer import backend. Media can now return this payload as
`InProcessFfmpegNative` after app admission; this does not make renderer support
ready. `AppUiFrameRenderer` must continue reporting native import unavailable
until its D3D11 bridge can import and sample that exact lease into the planned
float working-space resource.
The shared
`execute_native_decoded_frame_import(...)` helper owns support validation,
working-frame plan creation, backend invocation, and returned-resource contract
verification. Platform/window/app code must not fabricate
`GpuColorFrameResource` entries directly from decoder handles; a real D3D12,
D3D11, VideoToolbox, VA-API, or CUDA adapter must return the exact planned float
working frame or fail with a structured backend error.

## Effect Integration

`mondrian-effects` lowers supported compiled graphs to `CompiledEffectGpuPlan`,
a graphics-API-neutral contract. The preview scheduler carries that plan and
the timeline frame seed across the app/window boundary; it does not interpret
effect math or own wgpu resources. `GpuFrameCompositor` consumes the plan in its
`Rgba32Float` working-space pass, applying the fused point operations before
straight-alpha composition. The retired RGBA8 global executor/upload/readback
path must not be reintroduced.

The first native subset is a bounded single-source chain of ColorAdjust,
WhiteBalance, Vignette, and deterministic Grain. Unsupported topology, spatial
sampling, LUT resources, and custom operations remain explicit lowering
blockers until dedicated renderer graph passes provide their resource and alpha
contracts.

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
the render graph records a native pass.

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
`RenderGpuOutputBoundaryRuntime` is the app/export owner for this path's
long-lived renderer state. It combines the OCIO shader cache, pure backend prep
runtime, concrete backend-object runtime, GPU color frame id allocator, and
`GpuColorFrameResourceTable<GpuColorFrameWgpuResource>` so callers do not split
those contracts across unrelated services.
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
`mondrian-renderer`. This smoke proves renderer-side upload + native GPU OCIO + readback sequencing; it does not prove OS
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
transform was CPU OCIO or GPU-native; upload/readback counts; and whether the
frame is truly zero-copy/low-copy. The same
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
`AppUiPreviewService` resolves the timeline and composites a working-space
`CpuColorFrame`, while the app window validates the requested display boundary
against that contract and records the display/output boundary through the
session-owned GPU runtime. Unsupported presentation requests, such as HDR output
on an SDR-only surface, DCI-P3 output on an sRGB surface, Rec.2020 SDR output
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
inside CPU media workers.
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
Headless smoke tests cannot create this window/session boundary, so
`AppUiPreviewService::diagnostics()` separately reports GPU preview candidate
requests, ready/current/loading/unavailable outcomes, candidate pixels, and
external-frame handoff counters. Those counters prove the service produced a
working-frame candidate for the window path; they do not replace the
window-session telemetry for actual wgpu output recording.
The self-hosted UI renderer has an external GPU texture plane for that preview
path. `DrawCommand::ExternalTexture` carries only a stable renderer-owned key,
bounds, UVs, and tint; widgets and panel models do not own wgpu objects.
`ViewerFrameContent` is the widget/app-model boundary and can carry either a
CPU `RasterImage` reference or a GPU `ViewerExternalTextureFrame` key.
Workspace redraw treats GPU preview preparation as a pre-refresh scheduling
step: `prepare_viewer_gpu_preview()` runs before the dirty UI tree is refreshed
for painting. This prevents a new playback frame from first generating a CPU
raster preview merely because the external GPU texture for that frame has not
yet been registered. If GPU preparation fails or is unavailable, the following
UI refresh still reaches the normal raster/stale/loading fallback in the same
redraw.
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
render graph owns GPU-resident frame handles.

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
