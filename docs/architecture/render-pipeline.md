# Render Pipeline

The intended render path is shared by preview and export:

```text
TimelineEvaluationRequest
  -> Timeline Evaluation
  -> FlatActiveClip
  -> TimelineRenderPlan
  -> TimelineRenderPlanElement
  -> Clip Sampling / Generated Source
  -> Effect Graph Evaluation
  -> Layer Composite
  -> Color Transform
  -> Display or Export Encode
```

## Current Implementation

`mondrian-renderer::timeline_render_plan` evaluates one sequence frame through
`evaluate_timeline_render_plan(source, request)`. The request carries:

- render intent: preview, export, thumbnail, or analysis
- render quality: interactive, draft, or final
- color target: display, export, or working space
- scheduler policy: whether frame dropping is allowed
- preview/export resolution scale

The result is a `TimelineRenderPlan` with ordered `TimelineRenderPlanElement`
values plus diagnostics for active clips, emitted elements, zero-opacity skips,
and unrenderable skips.

`collect_timeline_color_diagnostics(...)` reports the clip override, working
space, and output space together with their canonical `ColorEncodingSpec`
values. `collect_timeline_color_diagnostics_with_display_view(...)` attaches
the resolved OCIO display/view for presentation diagnostics. Renderer
diagnostics must consume `ColorSpace::encoding()` rather than duplicating
color-space metadata.

`evaluate_timeline_render_plan(...)` is the render-plan entry point. Callers
must choose an explicit `TimelineEvaluationRequest` intent so preview, export,
thumbnail, and analysis paths cannot accidentally share ambiguous defaults.

`TimelineRenderPlanElement` variants are:

- `Media`
- `Adjustment`
- `SolidColor`
- `NestedSequence`

Each element carries opacity, blend mode, transforms where applicable, effect graph, frame seed, and color/media interpretation data.

`timeline_composite` exposes one color-managed composition contract:
`composite_timeline_elements_color_frame(...)`. It returns a typed
`CpuColorFrame` whose descriptor records domain, encoding, residency, dimensions,
and color space. Viewer preview and export must consume this typed working-frame
contract, then apply their respective working -> output boundary through
`RenderOutputColorBoundary` and renderer color stage execution helpers.
Bare RGBA8 buffers are valid only at source import, debug/golden snapshot, UI
presentation readback, and CPU encoder boundaries. They are not a renderer-stage
exchange format.

Nested sequences return typed linear `CpuColorFrame` values to their parent.
Child working identities may be converted to the parent working identity by an
explicit OCIO working-to-working processor, but they are never sent through a
display/export transform or RGBA8 quantization during recursion. Preview and
export apply their display/deliverable transform exactly once at the root
boundary.

The CPU timeline compositor keeps identity-transform media, solid-color layers,
and float-capable adjustment layers in the typed float/linear working frame.
Timeline blend modes, including seeded Dissolve, are implemented by
`mondrian-effects`' float pixel blend contract and must not round-trip through
RGBA8 scratch buffers. Extended working values therefore remain available to the
final output boundary. Every existing built-in unary render operation, including
color adjustment, white balance, blur, sharpen, vignette, chromatic aberration,
grain, and LUT, runs in this path through the same `mondrian-effects` float
contract. Spatial effects use premultiplied-alpha sampling internally while the
typed frame remains straight-alpha. Affine geometric transforms (scale, rotate,
translate) are implemented
in the float/linear path using inverse-affine mapping with bilinear sampling,
so media and solid layers with non-identity transforms no longer require legacy
RGBA8 fallback. Custom processors without a float ABI and non-unary effect graph
nodes are handled separately: custom processors still require the diagnosed
RGBA8 fallback, while built-in Blend, Mask, MaskSource, and ordered MultiInput
nodes execute in the CPU float DAG. Clip masks rasterize directly to float matte
coverage. Solid layers must materialize their float source when they carry an
effect graph or affine transform, execute that same compiled graph, and then use
the shared layer sampler; diagnostics must never claim a solid effect is float
while bypassing its pixel semantics.
`TimelineCompositeDiagnostics` makes that fallback explicit: preview/export
callers can see whether a composite stayed on the float/linear path or fell back
to legacy RGBA8 because of transform or effect support. Blend-mode counters stay
in the diagnostic contract for future unsupported blend contracts, but current
built-in media, solid, and float-capable adjustment blend modes are expected to
remain float/linear. Preview and export diagnostics must aggregate these
counters so performance smoke reports and job panels can identify which legacy
color path blocked a fully float/linear frame. Callers should use
`TimelineCompositeDiagnostics::color_path_summary()` as the renderer-owned
contract for high-level path state and structured legacy reason breakdowns
instead of re-inferring path safety from individual counters.

Decoded media enters the graph as a typed source/import RGBA8 boundary
(`CpuEncodedColorFrame::source_rgba8`). Sources that are already in linear
float (synthetic test data, float decode output, or effect graph intermediates)
can use `LinearFloatSource` to bypass the RGBA8 quantization path entirely.
Preview and export must use `RenderInputTransform` plus
`execute_cpu_input_stage(...)` or `execute_cpu_input_stage_float(...)` to
produce a result carrying both `CpuColorFrame` and stage diagnostics before
building `TimelineMediaLayer`. Timeline media layers therefore carry typed
working frames, not naked RGBA slices. `CpuColorFrame` stores its linear
`WorkingRgbaF32Frame` payload in shared immutable storage so preview caches, lazy CPU
fallbacks, and export scheduling can clone the typed frame contract without
deep-copying a full 16-byte-per-pixel working frame. Copies that need owned
mutable float data must happen explicitly at execution boundaries.

