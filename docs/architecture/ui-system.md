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
`arboard`; other platform hooks remain policy no-ops until their app-shell
behavior is specified.

## Event Requests

Widgets can request side effects while handling an event:

- pointer capture: keep pointer move/up events routed to the same widget during
  drags or text selection.
- IME state: enable or disable text input composition for the focused text
  widget.
- repaint: request another frame for composition previews, cursor blink, or
  delayed UI.

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

## Pointer Capture

Drag widgets request capture on mouse down and release capture on mouse up.
`Slider`, `DockSplitter`, and text selection depend on this behavior. Future
Timeline clip drags, curve editor handles, and color picker gestures should use
the same request path.

Parent-owned chrome that must win over child hit targets, such as
`DockSplitter` handles over tab bars, uses `Widget::before_child_event()`.
Splitter handles paint above children, use a narrower 6px interaction zone by
default, grow in stroke width while hovered or dragged, and span the full
splitter bounds without endpoint gaps.

Slider value mapping uses the same thumb-centered track for painting and
pointer updates. The thumb rect must remain inside widget bounds; if a parent
gives a short row, the thumb shrinks vertically instead of being clipped.

## Overlays

Dropdowns, context menus, and tooltips derive event hit regions and paint
geometry from the same rect helpers. Disabled menu items consume pointer input
without dispatching actions or closing the overlay; outside clicks close open
menus. Tooltip requests preserve their delay timer when the same tooltip is
reported repeatedly during hover, and tooltip painting clamps to the current
clip rect.

Dropdowns request pointer capture while open so outside clicks, Escape, wheel
events, and release events continue to route to the popup even when the pointer
is over another widget. The opening click's release is suppressed so it cannot
accidentally select the first item under the cursor; item actions dispatch only
when a press and release land on the same enabled row. Long dropdown menus clip
their item list and scroll with the same positive-delta-means-content-down
offset convention as `ScrollView`.

Tooltip positions are anchored when the pointer enters a trigger and then
clamped by the tooltip widget to the current clip rect. Repeating the same
tooltip request keeps the original anchor so the popup does not chase pointer
movement. Tooltips draw the popover fill directly without a high-emphasis ring
border.

## Scroll Views

Scroll containers translate pointer events from screen coordinates into child
content coordinates using `screen - viewport_origin + scroll_offset`, matching
the inverse of their paint transform. Wheel events are handled only inside the
viewport and offsets are clamped after wheel input and layout.

`ScrollView` exposes a draggable vertical scrollbar thumb when content
overflows. Thumb drags request pointer capture, map thumb-track movement back
to content scroll offset, and release capture on mouse up. Clicking the
scrollbar track outside the thumb pages the viewport by one visible span.

## Color Input

Color parsing and conversion live in `mondrian-core`, not in the widget layer.
ColorPicker and inspector controls should use the shared HEX/RGBA/HSL/HSV/CMYK
models so text inputs, swatches, and future effect parameters round-trip through
the same math.

## Rendering Notes

Widgets emit draw commands only. Checkbox checkmarks are vector line commands,
not font glyphs, so they are stable across operating systems and font stacks.
Tooltip widgets draw border, fill, and text commands in that order.

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
back-face culling enabled. Line geometry uses square caps so diagonal strokes,
including checkbox checkmarks, do not look clipped at segment endpoints.
