# Color Management

Mondrian has one typed color-management pipeline. Accepted ADR-0005 defines
Mondrian Standard as a Mondrian-owned, immutable, versioned OCIO package and
uses stock OCIO as the default execution infrastructure. Mondrian Standard,
ACES, and Custom OCIO are product-level modes over that shared integration, not
three renderer engines. Missing processors or configs required by a selected
mode must surface as errors instead of falling back to different color science.

The Rust integration pins `ocio-rs` 0.2.1 revision `933c65dc` with the `bundled`
feature enabled, so normal application builds exercise the real OpenColorIO
bridge rather than a stub runtime. Its Windows bundled path consumes the exact
CMake install directly and does not depend on a machine-global `pkg-config`.

The bundled endpoint/input config is packaged as
`crates/mondrian-core/assets/ocio/mondrian_default_ocio_v2.ocio`. Standard mode
parses that immutable base and assembles its versioned product View in memory
with stock OCIO transforms; it never resolves an upstream `latest` alias or a
machine-local LUT. The base retains pinned OCIO Studio input definitions and
explicit ACES transforms for the separate ACES-facing capability surface, but
the Standard active/default View is not an ACES Output Transform.

The current package, Mondrian Standard v3, exposes `Mondrian Standard SDR v2`.
It creates a separate stock-OCIO scene View Transform for sRGB, Rec.709,
Display P3, and Rec.2020 SDR. Each graph converts the AP0 scene reference to the
target's linear primaries, enters HSV, normalizes H/S/V to a declared
`[0,1] x [0,1.25] x [0,4]` domain, and applies a deterministic 61-cube
tetrahedral surface that preserves hue and contains only high-value saturation
excursions. A 4096-entry OCIO 1D LUT then applies the pinned value shoulder;
its samples are generated from the documented OCIO `GradingRGBCurve` during
package assembly so the per-pixel GPU program does not execute the expensive
B-spline evaluator. After HSV inversion, an OCIO `RangeTransform` provides the
declared output safety boundary, followed by exactly one target signal encoding
and one display-reference bridge. Normal Rec.709 remains within one 8-bit code
value through the complete input-to-working-to-View path.

The immutable v2 package remains available for projects that explicitly pin it.
Its `Mondrian Standard SDR v1` is the earlier AP0-to-XYZ, FilmLight E-Gamut,
log2 allocation, pinned 57-cube AgX Base formation graph documented in
`assets/ocio/MONDRIAN_STANDARD_SDR_V1_NOTICE.md`. Runtime source identity and
output intent both carry the package identity, so reopening v2 cannot silently
select the v3 graph. New projects never select the legacy graph by alias.

`Mondrian Standard HDR 1000 nits v1` uses the same AP0-reference to XYZ D65,
FilmLight E-Gamut, and log2 allocation stages, followed by a pinned 57-cube AgX
1000-nit, P3-D65-limited HDR formation resource. That resource authors a
Rec.2100 HLG image with a 100-nit program reference white. OCIO then decodes it
to the shared display-reference XYZ connection space and applies exactly one
selected display encoding: Rec.2100 HLG or Rec.2100 PQ. Consequently HLG and PQ
do not have independent picture formation and Standard does not invoke an ACES
Output Transform. The exact upstream commit, blob and byte digests, domain,
resolution, authored luminance contract, and BSD-3-Clause notice are recorded
in `assets/ocio/MONDRIAN_STANDARD_HDR_V1_NOTICE.md`.

Output-target resolution maps Rec.2100 HLG and PQ to that HDR View while SDR
targets map to the SDR View. A display without its target-class Standard View
fails closed; it must never borrow the sRGB Standard View or an inactive ACES
View. Display probing and timeline output planning share this target-aware
resolver so diagnostics cannot report a different View than the render
boundary.

`MondrianStandardOutputTargetContract` is the single typed description of each
Standard program output. It binds the selected OCIO display/View to the encoded
primaries/transfer/matrix, authored rendering gamut limit, reference white,
nominal peak, and nominal black. SDR targets currently bind 100-nit reference
white and peak; HLG/PQ bind 100-nit reference white and the fixed 1000-nit,
P3-D65-limited HDR View. Display/view lookup delegates to this contract so
rendering, validation, and delivery metadata cannot maintain parallel string or
luminance tables.

New projects and sequences default to the SceneReferred workflow and therefore
execute the exact package-pinned Standard SDR or HDR View for Program Output.
This is the normal product path for finished SDR video as well as log, HDR,
scene-light, CG, and grading work; the SDR View is specifically constrained to
preserve normal Rec.709 rather than impose an unconditional filmic reshape.
An explicit DisplayReferred workflow remains available as a technical direct
colorimetric bypass. `Mondrian Standard` names the immutable engine/package
contract, while workflow controls whether its rendering View participates.
The Standard Program Output contract currently has six exact View targets:
sRGB, Rec.709, Display P3, Rec.2020 SDR, Rec.2100 HLG, and Rec.2100 PQ. Rec.601
PAL/NTSC remain valid input identities and explicit DisplayReferred
colorimetric outputs, but are not offered or accepted as SceneReferred Standard
targets until the immutable package defines corresponding Views.
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
Selecting an explicit OCIO display while Standard is active still resolves the
versioned Standard View registered under that display. It never adopts the
display's config-default ACES View; a display without a Standard View fails
closed.

## Effect Processing Domains

Effects declare processing semantics in `mondrian-effects`; color conversion
remains renderer-owned. A compiled effect graph starts and ends in the
sequence's scene-linear working RGB domain and records any internal
scene-linear, log/perceptual, display-linear, or display-encoded RGB boundary as
an explicit transition. Those transitions are processor requests against the
project's selected Standard, ACES, or Custom OCIO configuration; they must not
be approximated with native transfer functions, an ACES-specific side engine,
or a baked RGBA8 fallback.

The compiled domain plan is backend-neutral. CPU and GPU backends may fuse
adjacent matrix, 1D, and 3D OCIO operations when OCIO proves the same processor
semantics, but they must preserve node order and the exact endpoint identities.
Non-color data and alpha/mask payloads are typed, non-convertible domains.
The CPU timeline backend executes legal transitions directly through the
selected engine's OCIO CPU processors without copying between pixel containers.
Those processors are retained only by the explicit
`RenderCpuColorExecutionSession` owned by the Preview runtime, one Export job,
or a dedicated derived-media worker. Timeline compositor scratch carries that
same Session through effect-domain transitions, source adaptation, nested
working-space conversion, and final output adaptation. Preview and export
therefore use one renderer entry point and one owner-scoped processor lifetime;
there is no process or thread-local hot-path cache. Processor resolution
failures, GPU plans that have not yet scheduled equivalent OCIO passes, and
invalid non-color crossings remain fail-closed and expose the shared
`effect_domain_unresolved` root cause so correctness cannot diverge between
interactive and final rendering.

Scene-linear gamut repair is separate from the final display/output transform.
The `builtin.gamut_compression` Effect implements the normative ACES 1.3
Reference Gamut Compression in AP1 with the published limits
`[1.147, 1.264, 1.312]`, thresholds `[0.815, 0.803, 0.880]`, and power `1.2`.
Its CPU oracle tests use independent official CTL-produced OpenColorIO vectors.
Working-space conversion is explicit and invertible around that operator; no
output gamut, monitor profile, or OCIO View is inferred. The separate Highlight
Recovery Effect is a luminance-preserving scene-linear chroma reconstruction,
not a display tone map and not RAW highlight-detail recovery.

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

BT.2100 HLG validation has a separate fixed-display oracle for Mondrian
Standard's current 1000-nit, zero-black target. It applies the normative inverse
OETF and the BT.2020-luma OOTF at system gamma 1.2, producing the same explicit
display-linear cd/m2 type. Normative breakpoint, 203-nit graphics-white and peak
vectors protect the oracle. The renderer quality corpus sends every renderable
stimulus through the production HLG and PQ output boundaries, independently
decodes both signals to absolute BT.2100 RGB, and compares each channel with a
bounded absolute/relative luminance tolerance. This qualifies the HLG terminal
encoding without treating HLG/PQ agreement inside OCIO as an independent
reference. Other display peaks or black levels require a separately qualified
contract rather than reusing this fixed oracle.

