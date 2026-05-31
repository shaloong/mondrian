# Architecture V2 — Gap Analysis

**Date:** 2026-05-31 (updated after Phase 0-3)
**Assessment:** Code health **Good**, architecture conformity **70%** vs. [V2 Blueprint](architecture-v2-blueprint.md).

---

## 1. Current State Summary

### Strengths

- 9 crates in strict DAG dependency order (no cycles)
- Strong typing: newtype UUIDs, `Rational`/`TimeCode` (frame-exact, no floats), 28 `BlendMode` variants
- Event-driven via `EventBus` (crossbeam broadcast pub/sub)
- Command pattern undo/redo (`CommandHistory`, max 200 entries)
- Unified keyframe system (`PropertyBag` / `AnimatedProperty`, Bezier interpolation)
- **Effect system is already a true DAG** — topological sort, dual-input nodes (`Blend`, `Mask`), sub-graph caching
- 281 unit tests

### Weaknesses

- 16 files >1000 lines (worst: `viewer_panel.rs` 6081, `effect_controls_panel.rs` 5907)
- Zero integration tests
- Two crates with zero test coverage (`mondrian-ai`, `mondrian-assets`)
- 12+ global mutable singletons (`OnceLock<Mutex<>>`)
- 513 `.clone()` calls suggest ownership model issues
- 3 dead WGSL shaders written but never used

---

## 2. Architecture Conformity Matrix

