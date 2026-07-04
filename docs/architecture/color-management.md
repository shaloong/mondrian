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
`mondrian-core::mondrian_default_ocio_contract()` is the Rust-level product
contract for that asset. It lists the pinned config name, virtual path, default
display/view, scene-linear working role, supported Mondrian `ColorSpace`
mappings, and product-supported display/view pairs. Core tests validate the
embedded `.ocio` file against this contract so config edits fail loudly when
they break Standard mode.
`mondrian-core::validate_mondrian_default_ocio_contract()` is the production
validation gate for this asset. It returns a structured report after proving
the embedded config parses, matches the pinned roles/display contract, resolves
every Mondrian color-space mapping, builds CPU processors for the full contract
color-space matrix, and extracts GPU shaders for every non-identity
color-space transform plus every supported display/view transform.

## Engines

- `ColorEngine::MondrianSmart`: productized Standard/Simple policy over the
  Mondrian default OCIO source.
- `ColorEngine::Ocio`: explicit OCIO mode over `$OCIO`, a selected built-in,
  or a path source.

Explicit OCIO mode must load its selected config successfully. It must not
silently fall back to a different color science. Mondrian Standard follows the
same rule for the embedded `mondrian_default_ocio_v1` asset.
The `$OCIO` environment source is intentionally fail-closed: if the variable is
unset or points to a missing file, Mondrian reports that selected source as
invalid instead of scanning machine-specific standard paths.

## OCIO Global State Management

All OCIO config mutations are centralized in `mondrian_core::ocio` through
`OcioGlobalState`, a mutex-protected struct that owns:
- The loaded config path (or virtual path for built-in/embedded configs)
- The source identity (`OcioConfigSource`) that loaded the current config
- A monotonic generation counter for cache invalidation

`ocio_rs::set_current_config` is called inside the mutex guard so the C++ global
and Rust metadata are updated atomically. Concurrent `current_config()` callers
cannot see a half-updated state.

`ocio_config_generation()` returns the current generation counter. Renderer GPU
caches (e.g., `OcioGpuShaderCache`) include this generation in their cache keys
so stale entries are automatically invalidated when the config changes. Callers
can also explicitly call `OcioGpuShaderCache::clear()` when a config switch is
detected.

`ocio_config_source()` returns the source identity of the currently loaded
config, enabling diagnostics and source-aware idempotency checks.

## Project and Sequence

`ProjectColorManagement` stores the project-level engine. `SequenceColorManagement` can inherit from the project or override its own engine and policies.
Sequence editing-mode presets may update editing format defaults such as
resolution, frame rate, display format, and working color space, but they must
not reset `SequenceColorManagement`, HDR metadata preservation payloads, or
tone-map policy. Color-management state is explicit user/project intent and is
validated fail-closed after the preset is applied.

Important fields:

- `workflow`: DisplayReferred, SceneReferred, Aces
- `display_management`: monitor/profile reference, viewer SDR/HDR mode, and tone-map policy
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
Timeline compositing must therefore prefer direct float/linear operations for
supported working-space media, solid-color layers, and float-capable unary
effects, including media, solid, and adjustment blend modes that have a seeded
float pixel blend contract; any temporary RGBA8 path inside legacy effects or
transforms must remain explicit and visible in tests until that subsystem has
its own float/linear contract.

