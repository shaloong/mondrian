# Mondrian Self-Hosted UI System

> Status: Input and event kernel in progress.

## Layers

```text
winit
  -> mondrian-platform
  -> mondrian-ui-events
  -> mondrian-ui-core Widget
  -> mondrian-ui-renderer DrawCommand
  -> wgpu
```

`mondrian-ui-core` owns platform-neutral primitives: geometry, events,
widgets, focus traits, shortcut traits, tooltip traits, and the event request
surface. It does not depend on winit, wgpu, arboard, or OS APIs.
Shared geometry helpers such as `Rect::intersection` live here so clipping
semantics are consistent across widgets, hit testing, and renderer-facing
command generation instead of being reimplemented per component.

`mondrian-ui-events` owns routing: hit testing, focused keyboard dispatch, and
pointer capture. Widgets record side-effect intent in `EventRequests`; the
router or app shell applies those requests.

UI theme colors are authored as design-tool sRGB hex values, while the wgpu
fragment pipeline writes linear RGB into the presentation target. The UI
renderer therefore converts shape colors from sRGB tokens to linear RGB when
building GPU vertices, uses sRGB texture sampling for full-color raster atlas
images such as thumbnails, and leaves R8 glyph/vector alpha masks in linear
alpha space. Widgets and theme tokens should not pre-darken colors to compensate
for display output, nor should they decode or encode the same color twice; the
renderer boundary owns this conversion so dark surfaces such as title bars and
timeline tracks match their authored hex values on screen.

`mondrian-platform` owns OS integration. Text widgets use `PlatformService`
for clipboard operations instead of calling platform APIs directly.
`SystemPlatformService` currently provides desktop clipboard copy/paste through
`arboard`. `DesktopEyedropper` owns desktop-coordinate screen sampling and
best-effort global pointer polling for color picking. `mondrian-app` centralizes
the winit adapter in `self_hosted::runtime`: it drains router side-effect requests,
translates between window-local and desktop coordinates, paints the shell-owned
eyedropper overlay, resolves shell cursor priority, and feeds sampled colors
back into the widget tree.

## Application Entrypoints and App Modules

`mondrian-app/src/main.rs` is the product entrypoint and launches the
self-hosted winit/wgpu editor shell through `self_hosted::window`.
The window runner is responsible for process-level UI bootstrap only: it
installs tracing, honors `RUST_LOG`
through `EnvFilter`, enters the shared Tokio background runtime used by app
actions, and then owns the native winit event loop.
`mondrian-app/src/bin` is reserved for developer-only binaries: widget
galleries, pipeline smoke tests, and visual diagnostics. Binaries in `src/bin`
must stay thin; reusable runtime, panel, or mapping logic belongs in library
modules. The product shell should not be mirrored by a second launcher binary.

The legacy egui UI lives under `mondrian-app/src/egui_ui` and remains migration
reference code only. That module contains egui panels, egui theme tokens,
viewer helpers, and the old timeline implementation. New self-hosted UI code
must not be added there unless it is deliberately deleting or extracting legacy
behavior. It is crate-private and must not be exposed as public API, product
binary code, or plugin-extension guidance while it remains reference code.
The legacy egui shortcut preference module at `mondrian-app/src/shortcuts.rs`
is also crate-private; new shortcut routing, labels, persistence, and
Preferences UI must use `self_hosted::shortcuts` and
`SelfHostedPreferences.shortcut_overrides`.
Media-cache filesystem policy is not a UI concern: `mondrian-app::app::media_cache`
owns cache usage statistics, size/age cleanup, and clear-directory behavior.
Legacy viewer panels may call it before clearing their GPU textures, decoder
pools, and prefetch state, but app preferences and future self-hosted
preferences must not depend on `egui_ui::viewer_panel` for cache maintenance.
The Phase 5 parity audit lives in
`docs/architecture/self-hosted-ui-parity.md`. It is the source of truth for
which legacy egui workflows are product-covered by self-hosted UI, which panels
are intentionally retired, and which remaining egui references must be removed
or quarantined before the compatibility path can disappear.

The self-hosted UI application adapter lives under
`mondrian-app/src/self_hosted`. `self_hosted::runtime` owns winit-side request
application for reusable widgets. `self_hosted::rendering` owns the shared
wgpu frame submission path for self-hosted windows, including cosmic-text glyph
uploads, surface texture acquisition, present, and surface reconfigure on
loss/outdating. `self_hosted::menu_bar` owns the product menu model, shortcut
hints, and top menu widget. `self_hosted::action_availability` owns the shared
app-state gate used by menus, focused shortcuts, host dispatch, and panel
adapters. `self_hosted::shell` owns reusable root-widget composition such as the
menu bar, dock tree, and modal layer; developer binaries should use
`SelfHostedAppRoot` rather than defining shell widgets inline.
`self_hosted::workspace_layout` owns the persistable dock-tree schema for the
Custom workspace and converts live dock widgets into a sanitized shell layout
snapshot; reusable widgets expose state but do not serialize user preferences.
Dock panel tab dragging is split across the same boundary: `mondrian-ui-core`
uses `DragPayload::PanelTab` for platform-neutral drag routing,
`mondrian-ui-widgets::DockTabBar` gives tab-strip insertion/reordering priority
and emits exact tab insertion indices, `mondrian-ui-widgets::DockPanel`
handles only content-area dock-guide hover/drop, and `self_hosted::shell`
resolves `app.shell/relocate_panel` into `SelfHostedWorkspaceLayout`
operations. Tab-bar drops use `relocate_panel_to_tab_index`; content guide
center/edge drops use `relocate_panel`. Dropping on a tab bar creates browser-like
tab insertion without showing the dock guide; dropping on the content guide
uses one shared full-panel five-zone geometry (center rectangle plus four
trapezoid edge regions) for both hover hit-testing and overlay painting, so the
visual affordance matches the actual drop target across the whole content area.
Guide-edge drops split around the target group, including dragging the active
tab onto its own panel content edge when that group still has another visible
tab to anchor the remaining leaf. `DockTabBar`, `DockPanel`, and
`DockSplitter` should keep one shared interaction language: low-noise resting
chrome, explicit active/hover emphasis, and no duplicate drag feedback layers
that would make dense editing layouts feel visually busy. The app shell owns persistence by promoting the workspace to Custom and
saving the resulting layout through `SelfHostedUiHost`, so reusable widget
crates remain free of preference I/O.
`self_hosted::startup` owns the launch-time root surface
shown before any project is open; it emits only app-shell lifecycle actions and
does not own project creation, loading, recent-file persistence, or editor
state. `self_hosted::icons` owns the app-layer registry for bundled designer SVG
icon assets and converts them into
`mondrian-ui-widgets::VectorIcon` / `IconButton` values without depending on
legacy egui theme types. `self_hosted::panels` owns panel adapters that map
application-facing concepts into generic widget view models.
Common editor operations should not be keyboard-only. When a self-hosted global
shortcut is added for a visible NLE operation such as duplicate, delete, ripple
delete, split, mark in/out, transport stepping, select all, or deselect all, the
corresponding menu row should use the same action, shortcut descriptor table, and
`self_hosted::action_availability::app_state_action_enabled` gate.
`self_hosted::host::SelfHostedUiHost` owns the reusable product
state bridge: it keeps the startup root, workspace root, current `AppState`,
dirty refresh flag, visible shell mode, and queued-action draining together so
window entrypoints do not duplicate root/AppState refresh plumbing.
`self_hosted::window` owns the reusable winit/wgpu product-window runner used
by the `mondrian` binary. The boundary type is `SelfHostedPanelModels`: real
`AppState` / `EditorState` adapters should produce this model, while
`SelfHostedPanelModels::demo()` is test-only fixture code and must not be part
of product entrypoints.
`SelfHostedPanelModels::from_app_state` is the app-side snapshot boundary: it
reads the current `AppState`, asset library, effect registry, selection state,
and timeline sequence into generic widget models. Timeline adapters start at
`TimelinePanelModel::from_sequence`, which maps `mondrian-timeline::Sequence`
plus app-layer selection DTOs into widget view models and stable-id-backed
actions. The official `mondrian` entrypoint starts from a real empty
`AppState`, builds `SelfHostedPanelModels::from_app_state`, and refreshes from
that same boundary after dispatched actions. Component fixtures remain in
`ui_demo` and explicit `SelfHostedPanelModels::demo()` tests only.
The winit runner should request redraws from explicit state transitions: widget
or router repaint requests, surface resize/reconfigure, background preview or
thumbnail refreshes, startup/workspace window replacement, and render follow-up
work such as resource uploads. Plain `CursorMoved` events must not force a full
UI redraw by themselves; widgets that change hover, drag previews, tooltips, or
cursor-dependent overlays are responsible for emitting `ctx.request_repaint()`.
The runner also avoids repeating native cursor-icon updates when the resolved
icon has not changed, keeping OS-level window dragging and splitter hover
feedback from competing with redundant shell work.

The self-hosted entrypoint uses distinct native window roles for startup and
workspace. Startup owns a fixed transparent undecorated winit window. Once
create/open resolves to a concrete editor action and `AppState` owns an open
project, the host switches the active root to the workspace and the winit
adapter replaces the native session: it hides/drops the startup window, creates
a resizable undecorated workspace window, builds a fresh surface/router/runtime
for that root, updates the UI bounds, and relayouts before the first workspace
frame. The workspace keeps one product chrome row owned by `TitleBar`; it must
not combine native OS decorations with the self-hosted title/menu bar. Closing
the project follows the same boundary in reverse rather than mutating
creation-time window attributes in place. Workspace windows request platform
rounded corners where the operating system exposes a native top-level window
corner preference; this keeps the whole editor shell visually rounded without
reintroducing system title bars or clipping reusable widget content in the
renderer.
Recent-project recovery, crash recovery, and future onboarding belong in the
startup model and should be surfaced through app-layer actions rather than
reintroducing a separate project browser or console panel.
The startup surface may use app-owned embedded raster resources, such as the
bootstrap banner, decoded once inside `self_hosted::startup` and sent through
the renderer raster-image path. The left launch half is intentionally pure
raster: do not layer app icons, product names, marketing copy, badges, or
translucent overlays over it. If that side needs text, it belongs in the bitmap
asset. Those resources are product shell assets, not reusable widget-crate
dependencies; generic widgets should continue receiving already-decoded image
view models when they need raster content.
The workspace panel set follows NLE product surfaces only: Viewer, Timeline,
Assets, Inspector, Effects, Node Graph, and Export. Project media browsing lives
inside Assets as the project library; self-hosted menus and dock factories must
not grow separate Project-browser or Console panels.
The self-hosted recent-project list is persisted in
`self_hosted::preferences_store::SelfHostedPreferences`: it records only shell
launch history, is filtered to existing files when loaded, and is rendered by
`self_hosted::startup` through explicit view models. Startup rows emit
`app.shell/open_recent_project`, which the shell resolves to `Action::OpenProject`;
widgets must not read the filesystem or mutate editor state directly.
Autosave recovery follows the same boundary. `SelfHostedUiHost` discovers
crate-local `CrashRecoveryCandidate` values, maps them into startup view models,
and the startup surface emits `app.shell/recover_project`. The shell resolves
that request into `ui.project/recover_from_autosave`; only `AppState` opens the
autosave snapshot, writes the recovered project, and clears recovery files.
Startup also hosts shell-local modals when no project is open. Its New Project
entry opens the shared `NewProjectDialog` through `self_hosted::modal`, applies
draft updates to `SelfHostedNewProjectDraft`, and produces a concrete
`ui.project/create_with_settings` action only after the user confirms and the
platform save dialog returns a path. Startup must not use a hidden direct-create
shortcut with default project settings.

Product top chrome is `self_hosted::title_bar::TitleBar`: it combines the
product favicon, product menu bar, a read-only project/sequence title,
draggable titlebar space, and platform-aware custom window controls in one row.
The left brand affordance is icon-only; do not duplicate the product name as
text in the titlebar. The favicon is a product-shell asset: the self-hosted
titlebar rasterizes `mondrian-app/assets/favicon.svg` through the app-layer
raster asset helper, while Windows executable metadata embeds
`mondrian-app/assets/favicon.ico` directly from the app build script. Do not
reintroduce PNG-to-ICO generation in the build pipeline or move branded assets
into reusable widget crates. Native OS
titlebar buttons are not embedded directly: winit does not expose a portable way
to keep the self-hosted title/menu row while borrowing only the operating
system's minimize, maximize, and close buttons. `self_hosted::window_controls`
owns the client-side control order, edge, hit targets, hover treatment, and
glyphs for Windows, macOS, and Linux styles; `TitleBar` only consumes its layout
and event surface. Windows close hover/press colors are semantic theme tokens so
the self-drawn chrome can follow native red affordances without hardcoding
palette values in the app shell. Window controls emit app-shell custom actions only;
`SelfHostedUiHost` converts them into `SelfHostedShellCommands` and
`self_hosted::window` applies native minimize, maximize, drag, fullscreen, or
quit side effects after widget and `AppState` borrows end. These commands must
not be added to the editor-state core action enum unless they mutate portable
editor data. Native `CloseRequested` events use the same app-shell quit action
path as client-side close buttons, menu rows, and shortcut commands so all
window-exit policy has a single shell boundary.
The title text is centered inside the remaining blank titlebar span between the
menu group and platform window controls, not against a fixed window midpoint or
hard-coded offset. It elides inside that span when the project name is too long
so menu triggers and window controls keep their native-feeling hit targets.
Top menu triggers use the lightweight `DropdownTriggerStyle::MenuBar` treatment
and content-width layout: closed triggers should read like native menu text, not
filled toolbar buttons, and should not reserve a persistent arrow affordance.
Top-level product menu rows are text-first shell commands with optional shortcut
hints only; they should not pass icon assets into `MenuItem`, even when the
underlying generic dropdown component can still render icon lanes for embedded
selectors and context menus.
The product menu bar has exactly five stable top-level groups, displayed in the
Chinese-first shell as 文件, 编辑, 视图, 窗口, and 帮助. Their stable product
roles remain File, Edit, View, Window, and Help for action naming and tests. Do
not add top-level buckets for workflow-specific domains such as Playback,
Sequence, Workspace, Assets, Effects, or Timeline.
Route commands to the narrowest useful surface instead: timeline or asset
context menus, focused keyboard shortcuts, panel toolbars, inspector controls,
sequence selectors, or existing rows inside the five groups. `View` owns view
presentation controls such as fullscreen. `Window` owns dock panel visibility
and workspace presets. Adding another top-level menu requires an
architecture update because the product chrome is intentionally kept compact.
Unused menu-bar allocation remains draggable titlebar space; `TitleBar` should
exclude only actual menu trigger hit rects and window-control rects from native
drag initiation.

Shell-local modals, such as New Project, Preferences, and About, live in their
own `self_hosted::*_dialog` modules and are hosted by `self_hosted::modal`.
They emit stable `app.shell` custom actions defined in `app::ui_actions`.
Preferences reads a `SelfHostedPreferencesModel` snapshot produced from the
`AppState`, current workspace preset, and typed `SelfHostedPreferences` store.
`self_hosted::host::SelfHostedUiHost` owns loading, applying, and persisting
that store; dialogs only render models and emit `app.shell` actions. The theme
preset is the first migrated persisted preference and is applied through
`mondrian-ui-theme` before root painting. Editable or persisted preferences
must migrate through typed app preference DTOs as those settings are exposed to
the self-hosted shell; the preferences surface must not read or duplicate
legacy egui-only dialog state, perform disk I/O, or show static placeholder
values for state the self-hosted shell does not actually own.

`mondrian-editor-ui` owns the long-lived editor panel contract. Panel instances
are created with `PanelInitContext`, which is limited to stable services such as
the shared `EventBus`. Widget-tree rebuilds receive `PanelBuildContext`, which
contains the current read-only `EditorState` snapshot plus a semantic
`Action` dispatch sink. Panels must not retain mutable app state or bypass that
dispatch path; app-specific adapters may map richer `AppState` data into
panel/view models before constructing widgets, but the reusable panel boundary
stays `EditorState` + `Action`.

Selection DTOs that describe editor state, such as `SelectedClipRef`, live in
`mondrian-app::app` rather than legacy UI modules. Legacy egui panels and
self-hosted adapters may both depend on these app-layer DTOs, but app/domain
state must not depend on widget modules.

