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

Archive `format_version`, document `schema_version`, and embedded library
`PRAGMA user_version` are independent contracts. `manifest.json` records the
expected library schema version in addition to archive layout. A v1 manifest
is written with that field explicitly.

Archive and document JSON pass through separate version registries before typed
deserialization. During Alpha there are deliberately no legacy document steps:
schema v4 is the sole accepted author schema, and older/future versions fail
instead of being guessed. Version 4 keeps the explicit tagged
`mondrian_standard` / `aces` / `custom_ocio` project contract and makes every
Mondrian Standard package-identity field mandatory: product ID/version, config
ID/SHA-256, complete package SHA-256, working-space ID/version, and default View
Transform ID/version. The Alpha format intentionally provides no alias,
fallback, or migration from v3; a missing or edited identity fails closed. The
registry remains the explicit seam for adding a real migration policy only when
compatibility becomes a product promise.

SQLite schema ownership remains in `mondrian-assets`. Its ordered Registry uses
`PRAGMA user_version`, applies each step in a transaction, validates the current
tables/columns, and rolls back both DDL and version on failure. The former
best-effort `ALTER TABLE` calls that discarded errors have been removed.

Future split-entry layouts require an archive migration and new
`format_version`; persisted editing fields require an explicit schema decision.
SQLite migrates only in the extracted runtime copy. The source `.mdp` is never
rewritten by open.

Schema v2 persists canonical rational `TimelineTime` values directly. It does
not contain frame-oriented `TimeCode`, `TimeTicks`, or compatibility aliases.

The checked current document fixture lives under
`crates/mondrian-project/tests/fixtures/current`; the SQLite upgrade fixture
remains under `crates/mondrian-assets/tests/fixtures/v0`.