The app UI presentation surface is also part of the color contract. Mondrian
targets wgpu 30 or newer for presentation because surface color space selection
and `Surface::display_hdr_info(...)` are required display-management evidence.
The wgpu window session must select an explicit sRGB SDR surface format and
`SurfaceColorSpace::Srgb`, failing closed when the backend exposes only
non-sRGB presentation formats or the chosen format cannot be configured with
sRGB color space. It must not fall back to a non-sRGB swapchain, because that
would hide OS/backend display management errors behind a visually plausible but
untrusted viewer path.
Preview display color space is resolved from the sequence/project
`DisplayManagementPolicy` before building `RenderOutputColorBoundary`; the app
window then validates that boundary against its real display-output contract
(surface format, selected `SurfaceColorSpace`, SDR/HDR mode, per-format color
space capabilities, `display_hdr_info` snapshot, present modes, alpha modes, and
current monitor fingerprint). Display preview output is accepted only when the
requested output color space has a direct presentation contract on the selected
surface color space. Rec.709/sRGB requires `SurfaceColorSpace::Srgb`, DCI-P3
requires `DisplayP3`, Rec.2100 PQ requires `Bt2100Pq`, and Rec.2100 HLG
requires `Bt2100Hlg`; Rec.2020 SDR and camera-log acquisition spaces are not
presentation surfaces and are blocked until the viewer resolves them through an
explicit display transform. HDR preview output is therefore blocked on an
SDR-only surface instead of silently presenting through SDR, and wide-gamut
preview output is blocked on an sRGB surface instead of relying on OS/backend
implicit conversion. Window resize, scale-factor changes, and moves refresh the
display-output contract; any contract change invalidates the GPU viewer output
texture and output-boundary runtime frame resources, and surface format or
color-space changes rebuild the UI frame renderer before presenting again. EDR,
monitor ICC correction, true HDR swapchains, and dynamic per-monitor profile
switching require additional platform-specific contracts before they can be
enabled.

GPU-resident color frames use `GpuColorFrameHandle`, a renderer resource-table
handle with the same `ColorFrameDescriptor` contract. CPU/GPU transfers are
scheduled explicitly by `RenderColorStagePlan` nodes rather than hidden inside
color conversion helpers.
When a GPU OCIO stage has no upload/readback requirements and no native
blockers, `RenderGpuColorPassSchedule` binds the source `GpuColorFrameHandle`,
target `GpuColorFrameHandle`, `RenderColorTransformGpuPlan`, and
`OcioGpuWgpuRenderPassNodePlan` into a schedulable unit. It fails closed if the
frame descriptors, residency, extents, or OCIO resource keys differ.
GPU transforms that cross an encoded/linear boundary must declare wrapper-side
transfer operations in `OcioGpuWgpuWrapperColorContract`. Source/import ->
working passes decode OCIO output into `LinearFloat`; working -> output passes
encode sampled linear values before invoking the OCIO program. The wrapper color
contract participates in the wrapper link/source/render-pipeline hashes, so
pipelines with different color-domain semantics cannot share the same shader.
Display/view output boundaries carry an explicit `RenderOcioDisplayView` when
the caller wants OCIO presentation semantics instead of a color-space delivery
transform. CPU display/view boundaries execute through the OCIO display
processor. GPU display/view shader extraction is modeled with
`OcioGpuShaderRequest::DisplayView`; before Naga translation, Mondrian lowers
OCIO legacy combined LUT samplers such as `uniform sampler1D` into explicit
wgpu texture/sampler declarations using the OCIO descriptor binding contract.
Logical 1D LUT sampling is represented as a 2D texture sample with a fixed
second coordinate, matching the existing LUT upload contract.

Preview and export final transforms are renderer execution concerns. App and
export crates build a `RenderOutputColorBoundary` from their `ColorContext` and
pass typed working frames to renderer stage execution helpers; they must not
construct display/export `RenderColorTransform` values or duplicate
working -> output conversion logic locally. CPU reference execution uses
`execute_cpu_output_boundary_rgba8(...)`, which returns encoded pixels plus
color/stage diagnostics in the same boundary result. App/export crates must not
instantiate `RenderOutputColorBoundaryExecutor::cpu_only()` directly. Native
GPU execution uses `RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend(...)`.
`RenderOutputColorBoundaryPlanner` owns CPU-only versus PreferGpu stage
selection for that boundary, and PreferGpu planning reports native blockers
instead of falling back to CPU stages. The resulting
`RenderOutputColorBoundaryStagePlan` is also the only supported bridge from
final-output planning into `RenderGpuOutputStageResourcePlan`.
The app window session owns viewer GPU output telemetry next to that runtime.
Every preparation attempt records whether the external texture was already
current, the preview was loading or unavailable, the display contract blocked
presentation, wgpu recording failed, the output texture was missing, or an
external texture was registered/rejected. Successful and rejected registration
paths accumulate the actual `RenderColorStageDiagnostics` returned by the
recorded GPU output stage, so product logs and smoke tests can prove that the
main preview path used the intended upload + native GPU color + optional
readback schedule instead of inferring it from renderer tests. Viewer telemetry
also derives a last-attempt `health` summary from that same outcome, stage, and
display-presentation state, separating native GPU boundary readiness, display
contract readiness, presentation readiness, missing output textures, record
failures, and external texture rejection from cumulative counters. When
`MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT` is set, the app window appends these viewer
GPU output diagnostics as JSONL so real playback/scrubbing sessions can be
correlated with preview/export perf health reports. Each record carries the
last frame context (sequence id, timeline frame, preview dimensions, external
texture key, output target/color space, tone-map flag, and optional display/view)
plus cumulative ready/degraded/blocked/failed/rejected/waiting health counts, so
budget failures can be measured and traced back to the exact viewer boundary.

