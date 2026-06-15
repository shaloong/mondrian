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

`mondrian-ui-events` owns routing: hit testing, focused keyboard dispatch, and
pointer capture. Widgets record side-effect intent in `EventRequests`; the
router or app shell applies those requests.

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

`mondrian-app/src/main.rs` is the product entrypoint and builds the current
production egui shell through `mondrian-app::app::MondrianApp`.
`mondrian-app/src/bin` is reserved for developer-only binaries: widget
galleries, pipeline smoke tests, and migration integration shells. Binaries in
`src/bin` must stay thin; reusable runtime, panel, or mapping logic belongs in
library modules.

The legacy production egui UI lives under `mondrian-app/src/egui_ui`. That
module contains egui panels, egui theme tokens, viewer helpers, and the old
timeline implementation. New self-hosted UI code must not be added there unless
it is deliberately bridging or deleting legacy egui behavior.

The self-hosted UI application adapter lives under
`mondrian-app/src/self_hosted`. `self_hosted::runtime` owns winit-side request
application for reusable widgets. `self_hosted::rendering` owns the shared
wgpu frame submission path for self-hosted windows, including cosmic-text glyph
uploads, surface texture acquisition, present, and surface reconfigure on
loss/outdating. `self_hosted::shell` owns reusable root-widget composition such
as the menu bar plus dock tree; developer binaries should use `SelfHostedAppRoot`
rather than defining shell widgets inline. `self_hosted::icons` owns the
app-layer registry for bundled designer SVG icon assets and converts them into
`mondrian-ui-widgets::VectorIcon` / `IconButton` values without depending on
legacy egui theme types. `self_hosted::panels` owns panel adapters that map
application-facing concepts into generic widget view models.
`self_hosted::host::SelfHostedUiHost` owns the reusable product
state bridge: it keeps the root widget, current `AppState`, dirty refresh flag,
and queued-action draining together so window entrypoints do not duplicate
root/AppState refresh plumbing. `self_hosted::window` owns the reusable
winit/wgpu product-window runner; `src/bin/self_hosted_app.rs` is only a thin
executable launcher. The boundary type is `SelfHostedPanelModels`: real `AppState` /
`EditorState` adapters should produce this model, while
`SelfHostedPanelModels::demo()` is test-only fixture code and must not be part
of product entrypoints.
`SelfHostedPanelModels::from_app_state` is the app-side snapshot boundary: it
reads the current `AppState`, asset library, effect registry, selection state,
and timeline sequence into generic widget models. Timeline adapters start at
`TimelinePanelModel::from_sequence`, which maps `mondrian-timeline::Sequence`
plus app-layer selection DTOs into widget view models and stable-id-backed
actions. During the migration, the `self_hosted_app` developer binary is the
product-shell tracer: it starts from a real empty `AppState`, builds
`SelfHostedPanelModels::from_app_state`, and refreshes from that same boundary
after dispatched actions. Component fixtures remain in `ui_demo` and explicit
`SelfHostedPanelModels::demo()` tests only. When the self-hosted UI becomes the
official `mondrian` entrypoint, it should keep calling into this module with
real panel models instead of moving logic back into `src/bin`.

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
`app::exporting::TimelineExportRequest`.
Self-hosted export forms persist their editable draft in `AppState::export_draft`
through `ui.export.set_draft`, so widget-tree refreshes and dock layout changes
do not reset selected preset, selected sequence, range, or output path.
Choosing an export output path is also an app-shell intent: panels emit
`app.shell.export_output_dialog` with a suggested name/container extension, and
`self_hosted::shell::resolve_app_shell_action` converts the native save-dialog
result into `ui.export.set_draft(OutputPath(...))`. Widgets must not call
platform file dialogs directly.
Those app-shell dialog intents are built through `app::ui_actions` helpers so
menus and self-hosted panels share the same stable custom-action ids. Shell
local actions, such as About and close-modal, use the same helper boundary
even when they do not resolve to editor-state actions. Native window commands
are deliberately separate from editor state: Quit is emitted as
`app.shell.quit`, and `Action::ToggleFullscreen` is consumed by the
self-hosted host as a `SelfHostedShellCommands` value for the winit entrypoint.
They must not be dispatched into `AppState`, where `CloseProject` keeps the
narrow meaning of closing the current project.
`self_hosted::shell::resolve_app_shell_action` is the tested boundary that
turns those intents into concrete project creation, `OpenProject`,
`ImportMedia`, and `SaveProjectAs` actions after a native adapter supplies
platform dialog results. Project creation actions carry full
`SequenceSettings` and `ProjectSettings` payloads before reaching `AppState`.
The self-hosted new-project flow stages editable form state in
`SelfHostedNewProjectDraft`, which owns the same settings structs used by
project creation so the eventual custom form cannot drift from lifecycle
semantics.
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
the active clipboard.
Inspector timing controls reuse timeline trim actions for clip In/Out changes
instead of introducing a parallel editing path. `TrimClipStart` and
`TrimClipEnd` accept source in/out times from inspector-style controls, convert
them to timeline trim frames with the clip's current positive finite speed
multiplier, then route through `AppState::trim_clips_bulk_to_frame`; locked
track validation, linked-clip trim behavior, undo snapshots, and timeline
modified events therefore stay centralized in the timeline command path.
Inspector effect rows are read from the selected clip's effect instances and
toggle or remove effect instances through `AppState::set_clip_effect_enabled`
and `AppState::remove_effect_from_clip`; the widget layer sees only button /
checkbox values plus stable effect ids.
The generic `Action::RemoveEffect` resolves the clip id into the active sequence
selection reference and reuses the same remove-effect command, so inspector
buttons, shortcuts, scripts, and macros share locked-track validation and undo
behavior.
`Action::ReorderEffects` follows the same boundary and calls
`AppState::reorder_effects_for_clip`, which clamps the target slot, rejects an
invalid source index, and records one undoable sequence snapshot only when order
actually changes. Self-hosted Inspector reorder controls emit this typed action
directly rather than adding an inspector-specific custom action.
Effects browser activation uses the same protocol family: when a video clip is
selected, effect rows carry a `ui.effects` add-to-clip payload with the selected
clip id and serialized `EffectType`, and `AppState` routes it through
`add_effect_to_clip`.
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
to children.