The float transform path (`CpuColorTransformExecutor::input_to_working_float`
and `transform_float`) operates directly on f32 data without u8 quantization,
preserving HDR/log/10-bit precision. The `RenderColorTransformBackend::CpuOcioFloat`
backend indicates the float path was used, and `used_rgba8_boundary: false`
proves no intermediate quantization occurred. The existing RGBA8 path remains
available for decoded media, UI raster, debug/golden boundaries, and legacy
effects, with `used_rgba8_boundary: true` in diagnostics.
When a float output transform needs OCIO's contiguous f32 buffer, it must pack
directly from the borrowed `CpuColorFrame` pixels and avoid cloning the whole
typed `Vec<[f32; 4]>` before flattening.

Color-transform executors emit `RenderColorTransformDiagnostics` for input and
output boundaries. Preview diagnostics aggregate transform calls, transformed
pixels, and temporary RGBA8 boundary crossings so performance smoke tests can
catch accidental CPU-bound color work as the GPU path comes online.

`FrameCompositor` supports GPU batched compositing with texture pooling and can
return either RGBA readback or a GPU texture. GPU render graph nodes return
`GpuColorFrameHandle` values carrying the same descriptor contract as CPU
frames, rather than naked textures or byte vectors.

`RenderColorStagePlanner` sits between timeline evaluation/compositing and the
CPU/GPU color executors. It produces ordered stage plans for CPU transforms,
GPU OCIO transforms, upload, and readback. App and export crates should consume
renderer stage plans instead of deciding CPU/GPU/readback behavior locally.
`RenderOutputColorBoundaryPlanner` is the final-output boundary wrapper around
that planner. Its CPU-only mode is the current correctness execution path;
its PreferGpu mode must produce GPU/upload/readback stages plus explicit
blocker diagnostics instead of silently falling back to a CPU output stage.
`RenderOutputColorBoundaryExecutor` is the lower-level CPU final-output
execution boundary: callers choose an explicit strategy at construction time,
and the executor owns the final-output plan/execute sequence instead of
exposing low-level transform executors to app/export code. App/export CPU
reference callers use `execute_cpu_output_boundary_rgba8(...)`, which returns
encoded RGBA8 pixels plus transform/stage diagnostics as one boundary contract;
native GPU app/export callers must use
`RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend(...)`
with `RenderGpuOutputBoundaryRuntimeOwnedBackendContext`. GPU planning,
backend-object preparation, or recording failures are reported structurally and
must not silently run the CPU executor.
App/export integrations should hold a `RenderGpuOutputBoundaryRuntime` per
renderer backend lifetime. The runtime owns the OCIO shader cache, pure backend
prep cache, concrete backend-object cache, GPU frame id allocator, and GPU
frame resource table, and exposes an executor-level record method that accepts
only the per-submission device/queue/encoder/load-op context. Lower-level code
that already owns a prepared pipeline, OCIO bind group, and pass node may still
use `RenderGpuOutputBoundaryBackendContext`.
For native GPU OCIO execution, `RenderGpuColorPassSchedule` is the bridge
between the stage plan and backend recorder: it requires GPU-resident source and
target frame handles, a blocker-free `RenderColorTransformGpuPlan`, and a
matching `OcioGpuWgpuRenderPassNodePlan`. The schedule also owns the renderer
entry points that bind a resolved input frame view into the wrapper bind group
and record the output target through `OcioGpuWgpuRenderPassRecorder`; preview
and export callers must not duplicate OCIO pass assembly. Resolved GPU frame
views must come from `GpuColorFrameResourceTable`, which validates the
`GpuColorFrameHandle` contract before exposing the backend texture/view/sampler
payload. Callers that already have concrete wgpu resources should use
`record_wgpu_from_resources` so resource resolution, wrapper bind-group
preparation, output target construction, and recorder invocation stay in one
renderer-owned path. CPU boundary frames and empty GPU targets must enter that
resource table through `GpuColorFrameUploadPlan` / `GpuColorFrameAllocationPlan`
and `GpuColorFrameUploader`, not ad hoc texture creation in preview or export.
Use `RenderOutputColorBoundaryStagePlan::gpu_resource_plan(...)` to bridge a
blocker-free final-output GPU stage plan into `RenderGpuOutputStageResourcePlan`
so transfer descriptors, frame ids, and the scheduled GPU transform stay
consistent. CPU-only plans and GPU plans with native blockers must fail
structurally at this bridge instead of falling back to CPU execution. Then call
the resource plan's materialization helper to fill the resource table. If the
same stage plan ends in `ReadbackToCpu`,
`RenderGpuOutputStageResourcePlan` carries the `GpuColorFrameReadbackPlan` and
owns resource-table lookup plus readback-copy recording. Preview/export callers
should use
`RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend(...)`;
the runtime owns planning, resource-plan derivation, backend-object
preparation, materialization, schedule validation, pass recording, and optional
readback in renderer-owned order. The returned `RenderGpuOutputStageRecord`
must carry `RenderColorStageDiagnostics` for the recorded upload, GPU color
pass, optional readback, blocker breakdown, and pixel budget so preview/export
telemetry does not infer GPU execution after the fact. The blocker breakdown
must distinguish missing shader modules, OCIO resource bind groups, fullscreen
wrappers, and render pipelines. Lower-level renderer code that already owns a validated
`RenderOutputColorBoundaryStagePlan` or `RenderGpuOutputStageResourcePlan` may
call the matching stage/resource recorder with renderer backend contexts.
GPU-to-CPU output for encode, thumbnails, tests, or debug captures must use one
of those renderer-owned paths. Export readback must be serialized through the
export frame contract selected from sequence bit depth: 8-bit export writes
RGBA8 raw-video bytes, while high-bit-depth export reads the renderer GPU
`Rgba16Float` boundary and packs normalized channels into FFmpeg `rgba64le`
pipe bytes. When GPU output is unavailable, high-bit-depth export CPU fallback
must use the renderer-owned `execute_cpu_output_boundary_float(...)` helper,
which applies the working -> output OCIO float transform without u8
quantization. The caller flattens the float result into `[f32]` and uses
`ExportFrameContract::pack_rgba_f32(...)` to produce `rgba64le` pipe bytes.
Only when the float helper is unavailable or fails should the CPU fallback
pack an RGBA8 boundary into that pipe contract; that path must remain
diagnostically visible as a precision fallback (`CpuRgba8BoundaryPackedToHighBitDepthPipe`).
Health reports must distinguish GPU output fallback from output precision
fallback: the former explains why native GPU output did not execute, while the
latter explains why a high-bit-depth delivery contract was satisfied by bytes
derived from an RGBA8 CPU output boundary.
Tone-mapped export delivery must also be explicit. If an export color context
requests tone mapping but the final export output boundary does not carry an
OCIO export view/display-view transform, the export health report must fail
with a structured output-transform issue instead of relying on the color-space
pipeline's `tone_map` flag. That flag is not a substitute for an OCIO view
transform.
Current CPU preview/export execution uses `execute_cpu_input_stage(...)` for
source boundaries, `execute_cpu_output_boundary_rgba8(...)` for final display
or export output, and `execute_cpu_output_boundary_float(...)` for
high-bit-depth CPU fallback. The float helper delegates to
`CpuRenderColorStageExecutor::output_transform_float(...)` and returns a
float `CpuColorFrame` with output transform applied; app/export code should
not instantiate the final-output executor directly or call
`execute_cpu_output_stage(...)` for final timeline output. Direct
`CpuColorTransformExecutor` usage is limited to renderer internals and its
focused unit tests.