Input transforms follow the same rule. Decode/import code wraps source pixels in
`CpuEncodedColorFrame::source_rgba8`, builds a `RenderInputTransform`, and asks
the renderer stage executor for a `CpuColorFrame` in timeline working space.
App and export crates must not perform source -> working color conversion with
local `ColorPipeline` calls or direct low-level transform executor calls.

Renderer color-transform executors provide detailed diagnostics for both input
and output boundaries: backend, direction, typed input/output descriptors,
pixel count, and whether a temporary RGBA8 CPU boundary was crossed. Product
preview/export diagnostics should aggregate these records instead of inferring
color workload from UI or encoder code. Transform failures must carry the same
direction and typed boundary descriptors alongside the backend reason so UI,
logs, and export reports can identify which source, working, display, or export
boundary failed without parsing free-form OCIO text.

Renderer color-stage diagnostics sit one layer higher than transform
diagnostics. A `RenderColorStagePlan` records the scheduled CPU, GPU, upload,
and readback stages for a color boundary; `RenderColorStageExecution<T>` carries
the transform result plus that stage summary. Preview and export performance
smokes should report both layers; export jobs also persist the stage summary on
`ExportJobColorDiagnostics`. Transform diagnostics prove color semantics, while
stage diagnostics prove scheduling, residency, and GPU-readiness. GPU stage
diagnostics must preserve blocker breakdowns for shader module preparation,
OCIO LUT/uniform bind groups, fullscreen wrapper generation, and render-pipeline
preparation; dashboards should not rely on a single aggregate blocker count.
Preview performance smoke reports and export job diagnostics must also include
structured legacy RGBA8 composite reasons (`layer`, `reason`, `count`) derived
from renderer composite diagnostics via `TimelineCompositeColorPathSummary`.
These reason details are the migration contract for removing old
blend/effect/transform paths. The summary is evidence; the versioned health
report is the external contract. Dashboards, job panels, JSONL artifacts, and
CI gates must consume the report verdict/check/root-cause/action model instead
of re-inferring fallback causes from aggregate counters.
Export job color reports also carry the renderer-owned GPU blocker breakdown
and legacy RGBA8 breakdown next to the health booleans (`fully_float_linear`,
`gpu_path_ready`). UI labels, JSONL reports, and future CI budgets must read
those structured fields through the report rather than parsing totals or
rebuilding reason lists.
Export simulation performance reports written through `MONDRIAN_EXPORT_SIM_OUTPUT`
include a versioned `color_report` as the sole export color report contract.
`ExportJobDiagnostics::color_report` and `ExportJobColorDiagnostics::health_report`
are the shared constructors for job panels, telemetry, and perf artifacts. That
report embeds the same summary as threshold evidence, then adds a verdict, fixed
checks, root causes, and actions. Export simulation smoke tests fail closed
against the report by default: diagnosed frames must be present, no GPU
blockers, no transfer stages, no structured legacy RGBA8 reasons, fully
float/linear composites, and GPU path readiness are required.
Preview performance JSONL reports written through `MONDRIAN_PERF_OUTPUT` expose
the same idea as versioned preview color reports. Decode/cache and
continuous-playback smokes write `preview_color_report` as the sole preview
color report contract; it embeds the structured health summary, verdict, fixed
checks, root causes, and actions. Preview smokes fail closed on that report by
default: health must be present, composites must be fully float/linear, GPU path
readiness must hold, and GPU blockers, transfer stages, legacy RGBA8 reasons,
and missing-metadata policy rejections must remain zero.
Headless preview smoke reports must also carry GPU preview candidate counters:
request, ready/current/loading/unavailable outcomes, candidate pixels, and
external texture registration handoff counters. These counters prove that the
preview service produced a working-space candidate for the app-window GPU
output boundary; the window-session GPU output telemetry remains the authority
for whether wgpu recording and external texture registration actually succeeded.
Viewer GPU-output budget summaries now also retain the latest cumulative
`RenderColorStageDiagnostics` counters and the last full health flag set from
the app-window JSONL stream. The versioned viewer health report is the
operator-facing diagnostic layer above that summary: it groups checks into
capture integrity, viewer output, GPU color path, display contract, display
capability drift, and media color policy, and emits stable root-cause/action
codes. Tooling should treat the budget summary as the threshold evidence and
the health report as the canonical diagnostic interpretation.

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
Reverse media identification also lives on the same contract:
`ColorSpace::from_ffmpeg_tags(...)` resolves exact delivery tag triplets, and
`ColorSpace::from_ffmpeg_tag_hints(...)` contains the centralized partial-tag
interpretation rules used by media probing. `mondrian-media` must capture raw
FFmpeg/CICP tags and call those core helpers rather than maintaining a separate
color-space mapping table.
Acquisition/log identification is represented as structured
`VideoColorMetadataHint` values captured from container and stream metadata.
Hints currently recognize explicit Apple Log, S-Log3/S-Gamut3.Cine, and ARRI
LogC4 names and take precedence over generic CICP delivery tags. Future
container-specific side-data parsers should feed the same hint model instead of
adding another color-space decision path.
`VideoColorDetectionMethod` records whether the final media decision came from
a metadata hint, CICP tags, missing metadata, or decoder unavailability; UI,
logs, and export reports should surface that method instead of asking users to
infer provenance from raw tags.
HDR stream side-data is captured as `VideoHdrMetadataSummary` entries on
`VideoStreamInfo` and copied into `VideoColorDiagnostic`. The summary records
side-data kind, payload size, and a typed `mondrian-core` payload when FFmpeg
exposes a stable stream-side-data ABI. ST 2086 mastering display metadata and
CTA-861.3 MaxCLL/MaxFALL content-light metadata are parsed into shared core
value objects that can also format x265-compatible `master-display` and
`max-cll` strings. HDR10+, Dolby Vision configuration, and ICC profile payloads
remain presence/diagnostic records until dedicated parsers are introduced.
Sequence/export HDR preservation stores these typed core payloads directly and
fails closed when either ST 2086 mastering-display metadata or MaxCLL/MaxFALL
content-light metadata is missing; export must not synthesize hidden defaults.
Preview/export parity is protected by frame-level contracts: app preview tests
compare multilayer preview compositing against the export output boundary with a
stable RGBA hash, compare the shared preview/export color-health fields for the
same frame, and compare normalized report verdict/check/root-cause/action
signatures. Renderer golden tests cover lower-level compositing fixtures. GPU
and float-pipeline changes must keep these contracts green or update them only
with intentional visual-reference and diagnostics-contract changes.

