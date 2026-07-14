# Color Management

Mondrian has one typed color-management pipeline. Proposed ADR-0005 defines
Mondrian Standard as a Mondrian-owned, immutable, versioned OCIO package and
uses stock OCIO as the default execution infrastructure. Mondrian Standard,
ACES, and Custom OCIO are product-level modes over that shared integration, not
three renderer engines. Missing processors or configs required by a selected
mode must surface as errors instead of falling back to different color science.

The Rust integration is `ocio-rs` 0.2.x with the `bundled` feature enabled, so
normal application builds exercise the real OpenColorIO bridge rather than a
stub runtime.

The bundled endpoint/input config is packaged as
`crates/mondrian-core/assets/ocio/mondrian_default_ocio_v1.ocio`. Standard mode
parses that immutable base and assembles its versioned product View in memory
with stock OCIO transforms; it never resolves an upstream `latest` alias or a
machine-local LUT. The base retains pinned OCIO Studio input definitions and
explicit ACES transforms for the separate ACES-facing capability surface, but
the Standard active/default View is not an ACES Output Transform.

`Mondrian Standard SDR v1` is a five-stage OCIO `GroupTransform`: the AP0 scene
reference is adapted to CIE XYZ D65 by an OCIO `BuiltinTransform`, converted to
FilmLight E-Gamut by a matrix, distributed through an OCIO log2
`AllocationTransform` covering -10 through +15 stops, formed by the pinned
57-cube AgX Base resource using tetrahedral reconstruction, and converted to
the display-reference connection space before display encoding. The resource
is the authored picture-formation transform, not a low-resolution bake of an
ACES processor. Its exact upstream commit, byte digest, domain, resolution,
and BSD-3-Clause notice are recorded in
`assets/ocio/MONDRIAN_STANDARD_SDR_V1_NOTICE.md`.

The same Standard SDR formation is registered against sRGB, Rec.1886 Rec.709,
Gamma 2.2 Rec.709, and Display P3 display color spaces. Output-target resolution
is explicit: sRGB, Rec.709, and P3 select their matching display rather than the
config's global default. Until a versioned Standard HLG/PQ View is present, an
HDR tone-map request fails closed and must never borrow an inactive ACES View.

Display-referred SDR projects do not invoke this scene View merely because it
exists: their output boundary remains direct colorimetric OCIO conversion.
Scene-referred workflows or an explicit tone-map policy select the Standard
View, so ordinary Rec.709-to-Rec.709 editing is not needlessly filmicized.
`mondrian-core::mondrian_default_ocio_contract()` is the Rust-level product
contract for that package. It lists the Standard package version, pinned config
name, exact config and whole-package SHA-256 digests, resource digests, virtual
path, default display/view,
scene-linear working role, supported Mondrian `ColorSpace` mappings, and
product-supported display/view pairs. Loading Standard mode recomputes the
digest and fails closed before parsing if the embedded content no longer
matches the versioned contract. Core tests validate the
embedded `.ocio` file against this contract so config edits fail loudly when
they break Standard mode.
`mondrian-core::validate_mondrian_default_ocio_contract()` is the production
validation gate for this package. It returns a structured report after proving
the embedded config and resources match their digests, the assembled config
matches the pinned roles/display contract, resolves
every Mondrian color-space mapping, builds CPU processors for the full contract
color-space matrix, and extracts GPU shaders for every non-identity
color-space transform plus every supported display/view transform.

## Color-Science Validation Primitives

OCIO-backed production nodes execute through OCIO. Independent accuracy
validation uses `mondrian-core::color_science`, which owns the finite
`CieLabD50` value type, explicit normalized sRGB -> D50 CIELAB conversion, and
CIEDE2000 implementation. These primitives are an oracle-facing measurement
layer, not a second production transform engine.

The sRGB conversion follows the W3C Color 4 sequence and uses one internally
consistent white reference through linear sRGB -> XYZ D65 -> Bradford D50 ->
CIELAB. Inputs outside the normalized encoded sRGB raster range fail instead of
being clipped. CIEDE2000 branch behavior is tested against all 34 supplemental
Sharma/Wu/Dalal 2005 reference pairs stored in the versioned
`crates/mondrian-core/tests/reference/ciede2000_sharma_2005.json` corpus. The
corpus records its source and citation, and intentional formula changes must
continue to satisfy its `1e-4` published-value tolerance.

The D50 CIELAB API is restricted to SDR display validation. Scene-linear
working values use numeric channel/RMS/distribution budgets instead. HDR
display validation follows ITU-R BT.2124: normalized full-range BT.2100 PQ is
decoded to absolute-luminance BT.2100 RGB in cd/m2, converted through ICtCp to
ITP, and compared with Delta E ITP. `Bt2100DisplayLinearRgb`, `CieXyzD65Nits`,
and `ItpDisplay` keep luminance and white-point semantics explicit. The Annex 4
reference calculation and PQ absolute-luminance endpoints are regression tests.
Out-of-gamut negative display-linear values are retained through ITP conversion
rather than silently clamped.

