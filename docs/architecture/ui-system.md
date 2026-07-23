# UI System

Mondrian's UI is self-hosted: winit/platform integration, retained widgets, wgpu rendering, theme tokens, event routing, dock/layout, and app panel adapters.

Color-space selectors expose Rec.601 PAL and Rec.601 NTSC as distinct encoded
identities. They are not native window surface color spaces; viewer
presentation still passes through the configured display/view transform before
targeting an sRGB, Display P3, PQ, or HLG surface.

## Crate Split

- `mondrian-ui-core`: widget trait, event types, accessibility, focus/shortcut/tooltip traits, tree traversal.
- `mondrian-ui-theme`: semantic tokens and theme preference.
- `mondrian-ui-layout`: reusable layout algorithms.
- `mondrian-ui-renderer`: draw-command renderer over wgpu.
- `mondrian-ui-text`: text layout/raster support.
- `mondrian-ui-events`: event routing, focus/capture/shortcut/IME/DnD side effects.
- `mondrian-ui-tooltip`: tooltip manager/widget.
- `mondrian-ui-widgets`: controls and editor-specific reusable surfaces.
- `mondrian-app::app_ui`: product shell and panel adapters.

## Widget Contract

`Widget` is retained-mode and exposes:

- `measure`
- `layout`
- `event`
- `paint`
- optional overlay paint/hit-test
- children traversal
- focus/accessibility metadata

`paint()` must be side-effect free. `event()` may request platform side effects through `EventRequests`; app/platform layers execute them.

Inspector effect rows are projected from the domain `ParameterSchema`. Numeric
soft bounds and step, enum options, and typed resource intent come from that
single schema; `AnimatablePropertyUiMetadata` only groups or spatially lays out
controls. The UI routes mutations through the instance address but displays and
persists the definition-stable `ParameterId`. It must not recreate ranges,
accept enum keys absent from the schema, or flatten a resource reference into a
generic text parameter.

Basic Title Inspector rows use the same definition-backed property projection
and mutation action as other Clip content; the panel does not own a parallel
title draft or reconstruct property ranges/options. Text is multiline, the
concrete font family remains editable regardless of display length, and
font-size/fill/tracking/line-height animation is evaluated at exact Clip
source-local author time. A mutation validates Track lock and the complete
candidate title before recording one Sequence snapshot. The generic legacy
solid-color tint row is hidden for Basic Title so two controls cannot claim
authority over its fill.

Inspector audio source controls project existing author state rather than own
it. Each row addresses one stable `AudioComponentEditId`; media choices carry
only Asset `AudioSourceComponentId` values and nested choices carry only child
`ProgramOutputId` values. The application validates source domain, Asset/child
membership, track lock, and the complete audio author aggregate before it
records one undoable Sequence snapshot. A neighboring Asset stream-mapping row
is intentionally a different command family: refresh acquires current probe
evidence without retargeting, and rebind atomically changes one Asset catalog
binding while preserving its logical ID. UI labels may describe physical stream
indices, language, title, and layout, but those indices never enter Timeline
authoring payloads.

The same Component section projects enabled state, static dB volume,
normalized pan/balance, and independent exact edge fades. Every interaction
emits one typed field mutation instead of serializing the whole
`AudioComponentEdit`; the application re-resolves Clip/Track/edit ownership and
validates the full candidate before commit. Volume exposes a normal working
range of `-60..+12 dB` while preserving the author hard range
`-120..+24 dB`; pan displays `-100..+100` and crosses the domain boundary only
as normalized `-1..1`. Fade seconds are quantized once to exact
`TimelineTime`, bounded by Clip duration, and zero means absent. Curve selection
preserves the exact duration and is disabled when that fade is absent. The
panel owns no parallel audio draft and no execution interpretation.

## Product SVG Capability

Bundled product SVGs are static UI artwork, not a general document or title
format. Their admitted subset is path/group geometry with fill, stroke, clip,
gradient, and transform semantics. The repository's SVG corpus contains no
text nodes, embedded raster images, remote resources, or foreign objects, and
the `usvg`/`resvg` dependencies therefore disable their default text,
system-font, memory-mapped-font, and raster-image features. `VectorIcon`
normalizes this subset into retained meshes and an optional cached raster; the
app favicon uses the same path-only rasterizer.

Adding an unsupported SVG feature requires an explicit capability decision,
new adversarial parsing/raster tests, and a dependency review. It must not
silently widen every UI process's parser surface. Timeline titles and imported
visual media use their own typed authoring and media pipelines and must never
be routed through the product-icon parser.

## Focus and Accessibility

Focus ownership is not the same as visible focus indication. `FocusSource::Keyboard` may show a focus ring; pointer/programmatic focus owns keyboard input but normally does not show the ring.

Accessibility `focused` must reflect real focus ownership, not `focus_visible`.

## Overlay and Menus

Dropdowns, context menus, popovers, and tooltips should render through overlay paint/hit-test so they are not clipped or hidden behind sibling panels. Menubar menus and context menus should share menu primitives.

## Commands

Menus, shortcut preferences, command palette, and future plugins should consume `app_ui::commands` descriptors. Menus are command presentation, not business logic owners.

