# Project Model

Mondrian uses a Premiere-style lightweight project document. The project file
stores editing decisions, project settings, sequence structure, and asset-library
metadata. Rebuildable caches, proxies, waveforms, thumbnails, and preview renders
must live outside `.mdp`.

## Persistent Project Document

`mondrian-project::ProjectDocument` is the canonical saved project payload inside
`.mdp`:

- `schema_version`
- `project_id`
- `document_revision`
- `meta: ProjectMeta`
- `settings: ProjectSettings`
- `sequences: SequenceCollection`
- `proxy_mode_assets: Vec<AssetId>`

`mondrian-core` keeps only shared project metadata and settings types. It does
not define a second top-level project container.

## Runtime State

`AppState` owns:

- active sequence and sequence collection
- active/default sequence IDs and navigation stack
- project id, metadata, and current document revision
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

## Format Evolution

The current document layout intentionally stays single-document:

```text
manifest.json
project.json
library/index.db
```

Future split-entry layouts must be introduced by a new `format_version` in
`manifest.json`. The existing alpha loader rejects unsupported layouts instead of
silently guessing.
