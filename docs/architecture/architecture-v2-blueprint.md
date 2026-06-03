# Mondrian Architecture V2 — Target Blueprint

**Status:** Substantially complete (93% conformity)
**Branch:** `develop` (merged: `feat/architecture-v2-dag-gpu`, `feat/gpu-texture-sharing`, `fix/effects-tests`)
**Date:** 2026-06-03

---

## Ideal Long-Term Architecture

### Overall Data Flow

```
Asset System
      ↓
Timeline / Sequence (time organization only, no effect logic)
      ↓
Clip Graph (each Clip = evaluable node)
      ↓
Effect DAG (true effect system, node graph)
      ↓
Render Graph (GPU render graph, compiled & optimized)
      ↓
GPU Backend (wgpu: Vulkan / Metal / DX12 / WebGPU)
      ↓
Preview / Export
      ↓
FFmpeg Encode (I/O codec layer only, NOT core rendering)
```

---

## Layer Responsibilities

### 1. Asset System

- Manages video, image, audio, LUT, mask, generated content (solid color, gradient, procedural), AI assets.
- `AssetSource` enum: `File`, `Generated`, `Procedural`, `Remote`.
- Asset ≠ file path. An asset is a typed data source, not a URI.

```rust
pub enum AssetSource {
    File(PathBuf),
    Generated(GeneratedKind),
    Procedural(ProceduralSpec),
    Remote(Url),
}

pub enum GeneratedKind {
    SolidColor(Color),
    AdjustmentLayer,
    Gradient(GradientSpec),
    Noise(NoiseSpec),
}

pub trait ProceduralSpec: Send + Sync {
    fn resolution(&self) -> Resolution;
    fn pixel_at(&self, x: u32, y: u32, time: TimeCode, seed: i64) -> Color;
}
```

### 2. Timeline / Sequence

- **ONLY responsible for time organization**: clip placement, trimming, splitting, transition timing, track relationships, time remapping, nesting, playback logic.
- **MUST NOT contain**: effect logic, shaders, compositing methods, or direct calls to any rendering crate.
- Communicates with the effect system via a trait boundary defined in `mondrian-core`.

```rust
/// Defined in mondrian-core. Implemented by mondrian-effects.
pub trait EffectGraphEvaluator: Send + Sync {
    fn evaluate_graph(
        &self,
        effects: &[EffectNodeData],
        masks: &[MaskComponentData],
        time: TimeCode,
    ) -> EffectGraphHandle;
}

/// Opaque handle — timeline stores it, never inspects.
pub struct EffectGraphHandle(Box<dyn std::any::Any + Send + Sync>);
```

### 3. Clip Graph

- Each Clip is a local render graph. Example:

```
Media Source → Effects → Masks → Blend
```

- A Clip is not a simple video segment — it is an **evaluable node**, unifying:
  - Regular media clips
  - Adjustment layers
  - Text layers
  - Particle generators
  - Nested compositions
  - Solid color generators

```rust
pub trait ClipGraphNode: Send + Sync {
    fn clip_id(&self) -> ClipId;
    fn source_time(&self, timeline_time: TimeCode) -> TimeCode;
    fn build_source(&self, ctx: &ClipGraphContext) -> EffectGraphNode;
    fn transform_matrix(&self, time: TimeCode) -> Mat3;
    fn opacity(&self, time: TimeCode) -> f32;
    fn blend_mode(&self) -> BlendMode;
}
```

### 4. Effect DAG (Core)

- The true visual graph: nodes with multi-input, multi-output, branching, merging, mask routing, shared sources.
- Each node: `inputs: Vec<InputPort>`, `outputs: Vec<OutputPort>`, `params: ParamSet`.
- **Reject linear effect stacks**. Must be a directed acyclic graph.

```rust
pub struct EffectNode {
    pub id: EffectNodeId,
    pub kind: EffectNodeKind,
    pub params: ParamSet,
}

pub enum EffectNodeKind {
    Source,
    UnaryEffect { input: PortId, op: EffectRenderOp },
    Blend { inputs: [PortId; 2], blend_mode: BlendMode },
    Mask { input: PortId, mask: PortId, mask_op: MaskOp },
    MultiInput { inputs: Vec<PortId>, combiner: CombinerOp },
    Output { input: PortId },
}
```

### 5. Render Graph

- The Effect DAG compiles into a GPU Render Graph (Pass Graph).
- Supports: pass fusion, cache reuse, tiled rendering, async compute, partial redraw.
- Render Graph IR is backend-agnostic (wgpu is the concrete backend).

```rust
pub struct RenderGraph {
    pub passes: Vec<RenderPass>,
    pub resources: ResourcePool,
    pub dependencies: Vec<PassDependency>,
}

pub enum RenderPass {
    Compute(ComputePassDesc),
    Composite(CompositePassDesc),
    Readback(ReadbackPassDesc),
}
```

### 6. GPU Backend

- Unified `wgpu` for all image processing, procedural rendering, and simulation.
- FFmpeg serves ONLY as I/O codec layer (decode / encode), NOT for core compositing or effects.

### 7. UI Principles

- The ground truth is the **node graph**. The UI may provide multiple views:
  - Linear projection view (Premiere-like)
  - Node graph view (Nuke / DaVinci Fusion-like)
- These are **NOT two independent systems** — they are projections of the same underlying graph.
- Selection state is maintained ONCE in `AppState`, consumed by all panels.

### 8. Core Primitive Philosophy

- NEVER hardcode an effect as a monolithic feature.
- Define a small set of atomic operations: Transform, Blur, Blend, Noise, Warp, Mask, Particle.
- Complex effects are compositions of primitives.
- This naturally enables: AI nodes, material graphs, audio-reactive graphs, script-generated graphs.

---

## Migration Target State by Crate

| Crate | V2 Role |
|-------|---------|
| `mondrian-core` | Shared types + `ClipGraphNode` trait + `EffectGraphEvaluator` trait + `RenderGraph` IR + `AssetSource` |
| `mondrian-timeline` | Sequence / Track / Clip data model. Depends ONLY on `mondrian-core`. Holds `Arc<dyn EffectGraphEvaluator>`. |
| `mondrian-effects` | Implements `EffectGraphEvaluator`. DAG compilation, scheduling, caching. GPU + CPU execution paths. |
| `mondrian-renderer` | wgpu `RenderGraph` compile & execute. Texture pool, pass fusion, GPU resource scheduling. Depends on `mondrian-core` + `mondrian-effects`. |
| `mondrian-media` | FFmpeg decode + audio mix. GPU YUV→RGB conversion. Cache tiers. Depends on `mondrian-core`. |
| `mondrian-export` | Consumes `RenderGraph`, feeds frames to FFmpeg encoder. |
| `mondrian-assets` | Asset library with `AssetSource` enum. SQLite-backed. |
| `mondrian-ai` | AI orchestrator. No change in V2. |
| `mondrian-app` | egui UI. Single selection state. Timeline + Node Graph views as graph projections. |