## Transform providers and output intent

- `ColorEngine::MondrianStandard { package }`: productized policy selecting the
  bundled, version-pinned Mondrian OCIO package. The persisted package identity
  requires its product ID/version, config ID/SHA-256, full package SHA-256,
  working-space ID/version, and separate SDR plus 1000-nit HDR View Transform
  IDs/versions. Distinct single-variant identity types prevent a tampered
  project from swapping the SDR and HDR identities while still deserializing.
- `ColorEngine::Aces`: an explicit official ACES mode whose project payload
  stores a versioned Studio or CG Config preset. Presets resolve to exact OCIO
  built-in registry identifiers; they never follow an upstream `latest` alias.
- `ColorEngine::CustomOcio`: explicit studio/user OCIO provider over `$OCIO`, a
  selected built-in, or a path source. Its target-qualified output bindings own
  final rendering when `OutputTransformIntent::CustomOcio` is selected.

Final-output color science is selected separately through
`OutputTransformIntent`. `MondrianStandard` carries the same complete immutable
package identity from its first release, `Aces` carries a target-aware preset,
`CustomOcio` carries the requested standardized output target while the selected
engine identity owns its named display/view, and `Colorimetric` requests a direct working-to-encoded
conversion. The intent survives preview/export planning and cache identity
independently from the CPU/GPU implementation used to execute it. There is no
second project/sequence display-view override: the selected engine identity is
the sole source of the final picture-formation transform.

Explicit OCIO mode must load its selected config successfully. It must not
silently fall back to a different color science. Mondrian Standard follows the
same rule whenever an input or endpoint node requires the embedded
`mondrian_default_ocio_v2` asset. Failure of that provider does not authorize
substitution of another config, approximate LUT, or non-conformant native conversion.
The `$OCIO` environment source is intentionally fail-closed: if the variable is
unset or points to a missing file, Mondrian reports that selected source as
invalid instead of scanning machine-specific standard paths.
`ColorEngine::output_display_view(target)` is the engine-owned resolution
boundary. Standard and ACES resolve from their immutable package/preset; Custom
OCIO resolves only an exact binding saved for that target. Its unqualified
`default_display_view()` query is rejected. Product callers cannot read a
process-global default and then infer which engine or output label it belongs
to; the unqualified processor and display-query APIs are intentionally not public.

Each versioned ACES preset also pins its default display/view pair and a
registry regression verifies those names against the bundled stock OCIO
config. Timeline context creation therefore records the named ACES intent
without selecting process-global state. A missing preset or processor fails at
the explicit load/planning boundary; it must never rewrite that intent to
`Colorimetric`.

ACES Program Output is represented by a typed `Aces { preset }` intent rather
than an eagerly copied default display/view. At the output boundary, the pinned
preset resolves the requested encoded target to an exact View from that
immutable config: both current presets support sRGB, Rec.709, Display P3, and
1000-nit Rec.2100 PQ; the Studio preset additionally supports Rec.2100 HLG.
Neither official preset has an ACES Rec.2020 SDR rendering View. Unsupported
target/preset combinations and preset drift fail before render planning; the
implementation must never run the default Rec.709 View and relabel its pixels
as another output. A registry test validates every declared mapping against the
real OCIO built-in config.
The existing real-wgpu ACES PQ accuracy gate now enters through this typed
intent and the production `RenderOutputColorBoundary::from_intent` path before
comparing GPU output with the stock-OCIO CPU result.

Custom OCIO is persisted as a complete `CustomOcioProjectIdentity`, not a bare
locator. It requires the source, primary config SHA-256, a versioned dependency
manifest SHA-256 over every external resource reachable through each executable
colorspace-to/from-working route and selected output processor, the exact
working space, and a sorted non-empty set of `CustomOcioOutputIdentity`
bindings. Each binding contains one standardized
Mondrian `ColorSpace`, exact display/view, the resolved OCIO display color-space
endpoint (including resolution of `<USE_DISPLAY_NAME>`), and effective look
expression. The same target cannot appear twice and one display/view cannot
claim multiple output labels. The identity also pins the sorted role map and an
explicit dynamic-property override list. Current projects save no dynamic
overrides and reject non-empty override lists until typed execution exists.
Missing fields, semantic mismatches, config edits, role/view/endpoint changes,
and external LUT changes fail closed during deserialization or config validation.

> **Identity stability rule (completed for Standard and Custom at M2):** a persisted identity
> may cover only contractual bytes — config text, assembly manifest, and
> embedded resources. Engine-derived processor cache IDs are build-local
> execution facts and must never enter a persisted identity; the Mondrian
> Standard package digests and the Custom OCIO dependency manifest both follow
> this rule. OCIO config and processor cache IDs remain runtime-only cache and
> diagnostic evidence. Reopening unchanged contractual config/resource bytes
> on another OCIO engine build refreshes that evidence; altered config, LUT,
> role, working-space, display/view, endpoint, or look contracts still fail
> closed.

When the UI selects only a Custom `.ocio` file,
`ProjectColorEnvironment::custom_ocio` validates the complete set of Sequence
working/output pairs, then `ColorEngine::custom_ocio_for_outputs` scans the
config for a uniquely target-compatible display color-space endpoint for every
output required by the future-Sequence template and all current Sequences. It
prefers each display's declared default View, but never substitutes the config's
global default for another output target.
Recognized sRGB, Rec.709, Display P3, Rec.2020 SDR, PQ, and HLG endpoint names
must agree with the requested label. Unknown or ambiguous studio conventions
require an explicit `custom_ocio` display/view declaration; that declaration and
the actual endpoint are both pinned. The simple Project Settings flow creates
the complete deduplicated binding set atomically. The new-Project flow naturally
starts with its one initial Program Output. A future advanced mapping UI may
author explicit mappings for ambiguous facility names without changing project
semantics; until then, one unresolved required target rejects the complete
Custom candidate rather than relabeling or retaining partial bindings.
The current Custom identity also pins exactly one Mondrian working-space name;
its processor-graph digest is defined around that space. Sequence/project
validation therefore rejects a Custom engine paired with any other sequence
working space before render planning or persistence. CPU/GPU processor creation
retains the same check as a defense-in-depth boundary. Mondrian Standard pins
the package-defined Linear Rec.2020 working space, while ACES keeps its
supported working-space selection explicit.

## OCIO Global State Management

All OCIO config mutations are centralized in `mondrian_core::ocio` through
`OcioGlobalState`, a mutex-protected struct that owns:

- The loaded config path (or virtual path for built-in/embedded configs)
- The source identity (`OcioConfigSource`) that loaded the current config
- The complete Custom identity last validated against that loaded config
- A monotonic generation counter for cache invalidation

`OCIO_CONFIG_OPERATION` serializes the exact sequence of selecting a config and
constructing a CPU Processor or extracting a GPU shader. The lease ends before
CPU pixel application and before any GPU execution, so frames do not serialize
on a process-wide color lock. This is required because the current `ocio-rs`
bridge exposes OCIO's process-global current config during construction even
though the baked Processor itself is independent afterward.

Global config installation, cache invalidation, config serialization, Standard
transform construction, and GPU descriptor extraction use `ocio-rs`'s fallible
APIs. A bridge failure leaves Mondrian's source identity and generation
unchanged and is reported to the caller; no path may silently retain a stale
config or panic at the FFI boundary.