The startup and editor shells share the same new-project dialog state machine.
New-project and project-settings workflows consume one app-UI color-engine
catalog; neither dialog owns the product list of Mondrian Standard, pinned ACES
presets, and Custom OpenColorIO. Their dropdowns produce workflow-local draft
actions while the shared native-selection boundary validates a chosen `.ocio`
file and pins its working/display/view and digest identity. Invalid configs
remain visible as dialog errors and never create a partial path-only mode. The
two shells only route requests and must not duplicate this state transition.
Applying the project-settings modal emits one complete project-domain engine
replacement action. App-domain admission prepares the candidate config and
validates the future-Sequence template plus every existing Sequence atomically;
the modal never mutates renderer, Sequence, or persistence state directly.
Sequence settings shows the Project engine as read-only and disables its
working-space control when that engine pins one immutable working identity. The
shell draft also ignores forged changes through that disabled control;
app-domain validation remains the final authority for persisted actions.
The workflow control presents SceneReferred first because new Standard
sequences use the package-pinned product View by default. DisplayReferred remains
an explicit direct-colorimetric bypass; the UI must not relabel it as the normal
Standard path.

Opening an editor dialog is a shell action because it mutates transient UI state,
not the undoable domain model. The dialog's committed payload must flow through a
domain/app action owned by the target subsystem. For example, Asset Library →
Interpret Footage opens an app-shell modal, but applying Auto/Override is
an asset-library mutation that persists `AssetMediaInterpretation`.
The modal keeps the input color-space and encoded signal-range dropdowns compact
while retaining a read-only diagnostic body. Auto is the default for both;
explicit color spaces and Full/Limited range selections persist independently,
and Auto displays the current resolved/detected result. Changing either control
must preserve the other control and the asset payload classification.
Preview and thumbnail work keys preserve that Auto/Override authority through
`DecodedVideoRangeContract`; the UI must not flatten it to the currently shown
probe value before decode, because an explicit frame tag may refine Auto while
an authored override must continue to win.
The action that opens the modal must preserve decoder range, raw CICP
primaries/transfer/matrix, detector evidence and warnings, plus the effective
engine and working identity; reducing this payload to the final color-space
guess loses the facts needed to audit an inference. The dialog displays the
resolution method/confidence, whether the result is inferred, user override
state, signal tags, evidence/warnings, the input-to-working path, and the exact
stock-OCIO processor cache-id. Processor identity is queried only when the user
opens or changes the dialog, never while rebuilding asset cards.
Machine-readable diagnostics also retain counts for lower-priority metadata
hints rejected by CICP or ICC, so preview/export health surfaces can distinguish
an unambiguous result from one that won over conflicting comments or file-name
inference without parsing display strings.
The picker includes encoded delivery/camera spaces plus supported scene-linear
and ACES source identities. Sequence output controls use a separate fixed list
of display-referred spaces, so ACES2065-1, ACEScg, ACEScct, linear RGB, and
camera Log identities cannot appear as presentation targets.
Non-color data is an asset payload classification for advanced utility-channel
workflows, not an option in the primary color-space picker. A future payload or
channel-role control may edit that classification, but the Interpret Footage
color-space and range dropdowns must preserve the existing payload value while
changing only their owned interpretation field; they must never create or clear
`AssetColorPayload::NonColorData`.

## Timeline Position Presentation

Sequence settings persist one `TimelineDisplaySettings` value and resolve it
through the core `TimelineDisplayContract`. Viewer playback chrome and Timeline
ruler receive that resolved Interface from the app panel Adapter. Widgets may
choose compact label density, but may not infer NDF/DF, apply an origin, clamp
negative time, or split/reassemble a formatted timecode string.

Changing Frames/SMPTE presentation is an undoable Sequence-settings edit, not a
transport or media-time mutation. If persisted settings do not validate against
the Sequence frame rate, the domain rejects the snapshot; a forged invalid
runtime state fails visibly instead of silently selecting another counting
mode. The product menu exposes only formats with implemented parsing/formatting
and reference tests.

Visual Transition UI intent follows the same domain-light Widget seam as Clip
editing. `mondrian-ui-widgets` may emit track/Clip/Transition view indices and
frame-grid gesture proposals, while the App panel Adapter resolves stable
identities. Selection stores only `VideoTransitionId`; it never mirrors a Track
identity that author validation derives from strong Clip endpoints. Default
duration, exact Timeline Time conversion, source-handle admission, Track locks,
and the one-Undo author transaction remain App-owned. Ordinary Delete targets a
selected Transition; Ripple Delete is unavailable because deleting a
Transition cannot move Timeline placements.

The Timeline Adapter produces every Clip/Transition stable identity and its
domain-light view model in one projection pass. It must not build a second ID
list with an independent filter: failed time projection is allowed to omit one
view, but can never shift later gesture indices onto another author entity.
Nested-Sequence identities travel in that same Clip projection. The Widget owns
only overlay geometry, hit testing, resize preview, and an edge-constrained
frame proposal. Transition overlays take hit priority over their endpoint Clips;
right-clicking an exact adjacent unlocked video cut exposes the default Cross
Dissolve command. Resize preview never mutates the supplied model and commits
exactly once on pointer release.

