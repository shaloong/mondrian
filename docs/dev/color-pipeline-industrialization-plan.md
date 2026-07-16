# Color Pipeline Industrialization Plan

Temporary execution plan for pushing Mondrian's color pipeline toward
Blender/DaVinci Resolve/Premiere/Final Cut level reliability. Delete this file
after the color-pipeline industrialization work is complete.

This document is intentionally prescriptive. It is written so a weaker model can
execute the work without relying on prior conversation memory.

## Authority and Baseline

The current worktree and git history are the authority. Do not rely on any old
chat summary when it conflicts with code, docs, tests, or commits.

Before each implementation pass, read at least:

- `AGENTS.md`
- `docs/architecture/color-management.md`
- `docs/architecture/render-pipeline.md`
- `docs/architecture/media-pipeline.md`
- `docs/architecture/gpu-native-renderer.md`
- `docs/architecture/ui-system.md`
- relevant crate code for the phase

Current rough maturity estimate:

- Architecture and fail-closed policy: about 75%
- Diagnostics/reporting: about 80%
- CPU/OCIO correctness: about 70%
- Native GPU main path: about 50%
- Real display management: about 40%
- Dirty media metadata handling: about 65%
- Preview/export parity and continuous gates: about 65%
- Overall remaining work to approach mature NLE reliability: about 35%-40%

Most remaining work is not about inventing concepts. It is about converting
existing contracts and diagnostics into production paths, reducing legacy RGBA8
execution, validating real display behavior, and expanding dirty-media coverage.

## Non-Negotiable Principles

1. One color pipeline.

   Media metadata or user intent resolves into Mondrian color interpretation,
   then OCIO colorspace/display-view resolution, then renderer graph execution,
   then preview/export/cache. Preview and export may differ in scheduling,
   cache lifetime, or readback strategy only. They must not duplicate input
   interpretation, transform selection, compositing semantics, or policy logic.

2. Stock OCIO is the single default execution infrastructure.

   Per accepted ADR-0005, Mondrian Standard is a bundled, immutable, versioned
   OCIO package. ACES and Custom OCIO are parallel product modes over the same
   integration. Do not build a native color engine speculatively. A native
   specialization requires a same-math comparison proving a documented
   capability, fidelity, fusion, or material multi-GPU p95/p99 benefit. Missing
   config, display/view, colorspace, shader extraction, or processor must be
   reported as a structured error. Do not add "looks fine" fallbacks,
   approximate LUT substitutions, or implicit alternate views.

3. No Standard fallback.

   Mondrian Standard must fail closed if the bundled config contract breaks.
   Never silently fall back to Rec.709, sRGB, CPU helper transforms, or an
   alternate config to make UI/export continue.

4. Auto, Override, and Data are separate concepts.

   `MediaColorInterpretation` expresses only Auto or user Override.
   `AssetColorPayload::NonColorData` expresses utility/data payload bypass.
   Interpret Footage color-space dropdowns must contain Auto plus concrete
   color spaces only. Data/NonColorData must not appear as a color-space option.

5. RGBA8 is a boundary, not the internal color contract.

   RGBA8 may exist at decode/import, debug/golden snapshots, UI presentation
   readback, CPU encoder boundaries, or explicitly documented legacy paths.
   Every legacy RGBA8 fallback must carry a structured reason.

6. GPU reports must prove real execution, not intent.

   CPU correctness tests prove math semantics. They do not prove playback,
   scrubbing, viewer, or export scheduling. Native GPU path claims must be based
   on actual `RenderGpuOutputBoundaryRuntime` recording, runtime diagnostics,
   stage reports, and readback/presentation evidence.

7. Display management claims must be conservative.

   A model that contains monitor/profile/display fields is not proof of correct
   display. OS monitor ICC behavior, HDR/EDR, surface color space, swapchain
   format, UI external texture compositing, and multi-display changes must all
   be represented as real contracts or explicit blockers.

8. Metadata detection must be evidence-based.

   Do not implement `detect_color_space(file) -> ColorSpace`. Use structured
   results with color space, confidence, evidence, warnings, source/method, and
   user-overridable status. Preserve dirty/conflicting metadata instead of
   flattening it into one resolved enum.

9. Reports are external contracts.

   Perf JSONL, smoke tests, app panels, export reports, and diagnostics tooling
   should consume versioned report contracts. Avoid parallel app-local schemas
   when renderer/export/media already own structured evidence.