CPU Processors live in a bounded, explicit `OcioCpuProcessorSession` keyed by
the complete `ColorEngine`, config revision, encoded/working endpoint
identities, and display/view when applicable. Immutable embedded and built-in
packages use their pinned engine identity as the stable revision, so switching
Standard -> ACES -> Standard inside one owner does not discard a warm Standard
Processor. A validated Custom engine already contains its config and
dependency-manifest digests, so a changed mutable source creates a different
`ColorEngine` identity rather than mutating a cache entry. OCIO's opaque config
and processor cache IDs remain inside runtime evidence and baked processor
bundles. A warm hit
is looked up before process-global config selection and performs no config
switch, file I/O, or Processor construction.

The Session is deliberately non-`Send`: Preview, each Export job, and each
derived-media worker construct and retire it on their owning execution thread.
This prevents accidental cross-worker residency and avoids a cache-wide
hot-path lock. Resource policy can synchronously reconfigure the capacity or
clear the Session; active frame execution keeps ordinary Rust borrow
exclusivity. Hit, miss, eviction, occupancy, and capacity are observable through
the Session's diagnostics. Public convenience transforms use the uncached
reference path and are suitable for tests or one-shot calls, not repeated
production execution.

GPU requests carry `ColorEngine` as part of their immutable cache identity.
`OcioGpuShaderCache` therefore keeps warm plans for Standard and ACES
simultaneously and cannot return one engine's shader for another engine merely
because their endpoint names match. Shader extraction still occurs under the
short config-operation lease, after which the renderer owns plain shader/LUT/
uniform metadata and performs compilation and execution without the lease.
Explicit cache clearing is reserved for renderer/device lifecycle invalidation,
not ordinary engine switching.

Project open or an explicit Custom `ensure_loaded` is different from the warm
processor path: under the config-operation lease it clears stock OCIO's global
file/processor caches, reloads path/environment configs, rebuilds the pinned
processor-graph fingerprint, and increments the process-global operational
generation. This is necessary for same-path LUT edits to become observable
without a process restart. Once the complete new `ColorEngine` identity is
validated, shader/processor cache lookups use that stored identity and never
use the operational generation as equality or repeat that work per frame.

`ocio_config_generation()` remains operational diagnostics for process-global
OCIO reloads. It is not semantic cache identity. Final-frame, media, thumbnail,
diagnostic, CPU-processor and GPU-shader caches carry the exact `ColorEngine`
and their resolved `ProgramColorContext` or `MediaInputColorContext`; switching
the process-global current config cannot
make an object from one engine satisfy another engine's key.

`ocio_config_source()` returns the source identity of the currently loaded
config, enabling diagnostics, exact `ColorEngine::is_available()` checks, and
source-aware idempotency.

## Project and Sequence

`ProjectColorEnvironment` stores the Project's one exact engine.
`SequenceColorSettings` stores no engine and has no inheritance switch. It has
three closed parts: working-space identity, media-input policy, and Program
Output policy. `SequenceDeliveryDefaults` separately owns encoded defaults and
authored HDR metadata. Every resolved context is the product of the Project
engine and only the Sequence semantics needed at that stage.
Sequence editing-mode presets may update editing format defaults such as
resolution, frame rate, and display format, but they preserve working color
space, `SequenceColorSettings`, `SequenceDeliveryDefaults`, and
tone-map policy. Color-management state is explicit user/project intent and is
validated fail-closed after the preset is applied.

Important fields:

- `color.working_color_space`
- `color.input.missing_metadata_policy`
- `color.input.auto_tone_map_media`
- `color.program_output.workflow`: SceneReferred (default Standard picture formation) or explicit DisplayReferred colorimetric bypass
- `color.program_output.tone_map_policy`
- `color.program_output.color_space`
- `delivery.video_range`
- `delivery.bit_depth`: default encoded 8/10/12-bit delivery sample depth
- `delivery` static HDR metadata authoring policy and payloads

Viewer display/profile policy is machine-local runtime state. It is not saved
in the Project or Sequence and is applied only after Program Output. Nested
processing belongs to each `NestedSequence` Clip placement edge. The supported
policies either convert the completed child working image exactly once at the
edge or evaluate the child directly in parent working space. A nominal
child-output bake is intentionally absent until its full execution and
reference contract exists. No edge can select another engine, and forcing the
parent working space never replaces the child's media-input interpretation
policy.

`DisplayToneMapPolicy` controls the final working-to-Program-Output boundary.
Its `Automatic` mode follows the effective workflow; the per-sequence
`color.input.auto_tone_map_media` preference is copied onto each media render
plan and becomes `MediaInputColorContext.input_tone_map`. Current-frame,
prefetch, and preroll resolution consume that same value. New sequences are
scene-referred and execute the selected
engine's product View. DisplayReferred is an explicit direct-colorimetric bypass.
`ColorEngine` is the sole Standard/ACES/Custom mode selector; workflow
deliberately has no ACES-branded variant.

Media probing keeps raw facts, diagnostic candidates, executable identity, and
user overrides separate. CICP/container tags, camera/log metadata hints,
complete transfer-and-gamut pairs in file names, HDR side data, and embedded ICC
profiles are retained with confidence and warnings. `candidate_color_space` is
never pixel authority. `VideoStreamInfo::executable_color_space()` delegates to
the only metadata-derived Auto execution seam, which simultaneously consumes
the proven sampling contract, raw `VideoColorMetadata`, every raw metadata hint,
and the derived `DetectedColorInterpretation`. Preview, Export, proxy,
thumbnail, and Interpret Footage diagnostics must use that complete seam; a
matrix-only or derived-evidence-only helper is not an executable alternative.
The public interpretation Interface therefore requires the caller to pass
`Option<VideoSamplingContract>` explicitly. Absence remains an explicit
unproven fact rather than being reconstructed from codec names or the selected
color space.

That seam reconstructs CICP identity from a closed set of numeric standardized
codes. The persisted name and `specified` bit must exactly match the canonical
form for that code; a name can never relabel a numeric code after deserialization.
Exact derived CICP evidence must equal the retained raw triplet and resolved
identity. For acquisition declarations, every executable evidence record must
exist exactly in the raw hint set and must still pass the closed declaration-key
parser. Stream scope outranks container scope; agreeing declarations at the
winning scope merge, while different executable identities at the same winning
scope are ambiguous and fail closed independent of dictionary enumeration
order. Lower-scope and conflicting records remain diagnostics and warnings, not
pixel authority. File names and free-form descriptive fields remain suggestions
regardless of confidence. Partial CICP and the current profile-name-only ICC
mapping likewise remain diagnostic: they may help Interpret Footage, but cannot
affect pixels without an explicit author override.
Before selection and persistence, raw hint records are canonicalized by scope,
declaration key, value, and provenance. Diagnostic suggestions that agree with
one exact winning declaration are retained but do not manufacture a
`MultipleMetadataHints` ambiguity. Container dictionary iteration order
therefore cannot change executable identity, warnings, or serialized
diagnostics.

CICP primaries/transfer identify RGB colorimetry; the matrix is an independent
YCbCr sampling fact. Auto execution requires explicit, uniquely mapped
primaries and transfer plus an explicit supported matrix for YUV sampling. A
probe-proven RGB sampling format has no YCbCr matrix obligation. CPU/native YUV
materialization consumes decoder matrix evidence and fails closed when it is
missing or unsupported; it never derives a matrix from the selected
`ColorSpace`. Partial tags may produce a candidate only. Unsupported or
contradictory fields remain preserved for diagnosis, and missing facts reach
the explicit project policy or user Override rather than an implicit guess.
`ocio_identity_processor_cache_id()` resolves the processor identity for the
effective source and working endpoints under the exact pinned engine/config.
It creates no GPU shader or renderer resource and fails closed on missing
configs or identities. Interpret Footage uses this query only after the modal
opens, alongside the retained raw range/CICP/evidence payload, so diagnostics
name the processor that production GPU extraction will use without adding work
to the asset-panel refresh path.