UI actions that need stable application ids use `mondrian-app::app::ui_actions`.
Self-hosted panel adapters translate domain-light widget events, such as
timeline clip indices or inspector value changes, into `Action::Custom` payloads
carrying track and clip ids. `AppState::dispatch_action` consumes that app-layer
protocol and calls existing undoable command/property-mutation paths. Timeline
move/trim/seek actions resolve through timeline command methods; Inspector clip
enabled, opacity, solid/tint color, and basic transform field actions resolve
through `AppState` snapshot commands and `mondrian-timeline` property hosts.
Generic selection actions update the app-level selection snapshot only:
`Select(Clip)` resolves the active sequence track from the clip id,
`SelectAll`/`Select(AllClips)` select all clips that AppState can currently
represent, `Select(AllTracks)` selects visible timeline tracks, and
`DeselectAll` clears track, clip, mask, and animation selection without entering
undo history.
The shared `Action::DeleteSelection` path deletes clips or tracks selected
through the AppState selection module. `Action::RippleDeleteSelection` is a
separate timeline-editing action that uses the same clip deletion path with
ripple enabled; track deletion remains a normal bulk track mutation. Clip
deletes use `remove_clips_bulk`; track deletes use one prevalidated bulk track
mutation, so shortcuts, menus, scripts, and self-hosted widgets all share one
action boundary. Clip deletion keeps the existing locked-track checks and
linked-clip cleanup; both clip and track deletion produce undo snapshots and
timeline modified events.
`Action::ImportMedia` is the shared boundary for platform file pickers, menus,
scripts, and future self-hosted asset browser commands. The app layer batches
the supplied paths through `AssetLibrary::import_media_file`, publishes
`AssetImported`, updates proxy-mode state when auto proxy is enabled, saves the
project opportunistically, and reports partial or complete failures through the
status hint instead of letting widget code own import side effects.
Native file dialogs belong to `mondrian-platform::PlatformService`; the
self-hosted File menu emits an app-shell custom action, resolves that dialog at
the window entrypoint, then dispatches `Action::ImportMedia` with concrete
paths.
Project create/open/save dialogs use the same boundary: menu widgets emit
app-shell dialog intents, the window entrypoint resolves them into concrete
`ui.project.create_with_settings` / `OpenProject(PathBuf)` /
`SaveProjectAs(PathBuf)` actions, and `AppState` performs project lifecycle
work plus status reporting. Widget code must not invent project paths or mutate
project files directly.
Export follows the same rule. UI frontends may collect a preset, sequence id,
timeline range, and output path, then dispatch `ui.export.enqueue`; `AppState`
owns timeline export request validation, recursive asset path collection,
offline-asset checks, and `RenderJob` creation through
`app::exporting::TimelineExportRequest`. A present sequence id is authoritative:
if that id no longer resolves to an exportable sequence, enqueue fails with a
status error instead of silently falling back to the active sequence. Only an
absent sequence id may use the active sequence fallback.
Self-hosted export panel model payload builders must mirror their enabled-state
validation and return no payload for missing sequences or blank output paths;
disabled buttons are not the only guardrail against invalid enqueue actions.
Sequence-scoped controls, such as range selection and output path picking, must
also derive their enabled state from the same model readiness checks, and the
status row should explain the first blocking reason instead of showing a generic
ready state when no sequence or output path is available.
Self-hosted export forms persist their editable draft in `AppState::export_draft`
through `ui.export.set_draft`, so widget-tree refreshes and dock layout changes
do not reset selected preset, selected sequence, range, or output path.
Choosing an export output path is also an app-shell intent: panels emit
`app.shell.export_output_dialog` with a suggested name/container extension, and
`self_hosted::shell::resolve_app_shell_action` converts the native save-dialog
result into `ui.export.set_draft(OutputPath(...))`. Widgets must not call
platform file dialogs directly.
Export queue visibility follows the same adapter boundary. The self-hosted
Export panel may show a bounded snapshot of recent `RenderJob` ids, output file
names, statuses, progress, and cancel affordances, but queue mutation still goes
through typed `ui.export.cancel_job` / `ui.export.clear_completed` actions.
`RenderQueue` owns worker state, cancellation flags, and terminal-job cleanup;
`Completed`, `Failed(_)`, and `Cancelled` are all terminal for clear-completed
semantics. Generic widgets only render labels and buttons from the panel model.
Those app-shell dialog intents are built through `app::ui_actions` helpers so
menus and self-hosted panels share the same stable custom-action ids. Shell
local actions, such as About and close-modal, use the same helper boundary
even when they do not resolve to editor-state actions. Native window commands
are deliberately separate from editor state: Quit is emitted as
`app.shell.quit`, and `Action::ToggleFullscreen` is consumed by the
self-hosted host as a `SelfHostedShellCommands` value for the winit entrypoint.
They must not be dispatched into `AppState`, where `CloseProject` keeps the
narrow meaning of closing the current project.
Close-project and quit requests share the same self-hosted pending-close guard:
`AppState::has_unsaved_project_changes()` compares the current project data
fingerprint to the saved `.mdp` archive, and failures are treated as unsaved.
When the guard finds unsaved changes, `SelfHostedUiHost` opens a shell modal for
Save and continue / Discard / Cancel. Save failures keep the modal open and
surface a status error; discard performs the pending close/quit without writing.
`self_hosted::shell::resolve_app_shell_action` is the tested boundary that
turns those intents into concrete project creation, `OpenProject`,
`ImportMedia`, and `SaveProjectAs` actions after a native adapter supplies
platform dialog results. Project creation actions carry full
`SequenceSettings` and `ProjectSettings` payloads before reaching `AppState`.
The self-hosted new-project flow stages editable form state in
`SelfHostedNewProjectDraft`, which owns the same settings structs used by
project creation so the eventual custom form cannot drift from lifecycle
semantics.
Sequence settings follow the same split: `app.shell.sequence_settings` opens a
shell-local `SelfHostedSequenceSettingsDraft`, draft widgets emit
`app.shell.sequence_settings_draft_changed`, and Apply resolves to
`ui.sequence.update_settings`. `AppState::update_sequence_identity_and_settings`
is the only layer that mutates the sequence name/settings, validates the full
`SequenceSettings`, syncs the sequence collection, and records one undoable
snapshot. Shell dialogs must not call `rename_sequence` plus
`update_active_sequence_settings` as separate operations.
The self-hosted sequence settings surface uses shell-local tabs for
format/audio, color management, and preview. All editable fields use typed draft
updates: editing mode, frame size, frame rate, pixel aspect ratio, field order,
video display format, custom width/height, start timecode frame, working/output
color spaces, tone mapping, color workflow, missing metadata policy, nested color
processing, video range, export bit depth, HDR metadata preservation, audio
sample rate, audio channel layout, audio display format, preview render format,
preview resolution scale, and preview cache.
`Action::SplitClipAtPlayhead` similarly routes to `AppState::split_at_playhead`,
which bulk-splits unlocked clips under the playhead and records one undoable
timeline snapshot only when a split actually occurs.
`NudgeClip` and `MoveClipToTrack` wrap the lower-level timeline move mutation at
the action boundary: the wrapper resolves clip location from the active
sequence, ignores true no-ops, records the undo snapshot, publishes timeline
modification, and refreshes the selected clip's track reference after cross-track
moves.
`Copy`, `Cut`, and `Paste` route through app-level clipboards. Animation
keyframes take precedence when the first selected clip has selected keyframes;
otherwise `Copy` stores timeline clips, `Cut` stores then removes selected
clips, and `Paste` recreates clips at the playhead with fresh clip ids and
links rebuilt only among pasted entries. `Duplicate` uses the same recreation
path but places the new group after the selected group's end without mutating
the active clipboard. Menu rows and shortcut dispatch gates must query
`AppState` clipboard capability helpers (`can_copy_to_app_clipboard`,
`can_cut_to_app_clipboard`, `can_paste_from_app_clipboard`) instead of
reconstructing clip, keyframe, locked-track, or active-clipboard rules in shell
or widget code. Paste availability must validate the actual active clipboard
target: animation-keyframe paste requires an unlocked selected clip target, and
clip paste requires every clipboard destination track to still exist and remain
unlocked, matching the mutation path that will run after dispatch.
Inspector timing controls reuse timeline trim actions for clip In/Out changes
instead of introducing a parallel editing path. `TrimClipStart` and
`TrimClipEnd` accept source in/out times from inspector-style controls, convert
them to timeline trim frames with the clip's current positive finite speed
multiplier, then route through `AppState::trim_clips_bulk_to_frame`; locked
track validation, linked-clip trim behavior, undo snapshots, and timeline
modified events therefore stay centralized in the timeline command path.
Inspector effect rows are read from the selected clip's effect instances and
toggle or remove effect instances through `AppState::set_clip_effect_enabled`
and `AppState::remove_effect_from_clip`; per-effect property edits dispatch
`ui.inspector.set_effect_property` and route through the effect instance's
`PropertyBag`. Static-value no-ops are ignored before recording undo history,
so slider/text/color controls cannot add empty undo steps. The widget layer sees
only button / checkbox / typed property values plus stable effect ids and paths.
The generic `Action::RemoveEffect` resolves the clip id into the active sequence
selection reference and reuses the same remove-effect command, so inspector
buttons, shortcuts, scripts, and macros share locked-track validation and undo
behavior.
`Action::ReorderEffects` follows the same boundary and calls
`AppState::reorder_effects_for_clip`, which clamps the target slot, rejects an
invalid source index, and records one undoable sequence snapshot only when order
actually changes. Self-hosted Inspector reorder controls emit this typed action
directly rather than adding an inspector-specific custom action. Effect-stack
toolbars use compact icon-only buttons with tooltips for reorder and removal so
the inspector stays dense without relying on text labels inside destructive
controls. Future effect-row drag/drop must reuse this same action boundary:
`DragPayload::Effect` is only a stable instance identity, while the host adapter
resolves the selected clip, source slot, target slot, locked-track policy, and
undo behavior through `AppState::reorder_effects_for_clip`.
Effects browser activation uses the same protocol family: when a video clip is
selected, effect rows carry a `ui.effects` add-to-clip payload with the selected
clip id and serialized `EffectType`, and `AppState` routes it through
`add_effect_to_clip`. The Effects panel model is derived from the current
`AppState` snapshot, not only from raw selection metadata, so rows are disabled
only when the effect registry itself has no browsable entries. No-target,
non-video, and locked-track states keep the catalog searchable and show the
apply blocker in the panel subtitle, but omit per-row activation actions. The
command layer still performs the authoritative locked-track and effect-target
validation.
This keeps reusable widgets index/value-based and UI-agnostic while avoiding
string parsing in business logic.

## Event Requests

Widgets can request side effects while handling an event:

- pointer capture: keep pointer move/up events routed to the same widget during
  drags or text selection.
- IME state: enable or disable text input composition for the focused text
  widget.
- cursor state: request shell-owned cursor changes, such as crosshair during
  eyedropper mode.
- eyedropper state: enter or leave platform color sampling. The router records
  the request and the app shell delegates desktop sampling to
  `mondrian-platform`.
- tooltip state: widgets call the injected `TooltipManager` through
  `EventContext`; winit shells inject the real manager and `self_hosted::runtime` paints
  the resulting tooltip in the top overlay pass.
- repaint: request another frame for composition previews, cursor blink, or
  delayed UI. The app shell consumes this through `EventRouter` and schedules a
  native window redraw.
- timer wakeups: delayed UI state, such as tooltip reveal timing, reports its
  next required update through the manager layer. `self_hosted::runtime` advances timers
  in `AboutToWait` and uses native `WaitUntil` scheduling instead of idle
  repaint loops.

This keeps widgets testable and avoids leaking winit types into reusable UI
crates.

## Widget Tree Access

`WidgetTreeView` adapts a borrowed `&mut dyn Widget` subtree into the
`WidgetTree` interface used by `EventRouter`. It recursively discovers widgets
through the indexed `child_count()`, `child()`, and `child_mut()` accessors.
The default implementation delegates to `children()` and `children_mut()` so
simple vector-backed containers stay compact.

Custom containers with named child fields should expose their logical children
through the indexed accessors to participate in deepest-hit routing and
router-level pointer capture without reshaping their storage into a `Vec`.
Containers that manually forward events can continue to work, but they should
be migrated toward transparent children before the main editor panels move to
the new UI.

Ancestors receive `after_child_event()` after a descendant handles an event.
This hook is for parent-owned state synchronization, such as rebuilding tab
content after a tab bar changes active index. It must not redispatch the event
to children. Router-generated `FocusLost` is still a child event: when a
focused child handles it because focus moved elsewhere, ancestors receive the
same post-hook so composite widgets can commit editors, close transient state,
or restore wrapper focus without waiting for stale-widget pruning.

The router treats widget ids as frame-local routing handles. Before routing a
new event it drops hovered, focused, or captured ids that are no longer present
in the current `WidgetTree`, which keeps rebuilt panels from inheriting stale
capture/focus state after popovers close, drags finish, or panel contents
refresh. If the stale widget owned focus, the router also emits an IME disable
request because the removed widget can no longer receive `FocusLost`.
If the focused widget still exists but no longer returns `can_focus()`, the
router sends `FocusLost`, releases focus, and disables IME before routing the
next event.
Any event path that lets a widget request focus must normalize the focused
panel from the current widget tree before returning, including pointer-move
paths used by hover, drag, or composite controls. This keeps panel-scoped
shortcuts deterministic even when focus is claimed outside ordinary click or
Tab traversal.

## Focused Text Input

Keyboard, text, and IME events route to `FocusManager::focused_widget()`.
`TextInput` requests focus on click, enables IME while focused, stores preedit
composition text, and inserts committed IME text through the same grapheme-aware
editing path as normal text input.
While preedit composition is active, the text input owns `KeyDown` events so
Backspace, Delete, arrows, and global shortcut fallbacks cannot mutate committed
text or dispatch editor actions before the platform sends the next preedit or
commit update. Escape clears the local preedit preview without committing text
and requests repaint.
Focus traversal treats a cycle back to the same widget as a no-op: the router
handles Tab but does not emit `FocusLost`/`FocusGained`, avoiding selection and
IME flicker when only one focusable control is present. If the current tree has
no focusable target at all, Tab remains `Ignored` rather than being promoted to
a handled shortcut, so empty startup/modal states do not swallow platform or
user-level key paths.

Single-line text input maintains a horizontal viewport owned by the widget. The
cursor is scrolled into view after layout, editing, navigation, or selection
changes; pointer hit testing accounts for the current scroll offset. IME cursor
areas use the caret rect rather than the full widget bounds so platform
composition windows can anchor near the insertion point.
Because `TextInput` is currently a single-line control, committed user input
normalizes line separators from typing, IME commit, and clipboard paste into
spaces before insertion. Paste reads and validates clipboard text before
replacing a selection; an empty or unavailable clipboard must preserve the
current selection and committed text. Word navigation uses Unicode whitespace
boundaries instead of ASCII-space-only checks, so pasted names or paths
containing tabs, non-breaking spaces, or platform line endings still behave
predictably.

Text content is clipped to the padded content rect, not the outer widget
bounds. App shells should show an I-beam cursor for text inputs only after the
input owns focus; hover alone should not switch the pointer shape. The
self-hosted product window derives this from the focused widget's
`accepts_text_input()` state when choosing the native cursor, while eyedropper
and splitter cursors keep higher priority. Native cursor refresh must run after
focus-changing keyboard and pointer events as well as pointer movement, so
clicking into a text field or tabbing focus does not wait for the next mouse
move before showing the I-beam.
Text inputs use the same `mondrian-ui-text` measurement path as glyph rendering
for cursor movement, selection geometry, hit testing, and horizontal scroll;
approximate width estimates are not used for editable text internals.
TextInput event handlers must request repaint whenever focus chrome, caret
position, selection highlight, committed text, mouse-drag state, or IME preedit
preview changes; these visuals must not depend on an unrelated shell redraw.
IME is a platform side effect: text widgets emit `EventRequests::ime`, the
event router exposes the latest request, and the app shell applies it to the
native window (`set_ime_allowed` plus cursor area for winit). Pointer clicks
outside the focused widget send `FocusLost` so IME is disabled when editing
ends.

`NumberInput` is a thin composite around `TextInput`, not a second editable-text
implementation. It reuses text focus, selection, IME, clipboard, clipping, and
scrolling behavior, then parses committed text changes into clamped/stepped
numeric actions. Invalid numeric text remains local and dispatches no action,
letting users type partial values without mutating domain state from malformed
input. Enter and focus loss are display commit points: valid text is reformatted
to the clamped/stepped value with the configured decimal precision, while
invalid text reverts to the most recent valid number already accepted by the
control. Focused numeric fields also support Up/Down and PageUp/PageDown
keyboard nudging; configured steps are reused, Shift multiplies the arrow step
by ten, and decimal precision supplies a predictable default when no explicit
step is available. Changing decimal precision must reformat the committed value
itself so fractional app-state snapshots such as `0.5` do not round through an
intermediate integer display.
Numeric widgets must sanitize inverted or non-finite ranges and values before
clamping so plugin descriptors and stale app snapshots cannot panic the UI
thread.
Panel adapters may set a preferred `NumberInput` width when composing compact
property rows; the input still receives its final bounds from layout and must
not own row-level sizing policy.

Text copy/cut shortcuts are consumed by `TextInput` only when a selection exists.
Clipboard and select-all editing shortcuts are exact `Ctrl` chords; Alt, Meta,
or Shift-modified variants are ignored so panel/workspace shortcut routing can
own them. Word navigation is owned by `Ctrl+Left/Right`, with
`Ctrl+Shift+Left/Right` extending selection by word. `Ctrl+Home/End` moves to
the text boundaries, and `Ctrl+Shift+Home/End` extends selection to those
boundaries. If there is no selection, `Ctrl+C` and `Ctrl+X` are ignored so
panel-level commands, such as copying clips or keyframes, can handle them.
When a `TextInput` receives an outside mouse down directly, it clears local
focus and IME state but returns `Ignored`; the clicked sibling must still receive
the event. Intentional focus loss that should stop propagation is delivered by
the router through `FocusLost`. Mouse release follows the same ownership rule:
an idle text input must ignore `MouseUp` and must not release pointer capture it
does not own. Only a text-selection drag that began in that input handles the
release and emits a capture release request.

## Shortcut Routing