Transition colors and blocked/hover/selection states are semantic theme tokens.
The App Adapter re-observes current external source-handle availability when it
projects the low-frequency author view and exposes a fail-closed diagnostic. It
does not repair, shorten, or mutate the authored range. Media execution and
per-frame playback therefore remain outside the Widget and panel seams.

Basic Title creation is a stable command exposed by the Graphics menu and
command registry. The App authoring Module—not the menu, panel, or Timeline
Widget—chooses the selected/free video Track, derives the selected range or
five-second default from the current edit state, adds a Track only when all
existing unlocked tracks are occupied, and commits placement plus any new
Track as one Undo transaction. Timeline presentation uses dedicated semantic
title colors and the ordinary Clip selection/trim/drag model.

## Semantic Action Completion

`Action` is an intent envelope, not proof that work happened. `AppState` accepts
only Actions owned by a concrete product Interface. Shell-only window/layout
Actions, unknown custom namespaces, and product operations without an
implementation return a structured failure; they never log and return
`Ok(())`. Undo/Redo errors cross the same boundary instead of being discarded.

Callers that need acceptance evidence must also verify domain postconditions.
For example, the Golden audio slice checks the installed `AuthoringSession`,
exact Sequence contract, stable Clip and Component Edit identities, values
before/after all Undo and Redo steps, a precise Generation/Sequence Revision
advance for every transaction, durable request identity and archive hash, and
values observed under a distinct fresh Session after load. Action admission or
a human-readable status hint alone cannot satisfy a Golden operation.

## Playback Tick Ownership

The winit host may wake the application while playback is running, but playback
state transitions belong to `AppState`. Window code passes elapsed time into
`AppState::advance_playback_clock(...)` and only reacts to the returned refresh
contract.

Playback frame advancement must:

- keep sub-frame elapsed time in an app-owned accumulator
- use the configured clock role for frame targeting
- pause on the last content frame and mark natural end-of-playback separately
  from user pause/seek
- expose a bounded next-frame wakeup delay so the UI loop does not busy-poll

Viewer preview rendering remains an adapter concern. It consumes the current
playback frame from `AppState`; it must not own playback state or mutate the
timeline to request frames.

Monitor direct manipulation uses viewer-scoped UI actions with sequence-space
payloads. The viewer surface may emit absolute clip transform intents for
position, scale, and rotation, but `AppState` remains the single mutation owner:
it validates payloads, checks track locks, applies timeline property mutations,
and records one undoable snapshot for each committed monitor edit.

## Viewer Preview Scheduling

The Color workspace owns a real `Scopes` panel rather than aliasing the Effects
panel. Expensive analysis is demand-driven by the live dock tree's active tab,
without allocating a persisted layout snapshot on each frame. Mere panel
presence is insufficient: a hidden or background Scopes tab schedules no
renderer aggregation, buffer clearing, readback, or scope repaint work. Scope
input is the retained Program Output boundary before local monitor adaptation,
so moving the window between monitors cannot change measured program values.
The window registers GPU-generated waveform, histogram, and vectorscope
textures with stable UI keys using the linear external-texture contract, and
unregisters all three when Scopes is hidden or Viewer presentation is reset.

Playback-frame refreshes use a narrow UI update path: the host advances
`AppState`, then refreshes viewer playback chrome/frame data and the timeline
playhead without rebuilding the full dock tree. A transport-only turn retains
the currently installed frame, transparent-canvas state, pending/stale state,
and typed blocker; passing an absent Preview source must never erase those
facts. The next production Preview turn alone may commit their replacement.
Preview completion and GPU external-texture registration/clearing use the same
narrow presentation refresh domain. They set `preview_dirty`, never the global
`ui_dirty` model-rebuild flag. Media import, project, preferences, workspace,
and other author-facing changes retain the full model path.

The titlebar, menu bar, and their child trigger Widgets are persistent for the
Window Session. A full model projection updates their title, command
availability, checked state, and shortcut labels in place; it does not replace
their Widget identities. Hover, press, focus, pointer capture, open overlays,
and other transient interaction state remain owned by the existing Widgets and
must not depend on Preview readiness or redraw cadence.

Timeline audio waveforms are not a Widget or Window execution feature.
`AppUiHost` owns one UI-independent `AudioWaveformService` composition instance,
polls its bounded completion pump, and injects an `AudioWaveformSource` handle
into the Timeline model. The Widget supplies `AssetId`, a source revision
derived from the immutable asset record, current file length/modification time,
and probed primary-audio facts, the visible source interval, and presentation
width. The handle returns only a
resident envelope or `None`; it cannot expose FFmpeg, worker channels, cache
mutation, generation state, or failure policy to paint/layout code. Project
library replacement rotates service generation, and source revision prevents a
same-asset relink from presenting stale waveform data.