10. No old-format compatibility debt.

   Mondrian is alpha. If a color API/model is wrong, migrate it cleanly.
   Update tests and docs. Do not keep obsolete compatibility branches unless a
   current production path still needs them and the blocker is documented.

## Branching, Commits, and Gates

The user allows grouping related phases into one branch. Use a branch name that
matches the bundle:

- `feat/color-gpu-main-path`
- `feat/color-display-management`
- `feat/color-metadata-diagnostics`
- `fix/color-legacy-rgba8-paths`
- `test/color-parity-gates`

Use conventional commits. Several related commits on one branch are fine.
Avoid one giant unrelated commit.

Minimum gates before committing a non-doc phase:

```bash
cargo fmt
cargo test -p <changed-crate> <relevant_filter>
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

For broad renderer/app/export changes, run targeted crate suites first, then
clippy:

```bash
cargo test -p mondrian-renderer
cargo test -p mondrian-app <relevant_filter>
cargo test -p mondrian-export <relevant_filter>
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Manual/ignored smoke gates should be run when the phase touches their path and a
real adapter or generated fixture is available:

```bash
cargo test -p mondrian-renderer gpu_output_boundary_runtime_smoke_report_on_real_wgpu_device -- --ignored --nocapture
cargo test -p mondrian-app viewer_gpu_output_budget_smoke -- --ignored --nocapture
cargo test -p mondrian-app viewer_gpu_output_display_baseline_smoke -- --ignored --nocapture
```

When a phase changes a crate, update the matching architecture doc. If a weak
model cannot explain which doc changed and why, the phase is incomplete.

## Phase 0 - Orientation and Contract Inventory

Goal: build a fresh map of the current color contracts before editing.

Required inspection:

- `crates/mondrian-core/src/color.rs`
- `crates/mondrian-core/src/ocio.rs`
- `crates/mondrian-media/src/info.rs`
- `crates/mondrian-renderer/src/color_stage.rs`
- `crates/mondrian-renderer/src/color_transform.rs`
- `crates/mondrian-renderer/src/ocio_gpu.rs`
- `crates/mondrian-renderer/src/timeline_composite.rs`
- `crates/mondrian-app/src/app_ui/preview.rs`
- `crates/mondrian-app/src/app_ui/window.rs`
- `crates/mondrian-app/src/app_ui/viewer_gpu_output_budget.rs`
- `crates/mondrian-export/src/queue/mod.rs`
- relevant perf tests in `crates/mondrian-app/src/app/perf_tests.rs`
  and `crates/mondrian-export/src/queue_perf_tests.rs`

Deliverable:

- No code required.
- A short implementation note in the final response listing the exact next
  phase selected and the files that will be modified.

Do not:

- Start by refactoring shared types.
- Infer behavior from docs without checking code.
- Trust old chat memory over git.

## Phase 1 - Report Contract Convergence

Goal: make renderer/export/preview/viewer reports consume engine-owned evidence
instead of parallel local schemas.

Current baseline:

- Renderer owns `RenderGpuOutputHealthReport`,
  `RenderGpuOutputFrameReport`, `RenderGpuOutputStageDiagnosticsReport`, and
  `RenderGpuOutputRuntimeDiagnosticsReport`.
- Viewer JSONL includes renderer-owned structured stage evidence next to
  temporary flat app counters.
- Export and preview have versioned health reports, but their report vocabulary
  is still app/export local.

Tasks:

1. Add renderer-owned runtime evidence to viewer diagnostics.

   Extend app-window viewer JSONL to include
   `RenderGpuOutputRuntimeDiagnosticsReport` derived from
   `RenderGpuOutputBoundaryRuntimeDiagnostics`.

   Acceptance:
   - JSONL has runtime report fields for shader cache, backend prep/object
     cache, frame table entries, and next frame id.
   - Existing flat tracing fields can remain temporarily, but tests must assert
     the structured runtime report exists.

2. Teach `viewer_gpu_output_budget` to prefer structured renderer stage/runtime
   reports.

   Acceptance:
   - Budget replay works when flat stage counters are absent but structured
     reports are present.
   - Unknown/missing required structured fields fail closed.
   - Tests cover both current mixed schema and structured-only schema.