`mondrian-ui-events::EventRouter` resolves registered shortcuts only after the
focused widget has had a chance to handle a `KeyDown`. This keeps text editing,
IME composition, and panel-local keyboard commands ahead of global shell
bindings while still giving menus and workspace commands a keyboard path when
no widget consumes the key. The self-hosted product window registers default
global shortcuts at the router boundary, not inside widgets: file commands use
Ctrl/Ctrl+Shift combinations, workspace switching uses Ctrl+Alt+number, and
panel focus uses Ctrl+Alt+mnemonics. Plain Space is intentionally not registered
globally so text input cannot accidentally toggle playback while typing.
Text-capable widgets expose `Widget::accepts_text_input()` while their editable
field is active. When that is true, the router does not resolve unmodified or
Shift-modified printable `KeyDown`s as shortcuts; the matching `TextInput` or
IME commit event remains the authoritative text mutation path. Ctrl and Meta
chords still reach shortcut resolution so explicit editing shortcuts such as
copy, paste, select-all, and user-defined command chords keep working. Alt-only
`KeyDown`s also reach widgets/router first, but the shell may still route a
following printable text payload so AltGr-style keyboard layouts can commit
characters when the platform reports real text.
Composite widgets that keep private `TextInput`s outside the public widget tree
must translate private focus back to the composite's stable widget id while
preserving the inner text field's local focus and IME requests. Router focus
must never point at an id that `WidgetTree::get()` cannot resolve on the next
event, otherwise text input, shortcut shielding, and focused-panel context will
be pruned as stale state.
Every product `PanelKind` exposed by the View menu must also have a default
focus shortcut descriptor so menu rows, shortcut labels, and router bindings
stay in one access model.
Common NLE editing keys such as Delete, Shift+Delete, Ctrl+K, I/O, Home/End,
arrow frame stepping, Ctrl+A, and Escape are registered as global fallback
bindings only; focused `TextInput`s, IME composition, dropdowns, sliders, and
timeline-local handlers still receive the key first.
Self-hosted menu shortcut hints read from the same default shortcut descriptor
table that registers router bindings, so displayed accelerators cannot drift
from actual keyboard behavior.
Shortcut-dispatched actions still pass through the self-hosted host's
`AppState` availability gate before shell dialogs or editor dispatch run. This
keeps keyboard shortcuts and disabled menu rows semantically aligned: an
unavailable Import, Save, Undo, or Redo command is ignored before native dialogs
or app mutations can start.
Timeline edit gates must mirror the action handler's authoritative target
validation instead of checking only whether a selection vector is non-empty. For
example, Delete and Ripple Delete are available for selected clips only when all
target tracks are unlocked, and for selected tracks only when every selected id
still exists and at least one video and one audio track remain after removal.
Shortcut resolution receives a `ShortcutContext` from the router focus state and
must search scopes in a fixed order: focused widget, focused panel, workspace,
then global. Same-scope duplicate registrations replace the older binding so
the active command is deterministic.
Self-hosted shortcut overrides must be resolved into a conflict-free active
descriptor table before router registration, menu hint lookup, and Preferences
row construction. A user override owns its chosen chord: any default descriptor
that would collide is omitted from the active table and shown as disabled in
Preferences, so visible shortcut hints never advertise a binding that dispatches
a different command. Same-priority default/default or override/override
collisions remain deterministic by descriptor order, matching router
replacement semantics. Preferences rows for collision-disabled descriptors must
also expose the owning descriptor id, for example "与 file.save_project 冲突",
instead of showing the same generic disabled state used by an explicit user
disable.
If no widget handles a `KeyDown` and no registered shortcut matches it,
`EventRouter` returns `EventResult::Ignored`; unmatched keys must not be
converted into `Action::NoOp`, because that would consume user-level tool,
input-method, and platform shortcut paths without a Mondrian command.
This does not re-inject an already delivered winit event back into the OS;
native global hotkeys and input-method switches are normally claimed by the
platform before the app receives them. The winit shell adapter must preserve
the router result: ignored shortcut chords are not promoted to shell actions,
so external tools, OS-level input switching, and user-remapped shortcuts remain
available outside Mondrian's registered command table.
Self-hosted default shortcuts are descriptors with stable ids. The shell loads
`SelfHostedPreferences.shortcut_overrides` before registering router bindings:
an override can replace the binding or set it to `None` to disable a default
shortcut. Menus and the Preferences shortcut list read the same active
descriptor table, so disabling a conflicting `Ctrl+Alt` panel/workspace chord
also removes the visible shortcut hint. The Preferences Shortcuts tab dispatches
shell-local Disable, Default, and Rebind actions keyed by descriptor id. Rebind
captures the next supported `KeyDown` inside the Preferences modal, consumes
Escape as cancel, and serializes only the shortcut key plus modifier booleans in
the shell action payload. `SelfHostedUiHost` persists those updates, makes the
new binding win through the conflict-free active descriptor table, and the
native window session immediately rebuilds the router's global shortcut scope
from that table by clearing `ShortcutScope::Global` and re-registering active
shortcuts. These preference updates are shell-local navigation/preferences
state, not editor actions, and must not create undo history.
Because the descriptor table is longer than the compact Preferences modal, the
Shortcuts tab keeps its heading fixed and scrolls the shortcut rows inside a
clipped viewport with a token-painted scrollbar; row buttons must use the same
scroll offset and must not receive pointer events outside that viewport.
Focused-panel context is inferred by walking from the focused widget to the
nearest ancestor widget that exposes `Widget::panel_kind()`. `PanelSlot` is the
normal boundary that returns a panel kind. Leaf controls request only widget
focus through `FocusManager::request_focus(widget)`; they must not hardcode
panel identities. The router normalizes focused-panel state after widget events
and at the start of routing, so panel shortcuts follow the actual dock location
even after a panel tree rebuild leaves the focused widget alive under a
different panel boundary.
Dock panel chrome currently carries `PanelKind` directly because focus scopes,
scroll-state restoration, and product menu targeting all use the same app-level
panel identity. Do not add widget-local aliases for the same enum; a future
domain-free dock API must migrate `Widget::panel_kind()` and shell mapping
together.
`PanelSlot` is also the normal paint and hit-test clipping boundary for docked
panel content. Ordinary panel widgets are clipped to the slot bounds so fixed
height diagnostic/demo content cannot bleed into adjacent panels. Popups,
tooltips, dropdowns, and color-picker overlays intentionally use
`paint_overlay()` and remain unclipped by the slot; overlay stacking is handled
by the shell/router overlay pass instead of per-panel paint.

## Pointer Capture

Drag widgets request capture on mouse down and release capture on mouse up.
`Slider`, `DockSplitter`, and text selection depend on this behavior. Future
Timeline clip drags, curve editor handles, and color picker gestures should use
the same request path. Focus loss may cancel an active drag, but widgets must
only emit a capture-release request when they actually own an active drag; a
keyboard-focused idle control must not clear unrelated router capture.

Composite widgets that own internal popup controls must not leak private child
`WidgetId`s into router-level capture. If the inner control is not a real node
in the widget tree, the wrapper translates capture/release requests to the
wrapper's own id before returning from `event()`.
Composite widgets with private text fields should also route pointer input to
the hit field first and route keyboard/IME input only to the focused private
field, rather than broadcasting events to every private child.

Parent-owned chrome that must win over child hit targets, such as
`DockSplitter` handles over tab bars, uses `Widget::before_child_event()`.
Splitter handles paint above children, use a narrower 6px interaction zone by
default, draw full-span geometric rectangles instead of text or glyphs, grow
while hovered or dragged, and span the full splitter bounds without endpoint
gaps. A splitter drag may be cancelled by `FocusLost` through the same
before-child path; only an active splitter drag handles that event and releases
capture, while an idle splitter ignores it.

Slider value mapping uses the same thumb-centered track for painting and
pointer updates. The thumb rect must remain inside widget bounds; if a parent
gives a short row, the thumb shrinks vertically instead of being clipped.

Timeline widgets own frame-space presentation interactions: selection, seeking,
scrolling, zooming, local drag previews, and edge-trim previews. On mouse
release they emit domain-light move/trim proposals (`TimelineClipMove`,
`TimelineClipTrim`) instead of resolving clip overlaps, ripple behavior, linked
media, source in/out offsets, or undo snapshots. Those semantics stay in
`mondrian-app` / `mondrian-timeline` command handling.
Presentation-level drag guards run before any proposal leaves the widget: source
locked tracks cannot start clip moves or trims, incompatible or locked target
tracks fold back to the source track, stale local clip references cancel the
active preview, and disabled timelines clear pending drag state at the event
boundary. The app adapter still resolves those proposals through stable
`TrackId` / `ClipId` identities and rejects stale ids or media-kind mismatches
before dispatching typed actions, with command handlers as the final mutation
authority.
Timelines expose
snapping as another widget-local preview layer: the explicit Snap toolbar
toggle is stored in `TimelineViewState`, defaults on, and focused `S` toggles
it without dispatching editor actions. When enabled, clip moves, edge trims,
and ruler playhead drags may adjust their proposed frame to nearby timeline
start, playhead, clip edge, or in/out candidates and paint a snap guide; the
app layer receives only the adjusted frame proposal. Future marker, linked
clip, or ripple-aware snapping should extend candidate generation without
moving timeline mutation rules into the widget crate. Timelines expose
persistent horizontal and vertical scrollbars in reserved gutters; scrollbar
thumb drags, endpoint-handle drags, and track paging must win hit testing over
clip selection and seeking. Horizontal scrollbar endpoint handles adjust the
visible frame span by mutating widget-local `pixels_per_frame`; vertical
scrollbar endpoint handles adjust widget-local track height. Timeline surfaces
are focusable: while focused they may handle
timeline-local navigation such as playhead nudging, but global editor commands
remain outside the widget layer. Timeline pointer capture ownership is tracked
explicitly and is separate from keyboard focus: `FocusLost` or disabled-event
cleanup releases capture only if an active timeline drag previously captured
the pointer, so an idle focused timeline cannot clear another overlay or
control's capture. Programmatic timeline disabling must cancel every pending
timeline-local drag preview, including playhead, in/out marker, track, clip,
trim, and scrollbar drags, so a later re-enable cannot commit stale pointer
state from a previous panel model.

## Overlays

Dropdowns, context menus, and tooltips derive event hit regions and paint
geometry from shared rect helpers. Dropdown triggers, context-menu popup chrome,
rows, separators, checked-row checkmarks, and scrollbars are painted through
shared menu helpers so action-backed dropdowns, context menus, and internal
selectors keep the same visual language. Checked rows are intentionally quiet:
the row fill stays the normal popover fill unless hovered, and selected state is
communicated by the checkmark only, with no full-row primary fill or decorative
left accent rail. Disabled menu items consume pointer input
without dispatching actions or closing the overlay; outside clicks close open
menus. Closed dropdown measurement is based on the trigger label only so long
popup choices do not widen compact inspector rows; the popup itself expands to
the longest row label. Dropdowns and context menus measure row labels through
the widgets crate's shared `mondrian-ui-text` measurer, so CJK, long Latin
labels, and platform font metrics use the same layout facts as final glyph
rendering instead of approximate character-width math.
Popup elevation goes through the widget crate's shared paint helper and theme
shadow tokens rather than per-widget hardcoded black alpha values, so dark/light
themes can tune perceived depth centrally. Dropdown popups use the renderer's
analytic soft-shadow primitive through the shared helper: widgets emit ambient
and contact shadow requests from semantic shadow tokens, and the GPU evaluates a
rounded-rectangle distance field falloff instead of stacking hard expanded
rectangles. This keeps menus visually closer to modern shadcn/Codex-style
floating surfaces while preserving deterministic, low-vertex draw commands.
Common color composition helpers such as alpha scaling, color mixing, and
softened borders also live behind that shared paint helper so widgets do not
drift in their interpretation of tokens.
Dropdowns, context menus, and embedded selectors such as the color-picker mode
menu use the shared anchored-menu geometry for viewport edge clamping and
above/below flipping, and their hit-testing is derived from the same rects used
for overlay painting.
Menus whose rows exceed the root overlay viewport must clip their row content
and scroll in the same PC wheel direction as dropdowns, rather than overflowing
into unrelated dock panels. All anchored-menu consumers, including dropdowns,
context menus, and embedded selectors, must require finite positive-area overlay
viewports before computing popup placement or emitting draw commands; invalid
or empty viewport clips should skip popup paint instead of relying on
renderer-side degenerate command rejection.
Dropdown trigger labels are clipped to the trigger text lane, reserving the
arrow area when enabled, so constrained form rows do not let long labels paint
over affordances or neighboring controls.
Popup row labels are also clipped to their padded text lane; disabled rows and
long labels must never bleed into separator geometry, scrollbars, or neighboring
rows.
Menu items may carry optional `VectorIcon` geometry. If any item in a dropdown
or context menu has an icon, the popup reserves one aligned icon lane for the
whole menu while still keeping designer assets mapped at the app layer.
Menu items may also carry a right-aligned shortcut hint; the shared menu row
helper owns the shortcut lane and label clipping so shell menus, context menus,
and internal selectors do not hand-place accelerator text differently.
Self-hosted menu availability is applied in the `mondrian-app` shell adapter
from the current `AppState` snapshot. The widgets crate owns disabled-row
behavior and painting only; project availability, undo/redo availability, and
native-dialog prerequisites stay at the app boundary so generic dropdowns do
not learn domain state.
Tooltip requests preserve their delay timer when the same tooltip is reported
repeatedly during hover, and tooltip painting clamps to the current clip rect.

Overlay-capable widgets paint their normal trigger chrome in `paint()` and
their floating chrome in `paint_overlay()`. `TreeWalker::paint()` runs the
overlay pass after the root's normal paint pass, so dropdown menus and similar
popups are not hidden by siblings that happen to paint later in the normal
content tree. Tooltip popups are overlay-only: a composite widget that owns a
tooltip must forward it from `paint_overlay()` instead of drawing it inside the
panel's normal `paint()` method. Composite shell widgets must also forward
overlay hit testing through their owned popups; for example `TitleBar` forwards
to `MenuBar`, and `MenuBar` forwards to each open `Dropdown`, so menu clicks are
not mistaken for outside-panel input after the first popup frame.

Dropdowns request pointer capture while open so outside clicks, Escape, wheel
events, and release events continue to route to the popup even when the pointer
is over another widget. The opening click's release is suppressed so it cannot
accidentally select the first item under the cursor; item actions dispatch only
when a press and release land on the same enabled row. Long dropdown menus clip
their item list and scroll with the same positive-delta-means-content-down
offset convention as `ScrollView`; offset-changing menu wheel input must
request repaint, while boundary wheel input may remain handled without
scheduling redundant redraws because the open popup still owns wheel capture
above panels beneath it. Menu separators are explicit non-action items and paint
geometric divider rects; disabled items only mute their text and must not draw
strikethroughs or divider-like chrome.
Open menus support keyboard navigation: unmodified Up/Down cycles through
enabled action rows while skipping separators and disabled rows, unmodified
Enter/Space activates the highlighted row, and Escape closes the popup. Modified
navigation and activation chords stay ignored so application shortcuts remain
centralized. Internal selectors such as the ColorPicker mode menu follow the
same keys but commit local widget state instead of dispatching editor actions.
Context-menu owners should treat a second right-click inside the same surface
as a replacement request, not as a simple close of the old menu. The old menu
is discarded and the event continues through the owner's normal target
resolution so asset cards, timeline clips, and empty canvas areas can open the
correct new menu and clear stale local targets in one gesture. Outside clicks
still close through the shared `ContextMenu` overlay hit-test path.
Menu bars coordinate sibling dropdowns: when one menu is open, clicking or
hovering another menu trigger closes the old popup and opens the new one.
Trigger-click opens suppress the matching release; parent-coordinated hover
opens must not, because the next release belongs to a fresh menu-item click.

Keyboard activation for focused controls follows desktop conventions: Button
handles Enter/Space as an activation gesture, and Checkbox handles Enter/Space
as a toggle gesture. Only unmodified KeyDown starts the semantic action and
enters the pressed visual state; modified chords stay ignored so application
shortcuts remain centralized. The matching KeyUp clears an existing pressed
state even if the modifier state changed before release. Keyboard events are
routed by focus, so these handlers do not perform hit testing.
Common controls must request repaint whenever hover, pressed, focus-visible, or
disabled-state cleanup changes their visual state. Pointer hover updates may
remain propagation-neutral, but the shell cannot rely on unrelated frame
invalidations to show pressed feedback, focus rings, or stale-state cleanup.

Color pickers reuse the shared color model conversions from `mondrian-core`
(`Color`, `RgbaColor`, `HslColor`, `HsvColor`, and `CmykColor`). The widget
layer owns layout, text fields, swatch painting, and model tabs only; conversion
math and hex parsing stay out of UI crates.

Curve editors own normalized 0..1 point editing, hit testing, insertion,
deletion, dragging, keyboard nudging, and paint geometry. Domain layers map
effect keyframes, speed ramps, or tone curves into `CurvePoint` view models and
commit semantic mutations from the emitted action; timeline/effect command logic
must not move into the widget crate.

Tooltip positions are anchored when the pointer enters a trigger and then
clamped by the tooltip widget to the current clip rect. Repeating the same
tooltip request keeps the original anchor so the popup does not chase pointer
movement. Tooltips use a low-emphasis border token, not primary/ring colors.
Tooltip text uses `draw_text_box` and `mondrian-ui-text` paragraph measurement
before painting its background. The text layer uses cosmic-text wrapping with
word breaks and glyph fallback for overlong tokens, so tooltip widgets do not
own line-breaking logic. Tooltip fill and border geometry must both be clamped
to the root clip rect so edge-adjacent popovers do not bleed outside the window.
If the root overlay clip is empty or non-finite, the tooltip widget should skip
painting entirely rather than emitting degenerate rect, clip, or text commands
and relying on the renderer to discard them.

## Overlay Contract

Dropdowns, popovers, context menus, tooltips, and shell affordances paint in
the overlay pass after normal widget content. A widget with an open top-layer
popup that needs outside-click dismissal must expose that boundary through
`overlay_hit_test()`, not by widening its normal `hit_test()` bounds. The event
router resolves overlay hits before normal content hits and searches child
overlays before a parent's broad close layer, so a nested or visually topmost
popup receives pointer and wheel events before an ancestor outside-click
catcher or a sibling panel.
Overlay paint ordering is a two-pass tree contract, not a local per-parent
style: the whole normal content pass completes first, then child overlays paint
through `paint_overlay()`. Regression tests should include an earlier child's
overlay painting after a later sibling's normal content, because this is the
case that prevents dropdowns, context menus, and tooltips from being hidden
behind adjacent dock panels.
Pointer capture remains authoritative for drags and open popup internals, but
only inside the current top-layer overlay scope: if a newer sibling overlay such
as a modal is visually above the captured widget, pointer routing targets that
top overlay and clears the covered sibling capture instead of leaving a stale
capture that can resurface after the overlay closes. Captured descendants inside
the active overlay keep receiving move/up events so sliders, scrubbers, and drag
handles do not lose capture just because their modal or popup exposes a broad
overlay boundary. Dragging popup internals should use pointer capture so move/up
events remain routed to the owning widget.
Active drag-and-drop uses the same overlay-first target resolution as pointer
events: open context menus, dropdowns, popovers, and modal surfaces must receive
`DragEnter`, `DragOver`, and `Drop` before normal panel content beneath them.

Application render loops must call `TreeWalker::paint_clipped()` with the
current window or surface bounds. Overlay placement uses `PaintContext.clip_rect`
as the root viewport for flipping and edge clamping; using the unbounded
`TreeWalker::paint()` helper in production shells makes dropdowns, color-picker
mode menus, and tooltips think the screen is infinite and can push popups off
the visible window.
After the widget tree overlay pass, the self-hosted runtime paints shell-owned
overlays in deterministic bottom-to-top order: active desktop eyedropper chrome
first, then tooltip chrome. This keeps textual hover help readable above
sampling affordances while still leaving widget popups below shell affordances.

## Scroll Views

Scroll containers lay their child content out in scrolled screen coordinates:
`child_origin = viewport_origin - scroll_offset`. Painting only pushes the
viewport clip and does not add a private content transform. This keeps widget
tree hit-testing, pointer capture, overlay routing, and child widget events in a
single screen-coordinate model; controls inside a scrolled inspector or list
therefore drag the same way as controls outside the scroll view. Wheel events
are handled only inside the viewport and offsets are clamped after wheel input
and layout. Offset-changing wheel, track, thumb drag, and scrollbar hover
transitions request repaint through `EventRequests`.
Programmatic and restored `ScrollViewState` offsets are sanitized at the widget
boundary: negative, NaN, and infinite values clamp to a finite scroll range
before child layout. Persisted or corrupted local state must never move child
content to non-finite screen coordinates.
Normal child hit testing is clipped by the scroll viewport through the
`Widget::child_hit_test_clip()` contract, so offscreen scrolled content cannot
steal clicks from sibling controls or panels. Overlay hit testing deliberately
ignores that normal-content clip: dropdowns, context menus, color pickers, and
tooltips owned by a scrolled child still paint and receive events in the top
overlay layer when open.
`ScrollView::event()` also enforces this viewport boundary itself for pointer
events. Directly routed events, compound-widget delegation, and unit tests must
therefore obey the same clipping rule as tree hit testing: normal child content
outside the viewport is inert, while open child overlays may still receive
overlay-routed input.
Nested scroll surfaces use child-first wheel routing. A `ScrollView` forwards
wheel input to the hit child before changing its own offset; only ignored wheel
events scroll the parent. This matches desktop editor behavior for scrollable
lists, dropdowns, and diagnostic panels embedded inside a larger scroll surface.