The router treats widget ids as frame-local routing handles. Before routing a
new event it drops hovered, focused, or captured ids that are no longer present
in the current `WidgetTree`, which keeps rebuilt panels from inheriting stale
capture/focus state after popovers close, drags finish, or panel contents
refresh. If the stale widget owned focus, the router also emits an IME disable
request because the removed widget can no longer receive `FocusLost`.
If the focused widget still exists but no longer returns `can_focus()`, the
router sends `FocusLost`, releases focus, and disables IME before routing the
next event.

## Focused Text Input

Keyboard, text, and IME events route to `FocusManager::focused_widget()`.
`TextInput` requests focus on click, enables IME while focused, stores preedit
composition text, and inserts committed IME text through the same grapheme-aware
editing path as normal text input.
Focus traversal treats a cycle back to the same widget as a no-op: the router
handles Tab but does not emit `FocusLost`/`FocusGained`, avoiding selection and
IME flicker when only one focusable control is present.

Single-line text input maintains a horizontal viewport owned by the widget. The
cursor is scrolled into view after layout, editing, navigation, or selection
changes; pointer hit testing accounts for the current scroll offset. IME cursor
areas use the caret rect rather than the full widget bounds so platform
composition windows can anchor near the insertion point.

Text content is clipped to the padded content rect, not the outer widget
bounds. App shells should show an I-beam cursor for text inputs only after the
input owns focus; hover alone should not switch the pointer shape.
Text inputs use the same `mondrian-ui-text` measurement path as glyph rendering
for cursor movement, selection geometry, hit testing, and horizontal scroll;
approximate width estimates are not used for editable text internals.
IME is a platform side effect: text widgets emit `EventRequests::ime`, the
event router exposes the latest request, and the app shell applies it to the
native window (`set_ime_allowed` plus cursor area for winit). Pointer clicks
outside the focused widget send `FocusLost` so IME is disabled when editing
ends.