3. Introduce shared color report vocabulary where practical.

   Do this carefully. Do not create a generic abstraction just to remove similar
   enum names. Only share types when preview/export/viewer truly have the same
   semantic area/check/root-cause/action.

   Acceptance:
   - No loss of existing root-cause/action specificity.
   - JSON schema version increments if output shape changes.
   - Tests assert old unsupported fields are absent when the contract says they
     are gone.

Likely files:

- `crates/mondrian-renderer/src/color_stage.rs`
- `crates/mondrian-app/src/app_ui/window.rs`
- `crates/mondrian-app/src/app_ui/viewer_gpu_output_budget.rs`
- `crates/mondrian-app/src/app/perf_tests.rs`
- `docs/architecture/render-pipeline.md`
- `docs/architecture/gpu-native-renderer.md`
- `docs/architecture/ui-system.md`
- `docs/dev/performance-profiling.md`

Tests:

```bash
cargo test -p mondrian-app viewer_gpu_output_budget
cargo test -p mondrian-app viewer_gpu_output_diagnostics_jsonl_includes_health_summary
cargo test -p mondrian-renderer gpu_output_health_report
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Phase 2 - Legacy RGBA8 Containment and Removal

Goal: reduce legacy RGBA8 execution to necessary boundaries and keep every
remaining fallback structured, counted, and mapped to a migration target.

Current baseline:

- `TimelineCompositeDiagnostics` and
  `TimelineCompositeColorPathSummary` expose legacy RGBA8 fallback counts.
- Preview/export reports include `fully_float_linear`,
  `legacy_reason_total`, and `legacy_breakdown`.
- Float/linear paths exist for normal media/solid blending and some adjustment
  effects.
- Geometry transforms and legacy-only effect paths still force legacy RGBA8.

Tasks:

1. Inventory all legacy RGBA8 fallback reasons.

   Use `TimelineCompositeLegacyBreakdown` as the authority. Do not add free-form
   strings. If a new reason is needed, add a typed field and tests.

   Acceptance:
   - Every legacy fallback has layer type, reason, and count.
   - Summary fails closed when legacy count and reason totals disagree.

2. Migrate geometry transforms off legacy RGBA8 where feasible.

   Implement or route float/linear geometry operations for identity-compatible
   and common affine transforms first. Preserve exact behavior with focused
   tests.

   Acceptance:
   - Common transform fixtures stay `fully_float_linear`.
   - Unsupported transform modes still emit structured legacy reasons.
   - No hidden quantization to RGBA8 inside float path.

3. Migrate float-capable effects out of legacy-only execution.

   Add or use `mondrian-effects` float contracts for eligible unary effects.
   Do not silently call the RGBA8 effect executor from float code.

   Acceptance:
   - Effect graph diagnostics explain which effect node blocked float/linear.
   - Preview/export health reports show zero legacy reasons for migrated cases.

4. Tighten perf gates.

   Preview/export smoke reports should fail by default on nonzero structured
   legacy reasons for the tested scenarios.

Do not:

- Delete legacy RGBA8 paths before all production callers have a replacement.
- Hide fallback behind "compatibility".
- Count a fallback without a typed migration reason.

Likely files:

- `crates/mondrian-renderer/src/timeline_composite.rs`
- `crates/mondrian-effects/src/execution.rs`
- `crates/mondrian-effects/src/adjustment.rs`
- `crates/mondrian-app/src/app_ui/preview.rs`
- `crates/mondrian-export/src/queue/mod.rs`
- `docs/architecture/render-pipeline.md`
- `docs/architecture/effect-system.md`

Tests:

```bash
cargo test -p mondrian-renderer timeline_composite
cargo test -p mondrian-effects
cargo test -p mondrian-app preview_color_report
cargo test -p mondrian-export export_color_health_report
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Phase 3 - Native GPU Main Path Integration

Goal: make native GPU color execution a real product path for viewer/playback
and export scheduling, not only a renderer smoke/test capability.

Current baseline:

- `RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend`
  records real upload + native GPU OCIO + optional readback.
- Renderer smoke proves real wgpu GPU output boundary execution.
- App window owns a `RenderGpuOutputBoundaryRuntime`.
- Viewer telemetry reports output attempts, stage diagnostics, display blockers,
  and external texture registration outcomes.
- CPU output path remains the correctness fallback for many product flows.