Renderer clip state is hierarchical. When a child widget pushes its own text or
content clip inside a `ScrollView`, the renderer intersects that child clip with
the active parent viewport clip before batching and applying the GPU scissor.
Nested clips must never replace their parent clip; otherwise text glyph images
inside a scrolled child can bleed over sibling controls outside the viewport.
Clipped containers must also narrow `PaintContext.clip_rect` while painting
children, not just push a renderer clip command. Text and paragraph widgets use
that paint-time clip for width decisions, so the component layer and renderer
clip stack must describe the same viewport. `ScrollView` therefore pushes the
same effective viewport clip that it stores in `PaintContext.clip_rect`, namely
the intersection of its bounds and the incoming parent clip. Widget code should
push local clips through `PaintContext::push_clip()` / `pop_clip()` rather than
calling the encoder directly, because the helper intersects local clips with the
current root/window clip before they become renderer scissor state.
Scrollable chrome that lives in the reserved gutter, such as the built-in
scrollbars, must still be clipped, but it uses the scroll container bounds
rather than the content viewport. A `ScrollView` paint pass therefore has two
explicit clip scopes: child content paints under the viewport clip, then
scrollbar chrome paints under the container-bounds clip. The component
`PaintContext.clip_rect` and renderer clip stack must match for each scope
during the pass.
Layout measurement must use the same viewport and scrollbar-gutter resolution
as final layout. `FlexLayout` therefore measures children with the parent inner
bounds instead of an unbounded constraint, and `ScrollView::measure()` performs
the same iterative gutter reservation used by `ScrollView::layout()`. Wrapped
text, inspector rows, and scrollable panels must not require a later splitter or
window resize to settle into their final line breaks.
Clip rectangles are snapped conservatively by flooring their top-left and
ceiling their bottom-right edge before GPU scissoring. Ordinary shape, image,
line, and vector geometry keeps subpixel coordinates so SDF antialiasing, MSAA,
and linear texture filtering behave like browser/native UI renderers instead of
rounding designer-authored geometry into visibly rough edges.
Line rendering uses GPU analytic segment SDF in the fragment shader. The CPU
batcher emits an expanded local-space quad with round-cap and antialias padding,
but it must not pre-rasterize hairlines or snap endpoints to integer pixels.
The shader derives the segment endpoints from that padded local geometry; CPU
coverage tests and shader-contract tests must stay in sync so 45-degree
hairlines remain connected and stable across subpixel phases and all primary
angles.

`ScrollView` defaults to vertical-only scrolling so inspector/property panels
continue to wrap and measure against their panel width. Scroll content fills the
viewport on the non-scrolling axis: vertical views lay the child out at viewport
width, horizontal views at viewport height, and dual-axis views clamp each axis
to at least the viewport size. This avoids narrow natural-size children causing
unstable text wrapping, clipping, or hit-test geometry inside panels.
Components that truly need overflow in the other direction must opt into
`ScrollAxes::Horizontal` or `ScrollAxes::Both`; horizontal scrolling then uses
Shift+wheel and a draggable bottom scrollbar. Dual-axis scrollbars reserve
viewport gutters and the bottom-right corner from one another, while painting
the scrollbar chrome in a separate clipped scope from child content.
Wheel events are consumed only when a child handles them or the scroll view
actually changes its offset; at scroll boundaries or on a disabled axis they
return ignored so an enclosing scroll surface can continue the gesture.
Thumb drags request pointer capture, map thumb-track movement back to content
scroll offset, and release capture on mouse up. Focus loss also cancels an
active thumb drag and releases capture, while an idle scroll view ignores focus
loss so it cannot clear unrelated capture state. Clicking the scrollbar track
outside the thumb pages the viewport by one visible span. Compound widgets that
embed a private `ScrollView` must translate its pointer-capture requests to the
outer widget id, because the inner scroll view is not present as an independent
node in the event tree.

## Panel Migration

Panel migration should start with low-risk inspector/property surfaces before
Timeline. Inspector-style controls should be assembled with the reusable
`PropertyPanel` / `PropertySection` / `PropertyRow` container in
`mondrian-ui-widgets`, then bound to editor state and undoable commands at the
panel/app layer. Docked panel chrome belongs to `DockPanel`, which owns the
`DockTabBar`, active-tab content rebuilding, `PanelSlot`, and overlay
forwarding. App entrypoints should call panel adapters in
`mondrian-app::self_hosted::panels` instead of reimplementing tab/content
synchronization. The self-hosted product Inspector slot uses this path with
real widgets (checkbox, slider, and color trigger) instead of a colored
placeholder. Its panel snapshot carries the selected clip identity, and user
edits emit stable inspector actions that mutate the selected clip through
undoable app state commands. Basic transform controls show position in sequence
pixels, uniform scale in percent units, and rotation in degrees; the app action
handler converts those UI values back into `Transform2D` property mutations.
Timing controls show absolute timeline frames and dispatch the same
`ui.timeline.trim_clips` payloads as the Timeline view. Single-clip controls
wrap their target in a one-element `clip_ids` list; multi-clip operations use
the same payload shape so app handlers can keep linked-clip behavior, locked
track validation, undo snapshots, and event publication centralized.
Timeline selection actions treat the clip id as authoritative and resolve the
current track from `AppState`; track ids in widget snapshots are context only
because they can be stale after moves, undo/redo, or refresh lag.
`TimelineClip::disabled` is visual playback state, not an input-hit-test guard:
disabled or locked-track clips must remain selectable so the Inspector can show
read-only state. Edit prevention belongs to locked-track checks in drag setup
and, authoritatively, app command handlers.
Track, clip, mask, and animation keyframe selection state is owned by
`mondrian-app::app::selection`; action handlers should call its AppState
methods instead of directly clearing individual selection fields. Track
selection is the broadest timeline target; selecting tracks clears clip, mask,
and keyframe scopes, while selecting clips clears selected tracks. Clipboard,
duplicate, paste, and timeline mutation paths should also replace or clear clip
selection through that module so nested selection scopes stay consistent.
Self-hosted Timeline track headers dispatch the generic
`Action::Select(SelectionTarget::Track(_))`; the widget exposes only
index-based `TimelineTrackRef`s, and the app adapter maps those refs to stable
`TrackId`s.
Track header controls follow the same boundary: `TimelineView` emits a
domain-light control kind for visibility, mute, or lock, while the app adapter
turns the current snapshot into a stable `ui.timeline.set_track_control`
payload. `AppState` then calls the undoable `set_track_visible`,
`set_track_muted`, or `set_track_locked` command path.
Track header reordering emits a view-index `TimelineTrackMove` proposal only.
The app adapter rejects cross-kind moves, resolves the source `TrackId`, converts
the target view index into a video/audio-local target index, and dispatches
`ui.timeline.move_track` so `AppState::move_track` remains the only mutation
path for track order.
Timeline context menu entries for adding video/audio tracks emit only a
`TimelineTrackKind`; the app adapter translates that into `ui.timeline.add_track`,
and `AppState` routes it through the existing undoable track creation commands.
Timeline pointer tools are widget-local session state. `TimelineTool::Select`
keeps normal selection, move, trim, and seek behavior. `TimelineTool::Blade`
matches the egui-era workflow without adding a separate app command by seeking
to the clicked frame and dispatching `TimelineEditCommand::SplitAtPlayhead`.
Shortcut keys `V` and `B` switch these widget-local tools; undoable timeline
mutation still starts only at the app command boundary. `TimelineViewState`
captures the active tool, zoom, scroll offsets, snapping flag, and track height
so `SelfHostedAppRoot` preserves timeline working context across panel model
rebuilds without storing that UI session data in `AppState`. The toolbar above
the ruler is reserved for compact mode and mark controls: Select, Blade,
Snapping, Mark In, and Mark Out.
Structural or destructive operations such as Add Video Track, Add Audio Track,
Split at Playhead, Delete, and Ripple Delete belong in the timeline context menu
and focused keyboard shortcuts rather than compact icon buttons. Context menu
rows use the same `TimelineTrackKind` or `TimelineEditCommand` adapters,
availability checks, and action factories as focused keyboard input.
Timeline command availability has two layers: the widget checks only local
view facts such as selection and playhead intersection, while the self-hosted
adapter injects app-state availability derived from the same locked-track,
clipboard, in/out, and sequence gates used by the top menus. Timeline context
menu rows and focused timeline shortcuts must consult that host availability
before dispatching. Typed timeline actions also pass through this gate before
they reach `AppState`: select, move, trim, selected enable/disable, explicit
range edits, seek, add/move track, and track controls must reject malformed
payloads, stale clip or track ids, missing sequences, and locked clip-edit
targets at the shell boundary. This gate prevents dead UI commands and stale
panel snapshots from producing editor status errors; deeper timeline mutation
rules such as overlap resolution and media-type compatibility remain
authoritative in the app/timeline command layer.
Disabled toolbar controls consume their click without dispatching so they
cannot accidentally seek or select timeline content underneath. Display zoom
mutates widget-local `pixels_per_frame` through Ctrl/Meta wheel or the
horizontal scrollbar endpoint handles and does not emit editor actions because
display zoom is not project data.
Timeline wheel input follows the same consumption rule as scroll containers:
vertical scroll, Shift+horizontal scroll, or Ctrl/Meta zoom handles the event
only when the corresponding offset or zoom value changes, so boundary wheel
input can bubble to an enclosing surface.
Timeline chrome buttons, including pointer tools, snapping, and mark controls,
publish hover hints through the shared tooltip manager rather than
painting local text labels inside the compact toolbar. Mark-command toolbar
hints append the same host-provided shortcut labels used by timeline context
menus without hardcoding platform shortcut text in the widget crate. Toolbar
vector assets are injected through `TimelineToolbarIconSlot` only; pointer tools
do not carry a second tool-specific icon path. This keeps the toolbar's hit
testing, availability, tooltip, and icon contracts aligned around the same
button model.
Asset drops follow the same boundary. `TimelineView` accepts
`DragPayload::Asset` only as a domain-light drop proposal with a view track ref
and frame. The self-hosted adapter resolves that view ref to a stable
`TrackId`, dispatches `ui.timeline.drop_asset`, and `AppState` prepares the
asset when necessary before calling the existing video/audio drop commands.
Clip construction, media-kind validation, linked audio creation, conflict
resolution, undo snapshots, event publication, and autosave therefore stay in
the app command layer rather than leaking into widgets or panel adapters.
Timeline-focused keyboard editing follows the same rule: `TimelineView` emits
domain-light `TimelineEditCommand`s, and the app adapter maps them onto shared
editor actions such as `Action::DeleteSelection` and
`Action::RippleDeleteSelection`. Split-at-playhead uses the same path via
`TimelineEditCommand::SplitAtPlayhead` and `Action::SplitClipAtPlayhead`;
focused timeline clipboard shortcuts use the clipboard edit commands
(`CutSelection`, `CopySelection`, `PasteAtPlayhead`, and
`DuplicateSelection`) before reaching app-level clipboard actions.
Timeline-local playhead stepping handles unmodified Left/Right/Home/End, with
Shift multiplying Left/Right by the coarse step; Ctrl/Alt/Meta seek chords stay
ignored so command routing and workspace shortcuts can own them explicitly.
Nested-sequence navigation stays in this boundary as well. `TimelineClip` only
marks that a clip is nested, never stores a `SequenceId`; when the clip context
menu is opened the widget emits `TimelineEditCommand::OpenNestedSequence` with
the local `TimelineClipRef`, and the app adapter resolves that ref to
`ui.timeline.open_nested_sequence`. `AppState` then calls the same
`open_nested_sequence` path used by the egui timeline, preserving sequence
navigation stack behavior.
Sequence navigation follows the same action boundary, but it is not exposed as
a top-level menu bucket. Nested-sequence return, active-default selection,
sequence creation, switching, duplication, deletion, and settings belong in
sequence-local surfaces such as the timeline context menu, sequence selector,
or dedicated settings affordance. Those surfaces should still emit the
`ui.sequence.*` app-shell actions and derive availability from the current
`AppState` snapshot so the UI does not offer a parent-return command when no
nested sequence is open or a destructive delete command when only one sequence
exists.
Timeline clipboard context-menu entries use
`TimelineEditCommand::CutSelection`, `CopySelection`, `PasteAtPlayhead`, and
`DuplicateSelection`, then map to the existing app-level `Action::Cut`,
`Action::Copy`, `Action::Paste`, and `Action::Duplicate` paths. Widget and
panel code must not own clipboard state or paste placement rules. Shortcut
labels for these menu rows are supplied by the host adapter from the active
shortcut table; `mondrian-ui-widgets` must not hardcode platform-specific
`Ctrl` / `Cmd` display strings.
Trim-to-playhead menu commands use explicit
`TimelineEditCommand::TrimSelectionInToPlayhead` /
`TimelineEditCommand::TrimSelectionOutToPlayhead` commands; the app adapter
maps them to `ui.timeline.trim_selected_clips_to_playhead`, and `AppState`
resolves the current selection and playhead at dispatch time. This matters for
right-click workflows because the widget first dispatches clip selection before
the menu command is activated; menu items must not freeze stale clip ids when
the menu opens. Host availability for these rows must use the same shared
`app_state_action_enabled` gate as menu shortcuts, including edge-specific
playhead validity; Trim In and Trim Out may differ at clip boundaries.
Roll edit follows the same selected-state boundary through
`TimelineEditCommand::RollSelectedCutToPlayhead` and
`ui.timeline.roll_selected_cut_to_playhead`: the context menu exposes the NLE
operation, the adapter requires exactly one editable selected clip, and
`AppState` resolves the nearest adjacent cut through the shared timeline
editing command. The widget must not compute neighboring clip pairs, source
limits, or ripple semantics locally.
Enable/disable selection follows the same boundary through
`TimelineEditCommand::EnableSelection` /
`TimelineEditCommand::DisableSelection` and
`ui.timeline.set_selected_clips_enabled`, so timeline context menus, future
shortcuts, and scripts share the same selected-clip mutation path.
Mark In / Mark Out shortcuts use `TimelineEditCommand::MarkInAtPlayhead` and
`TimelineEditCommand::MarkOutAtPlayhead`, then route through shared app actions
so timeline and viewer shortcuts can converge on the same command boundary.
Focused Timeline Space uses `TimelineEditCommand::TogglePlayback`, which the
self-hosted adapter maps to `Action::TogglePlay`; Space remains panel-local
rather than a global shortcut so text editing cannot accidentally toggle
playback.
Ruler marker dragging is the explicit-frame counterpart: `TimelineView` owns
only hit testing, pointer capture, and local preview for the in/out marker, then
emits `ui.timeline.set_in_out_point` on mouse release. The self-hosted adapter
does no sequence mutation; `AppState` consumes the typed payload and calls the
active sequence's `mark_in(frame)` / `mark_out(frame)` methods so normalization,
project synchronization, and autosave remain centralized.
Clearing the range follows the same path through
`TimelineEditCommand::ClearInOutPoints` and `ui.timeline.clear_in_out_points`;
widgets and panel adapters do not clear sequence fields directly. The top-level
Edit menu also uses this action and enables it only when the active sequence has
an explicit in or out point, so global menus, context menus, and future shortcut
bindings stay on the same AppState-owned mutation path.
The timeline context menu is implemented inside `TimelineView` with the shared
`ContextMenu` overlay component, but it still emits only the same
`TimelineEditCommand`s and track-add proposals as keyboard and toolbar input.
Right-clicking a clip selects that clip first, then menu actions dispatch
through the existing app adapter; the widget never mutates the sequence itself.
Context-menu row availability should reflect only widget-local target state:
selection-only commands are disabled when the view has no selected clip, Split
is disabled when the playhead does not intersect any clip, and Clear In/Out is
disabled when the view has no active range. Clipboard presence, locked-track
policy, and stale-id validation remain at the host/AppState gate and command
layer.
Modified variants should get explicit semantic commands instead of reusing
plain delete.
Timeline structure mutations that can invalidate ids, such as removing tracks,
must call the app selection pruning helper after the sequence mutation succeeds.
That pruning removes stale selected tracks, clips, masks, and animation
keyframes regardless of whether the mutation came from legacy egui,
self-hosted UI, shortcuts, or scripts.
Panel model adapters should read primary and multi-clip selection through the
same AppState selection queries instead of depending on `SelectionState` fields.
Timeline mutations that move clips should refresh selected clip track metadata
through the selection module; clip id remains the durable identity. Self-hosted
Timeline move actions must call the same semantic AppState command path as
keyboard/menu moves, rather than invoking low-level sequence mutation helpers
directly, so linked selections, undo snapshots, overlap policy, and locked-track
checks stay identical across UI surfaces.
The self-hosted timeline adapter must reject cross-media clip move proposals
before emitting an app action: video clips cannot be mapped onto audio target
tracks, and audio clips cannot be mapped onto video target tracks.
`AppState` must validate the same media-type invariant when handling
`ui.timeline.move_clip`, because shortcuts, menus, scripts, and future panel
surfaces can dispatch the app action without passing through the timeline
widget adapter.
`primary_selected_clip()` resolves the current sequence before returning so
single-target panels do not inherit stale track metadata.
Panel models should also resolve selected clips by clip id when reading a
Sequence so stale cached track metadata does not make selection disappear.
Attached effects appear as inspector rows with enable checkboxes, using effect
instance ids rather than list indices so reorder/remove operations can be added
without changing the widget contract. Removal buttons use the same effect id
payload and stay in the app-layer action protocol rather than deleting from the
widget tree directly. Reorder and remove affordances are icon-only app buttons
backed by designer-authored SVGs and component-level tooltips, preserving the
same control geometry for short and long localized labels.
This lets focus routing, overlay popups, repaint requests, shell runtime
behavior, and editor-state dispatch be validated in the same dock tree that
future panels will use. Timeline migration should reuse this path after
scrollbars, overlays, and property controls are stable.
When a self-hosted shell refreshes panel models from `AppState`, it may rebuild
panel content widgets, but it must preserve dock chrome state. `DockSplitter`
therefore exposes a layout snapshot containing splitter direction/ratio data
only, and `SelfHostedAppRoot::set_models` restores that snapshot before
relayout so app data changes do not reset user-resized panels.
Panel-local state that is not editor data should expose a small typed widget
snapshot and be restored at this same shell boundary. `PanelListState` keeps
filter, selection, and list scroll stable; `ScrollViewState` keeps long
Inspector/Export-style panels from snapping back to the top. Scroll state is
matched by owning `PanelKind` plus per-panel scroll ordinal so future panels can
contain multiple scroll surfaces without relying on widget ids or legacy
compatibility shims.
`ScrollView` keeps its content viewport separate from its scrollbar chrome: when
scrollbars are needed, the child is remeasured and laid out inside the reduced
viewport, while thumbs/tracks paint in the reserved gutter. Wrapped text must
therefore use the post-gutter width for measurement and painting, matching
browser/native scroll containers and preventing glyphs from bleeding under the
scrollbar lane.

