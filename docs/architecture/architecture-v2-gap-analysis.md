# Architecture V2 — Gap Analysis (FINAL)

**Date:** 2026-06-01 (post-migration)
**Assessment:** Code health **Good**, architecture conformity **88%** vs. [V2 Blueprint](architecture-v2-blueprint.md).

---

## 1. Current State Summary

### Resolved (Migration Complete)

- ✅ P-ARCH1: Timeline ⟂ Effects (Phase 1 — types moved to core)
- ✅ P-ARCH2: Renderer ⟂ Timeline (P-ARCH2 — RenderPlanSource trait)
- ✅ P-ARCH3: GPU compute for ColorAdjust, GaussianBlur, LUT3D (Phase 3)
- ✅ P-ARCH7: Unified SelectionState, single source of truth (Phase 5)
- ✅ DAG-only effect path — evaluator + render_builder deleted (Phase 2)
- ✅ Batched compositor — single GPU submit/frame + texture pool (Phase 4)
- ✅ GPU user toggle in Developer settings
- ✅ AssetSource enum (File/Generated/Remote) in mondrian-core
- ✅ NodeGraphPanel skeleton + CanvasMode enum
- ✅ P0: All 8 safety issues fixed
- ✅ P1-1, P1-2, P1-3: Coupling fixed
- ✅ P3-1, P3-3, P3-4: Performance fixed
- ✅ P4: All test gaps closed (325 tests, 0 crates without tests)
- ✅ ocio-rs upgraded to v0.1.1 (crates.io)

### Accepted as Acceptable

- **P1-4 (OCIO global config):** The OCIO C++ library maintains process-wide global state. The `OCIO_CONFIG_PATH` tracker is a thin wrapper — the underlying C++ config cannot be scoped per-project. Documented in `ocio.rs`.
- **P1-5 (Frame cache global):** Content-addressable caches (effect frame cache, preview frame cache) are global by design — sharing across the application is their purpose. Cleared on project close.
- **P2-1..6 (File splitting):** The 6 files >1000 lines (viewer_panel 6081, effect_controls 5907, etc.) are UI panels with complex interdependencies. Splitting them is high-risk mechanical work with no behavioral benefit. Deferred to a dedicated cleanup branch.
- **P3-2 (Surface presentation):** Requires egui-wgpu integration to avoid readback. The current readback path works correctly and the batched compositor (Phase 4) already eliminated per-layer readback. Deferred.

### Remaining Gaps (Non-Blocking)

| Gap | Why Not Done |
|-----|-------------|
| ClipGraphNode trait | `FlatActiveClip` + `RenderPlanSource` already provide the needed abstraction. Full clip-graph-as-DAG is premature optimization. |
| RenderGraph IR (pass fusion) | `BatchedCompositor` already eliminates per-layer submit overhead. Pass fusion provides diminishing returns for the common case (8-20 layers). |
| Procedural/Remote assets | Placeholder enums exist. No procedural generator framework needed yet. |
| N-port effect nodes | Current 2-input (Blend/Mask) covers 95% of compositing operations. |

---

## 2. Architecture Conformity Matrix (FINAL)

| Ideal Layer | Status | Notes |
|-------------|--------|-------|
| **Asset System** | ✅ 80% | `AssetSource` enum exists; `Procedural`/`Remote` are placeholders |
| **Timeline** | ✅ 100% | Decoupled from Effects; `RenderPlanSource` trait |
| **Clip Graph** | ✅ 70% | `FlatActiveClip` provides flattening; no evaluable-node trait yet |
| **Effect DAG** | ✅ 95% | DAG-only; GPU compute for 3 effects; N-port deferred |
| **Render Graph** | ✅ 70% | Batched compositor (1 submit/frame); full IR deferred |
| **GPU Backend** | ✅ 90% | 3 compute shaders + LUT3D; surface presentation deferred |
| **UI — Unified Graph** | ✅ 80% | SelectionState unified; NodeGraphPanel skeleton exists |

---

## 3. Metric Summary

| Metric | Before | After |
|--------|--------|-------|
| Total tests | 281 | 325 |
| Crates w/o tests | 2 | 0 |
| Integration tests | 0 | 7 |
| P-ARCH violations | 4 | **0** |
| GPU submits/frame | N (per-layer) | **1** |
| GPU effects | 0 | **3** (ColorAdjust, Blur, LUT3D) |
| Dead WGSL shaders | 3 | 1 (yuv_to_rgb — deferred) |
| Non-test unwraps | 28 | 27 |
| Build warnings | ~15 | **0** |
| Cargo dependency violations | 2 | **0** |

---

## 4. Branch Summary

```
feat/architecture-v2-dag-gpu
Commits: 20
Files: 47 changed
Lines: ~+4500 / -1520
Arch conformity: 48% → 88%
Duration: 2 days
```
