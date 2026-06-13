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
eyedropper overlay, and feeds sampled colors back into the widget tree.

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
application for reusable widgets. `self_hosted::shell` owns reusable root-widget
composition such as the menu bar plus dock tree; developer binaries should use
`SelfHostedAppRoot` rather than defining shell widgets inline. `self_hosted::panels`
owns panel adapters that map application-facing concepts into generic widget
view models. The boundary type is `SelfHostedPanelModels`: real `AppState` /
`EditorState` adapters should produce this model, while
`SelfHostedPanelModels::demo()` is only a developer fixture.
`SelfHostedPanelModels::from_app_state` is the app-side snapshot boundary: it
reads the current `AppState`, asset library, effect registry, selection state,
and timeline sequence into generic widget models. Timeline adapters start at
`TimelinePanelModel::from_sequence`, which maps `mondrian-timeline::Sequence`
plus app-layer selection DTOs into widget view models and stable-id-backed
actions. During the migration, the `self_hosted_app` developer binary uses
`SelfHostedAppRoot::demo()` as the integration shell. When the self-hosted UI
becomes the product shell, the official `mondrian` entrypoint should call into
this module with real panel models instead of moving logic back into `src/bin`.
The developer shell keeps demo panels visible for component testing, but its
action sink already dispatches through `AppState::dispatch_action` so menu and
timeline UI actions exercise the same app boundary as the future product shell.
Its timeline fixture is backed by a synthetic `AppState` sequence, so clip
selection, movement, trimming, and seeking carry stable ids and can refresh the
dock from a new model snapshot after dispatch.

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
represent, and `DeselectAll` clears clip, mask, and animation selection without
entering undo history.
The shared `Action::DeleteSelection` path deletes the current
`AppState::selection.selected_clips` through `remove_clips_bulk`, so shortcuts,
menus, scripts, and self-hosted widgets all reuse the same locked-track checks,
linked clip cleanup, undo snapshot, and timeline modified event behavior.
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
Project open/save dialogs use the same boundary: menu widgets emit app-shell
dialog intents, the window entrypoint resolves them into concrete
`OpenProject(PathBuf)` / `SaveProjectAs(PathBuf)` actions, and `AppState`
performs project lifecycle work plus status reporting. Widget code must not
invent project paths or mutate project files directly.
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
actually changes.
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

## Focused Text Input

Keyboard, text, and IME events route to `FocusManager::focused_widget()`.
`TextInput` requests focus on click, enables IME while focused, stores preedit
composition text, and inserts committed IME text through the same grapheme-aware
editing path as normal text input.

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
menus. Tooltip requests preserve their delay timer when the same tooltip is
reported repeatedly during hover, and tooltip painting clamps to the current
clip rect.

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
popup that needs outside-click dismissal must make `hit_test()` catch the
window while open, then decide in `event()` whether the pointer is inside the
trigger, inside the popup, or outside. Dragging popup internals should use
pointer capture so move/up events remain routed to the owning widget.

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
positive-delta scrolling, but exposes static and value-aware action adapters so
Assets, Effects, presets, and similar panels can bind to editor state outside
the widget crate. The self-hosted Effects panel builds rows from the shared
effect registry and, when a video clip is selected, activates rows through
undoable `AppState::add_effect_to_clip` commands. The `self_hosted_app` and
`ui_demo` Assets/Effects-style panels use this shared surface as the tracer
bullet for migrating list-heavy egui panels.

Timeline migration starts with the domain-light `TimelineView` surface in
`mondrian-ui-widgets`. It renders frame-space tracks, clips, ruler ticks,
playhead, vertical/horizontal scrolling, Ctrl-wheel zoom, clip selection, and
seek actions, but it does not depend on `mondrian-timeline` or mutate editor
state directly. Real timeline panels should map `Sequence` / `Track` / `Clip`
data into `TimelineTrack` / `TimelineClip` view models, then translate
selection and seek callbacks into semantic `Action`s or undoable commands at
the app layer. This keeps the renderer-facing timeline primitive testable while
preserving a clean path for progressively replacing the old egui timeline.

Value widgets stay editor-state agnostic. `Slider`, `Checkbox`, `ColorPicker`,
`ColorPickerTrigger`, and `CurveEditor` expose value-aware action adapters such
as `on_change(...)`, but they do not know about clips, effects, keyframes, or
undo history. Real panels map widget values to semantic `Action`s or command
objects at the panel/app layer. Programmatic state synchronization uses setters
such as `set_color()` / `set_points()` and must not emit actions; only user
input paths dispatch changes and request repaint.

`mondrian-ui-widgets` keeps extreme interaction and visual-command stability in
normal Rust tests. The component stress suite drives edge-size layouts, long
text, dropdown wheel scrolling, pointer-captured slider drags, color-picker
popovers, and timeline scroll/zoom, then asserts that generated paint geometry
is finite and clip/transform stacks remain balanced. This does not replace
human visual QA, but it catches common regressions before manual desktop
testing.

General panel composition should use `FlexContainer` rather than ad-hoc
coordinate code. `FlexContainer` is only a widget adapter over the pure
`mondrian-ui-layout::FlexLayout` algorithm, so layout math remains testable in
the layout crate while panels get normal widget-tree behavior: event routing,
overlay forwarding, hit testing, and child traversal.

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
Checkerboards are low-count deterministic colored-triangle geometry and use
the same rounded mask path when they sit inside rounded swatches. A compact
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
The mode selector shares the generic dropdown's token vocabulary and overlay
behavior, but it remains an internal selector because changing color models is
local widget state rather than an editor `Action`.

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
operating systems and font stacks. Wrapped text is represented as a constrained
text box draw command and resolved by `mondrian-ui-text`, not manually wrapped
inside individual widgets. Tooltip widgets draw border, fill, and text commands
in that order.

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