Browser-style row panels should use `PanelList` / `PanelListItem` instead of
ad-hoc colored placeholders or one-off row painting. `PanelList` owns local
selection, disabled rows, keyboard navigation, activation, and internal
positive-delta scrolling, but exposes static and value-aware action adapters
plus optional `DragPayload`s so Effects, presets, status rows, and similar
panels can bind to editor state outside the widget crate. Mouse single-click
selects a row, a second click on the same row activates it, and unmodified
Enter/Space uses the same activation path. Focused list keyboard navigation is
limited to unmodified Up/Down/Home/End/Enter/Space; modified chords stay ignored
so panel and workspace shortcut routing remains centralized. Escape clears the
list-local row selection when one exists, and otherwise stays ignored so global
`DeselectAll` can still clear editor selections. Pointer movement beyond the
drag threshold asks the router to begin an internal drag; the router, not the
source widget, owns `DragEnter` / `DragOver` / `DragLeave` / `Drop` delivery so
pointer capture from the source cannot block target panels. Focus loss cancels
an active row drag candidate or internal scrollbar drag and releases the list's
capture; idle lists must not release capture they do not own. Replacing list
items through `set_items()` follows the same rule without an event context: row
drag candidates and scrollbar drags are cleared immediately, and the next routed
event releases any capture the stale interaction owned. List wheel events follow
the same bubbling contract as `ScrollView`: scrolling consumes the event, while
boundary or non-overflow wheel input remains ignored for parent panels.
`PanelListItem` also owns optional tree-row metadata: depth, stable tree id, and
expanded state. Tree rows use SVG chevrons supplied by the app layer, toggle on
row click or Enter/Space, hide descendants when collapsed, and preserve collapsed
ids through `PanelListState` across model refreshes. Tree indentation must come
from this metadata, not from leading spaces in the row title.
Searchable list panels should use `PanelList::with_filter`, which exposes its
filter `TextInput` as a real widget tree child so focus, IME, and keyboard
routing stay framework-owned. Filtering
changes only visible row order; original item indices, row actions, drag
payloads, badges, icons, and disabled state remain the item identity used for
dispatch. The self-hosted Effects panel builds rows from the shared effect
registry and, when a video clip is selected, activates rows through undoable
`AppState::add_effect_to_clip` commands. The app action selects the newly
created effect instance after the mutation so Inspector and Node Graph
immediately target the same effect; the selection update is navigation state and
does not create a second undo entry. Inspector effect sections use the same
selection protocol: clicking a section header or background dispatches
`ui.inspector.select_effect`, while child controls keep owning their own
toggle/reorder/remove/property-edit events. This keeps Inspector and Node Graph
navigation synchronized without treating effect selection as an undoable
timeline mutation. Project lifecycle state
belongs to the shell and File menu surfaces instead of a dock panel, so editing
workspaces do not expose a separate project-status panel beside creative panels.
The lighter `List` widget remains available for demos and simple generic
surfaces; it follows the same unmodified-key navigation and activation contract
so Ctrl/Shift/Alt/Meta chords can continue through the central shortcut path.
The self-hosted Assets panel uses the dedicated `AssetGrid` card browser
instead of the row-list surface. `AssetGrid` keeps the same framework-owned
interaction contract as `PanelList`: filter input is a real `TextInput`, local
selection is preserved through `AssetGridState`, cards can activate typed
actions, card drag payloads start through the router, and file drops map to
app-layer import actions. Browser widgets embedded inside a `DockPanel` should
use embedded panel chrome so the dock header remains the only panel title; the
embedded browser body keeps the search field and rows/cards but omits duplicate
title text, explanatory subtitles, and the search-to-content divider. Empty
asset libraries are true empty grids with lightweight text, not disabled fake
asset cards; placeholder cards are reserved for unavailable/error states that
are not valid media library contents. State
restoration uses stable card ids first; index fallbacks are allowed only for
enabled cards that remain visible in the current filtered view, so model
refreshes cannot select hidden assets. Inline rename is transient card-local
state: filtering or model refresh that makes the target card hidden, disabled,
or non-renamable closes the editor without dispatching a rename action. Focus
loss cancels an active card drag candidate and releases the grid's pointer
capture, while an idle grid must not release capture it does not own. It also
owns domain-light right-click context menus:
the grid surface and individual cards receive plain `MenuItem`s, while the
widget handles popup placement, overlay painting, dismissal, keyboard
activation, and dispatch. Menus that contain only separators or disabled rows
must not open; multi-selection menus fall back to the card menu when they have
no activatable rows, and card menus with no activatable rows fall back to the
grid menu without changing the current selection. `AssetGrid` also exposes
item-level drop callbacks:
the widget reports the target card view model, while panel adapters decide
whether a given payload means move, import, or no-op. For drop callbacks,
`None` means "not handled; allow fallback", while `Action::NoOp` means "handled
without an editor action" so self-drops can consume the event without polluting
the app action stream. The self-hosted app
adapter maps the grid menu to
`app_shell` import requests and `ui.assets` create actions for adjustment
layers, solid-color assets, folders, and folder-aware imports. File drops
inside the asset browser emit `ui.assets.import_files`; right-click import
dialogs carry the same target folder and resolve to that action after the
native file picker returns.
Asset selection is local browser state owned by `AssetGrid`. Single click
selects one card, Ctrl-click toggles cards, Shift-click or Shift-key navigation
selects a visible range from the anchor, and Ctrl+A selects all enabled cards in
the current filtered view. Arrow/Home/End navigation ignores Ctrl/Alt/Meta
chords, while F2 rename and Enter/Space activation are unmodified-key gestures
so global shortcuts remain centralized. Escape clears this local browser
selection when one exists; with no local selection it stays ignored so the shell-level
`Action::DeselectAll` fallback can still clear editor selections. `selected_index`
remains the primary keyboard/focus item while `selected_indices` stores the
multi-selection set. Right-clicking an unselected card may update this local
selection before opening the card menu, but it must not dispatch the card's
normal select action during the popup-opening event; host-level action refresh
would otherwise rebuild the panel tree and drop the just-opened context menu.
When a selected card starts a drag, `AssetGrid` can
aggregate selected asset and folder card payloads into
`DragPayload::AssetSelection`; the Assets panel maps that payload to
`ui.assets.move_selection` so moving a multi-selection into a bin publishes one
app-layer operation. `AppState` must prevalidate every target asset and folder
reparent, including folder-cycle rejection, before applying any member of that
batch so a failed multi-selection move cannot leave assets or bins partially
moved. Timeline drops intentionally still accept only single-asset payloads
until the timeline insertion UX defines ordering and track placement for
multiple assets.
Adjustment-layer and solid-color creation actions also carry the current
browser folder id as `folder_id`, while folder creation uses `parent_folder_id`.
Creating any asset-browser item inside a bin therefore remains an app-layer
library mutation while the widget stays domain-light. Global imports from the
File menu or window-level drop fallback still use `Action::ImportMedia` and
import into the root/unfiled view because they have no active asset-browser
folder context. The app adapter maps `AssetRecord` into `AssetGridItem` view
data and semantic media color tokens; the widget crate does not depend on the
asset library or editor domain. The root Assets view is not a flat dump of
every database row: it shows top-level folders first, then root/unfiled assets.
Asset card body text is intentionally terse: the lower card strip shows only
the asset or folder name, while media kind, folder item counts, offline state,
and proxy state use compact badges. Folder item-count badges count direct child
assets plus direct child folders, without recursively flattening nested bins
into the visible grid.
Asset cards may receive an optional `RasterImage` thumbnail. The thumbnail is
an already-decoded RGBA payload with a stable atlas key; `AssetGrid` only
validates dimensions, clips it to the card preview region, and forwards it to
the renderer. Loading and failed thumbnail states are represented as card
preview geometry rather than text or font glyphs, so users can distinguish
queued decodes and offline/bad media without platform font dependencies. Asset
discovery, video-frame decoding, cache invalidation, and filesystem metadata
remain in app/media layers so the project media library does not become a
filesystem browser or media decoder. The self-hosted panel adapter accepts
thumbnail lifecycle data through `AssetThumbnailSource`, which lets a host-owned
cache or future background thumbnail queue feed cards without adding media
dependencies to `mondrian-ui-widgets`. `SelfHostedUiHost` owns the current
`AssetThumbnailCache`: model refreshes request missing video thumbnails without
blocking, a background worker decodes bounded RGBA frames through
`mondrian-media`, and the window event loop polls completions before repainting.
Asset cards expose compact `AssetGridItem::badges` for both media kind and
project-media status. The app adapter maps domain facts such as video/audio
kind, offline file paths, and proxy mode into badge labels plus semantic badge
tones; `AssetGrid` owns badge search participation, right-aligned preview
layout, clipping, and painting through theme status tokens. This keeps status
visible in the project media browser without turning widgets into asset-library
or filesystem readers.
Asset cards expose `ui.assets.delete_asset` through their card-level context
menu. `AppState` owns the actual deletion, including timeline cleanup for clips
that referenced the asset, event publication, status hints, and project save.
File-backed video/audio cards also expose `app.shell.reveal_in_file_manager`.
That command is intentionally a shell/platform side effect: the card emits a
stable app-shell payload containing the real filesystem path, the self-hosted
shell resolves it through `PlatformService::reveal_in_file_manager`, and no
editor action enters `AppState` or undo/redo. Synthetic assets such as solid
colors and adjustment layers do not receive this menu item because they have no
native file-manager target.
Offline video/audio cards additionally expose `app.shell.relink_asset_dialog`.
The shell owns the native replacement-file picker, then converts the selected
path into `ui.assets.relink_asset`. `AppState` owns the actual asset-library
mutation, project save, reload event, and status hint; widgets never call media
probing or filesystem mutation APIs directly.
Online video cards expose `ui.assets.set_proxy_mode` from the same card menu.
The panel adapter derives the checked/unchecked command from
`AppState::proxy_mode_assets`, while `AppState` owns persistence and background
proxy generation. Offline media must be relinked before proxy generation is
requested, keeping media-task side effects outside reusable widgets.
Asset and folder cards support inline rename through `AssetGrid`'s domain-light
editing session. The widget owns F2/title double-click editing, TextInput
focus/IME routing, commit/cancel behavior, and dispatching a rename callback
with the new title. The self-hosted Assets adapter maps that callback to
`ui.assets.rename_asset` or `ui.assets.rename_folder`; `AppState` performs the
library mutation, project save, reload event, and status hint. Card context
menus expose the same rename flow through `MenuItem::local`, so choosing Rename
starts the component's existing inline editor instead of dispatching a partial
app action with no edited text. `ContextMenu` still dispatches normal
`MenuItem::new` actions directly; local commands are intentionally interpreted
only by the component that created the popup. When inline rename commits or
cancels, `AssetGrid` sends `FocusLost` to the temporary `TextInput`, disables
IME through the normal text-input request path, and returns focus to the grid so
follow-up keyboard navigation or F2 editing does not depend on router stale-node
cleanup.
Folder cards expose `ui.assets.delete_folder` through the same card-level menu
surface. Folder deletion is an app/library mutation: `AssetLibrary` removes the
selected folder subtree, unlinks assets assigned to any deleted folder, and
publishes an asset-library reload through `AppState`. The self-hosted host then
revalidates its shell-local browser folder id during refresh and returns to the
root asset view if the current bin no longer exists.
When multiple asset/folder cards are selected, `AssetGrid` asks the app adapter
for a selection context menu. The self-hosted Assets adapter maps that menu to
`ui.assets.delete_selection`, so bulk deletion keeps timeline cleanup, asset
events, one library reload, project save, and status reporting in `AppState`
rather than dispatching a burst of widget-owned single-item commands.
Focused asset grids route Delete/Backspace through the same selection-menu
action factory rather than the global timeline-oriented `Action::DeleteSelection`,
so keyboard deletion and right-click deletion stay on the same typed asset
payload path.
Asset and folder cards can be dragged within the asset browser. Dropping an
asset on a folder card emits `ui.assets.move_asset` with that folder as the
target; dropping a folder on another folder emits `ui.assets.move_folder`.
Dropping either payload on empty browser space moves it to the currently viewed
folder, or to the root view when browsing all assets. `AssetLibrary` validates
missing targets and rejects folder cycles, so the widget layer never owns
library graph integrity.
Assets already assigned to a folder are counted on that folder card and appear
only when the self-hosted shell is browsing that folder. Folder navigation is
shell-local UI session state: folder cards emit `ui.assets.open_folder`, the
host validates the target folder against the current `AssetLibrary`, updates
`SelfHostedAppRoot::asset_folder_id`, and rebuilds panel models with that view
filter. The folder browser path is intentionally not stored in `AppState` and
does not participate in undo/redo.
`AppState::status_log` remains a bounded internal history fed by
`set_status_hint`, but it is not presented as a product dock log panel. Transient
status hints surface in the shell-owned bottom status bar together with preview
buffering, active export progress, and the current sequence/project context.
Runtime diagnostics stay on the tracing/logging path and developer preferences.
Real product panels keep single-click row selection local to the widget unless
the app has a stable domain selection to update; file commands, asset drags,
and effect insertion are emitted only through activation actions.
Product-shell accent colors for asset kinds, timeline fallback clip colors,
effect categories, status errors, and node-graph source/output nodes are
semantic `ColorTokens`. Panel adapters may map domain enums such as
`AssetKind` or `EffectType` onto those tokens, but should not introduce
panel-local hex literals for UI chrome. Reusable list rows use
`PanelListBadgeTone` for compact metadata/status badges so Effects, presets,
and future browsers can share neutral, accent, success, warning, and error
treatments without hard-coded colors or fixed-width text assumptions. Demo
fixture colors that represent clip media content may remain fixture data,
because they are not theme chrome.
The dark theme maintains a deliberate shadcn/zinc-style neutral surface ladder:
`background` and `card` form the normal app/dock floor in the current dark
preset, `panel_alt` is reserved for timeline/tool chrome or other intentional
bands rather than every dock header, `surface` / `surface_2` are compact control
surfaces, `viewer_stage` is the darker preview workspace, `canvas` is the fitted
sequence-frame fill, and `popover` / `border_strong` are reserved for elevated
overlays. Widgets may compose semantic tokens with shared paint helpers such as
`mix_color`, but should not flatten large editor areas into blue-gray
`card`/`popover` fills or reveal darker app gaps between a dock header and its
content. This keeps the self-hosted UI closer to professional NLE workspaces and
prevents visual hierarchy from depending on per-panel ad-hoc color constants.
The default dark preset is calibrated around a neutral black/zinc editor
workbench: `background`, `card`, `panel_alt`, `surface`, and `surface_2` should
step upward by small but visible amounts without drifting into blue slate.
`primary`/`ring` carry only the blue focus, playback, playhead, ruler range edge,
and transient drop-target language. Ordinary active controls, selected browser
rows/cards, dock-tab underlines, track-selection tints, and slider/checkbox
filled states use low-alpha foreground or neutral surface tokens instead of
primary blue. `primary` is not a license for broad blue fills; large selected
timeline/header regions should use neutral low-alpha composition so the
workspace remains dark and readable. Scrollbars and timeline range navigators
use pre-mixed opaque RGB where overlapping geometry would otherwise double-blend
semi-transparent whites.
Reusable browser surfaces such as `PanelList` and `AssetGrid` follow the same
rule: their panel body is a dark workspace mix and selected/hovered states tint
the item fill instead of painting the entire item with raw `accent` or `muted`.
`PanelList` rows stay lightweight: normal rows paint no persistent background,
hover/selection are the only row fills, and there is no strong border or
selection-side stripe. Selected rows use a subdued neutral foreground fill and
checked state should be expressed by explicit row content, such as a checkmark,
rather than a decorative blue block. Asset-grid preview wells should read as
dark media slots even before thumbnails arrive, so placeholder gradients and
badges do not dominate the panel. Embedded browser margins should align to the
same compact inset so Assets and Effects do not appear to belong to different
layout systems inside a shared dock group.
Panel models must attach explicit stable actions to rows instead of deriving
commands from titles, indices, or fixture-only prefixes. Synthetic demo rows
use explicit `ui.demo_panel` actions so they exercise the same select/activate
contracts as product panels without being confused with app-layer `ui.assets`,
`ui.effects`, `ui.timeline`, or `ui.inspector` protocols.
The default editing dock follows a conventional NLE shape: Assets/Effects,
Viewer, and Inspector occupy the upper workspace, while Timeline owns the full
bottom span. Project commands remain in the shell/menu layer, and export uses
its own workspace/panel instead of sharing a status/log tab group.
The editing preset keeps the left browser narrow, gives the center viewer the
largest share of the upper workspace, and leaves the inspector at a compact
right-column width. Dock headers share the same panel-body surface as their
content and do not draw a bottom divider between tab chrome and panel content.
Single-tab dock headers should paint as panel titles with a small neutral active
underline, not as full-width raised tabs; grouped browser tabs may keep larger
hit areas but should use hover fills and underline selection rather than heavy
active rectangles.
Dock tab labels are content-measured from the current display text with tokenized
padding, min/max widths, and the small tab-label typography token. Assets,
Effects, Inspector, Timeline, and future panels must not reserve equal-width
tabs just because they share a dock group; tab width follows the label while
hit targets stay large enough for normal pointer use. Panel chrome text is
secondary editor chrome and should stay visually quieter than panel content,
viewer controls, or timeline editing affordances.
Self-hosted `FocusPanel` actions activate the matching dock panel or grouped tab
through shell-local dock traversal and do not continue into `AppState`. The
traversal first understands grouped tabs in the default layout, then falls back
to direct panels used by built-in workspace presets. If the active dock tree
does not contain the requested panel, the shell switches to the panel's
preferred built-in workspace and activates it there, so focus shortcuts never
silently no-op.
Window-menu panel rows use `TogglePanel`. Direct dock-panel leaves, such as Viewer,
Timeline, Inspector, Assets, Node Graph, and Export, hide by removing the panel
leaf from `SelfHostedWorkspaceLayout`, collapsing now-empty split branches, and
promoting the root to `WorkspacePreset::Custom`. If toggled again while absent,
the shell restores the panel by switching to its preferred built-in workspace.
Grouped tabs that are not independent layout leaves, currently Effects inside
the Assets browser, participate in the same Window-menu contract through
panel-leaf `hidden_tabs` metadata. Hiding Effects keeps the Assets leaf and
filters only the Effects tab; toggling it again detects that the grouped tab is
absent and restores the preferred Editing workspace with Effects active.
The Window menu exposes that shell-local state through checked rows:
panel rows are checked only when the current live layout contains the direct
panel or grouped tab, while built-in workspace rows are checked only when that
named preset is active. Custom layouts intentionally leave built-in workspace
rows unchecked instead of pretending to be Editing.
Window-menu checked state follows the generic menu rule: visibility is shown by
the checkmark glyph only. It must not introduce blue checked backgrounds, left
accent bars, or per-row icons, because the menu is a state report rather than a
primary editing surface.
Self-hosted `SwitchWorkspace` is also shell-local: it rebuilds the dock tree
from the current `SelfHostedPanelModels` using named built-in preset factories
while keeping panel models read-only and app/domain mutation in `AppState`.
Editing prioritizes full-width timeline work, Color keeps Viewer/Timeline on
the left with Inspector/Effects on the right, Audio gives Timeline the lower
workspace, Compositing groups NodeGraph/Effects opposite Viewer/Inspector, and
Export pairs export settings with the Viewer. Refreshing panel models must
preserve the selected workspace preset so live app snapshots do not silently
reset the user's shell layout.
`WorkspacePreset::Custom` is a real persisted workspace, not an alias for
Editing. Its layout is stored as `SelfHostedWorkspaceLayout`: a binary tree of
split direction/ratio nodes and dock panel leaves with explicit `tabs:
Vec<PanelKind>`, active tab indices, and legacy grouped-tab visibility metadata
that live widgets cannot infer on their own.
`self_hosted::panels` is the only layer that materializes that schema back into
`DockSplitter` / `DockPanel` widgets from current `SelfHostedPanelModels`.
Loading preferences sanitizes custom ratios and active tabs before a root is
built, including clamping active tabs after hidden grouped tabs are applied.
The layout schema already models Premiere-style panel relocation through
`DockDropArea`: dropping onto the center inserts the dragged panel as the active
tab of the target group, while edge drops create a split around that target
group. Drag preview, hit testing, and persistence belong at the shell/widget
boundary; panel contents must remain ordinary `PanelKind`-addressed widgets
instead of inventing per-panel docking APIs.
Dragging a built-in workspace splitter promotes the root to Custom when
the split layout diverges from the built-in preset snapshot; the winit window
runner asks `SelfHostedUiHost` to persist that layout on left-button release or
focus loss, not during every mouse-move frame. TogglePanel-driven leaf removal
uses the same persistence path after the queued shell action drains. Built-in
presets remain template factories and can always be selected again to reset the
visible dock tree without deleting the saved Custom layout.
The self-hosted Assets panel maps real library cards to `ui.assets.prepare_drag`;
`AppState` resolves the asset record and reuses the existing `begin_drag_asset`
path so later Timeline drop handling stays shared with the egui implementation.
The same left dock hosts the Effects browser as an `Effects` tab so effect
insertion remains visible without changing the default split layout.
Asset cards are fixed-size browser cells. Their preview well uses a 16:9 aspect
ratio and a black background; raster thumbnails must aspect-fit inside that well
without overflowing, leaving black letterbox/pillarbox space as needed. Media
kind badges are app-layer Chinese labels, such as 视频, 音频, 图片, and 序列,
until the product has a real i18n layer. The footer is a single row with the
asset name left-aligned and duration right-aligned; both lanes elide when space
runs out, and the full value belongs in a tooltip rather than a taller card.
`AssetGridState` preserves hover by stable id across model refreshes because
tooltip timer redraws and shell model rebuilds should not make the hovered card
blink off while the pointer is still stationary over it.
Future browser zoom should be an explicit grid scale control or Ctrl-wheel
gesture, not implicit responsive card resizing.
The Effects browser should read like a professional NLE effect library: a
compact multi-level category tree with plain category rows and effect rows.
Its row height can be tighter than general-purpose browser lists because effect
libraries are expected to contain hundreds of entries; the app adapter should
use `PanelList::with_row_height(30.0)` or a similarly compact tokenized value
instead of large card-like rows.
Rows in this panel should not carry FX icons, GPU/3D/FX badges, or descriptive
subtitles; those richer labels belong in documentation, search metadata, or a
future inspector/help surface, not the dense effect list.
Effects categories are interactive `PanelList` tree nodes, not disabled
placeholder rows, so users can collapse and expand the library while effect rows
remain the only rows with apply actions.
Panel content factories must cover every `PanelKind` explicitly. Product shell
fallbacks should be disabled `PanelList` empty states that name the unsupported
panel, not anonymous colored boxes, so missing migrations stay visible and new
panel kinds force an intentional mapping.

