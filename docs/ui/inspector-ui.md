# Inspector UI

Inspector displays and edits properties for current selection.

## Source of Truth

Inspector reads `SelectionState`. It must not keep a parallel selected effect/mask/clip state.

## Sections

Recommended order:

1. Clip summary
2. Built-in transform/opacity/speed/blend/solid color where applicable
3. Effect stack
4. Mask stack
5. Timing or advanced metadata

Adjustment and nested sequence clips may expose a reduced built-in transform set. Domain code decides what is editable.

## Effect Stack

Effects are instances with stable `EffectId`. Reorder mutates the clip effect vector. Enable/bypass/delete/reset actions should be explicit and undoable where they change domain state.

Adding an effect may update selection/navigation immediately; that follow-up selection should not create a second undo entry.

## Built-In Properties

Built-in transform and clip properties are not removable. UI may reset values or hide controls but must not present deletion as if these were ordinary user effects.

## Parameter Controls

Control type should follow `PropertyValue` and UI metadata:

- bool -> checkbox/toggle
- enum/text options -> dropdown
- numeric -> slider/number input
- color -> color picker
- vec2/vec3/vec4 -> grouped numeric controls
- keyframable properties -> keyframe control near the parameter label