## Transform providers and output intent

- `ColorEngine::MondrianStandard { package }`: productized policy selecting the
  bundled, version-pinned Mondrian OCIO package. The persisted package identity
  requires its product ID/version, config ID/SHA-256, full package SHA-256,
  working-space ID/version, and default View Transform ID/version.
- `ColorEngine::Aces`: an explicit official ACES mode whose project payload
  stores a versioned Studio or CG Config preset. Presets resolve to exact OCIO
  built-in registry identifiers; they never follow an upstream `latest` alias.
- `ColorEngine::CustomOcio`: explicit studio/user OCIO provider over `$OCIO`, a
  selected built-in, or a path source. Its display/view owns final rendering only when
  `OutputTransformIntent::OcioDisplayView` is selected.

Final-output color science is selected separately through
`OutputTransformIntent`. `MondrianStandard` carries the same complete immutable
package identity from its first release, `OcioDisplayView` carries the named
display/view selected by advanced policy, and `Colorimetric` requests a direct
working-to-encoded conversion. The intent survives preview/export planning and
cache identity independently from the CPU/GPU implementation used to execute
it. Explicit export delivery-view policy overrides the Standard default rather
than being hidden in optional display/view strings.

Explicit OCIO mode must load its selected config successfully. It must not
silently fall back to a different color science. Mondrian Standard follows the
same rule whenever an input or endpoint node requires the embedded
`mondrian_default_ocio_v1` asset. Failure of that provider does not authorize
substitution of another config, approximate LUT, or non-conformant native conversion.
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
- `delivery_bit_depth`: actual 8-bit or 10-bit encoded sample depth
- HDR metadata preservation fields

`DisplayToneMapPolicy` controls the final working-to-display/export boundary.
Its `Automatic` mode follows the effective scene-referred workflow; the
per-sequence `auto_tone_map_media` authoring preference does not implicitly
turn every display-referred Rec.709 output into a View transform. That media
preference remains attached to individual media render plans, while final
output tone mapping is selected explicitly by workflow or policy. This keeps a
normal SDR sequence on the direct colorimetric OCIO processor and prevents a
configured-but-inactive display/view from changing preview or export pixels.

Media probing keeps automatic interpretation evidence separate from user
overrides. CICP/container tags, camera/log metadata hints, HDR side data, and
embedded ICC profiles are recorded as diagnostic evidence with confidence and
warnings; ICC-only streams may resolve to an inferred input color family, while
ICC-vs-CICP conflicts must be surfaced as warnings rather than silently changing
an explicit user override.

Camera acquisition identities are never represented by a transfer curve alone.
Each product `ColorSpace` binds an exact transfer and gamut pair, including
Apple Log/BT.2020, both Sony S-Log3 gamut variants, ARRI LogC3/AWG3 and
LogC4/AWG4, Canon Log2/Log3 Cinema Gamut D55, Panasonic V-Log/V-Gamut, RED
Log3G10/REDWideGamutRGB, Blackmagic Film/Wide Gamut Gen 5, DJI D-Log/D-Gamut,
and DaVinci Intermediate/Wide Gamut. A bare `S-Log3`, `LogC`, or similar curve
name is incomplete evidence and remains Unknown until metadata or a user
override supplies the gamut. ICC display-profile names are not camera metadata
and must not be used to guess these acquisition identities. The Interpret
Footage UI exposes the precise pairs; program/display output selectors expose
delivery spaces only.

Standardized SD video has two explicit encoded product identities:
`Rec601Pal` uses BT.470BG primaries, the BT.470BG gamma 2.8 transfer, and the
625-line BT.601 matrix; `Rec601Ntsc` uses SMPTE-C/SMPTE 170M primaries,
transfer, and the 525-line BT.601 matrix. Exact FFmpeg CICP triplets resolve to
these identities instead of being approximated as Rec.709. The embedded OCIO
config owns their source-to-working primary and transfer conversion, and its
full CPU/GPU processor-matrix validation includes both identities.

BT.2020 SDR is likewise an encoded boundary identity, not the Standard working
space. `ColorSpace::Rec2020` resolves to `Camera Rec.2020` and decodes the
BT.2020/BT.709-family SDR camera transfer before compositing;
`WorkingColorSpace::LinearRec2020` alone resolves to `Linear Rec.2020`. A
numeric regression pins code value 0.5 near 0.26 linear while preserving alpha,
so equal primaries cannot collapse encoded and linear processor endpoints.

