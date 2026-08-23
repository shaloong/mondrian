# Layout and Docking

Docking is the workspace shell for editor panels.

## Concepts

- Panel: product module such as Assets, Viewer, Timeline, Inspector.
- Dock area: split tree that owns panel slots.
- Tab group: one slot with multiple panel tabs.
- Splitter: draggable divider between split children.
- Workspace preset: named initial layout.
- Custom workspace: serialized user layout.

## Persistence

`AppUiWorkspaceLayout` persists custom layout in app UI preferences. Loaded layouts must be sanitized; invalid custom layouts fall back to the Editing preset.

## Ownership

Panel local UI state must use stable panel identity/owner, not visible title text. Renaming or translating labels must not lose panel state.

## Drag and Drop

Dock drag previews and drop zones should be overlay-rendered and should not be clipped by panel content. Drop operations mutate layout state, not app domain state.

## Splitters

Splitter hit targets may be larger than the visible line. Splitter geometry should be stable and pixel-snapped where possible.