Camera-log output is treated as a professional intermediate path. Export
validation rejects consumer delivery codecs for camera-log output and only
allows 10-bit-or-higher MOV/MXF ProRes configurations until richer metadata
carriage is implemented.

## Unknown Media

Media probing must preserve the distinction between explicit metadata and
policy assumptions. `mondrian-media::VideoStreamInfo.detected_color_space` is
the only field that means container/codec metadata identified a color space.
`VideoColorSpaceSource::MissingMetadata` and
`VideoColorSpaceSource::DecoderUnavailable` are diagnostic source states, not
permission for callers to bypass missing-metadata policy.
`VideoStreamInfo.color_metadata` stores the raw CICP-style primaries, transfer,
and matrix tags that FFmpeg reported, including numeric code, tag name, and
specified/unspecified state. Diagnostics, future UI warnings, and camera-log
identification should consume this raw metadata instead of parsing free-form
FFmpeg strings or inferring whether metadata existed from a resolved
`ColorSpace`.
`VideoStreamInfo.color_interpretation` is the structured explanation layer for
automatic detection. It carries the interpreted color space, confidence,
source/method, evidence, warnings, and whether the result is user-overridable.
Evidence distinguishes metadata hints, exact CICP triplets, partial CICP
matches, unsupported CICP tags, and decoder unavailability. Warnings must
surface ambiguity such as multiple camera metadata hints, hint-vs-CICP
conflicts, partial CICP inference, missing/unsupported tags, or decoder
unavailability. Warning payloads must preserve the original metadata source:
multiple-hint warnings keep selected and ignored hint key/value/scope records,
and hint-vs-CICP warnings keep the selected hint plus the raw CICP tag triplet.
Logs and export failures should consume `VideoColorDiagnostic::summary()` so
this provenance remains visible without duplicating formatter logic.
Machine-readable surfaces should consume `VideoColorDiagnostic::issue_summary()`
instead. The issue summary carries stable counts and flags for multiple
metadata hints, ignored hints, hint-vs-CICP conflicts, partial CICP inference,
missing/unsupported CICP tags, decoder unavailability, raw CICP presence, and
HDR side-data presence, so UI panels, perf JSONL, and export reports do not
parse diagnostic text.
Preview rejection logs and export failures must include the media asset id,
path, missing-metadata policy, and `VideoColorDiagnostic` summary so users can
identify whether the problem was missing tags, unsupported tags, or decoder
unavailability.

