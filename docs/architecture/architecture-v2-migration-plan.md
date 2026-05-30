# Architecture V2 — Phased Migration Plan

**Date:** 2026-05-30
**Branch:** `feat/architecture-v2-dag-gpu`
**Principle:** Each phase ≤ 1 week, independently verifiable, no feature regressions.

---

## Phase 0: Safety Patches + Test Foundation + Metrics (Week 1)

**Goal:** Make the codebase safe for large-scale refactoring.

### Tasks

| # | Task | Files | Est. |
|---|------|-------|------|
| 0.1 | Fix `RefCell` in `static` → `Mutex` | `media/src/preview.rs:86`, `app/src/ui/viewer_panel.rs:4602` | 0.5h |
| 0.2 | Fix 3 panic-prone unwraps | `media/src/cache.rs:65`, `app/src/ui/viewer_panel.rs:1022`, `app/src/ui/effect_controls_panel.rs:340` | 1h |
| 0.3 | Replace `.expect("poisoned")` with log-and-recover | `effects/src/effect.rs:473,481,487,507`, `export/src/queue.rs:1291` | 1h |
| 0.4 | Add in-memory SQLite CRUD tests for `mondrian-assets` | `assets/src/library.rs` (inline `#[cfg(test)]`) | 3h |
| 0.5 | Add workflow YAML parse tests for `mondrian-ai` | `ai/src/workflow.rs` (inline) | 2h |
| 0.6 | Add `FrameCache` LRU eviction + `MultiLevelCache` fallback tests | `media/src/cache.rs`, `media/src/multilevel_cache.rs` | 3h |
| 0.7 | Add integration smoke test: import media → open timeline → render 1 frame → assert non-empty | `tests/integration_tests.rs` (new) | 4h |
| 0.8 | Add `scripts/arch_metrics.sh` — counts dependency violations, globals, file sizes, test coverage | `scripts/arch_metrics.sh` (new) | 2h |

### Acceptance Criteria

- `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
- `cargo test --workspace` passes with new tests
- `cargo test -p mondrian-assets` has ≥10 tests (currently 0)
- `cargo test -p mondrian-ai` has ≥5 tests (currently 0)
- Integration smoke test passes on CI
- `scripts/arch_metrics.sh` produces machine-readable output

---

## Phase 1: Trait Boundaries — Decouple Timeline from Effects (Week 2)

**Goal:** Timeline crate no longer depends on Effects crate. Renderer crate no longer depends on Timeline crate.

### Tasks

| # | Task | Est. |
|---|------|------|
| 1.1 | Define `EffectGraphEvaluator` trait + `EffectGraphHandle` opaque type in `mondrian-core` | 3h |
| 1.2 | Define `TimelineRenderPlanElement` in `mondrian-core` (move from renderer) | 2h |
| 1.3 | Refactor `Clip::effects` to hold pure data (`Vec<EffectNodeData>`) instead of `Vec<EffectNode>` | 4h |
| 1.4 | Move `Clip::evaluate_compiled_effect_graph()` logic from timeline into effects crate as `EffectGraphEvaluator` impl | 4h |
| 1.5 | Inject `Arc<dyn EffectGraphEvaluator>` into the render plan builder | 2h |
| 1.6 | Remove `mondrian-effects` from `mondrian-timeline/Cargo.toml` dependencies | 0.5h |
| 1.7 | Remove `mondrian-timeline` from `mondrian-renderer/Cargo.toml` dependencies | 0.5h |
| 1.8 | Update all call sites in `mondrian-app` and `mondrian-export` | 4h |

### Acceptance Criteria

- `mondrian-timeline/Cargo.toml` has NO `mondrian-effects` dependency
- `mondrian-renderer/Cargo.toml` has NO `mondrian-timeline` dependency
- `cargo test --workspace` passes
- `cargo clippy` passes
- Manual preview render matches pre-refactor output (visual comparison of 5 representative frames)

---

## Phase 2: DAG-Only Effect Path — Remove Linear Stack (Week 3)

**Goal:** `EffectRenderPlan` deleted. All effects go through `EffectRenderGraph` → `CompiledEffectGraph`.

**Context:** The DAG infrastructure already exists. This phase is about removing the legacy fallback paths.

### Tasks

| # | Task | Est. |
|---|------|------|
| 2.1 | Mark `build_effect_render_plan()` as `#[deprecated]`, migrate all callers to `build_effect_render_graph()` | 3h |
| 2.2 | Migrate legacy builtin evaluators to `graph_builder` closures (`BasicCorrection`, `WhiteBalance`, etc. in `builtin_evaluator_for`) | 8h |
| 2.3 | Delete `EffectRenderPlan` type and `build_effect_render_plan()` function | 2h |
| 2.4 | Delete `EffectEvaluator` (the legacy `evaluate_effect_stack` pathway) | 2h |
| 2.5 | Delete `EffectRenderBuilder` — only `EffectGraphBuilder` and `EffectBranchingGraphBuilder` remain | 1h |
| 2.6 | Add `MultiInput` node kind for future N-port support (implementation deferred, type exists) | 2h |
| 2.7 | Add golden image tests: render 10 representative frames, compare PNG pixel data (±1 per channel) | 4h |
| 2.8 | Benchmark `apply_compiled_effect_graph` latency before/after; assert no regression | 2h |

