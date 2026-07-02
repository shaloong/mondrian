# Color Management

Mondrian has one color-management pipeline. Mondrian Standard and Custom OCIO
both resolve color transforms through OCIO config / processor. The bundled
Mondrian default OCIO config is required for Standard mode; missing config or
processor failures must surface as errors instead of falling back to another
color science.

The Rust integration is `ocio-rs` 0.2.x with the `bundled` feature enabled, so
normal application builds exercise the real OpenColorIO bridge rather than a
stub runtime.

The Standard mode config is packaged as
`crates/mondrian-core/assets/ocio/mondrian_default_ocio_v1.ocio` and loaded via
`include_str!` as `embedded:mondrian_default_ocio_v1`. It is pinned to the OCIO
ACES 2.0 studio config semantics instead of resolving an upstream `latest`
alias at runtime. Product builds therefore have a deterministic default color
science while still using real OCIO processors.

## Engines

- `ColorEngine::MondrianSmart`: productized Standard/Simple policy over the
  Mondrian default OCIO source.
- `ColorEngine::Ocio`: explicit OCIO mode over a selected environment,
  built-in, or path source.

Explicit OCIO mode must load its selected config successfully. It must not
silently fall back to a different color science. Mondrian Standard follows the
same rule for the embedded `mondrian_default_ocio_v1` asset.

## Project and Sequence

`ProjectColorManagement` stores the project-level engine. `SequenceColorManagement` can inherit from the project or override its own engine and policies.

Important fields:

- `workflow`: DisplayReferred, SceneReferred, Aces
- `missing_metadata_policy`
- `nested_processing`
- `output_color_space`
- `video_range`
- `export_bit_depth`
- HDR metadata preservation fields

## Working Space

`SequenceSettings.color_space` is the timeline working color space. Media input transforms resolve from clip/media interpretation into this working space. Output/display transforms convert from working/output context to the destination.

`ColorPipeline` execution is source -> working -> output. The management engine
dispatch must preserve that full chain; it must not collapse a timeline pipeline
to source -> output when a sequence working space is available.

OCIO execution also follows source -> working -> output. The Standard mode UI
can hide OCIO details from normal users, but the backend still routes through
the Mondrian default OCIO source and fails closed when that source is missing.

Renderer stages must carry typed color-frame metadata. `CpuColorFrame` is the
CPU-resident linear working-frame contract; future GPU frames must expose the
same domain/encoding/residency/color-space descriptor. RGBA8 is a boundary
format, not an intermediate color-management contract.

GPU-resident color frames use `GpuColorFrameHandle`, a renderer resource-table
handle with the same `ColorFrameDescriptor` contract. CPU/GPU transfers are
scheduled explicitly by `RenderColorStagePlan` nodes rather than hidden inside
color conversion helpers.
When a GPU OCIO stage has no upload/readback requirements and no native
blockers, `RenderGpuColorPassSchedule` binds the source `GpuColorFrameHandle`,
target `GpuColorFrameHandle`, `RenderColorTransformGpuPlan`, and
`OcioGpuWgpuRenderPassNodePlan` into a schedulable unit. It fails closed if the
frame descriptors, residency, extents, or OCIO resource keys differ.

Preview and export final transforms are renderer execution concerns. App and
export crates build a `RenderColorTransform` from their `ColorContext` and pass
typed frames to renderer stage execution helpers; they must not duplicate
working -> output conversion logic locally.

Input transforms follow the same rule. Decode/import code wraps source pixels in
`CpuEncodedColorFrame::source_rgba8`, builds a `RenderInputTransform`, and asks
the renderer stage executor for a `CpuColorFrame` in timeline working space.
App and export crates must not perform source -> working color conversion with
local `ColorPipeline` calls or direct low-level transform executor calls.

Renderer color-transform executors provide detailed diagnostics for both input
and output boundaries: backend, direction, typed input/output descriptors,
pixel count, and whether a temporary RGBA8 CPU boundary was crossed. Product
preview/export diagnostics should aggregate these records instead of inferring
color workload from UI or encoder code.

Renderer color-stage diagnostics sit one layer higher than transform
diagnostics. A `RenderColorStagePlan` records the scheduled CPU, GPU, upload,
and readback stages for a color boundary; `RenderColorStageExecution<T>` carries
the transform result plus that stage summary. Preview and export performance
smokes should report both layers: transform diagnostics prove color semantics,
while stage diagnostics prove scheduling, residency, and GPU-readiness.

Mondrian's `ColorSpace` enum maps to pinned OCIO color-space names in the
default config. The mapping is tested for every enum variant, and representative
delivery, HDR, and camera-log processor pairs must create real CPU processors.
Custom OCIO configs should provide the same names or aliases if they are used
with Mondrian's built-in `ColorSpace` enum.

## Color Encoding Contract

`ColorSpace::encoding()` is the canonical metadata source for color
primaries, transfer characteristic, matrix coefficients, and whether the space
is display SDR, display HDR, or camera log. Conversion code, preview
diagnostics, validation, and export tagging should read this contract instead
of maintaining separate ad hoc mappings.

`ColorSpace::ffmpeg_tags()` is derived from that contract and only returns tags
for standardized delivery/monitoring spaces. Camera-log acquisition spaces such
as Apple Log, S-Log3, and ARRI LogC4 currently do not emit FFmpeg delivery tags,
because writing guessed Rec.709 tags would mislabel the exported media.

Camera-log output is treated as a professional intermediate path. Export
validation rejects consumer delivery codecs for camera-log output and only
allows 10-bit-or-higher MOV/MXF ProRes configurations until richer metadata
carriage is implemented.