Text copy/cut shortcuts are consumed by `TextInput` only when a selection exists.
If there is no selection, `Ctrl+C` and `Ctrl+X` are ignored so panel-level
commands, such as copying clips or keyframes, can handle them.
When a `TextInput` receives an outside mouse down directly, it clears local
focus and IME state but returns `Ignored`; the clicked sibling must still receive
the event. Intentional focus loss that should stop propagation is delivered by
the router through `FocusLost`.

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
Shortcut resolution receives a `ShortcutContext` from the router focus state and
must search scopes in a fixed order: focused widget, focused panel, workspace,
then global. Same-scope duplicate registrations replace the older binding so
the active command is deterministic.
Focused-panel context is inferred by walking from the focused widget to the
nearest ancestor widget that exposes `Widget::panel_kind()`. `PanelSlot` is the
normal boundary that returns a panel kind. Leaf controls request only widget
focus through `FocusManager::request_focus(widget)`; they must not hardcode
panel identities. The router normalizes focused-panel state after widget events
so panel shortcuts follow the actual dock location.

## Pointer Capture

Drag widgets request capture on mouse down and release capture on mouse up.
`Slider`, `DockSplitter`, and text selection depend on this behavior. Future
Timeline clip drags, curve editor handles, and color picker gestures should use
the same request path.

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
gaps.

Slider value mapping uses the same thumb-centered track for painting and
pointer updates. The thumb rect must remain inside widget bounds; if a parent
gives a short row, the thumb shrinks vertically instead of being clipped.

Timeline widgets own frame-space presentation interactions: selection, seeking,
scrolling, zooming, local drag previews, and edge-trim previews. On mouse
release they emit domain-light move/trim proposals (`TimelineClipMove`,
`TimelineClipTrim`) instead of resolving clip overlaps, ripple behavior, linked
media, source in/out offsets, or undo snapshots. Those semantics stay in
`mondrian-app` / `mondrian-timeline` command handling. Timelines expose
overlay horizontal and vertical scrollbars when content overflows; scrollbar
thumb drags and track paging must win hit testing over clip selection and
seeking. Timeline surfaces are focusable: while focused they may handle
timeline-local navigation such as playhead nudging, but global editor commands
remain outside the widget layer.

## Overlays

Dropdowns, context menus, and tooltips derive event hit regions and paint
geometry from shared rect helpers. Dropdown triggers, context-menu popup chrome,
rows, separators, active indicators, and scrollbars are painted through shared
menu helpers so action-backed dropdowns, context menus, and internal selectors
keep the same visual language. Disabled menu items consume pointer input
without dispatching actions or closing the overlay; outside clicks close open
menus. Closed dropdown measurement is based on the trigger label only so long
popup choices do not widen compact inspector rows; the popup itself expands to
the longest row label. Dropdowns and context menus measure row labels through
the widgets crate's shared `mondrian-ui-text` measurer, so CJK, long Latin
labels, and platform font metrics use the same layout facts as final glyph
rendering instead of approximate character-width math.
Popup elevation goes through the widget crate's shared paint helper and theme
shadow tokens rather than per-widget hardcoded black alpha values, so dark/light
themes can tune perceived depth centrally. Common color composition helpers
such as alpha scaling, color mixing, and softened borders also live behind that
shared paint helper so widgets do not drift in their interpretation of tokens.
Dropdown trigger labels are clipped to the trigger text lane, reserving the
arrow area when enabled, so constrained form rows do not let long labels paint
over affordances or neighboring controls.
Tooltip requests preserve their delay timer when the same tooltip is reported
repeatedly during hover, and tooltip painting clamps to the current clip rect.

Overlay-capable widgets paint their normal trigger chrome in `paint()` and
their floating chrome in `paint_overlay()`. `TreeWalker::paint()` runs the
overlay pass after the root's normal paint pass, so dropdown menus and similar
popups are not hidden by siblings that happen to paint later in the normal
content tree. Tooltip popups are overlay-only: a composite widget that owns a
tooltip must forward it from `paint_overlay()` instead of drawing it inside the
panel's normal `paint()` method.