Camera acquisition identities are never represented by a transfer curve alone.
Each product `ColorSpace` binds an exact transfer and gamut pair, including
Apple Log/BT.2020, Sony S-Log2/S-Gamut, both Sony S-Log3 gamut variants, ARRI LogC3/AWG3 and
LogC4/AWG4, Canon Log2/Log3 Cinema Gamut D55, Panasonic V-Log/V-Gamut, RED
Log3G10/REDWideGamutRGB, Blackmagic Film/Wide Gamut Gen 5, DJI D-Log/D-Gamut,
and DaVinci Intermediate/Wide Gamut. A bare `S-Log3`, `LogC`, or similar curve
name is incomplete evidence and remains Unknown until metadata or a user
override supplies the gamut. The same rule applies to file names: `Slog3` or
`Log3G10` alone is not enough to infer primaries. ICC display-profile names are
not camera metadata and must not be used to guess these acquisition identities.
The Interpret Footage UI exposes the precise pairs; program/display output
selectors expose delivery spaces only. Package validation treats
`ColorSpace::ALL` as the product catalog: every entry must occur exactly once in
the immutable Standard mapping table before the full CPU/GPU processor matrix is
accepted. This prevents a UI-visible input identity from outrunning the bundled
OCIO package.

External scene-referred image identities are first-class `ColorSpace` values:
linear Rec.709, linear Rec.2020, linear P3-D65, ACES2065-1, ACEScg, and ACEScct.
They are available to automatic metadata interpretation and Interpret Footage,
but never to sequence output selectors. Generic EXR extension alone is not
evidence for ACES2065-1: the bundled config's fallback file rule is `Raw`, and
Mondrian requires explicit image metadata or a user override. ACES2065-1,
ACEScg, and the three linear RGB sources are valid identities for float source
frames and do not require an encoded-transfer decode. Decoder integrations must
preserve float samples into that entry point; support in the identity model does
not by itself imply that every container decoder already avoids RGBA8.

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
external source and delivery spaces, including scene-linear image sources.
`OcioColorSpaceIdentity::Color` and `OcioColorSpaceIdentity::Working` carry that
role distinction into CPU/GPU processor requests and cache keys. Thus an
external linear ACEScg source and an internal ACEScg working frame remain
different pipeline roles even though their numerical color space is identical.
Only scene-linear `ColorSpace` values convert directly into a working identity;
display-encoded and Log values require an OCIO processor.
`SequenceSettings.color.working_color_space` persists `WorkingColorSpace` directly.
Project files and sequence-setting actions do not accept encoded acquisition or
delivery identities in this field.

Mondrian Standard packages v2 and v3 pin `WorkingColorSpace::LinearRec2020`; the bundled OCIO
config pins its `scene_linear` role to `Linear Rec.2020`, and the package
contract validates the same mapping. The working values are unbounded
scene-linear floats, not a 0..1 display signal and not a request to clip colors
to the BT.2020 triangle. Negative components and values above one are preserved.
The package identity is also an authoring constraint: every Standard Project
Sequence and its future-Sequence template must use Linear Rec.2020. Project
validation, Sequence actions, and Project-environment replacement reject any
mismatch before mutation. Official ACES configs remain the only built-in mode
that permits an explicit choice among Mondrian's supported working identities.
An OCIO round-trip regression test crosses Linear Rec.2020 and ACEScg using
negative and extended-range samples and enforces a scale-aware `2e-5` tolerance.
The Linear Rec.2020 to SDR endpoint processor is also required to remain an
analytic GPU program with no LUT texture or dynamic uniform resources.

`InputColorResolution` returns a `ResolvedInputColor` value: `Color` requires a
source-to-working processor, `Data` requires an explicit non-color
bypass, and `Rejected` fails the media path. Missing metadata can assume
Rec.709 with a diagnosed policy branch or reject the source; it can never
silently reinterpret encoded samples as the sequence's linear working space.
CPU float decode payloads retain an additional encoding fact at this seam:
`SourceEncodedRgb` must enter the encoded-float input processor, while only
`SourceLinearRgb` with a scene-linear source identity may bypass the transfer
decode as a linear-float source. Preview and Export must preserve that fact;
the payload's scalar type or bit depth alone never proves scene linearity.
Renderer's `Source Frame Preparation` Module is the sole production owner of
that classification for CPU RGBA payloads. It also validates the decoded
extent, normalizes authored Alpha Interpretation before OCIO, and binds the
exact `RenderInputTransform`. Preview retains the prepared source for deferred
GPU or lazy CPU execution; Export executes that same Interface immediately.
Neither Adapter reconstructs a `CpuSourceColorFrame` from decoder fields.

`DataTexture` is a separate closed sample identity, not a missing-color or
rejected-color state. Timeline resolves it from the authored non-color payload;
Preview and Export lower it to the same `SourceFramePreparationIntent`.
Media then marks the decoded RGBA payload as `DecodedRgbaEncoding::DataTexture`,
and Renderer normalizes the numeric RGB(A) channels directly into the target
working-frame storage without constructing or executing an OCIO processor.
The result carries no OCIO diagnostics and records zero renderer color stages.
Only probe-proven RGB/GBR sources are admitted: applying a YCbCr matrix would
change utility-channel values, so YCbCr, native-surface, compact-YUV, and proxy
representations fail closed. The DataTexture cache identity depends on the
target working domain but not on Color Engine or input tone-map identity.

There is no public monolithic `ColorPipeline`. A source/input transform consumes
`OcioColorSpaceIdentity::Color` and produces
`OcioColorSpaceIdentity::Working`; effects and compositing accept only the
working identity; display and export boundary transforms consume working pixels
and produce a presentation/delivery `Color` identity. This split prevents an
external `ColorSpace` from being passed as a working space and prevents tone
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
The app preserves range authority before entering media through
`DecodedVideoRangeContract`: an explicit `MediaRangeInterpretation` override is
authoritative over repeated container or frame tags, while Auto prefers an
explicit frame-level fact and uses the current probe result only as fallback.
CPU swscale and native GPU NV12/P010 sampling resolve the same contract.
Preview-frame, thumbnail, and export decode/cache identities include both the
authority and baseline range, so changing a range override cannot reuse pixels
created under the old sampling contract. Proxy generation retains its own
versioned source-range contract and fingerprint.

Alpha is coverage, not color, and never passes through an OCIO RGB processor.
Decoded source frames are normalized to the renderer's straight-alpha working
contract before the source-to-working transform: straight input is shared
without copying, premultiplied input is unassociated at the typed source
boundary, and `Ignore` forces opaque coverage. Unassociation preserves extended
and negative float RGB values and treats zero-alpha RGB as transparent black.
Float coverage uses an exact-zero policy: after finite-domain validation and
clamping, every positive Float32 Alpha or opacity remains observable. No
epsilon may reclassify a 16-bit edge, filtered sample, Effect join, or repeated
source-over contribution as transparent. Premultiplied input is divided by
every positive Alpha and canonicalized to zero RGBA only at exact zero.
Preview and export cache identities include `AlphaInterpretation`, so changing
the interpretation cannot reuse pixels produced under a different coverage
contract.

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

`ColorFrameDescriptor` stores `ColorFrameSpace::{Color, Working, Device,
NonColorData}` plus
`ColorFrameAlpha::{StraightCoverage, PremultipliedCoverage, Opaque}`. Frame
domain validation and OCIO processor planning therefore share the same role
distinction instead of inferring it from transfer characteristics, and no
shader-local boolean may become a second alpha-association truth. Native
NV12/P010 source frames declare opaque coverage. Source normalization produces
straight or opaque coverage before OCIO; public color, effect, composite,
display-calibration and output stages reject premultiplied input rather than
processing associated RGB as color.