### Acceptance Criteria

- `EffectRenderPlan` type does not exist in the codebase
- `EffectRenderBuilder` type alias does not exist
- All 14 builtin effects use `graph_builder` or `branching_graph_builder`
- Golden image tests pass (all 10 frames within ±1 per channel)
- `apply_compiled_effect_graph` benchmark ≤ pre-refactor baseline

---

## Phase 3: GPU Compute Shaders — Activate wgpu Effects (Week 4)

**Goal:** At least 3 effects execute on GPU. Infrastructure for GPU/CPU hybrid execution.

### Tasks

| # | Task | Est. |
|---|------|------|
| 3.1 | Refactor `GpuContext` as non-singleton, inject into effect executor | 4h |
| 3.2 | Add `GpuDispatch` variant to effect node execution | 2h |
| 3.3 | Implement LUT 3D compute shader (activate existing `lut3d.wgsl`) | 4h |
| 3.4 | Implement Gaussian Blur compute shader (activate existing `blur_gaussian.wgsl`) | 4h |
| 3.5 | Implement ColorAdjust compute shader (exposure/contrast/saturation in single pass) | 4h |
| 3.6 | Add GPU/CPU scheduling marks in `CompiledEffectGraph` — nodes tagged as CPU or GPU | 3h |
| 3.7 | Implement GPU→CPU readback for mixed GPU/CPU chains | 6h |
| 3.8 | Remove "permanently disable GPU after 4 failures" — replace with per-frame detection | 2h |

### Acceptance Criteria

- LUT, Blur, ColorAdjust execute on GPU compute shaders
- GPU output matches CPU output pixel-exact (±1 per channel)
- Benchmark: 4K frame with LUT+Blur+ColorAdjust, GPU path ≥3x faster than CPU path
- GPU unavailable → automatic transparent fallback to CPU (per-frame)
- `MONDRIAN_FORCE_CPU=1` env var forces CPU path for testing

---

## Phase 4: Render Graph — Pass Fusion & GPU Resource Scheduling (Week 5)

**Goal:** Compositor uses compiled `RenderGraph` IR instead of iterative for-loop.

### Tasks

