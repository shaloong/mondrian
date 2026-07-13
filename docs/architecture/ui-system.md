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

## Focus and Accessibility

Focus ownership is not the same as visible focus indication. `FocusSource::Keyboard` may show a focus ring; pointer/programmatic focus owns keyboard input but normally does not show the ring.

Accessibility `focused` must reflect real focus ownership, not `focus_visible`.

## Overlay and Menus

Dropdowns, context menus, popovers, and tooltips should render through overlay paint/hit-test so they are not clipped or hidden behind sibling panels. Menubar menus and context menus should share menu primitives.

## Commands

Menus, shortcut preferences, command palette, and future plugins should consume `app_ui::commands` descriptors. Menus are command presentation, not business logic owners.

Opening an editor dialog is a shell action because it mutates transient UI state,
not the undoable domain model. The dialog's committed payload must flow through a
domain/app action owned by the target subsystem. For example, Asset Library →
Interpret Footage opens an app-shell modal, but applying Auto/Override is
an asset-library mutation that persists `AssetMediaInterpretation`.
The modal is a compact settings form with a single input color-space dropdown:
Auto is the default option, explicit color spaces persist as overrides, and Auto
displays the current resolved/detected result instead of explanatory copy.
Non-color data is an asset payload classification for advanced utility-channel
workflows, not an option in the primary color-space picker. A future payload or
channel-role control may edit that classification, but the Interpret Footage
color-space dropdown must preserve the existing payload value while changing
only `MediaColorInterpretation`; it must never create or clear
`AssetColorPayload::NonColorData`.

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

Playback-frame refreshes use a narrow UI update path: the host advances
`AppState`, then refreshes viewer playback chrome/frame data and the timeline
playhead without rebuilding the full dock tree.

`AppUiPreviewService` owns media preview scheduling. Each viewer preview request
starts a monotonic generation, and background media jobs check that their key is
still requested by the latest generation before decoding. Completed stale jobs
may warm the cache, but they do not force a UI refresh for an older playback
frame.
If a later generation requests the same media-preview key while a worker is
already decoding it, that in-flight decode remains current: generation changes
alone must not cancel identical frame/key work, or the viewer can livelock in a
permanent "preparing" state under repeated UI refreshes.
Proxy generation is an app service, not an action-handler detail. Import,
manual proxy-mode toggles, and preview playback pressure enqueue work through
the same `app::proxy_generation` dispatcher, while the preview service only
requests generation for already proxy-enabled video assets whose proxy path is
missing or stale. The preview service records request and dedupe counters so
deadline-driven proxy work is diagnosable without coupling viewer scheduling to
the asset panel UI.

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
that same media-frame cache entry. The decoded source `CpuEncodedColorFrame`
remains available for the GPU input-transform path; the lazy CPU working frame
is only a fallback materialization cache and must not replace the source
contract or become a separate color-interpretation path.
Decode failures are also held in a bounded LRU key cache so repeated bad media
does not grow memory unbounded during playback.
Resolved preview plans may reuse a bounded final-frame cache keyed by sequence,
dimensions, deterministic render-plan signature, and resolved media-frame
identity. The final-frame key also includes the effective preview color context,
so monitor/output changes invalidate previously rendered pixels. Unresolved
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
Viewer layout exposes a pixel-aligned `ViewerPresentationGeometry` after the
dirty widget tree has been refreshed. It separates the complete sequence canvas
from its visible intersection and derives a stable
`ViewerExternalTexturePresentation` containing output pixels plus normalized
source crop. The app/renderer may use that contract for working-linear spatial
processing; widgets never own a wgpu resource or choose a reconstruction
filter. Spatial external textures render only when their presentation identity
matches current layout exactly, so dock resize and zoom changes cannot stretch
old display/device code values while a replacement frame is prepared.
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
The UI thread must also consume completed background preview results with a
small per-poll budget. Large bursts of completed decode jobs are spread across
event-loop turns so pointer/keyboard/window events keep priority over cache and
diagnostic bookkeeping. Project close cancels queued and in-flight preview work,
clears preview caches/failure caches, and leaves workers alive for the next
project. Application quit additionally closes the preview worker queue and must
not perform a workspace-to-startup native-window role sync on the way out.
`AppUiPreviewService::diagnostics()` exposes render, cache, queue, decode, and
scheduler counters so performance tooling can distinguish cache misses,
backpressure drops, stale completions, decode failures, and GPU preview
candidate readiness without changing timeline evaluation. Scrub-adaptive
request counters expose whether interactive seeks are using normal, hot-region,
slow-latency, or recovery policy. Each service call produces
`preview_candidate_id`, and the same id is propagated into `AppUiGpuPreviewFrame`
when the frame is ready. Window-level telemetry records this candidate id and
state alongside structured runtime/stage evidence so a JSONL record can be
linked against the exact working-space attempt that fed it.
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
The CPU Viewer adapter owns a `CpuRasterPresentationContract`: Rec.709 and sRGB
SDR requests are output-transformed to an sRGB atlas payload and labeled
`RasterImageColorSpace::Srgb`; P3/PQ/HLG requests fail closed instead of being
tone-mapped or relabeled by the widget layer. GPU viewer candidates retain the
original monitor/output contract and continue through the native output/surface
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
short process-exit watchdog gives preview, project, runtime, and GPU resource
destruction a final bounded opportunity to finish.
If a platform driver blocks closure destruction, the watchdog terminates the
already-cleaned process instead of leaving a ghost or unresponsive window.