## Unknown Media

When detected media color space is missing, `MissingColorMetadataPolicy` resolves it as:

- Assume Rec.709
- Assume sequence working space
- Reject media

Clip-level `MediaInterpretation.color_space_override` takes precedence over detected metadata.

## Display and Export

Display transforms belong at preview presentation. Export transforms belong at export encoding/tagging. Do not bake display transforms into timeline source data.

`SequenceSettings::root_preview_color_context(...)` builds the monitor
presentation context with a caller-provided display/output color space.
`SequenceSettings::root_export_color_context(...)` builds the delivery context
from the sequence output color space. Callers must choose one of these explicit
entry points instead of using a generic root render context.

For `MondrianSmart` and explicit `Ocio` engines, root sequence contexts copy
the currently loaded OCIO config's default display/view into the context when
one is available. Absence of a display/view is only valid when no current OCIO
config exposes defaults; it is not a fallback color pipeline.

GPU preview should use OCIO shader extraction instead of CPU processor execution
for real-time playback. `mondrian-core::extract_ocio_gpu_shader_bundle` and
`mondrian-core::extract_ocio_display_gpu_shader_bundle` are the core extraction
boundaries: they return OCIO-generated shader text plus texture/uniform counts
and the processor cache id. `mondrian-renderer::OcioGpuShaderCache` stores this
as a renderer shader plan keyed by the request and OCIO processor cache id.
`mondrian-renderer::RenderColorTransformGpuPlanner` is the color-transform
scheduling boundary that turns typed frame descriptors plus renderer color
transforms into OCIO GPU shader plans with explicit resource contracts,
blockers, and upload/readback requirements.
`mondrian-renderer::RenderColorStagePlanner` wraps that boundary with ordered
CPU/GPU/upload/readback stages so preview and export can share scheduling
semantics while still choosing different output residency.

This cache is deliberately not a fake wgpu execution path. OCIO emits backend
shader source such as GLSL/HLSL/MSL, while Mondrian's native renderer should
prefer validated Naga IR via `wgpu::ShaderSource::Naga` once translation is
proven. A later backend compiler/upload stage must turn the cached OCIO plan,
`OcioGpuShaderTranslationCache` artifact, `OcioGpuWgpuResourcePlan`,
`OcioGpuWgpuLutUploadPlan`, packed/uploaded LUT textures,
`OcioGpuWgpuUniformUploadPlan`, packed/uploaded uniform buffers, and
`OcioGpuWgpuShaderModuleCache` output into validated
`OcioGpuWgpuBindResourcePlan` entries and
`OcioGpuWgpuBindGroupLayoutDescriptorPlan` descriptors, then through
`OcioGpuWgpuBindGroupPreparer` into concrete bind groups,
`OcioGpuWgpuPipelineLayoutPlan`, `OcioGpuGeneratedProgramContract`,
`OcioGpuWgpuWrapperLinkPlan`, `OcioGpuWgpuFullscreenShaderContract`, and
`OcioGpuWgpuWrapperShaderSourceArtifact` into
`OcioGpuWgpuRenderPipelineDescriptorPlan` and
`OcioGpuWgpuWrapperShaderModuleArtifactCache`, then concrete wrapper shader
modules through `OcioGpuWgpuWrapperShaderModuleCache`, and the fullscreen
pipeline through `OcioGpuWgpuRenderPipelineCache`, then through
`OcioGpuWgpuRenderPassNodePlan` and `OcioGpuWgpuRenderPassRecorder` before the
preview graph can execute it on the GPU. `RenderGpuColorPassSchedule` now owns
the preview/export-facing recording boundary: it validates source/target
`GpuColorFrameHandle` values, derives the wrapper input bind group from the
OCIO wrapper contract, turns the scheduled output handle into a render-pass
target, and calls the shared recorder. The wrapper source artifact is
stage-split; its combined source is diagnostic text, not the canonical
execution artifact. Mondrian validates/owns stage-split Naga IR for the wrapper
and keeps generated WGSL as diagnostics only. Preview/export frame evaluation
must resolve GPU frame handles through `GpuColorFrameResourceTable`; the table
revalidates descriptor and texture-format contracts for every lookup before
exposing backend resources such as `GpuColorFrameWgpuResource`.
`RenderGpuColorPassSchedule::record_wgpu_from_resources` is the single
preview/export-facing entry point that resolves those table entries, prepares
the wrapper bind group, and records the pass. `GpuColorFrameAllocationPlan`,
`GpuColorFrameUploadPlan`, and `GpuColorFrameUploader` are the shared renderer
entry points for allocating render targets and uploading CPU boundary frames
into `GpuColorFrameResourceTable`. `RenderGpuOutputStageResourcePlan` maps a
validated `RenderColorStagePlan` into those upload/allocation plans and a
transfer-resolved GPU transform that can be scheduled without callers mutating
transform internals. Until preview/export frame evaluation calls these entries
and then records through `record_wgpu_from_resources`, CPU processor execution
is the correctness path and the cached shader/module/upload/bind-resource/
layout/bind-group/wrapper-link/pipeline-contract plans are the production
boundary for GPU integration work.

Preview and export may therefore target different output color spaces while
sharing the same working color space, engine inheritance, workflow,
missing-metadata policy, and nested-processing policy.

## HDR/SDR

HDR output spaces include Rec.2100 PQ/HLG. Tone mapping is required when scene/HDR working data targets SDR output. HDR metadata can only be preserved for HDR output spaces.