GPU input transforms have their own renderer contract instead of piggybacking
on final-output plans. `RenderGpuInputStageResourcePlan` accepts a decoded CPU
`CpuEncodedColorFrame` in the `Source` domain, uploads it as `Rgba8Unorm`,
records the OCIO GPU input transform, and produces a GPU-resident linear
working frame in a float texture (`Rgba16Float` or `Rgba32Float`). It rejects
CPU-only plans, plans with native GPU blockers, non-source uploads, non-working
outputs, and `Rgba8Unorm` working outputs. This is the guarded bridge for
preview playback to move input OCIO off the CPU. The runtime-owned entry point
is `RenderGpuOutputBoundaryRuntime::record_wgpu_input_stage_owned_backend(...)`,
which reuses the same renderer-owned shader cache, backend-prep cache,
backend-object cache, frame-id allocator, and frame table as output boundaries.
`GpuFrameCompositor` can consume the resulting `GpuColorFrameHandle` directly
as a media layer through `GpuCompositeLayerSource::GpuFrame`, so the planned
preview path can become GPU input transform -> GPU working composite -> GPU
output boundary without re-uploading that media layer. It is not a hardware
decode or zero-copy path until `mondrian-media` supplies an actual GPU
texture/hardware frame instead of CPU RGBA bytes.
Native hardware-decoded frames must enter through the separate renderer-owned
`GpuNativeDecodedFrameImportPlan` contract. That contract does not model the
decoder surface itself as a color-frame handle; it records the decoder handle
family, source texture format, and shader-visible video sampling contract
before validating renderer backend support. The sampling contract includes
range, YCbCr matrix, transfer characteristic, effective bit depth, and chroma
siting. Backend adapters must not bake in their own BT.709/BT.2020,
limited/full, PQ/HLG, or chroma-location guesses. A valid import produces only
the post-sampling/post-input-transform linear working `GpuColorFrameHandle`.
This keeps media residency reporting, OS texture import probing, and renderer
graph resource ownership decoupled.
The import plan carries the complete `RenderInputTransform`: OCIO engine
selection, tone-map policy, working space, and the required GPU backend. The
video sampling matrix and transfer must exactly match the resolved source color
space. A CPU OCIO backend or conflicting source/sampling contract fails before
GPU frame allocation. Backend adapters therefore cannot bake in their own
source-to-working transform or silently reinterpret PQ/HLG and
BT.709/BT.2020.
The renderer import helper must also validate the incoming native payload before
backend execution. `GpuNativeDecodedFrameImportSource` exposes a
`GpuNativeDecodedFrameSourceDescriptor` (extent, decoder handle family, and
source texture format), and `execute_native_decoded_frame_import(...)` rejects a
payload whose descriptor does not match the import contract.
The renderer implements this source contract for media-owned
`PreviewNativeDecodedFrame` payloads and owns the fallible
`DecodedVideoSurfaceFormat` -> `GpuNativeDecodedFrameTextureFormat` mapping.
Unsupported planar/unknown media formats return a structured source-format
error before backend planning; app/window code does not duplicate that format
table.
Backend support validation, video sampling validation, and output working-resource validation
are all required: a renderer backend must never be asked to import a
D3D11/NV12 contract while receiving a different native surface, and it must not
sample a YCbCr surface without explicit range/matrix/bit-depth/chroma metadata.
The app viewer path now carries each decoded media layer's `CpuEncodedColorFrame`
plus `RenderInputTransform` alongside its CPU working-frame fallback. During
window recording it first tries `record_wgpu_input_stage_owned_backend(...)`
for each eligible media layer, feeds successful outputs to
`GpuFrameCompositor` as GPU-resident working frames, and records a structured
CPU-working-upload fallback only for layers whose GPU input stage fails.
This CPU source contract applies only to `PreviewDecodeOutcome::Frame(RgbaFrame)`.
`PreviewDecodeOutcome::NativeGpuFrame` must flow through the renderer native
decoded-frame import contract instead of being wrapped in `CpuEncodedColorFrame`
or silently transferred to CPU.
`CpuEncodedColorFrame` stores decoded RGBA8 payloads in shared immutable
storage so preview media-cache hits, GPU source contracts, and queued preview
frame clones do not deep-copy a full source frame. GPU upload plans keep shared
immutable upload bytes so source upload scheduling and plan clones do not copy a
decoded RGBA frame before `Queue::write_texture`. CPU-transform boundaries may
still request owned mutable bytes, but that copy must be visible at the boundary
rather than hidden in ordinary frame cloning. Media decode callers should use
the shared `CpuEncodedColorFrame` constructors when handing a decoded
`RgbaFrame` to the renderer.
Telemetry must report the actual path as `GpuOcio`, `CpuOcio`, or a mixed
variant; it must not infer GPU residency from preview-plan eligibility alone.
When a preview frame contains media layers, the viewer frame-residency payload
also carries an app-layer native video import readiness report. That report is
computed from media decoder residency, platform import probing, and renderer
native import support. Today it reports `CpuDecodedMedia` for preview media
because FFmpeg still returns CPU RGBA bytes; zero-copy and low-copy readiness
must only appear after a real decoder GPU handle and renderer import support are
both present.