Renderer stages must carry typed color-frame metadata. `CpuColorFrame` is the
CPU-resident Float32 Working-color contract and exposes no generic-domain
constructor. The renderer Adapter uses a distinct internal `CpuAlphaMaskFrame`
with no color-space identity for Mask frontier uploads; neither payload can be
mislabelled as the other. GPU frames expose the same
domain/encoding/residency/color-space/alpha descriptor. RGBA8 is a boundary
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
rather than silently changing color domain. Built-in blend and multi-input graph
nodes stay in the float working domain. Mask coverage is generated as Float32
alpha and never passes through an implicit 8-bit matte. A heterogeneous transfer
preserves that value as `ColorFrameSpace::NonColorData` plus
`ColorFrameDomain::AlphaMask`; OCIO and ordinary RGB effects reject it. The
production GPU Mask pass consumes this typed matte beside a scene-linear Working
frame, changes only straight alpha, and has real-device parity with the CPU
reference for every MaskOp and inversion state. GPU frame construction rejects
either half of an incoherent `NonColorData/AlphaMask` pairing before resource
publication.

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
contracts carry the complete 32-byte `DisplayCalibrationKey` derived from that
fingerprint, and LUT caches and pass preparation retain their own complete
sampled-LUT identity. `diagnostic_key()` is only a compact log/diagnostic
projection; it must never authorize equality, cache reuse, or processor
execution. A matching GPU pass
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
OCIO `GradingRGBCurve` may emit legal nested component l-values such as
`pixel.rgb.r`; Naga 30 parses that form but rejects the resulting store pointer.
The renderer's semantic-preserving GLSL lowering flattens identity nested
swizzles to `pixel.r` before Naga validation. Parse and validation failures emit
the generated GLSL source span and complete error chain, rather than only the
invalid function name.

Preview and export final transforms are renderer execution concerns.
The resolved output context's `output_transform` is the only product-level
transform truth;
the context does not duplicate resolved OCIO display/view strings.
App, thumbnail, and export code call
`RenderOutputColorBoundary::from_intent(...)`, which resolves the same intent
into a Display or Export boundary. They must not reinterpret Mondrian Standard,
construct display/export `RenderColorTransform` values, or duplicate working ->
output conversion logic locally. CPU reference execution uses
`execute_cpu_output_boundary_rgba8(...)`, which returns encoded pixels plus
color/stage diagnostics in the same boundary result. App/export crates must not
instantiate `RenderOutputColorBoundaryExecutor::cpu_only()` directly. Native
GPU execution uses `RenderGpuOutputBoundaryRuntime::record_wgpu_output_boundary_owned_backend(...)`.
Real-wgpu conformance tests read back `Rgba16Float` PQ display/view output and
compare it to the CPU OCIO path with distribution-aware Delta E ITP and separate
alpha budgets. This guards processor, wrapper, texture-format, readback, and
half-float precision as one output contract. The gate covers both the Mondrian
Standard 1000-nit PQ View and the separate ACES 2 1000-nit reference View; a
backend change therefore cannot silently break Standard while only the ACES
reference remains green.
Engine identity is enforced at processor creation, not inferred from View
spelling. A Mondrian Standard CPU or GPU request is accepted only when its
display/view pair belongs to the pinned Standard package contract. ACES Views
must use an explicit `ColorEngine::Aces` preset even if the immutable Standard
package's upstream base config happens to contain the same string; cross-mode
requests fail closed.

Stock OCIO emits tetrahedral 3D-LUT evaluation as six data-dependent GLSL
branches. Mondrian's wgpu lowering recognizes only that complete generated
block shape and replaces its corner selection with the algebraically equivalent
`min`/`max`/`step` formulation: the same four texels and the same tetrahedral
weights are sampled. It also lowers LUT reads to explicit level-zero sampling;
renderer LUTs have exactly one mip level, so this removes derivative work
without changing sampling semantics. Any unrecognized source shape is retained
unchanged. The recognizer closes the generated LUT operation at its own nested
brace boundary rather than at a following operation marker, so a terminal LUT
cannot consume the processor function's return or closing brace; the Standard
HLG wgpu preparation test covers that terminal-op shape. A 10,000-point corner/weight equivalence test, a 4096-pixel HDR
CPU/GPU Delta E ITP corpus spanning all six tetrahedra, negative values and
extended highlights, and the real-wgpu PQ gate protect this lowering. Linear
3D interpolation is explicitly rejected as a substitute: the fixed extended
range comparison differs from the tetrahedral reference by more than the
accepted drop-in budget.
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
blend/effect/transform paths. Product Timeline execution now treats any such
working-composite requirement as a pre-pixel blocker; the counters must never
be interpreted as a successfully published degraded frame. The summary is
evidence; the versioned health
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
Window, headless, and performance reports also retain compositor texture
bind-group creation/cache-hit counters. The renderer runtime report carries
the equivalent OCIO wrapper-input binding counters, allowing warm-frame gates
to distinguish necessary uniform/pass work from repeated backend object
creation without forcing a GPU completion wait.

Mondrian's `ColorSpace` enum maps to pinned OCIO color-space names in the
default config. The mapping is tested for every enum variant, and representative
delivery, HDR, and camera-log processor pairs must create real CPU processors.
Custom OCIO configs should provide the same names or aliases if they are used
with Mondrian's built-in `ColorSpace` enum.

## Color Encoding Contract

`ColorSpace::encoding()` is the canonical metadata source for color
primaries, transfer characteristic, matrix coefficients, and whether the space
is display SDR, display HDR, scene-linear, or scene-Log. Conversion code, preview
diagnostics, validation, and export tagging should read this contract instead
of maintaining separate ad hoc mappings.

`ColorSpace::ffmpeg_tags()` is derived from that contract and only returns tags
for standardized delivery/monitoring spaces. Camera-log acquisition spaces such
as Apple Log, S-Log3, and ARRI LogC4 currently do not emit FFmpeg delivery tags,
because writing guessed Rec.709 tags would mislabel the exported media.
Reverse media identification also lives on the same contract:
`ColorSpace::from_ffmpeg_tags(...)` resolves strict delivery tag triplets,
`ColorSpace::from_ffmpeg_colorimetry(...)` resolves complete input RGB
colorimetry without conflating it with sampling matrix, and
`ColorSpace::from_ffmpeg_tag_hints(...)` contains the centralized partial-tag
interpretation rules used by media probing. `mondrian-media` must capture raw
FFmpeg/CICP tags and call those core helpers rather than maintaining a separate
color-space mapping table.

Export encoding adds an explicit signal-representation contract after the
renderer output boundary. Sequence color management owns the creative output
identity and authored HDR metadata. A preset owns (or explicitly follows the
Sequence default for) encoded bit depth/range and owns its typed codec profile
and chroma sampling. `resolve_export_delivery(...)` validates those choices and
produces one immutable `ResolvedExportDeliveryContract`; there is no
execution-time `Auto` or encoder-selected profile. The pipe always carries
full-range encoded RGB;
`ExportVideoSignalContract` binds the codec pixel format, requested YUV range,
RGB-to-YUV matrix conversion, and emitted CICP matrix tag as one operation.
BT.2020-family outputs use the BT.2020 non-constant-luminance matrix, while
BT.709/P3-family and sRGB YUV deliveries use BT.709. An sRGB YUV delivery keeps
the sRGB transfer tag but uses a BT.709 matrix tag matching its actual YUV
samples; the RGB identity-matrix tag from `ColorSpace::encoding()` must not be
copied onto YUV media. Camera-log outputs still omit unverified delivery tags,
but their RGB-to-YUV matrix and range are explicit. GIF has no reliable color
tag contract and is therefore accepted only with an explicit sRGB output.

Static HDR metadata is currently supported only by the HEVC Main10/libx265
backend. SMPTE ST 2086 mastering-display and MaxCLL/MaxFALL values are emitted
through one atomic `x265-params` value so neither field can override the other.
AV1 and ProRes requests with static metadata writing fail validation until they
have independently implemented and verified bitstream/container backends.
ST 2086/CLLI are descriptive HEVC HDR SEI and are not artificially restricted to
PQ; HLG delivery may carry them when the authored delivery contract requires
it. Source HDR10+ and Dolby Vision data is not copied across a rendered edit:
the export health report emits `dynamic_hdr_metadata_sources` and
`dynamic_hdr_metadata_not_preserved`, and a `WriteAuthored` request fails before
encoder launch when referenced sources contain either
dynamic format. Dynamic delivery requires a separately validated re-authoring
workflow because source frame/scene metadata no longer describes edited pixels.

