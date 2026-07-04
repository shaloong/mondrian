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

## Effect Integration

`mondrian-effects` already exposes `EffectGpuExecutor` as an acceleration hook. Long-term, effects should compile to graph nodes that the renderer can execute on GPU where supported, with CPU fallback only for unsupported ops/plugins.

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
record for perf dashboards. The JSON includes a `health` summary derived from
the raw stage/runtime diagnostics: native GPU output readiness, upload + GPU
OCIO + readback stage completeness, blocker-free execution, shader/backend
runtime readiness, readback byte completeness, expected readback bytes, and
CPU/GPU parity within tolerance. This smoke proves renderer-side upload +
native GPU OCIO + readback sequencing; it does not prove OS
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
color space, tone-map flag, and optional OCIO display/view. This gives
playback/scrubbing sessions a persistent health stream that can be correlated
back to specific frames in addition to trace logs. The same session also owns
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
The window session also owns viewer GPU output telemetry next to the runtime:
each preparation attempt records whether it skipped because the external
texture was already current, media was loading, the preview was unavailable, the
display contract blocked presentation, wgpu recording failed, the output
texture was missing, or the external texture was registered. Successful and
rejected registrations accumulate the actual `RenderColorStageDiagnostics`
returned by `RenderGpuOutputStageRecord`, so logs and smoke tests can prove the
main preview path used upload + native GPU color + optional readback rather than
inferring it from model state.
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

Display owns swapchain/surface presentation. Export owns encode scheduling and file output. Neither should reinterpret timeline state; both consume the same evaluated render plan and color-management context.