Stage helpers return `RenderColorStageExecution<T>`, not the raw transform
result. App, export, tests, and benches must read frames from `.result` and
aggregate `.stage_diagnostics` where they expose observability. Preview
diagnostics, export job diagnostics, and export performance smoke reports record
stage-plan counts, CPU/GPU stage mix, transfer stages, GPU blockers, and touched
pixels from actual renderer stage executions so later GPU execution work can
prove it removed CPU bottlenecks instead of merely moving code around.
`RenderColorTransformError::ExecutionFailed` must keep the
transform direction plus typed input/output descriptors with the backend reason;
renderer GPU output smoke additionally records a real wgpu upload + GPU color
pass + readback boundary and emits `MONDRIAN_RENDERER_GPU_OUTPUT_JSON` (or JSONL
via `MONDRIAN_RENDERER_GPU_OUTPUT_SMOKE_OUTPUT`) so dashboards can verify the
native final-output path without launching the app window. The smoke report
contains both raw stage/runtime counters and a versioned `health_report` as the
sole high-level contract. That report embeds the derived health summary, fixed
checks, root causes, actions, and evidence so dashboards can distinguish
skipped adapters, incomplete native GPU sequencing, backend-cache/object
preparation failures, readback-size mismatches, GPU blockers, and CPU/GPU
parity failures without reverse-engineering the counters. The renderer owns the
serializable contract types for this layer directly in production code:
`RenderGpuOutputFrameReport`, `RenderGpuOutputStageDiagnosticsReport`,
`RenderGpuOutputRuntimeDiagnosticsReport`, and `RenderGpuOutputHealthReport`.
Smoke tests, perf tooling, and future app/export integrations must build on
those types instead of carrying a test-private schema copy.

## Required Semantics

- Disabled clips and zero-opacity clips do not enter the render plan.
- Clip blend mode overrides track blend mode; otherwise track blend mode applies.
- Adjustment layers operate on lower accumulated pixels, not as standalone media.
- Solid colors are generated sources, not file-backed frames.
- Nested sequences must preserve the configured nested color-processing mode.
- Effect graphs are compiled from clip effects plus masks at the evaluated time.

## Preview vs Export

Preview and export may use different scheduling, cache lifetime, and readback strategy. They must share timeline interpretation, clip ordering, effect evaluation, blend semantics, and color-management decisions.