| # | Task | Est. |
|---|------|------|
| 4.1 | Define `RenderGraph` IR types in `mondrian-core` | 4h |
| 4.2 | Define `RenderPass`, `RenderResource`, `PassDependency` types | 3h |
| 4.3 | Implement pass fusion compiler (adjacent same-size passes merge) | 6h |
| 4.4 | Implement GPU resource pool: `TexturePool` (size-class reuse), `BufferPool` (ring buffer) | 6h |
| 4.5 | Rewrite `FrameCompositor` to use `RenderGraph::compile(layers) → Execute` | 6h |
| 4.6 | Batch all draw calls into single `queue.submit()` per frame | 2h |
| 4.7 | Add `wgpu` timestamp query profiling behind `MONDRIAN_RENDER_PROFILE=1` | 3h |

### Acceptance Criteria

- `FrameCompositor::composite_frame()` internally uses compiled `RenderGraph`
- 8-layer timeline: single `queue.submit()` instead of 8 separate submits
- Benchmark: 8-layer Normal blend compositing ≥2x faster than per-layer path
- Adjacent Normal-blend layers with same dimensions fuse into single pass
- Developer can run `MONDRIAN_RENDER_PROFILE=1` to see GPU timeline in console

---

## Phase 5: UI — Single Selection State + Node Graph Skeleton (Week 6)

**Goal:** Single source of truth for selection. Timeline view is a projection of the graph. Node graph panel skeleton.

### Tasks

| # | Task | Est. |
|---|------|------|
| 5.1 | Define `SelectionState` in `AppState` (single struct with `selected_clips`, `selected_effect`, `selected_mask`) | 2h |
| 5.2 | Remove `ViewerPanel.canvas_selected_clip` — read from `AppState.selection_state` | 3h |
| 5.3 | Remove `TimelinePanel.selected_clips` — read from `AppState.selection_state` | 3h |
| 5.4 | All 10+ dual-write sites now write only to `AppState.selection_state` | 4h |
| 5.5 | Verify selection sync: canvas click → timeline highlight → effect controls, all within 1 frame | 2h |
| 5.6 | Add `NodeGraphPanel` skeleton (empty panel, toggle via `CanvasMode::NodeGraph` or shortcut) | 4h |

### Acceptance Criteria

- Exactly one `SelectionState` instance exists: `AppState.selection_state`
- Canvas click → timeline highlight → effect controls panel all consume same reference
- Toggle between Timeline view and (empty) Node Graph view via keyboard shortcut
- No regression in selection behavior (click, shift-click, drag-select)

---

## Performance Benchmarks — Expected vs Actual

| Operation | Pre-Migration (CPU) | Post Phase 3 (GPU) | Expected Speedup |
|-----------|--------------------|--------------------|------------------|
| 1080p Gaussian Blur (r=10) | ~12ms | ~0.8ms | 15× |
| 4K LUT 3D apply | ~8ms | ~0.5ms | 16× |
| 1080p ColorAdjust | ~2ms | ~0.3ms | 6× |
| 8-layer Normal composite | ~4ms (8 submits) | ~0.2ms (1 submit) | 20× |
| 8-layer mixed blend modes | ~15ms | ~8ms | 2× |

*Estimates based on typical GPU compute shader vs CPU scalar throughput. Actual benchmarks required at Phase 3 acceptance.*

---

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| GPU shader produces different pixels than CPU | Medium | High — visual regression | Golden image tests ±1 tolerance; `MONDRIAN_FORCE_CPU=1` fallback |
| Trait object dispatch on hot path slows rendering | Low | Medium | `EffectGraphHandle` internally stores `Arc<CompiledEffectGraph>` — no per-pixel vtable |
| Phase 1 trait injection breaks app compilation for >1 day | Low | High | 1.8 (4h allocated for fixing call sites); feature-flag gated |
| Breaking existing plugin effects during Phase 2 | Medium | Medium | Plugin SDK retains same API surface; upgrade is in `EffectGraphBuilderState` internals |
| Pass fusion bug causes incorrect compositing | Medium | High | Extensive golden image tests; fusible-only-when-identical-dimensions rule (conservative) |