Asset thumbnails follow the same Window boundary but retain an independent
execution policy. `AppUiHost` owns one
`app::thumbnail_service::AssetThumbnailService` through the shallow
`AssetThumbnailAdapter`; the panel performs only a nonblocking lookup and
receives `Loading`, structured failure, or a resident image. The service—not
the panel—owns source/color identity, deterministic still decode, bounded
admission and transport, generation cancellation, weighted raster LRU, failure
memory, and terminal diagnostics. The adapter converts the validated sRGB
`ThumbnailRasterFrame` to `RasterImage` without copying its `Arc<[u8]>` and
cannot manufacture a second cache or scheduling rule.

`app::preview_runtime::PreviewProductionRuntime` owns media preview scheduling.
`WindowPreviewAdapter` is only its Window output specialization. Each viewer preview request
starts a monotonic generation, and background media jobs check that their key is
still requested by the latest generation before decoding. Completed stale jobs
may warm the cache, but they do not force a UI refresh for an older playback
frame.
The same production runtime owns one bounded Basic Title task. The Timeline
evaluator emits a complete generated-title request including evaluated author
state, Sequence resolution, persisted title-safe margin, target resolution, and
working space. The background task owns font discovery, shaping, cache, and
failure memory; the Window Adapter only observes typed
Ready/Pending/Unavailable results. Title generation can therefore neither block
the winit thread nor rebuild shell chrome while Preview is pending.
If a later generation requests the same media-preview key while a worker is
already decoding it, that in-flight decode remains current: generation changes
alone must not cancel identical frame/key work, or the viewer can livelock in a
permanent "preparing" state under repeated UI refreshes.
Proxy generation is an `AppState`-owned service, not an action-handler or
Window detail. Import, manual proxy-mode toggles, and preview playback pressure
submit typed origins to the same instance-owned `app::proxy_generation`
Module. The Preview Adapter does not retain its own request set: exact dedupe,
queued priority promotion, retained failure, and project generation belong to
the service. Background polling observes one completion revision and refreshes
models so a newly fresh proxy can replace source fallback; no Widget owns a
worker, retry rule, or FFmpeg process.

Timeline Export follows the same UI boundary but is a separate offline Module.
`AppState::enqueue_timeline_export` resolves one selected Sequence, atomically
captures its reachable nested/media dependency closure, and submits an
immutable `RenderJob` to the instance-owned bounded `RenderQueue`. The queue
outlives neither its App composition owner nor its cancellation authority, but
an admitted snapshot intentionally remains independent of later Project edits
or Project closure. Panels call only the App Adapter for enqueue, cancel,
terminal cleanup, revision polling, and lightweight `ExportJobSnapshot`
observation. They may project structured phases, truthful units, failures, and
color diagnostics; they cannot clone the heavy Timeline payload, mutate status,
invent progress, or infer completion from file existence. Headless execution
uses the same queue/evidence Interface rather than a Window-specific path.

The export panel obtains delivery readiness from
`mondrian_export::resolve_export_delivery`, the same pure Interface enforced by
queue admission. It does not maintain a codec/color compatibility table.
Incompatibility disables action construction and outranks stale success status
text. This UI check is an early projection only; the queue remains authoritative
against the immutable snapshot.

The app export draft stores a stable built-in preset identity plus one
materialized editable `ExportPreset`. Selecting a built-in preset resets that
materialized value; changing container, codec/profile, raster, bit depth,
range, chroma, Alpha, CRF/VBV, GIF palette controls, or audio codec parameters
edits only the materialized draft. Catalog array position is presentation and
never becomes identity. Each form action carries a complete typed preset
snapshot, so no parallel UI-only field bag can disagree with enqueue. A
container edit rewrites the output suffix only when that suffix still matched
the previous container; an explicitly custom suffix is preserved. Admission
freezes the edited preset into the job.

Sequence output color, workflow, missing-metadata policy, tone-map intent,
delivery range/bit-depth defaults, and authored HDR metadata remain
Sequence-owned and editable. The Project engine is edited only through Project
Settings; machine-local display management is edited only through local Viewer
preferences. Only working-space editing is gated by the Project engine
contract. Nested processing is edited on a selected nested Clip placement, not
in Sequence Settings, because it describes that parent-to-child edge.