Preview media decoding must convert source media into the sequence working
color space before compositing. The source color space resolves from clip
override first, then explicitly detected media metadata, then the configured
missing-metadata policy. Resolved or assumed probe values are not media metadata;
callers must use `VideoStreamInfo.detected_color_space` and
`MissingColorMetadataPolicy::resolve_input_decision(...)` so preview and export
share the same `InputColorResolution` branch diagnostics. When preview/export
logs or UI need to explain why metadata was rejected or unsupported, they should
attach `VideoStreamInfo.color_interpretation` and `VideoStreamInfo.color_metadata`
raw CICP tags rather than rebuilding diagnostics from path or decoder text.
Export jobs carry this as `TimelineExportInput.asset_color_diagnostics`, and
preview emits the same diagnostic summary when missing-metadata policy rejects a
media asset. Preview also stores the latest viewer-request rejection as
`AppUiPreviewColorRejection` so panels, diagnostics, and automated smoke tests
can inspect the rejected asset id, path, missing-metadata policy,
`InputColorResolution` branch, working color space, and media diagnostic summary
without scraping logs. Playback prefetch must not overwrite this viewer-facing
snapshot.
Preview perf smoke reports and export frame diagnostics expose the same
per-branch input color-resolution counters, including override, detected
metadata, missing-policy assumptions, and missing-policy rejects. Data/non-color
texture resolution is reported as its own branch instead of being merged into
overrides, and it must come from asset payload classification rather than the
Interpret Footage color-space picker. These counters are part of the render-path
health contract: preview can remain real-time while still reporting whether it
was driven by authoritative media interpretation, non-color asset
classification, or by project policy. Aggregated report fields such as
`policy_assumptions` and `explicit_metadata_or_override` must be derived from
`InputColorResolutionSourceCounts` in `mondrian-timeline`, not hand-maintained
in preview/export reporting code.
Media metadata quality must travel through the same reporting path: export job
diagnostics carry `asset_issue_summary`, and preview media smokes serialize
`media_color_issues`, both derived from `VideoColorDiagnosticIssueAggregate`
rather than from parsed warning strings. The aggregate scope is the referenced
timeline asset set, not the whole asset-diagnostics cache, so preview/export
parity cannot be skewed by unrelated assets loaded in the project.
Timeline export writes accumulated render-path color diagnostics into
`RenderJob.diagnostics.color`. The worker updates this snapshot while frames are
actually rendered. App panels, logs, and JSONL reports should consume
`ExportJobDiagnostics::color_report()` or
`ExportJobColorDiagnostics::health_report()` as the stable semantic contract
instead of re-evaluating timeline state or rebuilding derived counters in UI
code. The embedded summary carries the same health concepts used by preview
reports:
explicit metadata/override totals, policy assumptions/rejections, data-texture
bypasses, legacy RGBA8 reason totals, float/linear completeness, GPU blockers,
GPU blocker breakdowns, legacy RGBA8 reason breakdowns, and GPU path readiness.
Export simulation perf JSONL includes this evidence only through the versioned
`color_report`; `color_health*` fields are not a supported external report
surface. Export simulation `passed` must include that report verdict: default
runs require diagnosed frames, fully float/linear composites, GPU path
readiness, zero GPU blockers, zero upload/readback transfer stages, and zero
structured legacy RGBA8 reasons.
Preview perf JSONL follows the same pattern with `preview_color_report` and,
for app-scale playback probes, `preview_playback_color_report`. The app preview
service owns the derivation from raw counters to report summary fields so perf
tests do not hand-maintain color health semantics. Preview media decode/cache
and continuous-playback smokes fail closed on those reports: default runs
require a present health summary, fully float/linear composites, GPU path
readiness, zero GPU blockers, zero upload/readback transfer stages, zero
structured legacy RGBA8 reasons, and zero missing-metadata policy rejections.
Live app-window viewer output diagnostics can additionally be persisted with
`MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT`. That JSONL stream records the last viewer
GPU output attempt, including the derived health summary, display/presentation
readiness, native GPU stage sequence, blocker breakdown, output texture state,
external texture registration outcome, and frame context needed to correlate
failures with a concrete sequence id, timeline frame, preview size, external
texture key, output target/color space, tone-map flag, optional display/view,
and the latest structured viewer color rejection when preview metadata policy
rejects an asset. The stream now also carries renderer-owned structured stage
evidence through `RenderGpuOutputStageDiagnosticsReport`, so downstream budget
or triage tools can migrate away from app-local flat stage counters without
redefining the renderer schema. It also carries the latest renderer runtime
snapshot through `RenderGpuOutputRuntimeDiagnosticsReport`, so viewer triage can
see shader-cache and backend-object failures from the same JSONL record stream.
The same records include cumulative ready/degraded/blocked/failed/rejected/waiting
health counts so smoke tooling can enforce viewer GPU-output budgets directly
from the JSONL stream. `viewer_gpu_output_budget` consumes this stream, emits a
versioned health report, and fails closed when ready/failed/blocked/rejected/
degraded thresholds are not met or when the reported cumulative `health_counts`
disagree with the statuses replayed from the JSONL records. Empty streams fail
explicitly with a `records` budget failure instead of being reported only as
missing ready frames. The report's embedded summary replays
`display_issue_summary` records into reason counts and payload-blocker counts,
and it replays viewer color rejections into a
`VideoColorDiagnosticIssueAggregate`, allowing smoke runs to fail on HDR,
wide-gamut, unsupported presentation, UI payload blockers, or metadata-policy
rejections even when a temporary health budget permits degraded frames. It also
fails closed when renderer-owned evidence is missing by default: each record is
expected to include both `accumulated_stage_report` and `runtime_report`, and
budget thresholds `max_missing_stage_reports` / `max_missing_runtime_reports`
govern temporary exceptions.
Per-reason display thresholds are part of the contract, so CI can relax one
failure class for investigation without silently tolerating the rest, and
unknown future display reasons still fail closed instead of disappearing inside
an aggregate display-issue allowance. The same evaluator lives in
`app_ui::viewer_gpu_output_budget` so Rust smoke tests and the CLI share one
budget implementation; the ignored `viewer_gpu_output_budget_smoke` test reads
`MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT`, writes the same health report into
`MONDRIAN_PERF_OUTPUT` when configured, and fails the test on budget violations.
Export diagnostics may expose additional preflight helpers, but final job-level
counters must be produced from the frame render path.
Preview exposes the same frame-level input color-resolution source counts from
its preview-intent evaluation path. Tests compare preview and export source
counts for the same timeline frame so display/output differences cannot hide a
divergence in media input interpretation.
Nested-sequence recursion depth is a timeline-level contract exposed as
`MAX_NESTED_SEQUENCE_RENDER_DEPTH`; preview, export video, export audio, and
diagnostic source-count paths must all use that same limit instead of local
hard-coded values.
Preview final-frame cache keys must include the effective color context so a
monitor/output transform change cannot reuse stale pixels from a previous view.
For OCIO-backed preview this includes the resolved display and view names, not
only the output color-space enum, because two views on the same display can
produce different presentation pixels.

