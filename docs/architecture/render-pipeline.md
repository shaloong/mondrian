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

Timeline evaluation consumes exact Timeline Time in a declared Sequence domain
and an explicit video Evaluation Grid. The current frame-oriented request is a
compatibility form: the renderer must resolve and retain the corresponding
Frame Position rather than treating the numeric frame field as universal time.
Subframe shutter/temporal samples use the same Timeline Time representation and
do not introduce a renderer-private tick scale.

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

Effect graph construction is part of render-plan evaluation and is fallible.
An enabled effect with no executable definition, unavailable runtime, invalid
resource, panicking builder, or invalid graph returns
`MondrianError::EffectGraphEvaluationFailed`. Preview and export therefore
receive no plan for that frame; only an explicitly disabled effect is omitted as
identity. Backend-specific blockers remain separate from this authoring/compile
admission decision.

`TimelineMediaPlan::source_time` is the sole media-decode target emitted by
Timeline evaluation. It remains exact through Preview scheduling, frame-store
identity, Export decode caching, and `PreviewDecodeRequest`; those consumers do
not rebuild seconds or microseconds. Without a media frame-rate override, the
Clip Time Transform result is preserved exactly. An explicit override creates
one declared source Evaluation Grid and applies `Floor` once. A
`TimelineNestedSequencePlan` likewise retains child-local exact time; Preview
and Export project it onto the referenced child Sequence's own frame rate when
they recursively evaluate that child. Parent frame numbers are never reused as
child frame numbers.

`timeline_composite` exposes one color-managed composition contract:
`composite_timeline_elements_color_frame(...)`. It returns a typed
`CpuColorFrame` whose descriptor records domain, encoding, residency, dimensions,
color space, and RGB/coverage alpha association. Viewer preview and export must consume this typed working-frame
contract, then apply their respective working -> output boundary through
`RenderOutputColorBoundary` and renderer color stage execution helpers.
Bare RGBA8 buffers are valid only at source import, debug/golden snapshot, UI
presentation readback, and CPU encoder boundaries. They are not a renderer-stage
exchange format.

Program and nested-sequence working canvases use transparent black as their
initial value and retain straight coverage alpha through CPU and GPU
compositing. Viewer background/checkerboard treatment is presentation-only and
must not mutate the Program frame. Export selects an explicit
`ExportAlphaMode`: `Preserve` keeps straight alpha only for a validated
alpha-capable codec/container contract, while `FlattenBlack` composites over
scene-linear black before the final output transform. Codec selection alone
must never imply alpha preservation, and setting alpha opaque after an encoded
output transform is not a valid flatten operation.