Successful encoder exit is not proof of a correct deliverable. Timeline export
derives `ExpectedVideoSignalConstraints` from the same
`ExportVideoSignalContract` used to build FFmpeg arguments, then probes the
finished file. Validation fails on mismatched pixel format, range, primaries,
transfer, or matrix. When `StaticHdrMetadataPolicy::WriteAuthored` is selected, the expected
ST 2086 and MaxCLL/MaxFALL payloads travel in the same validation contract. A
bounded second FFprobe call decodes only the first video frame, where libx265
exposes those SEI messages, and compares every mastering chromaticity,
luminance, MaxCLL, and MaxFALL value after encoder-scale quantization. Missing
or changed side data therefore fails the export even after a successful encoder
exit. `Omit` deliveries do not pay this extra probe.
Camera-log and GIF contracts additionally require color tags to be absent, so
an encoder cannot silently replace an intentionally untagged signal with
guessed metadata. ProRes 422 profiles are verified as 10-bit 4:2:2, while
ProRes 4444/4444 XQ are verified as 12-bit 4:4:4:4.

Delivery sample depth and renderer transport precision are separate contracts.
`DeliveryBitDepth` exposes only the 8-bit, 10-bit, and 12-bit formats implemented
by current codecs. H.264 High requires 8-bit 4:2:0; HEVC Main/Main10 require
8/10-bit 4:2:0 respectively; ProRes 422 profiles require 10-bit 4:2:2; ProRes
4444 profiles require 12-bit 4:4:4. A 10/12-bit delivery uses the internal
`EncodedRgba16Unorm` export frame contract and `rgba64le` FFmpeg pipe. Its GPU
output boundary is `Rgba16Float` so the renderer transform is not quantized
until the single explicit UNORM16 pack, but the pipe itself is never described
as Float16. `ExportFrameContract` separately defines true Float16 and Float32
master seams as `rgbaf16le` interleaved RGBA and `gbrapf32le` planar G/B/R/A.
Those float seams preserve finite extended-range values without normalized
clamping; UNORM seams validate finite input, clamp once to `[0, 1]`, and
quantize. Non-finite samples and finite values outside Float16 range fail
closed. No current user-facing preset selects a float master until the image/
video master backends are implemented. If GPU output and the renderer-owned
CPU float boundary both fail, export stops with structured precision-failure
diagnostics; an RGBA8 boundary is never expanded into a nominally high-bit pipe.

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
and tone-map intent into one exact contract. Output
space and tone mapping come from one explicit thumbnail presentation contract;
the worker must not infer them locally. The product thumbnail host derives that
contract from the Project engine and future-Sequence input/working defaults,
then requests an explicit sRGB rendering target independently from the active
Sequence. It executes the same
renderer CPU input and output boundary APIs used by preview, and submits the
result as `RasterImageColorSpace::Srgb` to the UI atlas. Cache, failure,
pending-request, active-request, and raster-atlas identities include the full
color contract. A color-context change clears visible thumbnail state and
invalidates in-flight request ownership, so a completion for an older exact
engine/contract or request generation cannot overwrite a newer completion.

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
tone-map intent, and file fingerprint. Both successful frames and
negative-cache entries are isolated by that exact identity. An OCIO hot reload
of changed Custom bytes must first resolve a different complete `ColorEngine`
identity; lifecycle code may use the operational generation to request
reevaluation, but it cannot change cache equality or make an old engine's
working pixels satisfy the new key. Display/output invalidation alone is not
sufficient because the exact input processor identity may also have changed.

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
a metadata hint, an ICC profile, supported CICP tags, unsupported CICP tags,
missing metadata, or decoder unavailability; UI, logs, and export reports
surface that method instead of asking users to infer provenance from raw tags.
HDR stream and first-frame side data are captured as
`VideoHdrMetadataSummary` entries on `VideoStreamInfo` and copied into
`VideoColorDiagnostic`. The summary records side-data kind, payload size, and a
typed `mondrian-core` payload when FFmpeg exposes a stable ABI. ST 2086
mastering display metadata and CTA-861.3 MaxCLL/MaxFALL content-light metadata
are parsed into shared core value objects that can also format x265-compatible
`master-display` and `max-cll` strings. Common HEVC files expose those values
only after decoding, so already identified HDR video receives one bounded
first-frame metadata decode during the background media probe; SDR imports do
not. Stream and frame facts are de-duplicated by semantic kind. ICC payloads are
parsed into a profile name plus an explicit
`IccColorSpaceMapping::{Mapped, Unmapped}` result. First-frame HDR10+ and stream
Dolby Vision configuration remain presence/diagnostic records until dedicated
parsers are introduced.
Sequence/export static HDR authoring stores these typed core payloads directly.
`StaticHdrMetadataPolicy::{Omit, WriteAuthored}` is explicit and never denotes
source passthrough. `WriteAuthored` fails closed when either ST 2086
mastering-display metadata or MaxCLL/MaxFALL content-light metadata is missing;
export must not synthesize hidden defaults.
Core validates positive rational denominators, physical CIE xy coordinates,
ordered mastering luminance bounds, and `0 < MaxFALL <= MaxCLL` before an
encoder string can be formed. ST 2086 mastering luminance describes the display
used while authoring, so it is deliberately not required to equal Standard's
content peak. MaxCLL describes the brightest content pixel and therefore must
not exceed the fixed Standard View peak when `WriteAuthored` is selected.
Preview/export parity is protected by frame-level contracts: app preview tests
compare multilayer preview compositing against the export output boundary with a
stable RGBA hash, compare the shared preview/export color-health fields for the
same frame, and compare normalized report verdict/check/root-cause/action
signatures. The product Color Media Golden uses the shared Hero Sequence rather
than an isolated color document: its HLG Main10 and sRGB straight-Alpha sources
occupy two adjacent stage-owned Tracks in the exact `500..525` window, with HLG
below Alpha. Reference execution explicitly selects the original source, never a
proxy; the HLG Clip keeps its exact source interval, the PNG keeps a zero-rate
hold, and durable reopen retains typed Track/Clip/Asset identities plus canonical
hashes. It proves decoded-code, independent sRGB-to-linear-Rec.2020, Alpha
isolation, production Preview/Export/reimport, and coexistence with earlier
authoring. It deliberately does not claim an independent absolute HLG
transfer-function oracle by itself or close Camera Log. HLG absolute qualification
is instead owned by the independent core oracle and the renderer-wide Standard
HLG/PQ luminance corpus described above.
Real-device execution of those color boundaries is a separate sealed contract:
the Windows Commercial Engine runner must carry the `mondrian-gpu-color` label,
execute the complete versioned accuracy/performance profile under
`sealed-required`, bind all wgpu reports to one captured DX12 GPU/driver, and
publish zero skipped gates. Ordinary cross-platform test success cannot replace
that evidence when an adapter or timestamp feature is unavailable.
A separate camera-log golden begins with encoded Sony
S-Log3/S-Gamut3.Cine bytes, exercises the preview lazy input transform into
Linear Rec.2020, and compares the resulting Standard sRGB frame pixel-for-pixel
with an independently executed export input/composite/output chain. These app
goldens name and pin the current Standard package and SDR View generation, so a
new immutable package cannot inherit the prior package's expected hash by
accident. Renderer
golden tests cover lower-level compositing fixtures. GPU and float-pipeline
changes must keep these contracts green or update them only with intentional
visual-reference and diagnostics-contract changes.

Camera-log output is treated as a professional intermediate path. Export
validation rejects consumer delivery codecs for camera-log output and only
allows 10-bit-or-higher MOV/MXF ProRes configurations until richer metadata
carriage is implemented.

## Unknown Media