Preview callers must use `TimelineEvaluationRequest::preview(...)`. Export
callers must use `TimelineEvaluationRequest::export(...)`. Any future thumbnail,
analysis, AI, or cache-warm path should add an explicit intent instead of
reinterpreting sequence state directly.

Preview and export health reports share a common color report vocabulary
defined in `mondrian_renderer::color_report_vocab`. Shared check codes
(`fully_float_linear`, `gpu_path_ready`, `gpu_blockers`, `transfer_stages`,
`legacy_reason_total`, `policy_rejections`) and normalized root-cause/action
codes are the canonical contract for cross-report comparison. Preview and
export reports may emit additional context-specific codes (e.g.,
`preview_gpu_color_stage_blocked`), but normalization functions map them to
shared canonical forms for parity testing. App and export crates must not
recompute color health semantics locally; they consume renderer-owned summary
fields and shared vocabulary codes.

The renderer test suite contains an explicit preview/export semantic-signature
contract. It allows request settings such as intent, quality, color target, frame
drop policy, and resolution scale to differ, but requires media source timing,
nested-sequence source timing, element order, blend/opacity, transforms, clip
interpretation, and nested color-processing mode to remain identical for the
same sequence frame. Renderer and app tests also pin the final color boundary:
for the same working frame, output color space, tone-map flag, and engine,
preview display and export delivery targets must produce identical RGBA pixels
while preserving distinct output domains in diagnostics. App-level parity tests
also compare `TimelineCompositeColorPathSummary`, the shared fields of the
preview/export color-health summaries, and the normalized versioned report
verdict/check/root-cause/action signatures for the same timeline frame, so
preview and export cannot silently diverge in float/linear versus legacy RGBA8
composite routing, stage scheduling, GPU blocker accounting, health booleans, or
diagnostic interpretation. The renderer golden suite includes a stable RGBA hash for
this preview/export Rec.709 boundary parity contract; intentional color-pipeline
changes must update that hash with the same care as image golden references.
The app preview suite also pins a stable Rec.2020-working to sRGB-output
multilayer preview/export RGBA hash, so preview and export cannot drift together
without an explicit golden update.

Scene-linear CPU/GPU validation uses the renderer-owned
`LinearRgbaAccuracyBudget` and `LinearRgbaAccuracyReport` contract. RGB and
alpha are evaluated independently: RGB reports maximum absolute error, mean
absolute error, RMSE, nearest-rank P99 error, and non-finite sample count, while
alpha uses its own coverage budget. A single peak-delta assertion is not an
adequate color gate because it cannot detect broad low-amplitude drift or
distinguish color arithmetic from alpha corruption. Real-wgpu point-effect
tests include negative and above-one working values and fail closed on NaN or
infinity.

Encoded SDR sRGB output validation uses a separate
`SrgbDisplayAccuracyBudget`/`SrgbDisplayAccuracyReport` contract. It converts
RGB code values through the explicit sRGB -> XYZ D65 -> Bradford-adapted XYZ
D50 -> CIELAB chain, then reports CIEDE2000 maximum, mean, and nearest-rank P99
error. Alpha remains coverage and has an independent code-value limit. The
real-wgpu plain output and OCIO display/view tests both use this contract, so a
small number of large errors and broad low-level drift are independently
bounded. The exact P99 implementation uses linear-time selection rather than a
full sort; this remains validation work and is not executed in the playback
frame loop.