## Working Space

`WorkingColorSpace` is the linear-light identity used by rendering, effects, and
compositing. It is deliberately separate from `ColorSpace`, which identifies
encoded acquisition and delivery spaces. `OcioColorSpaceIdentity` carries that
role distinction into CPU/GPU processor requests and cache keys, so encoded
`Camera Rec.709` cannot alias linear `Linear Rec.709 (sRGB)`. Camera log spaces
are rejected when converted to a working identity.
`SequenceSettings.working_color_space` persists `WorkingColorSpace` directly.
Project files and sequence-setting actions do not accept encoded acquisition or
delivery identities in this field.

Mondrian Standard v1 pins `WorkingColorSpace::LinearRec2020`; the bundled OCIO
config pins its `scene_linear` role to `Linear Rec.2020`, and the package
contract validates the same mapping. The working values are unbounded
scene-linear floats, not a 0..1 display signal and not a request to clip colors
to the BT.2020 triangle. Negative components and values above one are preserved.
An OCIO round-trip regression test crosses Linear Rec.2020 and ACEScg using
negative and extended-range samples and enforces a scale-aware `2e-5` tolerance.
The Linear Rec.2020 to SDR endpoint processor is also required to remain an
analytic GPU program with no LUT texture or dynamic uniform resources.

`InputColorResolution` returns a `ResolvedInputColor` value: `Color` requires an
encoded source-to-working processor, `Data` requires an explicit non-color
bypass, and `Rejected` fails the media path. Missing metadata can assume
Rec.709 with a diagnosed policy branch or reject the source; it can never
silently reinterpret encoded samples as the sequence's linear working space.

There is no public monolithic `ColorPipeline`. A source/input transform consumes
`OcioColorSpaceIdentity::Encoded` and produces
`OcioColorSpaceIdentity::Working`; effects and compositing accept only the
working identity; display and export boundary transforms consume working pixels
and produce an encoded presentation/delivery identity. This split prevents an
encoded `ColorSpace` from being passed as a working space and prevents tone
mapping from being attached to an interior color-space conversion.

OCIO execution also follows source -> working -> output. The Standard mode UI
can hide OCIO details from normal users, but the backend still routes through
the Mondrian default OCIO source and fails closed when that source is missing.
`mondrian-core` no longer exposes the legacy `RgbaF32Frame` /
`DisplayColorProfile` conversion path that decoded transfer functions, guessed
camera primary families, or applied a hand-written ACES-like tone curve. Program
color conversion must use typed OCIO processors. Monitor ICC adaptation remains
an explicitly separate presentation boundary in `display_calibration` and must
not become an alternate program rendering transform.

GPU-native decoded video follows the same source -> working contract. Native
NV12/P010 sampling expands range and converts YCbCr into the resolved encoded
source RGB signal; it does not choose independent color science. Decoder matrix
coefficients describe that sampling operation independently from RGB primaries
and are therefore carried without being overwritten by the product color-space
identity. The renderer
import plan then carries the complete `RenderInputTransform` (engine, tone-map
policy, working space, and GPU backend) into OCIO execution. Sampling
transfer facts that conflict with the resolved source color space, an explicitly
unsupported decoder matrix, or a CPU transform backend on the native path fail
closed before frame allocation.

CPU-decoded video follows the identical boundary ordering. Media first expands
YUV range and applies the resolved matrix into source-encoded RGBA; it does not
apply transfer, primaries, working-space, or display transforms. The resulting
typed `DecodedRgbaFrameContract` records the source interpretation and the
matrix/range actually applied. Renderer input processing then performs the
single source -> working OCIO transform. Preview, thumbnail, and export callers
must provide the same input color/range contract, and media must fail closed
rather than accept an implicit swscale Rec.601/limited-range assumption.

The typed frame graph distinguishes `EncodedFloat` from `LinearFloat`.
`EncodedFloat` is used for precision-preserving nonlinear samples at source,
display, and export boundaries. Native NV12/P010 sampling produces it before
the resolved OCIO input processor; float PQ/HLG/display/export processors
produce it after the final output boundary. `LinearFloat` is reserved for
linear-light working-space pixels and may participate in compositing.
`CpuEncodedFloatColorFrame` and `CpuColorFrame` are separate types so an encoded
output cannot accidentally re-enter linear effects or blending. Their payloads
are also distinct: `EncodedRgbaF32Frame` carries nonlinear boundary samples,
while `WorkingRgbaF32Frame` is reserved for linear-light pixels and carries a
`WorkingColorSpace` identity.