Tasks:

1. Make viewer output use runtime-owned GPU color path whenever display contract
   allows it.

   Acceptance:
   - The app-window path records through `RenderGpuOutputBoundaryRuntime`.
   - If GPU recording fails, telemetry reports structured failure and does not
     silently present CPU-colored pixels as GPU output.
   - Viewer JSONL includes stage and runtime evidence for the attempt.

2. Connect preview playback/scrubbing diagnostics to real GPU output attempts.

   The preview service can prove it produced a working-space candidate. The
   app-window telemetry proves whether final GPU output recording and external
   texture registration succeeded. Keep those layers separate.

   Acceptance:
   - Playback/scrubbing smokes can correlate preview candidate counters with
     viewer output attempts.
   - Stale/current/loading/unavailable states remain UI adapter semantics and
     do not mutate timeline playback state.

3. Add export GPU output scheduling path.

   Export may still need CPU readback for encoders. That readback must be an
   explicit `ReadbackToCpu` boundary from a GPU output plan, not an implicit CPU
   transform fallback.

   Acceptance:
   - Export diagnostics distinguish CPU correctness path from native GPU +
     readback path.
   - `ExportColorHealthReport` root causes/actions include GPU scheduling
     failures from renderer-owned stage/runtime evidence.
   - Export tests prove CPU/GPU boundary parity for representative output
     spaces.

4. Maintain CPU correctness path as reference only.

   CPU path remains valuable for golden correctness, parity, and unsupported
   environments. Product reports must not claim GPU readiness when CPU path ran.

Do not:

- Infer GPU execution from a plan alone.
- Treat shader cache readiness as frame execution.
- Treat generated WGSL as the canonical execution artifact.
- Reconstruct OCIO binding contracts from WGSL text.

Likely files:

- `crates/mondrian-renderer/src/color_stage.rs`
- `crates/mondrian-renderer/src/color_transform.rs`
- `crates/mondrian-renderer/src/ocio_gpu.rs`
- `crates/mondrian-app/src/app_ui/window.rs`
- `crates/mondrian-app/src/app_ui/preview.rs`
- `crates/mondrian-export/src/queue/mod.rs`
- `docs/architecture/gpu-native-renderer.md`
- `docs/architecture/render-pipeline.md`

Tests:

```bash
cargo test -p mondrian-renderer color_stage
cargo test -p mondrian-renderer ocio_gpu
cargo test -p mondrian-renderer gpu_output_boundary_runtime_matches_cpu_output_on_real_wgpu_device
cargo test -p mondrian-app viewer_gpu_output
cargo test -p mondrian-export color
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Run the ignored real-GPU smoke when hardware is available:

```bash
cargo test -p mondrian-renderer gpu_output_boundary_runtime_smoke_report_on_real_wgpu_device -- --ignored --nocapture
```

## Phase 4 - Real Display Management

Goal: move display management from "modeled and diagnosed" toward real
cross-platform display correctness.

Current baseline:

- App surface contract records format, selected surface color space, HDR mode,
  `display_hdr_info`, present modes, alpha modes, per-format surface color-space
  capabilities, display target fingerprint, and tone-map headroom diagnostics.
- Unsupported HDR/P3/log presentation is blocked instead of silently shown.
- Viewer GPU-output budget can fail per display issue reason and display
  capability drift reason.
- UI external texture compositing currently blocks some true HDR/P3 paths.

Tasks:

1. Preserve display target and surface contract evidence end to end.

   Acceptance:
   - Every viewer output attempt includes selected surface contract and display
     target fingerprint when available.
   - Refresh events include previous/next surface snapshots and explain changes.

2. Promote HDR/P3 only when the whole payload path supports it.

   The final output texture format, external texture sampling, UI compositor
   shader, and swapchain color space must move together. Partial support is a
   blocker, not readiness.

   Acceptance:
   - `UiExternalTextureCompositingRequiresSdrSrgb` or equivalent blockers remain
     visible until the payload path is upgraded.
   - No HDR/P3 readiness claim is emitted through an SDR-only payload path.

3. Add platform display profile strategy.

   Treat OS ICC/EDR/HDR behavior as platform-specific. Start with diagnostics
   and explicit unsupported states if full correction is not implemented.

   Acceptance:
   - Windows/macOS/Linux differences are represented as structured capability
     or unsupported diagnostics.
   - Monitor ICC presence/identity is captured where possible.
   - Missing OS profile access is reported, not ignored.

4. Build display-management smoke/budget fixtures.

   Add synthetic JSONL fixtures for HDR-on-SDR, P3-on-sRGB, payload blocker,
   monitor switch, tone-map headroom drift, surface format drift, present mode
   drift, alpha mode drift, and unknown future reason.

Do not:

- Claim real monitor correctness because `DisplayManagementPolicy` exists.
- Present Rec.2020 SDR or camera-log spaces directly as display surfaces.
- Auto-reconfigure swapchain to HDR/P3 without verifying payload path support.

Likely files:

- `crates/mondrian-app/src/app_ui/window.rs`
- `crates/mondrian-app/src/app_ui/viewer_gpu_output_budget.rs`
- `crates/mondrian-ui-renderer` if external texture compositing changes
- `docs/architecture/ui-system.md`
- `docs/architecture/color-management.md`
- `docs/architecture/gpu-native-renderer.md`

Tests:

```bash
cargo test -p mondrian-app display_output_contract
cargo test -p mondrian-app viewer_gpu_output_diagnostics
cargo test -p mondrian-app viewer_gpu_output_budget
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Phase 5 - Dirty Media Metadata Detection