This perceptual contract is deliberately named sRGB and rejects malformed
RGBA8 buffers. It must not be applied to Rec.709, Display P3, PQ, HLG, or
scene-linear data. HDR validation requires an absolute-luminance-aware model
and target display contract rather than relabeling CIELAB thresholds.

## Export Delivery View Transform

Export tone mapping is delivered through an explicit OCIO delivery view
transform, not through the color-space pipeline. The three transform
contracts are:

- **Color-space transform**: `source → working → output` conversion via
  `convert_pipeline*`. Does not carry tone mapping.
- **Preview display/view transform**: viewer presentation via
  `RenderOutputColorBoundary::display_view(...)` with `target: Display`.
 受 monitor/surface/display policy 影响。
- **Export delivery view transform**: encoded delivery output via
  `RenderOutputColorBoundary::export_view(...)` with `target: Export`.
  Carries the OCIO display/view transform (which includes tone mapping)
  into the export boundary. Uses `RenderColorTransform::delivery_view(...)`
  internally, which dispatches to `display_transform_float` / `display_transform`
  on the OCIO engine.

### Export Delivery View Policy

The export delivery view is configured through
`DisplayManagementPolicy::export_delivery_view: ExportDeliveryViewPolicy`,
which lives in both `ProjectColorManagement` and
`SequenceColorManagement`. Sequences inherit the project policy unless they
override it.

The sequence settings UI exposes this as a color-management source selector plus
an export delivery view selector. Sequence-local delivery view edits are active
only when color-management inheritance is disabled; otherwise export resolution
continues to use the project policy. UI code must write the typed
`ExportDeliveryViewPolicy` into `SequenceColorManagement.display_management`
instead of passing ad-hoc display/view strings directly to the export queue.

Policies:
- `None` (default) — no delivery view configured. Tone-mapped export fails
  closed with `ToneMapRequestedWithoutExportViewTransform`.
- `OcioConfigDefault` — uses the OCIO config's default display/view,
  resolved at render time via `ocio_default_display_view()`.
- `OcioDisplayView { display, view }` — uses a named OCIO display/view
  pair. Validated against the current OCIO config at resolve time.

`SequenceSettings::resolve_export_delivery_view(project_cm)` resolves the
effective policy into a `ResolvedExportDeliveryView` (or `None`/`Err`).
`root_export_color_context()` calls this resolver and populates
`ocio_display`/`ocio_view` from the result. If the resolver fails
(invalid display/view), the error is stored in
`ColorContext::export_delivery_view_error` for diagnostics.

`export_output_boundary_from_context(...)` in the export crate resolves
the boundary:

- `tone_map=true` + `ocio_display`/`ocio_view` present →
  `RenderOutputColorBoundary::export_view(...)` (delivery view path)
- `tone_map=true` + no view → plain `RenderOutputColorBoundary::export(...)`
  + `ToneMapRequestedWithoutExportViewTransform` diagnostic
- `tone_map=false` → plain export boundary, no view, no issue

Health reports distinguish export delivery view availability from preview
display/view: `output_transform_issues` records when tone mapping was
requested but no delivery view was available. This is a Fail condition
in the health report. The action code is `configure_export_delivery_view`.

## Preview/Viewer GPU Output Boundary

The preview/viewer path connects to the GPU color output boundary through
the app-window `prepare_viewer_gpu_preview()` function. The call chain is:

```
User scrub/play
  → RedrawRequested
  → prepare_viewer_gpu_preview(device, queue, session, host)
    → host.gpu_preview_frame_for_current_state()
      → AppUiPreviewService::gpu_preview_frame_for_state()
        → resolve_sequence_elements()  [timeline render plan]
        → returns AppUiGpuPreviewFrame { working_input, boundary }
          where working_input is either:
            - GpuComposite { layers } for supported simple layer stacks
            - CpuFrame(frame) after explicit CPU composite fallback
    → if GpuComposite:
        RenderGpuOutputBoundaryRuntime::record_wgpu_input_stage_owned_backend()
          for media layers with source/input contracts
        RenderGpuOutputBoundaryRuntime::record_wgpu_working_composite()
        RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_gpu_frame_owned_backend()
      else CpuFrame:
        RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend()
      → GPU output transform (OCIO display/view)
    → frame_renderer.register_external_texture_view()
    → queue.submit()
```

The `working_input` is either a GPU-composited working texture or a CPU
working-space fallback frame.
The `boundary` is a `RenderOutputColorBoundary` carrying the target display/view
for presentation. The GPU path executes the display transform on the GPU via
`RenderGpuOutputBoundaryRuntime`.

### CPU Fallback Path

When the window GPU output path cannot execute, the viewer falls back to the
raster preview path (`composite_resolved_preview`), which uses
`execute_cpu_output_boundary_rgba8()` as the explicit CPU presentation path.
GPU-output failures are recorded at the window boundary:

- `cpu_output_fallback_frames` — Number of frames using CPU fallback.
- `cpu_output_fallback_pixels` — Total pixels through CPU fallback.
- `PreviewGpuOutputBlocker::CpuFallbackRequested` — Typed blocker with reason.

CPU fallback is never silently used. Health reports distinguish:
- `Pass` — Clean GPU color output.
- `Warn` — GPU blocked but CPU fallback succeeded.
- `Fail` — Fail-closed color rejection.

### GPU Preview Cache Key