Media preview frames are held in a bounded LRU cache keyed by asset identity,
media file fingerprint (file length plus modification timestamp), source
frame/time, target preview dimensions, input color interpretation, target
working color space, tone-map policy, and color engine. A media frame decoded
for one working-space contract must never be reused for another viewer/export
color contract, and same-path media/proxy replacements must not reuse stale app
cache entries when the file fingerprint changes. Preview path resolution should
capture source/proxy freshness and the resolved file fingerprint in one
metadata probe path, so playback does not repeatedly stat the same source and
proxy only to build a cache key.
When a cached media frame enters a CPU preview fallback, its source/import ->
working-space transform may be lazily materialized once and reused by clones of
that same media-frame cache entry. Encoded RGBA8 decode retains a
`CpuEncodedColorFrame` for the GPU input-transform path. Scene-linear RGBA-f32
decode instead retains a shared `LinearFloatSource`; the Viewer uploads it
directly to `Rgba32Float` and executes the OCIO input stage on the GPU. Both
variants use the same typed source cache, and only materialize a CPU working
frame when software composition or GPU failure requires it. The lazy CPU
working frame is only a fallback
materialization cache and must not replace the source contract or become a
separate color-interpretation path.
Decode failures are also held in a bounded LRU key cache so repeated bad media
does not grow memory unbounded during playback.
Resolved preview plans may reuse a bounded final-frame cache keyed by sequence,
dimensions, deterministic render-plan signature, and resolved media-frame
identity. The final-frame key also includes the effective preview color context,
including the versioned `OutputTransformIntent`, so monitor/output or rendering
transform changes invalidate previously rendered pixels. Unresolved
media requests still bypass this cache until their source frame is available.
The product window and renderer context use the same renderer-owned wgpu device
feature contract for native NV12/P010 texture formats. Adapter-supported format
features are requested during device creation; P010 additionally requires the
16-bit normalized plane-view feature. Renderer native-import support carries a
typed decoder-device selector through playback-only preview jobs so hybrid-GPU
systems create decoder resources on the renderer's physical adapter. App
diagnostics continue to
report native import as unavailable until the platform resource-sharing,
synchronization, adoption, sampling, and input-transform bridge is connected.
Device feature enablement alone must never promote hardware decode admission.
During playback startup, the Preview Adapter schedules future media payloads
under the active priming deadline and recursively checks the next timeline
frame, including nested sequences. The Host forwards ready/available media
lookahead to the Playback Engine after background completions; it does not
advance the clock itself. Current presentation remains a separate one-shot
ticket, and pause/stop/seek continue to invalidate the Playback Epoch
immediately.
The preview service resolves the requested display color space from the active
display-management policy, but the app window owns real surface/display
validation. The window records the final GPU output boundary only after checking
the current wgpu surface/monitor contract, so an unsupported HDR viewer request
cannot silently reuse the SDR surface path. Resize, scale-factor, and move
events refresh that contract; any change invalidates the external GPU viewer
frame so monitor/output changes cannot reuse a texture produced for the previous
display target.
The display snapshot resolves Mondrian Standard's OCIO display/view from the
effective output color space, not from the config's global default: sRGB,
Rec.709, and Display P3 therefore retain distinct display identities. PQ and
HLG targets retain their target display identities and resolve the versioned
`Mondrian Standard HDR 1000 nits v1` View; the app must not substitute the sRGB
Standard View or an ACES View. Surface/monitor validation remains a later,
independent boundary and may still block presentation when the device cannot
carry the requested HDR signal.
Those snapshot strings are validation evidence only. Preview and asset
thumbnail execution pass the typed `ColorContext::output_transform` to
`RenderOutputColorBoundary::from_intent(...)`; neither app path reconstructs
the OCIO boundary from snapshot or optional context strings.
Window display resolution is computed from both the resolved `ColorEngine` and
display policy, and the session retains both identities. ACES and Custom OCIO
defaults are resolved only after their exact engine
config has loaded, and changing the engine alone refreshes the display contract
and invalidates display-dependent GPU preview state. A failed custom config may
not reuse whichever Standard or ACES config happened to be globally current.
An explicit display selected under Standard still resolves the versioned
Standard View under that display; it never inherits that display's ACES default.
Viewer layout exposes a pixel-aligned `ViewerPresentationGeometry` after the
dirty widget tree has been refreshed. It separates the complete sequence canvas
from its visible intersection and derives a stable
`ViewerExternalTexturePresentation` containing output pixels plus normalized
source crop. The app/renderer may use that contract for working-linear spatial
processing; widgets never own a wgpu resource or choose a reconstruction
filter. Spatial external textures render only when their presentation identity
matches current layout exactly, so dock resize and zoom changes cannot stretch
old display/device code values while a replacement frame is prepared.
External GPU frame identity also includes the resolved monitor adaptation.
The preview model derives that display-referred identity when it looks up a
registered external texture while retaining the unadapted identity for CPU
raster caches. This prevents valid GPU output from being silently replaced by
a raster frame and prevents display-specific textures from crossing monitor
contracts.
The startup/default window contract remains SDR sRGB unless an explicit display
output intent asks for a different presentation contract. The surface resolver
can choose Display P3, Rec.2100 PQ, or Rec.2100 HLG only when wgpu reports the
matching `SurfaceColorSpace` for a compatible format. SDR sRGB and Display P3
use sRGB-encoded surface formats; PQ/HLG require float or 10-bit non-sRGB
formats so the final color pass, not hardware sRGB conversion, owns the output
transfer. Camera-log and Rec.2020 working spaces are not presentation contracts
and must fail closed until mapped through an explicit display/view transform.
The viewer GPU output path records presentation readiness for each requested
display boundary. If the current surface already matches the requested display
space, recording may proceed. If wgpu reports that a better surface contract
exists but Mondrian would still have to pass the result through the UI external
texture compositor, the path must report a payload blocker instead of switching
the surface prematurely. This prevents false HDR/P3 readiness: real promotion
requires the output texture format, external texture sampling contract, UI
compositor shader, and swapchain color space to move together.
SDR viewer textures use an explicit `SrgbSurfaceCodeValuesOpaque` external
texture contract. The source is an unorm texture containing encoded output or
ICC device codes, not a linear UI image. A dedicated UI fragment pass applies
the inverse sRGB carrier curve before writing the sRGB attachment, whose store
conversion restores the original code values. The pass forces opaque output so
the renderer never alpha-blends nonlinear device codes, and clips normalized
SDR code values only at this presentation boundary. Registration fails
closed on non-sRGB surfaces. This carrier operation preserves code values; it
is not an implicit sRGB color-space assumption or a replacement for OCIO/ICC.
Viewer GPU output telemetry reports display-boundary blockers by reason, not
only as a total. It separates HDR-output-on-SDR-surface blockers from
surface-color-space blockers and stores the last blocked output color space,
selected surface color space, HDR mode, and supported surface-color-space
capabilities. This is the diagnostic boundary for real monitor/surface issues:
the model may request HDR, P3, or log output, but the app window must prove that
the current native wgpu surface can actually present it.
The Window owns that evidence collection, not the health policy derived from
it. `app::viewer_gpu_output_health` owns the shared attempt-outcome vocabulary,
health flags, cumulative counts, and pure classifier; production JSONL,
Headless tests, the budget CLI, and performance gates therefore cannot disagree
on what qualifies as `Ready` or silently downgrade a terminal failure.
Residency classification follows the same ownership rule. The Window passes
platform-probe and renderer-execution facts to
`app::viewer_gpu_output_residency`; it does not infer zero-copy from a planned
native input. Planned and executed working residency are distinct serialized
states, and execution-only counters remain zero until the renderer supplies an
actual completion record.
Native-video capability discovery is sampled once while constructing the
renderer/Window Session. `AppUiHost` no longer performs an independent probe:
hardware-decode admission and every residency record consume the same explicit
snapshot. A new probe generation requires Session/device reconstruction, so a
single frame cannot combine admission from one platform observation with
execution evidence from another.
Telemetry must also expose a stable display issue summary that names the reason,
target output color space, current or selected surface contract, desired surface
contract, payload blocker, and whether the target surface color space is
reported as supported. UI, perf JSON, and diagnostics tooling should consume this
summary rather than parsing Debug-formatted blocker/readiness payloads. The
summary must preserve the display-target fingerprint and surface encoding
evidence end to end, so viewer smoke/budget reports can tell whether a failure
happened on the wrong monitor, on the wrong surface contract, or only because
the current payload path cannot yet present that contract. Contract refreshes
caused by resize, scale-factor change, or moving onto another monitor must also
be persisted as structured events with previous/next surface snapshots so
diagnostics can explain how the current contract was reached. When an issue is
recorded after such a refresh, the issue summary should carry the correlated
preceding refresh event instead of forcing downstream tooling to infer that
relationship from separate records. The refresh snapshots should include enough
capability evidence to explain why the contract changed: surface format set,
per-format color-space support, present modes, alpha modes, and HDR headroom
diagnostics. Health reports derived from these summaries should surface refresh
churn, issue-after-refresh correlation, HDR headroom drift, surface-format set
drift, per-format color-space drift, present-mode drift, and alpha-mode drift
as separate root causes instead of one generic capability-drift bucket.
The same rule applies to media interpretation failures: viewer empty-state
diagnostics and export queue job summaries should consume
`VideoColorDiagnosticIssueSummary` / `VideoColorDiagnosticIssueAggregate`
directly and only use the compact human-readable summary as supporting context.
The viewer GPU-output budget evaluator consumes the same JSONL summary and
replays display issue reason counts plus payload-blocker counts, so smoke tests
can budget real display/surface regressions independently from broad health
status totals. That budget must stay fail-closed per reason as well as in
aggregate, so HDR-surface regressions, surface-color-space mismatches, payload
contract blockers, unsupported presentation intents, and unsupported surface
contracts can each trip their own threshold instead of disappearing inside one
combined display-issue count. Unknown future reason strings must also budget as
their own fail-closed class so diagnostics schema drift cannot hide inside a
temporarily relaxed aggregate threshold. The same JSONL records should also
preserve renderer-owned structured stage evidence (`RenderGpuOutputStageDiagnosticsReport`)
next to any temporary app-local flattened counters, so viewer tooling can
consume one renderer schema rather than rebuilding stage-breakdown models.
Renderer runtime evidence (`RenderGpuOutputRuntimeDiagnosticsReport`) should
travel with the same records so viewer budgets and triage can surface shader
cache extraction or backend-object preparation failures without inventing a
parallel app-local runtime taxonomy.
Playback requests may enqueue a small forward prefetch window, but prefetching is
best-effort: it must not rebuild UI state, block the current frame, or bypass the
generation checks that protect continuous playback from stale decode work.
Current-frame media requests are scheduled before forward prefetch, and the
worker queue/pending set are bounded. When playback outruns decode, obsolete or
excess preview jobs are dropped instead of back-pressuring the UI thread.
If the complete steady window is blank, the scheduler may prewarm only the
nearest visible media Clip activation inside its bounded cold-start horizon.
That activation consumes the existing prefetch capacity and generation; it
does not increase the retained steady-frame count or scan an unbounded gap.
The host supplies the Playback Engine's still-pending demand identity both when
it polls completed work and when it expires realtime work. Polling and
expiration always release scheduler capacity, but only an exact pending
identity may refresh visible playback state, create late-pressure evidence, or
become one `Late` delivery; work left behind after a GPU presentation completed
cannot publish another terminal outcome.
The UI thread must also consume completed background preview results with a
small per-poll budget. Large bursts of completed decode jobs are spread across
event-loop turns so pointer/keyboard/window events keep priority over cache and
diagnostic bookkeeping. Project close cancels queued and in-flight preview work,
clears preview caches/failure caches, and leaves workers alive for the next
project. Application quit additionally closes the preview worker queue and must
not perform a workspace-to-startup native-window role sync on the way out.
`PreviewProductionRuntime::diagnostics()` exposes an immutable observation snapshot;
the sibling `app::preview_runtime::diagnostics` Module exclusively owns its typed
decode/render/color evidence models and fail-closed report construction. The
realtime preview implementation records facts but does not contain acceptance
thresholds, root-cause classification, or remediation text. This keeps the
same report Interface available to UI presentation and Headless performance
gates without giving either caller authority over execution. The snapshot
contains render, cache, queue, decode, and scheduler counters so performance tooling can distinguish cache misses,
backpressure drops, stale completions, decode failures, and GPU preview
candidate readiness without changing timeline evaluation. Scrub-adaptive
request counters expose whether interactive seeks are using normal, hot-region,
slow-latency, or recovery policy. Each service call produces
`preview_candidate_id`, and the same id is propagated into `PreviewGpuFrame`
when the frame is ready. Window-level telemetry records this candidate id and
state alongside structured runtime/stage evidence so a JSONL record can be
linked against the exact working-space attempt that fed it.
Window-side Viewer replacement is a commit operation: a previous external GPU
texture remains registered until the replacement output has recorded,
registered, and been accepted by preview state. Typed renderer backpressure
retains that output without blocking the UI thread; subsequent event-loop turns
may retry the current candidate, while superseded candidates are discarded by
normal preview identity rules. This prevents transient native bridge or GPU
queue pressure from producing a blank Viewer.
The private `app::preview_runtime::presentation` Module is the sole application
arbitrator for registered outputs, raster-cache hits, scoped stale
content, deferred playback composites, CPU output transformation, packaging,
and pinning. The production Runtime coordinates scheduling and diagnostics;
`app::preview_execution` remains the sole generation/candidate/registered-output
lifecycle owner. This Locality prevents UI redraw code from reconstructing a
second output-selection policy. Raster cache and stale pinning retain the
UI-independent `PreviewRasterFrame`; the shallow `app_ui::preview` Window
Adapter performs the only conversion to `ViewerFrameImage` and shares the
existing pixel allocation.
CPU color/composite execution itself is not Window-owned:
`app::preview_cpu_execution` returns the final raster, complete execution facts,
and stage durations. Presentation records those facts into Window diagnostics
before packaging; a Headless Adapter consumes the same Module directly.
The same rule applies while a pause, seek, or exact-still request replaces the
current frame: `Stale` prefers the last presented external GPU frame for the
same sequence and output extent, then falls back to the pinned CPU raster. A
pending replacement must never demote an already visible GPU frame to an empty
or gray Viewer. Display-contract, sequence, and geometry changes still clear
the external frame explicitly, so stale reuse cannot cross presentation
semantics.
Preview diagnostics keep decode-stage timings separate from post-decode viewer
render timings. Decode reports classify session open, cache lookup, seek,
packet/decode, software scale, RGBA copy, and external-process wait cost;
render reports classify sequence resolution, final-frame cache lookup,
working-frame preparation, CPU timeline composition, CPU output/color boundary,
and final raster packaging. Perf tooling should use both reports before
assigning a slow frame to codec, cache, color, composite, or viewer packaging
work. Final raster viewer keys must be derived from the resolved render-plan
identity, not by hashing full RGBA payloads; large preview frames should not pay
an extra O(width * height) CPU scan just to name an atlas entry.
Asset thumbnail raster keys follow the same identity rule without weakening
color correctness: they hash the resolved source, working, output, display/view,
tone-map, engine, and OCIO-generation contract alongside asset path and file
fingerprint. The app-owned worker performs color transforms before
`RasterImage` construction and only the latest active request for an asset may
publish a completion. Color-context changes clear visible cache state and
invalidate request ownership so stale asynchronous results cannot overwrite a
new display contract.