`LinearFloatSource` accepts either an external scene-linear `ColorSpace` or an
internal `WorkingColorSpace`. Its frame descriptor preserves that role through
`ColorFrameSpace::Color` versus `ColorFrameSpace::Working`, and the CPU input
executor requests the corresponding typed OCIO endpoint before producing the
sequence working frame. When a decoder supplies external ACES2065-1/ACEScg or
linear RGB float samples, the renderer bypasses RGBA8 without mislabeling them
as pre-existing working frames. Media decoders must opt into this entry point
explicitly.

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
typed public frame remains straight-alpha. The Viewer spatial Module records its
horizontal RGBA32F intermediate as `PremultipliedCoverage`; shader uniforms are
derived from the input/output descriptors, and the vertical pass restores the
declared straight/opaque output contract. Premultiplied frames are rejected at
OCIO, effect, composite, display-calibration and output seams. Affine geometric transforms (scale, rotate,
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

Compiled effects also carry a backend-neutral color-domain plan. A graph whose
nodes require log/perceptual, display-linear, or display-encoded RGB is not a
legacy effect. `TimelineEffectColorRuntime` maps the sequence working domain and
named effect domains to exact OCIO identities, and the CPU timeline compositor
executes each planned edge in-place around the relevant graph node. Preview and
export both supply their project `ColorContext` to this renderer-owned boundary.
The float effect-output cache includes the engine/config/working-space identity.
If a processor cannot be resolved, `timeline_composite` returns a structured
`TimelineCompositeError` identifying execution or media/solid/adjustment domain
blockers and publishes no working frame. It must never execute those nodes in
scene-linear by accident or route them through the RGBA8 compositor. Data and alpha-domain graph errors
remain blockers rather than color-conversion requests. GPU graph lowering also
remains blocked until the GPU scheduler materializes the same OCIO edges.

Encoded decoded media enters the graph as a typed source/import RGBA8 boundary
(`CpuEncodedColorFrame::source_rgba8`). Scene-linear planar-f32 decoder output
enters as `LinearFloatSource`, so app preview, thumbnails, and export bypass
RGBA8 quantization while preserving the external source color identity.
Synthetic float data and effect graph intermediates use the same typed float
entry point.
Preview and export decode keys retain `DecodedVideoRangeContract`, not only an
already-flattened range value. Auto work can therefore consume a frame-level
range and fall back to the probe, while user Full/Limited overrides remain
distinct cache identities and authoritative sampling policy.
CPU preview/export fallbacks use `RenderInputTransform` plus
`execute_cpu_source_input_stage(...)` to dispatch the typed source to the
RGBA8 or float executor. GPU preview retains that same `CpuSourceColorFrame`
and executes the input transform without first materializing a CPU working
frame. Timeline media layers therefore carry typed working frames, not naked
RGBA slices. `CpuColorFrame` stores its linear
`WorkingRgbaF32Frame` payload in shared immutable storage so preview caches, lazy CPU
fallbacks, and export scheduling can clone the typed frame contract without
deep-copying a full 16-byte-per-pixel working frame. Copies that need owned
mutable float data must happen explicitly at execution boundaries.

The GPU compositor accepts renderer-owned working frames produced by native
decoder import and applies non-singular media affine transforms plus supported
fused point effects in one working-space render pass. These operations must not
materialize a CPU frame or schedule GPU readback; readback is reserved for an
explicit presentation, debug, or encoder boundary.

Rec.601 PAL/NTSC delivery keeps its distinct primaries, transfer, and matrix
tags through the export signal contract; swscale matrix selection and
post-encode validation are derived from that same contract.

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
`RenderOutputColorBoundaryFloat::program_scopes(...)` is the renderer-owned CPU
reference seam for program video scopes. It measures the boundary's encoded
float pixels with their exact output color-space identity, preserving 10-bit/HDR
signal precision and keeping monitor adaptation outside the measurement. The
real-time GPU path must implement equivalent GPU reduction rather than reading
the full output texture back to the CPU.

`GpuProgramScopesRuntime` is that real-time path. A demand-driven request
records atomic-u32 histogram, waveform, vectorscope, and signal-excursion counts
directly against the retained display-encoded Program Output texture. A second
compute pass materializes three RGBA8 linear display textures for the UI. The
count buffer, pipelines, display textures, uniforms, and display bind group are
retained by request shape; normal playback performs no scope readback. The only
readback is a test-only 2x2 GPU/CPU reference comparison.

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
Every `OcioGpuShaderRequest` carries the exact `ColorEngine`; this engine is part
of the shader-cache key and is used by core while selecting the config and
extracting the Processor shader. A Standard plan cannot be reused by ACES or
Custom OCIO solely because source, destination, display, and view strings match.
Normal project-engine switching does not flush unrelated warm shader plans.
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
export frame contract selected from delivery sample depth: 8-bit delivery writes
RGBA8 raw-video bytes, while 10-bit and 12-bit delivery read the renderer GPU
`Rgba16Float` boundary and packs normalized channels into FFmpeg `rgba64le`
pipe bytes. When GPU output is unavailable, 10/12-bit delivery CPU fallback
must use the renderer-owned `execute_cpu_output_boundary_float(...)` helper,
which applies the working -> output OCIO float transform without u8
quantization. The caller flattens the float result into `[f32]` and uses
`ExportFrameContract::pack_rgba_f32(...)` to produce `rgba64le` pipe bytes.
If both the GPU output path and renderer-owned CPU float helper fail, 10/12-bit
export fails closed. It must never manufacture an `rgba64le` payload from an
RGBA8 boundary. Export diagnostics record `FloatBoundaryUnavailable`, retain
the independent GPU fallback reason, and emit the renderer error before the job
can produce a misleading high-bit deliverable. Health report schema 3 names
this evidence as an output-precision failure, not a fallback.
Tone-mapped export delivery must also be explicit. If an export color context
requests tone mapping but the final export output boundary does not carry an
OCIO export view/display-view transform, the export health report must fail
with a structured output-transform issue instead of relying on the color-space
pipeline's `tone_map` flag. That flag is not a substitute for an OCIO view
transform.
After encoding, `mondrian-export` runs ffprobe and validates the actual encoded
pixel format, video range, primaries, transfer characteristic, and matrix
against expectations derived from the same export signal contract. Encoder
success without matching signal evidence is an export failure.

## Internal Float Precision

Working-space CPU frames and the GPU compositor use 32-bit float. This is a
fixed correctness contract, not a user preference. Native normalized encoded
sources and encoded output boundaries may use 16-bit float because their
values are bounded and they cross into a 10/12-bit delivery or a subsequent
32F working transform; OCIO LUT resources remain 32F.

Deterministic precision tests measure the current budget:

| Case | Measured worst error |
|---|---:|
| f16 round-trip over normalized `[0,1]` | `0.000244141` (`0.9998` 12-bit code) |
| f16 round-trip over scene-linear `[-16,16]` | `0.000488043` relative, `0.00390625` absolute |
| Hypothetical 256-layer HDR composite with f16 storage after every pass | `0.00274086` absolute |

The accumulated composite result is why working targets remain
`Rgba32Float`. A user-selectable 16F/32F working toggle would allow projects to
change numerical semantics and is not offered. At 3840x2160 one RGBA16F target
is 63.28 MiB and one RGBA32F target is 126.56 MiB; compositor ping-pong targets
are 126.56 MiB versus 253.13 MiB. The 2x memory/bandwidth cost is accepted for
working correctness, while bounded encoded boundaries retain 16F. A future
float deliverable or an unbounded intermediate must use a separately validated
32F boundary rather than relaxing this policy globally.

Current CPU preview/export execution uses `execute_cpu_input_stage(...)` for
encoded source boundaries and `execute_cpu_input_stage_float(...)` for
scene-linear decoder payloads. It uses
`execute_cpu_output_boundary_rgba8(...)` for final display or export output,
and `execute_cpu_output_boundary_float(...)` for
10/12-bit delivery CPU fallback. The float helper delegates to
`CpuRenderColorStageExecutor::output_transform_float(...)` and returns a
float `CpuColorFrame` with output transform applied; app/export code should
not instantiate the final-output executor directly or call
`execute_cpu_output_stage(...)` for final timeline output. Direct
`CpuColorTransformExecutor` usage is limited to renderer internals and its
focused unit tests.

GPU input transforms have their own renderer contract instead of piggybacking
on final-output plans. `RenderGpuInputStageResourcePlan` accepts a decoded
`CpuSourceColorFrame` in the `Source` domain. Encoded RGBA8 uploads as
`Rgba8Unorm`; scene-linear f32 uploads as `Rgba32Float` without quantization or
an intermediate CPU OCIO transform. It then records the OCIO GPU input
transform and produces a GPU-resident linear
working frame in a renderer-selected `Rgba32Float` texture. The working format
is not a caller option. It rejects CPU-only plans, plans with native GPU
blockers, non-source uploads, and non-working outputs. This is the guarded bridge for
preview playback to move input OCIO off the CPU. The runtime-owned entry point
is `RenderGpuOutputBoundaryRuntime::record_wgpu_input_stage_owned_backend(...)`,
which reuses the same renderer-owned shader cache, backend-prep cache,
backend-object cache, frame-id allocator, and frame table as output boundaries.
`GpuFrameCompositor` can consume the resulting `GpuColorFrameHandle` directly
as a media layer through `GpuCompositeLayerSource::GpuFrame`, so the planned
preview path can become GPU input transform -> GPU working composite -> GPU
output boundary without re-uploading that media layer. CPU sources still
require one host-to-device upload; native decoder surfaces use the separate
low-copy import path below.
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
The app viewer path now carries each decoded media layer's
`CpuSourceColorFrame` plus `RenderInputTransform` alongside its lazy CPU
working-frame fallback. During
window recording it first tries `record_wgpu_input_stage_owned_backend(...)`
for each eligible media layer, feeds successful outputs to
`GpuFrameCompositor` as GPU-resident working frames, and records a structured
CPU-working-upload fallback only for layers whose GPU input stage fails.
`PreviewDecodeOutcome::Frame(RgbaFrame)` becomes the encoded variant;
`PreviewDecodeOutcome::FloatFrame` becomes the shared scene-linear float
variant and uses the same GPU input stage.
`PreviewDecodeOutcome::NativeGpuFrame` must flow through the renderer native
decoded-frame import contract instead of being wrapped in `CpuEncodedColorFrame`
or silently transferred to CPU.
On Windows, native import copies the FFmpeg-owned D3D12VA surface into a
renderer-created shared NV12/P010 texture before YUV conversion. The bridge
retains the source `ID3D12Resource` and decode fence only until its copy-ready
fence completes; it must not defer that release until the next frame is
imported. A bounded decoder may need that same surface in order to produce the
next frame, so next-import retirement creates a circular wait even though the
Viewer output is already independent. `ViewerGpuExecutionRuntime` exposes a
non-blocking completed-source retirement operation. The Headless Adapter calls
it after its explicit GPU completion wait, while the Window Adapter advances it
at the beginning of every Preview prepare tick, including ticks that currently
have only a Loading candidate. Completion-query failure remains typed and
fail-closed. Renderer-owned bridge textures, pipeline state, and completed
Viewer outputs are unaffected; only the decoder-source command-lifetime lease
is retired.
Both `CpuEncodedColorFrame` and `LinearFloatSource` store decoded payloads in
shared immutable storage so preview media-cache hits, GPU source contracts, and
queued preview frame clones do not deep-copy a full source frame. GPU upload
plans preserve that shared u8/f32 payload and expose packed bytes only at
`Queue::write_texture`; float source upload therefore does not allocate a
second full-frame byte buffer. CPU-transform boundaries may
still request owned mutable bytes, but that copy must be visible at the boundary
rather than hidden in ordinary frame cloning. Media decode callers should use
the shared `CpuEncodedColorFrame` constructors when handing a decoded
`RgbaFrame` to the renderer.
Telemetry must report the actual path as `GpuOcio`, `CpuOcio`, or a mixed
variant; it must not infer GPU residency from preview-plan eligibility alone.
When a preview frame contains media layers, the viewer frame-residency payload
also carries an app-layer native video import readiness report. That report is
computed from media decoder residency, platform import probing, and renderer
native import support. Today it reports `CpuDecodedMedia` for CPU RGBA8 and
RGBA-f32 preview media; zero-copy and low-copy readiness
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

For audio, `TimelineExportSnapshot.media[AssetId]` freezes only stable Component
bindings reachable from the captured Sequence closure. Each binding carries an
absolute physical stream index, native layout, and the source fingerprint that
authorized it. Export resolution cannot consult the live Asset Library, fall
back to `0:a:0`, or reinterpret a missing Component; it uses the same standard
channel-matrix lowering as realtime Playback and fails the job when the frozen
binding or file revision no longer matches. Media decode returns native-layout
PCM; the shared prepared Contribution applies the same canonical Component
matrix used by Playback before any Sequence processing.
The frozen Sequence `AudioChannelLayout` is authoritative for Program
execution. Export's requested packaging layout is a distinct delivery contract
applied after the selected Program Output. The current Adapter admits only
versioned standard delivery pairs; unsupported named/custom or Discrete pairs
fail before encoding instead of being reduced to a matching `-ac` count.

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
Export jobs carry the evidence inside each
`TimelineExportSnapshot.media[AssetId]` dependency, and
preview emits the same diagnostic summary when missing-metadata policy rejects a
media asset. Preview also stores the latest viewer-request rejection as
`PreviewColorRejection` so panels, diagnostics, and automated smoke tests
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
`app::viewer_gpu_output_health` so Rust smoke tests and the CLI share one
budget implementation. The Module also owns the canonical attempt-outcome,
health-status, cumulative-count schema and the pure readiness classifier used
by the Window producer. The Window may collect display and texture-registration
facts, but it must not redefine `Ready`, `Degraded`, or terminal failure
classification. The ignored `viewer_gpu_output_budget_smoke` test reads
`MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT`, writes the same health report into
`MONDRIAN_PERF_OUTPUT` when configured, and fails the test on budget violations.
`app::viewer_gpu_output_residency` separately owns the canonical lowering from
declared preview layers and completed renderer execution into residency
evidence. Pre-execution records are explicitly `GpuWorkingCompositePlanned`
with `execution_observed=false`; only a completed renderer record may publish
`GpuWorkingCompositeExecuted`, zero-copy success, or observed upload/readback
counts. Platform import capability is an explicit input, never a hidden test
dependency or a substitute for frame execution. The Window captures one native
video import probe per renderer/Window Session and supplies that same immutable
snapshot to hardware-decode admission plus declared and executed residency;
per-frame re-probing may not splice different capability generations into one
attempt.
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
without an explicit golden update. App golden constants identify the exact
Mondrian Standard package and SDR View generation whose pixels they protect.

Golden images generated by Mondrian are regression evidence, not an independent
quality reference. `color_reference` defines the strict boundary for external
reference frames: provenance class, exact producer/specification version,
stimulus SHA-256, payload SHA-256, payload format, pixel encoding, dimensions,
alpha semantics, reference white, and nominal peak are all explicit. Unknown or
placeholder identities, stale hashes, shape mismatches, NaN/Inf, out-of-domain
PQ/HLG signals, and contradictory opaque alpha fail closed before comparison.
PNG, OpenEXR, and numeric JSON decoding remains injected through
`ColorReferenceDecoder`, keeping file codecs out of the realtime renderer while
allowing validation tools to preserve float negative and extended-range samples.
Only public-specification and independent-application origins qualify as
independent quality evidence; a Mondrian regression golden can use the same
import path but cannot promote itself to an external reference.

Scene-linear CPU/GPU validation uses the renderer-owned
`LinearRgbaAccuracyBudget` and `LinearRgbaAccuracyReport` contract. RGB and
alpha are evaluated independently: RGB reports maximum absolute error, mean
absolute error, RMSE, nearest-rank P99 error, and non-finite sample count, while
alpha uses its own coverage budget. A single peak-delta assertion is not an
adequate color gate because it cannot detect broad low-amplitude drift or
distinguish color arithmetic from alpha corruption. Real-wgpu point-effect
tests include negative and above-one working values and fail closed on NaN or
infinity.

The current Mondrian Standard package additionally has a digest-pinned numeric quality
corpus. The same generated working-space samples execute through the production
CPU OCIO SDR and PQ output boundaries; a separate implementation of the View is
not used as the oracle. The corpus enforces objective invariants rather than
self-comparison: finite normalized outputs, exact alpha preservation, neutral
axis, monotonic tone response, dense non-negative hue-boundary continuity, a
locally dense negative-channel continuity path, 10-bit ramp cardinality, and
legal/full-range signal codes. Public ColorChecker 2005 D50 xyY coordinates are
converted through an explicit Bradford D50-to-D65 adaptation and XYZ-to-linear
Rec.2020 matrix before entering that same production boundary. They provide
externally sourced stimuli, while future independent application frames provide
external output evidence on top of the invariants.

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
The production PQ GPU conformance gate applies that model to both the versioned
Mondrian Standard 1000-nit View and the independent ACES 2 reference View,
using the matching CPU OCIO processor as the semantic oracle.

## Engine-Owned Output View Transform

Export tone mapping is delivered through the selected color engine's typed OCIO
output View, not through the color-space pipeline. The three transform
contracts are:

- **Input color-space transform**: encoded `source → working` conversion via
  `RenderInputTransform`. Does not carry tone mapping.
- **Preview display/view transform**: viewer presentation resolved by
  `RenderOutputColorBoundary::from_intent(...)` with `target: Display`.
  受 monitor/surface/display policy 影响。
- **Export delivery view transform**: encoded delivery output resolved by the
  same constructor with `target: Export`. A named intent becomes an internal
  `RenderColorTransform::delivery_view(...)`, which dispatches through the
  explicit working-identity OCIO display processor.

### Single Output-Intent Authority

`SequenceSettings::root_program_color_context(project_cm)` resolves exactly one
`OutputTransformIntent` from the effective color engine and the sequence output
target. Mondrian Standard carries its immutable package identity, ACES carries a
target-aware preset, Custom OCIO carries the requested output target and resolves
its display/view/output-endpoint tuple from the pinned config identity, and an
explicitly display-referred workflow carries `Colorimetric`.

`DisplayManagementPolicy` is limited to monitor/profile identity, Viewer mode,
and tone-map policy. It cannot replace the engine-owned output View. The sequence
settings UI therefore exposes the engine and output target, but no independent
export display/view selector. This prevents preview pixels, encoded pixels, and
container color metadata from describing different output transforms.

`export_output_boundary_from_context(...)` in the export crate resolves
the boundary exclusively through `RenderOutputColorBoundary::from_intent(...)`.
Mondrian Standard resolves its output-target view from the pinned package;
Custom OCIO resolves only a binding matching the encoded output target;
colorimetric intent carries no view. If tone mapping is requested but the
resolved boundary has no view, export records
`ToneMapRequestedWithoutExportViewTransform`.

HDR metadata validation occurs before encoder launch, again before libx265
parameter construction, and after encoding against the finished bitstream. The
post-encode contract probes only the first decoded video frame when
`StaticHdrMetadataPolicy::WriteAuthored` was selected, because FFmpeg exposes libx265 ST 2086 and
MaxCLL/MaxFALL SEI as frame side data rather than stream fields. It compares the
encoded values at the x265 chromaticity/luminance quantization scales and fails
closed on missing, malformed, or changed metadata. Exports that do not request
`Omit` deliveries incur no frame-side-data probe. Source HDR10+ and Dolby
Vision metadata is never claimed as passthrough across rendered pixels: health
reports warn on referenced dynamic-HDR sources, and enabling the current
`WriteAuthored` request fails before encoding until a validated dynamic
metadata authoring backend exists. The effective
inherited/overridden engine determines whether a Standard output-target
contract applies. For Standard HLG/PQ, MaxCLL cannot exceed the View's fixed
1000-nit content peak. ST 2086 mastering-display peak remains independent
because it describes the authoring monitor, not the brightest content pixel; a
valid 4000-nit mastering display can therefore describe content formed by the
1000-nit Standard View.

Health reports distinguish export output-View availability from preview
display/view: `output_transform_issues` records when tone mapping was requested
but the engine-owned output intent could not provide a View. This is a Fail
condition in the health report. The action code is
`inspect_export_output_intent`.

## Preview/Viewer GPU Output Boundary

The preview/viewer path connects to the GPU color output boundary through
the app-window `prepare_viewer_gpu_preview()` function. The call chain is:

```
User scrub/play
  → RedrawRequested
  → prepare_viewer_gpu_preview(device, queue, session, host)
    → resolve the currently laid-out ViewerExternalTexturePresentation
    → host.gpu_preview_frame_for_current_state()
      → WindowPreviewAdapter::gpu_preview_frame_for_state()
        → resolve_sequence_elements()  [timeline render plan]
        → returns PreviewGpuFrame {
            working_input,
            program_output_boundary,
            monitor_adaptation,
          }
          where working_input is GpuComposite { layers }
    → RenderGpuOutputBoundaryRuntime::record_wgpu_input_stage_owned_backend()
        for media layers with source/input contracts
    → RenderGpuOutputBoundaryRuntime::record_wgpu_working_composite()
    → GpuViewerSpatialRuntime::record()
        → working-linear prefilter/crop/resize into visible Viewer pixels
        → transfer typed output into the shared OCIO frame table
    → RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_gpu_frame_owned_backend()
        → Program Output GPU transform (the same OCIO display/view intent as delivery)
        → retain the typed Program Output texture for scopes/cache diagnostics
    → optional GpuProgramScopesRuntime::record()
        → exact atomic aggregation from Program Output
        → GPU-only histogram/waveform/vectorscope display textures
    → optional RenderGpuOutputBoundaryRuntime::record_wgpu_intermediate_color_transform_owned_backend()
        → stock-OCIO colorimetric Program Output → monitor adaptation
        → no pass when both display identities match
    → optional GPU ICC monitor calibration
    → frame_renderer.register_external_texture_view()
    → queue.submit()
  → refresh newly published external Viewer frame and paint
```

The exact scopes aggregation groups four horizontally adjacent samples per
shader invocation and coalesces equal destination counters before the global
atomic write. Every pixel, tail pixel, excursion, histogram bin, waveform
sample, and vectorscope sample remains counted; flat or locally coherent image
regions avoid the worst global-atomic contention. The ignored
`program_scopes_gpu_perf` gate records two warmups and eight 4K samples with
hardware timestamps. It requires pooled pipelines/count storage/display
textures, GPU p95 <= 5 ms, and CPU command-recording p95 <= 0.5 ms by default.
Timestamp mapping and its CPU completion wait exist only in the test harness,
outside the production recording path.

The native `working_input` is a GPU-composited working texture. CPU fallback is
the separately diagnosed raster preview path; it is not a second interpretation
of this native stage graph.
The `program_output_boundary` is a `RenderOutputColorBoundary` carrying the
sequence Program Output intent. Monitor selection cannot replace that View.
`RenderMonitorAdaptation` separately carries the preview-only monitor identity;
it permits same-class SDR-to-SDR or HDR-to-HDR colorimetric conversion and
fails closed on SDR/HDR class changes because those require an explicit
rendering/tone-mapping policy. The Viewer record retains both Program Output
and final monitor handles in one pooled GPU resource table. Scopes consume the
former; presentation and optional ICC calibration consume the latter. Neither
route performs upload/readback between these stages.

Viewer GPU hardware timestamps and CPU command-recording attribution expose
Program Output and Monitor Adaptation as separate stages. Identical identities
still emit the ordered zero-duration monitor marker so profiling remains
structurally comparable without adding a render pass.

### CPU Fallback Path

When the window GPU output path cannot execute, the viewer falls back to the
raster preview path (`composite_resolved_preview`). It resolves the same root
Program Output context as the GPU Viewer, executes
`execute_cpu_program_monitor_boundary_rgba8()`, retains the encoded-float
Program Output, applies a second stock-OCIO encoded-float adaptation to the
sRGB UI atlas, and quantizes only after both transforms. The fallback therefore
cannot replace the program View with the atlas identity or insert an
intermediate RGBA8 round-trip.
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
Viewer presentation geometry is deliberately not part of the timeline/render
plan cache key. It scopes the external texture handoff instead: output extent
or normalized visible-region changes clear `Current`, allocate a new spatial
identity/key, and rerun only the spatial and display-boundary tail over the
current working candidate.

## GPU Working-Space Compositing

The preview/viewer path has a bounded native GPU working-space compositing path
through `gpu_compositor.rs`. For supported layer stacks, the app window records:

```
Resolved preview layers
  -> for each retained Windows D3D11 NV12/P010 media layer:
       ViewerNativeVideoImportRuntime
       -> bounded D3D11/DX12 shared-texture bridge entry
       -> native YUV shader into encoded-float Rgba16Float source texture
       -> OCIO GPU input transform into Rgba32Float working texture
       -> insert returned working resource into the composite frame table
  -> for each media layer with a source/input contract:
       RenderGpuOutputBoundaryRuntime::record_wgpu_input_stage_owned_backend()
       -> upload encoded RGBA8 once as Rgba8Unorm, or scene-linear f32 once as Rgba32Float
       -> run OCIO GPU input transform into Rgba32Float working texture
  -> GpuFrameCompositor::record()
     -> reuse one full-frame, opaque, identity GPU working layer directly
     -> sample GPU-resident media layers directly
     -> upload only media layers that already have a materialized CPU working fallback
     -> composite media/solid layers into an Rgba32Float working texture
     -> insert the working texture into RenderGpuOutputBoundaryRuntime frame table
  -> GpuViewerSpatialRuntime::record()
     -> prefilter strong downscales in working-linear light
     -> crop the visible source region and reconstruct it into Rgba32Float presentation pixels
  -> RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_gpu_frame_owned_backend()
     -> OCIO GPU display/output transform
  -> frame_renderer.register_external_texture_view()
```

Viewer publication is transactional. The window registers the newly completed
output and publishes its candidate identity before it unregisters the prior
texture key. Native-import backpressure or a failed candidate therefore leaves
the last completed frame registered instead of clearing the Viewer to white.
Intentional presentation invalidation (project/display/geometry changes) still
owns explicit clearing; an incomplete replacement does not.
The same execution evidence reports peak native-import contract pools and
bridge entries. Contract pools are globally bounded and completion-aware, while
each contract retains its own bounded in-flight bridge ring.

This keeps preview playback GPU-resident from input conversion through output
transform for the supported subset. Retained Windows D3D11 decoder surfaces use
a low-copy path, not zero-copy: the decoder array slice is copied once into a
shareable single-slice NV12/P010 texture, while YUV conversion, OCIO input,
working composite, and output remain GPU-resident. CPU-decoded media still uses
one typed source upload. The app media preview cache stores the decoded source
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
  RGBA8 or scene-linear f32 once and runs GPU OCIO input before compositing. If that input
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

`TexturePool` retains its fixed 2D, single-mip, single-sample textures by the
exact `(width, height, TextureFormat, TextureUsages)` contract with
size-class-based LRU reuse (8 per key, 64 total default). Release derives this
contract from the texture itself. Unsupported formats are never collapsed onto
an `Rgba8Unorm` key, and resources with different usage flags cannot alias.

### GPU execution timing

Renderer performance evidence separates four boundaries: CPU command recording
and queue submission, hardware GPU timestamp duration, CPU completion wait, and
end-to-end wall time. `GpuTimestampFrameTimer` owns the renderer-side timestamp
query contract and enables it only when both encoder timestamp features are
available. `GpuTimestampQueryRing` gives execution gates and continuous
telemetry a bounded asynchronous path: non-blocking polls recycle completed
slots, a full ring discards telemetry instead of back-pressuring presentation,
and offline gates perform one final wait only after the measured interval. The
production presentation loop must never introduce a per-frame query wait. A reported GPU duration therefore
means commands bracketed on the hardware timeline, not record/submit/wait wall
time. Gate reports also identify the adapter and retain compositor and spatial
pass diagnostics so regressions remain attributable to a concrete execution
path.
Each ring slot resolves five ordered hardware counters: frame begin, after
working compositing, after Viewer spatial processing, after the output color
boundary, and frame finish after optional display calibration. The four
adjacent deltas are reported alongside total GPU duration. Marker ordering is
validated before submission; a recording failure abandons and immediately
releases its slot without polling or waiting. Missing, duplicated, or
out-of-order markers fail the telemetry sample instead of producing misleading
attribution.
Successful Viewer records also expose CPU preparation attribution for native
input/import, working composite, spatial, output-boundary, and optional display
calibration stages. These timings end at command preparation and never claim to
be GPU execution time; they identify CPU-side bridge waits or per-frame object
construction before adding lower-level GPU pass timestamps.
Native import attribution further separates source validation, non-blocking
bridge acquisition, cached pipeline/intermediate preparation, YUV recording,
source-to-working color-stage preparation, resource extraction, and internal
bridge submission. Multi-layer candidates accumulate those costs before the
performance gate calculates each field's percentile.

OCIO GPU preparation caches the complete immutable static-pipeline assembly by
engine-qualified shader cache key, original shader hash, binding-contract hash,
and output texture format. A warm frame therefore does not rebuild resource
contracts, wrapper source, Naga artifacts, pipeline layouts, or render
pipelines. The concrete backend object also owns the wrapper bind-group layout;
per-frame input bind groups reuse that layout instead of creating another
layout object. LUT payload hashes are computed once when the immutable shader
plan is extracted, rather than walking a 57^3 payload during every frame.

The retained ignored `color_view_gpu_perf` hardware gate uses a spatially
varying GPU-resident 3840x2160 Linear Rec.2020 input and rotates Mondrian
Standard SDR, PQ, HLG, and the official ACES 2 1000-nit PQ preset. Sixty warm
samples per View report GPU timestamp and CPU-record p50/p95/p99, cold preparation, shader
size, LUT shapes, pass count, uploads, and readbacks. The measured timestamp
contains exactly one complete OCIO output pass and excludes input generation,
initialization, timestamp mapping, and CPU completion wait. Standard SDR, PQ,
and HLG require p95 <= 5 ms; Standard PQ additionally requires p95 <= 80% of
the like-for-like ACES PQ reference on the measured adapter. Environment
variables may tighten, but not silently disable, either budget.
The schema-4 report snapshots cache counters around the measured interval and
fails when any warm sample extracts a shader, prepares a static pipeline or
backend object, creates an OCIO wrapper input binding, allocates an output
texture, or evicts a pooled texture. Wrapper-binding and exact-contract texture
pool hits must cover every rotated View sample, so the timestamp budget cannot
mask recurring per-frame GPU object churn.

The July 2026 RTX 3050 Laptop/DX12 production-path smoke run used 20 warm
hardware-timestamp samples per View. The current segmented SDR v2 graph measured
1.094/1.109/1.381 ms p50/p95/p99 at 4K; Standard PQ measured p95 1.395 ms and
HLG p95 0.984 ms. All measured samples reused shader, static pipeline, backend
objects, wrapper bind groups, and output textures without upload or readback.
An earlier direct per-pixel OCIO `GradingRGBCurve` graph measured about 175 ms
and was rejected; the shipping graph bakes that same curve to a 4096-entry OCIO
1D LUT and retains the separate 61-cube gamut surface.

The same ignored integration target retains a separate schema-1 input and
primitive-transform gate. It rotates OCIO identity, Linear Rec.2020 to encoded
Rec.709 (matrix + OETF class), Rec.709 to working, and Sony
S-Log3/S-Gamut3.Cine to working. Identity and matrix/OETF use the production
GPU intermediate-transform recorder; decoded-source cases use the production
GPU-resident input-stage recorder. Source allocation and initialization happen
before timestamps, and measured samples contain one OCIO pass with neither
upload nor readback. The report carries exact source/destination identities,
processor cache id, shader/LUT resource shape, cold and warm CPU record cost,
GPU p50/p95/p99, and runtime-cache deltas. Its 5 ms default p95 budget and
creation-free warm-path gate are independent of the View-versus-ACES gate so
input-stage regressions cannot be hidden by output-stage results.

Renderer-owned color stages share a device-scoped exact-contract texture pool
across native import and Viewer output runtimes. A candidate returns its typed
resources only after its prior commands were submitted to the same ordered GPU
queue, or after recording was abandoned before submit. The next frame can then
reuse matching extent/format/usage storage without a CPU completion wait. Idle
resources use global LRU eviction, a three-resource per-contract cap, and a
384 MiB retained-byte cap; device reset clears the pool. Pool hits, misses,
releases, evictions, retained resources, and retained bytes remain observable in
runtime diagnostics.
CPU upload plans use the same pool before `queue.write_texture`; synchronous
export readback returns its completed input/output resources before releasing
the runtime lock, so later frames reuse storage without retaining per-frame
table entries.
The working compositor acquires both ping-pong accumulation targets from this
same pool. Multi-layer/effect semantics and pass ordering remain unchanged; the
pool only replaces repeated exact-contract allocation after an ordered submit.

### Integration Status

The production preview path uses GPU compositing for supported media, solid,
and adjustment elements:

- a single full-frame GPU working layer with Normal blend, full opacity,
  identity transform, and no non-identity effect bypasses the composite pass;
  the typed working handle is reused without allocating two 4K RGBA32F
  accumulators, while any semantic difference returns to the normal compositor;

- media frames must either already be in the sequence working color space or
  carry a source/input contract whose target working color space matches the
  sequence; differing source/preview extents are supported through
  inverse-affine GPU sampling;
- media and solid sources may carry a lowered ColorAdjust, WhiteBalance,
  Vignette, or Grain chain; effects run before source-over composition;
- adjustment layers process the lower accumulated working pixels and blend the
  result back with their opacity, matching the CPU float reference semantics;
- media and scene-linear procedural-solid layers may use invertible affine
  transforms; external effect-domain solids materialize before their OCIO
  round trip so authored effect/transform order remains unchanged;
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
