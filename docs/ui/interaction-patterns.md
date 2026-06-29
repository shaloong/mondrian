# Interaction Patterns

## Focus

Focus ownership and visible focus indication are separate.

- Keyboard traversal uses `FocusSource::Keyboard` and may show focus rings.
- Pointer focus uses `FocusSource::Pointer` and should not normally show rings.
- Programmatic focus owns input without implying a visible ring.
- Accessibility `focused` follows ownership, not ring visibility.

## Shortcuts

Unmatched shortcuts should fall through by returning ignored/unhandled behavior. This preserves OS tools, input methods, and user-level shortcut managers.

## Menus and Popovers

Menus, context menus, dropdowns, and popovers should share geometry, paint, hover, submenu, keyboard, and overlay behavior.

## Text Input and IME

Text inputs own printable text, composition/preedit/commit, selection, clipboard edits, cursor movement, and platform IME requests while focused.

## Drag and Drop

Pointer capture and drag state are owned by the event router. Widgets request drag/capture through event requests; app/platform code executes side effects.

## Accessibility

Every interactive control should expose role, name, focusable/disabled/focused state, and value where relevant. Icon-only buttons need an accessible name.