UI raster images are typed presentation payloads. `RasterImage`,
`DrawCommandEncoder::draw_raster_image`, and `DrawCommand::RasterImage` carry
`RasterImageColorSpace` end to end. The current renderer-owned image atlas is
`Rgba8UnormSrgb`, so it accepts only explicitly sRGB bytes. Unsupported spaces
fail visibly and are reported through `unsupported_raster_color_spaces`, which
the app promotes into frame diagnostics and resource-failure logs. Adding a P3
atlas later requires a separate compatible texture/pipeline path; it must not
silently reinterpret P3 bytes through the sRGB atlas.
The application-layer `PreviewRasterPresentationContract` resolves the CPU
boundary before any Widget object exists: Rec.709, sRGB, and Display P3 SDR
requests are monitor-adapted to an sRGB atlas payload; PQ/HLG requests fail
closed until an explicit SDR tone-mapping policy exists. The Window Adapter then
maps the proven encoding to `RasterImageColorSpace::Srgb`; the widget layer never
tone-maps or relabels bytes. GPU Viewer candidates retain the original
monitor/output contract and continue through the native output/surface
validation path.

GPU preview candidate counters are intentionally scoped to the headless service
boundary: they prove that a working-space frame was produced for the app-window
GPU output path, not that wgpu presentation recording succeeded. The app UI scale
smoke test
serializes a preview diagnostics probe into its JSON report and includes a
separate preview-playback refresh case without changing the existing UI-only
refresh benchmark paths.
`preview_media_decode_cache_smoke` extends this coverage with a generated
FFmpeg fixture and exercises real media import, decode readiness, cache-hit
refreshes, sequential-frame preview readiness, and a GPU preview candidate probe
as an ignored/manual perf probe.
`preview_media_continuous_playback_smoke` uses the same generated media path to
simulate a 30fps playback window and records `Ready`/`Loading`/`Stale`/
`Unavailable` counts plus a GPU preview candidate probe, with the contract that
steady playback keeps a current or stale frame visible instead of falling
through to an unavailable viewer.
The external-media variant treats current-frame readiness as a basis-point
contract (99.50% by default), requires complete hardware timestamp coverage for
every newly rendered frame, and reports hardware GPU, CPU record/submit,
completion-wait, and total wall p95 independently. The headless adapter's
16-slot timestamp ring submits and maps queries without a per-frame wait; the
offline gate drains once after playback. Ring saturation discards telemetry and
fails timestamp coverage instead of back-pressuring the measured scheduler.
Production window and headless Viewer device creation also request optional
32-bit-float filtering when supported, so OCIO LUT interpolation uses the same
wgpu feature contract in interactive presentation and real-media gates.
The same report attributes hardware duration across working composite, spatial,
output-boundary, and optional display-calibration stages using ordered encoder
timestamps. This attribution is distinct from CPU stage preparation timings and
lets the gate localize a GPU regression without inserting a per-stage queue
submission or CPU/GPU synchronization point.
Continuous-playback decode failure extraction is scoped to PlaybackCursor plus
global fatal scheduler/worker failures. Random-access still and scrub latency
remain visible in the full diagnostic report but cannot fail a playback-only
gate; their dedicated probes own those budgets.