Dropdowns request pointer capture while open so outside clicks, Escape, wheel
events, and release events continue to route to the popup even when the pointer
is over another widget. The opening click's release is suppressed so it cannot
accidentally select the first item under the cursor; item actions dispatch only
when a press and release land on the same enabled row. Long dropdown menus clip
their item list and scroll with the same positive-delta-means-content-down
offset convention as `ScrollView`. Menu separators are explicit non-action
items and paint geometric divider rects; disabled items only mute their text and
must not draw strikethroughs or divider-like chrome.
Open menus support keyboard navigation: Up/Down cycles through enabled action
rows while skipping separators and disabled rows, Enter/Space activates the
highlighted row, and Escape closes the popup. Internal selectors such as the
ColorPicker mode menu follow the same keys but commit local widget state instead
of dispatching editor actions.
Menu bars coordinate sibling dropdowns: when one menu is open, clicking or
hovering another menu trigger closes the old popup and opens the new one.
Trigger-click opens suppress the matching release; parent-coordinated hover
opens must not, because the next release belongs to a fresh menu-item click.

Keyboard activation for focused controls follows desktop conventions: Button
handles Enter/Space as an activation gesture, and Checkbox handles Enter/Space
as a toggle gesture. KeyDown performs the semantic action and enters the pressed
visual state; the matching KeyUp clears the pressed state. Keyboard events are
routed by focus, so these handlers do not perform hit testing.

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
own line-breaking logic.

## Overlay Contract

Dropdowns, popovers, context menus, tooltips, and shell affordances paint in
the overlay pass after normal widget content. A widget with an open top-layer
popup that needs outside-click dismissal must expose that boundary through
`overlay_hit_test()`, not by widening its normal `hit_test()` bounds. The event
router resolves overlay hits before normal content hits and searches child
overlays before a parent's broad close layer, so a nested or visually topmost
popup receives pointer and wheel events before an ancestor outside-click
catcher or a sibling panel.
Dragging popup internals should use pointer capture so move/up events remain
routed to the owning widget.

## Scroll Views

Scroll containers translate pointer events from screen coordinates into child
content coordinates using `screen - viewport_origin + scroll_offset`, matching
the inverse of their paint transform. Wheel events are handled only inside the
viewport and offsets are clamped after wheel input and layout. Offset-changing
wheel, track, thumb drag, and scrollbar hover transitions request repaint
through `EventRequests`.

`ScrollView` exposes a draggable vertical scrollbar thumb when content
overflows. The scrollbar is an overlay affordance and does not reserve child
layout width. Thumb drags request pointer capture, map thumb-track movement
back to content scroll offset, and release capture on mouse up. Clicking the
scrollbar track outside the thumb pages the viewport by one visible span.

## Panel Migration

Panel migration should start with low-risk inspector/property surfaces before
Timeline. Inspector-style controls should be assembled with the reusable
`PropertyPanel` / `PropertySection` / `PropertyRow` container in
`mondrian-ui-widgets`, then bound to editor state and undoable commands at the
panel/app layer. Docked panel chrome belongs to `DockPanel`, which owns the
`DockTabBar`, active-tab content rebuilding, `PanelSlot`, and overlay
forwarding. App entrypoints should call panel adapters in
`mondrian-app::self_hosted::panels` instead of reimplementing tab/content
synchronization. The `self_hosted_app` Inspector slot uses this path with real
self-hosted widgets (checkbox, slider, and color trigger) instead of a colored
placeholder. Its panel snapshot carries the selected clip identity, and user
edits emit stable inspector actions that mutate the selected clip through
undoable app state commands. Basic transform controls show position in sequence
pixels, uniform scale in percent units, and rotation in degrees; the app action
handler converts those UI values back into `Transform2D` property mutations.
Timing controls show absolute timeline frames and dispatch the same trim
payloads as the Timeline view.
Timeline selection actions treat the clip id as authoritative and resolve the
current track from `AppState`; track ids in widget snapshots are context only
because they can be stale after moves, undo/redo, or refresh lag.
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
Timeline corner controls for adding video/audio tracks emit only a
`TimelineTrackKind`; the app adapter translates that into
`ui.timeline.add_track`, and `AppState` routes it through the existing undoable
track creation commands.
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
`TimelineEditCommand::SplitAtPlayhead` and `Action::SplitClipAtPlayhead`.
Mark In / Mark Out shortcuts use `TimelineEditCommand::MarkInAtPlayhead` and
`TimelineEditCommand::MarkOutAtPlayhead`, then route through shared app actions
so timeline and viewer shortcuts can converge on the same command boundary.
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
`primary_selected_clip()` resolves the current sequence before returning so
single-target panels do not inherit stale track metadata.
Panel models should also resolve selected clips by clip id when reading a
Sequence so stale cached track metadata does not make selection disappear.
Attached effects appear as inspector rows with enable checkboxes, using effect
instance ids rather than list indices so reorder/remove operations can be added
without changing the widget contract. Removal buttons use the same effect id
payload and stay in the app-layer action protocol rather than deleting from the
widget tree directly.
This lets focus routing, overlay popups, repaint requests, shell runtime
behavior, and editor-state dispatch be validated in the same dock tree that
future panels will use. Timeline migration should reuse this path after
scrollbars, overlays, and property controls are stable.
When a self-hosted shell refreshes panel models from `AppState`, it may rebuild
panel content widgets, but it must preserve dock chrome state. `DockSplitter`
therefore exposes a layout snapshot containing splitter direction/ratio data
only, and `SelfHostedAppRoot::set_models` restores that snapshot before
relayout so app data changes do not reset user-resized panels.

