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

This is the stable boundary before native GPU execution. The cache must not
claim wgpu execution until a backend compiler/upload layer creates real pipeline
resources, bind groups, LUT textures, and render-graph nodes from the OCIO plan.
Renderer diagnostics should track cache hits, misses, and extraction failures so
preview performance work can distinguish shader planning cost from actual frame
execution cost.

`OcioGpuShaderCache::prepare_wgpu_execution(...)` reports native execution
blockers explicitly, including shader-language translation, LUT texture upload,
and uniform packing. Render scheduling must treat those blockers as diagnostics,
not as silent fallback.

The same preparation step also returns an `OcioGpuWgpuResourcePlan`. This plan
is the renderer contract for the future native pass: the OCIO binding contract
reported by the GPU shader descriptor, the separate Mondrian fullscreen wrapper
contract for input sampling/output location, input/output frame resource counts,
LUT texture counts, uniform buffers, samplers, bind group entries, bind groups,
and stable resource/pipeline-layout hashes.
It is not a fake resource allocation layer; concrete `wgpu::ShaderModule`,
`wgpu::Texture`, `wgpu::BindGroupLayout`, `wgpu::BindGroup`, and pipeline
objects must still be created by the backend compiler/upload layer before
`can_execute()` can become true.

`OcioGpuWgpuResourceCache` prepares and caches the backend binding-layout
contract derived from that resource plan. OCIO bind-group entries use the
descriptor set, uniform-buffer binding, texture binding start, and LUT binding
indices reported by OCIO rather than inferred WGSL text or count-based slot
assignment. Mondrian wrapper resources stay in a separate wrapper contract.
This cache reports hits/misses independently from OCIO shader extraction. It is
the handoff point for the future object-creation layer; it still does not
allocate `wgpu` objects or bypass native blockers.

`OcioGpuShaderTranslationCache` is the next boundary toward native execution.
It parses OCIO GLSL-family shader text through Naga, validates the module, and
caches successful Naga IR artifacts by source shader hash, OCIO binding-contract
hash, stage, source language, and target language. Optional WGSL is emitted only
as debug/diagnostic output; it is not the canonical execution artifact. Future
execution should prefer `wgpu::ShaderSource::Naga` while translation failures
remain structured diagnostics, not fallbacks. This allows real OCIO shader
compatibility work to proceed incrementally while keeping `can_execute()` false
until shader translation, LUT upload, uniform packing, bind-group creation, and
pipeline creation are all proven.

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
interpolation-aware `wgpu::Sampler` objects from that packed plan, but upload
success alone still does not make the color pass executable.

`OcioGpuWgpuShaderModuleCache` is the first concrete backend-object boundary.
It validates that an `OcioGpuTranslatedShader` and `OcioGpuWgpuResourcePlan`
share the same shader hash and expanded OCIO binding contract before creating a
`wgpu::ShaderModule` with `wgpu::ShaderSource::Naga`. It deliberately does not
create bind groups, upload textures, pack uniforms, or create the final
fullscreen render pipeline; those remain explicit native blockers until the
resource upload and wrapper shader contract are implemented and verified.

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
performance budgets.

## Allowed Readback

Readback is allowed for:

- final CPU encode paths until GPU encode exists
- thumbnails or cached previews that explicitly require CPU pixels
- tests/golden images
- debug/profiling captures
- interoperability with CPU-only plugins

Readback is not allowed as an invisible hop between ordinary render passes.

## Display and Export Boundary

Display owns swapchain/surface presentation. Export owns encode scheduling and file output. Neither should reinterpret timeline state; both consume the same evaluated render plan and color-management context.