`ColorFrameDescriptor` stores `ColorFrameSpace::{Encoded, Working}`. Frame
domain validation and OCIO processor planning therefore share the same role
distinction instead of inferring it from transfer characteristics.

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
All existing built-in unary render operations execute after the OCIO input
transform in the sequence working space and before any display or export output
transform. Their float implementation preserves extended RGB values; spatial
sampling uses premultiplied alpha internally, and LUT sampling does not turn its
normalized lookup domain into an implicit clamp of the working frame. Custom
processors without a declared float ABI must remain diagnosed legacy boundaries
rather than silently changing color domain. Built-in blend, mask, mask-source,
and multi-input graph nodes stay in the same float working domain; mask coverage
is generated as float alpha and never passes through an implicit 8-bit matte.
These CPU contracts do not remove GPU residency blockers until equivalent GPU
graph execution exists.

The app UI presentation surface is also part of the color contract. Mondrian
targets wgpu 30 or newer for presentation because surface color space selection
and `Surface::display_hdr_info(...)` are required display-management evidence.
The wgpu window session must select an explicit sRGB SDR surface format and
`SurfaceColorSpace::Srgb`, failing closed when the backend exposes only
non-sRGB presentation formats or the chosen format cannot be configured with
sRGB color space. It must not fall back to a non-sRGB swapchain, because that
would hide OS/backend display management errors behind a visually plausible but
untrusted viewer path.
Preview display color space and OCIO display/view are resolved from the
sequence/project `DisplayManagementPolicy` before building
`RenderOutputColorBoundary`; the app preview boundary must carry the resolved
display/view when present, and the app window then validates that boundary
against its real display-output contract
(surface format, selected `SurfaceColorSpace`, SDR/HDR mode, per-format color
space capabilities, `display_hdr_info` snapshot, present modes, alpha modes, and
current monitor fingerprint). Display preview output is accepted only when the
requested output color space has a direct presentation contract on the selected
surface color space. Rec.709/sRGB requires `SurfaceColorSpace::Srgb`, Display P3
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

Monitor calibration is a distinct boundary after the OCIO display/view output.
`DisplayCalibrationLut3d` represents standard encoded display RGB to ICC device
RGB; it is not a working-space effect and cannot be inserted before compositing.
The LUT stores RGBA32F samples and a full-payload ICC fingerprint. Hot frame
contracts carry its non-authoritative compact calibration key while LUT caches
and pass preparation validate the complete fingerprint. The compact key must
never authorize cache reuse or processor execution. A matching GPU pass
must produce `ColorFrameSpace::Device` plus `DeviceFloat`, preventing
device values from being mistaken for a reusable standard color space. The app
records this pass only between the OCIO display/view output and the UI
presentation carrier. ICC paths keep the OCIO encoded output and device output
in `Rgba16Float`; LUT storage and interpolation math remain 32F. Uncalibrated,
stale, or fingerprint-mismatched profiles remain fail-closed.

GPU-resident color frames use `GpuColorFrameHandle`, a renderer resource-table
handle with the same `ColorFrameDescriptor` contract. CPU/GPU transfers are
scheduled explicitly by `RenderColorStagePlan` nodes rather than hidden inside
color conversion helpers.
When a GPU OCIO stage has no upload/readback requirements and no native
blockers, `RenderGpuColorPassSchedule` binds the source `GpuColorFrameHandle`,
target `GpuColorFrameHandle`, `RenderColorTransformGpuPlan`, and
`OcioGpuWgpuRenderPassNodePlan` into a schedulable unit. It fails closed if the
frame descriptors, residency, extents, or OCIO resource keys differ.
GPU transforms express encoded/linear boundaries in the OCIO processor endpoints.
Source/import requests use `Encoded(source) -> Working(destination)` and output
requests use `Working(source) -> Encoded(destination)` or an explicit display/view.
The fullscreen wrapper only samples, invokes the generated OCIO function, and
writes the result; it contains no independent transfer-function math. Processor
request identity participates in shader cache keys, preventing pipelines with
different color-domain semantics from sharing a shader.
Output texture format and descriptor encoding are one contract: `Rgba8Unorm`
requires `EncodedRgba8`, while `Rgba16Float`/`Rgba32Float` output targets require
`EncodedFloat`. Resource planning rejects contradictory pairs. CPU float output
passes linear working samples directly to a processor whose source endpoint is
the corresponding OCIO linear working identity, matching GPU execution.
Display/view output boundaries carry an explicit `RenderOcioDisplayView` when
the caller wants OCIO presentation semantics instead of a color-space delivery
transform. CPU display/view boundaries execute through the OCIO display
processor. GPU display/view shader extraction is modeled with
`OcioGpuShaderRequest::DisplayView`; before Naga translation, Mondrian lowers
OCIO legacy combined LUT samplers such as `uniform sampler1D` into explicit
wgpu texture/sampler declarations using the OCIO descriptor binding contract.
Logical 1D LUT sampling is represented as a 2D texture sample with a fixed
second coordinate, matching the existing LUT upload contract.
The backend preserves the interpolation policy reported for every OCIO LUT:
`Nearest` creates non-filtering texture/sampler bindings, while every other
OCIO host interpolation mode creates filtering bindings and linear hardware
sampling, matching OCIO's reference OpenGL host contract. Because LUT payloads
remain 32-bit float, renderer-created devices request `FLOAT32_FILTERABLE` when
the adapter advertises it. A filtering plan on a device without that feature
fails as a typed backend-object error before bind-group creation rather than
silently changing the LUT to nearest sampling. Real-wgpu coverage builds a
filtering Apple Log -> Rec.709 OCIO pipeline on the selected production device.