When detected media color space is missing, `MissingColorMetadataPolicy` resolves it as:

- Assume Rec.709
- Assume sequence working space
- Reject media

Clip-level `MediaInterpretation.color_space_override` takes precedence over detected metadata.
Preview and export both follow override -> detected metadata -> missing-policy
resolution through `MissingColorMetadataPolicy::resolve_input_decision(...)`.
The returned `InputColorResolution` is the shared diagnostic record for the
decision branch, including override, detected metadata, policy, sequence working
space, and whether the media was rejected. Export
`TimelineExportInput.asset_color_spaces` is a detected-only
metadata table; absence of an asset id means "resolve via policy", not
"fallback to Rec.709". Export `asset_color_diagnostics` carries the matching
per-asset diagnostic snapshot, including `color_interpretation`, and must be
used for failure messages and reports, not for choosing the transform.
`VideoColorDiagnosticIssueAggregate` is the stable rollup for these per-asset
snapshots: export job diagnostics carry it as `asset_issue_summary`, and
preview media smoke reports serialize the same aggregate as `media_color_issues`
for CI/perf JSONL. Both surfaces must scope the aggregate to assets actually
referenced by the active sequence graph, including nested sequences, instead of
blindly folding every cached diagnostic record in memory. App viewer rejection
messages and export-queue labels should read the same structured summaries
instead of inventing a parallel free-form issue taxonomy.
Preview diagnostics count every `InputColorResolutionSource` branch and perf
smoke reports derive both `explicit_metadata_or_override` and
`policy_assumptions` totals from those counters. A production color-path report
therefore distinguishes "metadata was known" from "the missing-metadata policy
kept playback moving" instead of hiding both behind one resolved color space.
The non-color `DataTexture` branch is reported separately from overrides so
utility/data-channel assets cannot be mistaken for user color-space overrides.
Export jobs accumulate the same counters in `RenderJob.diagnostics.color` while
the worker renders frames. UI and telemetry should read that job snapshot rather
than recomputing color interpretation from asset records.

## Display and Export

Display transforms belong at preview presentation. Export transforms belong at export encoding/tagging. Do not bake display transforms into timeline source data.

`SequenceSettings::root_preview_color_context(...)` builds the monitor
presentation context with a caller-provided display/output color space.
`SequenceSettings::root_export_color_context(...)` builds the delivery context
from the sequence output color space. Callers must choose one of these explicit
entry points instead of using a generic root render context.

Display management is explicit in the resolved `ColorContext`. Project settings
own the default `DisplayManagementPolicy`; sequences inherit that policy unless
they disable color-management inheritance. The policy carries the monitor/profile
reference, viewer SDR/HDR mode, and tone-map policy. `DisplayToneMapPolicy`
resolves the concrete `tone_map` flag for working -> output boundaries,
including HDR-working to SDR-output presentation, so preview, export, cache
keys, and future diagnostics do not infer tone mapping from scattered booleans.

