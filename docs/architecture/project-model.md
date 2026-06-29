# Project Model

The project model is split between lightweight project metadata/settings and runtime/editor state.

## Persistent Project Container

`mondrian-app::app::ProjectFile` is currently the saved project payload inside `.mdp`:

- `name`
- `sequences: SequenceCollection`
- `project_settings: ProjectSettings`
- `proxy_mode_assets: Vec<AssetId>`
- embedded `library/index.db`

`mondrian-core::Project` contains top-level metadata and settings, but the active app save path currently serializes `ProjectFile` from `AppState`.

## Runtime State

`AppState` owns:

- active sequence and sequence collection
- active/default sequence IDs and navigation stack
- project path and runtime directory
- project settings
- asset library handle
- selection state
- playback/audio state
- render queue/export draft
- clipboard and animation selections
- UI-facing status log

Runtime-only state must not leak into project JSON unless it is part of the project contract.

## Selection

`SelectionState` is the single source of truth for selected tracks, clips, effect, and mask. Inspector, timeline, viewer, and node graph must coordinate through this state rather than keeping panel-local copies.

Selection/navigation updates that follow a user command may be immediate UI state changes and should not create a second undo entry unless the selected object itself is edited.

## Asset Library

The project runtime directory contains a SQLite asset library. On save, `library/index.db` is streamed into the `.mdp`; on open it is extracted into the runtime directory. Timeline clips reference assets by `AssetId`.

Generated assets such as adjustment layers and solid colors are represented as library records with synthetic `mondrian://...` paths.