Preview and export final transforms are renderer execution concerns. App and
export crates build a `RenderOutputColorBoundary` from their `ColorContext` and
pass typed working frames to renderer stage execution helpers; they must not
construct display/export `RenderColorTransform` values or duplicate
working -> output conversion logic locally. CPU reference execution uses
`execute_cpu_output_boundary_rgba8(...)`, which returns encoded pixels plus
color/stage diagnostics in the same boundary result. App/export crates must not
instantiate `RenderOutputColorBoundaryExecutor::cpu_only()` directly. Native
GPU execution uses `RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend(...)`.
Real-wgpu conformance tests read back `Rgba16Float` PQ display/view output and
compare it to the CPU OCIO path with distribution-aware Delta E ITP and separate
alpha budgets. This guards processor, wrapper, texture-format, readback, and
half-float precision as one output contract.
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
local processor chains or direct low-level transform executor calls.

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

Export encoding adds an explicit signal-representation contract after the
renderer output boundary. The pipe always carries full-range encoded RGB;
`ExportVideoSignalContract` binds the codec pixel format, requested YUV range,
RGB-to-YUV matrix conversion, and emitted CICP matrix tag as one operation.
BT.2020-family outputs use the BT.2020 non-constant-luminance matrix, while
BT.709/P3-family and sRGB YUV deliveries use BT.709. An sRGB YUV delivery keeps
the sRGB transfer tag but uses a BT.709 matrix tag matching its actual YUV
samples; the RGB identity-matrix tag from `ColorSpace::encoding()` must not be
copied onto YUV media. Camera-log outputs still omit unverified delivery tags,
but their RGB-to-YUV matrix and range are explicit. GIF has no reliable color
tag contract and is therefore accepted only with an explicit sRGB output.

Static HDR10 metadata is currently supported only by the H.265/libx265
backend. SMPTE ST 2086 mastering-display and MaxCLL/MaxFALL values are emitted
through one atomic `x265-params` value so neither field can override the other.
AV1 and ProRes requests with metadata preservation fail validation until they
have independently implemented and verified bitstream/container backends.

Successful encoder exit is not proof of a correct deliverable. Timeline export
derives `ExpectedVideoSignalConstraints` from the same
`ExportVideoSignalContract` used to build FFmpeg arguments, then probes the
finished file. Validation fails on mismatched pixel format, range, primaries,
transfer, or matrix. Camera-log and GIF contracts additionally require color
tags to be absent, so an encoder cannot silently replace an intentionally
untagged signal with guessed metadata. ProRes 422 profiles are verified as
10-bit 4:2:2, while ProRes 4444/4444 XQ are verified as 12-bit 4:4:4:4.

Delivery sample depth and renderer transport precision are separate contracts.
`DeliveryBitDepth` exposes only the 8-bit, 10-bit, and 12-bit formats implemented
by current codecs. ProRes 422 profiles require 10-bit; ProRes 4444 profiles
require 12-bit. A 10/12-bit delivery uses the internal `Rgba16Float` export frame
contract and `rgba64le` FFmpeg pipe so the renderer output transform is not
quantized to 8-bit before encoding. That internal transport does not represent
a 16-bit-float deliverable; no such user-facing option exists until a real
float image/video backend is implemented.

