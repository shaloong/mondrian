# UI System

Mondrian's UI is self-hosted: winit/platform integration, retained widgets, wgpu rendering, theme tokens, event routing, dock/layout, and app panel adapters.

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

Media preview frames are held in a bounded LRU cache keyed by asset identity,
file modification stamp, source frame/time, target preview dimensions, input
color interpretation, target working color space, tone-map policy, and color
engine. A media frame decoded for one working-space contract must never be
reused for another viewer/export color contract.
Decode failures are also held in a bounded LRU key cache so repeated bad media
does not grow memory unbounded during playback.
Resolved preview plans may reuse a bounded final-frame cache keyed by sequence,
dimensions, deterministic render-plan signature, and resolved media-frame
identity. The final-frame key also includes the effective preview color context,
so monitor/output changes invalidate previously rendered pixels. Unresolved
media requests still bypass this cache until their source frame is available.
The preview service resolves the requested display color space from the active
display-management policy, but the app window owns real surface/display
validation. The window records the final GPU output boundary only after checking
the current wgpu surface/monitor contract, so an unsupported HDR viewer request
cannot silently reuse the SDR surface path. Resize, scale-factor, and move
events refresh that contract; any change invalidates the external GPU viewer
frame so monitor/output changes cannot reuse a texture produced for the previous
display target.
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
diagnostics.
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
temporarily relaxed aggregate threshold.
Playback requests may enqueue a small forward prefetch window, but prefetching is
best-effort: it must not rebuild UI state, block the current frame, or bypass the
generation checks that protect continuous playback from stale decode work.
Current-frame media requests are scheduled before forward prefetch, and the
worker queue/pending set are bounded. When playback outruns decode, obsolete or
excess preview jobs are dropped instead of back-pressuring the UI thread.
`AppUiPreviewService::diagnostics()` exposes render, cache, queue, decode, and
scheduler counters so performance tooling can distinguish cache misses,
backpressure drops, stale completions, decode failures, and GPU preview
candidate readiness without changing timeline evaluation. GPU preview candidate
counters are intentionally scoped to the headless service boundary: they prove
that a working-space frame was produced for the app-window GPU output path, not
that wgpu presentation recording succeeded. The app UI scale smoke test
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
