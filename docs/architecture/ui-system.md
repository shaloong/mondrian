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
through each container's `children()` and `children_mut()` methods.

Custom containers must expose their logical children through those methods to
participate in deepest-hit routing and router-level pointer capture. Containers
that manually forward events can continue to work, but they should be migrated
toward transparent children before the main editor panels move to the new UI.

## Focused Text Input

Keyboard, text, and IME events route to `FocusManager::focused_widget()`.
`TextInput` requests focus on click, enables IME while focused, stores preedit
composition text, and inserts committed IME text through the same grapheme-aware
editing path as normal text input.

Text copy/cut shortcuts are consumed by `TextInput` only when a selection exists.
If there is no selection, `Ctrl+C` and `Ctrl+X` are ignored so panel-level
commands, such as copying clips or keyframes, can handle them.

## Pointer Capture

Drag widgets request capture on mouse down and release capture on mouse up.
`Slider`, `DockSplitter`, and text selection depend on this behavior. Future
Timeline clip drags, curve editor handles, and color picker gestures should use
the same request path.

## Rendering Notes

Widgets emit draw commands only. Checkbox checkmarks are vector line commands,
not font glyphs, so they are stable across operating systems and font stacks.
Tooltip widgets draw border, fill, and text commands in that order.