Derived proxy media follows the same encoding contract. The app resolves one
`ProxyColorContract` from asset interpretation plus ingest metadata before
proxy lookup or generation. Media code persists that source color space,
source bit depth, encoded range, and selected encoder/pixel-format profile in a
versioned sidecar manifest. Proxy freshness requires an exact source
fingerprint and exact contract match. Working/rendering and display/output
spaces are deliberately absent from this source-referred artifact contract;
their transforms still occur in renderer input and final output boundaries.
HDR, camera-log, and high-bit-depth sources select a 10-bit proxy profile under
automatic policy, while an explicitly incompatible 8-bit H.264 request fails
instead of quantizing silently. Camera-log proxies carry no invented FFmpeg
delivery tags and rely on their explicit sidecar interpretation. Proxy
generation also requires an explicit full/limited source range: its FFmpeg
filter graph declares source frame metadata and matching scale input/output
range, so an `Unknown` ingest range cannot fall through to FFmpeg heuristics.
Asset thumbnails are presentation artifacts, not source frames. The app-owned
thumbnail worker resolves `AssetMediaInterpretation`, detected input color,
encoded source range, working space, engine, display/view, output space,
tone-map intent, and current OCIO config generation into one contract. Output
space and tone mapping come from the resolved `ColorContext`; the worker must
not replace them with thumbnail-local defaults. The product thumbnail host
currently requests an explicit sRGB presentation context, executes the same
renderer CPU input and output boundary APIs used by preview, and submits the
result as `RasterImageColorSpace::Srgb` to the UI atlas. Cache, failure,
pending-request, active-request, and raster-atlas identities include the full
color contract. A color-context change clears visible thumbnail state and
invalidates in-flight request ownership, so an older OCIO generation or
tone-map result cannot overwrite a newer completion.

Thumbnail rejection and execution failures are structured rather than reduced
to an absent image or a free-form log string. `AssetThumbnailFailureReason`
separates missing ingest facts, unsupported non-color data, rejected input
policy, unresolved source range, invalid output identity, unsupported raster
output, worker availability, decode cancellation/failure, unexpected GPU
frames, input/output transform failures, and invalid raster payloads. The cache
deduplicates repeated observations of the same failed request and exposes
reason counters through `AssetThumbnailDiagnostics`. A disconnected worker is
committed to the failure cache synchronously, so the UI cannot remain in a
false `Loading` state. Human-readable detail is evidence only; stable reason
codes are the reporting and aggregation contract.

Viewer media caches store source-decoded or input-transformed working frames,
never display-transformed frames. Their request identity includes the resolved
source space and range, target dimensions, working space, color engine,
tone-map intent, file fingerprint, and OCIO config generation. Both successful
frames and negative-cache entries are isolated by that identity. An OCIO hot
reload therefore cannot reuse working pixels or a transform failure produced
by an older processor generation; display/output cache invalidation alone is
not sufficient because the input processor may also have changed.

`RasterImage` and `DrawCommand::RasterImage` always carry an explicit
`RasterImageColorSpace`; bare RGBA8 has no UI presentation meaning. The current
atlas is `Rgba8UnormSrgb` and accepts only `Srgb`. Any other declared space is
replaced by the renderer's visible failure placeholder and increments both the
general raster failure count and `unsupported_raster_color_spaces`. It is not
reinterpreted as sRGB. Supporting Display P3 raster assets requires a matching
atlas/pipeline/presentation contract rather than relaxing this guard.
The CPU Viewer raster path resolves its own presentation payload contract at
this boundary: Rec.709 and sRGB SDR viewer requests are encoded through the
renderer output boundary into sRGB bytes before `RasterImage` construction.
The source/working frame is not relabeled. Display P3, PQ, and HLG requests do
not enter the SDR raster atlas and remain blocked for the native GPU/surface
presentation path.
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
`max-cll` strings. ICC payloads are parsed into a profile name plus an explicit
`IccColorSpaceMapping::{Mapped, Unmapped}` result. HDR10+ and Dolby Vision
configuration remain presence/diagnostic records until dedicated parsers are
introduced.
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
`VideoStreamInfo.color_range` separately stores the decoder's encoded
quantization range as `Limited`, `Full`, or explicit `Unknown` and propagates
into `VideoColorDiagnostic`. Range is a sampled signal fact, not a color-space
fallback. Proxy generation consumes the persisted stream fact, while native
YUV decode consumes the equivalent per-frame sampling fact; neither derives
range from primaries, transfer, matrix, codec, or pixel format.
`VideoStreamInfo.color_interpretation` is the structured explanation layer for
automatic detection. It carries the interpreted color space, confidence,
source/method, evidence, warnings, and whether the result is user-overridable.
Evidence distinguishes metadata hints, exact CICP triplets, partial CICP
matches, unsupported CICP tags, mapped/unmapped ICC profiles, and decoder
unavailability. An ICC `RGB`/`GRAY` signature identifies only the ICC channel
model; it is never evidence for Rec.709 or sRGB. Only an explicitly identified
supported standard produces a managed `ColorSpace`. An ICC-only unmapped stream
remains `MissingMetadata`, preserves the profile as evidence, and emits
`IccProfileUnmapped` so normal missing-input policy can reject or diagnose its
fallback. Warnings must
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
from the sequence output color space. The export delivery view is resolved from
the effective `ExportDeliveryViewPolicy` in `display_management` (inherited from
project or overridden by sequence). Callers must choose one of these explicit
entry points instead of using a generic root render context.

