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
- CPU RGBA should not be the default exchange type between renderer stages.

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