Timeline migration starts with the domain-light `TimelineView` surface in
`mondrian-ui-widgets`. It renders frame-space tracks, clips, ruler ticks,
playhead, vertical/horizontal scrolling, Ctrl-wheel zoom, clip selection, and
seek actions, but it does not depend on `mondrian-timeline` or mutate editor
state directly. Real timeline panels should map `Sequence` / `Track` / `Clip`
data into `TimelineTrack` / `TimelineClip` view models, then translate
selection and seek callbacks into semantic `Action`s or undoable commands at
the app layer. This keeps the renderer-facing timeline primitive testable while
preserving a clean path for progressively replacing the old egui timeline.
The visual baseline is compact NLE density: 42px default tracks, 28px ruler,
132px minimum track header column, subtle alternating lane fills, weak row
separators, a one-pixel playhead with a small five-sided ruler handle,
persistent 8px scrollbars in reserved gutters with circular endpoint handles, an
explicit magnet Snap toggle in the tool strip, V/A track badges, and clip blocks
with tokenized selected borders plus trim-handle affordances on hover/selection.
Track headers should stay terse: the V1/V2/V3/A1/A2 labels are the track
identity and the badge width should be measured from the label with equal
horizontal padding, while visibility, mute, and lock are icon buttons sourced
from the app SVG icon registry. Do not add persistent explanatory text such as
"video track", "mute", or "locked" inside the compact header row.
Audio clip view models may
carry normalized waveform peaks; `TimelineView` paints them as compact vertical
peak columns without decoding media or owning a waveform cache. Ruler ticks and
snap guides should use low-alpha semantic timeline tick tokens rather than full
panel borders, so time markings and edit alignment cues read without turning
the timeline into a table. Ruler labels should adapt to the visible frame
scale: close zoom levels may show SMPTE-style `HH:MM:SS:FF`, normal edit ranges
can collapse to `MM:SS`, and long ranges should avoid noisy frame labels.
App adapters should pass the active sequence frame rate into `TimelineView`, so
SMPTE labels and major-step thresholds follow real 23.976/25/29.97/60fps
timelines instead of assuming 30fps.
Minor and major tick cadence should also be chosen independently from the
visible frame span: minor marks stay dense enough for scanning, while labeled
major ticks snap to calmer second/minute multiples instead of a fixed
`minor * 4` pattern.
Alternating timeline lane fills, range markers, scrollbar alpha, playhead color,
and selected clip outlines are semantic timeline tokens so compact editor
density remains theme-owned rather than embedded in the drawing code. Timeline
work areas are expressed first as a thin ruler bar, with only optional very weak
track tint and one-pixel boundaries; they must not compete with selected clips.
Range selections, rendered-cache bars, and playhead-previous areas are separate
future semantics and should not reuse the work-area fill.
Timeline Range Navigators are not normal scrollbar thumbs. Their center body
pans the visible time/track range, while the leading and trailing circular
handles resize that visible range and therefore alter timeline zoom or track
density. Paint them as track, body, leading handle, then trailing handle. The
body should visually extend under the full circular handles so the navigator
reads as one continuous Premiere-style viewport block, but state changes must
use theme-owned opaque replacement colors or a true union/mask pass rather than
stacked translucent overlays. Body hover uses a grab cursor, body drag uses
grabbing, and handle hover/drag uses resize cursors. During an active navigator
drag, pointer handling must remain captured even when the cursor leaves the
timeline bounds; the active drag path owns cursor requests until mouse-up or
focus loss. Horizontal handle resizing should solve from the handle's
navigator-track position, including fixed timeline content padding, so circular
handles stay visually anchored under the pointer instead of drifting as zoom
changes.
Timeline tracks expose distinct visual slots for normal, targeted/selected,
locked, muted, hidden, drag/drop-target, and future solo states. The widget can
paint states only when the view model carries real state; do not invent a solo
indicator until the app/domain layer exposes solo. Long clip labels are clipped
inside the clip card and surface their full label through the shared tooltip
manager when hover reveals truncation.
When no timeline model is available, app panels should disable the surface so
empty shells do not steal focus, seek, or hold pointer capture. The app panel
model owns the empty-state reason, such as no open sequence or an empty
sequence, while `TimelineView` only paints the supplied message in the timeline
body and keeps add-track command routing available for sequence-backed empty
timelines. Disabled timeline shells must also mute toolbar chrome and suppress
the playhead so a no-sequence workspace does not present editing affordances as
available. Empty timeline messaging should follow the same restrained product
language as other panels: a short title plus optional body copy, left-aligned in
the upper third of the content area rather than centered like a splash screen.
The timeline body should not paint persistent vertical ruler grid lines inside
every track; time structure belongs in the ruler row, while playhead, in/out
ranges, snap guides, and transient drag indicators provide alignment cues in
the track body.

Value widgets stay editor-state agnostic. `Button`, `Checkbox`, `Slider`,
`TextInput`, `Dropdown`, `ColorPicker`, `ColorPickerTrigger`, and `CurveEditor`
expose action adapters such as `on_click(...)`, `on_change(...)`, or
`on_select(...)`, but they do not know about clips, effects, keyframes, or undo
history. Real panels map widget values to semantic `Action`s or command objects
at the panel/app layer. Programmatic state synchronization uses setters such as
`set_color()` / `set_points()` and must not emit actions; only user input paths
dispatch changes and request repaint.
Adapters that cannot target editor state should return `Action::NoOp` rather
than inventing legacy custom action names; `AppState` dispatch treats NoOp as a
first-class empty action without logging it as an unimplemented command.
Focused sliders handle arrow/Page/Home/End value changes locally only for
unmodified keys and Shift large-step variants. Focused number inputs follow the
same rule for Up/Down/Page nudging before delegating other keys to their inner
`TextInput`. Ctrl, Alt, and Meta chords stay ignored by value widgets so
workspace shortcuts, input methods, and user-level tool hotkeys remain
centralized outside the component. Programmatically disabling a focused number
input ends the edit session before any further key handling: the wrapper focus
flag is cleared and pending invalid display text is normalized back to the most
recent valid value. Sliders use a compact neutral visual: a thin track,
foreground-filled progress and thumb, and neutral surface track background
rather than primary blue. Slider focus loss clears keyboard focus and an active
drag if present, but it must not release pointer capture when the slider was
only keyboard focused.
Self-hosted Inspector actions should use typed payloads for clip mutations.
Scalar clip fields that need both coarse and precise editing, such as opacity,
transform values, and trim frames, compose `Slider` plus `NumberInput` in the
panel adapter. Both controls emit the same typed inspector action, so the app
layer receives one mutation contract independent of the user's edit gesture.
The curve editor currently emits `ui.inspector.set_clip_curve` with normalized
points; AppState maps them to opacity keyframes over the selected clip's
timeline span so curve edits participate in undo/redo and render evaluation.
Effect property rows are adapter-owned: bools, scalar numbers, colors, text,
and Vec2/Vec3/Vec4 values render as typed controls in the self-hosted Inspector,
then dispatch `ui.inspector.set_effect_property` with the full `PropertyValue`.
Numeric scalar rows and stacked vector components use the same
`Slider` + `NumberInput` composition as clip properties, preserving untouched
vector components in the emitted payload. Numeric effect rows pass descriptor
min/max/step metadata into both controls; integer properties default to unit
steps so typed, keyboard, and dragged values stay on the same grid as the
underlying effect property rather than relying on lossy float-to-int truncation.
The adapter must sanitize descriptor numeric bounds before constructing controls
or clamping emitted values; plugin-provided NaN, infinite, missing, or reversed
min/max values are not allowed to panic the Inspector.
Inspector clip mutations are validated at the AppState boundary, including
locked-track protection; widgets stay domain-light and do not decide whether a
clip can be edited.
Inspector panel models still expose edit availability from the current
`AppState` snapshot. When the selected clip's track is locked, the self-hosted
Inspector remains readable but disables clip style, transform, timing, effect,
property, and curve controls before they can dispatch actions. This is UI
affordance only; `AppState` keeps the authoritative locked-track validation.
When no clip is selected or no sequence is open, the app model must expose an
empty-state message and the self-hosted Inspector should render only a compact
status section rather than default clip-style, transform, timing, or animation
controls. Empty inspectors must not imply a real editable target through
placeholder parameter values.
Inspector panel models derive the displayed curve from those opacity keyframes,
falling back to a flat curve at the evaluated opacity when no animation exists.
When existing keyframes do not land on clip boundaries, the panel model
synthesizes endpoint samples from evaluated opacity and keeps interior
keyframes at their normalized positions.
`TextInput::on_change(...)` dispatches only when committed text changes, such
as typed text, paste/cut/delete edits, or IME commit. Cursor movement,
selection changes, and IME preedit updates remain local so form bindings do not
receive noisy non-mutating actions.
Winit keyboard and IME conversion lives in the self-hosted shell runtime so
`ui_demo` and product windows share the same `KeyDown` / `TextInput` /
`ImePreedit` / `ImeCommit` / `ImeCancel` semantics. Runtime conversion maps
winit `Ime::Disabled` to explicit `ImeCancel` rather than an empty preedit
sentinel; focused text widgets clear local composition without dispatching form
changes or committing text. Entry binaries route Escape through the widget tree
first and must not treat ignored Escape as a native window close; quitting
remains an explicit shell command or platform close request.
The runtime emits printable `TextInput` only when Ctrl and Meta are clear; Shift
remains allowed for uppercase and symbol input, and Alt-only text remains
allowed when winit reports printable text so AltGr-style keyboard layouts do not
lose characters. Ctrl, Ctrl+Alt, and Meta shortcut chords therefore reach
widgets and the central router as `KeyDown` without also inserting text into
focused fields. Space must be recognized both as
`NamedKey::Space` and as `Key::Character(" ")` so playback/timeline shortcuts
remain platform-stable while focused text fields can still receive a printable
space through `TextInput`.
Entrypoints must also consume `WindowEvent::ModifiersChanged` through
`winit_modifiers_to_ui_modifiers`; key-edge tracking is only a fallback for the
current keyboard event and must not be the sole source of modifier state. The
fallback still tracks Super/Meta/Hyper as the UI `meta` modifier so OS and tool
shortcut chords do not drift into printable text handling if a platform delivers
keyboard edges before a fresh modifier snapshot. The same fallback maps
`NamedKey::AltGraph` to the UI `alt` modifier while preserving Alt-only
printable text routing for keyboard layouts that emit text through AltGr-style
input.
When a winit window reports `WindowEvent::Focused(false)`, entrypoints must
route `UiEvent::FocusLost` and reset the tracked modifiers to
`Modifiers::none()`. The router treats that as a window-level blur: active
drags are cancelled, capture and hover are released, focused widgets receive
`FocusLost`, IME is disabled, and tooltip state is hidden.
Internal drag/drop has the same router-owned lifecycle. Once a widget requests
`DragRequest::Begin`, the router owns the active payload, routes movement as
overlay-first `DragEnter` / `DragOver` / `DragLeave`, converts left-button
release into a single `Drop`, and ends the drag session even when the target
ignores that drop. Widgets may paint hover affordances and dispatch
domain-light drop proposals, but they must not rely on a second cleanup event
after `Drop` to terminate the drag.
Pointer and wheel events, including runtime-synthesized pointer events such as
eyedropper polling, must carry the current modifier state tracked by the
entrypoint so timeline zoom, alternate drag modes, and shifted scrolling do not
lose keyboard context.
Winit mouse buttons are converted losslessly at the shell boundary for the
buttons the UI model understands: left, right, middle, back, forward, and
opaque `Other(u16)`. Unknown buttons must not be downgraded to left click,
otherwise side buttons can accidentally activate destructive controls.
OS file drag/drop enters the same UI event model as internal drags via
`DragPayload::File`. Product windows route hovered/dropped files through the
widget tree first; if no widget handles the final drop, the self-hosted app
falls back to `Action::ImportMedia` so dropping media into the window remains a
useful default workflow. `PanelList` and `AssetGrid` expose domain-light
`on_drop` adapters; the self-hosted Assets panel maps file drops to
`ui.assets.import_files` through `AssetGrid`, including the current asset-folder
target. Other panels can opt into their own drop semantics without teaching
generic widgets about application state.
The same boundary applies to asset-browser creation commands: context menu
items dispatch `ui.assets.create_adjustment_layer`, `ui.assets.create_solid_color`,
or `ui.assets.create_folder` with the current asset-folder target, and
`AppState` performs the actual library writes, default naming, folder
validation, persistence, event publication, and status hints.
Shell cursor selection is also centralized in the runtime. Entrypoints provide
the current eyedropper, splitter, and focused-text state; the runtime resolves
priority as eyedropper sampling, splitter resize affordance, focused text
editing, then default cursor.
Self-hosted entry binaries should collect widget-dispatched actions during
event routing, then drain them after the root borrow ends. Shell-local actions
such as the new-project dialog mutate `SelfHostedAppRoot`; only confirmed
project creation emits the editor-facing `ui.project.create_with_settings`
action consumed by `AppState`.
Diagnostic/demo containers that manually route child events must still follow
the same event contract as `EventRouter`: visible overlays get first priority,
ordinary pointer events go only to hit-test targets, captured widgets receive
their drag/move/up stream, and keyboard/text input follows focused widgets.
They must not broadcast pointer events to every child, because that couples
independent component state and hides real scroll/dropdown regressions.
The pending queue lives in `self_hosted::action_queue`; `SelfHostedUiHost`
drains it after routing and applies shell/AppState refresh policy. Entry
binaries should use these types rather than open-coding shell/AppState
dispatch. The host refreshes panel models after every dispatched editor action,
including actions that return an error, because action handlers may still update
status hints or other user-visible state before reporting the failure.
`SelfHostedUiHost::drain_pending_actions` also returns window-host commands
such as quit and toggle-fullscreen. Entrypoints apply those commands only after
event routing and model refresh have completed, so native side effects stay out
of widget code and out of `AppState`. Native platform close requests must enter
the same pending-action queue as `app.shell.quit`; entrypoints must not call the
event-loop exit primitive directly from `WindowEvent::CloseRequested`.
Editor actions dispatched from the self-hosted host must surface failures in
the status bar. `AppState` action handlers should set specific localized
`status_hint` errors when they can explain the failing workflow. If an action
returns an error without setting a new error hint, `SelfHostedUiHost` writes a
generic `操作失败：...` fallback so failures are visible without opening a
diagnostics panel. The fallback must not overwrite a newer action-specific
error produced by `AppState`.
Self-hosted UI scale coverage lives in the ignored
`self_hosted_ui_scale_smoke` test:
`cargo test -p mondrian-app self_hosted_ui_scale_smoke -- --ignored --nocapture`.
It constructs a real SQLite-backed asset library and a long synthetic
`AppState`, then exercises panel-model snapshotting, cold root build, repeated
refresh, resize loops, draw-command emission, and sustained playback refresh
without requiring real media files or a GPU surface. The default scenario uses
hundreds of assets, clips, and effect nodes; `MONDRIAN_UI_PERF_ASSETS`,
`MONDRIAN_UI_PERF_CLIPS`, `MONDRIAN_UI_PERF_EFFECTS`,
`MONDRIAN_UI_PERF_RESIZE_ITERS`, `MONDRIAN_UI_PERF_PLAYBACK_FRAMES`,
`MONDRIAN_UI_PERF_REFRESH_ITERS`, and the matching `*_MS` threshold variables
can scale the smoke for baseline/current comparisons. Successful runs print
`MONDRIAN_PERF_JSON` and append it to `MONDRIAN_PERF_OUTPUT` when configured.
The native window surface lifecycle is centralized in the self-hosted window
session. Zero-sized resize events, such as minimize transitions, must not
reconfigure the surface or relayout the root. Real size changes reconfigure the
surface, update root bounds, relayout, and request a redraw. Same-size resize
events are no-ops, while `ScaleFactorChanged` always relayouts and redraws so
DPI-dependent geometry can settle even when the physical surface size is
unchanged. `SelfHostedFrameRenderer` treats lost/outdated surfaces as
`Reconfigured`, schedules a follow-up redraw, skips timeout/occluded frames, and
requests a deterministic follow-up frame after text or raster atlas uploads.
Shell chrome that presents transient project status should keep error feedback
visible without requiring the user to scroll a compact dock panel. Dock panels
should stay focused on editing surfaces rather than general project diagnostics.
The workspace root reserves title-bar and status-bar height before laying out
the dock tree; panels must not assume they own full-window coordinates.
Registered self-hosted UI action namespaces are strict protocols: known
namespaces with unknown action names return workflow errors instead of being
silently ignored, so widget/app wiring mistakes fail during development.
The new-project dialog edits the real `SelfHostedNewProjectDraft` settings via
typed draft-update payloads and presents validated production presets for frame
size, frame rate, audio sample rate, proxy generation, and preview caching.
Shell modals are routed through `self_hosted::modal::ShellModal` and should use
theme modal tokens such as `colors.modal_scrim`, `colors.popover`, and spacing
radii instead of per-dialog hard-coded chrome. Each concrete modal lives in its
own module, such as `new_project_dialog` or `about_dialog`, while
`SelfHostedAppRoot` only opens, closes, lays out, and routes the active modal.
When a modal is active, the root must treat it as a top-layer input boundary:
events ignored by the modal are still handled by the root and must not fall
through to menu, dock, panel, or shortcut behavior behind the scrim.
`ShellModal` exposes a full-window overlay hit boundary so stale sibling
overlays, such as an already-open menu dropdown, cannot intercept input above
the active modal.
The root exposes children in bottom-to-top z-order for routing and overlay
painting: dock content, status bar, menu/title bar, then the active modal.
Normal painting should follow the same order so visual stacking and interaction
stacking stay aligned.
Modal card geometry and chrome should be centralized through
`mondrian-ui-widgets::DialogSurface`; app dialogs should keep only local content
layout and event semantics.
Dialog and form copy should use reusable `mondrian-ui-widgets::Label`
instances for semantic color, padding, and wrapping rather than direct
per-dialog `draw_text` calls.
Inspector/property-panel titles, section headers, and row labels follow the
same rule through `PropertyPanel`'s internal `Label` instances. When the
Inspector is hosted inside a dock panel, it uses embedded panel chrome so the
dock header supplies the panel name and the property stack starts directly at
its first meaningful section. Empty inspector states should use a dedicated
panel-level empty-state layout rather than a fake one-row form section, so
wrapped guidance copy can sit at a stable top offset without row clipping.
That empty-state rhythm should stay close to the Assets panel: quiet left-aligned
title/body copy anchored in the upper third of the panel instead of a centered
placeholder card.
Inspector sections should read as a professional parameter stack, not nested
cards. `PropertyPanel` paints the panel body from the normal panel token, uses
thin section dividers, and reserves stronger chrome only for the selected
section's restrained selection tint. Standard property rows are 30px tall, with labels
vertically centered in compact rows and pinned near the top of tall rows by
`FormLayout`; oversized curve editors or color pickers can opt into explicit
taller row heights while staying clipped to their row/control rects.
`PropertyPanel` clips each row and its form-control rect during normal paint so
oversized controls cannot leak across inspector rows or outside a `ScrollView`.
Dropdowns, color-picker popups, context menus, and tooltips that must escape a
panel should render through `paint_overlay` instead of ordinary row paint.
Reusable labeled-field geometry should flow through
`mondrian-ui-widgets::FormLayout` / `FormRowOptions` instead of each component
recalculating label and control rectangles independently.
`FormLayout` also owns the measurement constraint for the control lane; property
rows must measure child controls with that bounded lane width and row height
before laying them out. Inspector controls should not be measured with
`LayoutConstraint::LOOSE`, because dropdowns, text inputs, color pickers, and
wrapped labels need the same width during measure and layout.
Form controls should expose a common `enabled(bool)` / `disabled()` builder
where practical. Disabled controls must not dispatch actions, request pointer
capture, or participate in focus traversal, and should render with muted theme
tokens rather than panel-local color constants.