Display management is explicit in the resolved `ColorContext`. Project settings
own the default `DisplayManagementPolicy`; sequences inherit that policy unless
they disable color-management inheritance. The policy carries the monitor/profile
reference, viewer SDR/HDR mode, tone-map policy, and export delivery view policy.
The app sequence settings panel must expose that inheritance boundary before
sequence-level color controls. When inheritance is enabled, sequence-local color
controls are presentation-only draft state and must not be shown as active
render/export policy.
`DisplayToneMapPolicy` resolves the concrete `tone_map` flag for working -> output
boundaries, including HDR-working to SDR-output presentation, so preview, export,
cache keys, and future diagnostics do not infer tone mapping from scattered booleans.

For preview contexts, root sequence contexts copy the currently loaded OCIO
config's default display/view into the context when one is available. For export
contexts, the delivery view is resolved from `ExportDeliveryViewPolicy` — the OCIO
config defaults are NOT automatically used as export delivery views. Absence of a
delivery view in the export context is valid when no policy is configured; it
triggers a fail-closed diagnostic when tone mapping is requested.
Mondrian Standard preview contexts must explicitly load the embedded
`mondrian_default_ocio_v1` config before resolving the default display/view.
They must not call OCIO's process-global current-config query while Mondrian's
own OCIO state is empty, because that lets OCIO probe `$OCIO` independently and
print "Color management disabled" even though Standard mode is supposed to use
the bundled config. Explicit environment OCIO remains fail-closed when `$OCIO`
is not configured.
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

## GPU Output Blocker Taxonomy

Every reason the GPU color output boundary cannot execute is captured as a typed
enum variant rather than an opaque counter. The taxonomy covers three layers:

### Renderer-Level Blockers (`OcioGpuWgpuBlocker`)

These are detected during OCIO shader extraction, backend preparation, and
pipeline construction in `RenderGpuOutputBoundaryRuntime`:

- `OcioConfigNotLoaded` — OCIO config is not loaded or unavailable.
- `OcioProcessorUnavailable` — OCIO processor could not be created.
- `OcioGpuShaderExtractionFailed` — Shader extraction failed (transpilation,
  Naga, or backend error).
- `ShaderModuleNotPrepared` — Backend shader module not prepared.
- `OcioResourceBindGroupNotPrepared` — OCIO LUT/uniform resources not bound.
- `FullscreenWrapperNotPrepared` — Fullscreen wrapper shader missing.
- `RenderPipelineNotPrepared` — Final render pipeline not prepared.

These feed into `RenderColorStageGpuBlockerBreakdown` which carries per-reason
u64 counters (all 7 fields) and is part of `RenderColorStageDiagnostics` and
the renderer `RenderGpuOutputHealthReport`.

### App/Window-Level Blockers (`PreviewGpuOutputBlocker`)

These are detected during app-window GPU scheduling in `prepare_viewer_gpu_preview()`:

- `SurfaceContractMismatch` — Surface format does not support the output color
  space.
- `UnsupportedDisplayColorSpace` — Display color space not supported.
- `UnsupportedHdrSwapchainOrEdr` — HDR swapchain/EDR mode not supported.
- `FrameNotGpuResident` — Working frame is not GPU-resident.
- `LegacyRgba8CompositeBoundary` — Compositing fell back to legacy RGBA8.
- `CpuFallbackRequested` — CPU fallback was explicitly requested.

These are recorded per-frame in `PreviewGpuOutputBlockerBreakdown` (all 14
fields from both renderer and app layers) and surface in the preview health
report with structured root causes and action codes.

### Health Report Action Codes

Every blocker variant carries a machine-readable `action_code()` for health
report follow-up:

- `prepare_ocio_gpu_resources` — Load/reload OCIO config and verify processor.
- `inspect_gpu_blocker_breakdown` — Inspect renderer GPU blocker breakdown.
- `configure_display_contract` — Reconfigure display output contract.
- `ensure_gpu_frame_residency` — Ensure working frame is GPU-resident.
- `avoid_legacy_rgba8_boundary` — Migrate legacy RGBA8 composites.
- `investigate_cpu_fallback` — Investigate why CPU fallback was requested.

## CPU Correctness Path vs GPU Playback Path

Preview and export share one color pipeline: the CPU correctness path
(`CpuRenderColorStageExecutor`) produces bit-exact reference output, and the
GPU playback path (`RenderGpuOutputBoundaryRuntime`) produces the production
texture for presentation or encoding.

