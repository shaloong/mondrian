# Shortcut Spec

Shortcuts resolve through `ShortcutManager` and app UI command descriptors.

## Binding

`ShortcutBinding` is:

- `key: KeyCode`
- `modifiers: Modifiers`

Helper constructors exist for key-only, Ctrl, and Ctrl+Shift.

## Scope Priority

Resolution priority is:

```text
Widget > Panel > Workspace > Global
```

Context includes the focused widget and focused panel.

## Command Registry

Built-in shortcuts should be declared in `app_ui::commands`. Menus, preferences, command palette, and future plugins should read the same descriptors.

## Unmatched Shortcuts

Shortcut-like chords that have no binding are ignored by the app and may be counted in diagnostics. They should not be swallowed merely because they look like shortcuts; this allows OS/input-method/tool shortcuts to continue working.

## Text Input

Focused text inputs own printable text, IME commits, selection, copy/cut/paste editing, and navigation keys according to widget behavior. Shortcut resolution must not steal ordinary text input.