Browser-style panels should use `PanelList` / `PanelListItem` instead of
ad-hoc colored placeholders or one-off row painting. `PanelList` owns local
selection, disabled rows, keyboard navigation, activation, and internal
positive-delta scrolling, but exposes static and value-aware action adapters
plus optional `DragPayload`s so Assets, Effects, presets, and similar panels can
bind to editor state outside the widget crate. Mouse single-click selects a row,
a second click on the same row activates it, and keyboard Enter/Space uses the
same activation path. Pointer movement beyond the drag threshold asks the
router to begin an internal drag; the router, not the source widget, owns
`DragEnter` / `DragOver` / `DragLeave` / `Drop` delivery so pointer capture from
the source cannot block target panels. The self-hosted Effects panel builds rows
from the shared effect registry and, when a video clip is selected, activates
rows through undoable
`AppState::add_effect_to_clip` commands. The `self_hosted_app` and `ui_demo`
Assets/Effects-style panels use this shared surface as the tracer bullet for
migrating list-heavy egui panels. The self-hosted Project slot also uses
`PanelListModel::from_project_status` to show project file, active sequence,
asset-library, and current status-hint state instead of a colored placeholder.
The adjacent Console tab reads `AppState::status_log`, a bounded history fed by
`set_status_hint`, and shows recent messages newest-first before runtime
summary rows. `clear_status_hint` clears only the transient bottom-bar hint; it
does not erase Console history.
Real product panels keep single-click row selection local to the widget unless
the app has a stable domain selection to update; file commands, asset drags,
and effect insertion are emitted only through activation actions.
`PanelListModel::demo_activate_prefix` is compiled only for tests and exists
for synthetic fixture commands; product panel models must attach explicit
stable actions to rows instead of deriving commands from titles or indices.
Synthetic demo rows use the `ui.demo_panel` action namespace so they cannot be
confused with the app-layer `ui.assets`, `ui.effects`, `ui.timeline`, or
`ui.inspector` protocols.
The lower-left dock exposes that Project status as the first tab beside
Console, so product-shell state is visible in the default layout without adding
another split.
Self-hosted `FocusPanel` and current View-menu `TogglePanel` actions activate
the matching dock panel or grouped tab through shell-local dock traversal and do
not continue into `AppState`. The traversal first understands grouped tabs in
the default layout, then falls back to direct panels used by built-in workspace
presets such as Color, Compositing, and Export. True hide/show panel visibility
should be added as a separate dock-tree policy so it can handle split collapse
and restoration deliberately.
Self-hosted `SwitchWorkspace` is also shell-local: it rebuilds the dock tree
from the current `SelfHostedPanelModels` using a built-in preset while keeping
panel models read-only and app/domain mutation in `AppState`. Refreshing panel
models must preserve the selected workspace preset so live app snapshots do not
silently reset the user's shell layout.
The self-hosted Assets panel maps real library rows to `ui.assets.prepare_drag`;
`AppState` resolves the asset record and reuses the existing `begin_drag_asset`
path so later Timeline drop handling stays shared with the egui implementation.
The same left dock hosts the Effects browser as an `Effects` tab so effect
insertion remains visible without changing the default split layout.
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
When no timeline model is available, app panels should disable the surface so
empty shells do not steal focus, seek, or hold pointer capture.

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
Self-hosted Inspector actions should use typed payloads for clip mutations.
The curve editor currently emits `ui.inspector.set_clip_curve` with normalized
points; AppState maps them to opacity keyframes over the selected clip's
timeline span so curve edits participate in undo/redo and render evaluation.
Inspector clip mutations are validated at the AppState boundary, including
locked-track protection; widgets stay domain-light and do not decide whether a
clip can be edited.
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
`ImePreedit` / `ImeCommit` semantics. Entry binaries should route Escape
through the widget tree first and only treat it as a window close when ignored.
Entrypoints must also consume `WindowEvent::ModifiersChanged` through
`winit_modifiers_to_ui_modifiers`; key-edge tracking is only a fallback for the
current keyboard event and must not be the sole source of modifier state.
When a winit window reports `WindowEvent::Focused(false)`, entrypoints must
route `UiEvent::FocusLost` and reset the tracked modifiers to
`Modifiers::none()`. The router treats that as a window-level blur: active
drags are cancelled, capture and hover are released, focused widgets receive
`FocusLost`, IME is disabled, and tooltip state is hidden.
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
useful default workflow. `PanelList` exposes a domain-light `on_drop` adapter;
the self-hosted Assets panel maps file drops to `Action::ImportMedia` there,
while other panels can opt into their own drop semantics without teaching the
generic list widget about application state.
Shell cursor selection is also centralized in the runtime. Entrypoints provide
the current eyedropper, splitter, and focused-text state; the runtime resolves
priority as eyedropper sampling, splitter resize affordance, focused text
editing, then default cursor.
Self-hosted entry binaries should collect widget-dispatched actions during
event routing, then drain them after the root borrow ends. Shell-local actions
such as the new-project dialog mutate `SelfHostedAppRoot`; only confirmed
project creation emits the editor-facing `ui.project.create_with_settings`
action consumed by `AppState`.
The pending queue lives in `self_hosted::action_queue`; `SelfHostedUiHost`
drains it after routing and applies shell/AppState refresh policy. Entry
binaries should use these types rather than open-coding shell/AppState
dispatch. The host refreshes panel models after every dispatched editor action,
including actions that return an error, because action handlers may still update
status hints or other user-visible state before reporting the failure.
`SelfHostedUiHost::drain_pending_actions` also returns window-host commands
such as quit and toggle-fullscreen. Entrypoints apply those commands only after
event routing and model refresh have completed, so native side effects stay out
of widget code and out of `AppState`.
Project/status panel models should keep transient status rows near the top of
the list, before command rows, so error feedback from failed actions is visible
without requiring the user to scroll a compact dock panel.
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
painting: dock content, menu bar, then the active modal. Normal painting should
follow the same order so visual stacking and interaction stacking stay aligned.
Modal card geometry and chrome should be centralized through
`mondrian-ui-widgets::DialogSurface`; app dialogs should keep only local content
layout and event semantics.
Dialog and form copy should use reusable `mondrian-ui-widgets::Label`
instances for semantic color, padding, and wrapping rather than direct
per-dialog `draw_text` calls.
Inspector/property-panel titles, section headers, and row labels follow the
same rule through `PropertyPanel`'s internal `Label` instances.
Reusable labeled-field geometry should flow through
`mondrian-ui-widgets::FormLayout` / `FormRowOptions` instead of each component
recalculating label and control rectangles independently.
Form controls should expose a common `enabled(bool)` / `disabled()` builder
where practical. Disabled controls must not dispatch actions, request pointer
capture, or participate in focus traversal, and should render with muted theme
tokens rather than panel-local color constants.

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