Goal: make media color detection realistic for dirty production footage while
keeping Auto/Override/Data boundaries clean.

Current baseline:

- `DetectedColorInterpretation` carries color space, confidence, source/method,
  evidence, warnings, and user-overridable status.
- CICP exact/partial detection exists.
- Metadata hints detect Apple Log, S-Log3/S-Gamut3.Cine, and ARRI LogC4.
- HDR side data and ICC profile presence are surfaced in diagnostics.
- Preview/export aggregate `VideoColorDiagnosticIssueAggregate` over referenced
  assets.

Tasks:

1. Expand detector evidence coverage.

   Add structured evidence for:
   - Rec.709
   - sRGB
   - Rec.2020
   - Rec.2100 PQ
   - Rec.2100 HLG
   - Apple Log
   - S-Log3/S-Gamut3 variants
   - ARRI LogC4
   - ICC profile presence and inferred profile family
   - HDR10 mastering display metadata
   - MaxCLL/MaxFALL
   - dynamic HDR10+ presence
   - Dolby Vision config presence

   Add more only when evidence is preserved as structured data.

2. Strengthen conflict handling.

   Conflicts to preserve:
   - metadata hint vs CICP
   - multiple camera/log hints
   - partial CICP inference
   - missing/unsupported tags
   - ICC vs CICP mismatch
   - HDR side data inconsistent with SDR tags

   Acceptance:
   - Warnings preserve selected and ignored evidence.
   - UI/export/perf reports consume issue summaries, not parsed text.

3. Improve confidence policy.

   Example policy direction:
   - Exact CICP delivery triplet: high confidence.
   - Explicit camera/log metadata hint: high confidence, but warning if CICP
     conflicts.
   - Partial CICP: medium confidence with warning.
   - Missing tags with project policy assumption: no detected color space; this
     is not metadata.
   - Decoder unavailable: no detected color space; report source state.

4. Keep Auto dynamic.

   Auto must not persist the currently resolved detected colorspace. Detector
   improvements should affect Auto assets on re-probe. User Override must remain
   stable.

Do not:

- Use filename guesses as high confidence without evidence/warnings.
- Turn missing metadata policy assumptions into detected metadata.
- Put Data/NonColorData in the color-space dropdown.
- Write FFmpeg delivery tags for camera-log acquisition spaces unless a
  deliberate professional intermediate contract allows it.

Likely files:

- `crates/mondrian-media/src/info.rs`
- `crates/mondrian-core/src/color.rs`
- `crates/mondrian-core/src/icc.rs`
- `crates/mondrian-app/src/app_ui/interpret_asset_dialog.rs`
- `crates/mondrian-app/src/app_ui/panels.rs`
- `crates/mondrian-app/src/app_ui/preview.rs`
- `crates/mondrian-export/src/queue/mod.rs`
- `docs/architecture/media-pipeline.md`
- `docs/architecture/color-management.md`

Tests:

```bash
cargo test -p mondrian-media color
cargo test -p mondrian-core color
cargo test -p mondrian-app interpret_asset_dialog
cargo test -p mondrian-app preview_color
cargo test -p mondrian-export color
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Phase 6 - Preview/Export Parity and Golden Gates

Goal: prove that preview and export share interpretation, compositing, final
boundary behavior, and report semantics.

Current baseline:

- App-level tests compare preview/export color health fields.
- Golden frame hashes exist for some renderer/app preview/export boundary cases.
- Preview/export report signatures are compared in selected tests.

Tasks:

1. Add representative timeline golden frames.

   Cover:
   - Rec.709 -> sRGB SDR
   - Rec.2020 working -> sRGB output with tone map policy
   - HDR working -> SDR output
   - camera-log input -> Rec.709/sRGB output
   - explicit override vs detected metadata
   - missing metadata policy reject
   - data texture bypass
   - multilayer float/linear blend
   - legacy fallback fixture, only while legacy path remains

2. Normalize report signatures.

   Preview/export reports may have different profile names or target labels, but
   shared health semantics must match for the same timeline frame.

   Acceptance:
   - Verdict/check/root-cause/action signatures are compared after normalized
     target-specific fields are removed.
   - Differences require explicit test updates and doc notes.

3. Add perf budget JSONL gates.

   Budget scenarios:
   - 1080p 29.97 fps
   - 1080p 60 fps
   - 4K 29.97 fps
   - 4K 60 fps
   - multi-track blend
   - multi-effect stack
   - HDR -> SDR
   - scrubbing
   - proxy/cache
   - export scheduling

   Keep budgets realistic for alpha, but fail closed for color-path evidence:
   missing report, GPU blocker, transfer stage, structured legacy reason, or
   policy rejection should fail unless the scenario explicitly allows it.

Do not:

- Update golden hashes casually.
- Hide preview/export divergence behind separate reports.
- Let unrelated loaded asset diagnostics affect active-sequence report results.

Likely files:

- `crates/mondrian-renderer/tests/golden_composite.rs`
- `crates/mondrian-app/src/app_ui/preview.rs`
- `crates/mondrian-app/src/app/perf_tests.rs`
- `crates/mondrian-export/src/queue_perf_tests.rs`
- `docs/dev/performance-profiling.md`
- `docs/architecture/render-pipeline.md`

Tests:

```bash
cargo test -p mondrian-renderer golden
cargo test -p mondrian-app preview_export
cargo test -p mondrian-app preview_color_report
cargo test -p mondrian-export export_color
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Phase 7 - OCIO/GPU Backend Hardening

Goal: make the OCIO GPU backend path robust enough to carry production playback
and export scheduling.

Current baseline:

- OCIO GPU shader extraction exists through `mondrian-core`.
- Renderer resource planning takes binding contracts from OCIO descriptors, not
  generated shader text.
- Wrapper source artifact is diagnostic text. Stage-split Naga IR/module
  artifacts are the intended execution path.
- Backend prep/object runtimes create resources, bind groups, wrapper modules,
  render pipeline, and render-pass nodes.

Tasks:

1. Keep Naga/module artifacts canonical.

   Acceptance:
   - Runtime object creation uses validated Naga/module artifacts.
   - WGSL/GLSL text remains diagnostic output only.
   - Tests reject attempts to infer binding layout from shader source text.

2. Expand resource validation.

   Validate LUT dimensions, uniform buffer layout, texture/sample type,
   binding ranges, resource names, extents, and cache-key stability.

3. Add backend failure diagnostics.

   Backend object failures should preserve which layer failed:
   shader module, OCIO resource bind group, wrapper bind group, wrapper shader
   module, pipeline layout, render pipeline, render pass node, or readback.

4. Stress cache behavior.

   Add tests for reuse, invalidation, and mismatched contracts:
   - color-space transform vs display-view transform
   - output format change
   - wrapper color contract change
   - LUT/uniform payload change
   - surface target format change

Do not:

- Treat "shader cache has entry" as "GPU frame executed".
- Build bind groups from parsed WGSL.
- Allow cache reuse across different color-domain semantics.

Likely files:

- `crates/mondrian-renderer/src/ocio_gpu.rs`
- `crates/mondrian-renderer/src/color_stage.rs`
- `crates/mondrian-renderer/src/color_transform.rs`
- `docs/architecture/gpu-native-renderer.md`

Tests:

```bash
cargo test -p mondrian-renderer ocio_gpu
cargo test -p mondrian-renderer color_stage
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Phase 8 - UI and Operator Diagnostics

Goal: make failures understandable in app panels without duplicating engine
logic in UI code.

Tasks:

1. App panels must consume report contracts.

   Panels may format labels, but should not recompute color health semantics.
   Use `health_report()`, `summary()`, or issue aggregates from owner crates.

2. Show Auto interpretation responsibly.

   Interpret Footage should show:
   - Auto - detected color space / confidence / source
   - Auto - unresolved/missing metadata / policy handling
   - warnings from structured evidence

   It must not make low-confidence assumptions look like explicit metadata.

3. Make blocked display/GPU paths actionable.

   UI/operator labels should expose root-cause/action codes or concise labels
   derived from them. Avoid parsing debug strings.

Do not:

- Put explanatory wall text in normal UI surfaces.
- Rebuild issue taxonomies in panels.
- Hide fail-closed behavior behind generic empty viewer messages.

Likely files:

- `crates/mondrian-app/src/app_ui/panels.rs`
- `crates/mondrian-app/src/app_ui/interpret_asset_dialog.rs`
- `crates/mondrian-app/src/app_ui/window.rs`
- `docs/architecture/ui-system.md`

Tests:

```bash
cargo test -p mondrian-app panels
cargo test -p mondrian-app interpret_asset_dialog
cargo test -p mondrian-app viewer_gpu_output
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Definition of Done for the Whole Plan

The color pipeline can be considered "industrialized enough for alpha" when all
of the following are true:

- Standard OCIO config is validated fail-closed at startup/config load.
- Preview and export share input interpretation, working-space compositing, and
  final output boundary planning.
- Auto/Override/Data boundaries are enforced in model, UI, preview, and export.
- Remaining legacy RGBA8 paths are few, documented, structured, and budgeted.
- Common preview/export scenarios are fully float/linear by default.
- Native GPU output path is used by real viewer/playback paths when supported.
- Export can report whether it used CPU correctness, native GPU + readback, or
  a blocked GPU path.
- Renderer GPU output reports expose frame, stage, runtime, health, root causes,
  actions, and evidence through renderer-owned types.
- Viewer JSONL and budgets consume structured renderer/display/media evidence.
- Display management blocks unsupported HDR/P3/log presentation and explains
  surface/payload/monitor reasons.
- Dirty media metadata detection preserves confidence, evidence, warnings, and
  conflicts.
- Preview/export golden tests cover representative SDR, HDR, log, override,
  missing metadata, data texture, multilayer, and legacy fixtures.
- Perf JSONL gates fail closed on missing color reports, GPU blockers, transfer
  stages, unexpected legacy RGBA8 reasons, and policy rejections.
- Architecture docs match code.
- `cargo fmt` and
  `cargo clippy --workspace --all-targets --all-features -- -D warnings` pass.

## Stop Conditions

Stop and ask for stronger model/user review if any of these happen:

- A phase requires changing public project file semantics outside color pipeline
  scope.
- A display-management change would silently alter OS/platform presentation
  behavior without diagnostics.
- A GPU path appears to require parsing generated WGSL as the source of truth.
- A metadata rule cannot preserve its evidence/conflict source.
- A legacy RGBA8 path cannot be migrated but also cannot produce a structured
  reason.
- Preview/export parity breaks and the cause is not understood.

## Suggested Execution Order

Recommended order for the next agents:

1. Phase 1 - Report Contract Convergence.
2. Phase 3 - Native GPU Main Path Integration, viewer first.
3. Phase 2 - Legacy RGBA8 Containment and Removal.
4. Phase 6 - Preview/Export Parity and Golden Gates.
5. Phase 5 - Dirty Media Metadata Detection.
6. Phase 4 - Real Display Management.
7. Phase 7 - OCIO/GPU Backend Hardening as needed by phases 3 and 4.
8. Phase 8 - UI and Operator Diagnostics after underlying contracts settle.

This order is pragmatic: diagnostics and report contracts give every later
phase a verification wall, GPU integration then proves real execution, legacy
RGBA8 work reduces quality loss, parity tests prevent drift, metadata improves
input trust, display management handles the hardest platform-specific risks,
and UI labels come last so they can consume stable contracts instead of
reimplementing them.