Viewer models consume an explicit preview readiness state. `Ready` frames are
current, `Loading` means the requested frame is queued/in flight, and `Stale`
means the viewer may keep the last ready frame visible while the current frame is
prepared or a failed media key is protected by the bounded failure cache. Stale
frame reuse is scoped to the same sequence and preview dimensions. These states
are presentation/adaptor semantics only; they must not mutate timeline playback
state or affect export evaluation.
`Transparent` is also exact and Ready: it means the active Sequence produced a
valid transparent Program canvas without a texture. The Viewer paints that
canvas using the persisted presentation-only checkerboard/black preference,
shows no diagnostic empty-state text, and never writes the chosen background
into Program pixels. `Unavailable(NoContent)` is reserved for the absence of an
active output target; `Blocked` and `Failed` remain warning/error states.
When the preview service returns `Unavailable` because color management rejected
media, the viewer model must consume `ViewerPreviewColorRejectionModel` instead
of showing a generic empty viewer. The status should remain warning-toned and
the empty message should include the rejected media path, missing-metadata
policy, input-resolution branch, and media diagnostic summary. This keeps
fail-closed color behavior visible without scraping tracing logs.
The native app entrypoint owns a four-thread Tokio runtime for background UI
work. After the event loop exits, the runtime is shut down with a bounded
timeout rather than dropped normally: Tokio's default runtime drop can wait
indefinitely for blocking tasks and leave a headless Mondrian process after the
window has closed. Background operations must therefore treat cancellation as
cooperative and may not rely on an unbounded runtime drain during process exit.
Once action draining produces a quit command, the host returns it immediately;
it must not refresh or lay out the widget tree after the preview service and
project state have already begun shutdown.
When the host begins a confirmed quit (after any unsaved-work decision), a
two-second process-exit watchdog gives preview, project, runtime, and GPU
resource destruction a final bounded opportunity to finish. A quit command is
not followed by another native redraw or window-role synchronization, avoiding
a redraw/destruction race on the exiting window. If a platform media, audio, or
GPU driver blocks closure destruction, the watchdog uses the platform's
no-destructor termination primitive (`TerminateProcess` on Windows, `_exit` on
Unix) if application-level cleanup does not return in time;
`std::process::exit` is deliberately not used because DLL detach hooks can
deadlock on locks held by terminating worker threads. The watchdog is armed
only after the guarded unsaved-work decision and immediately before bounded
preview/project cleanup begins, so it cannot bypass save/discard/cancel
semantics but still bounds a cleanup call blocked in a third-party runtime.