Media probing must preserve the distinction between diagnostic candidates,
validated executable metadata, and policy assumptions.
`VideoStreamInfo.color_interpretation.candidate_color_space` is UI/diagnostic
state only; `VideoStreamInfo::executable_color_space()` is the sole validated
Auto identity allowed to reach rendering. It revalidates raw sampling, CICP, and
metadata declarations on every call, so a serialized candidate, confidence,
authority enum, or evidence record cannot authorize pixels after its raw probe
facts are missing or disagree.
`VideoColorSpaceSource::{MissingMetadata, UnsupportedMetadata,
DecoderUnavailable}` are distinct diagnostic source states, not permission for
callers to bypass missing-metadata policy. `MissingMetadata` means that no CICP
field was specified. Any specified but unresolved, ambiguous, contradictory, or
unsupported CICP combination is `UnsupportedMetadata`; it must not be reported
as absent metadata or silently guessed as a familiar product space.
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
automatic detection. It carries the candidate color space, confidence,
source/method, evidence, warnings, and whether the result is user-overridable.
Evidence distinguishes typed metadata declarations from suggestions, exact
CICP facts, partial CICP candidates, unsupported CICP tags, mapped/unmapped ICC
profiles, and decoder unavailability. An ICC `RGB`/`GRAY` signature or profile
description identifies neither verified primaries nor TRC. Until a verified
ICC chromaticity/TRC mapping or processor route exists, all ICC name mappings
remain candidates and `executable_color_space()` returns `None`. An ICC-only unmapped stream
with no CICP tags remains `MissingMetadata`, preserves the profile as evidence,
and emits `IccProfileUnmapped` so normal missing-input policy can reject or
diagnose its fallback. If specified but unsupported CICP tags accompany that
ICC profile, the source and method remain the unsupported-CICP state and both
pieces of evidence survive. Warnings must
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
decoder unavailability, raw CICP presence, and HDR side-data presence. It keeps
separate counters for missing and unsupported CICP tags, so UI panels, perf
JSONL, and export reports do not parse diagnostic text. Aggregates likewise
count `method_missing_metadata` and `method_unsupported_cicp_tags` independently.
Preview rejection logs and export failures must include the media asset id,
path, missing-metadata policy, and `VideoColorDiagnostic` summary so users can
identify whether the problem was missing tags, unsupported tags, or decoder
unavailability.

When executable media color identity is missing, `MissingColorMetadataPolicy` resolves it as:

- Assume Rec.709
- Assume sequence working space
- Reject media

Clip-level `MediaInterpretation.color_space_override` takes precedence over executable metadata.
Preview and export both follow override -> executable metadata -> missing-policy
resolution through `MissingColorMetadataPolicy::resolve_input_decision(...)`.
The returned `InputColorResolution` is the shared diagnostic record for the
decision branch, including override, executable metadata, policy, sequence working
space, and whether the media was rejected. Each Export
`TimelineExportSnapshot.media[AssetId]` dependency stores the single structured
color diagnostic beside the matching persistent interpretation and source
revision; it does not cache a second color identity. Absence of executable color means "resolve via policy", not
"fallback to Rec.709"; absence of a required Asset dependency is an invalid
snapshot and fails closed. The diagnostic, including `color_interpretation`,
must be used for failure messages and reports, not for choosing the transform.
`VideoColorDiagnosticIssueAggregate` is the stable rollup for these per-asset
snapshots: export job diagnostics carry it as `asset_issue_summary`, and
preview media smoke reports serialize the same aggregate as `media_color_issues`
for CI/perf JSONL. Export scopes the aggregate through one immutable
selected-range `ExportMediaDiagnosticSet`: prepared visual interval
reachability excludes hidden/muted and range-outside placements, includes
Transition endpoints and nested dependencies, and expands child reachability
for finite or unbounded temporal Effect extent. Static-HDR delivery preflight
and the eventual job report consume the same frozen aggregate; neither may
re-walk author Tracks or fold every cached diagnostic record in memory. App
viewer rejection messages and export-queue labels should read the same
structured summaries instead of inventing a parallel free-form issue taxonomy.
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

`SequenceSettings::root_program_color_context(...)` builds the shared Program
Output context from the Sequence output color space. Preview and scopes consume
this semantic boundary before any local monitor adaptation. Its
typed output intent is resolved exclusively from the effective color engine and
requested output target; display management cannot replace that engine-owned
intent. `ProgramColorContext` is a closed value: its fields are private and its
output is either an encoded endpoint or an internal working-space endpoint.
Root construction validates the Project engine, Sequence working space,
display-referred output, intent identity, and rendering-View mode even when a
caller has not separately run Sequence settings validation. The concrete
tone-map getter is derived from the validated transform resolving a rendering
View; it cannot drift from the output intent as a separately mutable flag.
The native GPU Viewer resolves this Program Output context first. The
CPU raster fallback does the same through the presentation-only
`execute_cpu_program_monitor_presentation_rgba8_with_session()` Interface: it
retains the Program Output descriptor and diagnostics, consumes the uniquely
owned float pixels for stock-OCIO monitor adaptation to the sRGB UI atlas, and
quantizes only at the atlas boundary. Callers that require Program Output pixels
for Export or scopes use the retained-output Interface, which preserves those
pixels and produces a separate monitor output. There is no Sequence “preview
color context”:
prefetch/preroll use Program semantics only to derive media-input contracts,
while presentation resolves monitor policy after Program Output.

Export admission resolves an independent `ExportColorTarget`. `FollowSequence`
uses Program Output; `Colorimetric` permits an explicit display or Camera Log
encoding without changing the Sequence; `RenderingView` resolves exactly one
View from the Project engine and requires a display-referred target. The
resolved target, encoded representation, renderer boundary, FFmpeg tags, and
post-encode validation share one admitted contract. Export derives that contract
through `ProgramColorContext::for_export_output(...)`; direct field replacement
is impossible. Only a root context may derive an encoded output. A nested
context is closed to `Working(parent_working_space)`, carries no rendering View,
and fails if a caller attempts to promote it to delivery or display output.
Thumbnail and other display-derived paths use
`for_rendering_view_output(...)`; if the Project engine cannot prove that View,
they remain unavailable instead of inventing a fallback context.

`RenderMonitorAdaptation` is the renderer-owned preview-only contract from the
encoded Program Output identity to the local monitor identity. It is a stock
OCIO color-space processor, never a second View Transform. Matching identities
take a zero-pass route. SDR-to-HDR and HDR-to-SDR requests fail closed rather
than hiding a second rendering transform inside monitor adaptation. The Viewer
GPU runtime retains the pre-adaptation Program Output handle independently from
the final monitor handle, keeping scopes and future Program Output caches
unaffected by ICC/surface policy.

Display management is machine-local runtime state. `DisplayManagementPolicy`
has private fields and validated construction for four independent choices:
`MonitorOutputIntent`, `DisplayCalibrationPolicy`, `IccRenderingIntent`, and
`ViewerDisplayMode`. It is persisted only in the versioned user-level
`AppUiPreferences`, restored into `AppState` before Preview starts, and is
never persisted as Project or Sequence author data. Exact OCIO display/view
preferences are admitted only when the active engine maps that pair to a
standardized monitor output target; an arbitrary View cannot become a second
creative output authority. The Program Output tone-map policy is a different
concern and remains explicit in
`SequenceColorSettings.program_output`.
`DisplayToneMapPolicy` resolves the concrete Program Output `tone_map` flag for
working -> output boundaries, including HDR-working to SDR-output delivery, so
preview, export, cache keys, and diagnostics do not infer it from scattered
booleans. Monitor adaptation follows Program Output and cannot replace its
engine-owned rendering View. ICC is a separate final device-calibration stage
with perceptual, relative-colorimetric, saturation, and absolute-colorimetric
intents. Surface capability remains Window/platform evidence and is never
inferred from either preference.