Pixel alignment is applied selectively, not as a global transform. Text,
animated content, and continuous editor geometry may keep subpixel positions;
hard UI chrome such as 1px separators, modal outlines, and splitter strokes
should use `mondrian-ui-widgets::paint` stroke helpers so stroke edges land on
device-pixel boundaries. Components should not hand-roll `round()` formulas for
new chrome.

`mondrian-ui-widgets` keeps extreme interaction and visual-command stability in
normal Rust tests. The component stress suite drives edge-size layouts, long
text, dropdown wheel scrolling, pointer-captured slider drags, color-picker
popovers, context-menu overlays, scroll containers, panel lists, viewer chrome,
and timeline scroll/zoom, then asserts that generated paint geometry is finite
and clip/transform stacks remain balanced. This does not replace human visual
QA, but it catches common regressions before manual desktop testing.

General panel composition should use `FlexContainer` rather than ad-hoc
coordinate code. `FlexContainer` is only a widget adapter over the pure
`mondrian-ui-layout::FlexLayout` algorithm, so layout math remains testable in
the layout crate while panels get normal widget-tree behavior: event routing,
overlay forwarding, hit testing, and child traversal.
List-style panel rows use `PanelListItem` with optional `VectorIcon` geometry
for command and asset affordances. App panels must source those icons from
`self_hosted::icons::AppIcon` and pass only parsed vector geometry into the
generic widget layer.

## Viewer Surface

The self-hosted Viewer panel uses the domain-light `ViewerSurface` widget
instead of a colored placeholder. App code maps `AppState` / `Sequence` into a
small `ViewerPanelModel` containing title, playback status, current frame,
duration, source resolution, and an optional `ViewerFrameImage` alias over the
shared `RasterImage` payload. The widget owns preview chrome,
source aspect-ratio fitting, raster-image presentation, tokenized abnormal
status-badge tones, empty-canvas messaging, metadata labels, and safe-area guide
drawing only; frame decoding, preview scheduling, and GPU texture lifecycle
remain app/runtime responsibilities.
Viewer painting treats the fitted sequence frame as the only real image canvas.
The panel body uses the dedicated `viewer_panel` token, while the surrounding
preview stage uses `viewer_stage` as a future pan/zoom workspace. The fitted
sequence frame uses `canvas` as its empty-frame fill, so the user can distinguish
the workspace from the actual sequence rectangle. The sequence frame itself is a
straight-edged rectangle; empty projects omit the checkerboard and show only the
dark stage, neutral canvas fill, and safe-area guides. Transparent content or an
explicit transparent-background toggle may enable the low-presence checkerboard.
Preview frames, empty messages, and safe-area guides are clipped to that
sequence rectangle and then to the viewport, so content outside the sequence
frame is never visible.
Transport buttons and zoom/quality chips use compact toolbar surfaces; the
play/pause button may be slightly more prominent, but viewer controls should not
look like generic form inputs or large rectangular tabs.
Normal viewer titles and healthy statuses, such as sequence names, `Ready`, or
`Playing`, remain model data and are not painted in the panel body by default;
the dock tab and transport/timecode chrome already provide enough context.
Warning/error statuses may surface as a compact badge so exceptional preview
problems remain visible without making every frame look like a status card.
The product host supplies viewer frames through `ViewerPreviewSource`.
`SelfHostedPreviewService` is the app-layer boundary that interprets timeline
render plans, owns compositor scratch state and preview cache keys, and injects
render-ready `ViewerFrameImage` values into `ViewerPanelModel`. The initial
self-hosted path renders solid-color timeline elements directly via the shared
`mondrian-renderer` CPU compositor and requests media frames through a
host-owned background preview worker. Model refresh never blocks on media
decode: missing media frames are queued, `SelfHostedUiHost::poll_background_tasks`
collects completions, and a later refresh composites only when all required
media RGBA inputs are available. Adjustment-layer plans are forwarded to the
same compositor pass once preceding visual layers exist. Nested-sequence plans
render recursively through the same service with a fixed depth guard; if a child
sequence is missing or any recursive media input is still unavailable, the
viewer returns no frame instead of presenting a partial preview as correct
output.
Viewer transport controls are part of this chrome but stay domain-light: the
visible transport strip contains jump start/end, step back/forward, and
play/pause only. Mark In/Out remains a focused keyboard workflow, not a viewer
button, so the preview panel does not duplicate timeline editing controls. The
self-hosted app must source those visible transport icons from
`self_hosted::icons::AppIcon` / bundled SVG assets rather than recreating
product icons in widget drawing code. By default these controls emit shared
editor actions (`GoToStart`, `StepBack`, `TogglePlay`, `StepForward`,
`GoToEnd`), and embedders may override the mapping with a control callback when
a host needs a custom command boundary. The self-hosted app panel must bind that
callback explicitly so the app adapter, not the generic widget, owns the command
boundary for transport controls. Playback state changes, mark semantics, frame
stepping semantics, preview scheduling, and audio/video sync remain in the
app/runtime layers. Zoom and preview-quality
labels are explicit model fields. The Viewer bottom-left metadata lane is kept
to the current timecode; resolution, frame-rate, absolute frame, and duration
remain model data but should not crowd the default transport strip. Viewer zoom is shell-local display state owned
by `SelfHostedAppRoot`; `ui.viewer.cycle_zoom` is consumed before AppState
dispatch, survives model refreshes, and is not undoable because it does not
change the project. The shell injects both zoom label and fixed zoom scale into
`ViewerPanelModel`, and `ViewerSurface` uses that scale as a real display
transform while clipping oversized canvases to the viewer viewport.
Preview-quality interaction emits
`ui.viewer.set_preview_resolution_scale`; `AppState` updates the active sequence
preview settings through an undoable sequence snapshot without stopping
playback, so quality switching remains a viewer operation rather than a
sequence-settings-dialog draft mutation. The preview quality label comes from
the active sequence preview scale (`1/1` at 1.0, percentage labels below full
resolution), not from whether a preview frame has already arrived. Zoom and
preview-quality chips dispatch only when the host provides their callback;
otherwise they keep local press/repaint feedback but must not send
`Action::NoOp` into the app dispatch path. Viewer
transport chrome collapses its visible control set at narrow widths before
allowing buttons to overflow panel bounds.
`ViewerSurface` is focusable when enabled. Pointer clicks inside the viewer give
it focus, while `FocusGained` shows tokenized focus chrome for keyboard
navigation. Focused viewers handle unmodified Space, Left/Right, Home/End, and
I/O through the same control callback used by clickable chrome; modified keys
fall through to the shell shortcut router. Space remains intentionally absent
from global shortcuts so text editing cannot toggle playback. Focus loss and
disabled cleanup clear hover, pressed, and focus-ring chrome and request repaint
when any of those visual states changed, so transport buttons and chips cannot
remain visually pressed after focus or availability changes. Programmatic
viewer disabling must clear the same interaction state immediately so a later
re-enable cannot dispatch a stale mouse release from a previous preview model.
Empty app state maps to a disabled viewer model so the product shell can show
a clipped empty canvas message without pretending a preview texture exists.
`ui_demo` should use the same `ViewerSurface` for the Viewer panel and keep
separate text diagnostics in the Text tab, so visual QA exercises production
viewer chrome instead of a demo-only widget.
Demo-only missing panel content should use disabled `PanelList` empty states,
not anonymous colored rectangles, so visual QA can tell whether a surface is
intentionally absent or accidentally blank.

## Color Input

Color parsing and conversion live in `mondrian-core`, not in the widget layer.
ColorPicker and inspector controls should use the shared HEX/RGBA/HSL/HSV/CMYK
models so text inputs, swatches, and future effect parameters round-trip through
the same math. The custom `ColorPicker` owns HSV area, hue bar, alpha bar,
mode selection, keyboard nudging, eyedropper state, and text-field
synchronization. Color-axis nudging handles unmodified arrows and Shift
large-step arrows only; Ctrl/Alt/Meta arrow chords and modified mode-menu
navigation stay ignored so shortcut routing remains centralized.
`ColorPickerTrigger` wraps a private popup picker and must translate the popup's
pointer capture, focused text-field ownership, and `accepts_text_input()` state
back to the trigger id. Inspector popups therefore keep a router-visible focus
owner while the embedded picker preserves its own active field, IME, and color
editing state.

The renderer exposes gradient rectangle and per-vertex colored triangle draw
commands backed by the existing batch pipeline. Colored triangle fans may carry
an optional rounded-rect mask so non-rectangular gradients reuse the same SDF
antialiasing path as circles and rounded rectangles. Widgets should prefer
these primitives for UI gradients instead of tessellating many sampled
rectangles or adding UI-local shaders. The `ColorPicker` uses gradient
rectangles for its HSV area overlays, hue ramp, and alpha ramp. It uses a
masked colored triangle fan for the optional hue/saturation wheel.
Checkerboards are low-count deterministic colored-triangle geometry provided by
the shared widget paint helpers, and use the same rounded mask path when they
sit inside rounded swatches. Checkerboard light/dark cells are semantic theme
tokens, not widget-local literals, so transparent previews can be tuned for
dark, light, and future high-contrast themes.
The crosshair and slider handles use semantic color-handle tokens rather than
local black/white literals, because they must remain legible over arbitrary
sampled colors while still being adjustable per theme.
`ColorPickerTrigger` wraps the full picker for inspector rows and toolbar use:
the trigger paints the current color above a checkerboard and opens the full
picker in the overlay pass. Trigger-owned popups may hide the picker's internal
swatch to avoid duplicated color chips, while embedded inspector pickers can
still use `ColorPicker` directly. Color model fields use mode-specific compact
columns: HEX gets one full-width field, RGB/HSL/HSV fit four channels on one
row, and CMYKA fits five compact numeric fields on one row. The picker exposes
a visible eyedropper button that enters sampling mode. The widget accepts an
optional `VectorIcon` for that button but stays asset-agnostic; the self-hosted
app shell supplies the bundled `AppIcon::Eyedropper` SVG from its product icon
registry. The bundled source is the canonical `assets/icons/eyedropper.svg`
asset, with no style suffix variants such as `eyedropper-bold.svg`, so the
picker, demo, and inspector share the same designer-authored symbol. The widget
stays platform-neutral: it emits
`EventRequests::eyedropper`, handles `UiEvent::EyedropperSample` /
`UiEvent::EyedropperCancel`, and never calls screen-capture or OS pointer APIs
directly. Winit shells complete sampling via
`mondrian_app::self_hosted::runtime::WinitUiRuntime`, which delegates platform
work to `mondrian-platform::DesktopEyedropper` and routes the sampled color
back as `UiEvent::EyedropperSample`. Shell-owned eyedropper overlay chrome uses
theme tokens for its magnifier shell and contrast dot; the sampled preview color
is the only dynamic fill. While desktop eyedropper sampling is active, a winit
focus-loss event is not routed as ordinary UI `FocusLost`: external sampling
commonly moves focus to another app, and clearing widget capture at that point
would prevent the later global `EyedropperSample` from reaching the picker that
requested it.
The trigger owns tree-level focus for its popup. Inner text fields are embedded
editing state inside the picker; closing the popup must send them `FocusLost`
and disable IME rather than leaving focus on an internal field id that is not a
stable widget-tree node.
The mode selector shares the generic dropdown's token vocabulary and overlay
behavior, but it remains an internal selector because changing color models is
local widget state rather than an editor `Action`.
Disabled color pickers propagate disabled state into their text inputs, close
the mode menu, cancel pointer/eyedropper interactions, opt out of focus
traversal, and keep painting the current color in muted chrome for inspector
empty states. Disabled-event cleanup releases pointer capture for the
pre-sampling eyedropper button press state as well as active desktop sampling,
color-area drags, and embedded text-field capture, because keyboard focus and
capture ownership are separate router concerns. Programmatic disabling must
clear those interaction fields immediately and remember any required platform
cleanup for the next routed event, so re-enabling the picker cannot revive a
stale color drag, text-field capture, or desktop eyedropper sample from a
previous inspector model.
Circular color areas are painted as colored triangles clipped by a rounded-rect
SDF mask. Their triangle fan must overdraw past the mask radius so the shader's
analytic circle, not polygon chords from the fan, defines the visible edge.

Labels are passive display widgets, but they still clip text to their padded
content bounds during paint. Wrapped labels measure and paint against the same
effective content width, taking the parent layout constraint and any explicit
maximum width together. This keeps property rows, panel headers, and compact
tool surfaces from letting long labels spill into adjacent controls even when
the parent layout constrains them below their natural text width.
Compact label-like controls such as buttons, checkboxes, list rows, and dock
tabs also clip their text lane locally so a long caption cannot bleed into the
next control before a parent-level clip catches it.

## Node Graph

Node graph rendering starts as a domain-light projection in
`mondrian-ui-widgets::NodeGraphView`. The widget owns compact node layout,
port/edge painting, focus/keyboard selection, disabled presentation, selection
chrome, and hit testing for generic `NodeGraphNode` / `NodeGraphEdge` values.
Pointer node selection requests widget focus so arrow-key navigation and
Home/End edge-node jumps plus Enter/Space activation can continue from the
clicked node without requiring a separate focus shortcut. Clicking empty graph
space inside a non-empty graph also requests focus but must not select a node or
dispatch an action; the next unmodified navigation key starts from the graph's
first or last node. Node graph keyboard navigation is limited to unmodified
keys; Ctrl/Shift/Alt/Meta chords are ignored by the widget so panel and
workspace shortcut routing can handle them centrally. It does not own effect
semantics, undo history, or graph mutation rules.