- **CPU correctness path**: Uses `execute_cpu_output_boundary()` or
  `execute_cpu_output_boundary_rgba8()` for reference output. It is the
  authoritative semantic path when OCIO config and processors are available;
  if they are missing, the request fails closed with diagnostics rather than
  silently substituting a fallback color space.
- **GPU playback path**: Uses `record_wgpu_output_boundary_owned_backend()` to
  produce a GPU-resident output texture. Requires all 7 renderer-level OCIO
  blockers to be resolved. The app-window path validates display contract
  compatibility before recording.

CPU fallback is always explicitly recorded — never silently used as "GPU ready".
Preview and export never independently interpret color spaces; they share the
same `ColorContext`, `RenderOutputColorBoundary`, and `RenderColorTransform`
resolution through the renderer layer.

## Structured Fallback Diagnostics

When GPU path cannot execute, diagnostics include:

- **Preview raster path** (`composite_resolved_preview`): Executes the explicit
  CPU presentation boundary and records structured legacy RGBA8 composite
  blockers when the working composite had to leave the float/linear path. This
  path is the fallback target, so it must not self-report every successful
  raster frame as a GPU-output fallback.
- **Window GPU path** (`prepare_viewer_gpu_preview`): Records
  `cpu_output_fallback_frames`, `cpu_output_fallback_pixels`, and typed
  `PreviewGpuOutputBlocker` evidence when native GPU output recording fails.
  Display contract blockers and GPU record failures are captured with structured
  evidence.
- **Export path**: Records `gpu_output_cpu_fallbacks` and
  `gpu_output_fallback_reasons` in `ExportJobColorDiagnosticsSummary`.

All three paths share the same `color_report_vocab` canonical root-cause and
action codes, enabling cross-report comparison between preview and export.

## Explicitly Unsupported Features

These features are defined in the type system but not implemented. Diagnostics
use `PreviewGpuOutputBlocker::UnsupportedFeature` with stable `feature` codes
and `document_unsupported_feature` action code.

- **`MonitorProfileReference::IccProfile` outside a resolved Display Output
  Contract** — Windows OS default ICC profile discovery is available through
  `mondrian-platform`, and preview scheduling consumes the resolved Display
  Output Contract when it maps the ICC profile to a managed monitor color
  space. If the contract is missing, invalid, unreadable, or unmapped, preview
  fails closed with `icc_preview_color_space_resolution` or the structured
  display-contract blocker. It must not silently fall back to Rec.709, sRGB, or
  the sequence output color space.
- **Real OS HDR/EDR detection** — `ViewerDisplayMode::HdrPq` /
  `ViewerDisplayMode::HdrHlg` are explicit user selections. On Windows the app
  probes DisplayConfig Advanced Color support/enabled/force-disabled state, bits
  per channel, color encoding, and SDR white level through `mondrian-platform`;
  known disabled or unsupported state blocks HDR preview. wgpu
  `SurfaceColorSpace` compatibility is still only the swapchain side of the
  contract. macOS/Linux and unavailable probes record
  `MonitorHdrCapabilityUnknown` / `MonitorHdrCapabilityUnsupported` blockers
  when HDR correctness cannot be confirmed.
- **GPU compositing** — The `gpu_compositor.rs` module is wired into the
  preview/viewer GPU path for the safe production subset: media-layer affine
  transforms, identity solid transforms, Normal blend mode, supported fused
  working-linear media/solid effect chains, working-linear adjustment layers,
  and at most five executed layers.
  It composites into an `Rgba32Float` working-space GPU texture, then feeds the
  same renderer-owned OCIO GPU output boundary used by the rest of preview.
  GPU effect lowering supports ColorAdjust, WhiteBalance, Vignette, and Grain
  without an encoded/RGBA8 intermediate. Adjustment plans sample the current
  accumulator, process it in the same working space, and blend the result back.
  Unsupported layer stacks fail back to the CPU reference compositor with
  structured `GpuCompositingDiagnostics` blocker reasons (`EffectRequiresCpu`,
  `UnsupportedBlendMode`, `UnsupportedTransform`, `TooManyLayers`,
  `GpuUnavailable`).
- **`FrameNotGpuResident` blocker** — The current preview GPU compositing path
  supports CPU-layer upload into GPU compositing (`GpuWithUpload`) and then keeps
  the composited working frame GPU-resident for OCIO output. This blocker is
  reserved for future paths that require already-resident media textures and
  intentionally disallow upload.

The `UnsupportedFeature` blocker variant exists precisely so that when these
limitations are resolved, the diagnostic path can be updated without changing
the taxonomy.

## HDR/SDR

HDR output spaces include Rec.2100 PQ/HLG. Tone mapping is required when scene/HDR working data targets SDR output. HDR metadata can only be preserved for HDR output spaces.
