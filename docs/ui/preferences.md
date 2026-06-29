# Preferences

Preferences apply immediately. The closing button should behave like "Close", not "Apply".

## Persistence

App UI preferences are stored in `app_ui_preferences.json` under the app data directory and include:

- version
- theme preference
- workspace preset
- recent projects
- shortcut overrides
- custom workspace layout
- waveform display mode

Invalid or incompatible preferences fall back to defaults.

## Layout

Preferences should use a simple sidebar plus content area. Avoid table-heavy forms for command/shortcut editing.

## Shortcuts

Shortcut preferences should present commands first and shortcuts second:

- searchable command list
- collapsible categories
- keycap display
- click-to-rebind
- conflict resolution
- per-command actions in hover/overflow menu

## Button Groups

Discrete preferences such as theme and waveform display should use a shared segmented/button-group component with tokenized selected state and stable geometry.