The self-hosted `PanelKind::NodeGraph` panel maps the currently selected clip to
a read-only render chain: Source -> each clip effect -> Output. The app adapter
derives node titles, disabled state, and semantic accents from the same clip
and effect data used by the Inspector, so the graph is another view of the same
state rather than a separate editor model. When no graph can be built, the app
adapter owns the empty-state reason and passes it into `NodeGraphView` while
disabling graph input, so empty projects and no-clip selections do not expose a
focusable graph surface. Node selection is translated back to the existing
timeline clip-selection action while the editor state has no effect-node
selection target; future node editing should add typed app-layer actions before
enabling rewiring or parameter mutation in the widget.

## Curve Editing

Curve editing starts as a domain-independent widget primitive in
`mondrian-ui-widgets`. `CurveEditor` owns normalized `0.0..=1.0` point layout,
hit testing, pointer capture, monotonic-x dragging, keyboard nudging, and themed
grid/curve/handle painting. The curve stroke and selected point fill should use
neutral foreground/surface tokens, not primary blue; blue remains reserved for
focus, playhead/range edges, and explicit active states. Nudging selected points
handles unmodified arrows and Shift large-step arrows only; Ctrl/Alt/Meta arrow
chords stay ignored so shortcut routing remains centralized. Focus loss or
disabling the control may cancel an active point drag, but it must release
pointer capture only when such a drag exists; keyboard focus alone does not
imply capture ownership.
Programmatic disabling clears the active drag immediately and remembers that
the next routed event must release capture, so re-enabling the widget cannot
apply stale pointer movement from a previous panel model. Timeline keyframes,
effect graph curves, and color curves should map their domain data into this
primitive and commit semantic mutations at the panel/app layer instead of
teaching the widget about clips, effects, or undo history.

## Rendering Notes

Widgets emit draw commands only. Checkbox checkmarks are filled triangle-list
commands, not font glyphs or paired line strokes, so they are stable across
operating systems and font stacks. Widgets that need text dimensions use the
widgets crate's shared `mondrian-ui-text` measurement helper; the approximate
`mondrian-ui-core::estimate_text_width` helper is only a low-level fallback for
code that cannot depend on the text crate. Wrapped text is represented as a
constrained text box draw command and resolved by `mondrian-ui-text`, not
manually wrapped inside individual widgets. Tooltip widgets draw border, fill,
and text commands in that order. Standard focus-visible outer rings use the
shared widget paint helper so their alpha, outset, and corner-radius expansion
stay consistent across buttons, dropdowns, pickers, lists, and other controls.
Icon-only buttons paint `VectorIcon` assets rather than text glyphs. SVG is an
import format for designers: the widget layer normalizes SVG documents through
usvg so basic shapes, inherited paint, relative path commands, arcs, and
transforms become renderable path data. The icon keeps lyon-tessellated
theme-tinted triangle meshes as a geometry fallback and metadata path, while
normal small-icon painting rasterizes the SVG source with resvg/tiny-skia at the
ceil of the fitted logical size. For small icons, the widget layer may render a
temporary higher-resolution pixmap with resvg/tiny-skia, then CPU box-filter it
back to the target pixel size before submitting a stable raster-image key to the
renderer. The draw command keeps the original fitted subpixel bounds so resize,
scroll, and DPI scaling do not introduce pixel-snap jitter. Supersampling
prioritizes small-icon coverage up to 4x and steps down only when the temporary
pixmap would exceed the 1024px per-edge raster icon budget, so common SVG
controls get smoother diagonal and curve coverage without letting icon rasters
consume a disproportionate slice of the shared 2048px image atlas.
Icons above that budget fall back to lyon-tessellated triangles; this is a
capacity guard, not the normal visual-quality path.
Larger bounds may fall back to the lyon mesh path.
Text buttons that need command glyphs use the same optional leading
`VectorIcon` path, so icon-only and icon-plus-label controls share parsing,
caching, focus, disabled, and text clipping behavior. Icon-only buttons expose
their label through the shared tooltip manager instead of painting visible
fallback text, which keeps inspector and toolbar density independent of font
availability and localization length.
Bundled SVGs should be loaded through `VectorIcon::from_static_svg` with a
stable icon id so parsing, lyon tessellation, and target-size raster cache reuse
stay deterministic; repeated widget-tree construction must clone cached geometry
rather than reparsing XML.
Product-level bundled icons should enter the custom UI through
`self_hosted::icons::AppIcon` so panel migration code does not duplicate
`include_str!()` paths or depend on legacy egui icon enums. Development tests
must still parse every bundled `AppIcon`, but production panel and menu
construction should not `panic!` if one SVG fails to parse: menu/list rows keep
their text without an icon, text buttons fall back to their label, and dense
icon-only controls may degrade to compact labeled buttons. This keeps a broken
decorative asset from taking down the editor while preserving CI coverage for
the underlying asset regression.
Controls with different geometry, such as slider thumb halos or inset timeline
focus borders, may keep local painting while preserving the same theme token
vocabulary.

`DrawEncoder` preserves subpixel geometry for rectangles, gradients, lines,
images, raster icons, analytic shadows, and arbitrary triangle meshes.
Rectangles, circles, and soft shadows use GPU rounded-rect distance fields; a
true circle is a square bounds whose corner radius clamps to half the side,
while non-square bounds intentionally render as a rounded rectangle or capsule
rather than an ellipse. Soft shadows are encoded as one blur-expanded quad with
local pixel coordinates relative to the caster rect, plus blur radius, spread,
offset, and color from theme shadow tokens. The fragment shader computes the
rounded-rect signed distance and applies a smooth falloff, giving popovers
modern ambient/contact elevation without a per-frame offscreen blur pass. A
future backdrop blur or acrylic surface should be added as a separate renderer
pass rather than overloading drop-shadow commands. Circle SDF tests should
sample boundary points across angle families and tiny sizes so roundness and
numeric stability do not depend on only the cardinal points. Batch-level circle
tests should also reconstruct signed distance from the final generated
triangles, so pixel-to-NDC conversion, y-axis flipping, local UV interpolation,
and pixel-size radius data are covered together rather than only by the ideal
CPU SDF helper. Line commands use a dedicated capsule SDF with conservative
analytic-AA geometry padding and round caps, because a
1px 45-degree stroke rendered as a bare quad can miss MSAA sample positions and
appear broken or intermittent. The visible line width still comes from the
fragment SDF radius; the CPU-generated coverage quad only defines conservative
draw bounds. It expands by the stroke radius plus AA padding both perpendicular
to the stroke and along the stroke axis, with padding wider than the shader's
visible AA edge so backend derivative and MSAA sample differences cannot clip
the fringe before the fragment SDF runs. Wide, short, and zero-length lines
therefore keep their round caps inside the conservative geometry.
The batch builder must also reject lines whose finite inputs overflow while
deriving length, local SDF bounds, coverage-quad points, or final NDC vertices;
primitive safety is enforced before any vertex reaches the GPU.
Together with 4x MSAA resolve and linear atlas sampling, this keeps circles,
SVG raster icons, diagonals, and text glyph images smooth during resize and
scroll. The renderer batch builder is the final primitive-safety boundary: it
rejects zero-sized surfaces, non-finite or non-positive draw bounds, non-finite
colors, invalid UV rectangles, and invalid corner radii before generating GPU
vertices. Clip bounds are the exception to normal draw rejection: they are
expanded conservatively with floor/ceil when the command is recorded, then
intersected hierarchically by the renderer and applied as GPU scissors; invalid
clip scopes become empty clips so enclosed content cannot leak outside the
broken scope. The final GPU scissor conversion rejects non-finite or non-positive
clip rectangles again, so context submission remains safe even if a future batch
path bypasses the normal clip sanitizer. Invalid translate pushes become
zero-offset stack entries so later pop commands still preserve transform-stack
balance. Widgets may still opt into
stable pixel placement at semantic edges with `snap_point()` or local layout
policy, but whole-sale snapping of draw commands is avoided because it degrades
curved/vector geometry and can misalign glyph bitmap bearings.
The GPU UI pass renders through a cached 4x MSAA target and resolves into the
surface view. Raster icon images use a separate renderer-owned image atlas with
linear sampling so SVG icons receive browser-like coverage from resvg/tiny-skia
without sharing mutable atlas state with text glyphs. Raster atlas draws use the
full-color image render mode: sampled RGBA is multiplied by tint, while glyph
atlas draws keep the alpha-mask tint path. Do not route arbitrary raster images
through `RenderMode::Glyph`, because that collapses multicolor assets such as
the product favicon into a single tinted alpha mask. Small SVG rasters are
prefiltered before atlas upload rather than relying on GPU minification as a
box filter; layout bounds remain the authoritative hit-test and composition
geometry. Image UVs remain unsnapped because they are texture coordinates rather
than screen-space geometry.
Linear-sampled atlas entries must upload their full allocated rectangle,
including edge-dilated padding around the inner content UV. Padding is not just
reserved packing space: it is sampled by the GPU at fractional edges, so leaving
it unwritten or transparent can produce dirty borders, alpha fringing, or
neighbor bleeding on glyphs, SVG icons, thumbnails, and checkerboard-backed
color previews.
Raster image commands must not disappear silently when their payload is invalid
or the shared image atlas cannot allocate space. The renderer replaces failed
uploads with a low-alpha diagnostic rectangle and increments
`UiRenderFrameStats::failed_raster_images`; app shells may surface that counter
as resource pressure, but they must not start an unconditional redraw loop
because an exhausted atlas will not heal on the next frame.

Text uses cosmic-text layout and swash grayscale alpha masks in the glyph atlas.
The atlas cache intentionally ignores subpixel bins for the default UI text path
so glyph metrics and bearings stay stable across window resizes and repeated
layout. Subpixel placement is represented by the glyph image bounds emitted by
`mondrian-ui-text`, and the renderer samples the glyph atlas linearly so
fractional positions interpolate coverage instead of snapping to the nearest
texel. LCD/subpixel-color AA is intentionally not used in the default UI path
because it interacts poorly with transparent surfaces, transforms, and
cross-platform compositor differences; a future rich-text/editor mode may add a
separate subpixel-bin atlas where sharper text is worth the cache cost.
Text resolution returns `ResolvedTextCommands` rather than a bare command list so
the app can inspect `TextResolveStats`. Missing glyphs caused by rasterization
or atlas allocation failures must be counted instead of silently disappearing;
the shell may surface the counter, but missing glyphs should not trigger an
unbounded redraw loop.
`SelfHostedFrameRenderer` combines text and raster image diagnostics into
`SelfHostedFrameDiagnostics`. Product and demo windows pass presented frame
results through `SelfHostedRenderDiagnosticReporter`, which logs only changed
failure counts and resets after a healthy frame. Render diagnostics should go to
the tracing/log path by default; the status bar is reserved for actionable
project or editor-state messages. Status bar text is top-positioned from the
bar height and metadata font size, not a fixed baseline, so the bottom chrome
cannot clip half of the glyphs on compact window sizes.

Tests that mutate the process-global theme must take the self-hosted
`theme_test_guard()` before calling `set_theme_preset()`. Most widget tests
should avoid the global theme entirely and pass an explicit
`ThemePreset::build()` snapshot to `PaintContext`; global theme tests without
the guard can race under Rust's parallel test runner and make visual token
regressions look intermittent.

The renderer must flush draw batches when clip state changes and must apply the
batch clip rect as a GPU scissor before drawing. A command emitted inside
`PushClip`/`PopClip` must not share a batch with unclipped geometry, otherwise
glyph images and other later-resolved commands can bleed outside their widget
content rects. Clip commands are resolved through the active translate stack
when the batch is built, producing a screen-space effective clip that is then
intersected with parent clips. That effective clip travels with the batch; later
draw transforms must not reinterpret it or mix clipped and unclipped vertices in
one batch.
The text resolver preserves surrounding draw-state commands. A `Text` command
inside `PushClip`/`PopClip` resolves into glyph `Image` commands at the same
sequence position, still enclosed by the original clip scope.
Self-hosted windows share `SelfHostedFrameRenderer` for the text-atlas upload
and surface-present path. `mondrian-ui-text` resolves text into glyph image
commands and exposes pending glyph uploads before the frame is submitted, so the
renderer can upload first-use glyphs before drawing. The shared renderer still
requests one deterministic follow-up redraw after first-use glyph or raster
image uploads, because some backends make freshly written atlas texels visible
one frame later. Window loops should depend on `SelfHostedFrameResult` for this
warm-up redraw instead of adding entrypoint-specific repaint hacks.

Line commands are expanded to coverage quads with deterministic triangle winding
for every orientation, then shaded as capsule SDFs in local line coordinates.
The UI pipeline disables back-face culling for 2D primitives: winding remains a
batch-builder quality contract and a regression-test signal, but a missed
orientation must not make a production UI stroke disappear. The quad is only the
conservative draw bounds; the visible stroke edge, AA, and round caps come from
the fragment shader. Its bounds must still include the round-cap radius on the
line axis, not only AA padding, otherwise valid wide or zero-length strokes can
be clipped before shading. This matters for splitter handles and tool icons
because thin diagonal strokes must not depend on sample coverage alone.
Line regressions should be tested as angle families, not only as horizontal and
vertical strokes: 1px lines at common diagonal angles must keep front-facing
winding, local pixel-space SDF coordinates, a continuous centerline, stable
pixel-center coverage at subpixel offsets, and a bounded alpha profile along
the stroke. Hairline tests should also assert an 8-connected visible coverage
path from the start cap to the end cap for 45 degree strokes, because local
per-step visibility can miss dotted-line regressions that are obvious to users.
Renderer tests should reconstruct line
coverage from the final batch triangles, not only from ideal SDF-local
coordinates, so pixel-to-NDC conversion, y-axis flipping, triangle winding,
local coordinate interpolation, and cap coverage stay covered as one contract.
The CPU coverage model and WGSL fragment shader AA clamp/edge-scale constants
must be tested as the same contract; changing shader smoothstep parameters
without updating the coverage model can leave tests green while reintroducing
weak diagonal strokes.
Axis aligned
semantic separators may snap to pixel centers locally, but arbitrary angle
lines should keep their authored subpixel endpoints so diagonal strokes do not
shimmer or change slope during resize and scroll. Filled triangle-list
commands are also normalized to front-facing winding after the pixel-to-NDC y
flip while preserving the authored subpixel vertices. Per-vertex colored
triangles must swap color and mask-local coordinates with their corresponding
point when winding is normalized; otherwise masked color fans can stay visible
while their hue or SDF mask coordinates drift. Masked colored-triangle tests
should reconstruct the rounded-mask signed distance from final batch vertices,
matching the circle batch tests so color-wheel and gradient swatches verify
pixel-to-NDC conversion, local UV interpolation, and mask radius together. The
batch builder rejects non-finite triangle positions, non-finite vertex colors,
and zero-area or near-zero-area triangle primitives before they reach the GPU
vertex buffer; `DrawEncoder` still preserves valid subpixel vertices and only
drops incomplete triangle tails. Checkbox checkmarks use one filled triangle-list
shape on the 16px checkbox grid instead of two independent line strokes, so the
elbow has a single joined fill and cannot form a visual X.

Node graph widgets stay domain-light: `mondrian-ui-widgets::NodeGraphView`
only knows stable node ids, screen-space layout, and selection chrome. The app
adapter owns the semantic mapping from graph node id to editor target (`Clip`,
`Effect`, or `Output`) and dispatches typed app actions from that mapping. Clip
selection remains timeline-scoped, while effect selection is a nested
`SelectionState::selected_effect` value validated against the active sequence
and pruned with stale clips/effects. Selecting an effect is navigation state and
must not enter timeline undo history; mutating, reordering, or removing effects
continues to use undoable editor actions.

The self-hosted product shell persists user-facing shell preferences through
`SelfHostedUiHost`, not reusable widgets or `SelfHostedAppRoot`. Theme preset
workspace preset, and the optional Custom workspace layout are restored before
the first root widget is built, so the initial dock tree matches the last
product workspace. Shell-only actions such as `Action::SwitchWorkspace` or
`FocusPanel` fallback update the root immediately; `SelfHostedUiHost` then
compares the root workspace before/after handled shell actions and writes any
changed workspace preset/layout back through `self_hosted::preferences_store`.
Widget-local layout changes that do not dispatch an action, such as splitter
drags, are synchronized explicitly by the window runner after the interaction
settles. Editor-state actions and widget models remain disk-I/O free.
Preference loading is intentionally strict while the self-hosted UI is alpha:
files with an incompatible schema version, malformed JSON, or missing required
fields fall back to clean defaults. Loaded recent projects are filtered to
existing, de-duplicated paths; shortcut overrides must reference known
descriptors; and custom workspace layouts are sanitized before use. If the
stored workspace preset is Custom but the stored layout cannot materialize as a
split-root dock tree, the shell downgrades to the Editing preset and drops the
stale custom layout instead of opening a broken workspace.

Panel model refreshes must not erase local panel interaction state. Reusable
widgets expose small explicit state snapshots for UI-local affordances such as
`PanelList` filters, selection, and scroll offsets. Dock containers expose their
active tab through widget APIs, and `SelfHostedAppRoot` captures that shell-local
navigation state before rebuilding dock content from fresh `AppState` models.
`SelfHostedUiHost` must also defer same-mode dirty refreshes while the active
widget tree has transient interaction, such as an open context menu/dropdown or
an active text input. Background thumbnail or preview completions may mark the
UI dirty, but they must not rebuild the root tree until the transient surface is
closed; otherwise search fields lose focus and context menus disappear during
ordinary clicks.
Refresh restoration must use stable ownership, such as `PanelKind` plus a
per-panel ordinal, rather than visible titles or labels. Display text can change
for localization, product naming, or dynamic folder context, and must not decide
whether filter, selection, scroll, or active-tab state survives a model rebuild.
It restores active tabs before list state so grouped panels such as
Assets/Effects keep showing the surface the user was working in. Export is an
independent panel/workspace. Splitter layout restoration remains owned by
`DockSplitter`, while durable Custom layout persistence is owned by
`SelfHostedWorkspaceLayout` and the preferences store. This keeps app-state data
replacement separate from ephemeral user navigation state and from user-facing
workspace customization.

Until the i18n layer exists, self-hosted product surfaces are Chinese-first:
menu triggers, dock tab display names, empty states, context-menu rows, dialog
titles, file-dialog labels, and inspector/export field labels should be Chinese
unless the text is a file extension, codec, color-space standard, shortcut, or
other industry term normally shown in Latin script. Tests and state restoration
must assert stable actions or `PanelKind` ownership rather than English visible
labels.