For `MondrianSmart` and explicit `Ocio` engines, root sequence contexts copy
the currently loaded OCIO config's default display/view into the context when
one is available. Absence of a display/view is only valid when no current OCIO
config exposes defaults; it is not a fallback color pipeline.
Preview cache keys and timeline color diagnostics treat display/view as part of
the effective presentation context. A display/view change must invalidate cached
viewer frames even when the output `ColorSpace` enum is unchanged.

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
The OCIO binding contract is taken from the shader descriptor, not inferred
from generated WGSL or Naga output, and `OcioGpuWgpuResourcePlan` rejects
invalid descriptor counts, bindings, resource names, extents, and missing
uniform buffers before backend resource planning continues.
`OcioGpuWgpuBackendPrepRuntime` owns the pure preparation caches that assemble
prepared resource layout, wrapper binding/layout, wrapper link/source,
validated wrapper Naga artifacts, and render descriptor into one static
pipeline contract before concrete wgpu object creation begins.
`OcioGpuWgpuBackendObjectRuntime` owns concrete backend-object preparation for
that static pipeline: LUT/uniform upload, bind-resource validation, OCIO bind
group, stable wrapper input layout, wrapper shader modules, pipeline layout,
render pipeline, and render-pass node are cached as one backend object bundle.
`RenderGpuColorPassSchedule::record_wgpu_from_resources` is the single
preview/export-facing entry point that resolves those table entries, prepares
the wrapper bind group, and records the pass. `GpuColorFrameAllocationPlan`,
`GpuColorFrameUploadPlan`, and `GpuColorFrameUploader` are the shared renderer
entry points for allocating render targets and uploading CPU boundary frames
into `GpuColorFrameResourceTable`. `RenderGpuOutputStageResourcePlan` maps a
validated `RenderColorStagePlan` into those upload/allocation plans and a
transfer-resolved GPU transform that can be scheduled without callers mutating
transform internals. Its materialization path uploads/allocates resources and
preflights resource-table slots before inserting, so preview/export does not
hand-assemble table entries. When the stage plan ends in `ReadbackToCpu`, the
resource plan carries the matching `GpuColorFrameReadbackPlan`; preview/export
must resolve and record that readback through the renderer-owned output
boundary API instead of deriving it from texture format at the call site.
`RenderGpuOutputBoundaryRuntime` is the intended owner for long-lived preview
or export GPU output state: it keeps the OCIO shader cache, pure backend prep
runtime, concrete backend-object runtime, GPU frame id allocator, and GPU frame
table together for the backend lifetime.
`RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend(...)`
is the preview/export-facing sequencing point for this output boundary: it
plans the boundary, prepares runtime-owned backend objects, derives the resource
plan, materializes resources, schedules the pass, records the OCIO fullscreen
draw, and records the optional readback copy in one command encoder. Its
per-submission inputs are grouped in
`RenderGpuOutputBoundaryRuntimeOwnedBackendContext` so app/export code passes
device, queue, encoder, and load operation without hand-threading wgpu
pipelines, bind groups, pass nodes, or the frame table through each layer.
The record result carries `RenderColorStageDiagnostics`; preview and export
must use that diagnostics payload as the authoritative evidence for native GPU
OCIO usage, transfer/readback counts, and color-stage pixel budgets.
Callers that already hold a stage or resource plan may use the lower-level
recorders, but app/export scheduling should prefer the runtime-owned boundary API so
final-output policy remains renderer-owned. `GpuColorFrameReadbackPlan`
and `GpuColorFrameReadback` are the only renderer-owned GPU-to-CPU boundary for
encoded output frames; they currently read back only explicit `Rgba8Unorm` /
`EncodedRgba8` contracts and do not reinterpret float targets. Until
preview/export frame evaluation calls this output-stage recorder, CPU
processor execution is the correctness path and the cached
shader/module/upload/bind-resource/layout/bind-group/wrapper-
link/pipeline-contract plans are the production boundary for GPU integration
work.

Preview and export may therefore target different output color spaces while
sharing the same working color space, engine inheritance, workflow,
missing-metadata policy, and nested-processing policy.

## HDR/SDR

HDR output spaces include Rec.2100 PQ/HLG. Tone mapping is required when scene/HDR working data targets SDR output. HDR metadata can only be preserved for HDR output spaces.