The `ViewerPreviewCacheKey` includes:
- `sequence_id`, `width`, `height` — Frame geometry.
- `plan_signature` — Hash of: working color space, output color space,
  tone map flag, color engine, display management policy, OCIO display/view,
  element signatures (media frame signatures, effects, transforms).

Changing display/view, OCIO config generation, or display contract
invalidates the cache key and forces re-rendering.

## GPU Working-Space Compositing

The preview/viewer path has a bounded native GPU working-space compositing path
through `gpu_compositor.rs`. For supported layer stacks, the app window records:

```
Resolved preview layers
  -> for each retained Windows D3D11 NV12/P010 media layer:
       AppUiNativeVideoImportRuntime
       -> bounded D3D11/DX12 shared-texture bridge entry
       -> native YUV shader into encoded-float Rgba16Float source texture
       -> OCIO GPU input transform into Rgba16Float working texture
       -> insert returned working resource into the composite frame table
  -> for each media layer with a source/input contract:
       RenderGpuOutputBoundaryRuntime::record_wgpu_input_stage_owned_backend()
       -> upload CPU decoded source RGBA8 once as Rgba8Unorm
       -> run OCIO GPU input transform into Rgba16Float working texture
  -> GpuFrameCompositor::record()
     -> sample GPU-resident media layers directly
     -> upload only media layers that already have a materialized CPU working fallback
     -> composite media/solid layers into an Rgba32Float working texture
     -> insert the working texture into RenderGpuOutputBoundaryRuntime frame table
  -> RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_gpu_frame_owned_backend()
     -> OCIO GPU display/output transform
  -> frame_renderer.register_external_texture_view()
```

This keeps preview playback GPU-resident from input conversion through output
transform for the supported subset. Retained Windows D3D11 decoder surfaces use
a low-copy path, not zero-copy: the decoder array slice is copied once into a
shareable single-slice NV12/P010 texture, while YUV conversion, OCIO input,
working composite, and output remain GPU-resident. CPU-decoded media still uses
one RGBA8 source upload. The app media preview cache stores the decoded source
frame plus the `RenderInputTransform` contract without eagerly materializing a
CPU working frame. CPU working frames are generated lazily only when the CPU
reference compositor or a runtime fallback actually needs them. It also removes
the extra `UploadToGpu` stage between working composite and output transform;
the renderer contract is covered by `from_gpu_working_frame()`.

### Capability Classification

- **`GpuNative`** — All media sources are GPU-resident, every executed layer
  uses Normal blend mode and a supported working-linear effect plan, and the
  executed stack has ≤5 layers. Native D3D11 media enters through the bounded
  low-copy import backend; procedural and adjustment layers require no import.
- **`GpuWithUpload`** — Layer structure supports GPU compositing, but at least
  one layer enters from CPU memory. The preferred media path uploads decoded
  source RGBA8 once and runs GPU OCIO input before compositing. If that input
  stage is unavailable, the app records a structured fallback and uploads an
  already materialized CPU working frame when one exists. Source-only preview
  cache entries fail closed for that GPU attempt instead of silently performing
  an unplanned CPU input transform inside the app-window render pass.
- **`CpuFallback`** — GPU compositing not possible. Reason is classified as:
  - `EffectRequiresCpu` — Effect graph needs CPU execution
  - `UnsupportedBlendMode` — Only Normal is GPU-supported
  - `UnsupportedTransform` — Transform cannot be represented by the GPU compositor
  - `FrameNotGpuResident` — Frame must be uploaded
  - `TooManyLayers` — Exceeds the bounded 5-layer GPU composite stack
  - `GpuUnavailable` — No GPU device/queue

### Texture Pool

`TexturePool` supports `Rgba8Unorm`, `Rgba16Float`, and `Rgba32Float`
formats with size-class-based LRU reuse (8 per key, 64 total default).

### Integration Status

The production preview path uses GPU compositing for supported media, solid,
and adjustment elements:

- media frames must either already be in the sequence working color space or
  carry a source/input contract whose target working color space matches the
  sequence; differing source/preview extents are supported through
  inverse-affine GPU sampling;
- media and solid sources may carry a lowered ColorAdjust, WhiteBalance,
  Vignette, or Grain chain; effects run before source-over composition;
- adjustment layers process the lower accumulated working pixels and blend the
  result back with their opacity, matching the CPU float reference semantics;
- media layers may use invertible affine transforms; solid layers currently
  require identity transforms to match the float reference compositor;
- all executed layers must use `BlendMode::Normal`;
- skipped leading/identity/zero-opacity adjustments do not consume capacity;
  the remaining executed layer count must be ≤5.

If any condition is not met, the GPU-preview candidate path records
`GpuCompositingDiagnostics { cpu_fallback_composites, first_blocker }` and
returns unavailable for that GPU candidate instead of running the CPU reference
compositor on the UI/event thread. Playback and buffering paths may show a stale
viewer frame or loading state, but they must not synchronously composite media
frames just to produce a fallback candidate. Paused still-frame preview may use
the raster CPU correctness path. This fail-closed gate is intentional:
unsupported effects, blend modes, transforms, or resampling must not silently
run through a visually different GPU approximation, and unsupported GPU
compositing must not make transport controls or window close unresponsive.