Root contexts retain one typed `OutputTransformIntent`. Colorimetric contexts
carry no view. Tone-mapped Standard contexts carry the immutable package
identity; core resolves its target-specific SDR/PQ/HLG display/view only when
renderer builds the boundary and rejects engine/package drift. ACES contexts
carry a target-aware preset; Custom OCIO contexts carry a target-qualified
`CustomOcio` intent and resolve the named View only from that target's exact
project binding. A missing,
unsupported, or invalid engine-owned output mapping fails closed instead of
falling back to a different View or partially populated strings. Explicit
environment OCIO remains fail-closed
when `$OCIO` is not configured.
Preview cache keys include the complete typed output intent. An intent change
invalidates cached viewer frames even when the output `ColorSpace` is unchanged.

Program video scopes are measured from the exact float display/export-encoded
Program Output, before any monitor/ICC adaptation or UI raster conversion.
`mondrian-core::compute_program_color_scopes_rgba_f32` requires an explicit
standardized output identity and selects Rec.601, Rec.709/sRGB, Display P3, or
Rec.2020 luma/chroma coefficients accordingly. Working-linear and camera-log
identities, malformed buffers, and non-finite RGB fail closed. The RGBA8 helper
exists for already-quantized SDR boundaries, but HDR/10-bit scope paths must use
the float helper. Negative and above-nominal RGB/luma values remain visible as
explicit `ProgramSignalExcursions` even though the density plots group them into
their endpoint bins. A final renderer float boundary exposes this contract through
`RenderOutputColorBoundaryFloat::program_scopes`, so callers cannot accidentally
measure working pixels or a monitor-adapted `ViewerFrameImage`. GPU scopes must
use the shared `ProgramSignalColorimetry` coefficients and a compute/reduction
path over the Program Output texture; they must not introduce a per-frame
GPU-to-CPU readback. The production GPU runtime uses exact atomic-u32 counts and
generates display density textures on-device. Viewer validation rejects a scope
request whose signal identity differs from the Program Output boundary before
recording any GPU work.

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
`RenderGpuOutputBoundaryRuntime` owns GPU output state for one explicit
execution lifetime. Preview retains it in the live Viewer device Session.
Export creates a separate instance per queue attempt, permits reuse only across
frames of that attempt, scopes GPU-failure backoff to that attempt, and releases
the instance before terminal publication. It keeps the OCIO shader cache, pure
backend prep runtime, concrete backend-object runtime, GPU frame id allocator,
and GPU frame table together without process-global or cross-job residency.
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
`EncodedRgba8` contracts and do not reinterpret float targets. Preview and
Export both call the output-stage recorder in production. A typed GPU failure
may select the renderer-owned CPU correctness boundary when the output contract
permits that fallback; diagnostics must record the attempt, reason, transfer
shape, and actual CPU result instead of reporting GPU execution.

Preview presentation and export may target different encoded output spaces
while sharing the same Project engine and Sequence program semantics. Nested
processing is resolved per placement edge during recursion; it is not a root
Sequence policy.

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

The renderer's real-wgpu parity gate executes one varied Linear Rec.2020 float
stimulus through every Mondrian Standard target (sRGB, Rec.709, Rec.2020 SDR,
Display P3, HLG, and PQ), reads the production RGBA16F boundary, and compares it with the
stock-OCIO CPU float result under a 0.001 maximum channel-error budget. The six
targets share one device/runtime during the test, matching production cache
reuse instead of hiding target-specific shader drift behind separate setup.

CPU fallback is always explicitly recorded — never silently used as "GPU ready".
Preview and export never independently implement color math; they share
`RenderOutputColorBoundary` and `RenderColorTransform` resolution through the
renderer layer. Preview supplies `ProgramColorContext`; Export supplies its
admitted target with the same Project engine and working-domain contract.

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

## Bounded Support and Explicitly Unsupported Features

The following boundaries are either intentionally blocked or implemented only
for a documented production subset. Unsupported requests use
`PreviewGpuOutputBlocker::UnsupportedFeature` with stable `feature` codes and
the `document_unsupported_feature` action code; bounded support must enumerate
its admitted subset and fail closed outside it.

- **Enabled `DisplayCalibrationPolicy` outside a resolved Display Output
  Contract** — Windows WCS, macOS CoreGraphics, Wayland color-management-v1,
  and X11 root properties provide platform-native default ICC discovery through
  `mondrian-platform`, and preview scheduling consumes the resolved Display
  Output Contract when it maps the ICC profile to a managed monitor color
  space. If the contract is missing, invalid, unreadable, or unmapped, preview
  fails closed with `icc_preview_color_space_resolution` or the structured
  display-contract blocker. It must not silently fall back to Rec.709, sRGB, or
  the sequence output color space.
- **HDR presentation without complete native evidence** — `ViewerDisplayMode::HdrPq` /
  `ViewerDisplayMode::HdrHlg` are explicit user selections. On Windows the app
  probes DisplayConfig Advanced Color support/enabled/force-disabled state, bits
  per channel, color encoding, and SDR white level through `mondrian-platform`;
  known disabled or unsupported state blocks HDR preview. wgpu
  `SurfaceColorSpace` compatibility is still only the swapchain side of the
  contract. macOS adds AppKit EDR headroom, Wayland consumes the active output
  image description, and Linux DRM/EDID remains hardware-only fallback evidence.
  Any unavailable or hardware-only probe records
  `MonitorHdrCapabilityUnknown` / `MonitorHdrCapabilityUnsupported` blockers
  when HDR correctness cannot be confirmed.
- **GPU compositing (resource-admitted production path)** — The `gpu_compositor.rs` module is wired into the
  preview/viewer GPU path for the supported production set: media-layer affine
  transforms, procedural-solid affine transforms, every canonical BlendMode, supported
  fused working-linear media/solid effect chains, working-linear adjustment
  layers, with no semantic layer-count ceiling.
  It composites into an `Rgba32Float` working-space GPU texture, then feeds the
  same renderer-owned OCIO GPU output boundary used by the rest of preview.
  GPU effect lowering supports ColorAdjust, Vignette, and deterministic Grain
  without an encoded/RGBA8 intermediate. Adjustment plans sample the current
  accumulator, process it in the same working space, and blend the result back.
  Unsupported operations fail back to the CPU reference compositor with
  structured `GpuCompositingDiagnostics` blocker reasons (`EffectRequiresCpu`,
  `UnsupportedTransform`, `GpuUnavailable`). The shared GPU
  compositor implements the CPU Float32 straight-alpha algebra for every mode;
  Dissolve carries the complete frame seed and destination pixel identity, and
  real-device parity covers threshold and non-separable channel-tie inputs.
- **`FrameNotGpuResident` blocker** — The current preview GPU compositing path
  supports CPU-layer upload into GPU compositing (`GpuWithUpload`) and then keeps
  the composited working frame GPU-resident for OCIO output. This blocker is
  reserved for future paths that require already-resident media textures and
  intentionally disallow upload.

The `UnsupportedFeature` blocker variant exists precisely so that when these
limitations are resolved, the diagnostic path can be updated without changing
the taxonomy.

## HDR/SDR

HDR output spaces include Rec.2100 PQ/HLG. Tone mapping is required when scene/HDR working data targets SDR output. Authored static HDR metadata can only be written for HDR output spaces.

The native Viewer presentation seam never blends machine UI in PQ/HLG code
space. Renderer output remains encoded target code values, while the UI carrier
decodes it into target-primary display-linear values using a 100-nit reference
white, composites linearized UI there, and applies the final PQ/HLG encoding at
the swapchain store. Display P3 follows the same target-primary linear rule with
the sRGB transfer. ICC-calibrated output is not relabelled as one of these
standard targets; it uses the separately typed opaque device-code carrier.
## OCIO Cache Revision Contract

Mondrian Standard and pinned builtin ACES packages are immutable and use cache
revision zero; their complete package/source identity is already part of every
processor and shader request. Validated Custom path and environment configs
also carry their complete resolved digest identity; reloading changed bytes
produces a new engine key. The monotonic OCIO selection generation is
operational evidence, not semantic cache identity. CPU processor and renderer
GPU shader caches therefore cannot alias changed mutable sources, while a warm
owner-scoped lookup performs no generation lock or filesystem check.