## Viewer Surface

The self-hosted Viewer panel uses the domain-light `ViewerSurface` widget
instead of a colored placeholder. App code maps `AppState` / `Sequence` into a
small `ViewerPanelModel` containing title, playback status, current frame,
duration, and source resolution. The widget owns preview chrome, source
aspect-ratio fitting, metadata labels, and safe-area guide drawing only; GPU
preview texture ownership remains a future renderer/runtime integration point.
Empty app state maps to a disabled viewer model so the product shell can show
clear no-signal chrome without pretending a preview texture exists.
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
synchronization.

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
sit inside rounded swatches. A compact
`ColorPickerTrigger` wraps the full picker for inspector rows and toolbar use:
the trigger paints the current color above a checkerboard and opens the full
picker in the overlay pass. Trigger-owned popups may hide the picker's internal
swatch to avoid duplicated color chips, while embedded inspector pickers can
still use `ColorPicker` directly. Color model fields use mode-specific compact
columns: HEX gets one full-width field, RGB/HSL/HSV fit four channels on one
row, and CMYKA fits five compact numeric fields on one row. The picker exposes
a visible geometric eyedropper button that enters sampling mode. The widget
stays platform-neutral: it emits `EventRequests::eyedropper`, handles
`UiEvent::EyedropperSample` / `UiEvent::EyedropperCancel`, and never calls
screen-capture or OS pointer APIs directly. Winit shells complete sampling via
`mondrian_app::self_hosted::runtime::WinitUiRuntime`, which delegates platform work to
`mondrian-platform::DesktopEyedropper` and routes the sampled color back as
`UiEvent::EyedropperSample`.
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
empty states.

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
It does not own effect semantics, undo history, or graph mutation rules.