| Ideal Layer | Current Implementation | Gap Level | Specific Gaps |
|-------------|----------------------|-----------|---------------|
| **Asset System** | `mondrian-assets` with SQLite. `AssetKind` has 4 variants. No `AssetSource` enum — uses `mondrian://` URI scheme hack. | **Medium** | Missing `AssetSource` (File/Generated/Procedural/Remote); `path: PathBuf` carries source semantics implicitly; no procedural generator framework |
| **Timeline** | `mondrian-timeline`. Correctly models Sequence→Track→Clip hierarchy and time organization. BUT directly imports from `mondrian-effects`. | **Medium** | **P-ARCH1 violation:** `Clip::evaluate_compiled_effect_graph()` calls `mondrian_effects::build_effect_render_graph` directly ([clip.rs:12](crates/mondrian-timeline/src/clip.rs#L12)). Undo uses full-Sequence snapshots only. |
| **Clip Graph** | **Does not exist.** Clip is a data container. Different `ClipKind` variants handled via if-else branches in the render plan builder. | **High** | No `ClipGraphNode` trait; no unified evaluable-node abstraction; Adjustment Layer is a special case in the compositor, not a graph node |
| **Effect DAG** | `mondrian-effects`. **Surprisingly close to ideal.** `EffectGraphNodeKind::{Source, UnaryEffect, Blend, Mask, MaskSource}` with Kahn's algorithm topological sort. Dual-input nodes. Two-tier LRU cache (graph-level + per-node). Plugin SDK. | **Medium** | Execution is 100% CPU; legacy `EffectRenderPlan` (flat list) still exists as primary API; max 2 inputs per node (no N-port); no GPU compute nodes |
| **Render Graph** | **Does not exist.** Iterative for-loop compositor in `timeline_composite.rs`. Each layer is a separate `queue.submit()`. No pass fusion. No tiled rendering. No GPU resource scheduling. | **High** | No `RenderGraph` IR; no pass compiler; no GPU resource pool; every frame does GPU readback; per-layer submit penalizes multi-layer timelines |
| **GPU Backend** | Minimal wgpu usage. Single `composite.wgsl` shader for Porter-Duff over. GPU is an optional accelerator with fallback to CPU after 4 consecutive failures. | **High** | All effects are CPU pixel ops; 3 shaders written but unused (`lut3d.wgsl`, `blur_gaussian.wgsl`, `yuv_to_rgb.wgsl`); no compute shader dispatch infrastructure |
| **UI — Unified Graph View** | Linear timeline view only. `AnimationBubbleHost::Graph` variant reserved but unimplemented. Selection state duplicated across 3 locations. | **Medium** | Three copies of clip selection; no node graph panel; timeline view operates directly on Sequence data rather than reading a graph projection |

---

## 3. P0-P4 Code Smells (Full List)

### P0 — Correctness & Safety (Fix Before Any Refactor)

| # | File | Line | Issue | Fix |
|---|------|------|-------|-----|
| P0-1 | `media/src/preview.rs` | 86 | `static PREVIEW_DECODE_SESSION: RefCell<...>` — not `Sync`, multi-thread panic risk | `Mutex<Option<...>>` |
| P0-2 | `app/src/ui/viewer_panel.rs` | 4602 | `static LAYER_DECODE_RUNTIME: RefCell<...>` — same issue | `OnceLock<Runtime>` |
| P0-3 | `media/src/cache.rs` | 65 | `NonZeroUsize::new(config.max_frames).unwrap()` — panics if 0 | `max(1)` |
| P0-4 | `app/src/ui/viewer_panel.rs` | 1022 | `self.mask_tool.unwrap()` — panics if None | `if let` or `match` |
| P0-5 | `app/src/ui/effect_controls_panel.rs` | 340 | `.partial_cmp(...).unwrap()` — NaN returns None | `unwrap_or(Ordering::Equal)` |
| P0-6 | `effects/src/effect.rs` | 473,481,487,507 | `.expect("poisoned")` on RwLock — crashes app on poison | Recover or log-and-reset |
| P0-7 | `export/src/queue.rs` | 1291 | `.expect("failed to spawn export worker")` — crashes app | Graceful error to UI |
| P0-8 | Global | — | Zero integration tests | Add `tests/integration_tests.rs` |

### P1 — Coupling

| # | Files | Issue | Fix |
|---|-------|-------|-----|
| P1-1 | `timeline/Cargo.toml:10`, `timeline/src/clip.rs:12` | Timeline depends on Effects crate | `EffectGraphEvaluator` trait in core |
| P1-2 | `renderer/Cargo.toml:12`, `renderer/src/timeline_render_plan.rs:3-4` | Renderer depends on Timeline crate | Lift `TimelineRenderPlanElement` to core |
| P1-3 | `app/src/app.rs:540`, `app/src/ui/viewer_panel.rs:386`, `app/src/ui/timeline_panel.rs:34` | Selection state in 3 places | Single `AppState.selection_state` |
| P1-4 | `core/src/ocio.rs:17` | Global OCIO config prevents per-project settings | Inject into `ColorEngine` |
| P1-5 | `media/src/preview.rs:640` | Global preview frame cache, manually cleared | Bind to `DecoderPool` lifecycle |

### P2 — Readability

| # | File | Lines | Fix |
|---|------|-------|-----|
| P2-1 | `app/src/ui/viewer_panel.rs` | 6081 | Split: `viewer/mask_interaction.rs`, `viewer/gpu_fallback.rs`, `viewer/layer_decode.rs` |
| P2-2 | `app/src/ui/effect_controls_panel.rs` | 5907 | Per-effect sub-modules |
| P2-3 | `core/src/automation.rs` | 3221 | Split: `property_bag.rs`, `animated_property.rs`, `keyframe.rs`, `mutation.rs` |
| P2-4 | `app/src/app.rs` | 2534 | Split: per-concern sub-modules |
| P2-5 | `app/src/app/timeline_commands.rs` | 2424 | Per-command files |
| P2-6 | `effects/src/effect.rs` | 1914 | Split: `definition.rs`, `node.rs`, `render_op.rs`, `registry.rs`, `builtins.rs` |
| P2-7 | Global | 513 `.clone()` calls | Audit hot paths; use refs or `Cow` |

### P3 — Performance

| # | File | Issue | Expected Fix |
|---|------|-------|-------------|
| P3-1 | `renderer/src/pipeline.rs:157-190` | Per-layer `queue.submit()` — no batching | Batch all draws in one encoder |
| P3-2 | `renderer/src/pipeline.rs:377-449` | Per-frame GPU readback with `Maintain::Wait` | Present directly to surface |
| P3-3 | `effects/src/execution.rs` | All effects are CPU scalar ops | GPU compute shaders |
| P3-4 | `effects/src/adjustment.rs:427-490` | Box blur approximates Gaussian, CPU | Activate `blur_gaussian.wgsl` |

### P4 — Test Gaps

| # | Crate | Tests | Gap |
|---|-------|-------|-----|
| P4-1 | `mondrian-ai` | 0 | Orchestrator, providers, workflows untested |
| P4-2 | `mondrian-assets` | 0 | SQLite CRUD, import, schema migration untested |
| P4-3 | `mondrian-media` | 7 | Decoder, cache, proxy, multilevel cache untested |
| P4-4 | Global | 0 integration | No end-to-end pipeline tests |

---

## 4. Architecture Violation Details

### P-ARCH1: Timeline → Effects coupling

**Location:** [timeline/src/clip.rs:12](crates/mondrian-timeline/src/clip.rs#L12)

```rust
use mondrian_effects::{
    build_effect_render_graph, build_effect_render_plan,
    get_or_compile_scheduled_render_graph, EffectRenderOp
};
```

`Clip::evaluate_compiled_effect_graph()` at line 481-529 directly constructs `EffectGraphBuilderState`, injects mask nodes, and calls `get_or_compile_scheduled_render_graph()`. This is ~50 lines of effect-graph construction logic living in the timeline crate.

### P-ARCH2: Effect system (POSITIVE — already a DAG)

Node types in `graph.rs`:
```rust
pub enum EffectGraphNodeKind {
    Source,                                              // 0 inputs
    UnaryEffect { input, op },                          // 1 input
    Blend { base, overlay, blend_mode, opacity },       // 2 inputs (fan-in)
    Mask { input, mask, invert, mask_op },              // 2 inputs (fan-in)
    MaskSource { shape, feather, expansion, opacity },  // 0 inputs
}
```

Scheduler uses Kahn's algorithm with cycle detection ([graph.rs:417-456](crates/mondrian-effects/src/graph.rs#L417-L456)). The path from linear → DAG already exists via `compile_effect_render_graph()` which converts a `EffectRenderPlan` into a linear chain of `UnaryEffect` nodes.

The work here is **deletion of the legacy path**, not construction of a new one.

### P-ARCH3: CPU-centric rendering → PARTIALLY RESOLVED (Phase 3)

**Phase 3 added GPU compute shaders for ColorAdjust and GaussianBlur.** The `GpuBackend` provides compute-shader acceleration with transparent CPU fallback. A `GpuEffectExecutor` trait integrates GPU into the effect graph execution without API changes. LUT3D compute shader exists but is not yet activated (needs 3D texture upload support).

GPU compositing (Porter-Duff "Over") still exists as a separate accelerator with per-frame retry (no more permanent disable).

FFmpeg role: decode only (`DecoderPool`) and encode only (`ExportExecutor`). Core compositing is pure Rust CPU with GPU acceleration for supported effects.

### P-ARCH4: Global mutable state

12+ `OnceLock<Mutex<...>>` instances across the codebase. Key offenders:
- OCIO config: `static OCIO_CONFIG_PATH: Mutex<Option<PathBuf>>` ([ocio.rs:17](crates/mondrian-core/src/ocio.rs#L17))
- Effect frame caches: `static CACHE: OnceLock<Mutex<...>>` ([execution.rs:169,174](crates/mondrian-effects/src/execution.rs#L169))
- GPU compositor: `static GPU_COMPOSITOR: OnceLock<...>` ([viewer_panel.rs:4103](crates/mondrian-app/src/ui/viewer_panel.rs#L4103))
- Preview frame cache: `fn preview_frame_cache() -> &'static Mutex<...>` ([preview.rs:640](crates/mondrian-media/src/preview.rs#L640))

### P-ARCH6: Asset system

`AssetKind` has `Video | Audio | AdjustmentLayer | SolidColor` — decent coverage. But no `AssetSource` enum; source type is implicitly encoded via `path: PathBuf` using `mondrian://solid-color/{id}` URI hack.

Missing: `Procedural` (noise, gradient), `Remote` (URL-based), `Image` (still images as distinct from video), `LUT` (3D LUT files as first-class assets).

### P-ARCH7: UI dual source of truth

Selection state triplication:
1. `ViewerPanel.canvas_selected_clip` (for canvas rendering)
2. `AppState.canvas_selected_clip` (sync intermediary)
3. `TimelinePanel.selected_clips: HashSet<ClipSelection>` (for timeline UI)

Every canvas click writes to both ViewerPanel AND AppState copies (10+ dual-write sites). AppState then syncs into TimelinePanel on the next frame. This is fragile and will break when a node graph panel is added.