The self-hosted `SlotKind::NodeGraph` panel maps the currently selected clip to
a read-only render chain: Source -> each clip effect -> Output. The app adapter
derives node titles, disabled state, and semantic accents from the same clip
and effect data used by the Inspector, so the graph is another view of the same
state rather than a separate editor model. Node selection is translated back to
the existing timeline clip-selection action while the editor state has no
effect-node selection target; future node editing should add typed app-layer
actions before enabling rewiring or parameter mutation in the widget.

## Curve Editing

Curve editing starts as a domain-independent widget primitive in
`mondrian-ui-widgets`. `CurveEditor` owns normalized `0.0..=1.0` point layout,
hit testing, pointer capture, monotonic-x dragging, keyboard nudging, and themed
grid/curve/handle painting. Timeline keyframes, effect graph curves, and color
curves should map their domain data into this primitive and commit semantic
mutations at the panel/app layer instead of teaching the widget about clips,
effects, or undo history.

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
Icon-only buttons paint `VectorIcon` geometry rather than text glyphs. SVG is an
import format for designers: the widget layer normalizes SVG documents through
usvg so basic shapes, inherited paint, relative path commands, arcs, and
transforms become renderable path data, then uses lyon to tessellate fills and
strokes into cached theme-tinted triangle meshes.
Bundled SVGs should be loaded through `VectorIcon::from_static_svg` with a
stable icon id so parsing and lyon tessellation happen once; repeated widget-tree
construction must clone cached geometry rather than reparsing XML.
Product-level bundled icons should enter the custom UI through
`self_hosted::icons::AppIcon` so panel migration code does not duplicate
`include_str!()` paths or depend on legacy egui icon enums.
Controls with different geometry, such as slider thumb halos or inset timeline
focus borders, may keep local painting while preserving the same theme token
vocabulary.

`DrawEncoder` snaps axis-aligned UI geometry to whole pixels at command
recording time: rectangle bounds, line endpoints, clip bounds, image bounds,
and translate offsets. This keeps rounded-rect circles, slider thumbs,
splitter handles, and scrollbar thumbs visually stable after window resizing.
Text positions are left to the text renderer and caller-side layout policy, and
image UVs remain unsnapped because they are texture coordinates rather than
screen-space geometry.

The renderer must flush draw batches when clip state changes and must apply the
batch clip rect as a GPU scissor before drawing. A command emitted inside
`PushClip`/`PopClip` must not share a batch with unclipped geometry, otherwise
glyph images and other later-resolved commands can bleed outside their widget
content rects.

Line commands are expanded to quads with front-facing triangle winding for every
orientation. This matters for splitter handles because the UI pipeline keeps
back-face culling enabled. Filled triangle-list commands are also normalized to
front-facing winding after the pixel-to-NDC y flip. Checkbox checkmarks use one
filled triangle-list shape on the 16px checkbox grid instead of two independent
line strokes, so the elbow has a single joined fill and cannot form a visual X.
