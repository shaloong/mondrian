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

`document_revision` advances only after an explicit project save succeeds. It
is persistence/conflict evidence, not author-semantic identity. Each persisted
Sequence owns an independent monotonic `SequenceRevision`; Playback, Preview,
audio compilation, and render caches bind that revision, so an edited draft or
failed save cannot continue under an older Timeline identity.

`ProjectSettings.color_management.engine` is the sole persisted product-mode
selector for Mondrian Standard, ACES, or Custom OCIO. Sequence workflow stores
only the rendering-domain choice (SceneReferred by default for Standard picture
formation, or an explicit DisplayReferred colorimetric bypass); it must not
duplicate an ACES mode flag.
Archive round-trip tests resolve the reopened root program context and require
the default Standard project to retain the version-pinned Standard View intent.
An explicitly DisplayReferred project retains its direct colorimetric intent.
The new-project draft edits this same `ProjectSettings` value directly and
offers Mondrian Standard, the two version-pinned ACES 2.0 presets, and Custom
OCIO. Custom file selection must validate the config and pin its complete
identity before replacing the draft engine; cancellation or validation failure
leaves the previous engine intact.
Document validation applies each sequence's effective project/sequence engine
to its working-space setting. A Custom OCIO mismatch fails before archive save
or open, while the application performs the same check before project creation
and before atomically recording a sequence-settings command. This keeps invalid
processor routes out of both persistent documents and undo history.
The same rule applies to Mondrian Standard: v1 fixes Linear Rec.2020 in its
persisted package identity, so a Standard document with another sequence
working space is invalid rather than an undocumented alternate Standard path.
Project color-mode replacement is a narrow project action carrying one complete
`ColorEngine`. `AppState` first validates every inheriting sequence against a
candidate `ProjectColorManagement`, then loads the exact OCIO config, replaces
the engine, stops playback, refreshes preview access identity, and saves. A
validation/load/save failure leaves or restores the previous project engine;
the UI never mutates renderer state directly.
The editor exposes this action through File -> Project Settings. Its transient
draft starts from the persisted project engine and uses the active sequence's
working space only to build a reproducible Custom OCIO identity; `AppState`
still validates every inheriting sequence before accepting that identity.

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

SQLite schema v2 stores an `audio_components` catalog beside probe metadata and
user interpretation. The catalog owns stable logical audio Component IDs,
conservative physical-stream signatures, and the source fingerprint that
authorized them. Timeline and Project JSON never persist FFmpeg stream indices;
Export copies validated reachable selections into its immutable execution
snapshot. Explicit stream repair is committed atomically with refreshed probe
metadata and fingerprint evidence while preserving the logical Component ID.
Automatic reconcile does not retarget an existing ID or create physical-stream
aliases; only an explicit rebind may alias a stream, so another Project's stable
reference never has to be deleted as a side effect of repair. A separate refresh
operation re-probes and conservatively reconciles current candidates without
retargeting; this is required before selecting a newly discovered stream whose
index was absent from stored metadata.

## Format Evolution

The current document layout intentionally stays single-document:

```text
manifest.json
project.json
library/index.db
```

Archive `format_version`, document `schema_version`, and embedded library
`PRAGMA user_version` are independent contracts. `manifest.json` records the
expected library schema version in addition to archive layout. A v2 manifest
is written with that field explicitly.

Archive and document JSON pass through separate version registries before typed
deserialization. During Alpha there are deliberately no legacy document steps:
schema v12 is the sole accepted author schema, and older/future versions fail
instead of being guessed. Version 5 introduced the explicit tagged
`mondrian_standard` / `aces` / `custom_ocio` project contract and makes every
Mondrian Standard package-identity field mandatory: product ID/version, config
ID/SHA-256, complete package SHA-256, and working-space ID/version. Custom OCIO
likewise requires config and processor-graph
digests, working/display/view/look identities, roles, and an explicit dynamic
property list; a mutable source path alone is not a project color definition.
Version 6 removes the redundant persisted `ColorWorkflow::Aces` and reserves
Standard/ACES/Custom mode selection for `ProjectColorManagement.engine`.
The current application creates sequences as SceneReferred so Mondrian Standard
is the default Program Output View; DisplayReferred remains an explicit bypass
and the persisted enum keeps workflow separate from engine selection. Version 7 replaces the ambiguous
single default-View identity with mandatory, independently typed SDR and
1000-nit HDR View Transform IDs/versions. The Alpha format intentionally
provides no alias, fallback, or migration from v5/v6; a missing or edited
identity, missing parameter schema, or retired workflow fails closed. Version 8
separates stable parameter identity from instance addresses. Version 9 makes
value type, definition default, automation capability, unit/range,
Hold/Linear/Bezier execution semantics, enum/resource intent, and cache impact
one shared visual/audio `ParameterSchema`; audio Processor instances persist a
schema snapshot beside their exact curve. The registry remains the explicit
seam for adding a real migration policy only when compatibility becomes a
product promise.
Version 10 adds the nonzero persisted Sequence author revision and makes author
identity validation fail closed for duplicate identities and dangling strong
Clip references.
Version 11 replaces the independent `video_display_format` and
`start_timecode_frame` fields with mandatory `timeline_display`. The resolved
contract is shared by Viewer and Timeline, permits signed origins, validates
drop-frame against the exact Sequence rate, and removes unimplemented
Feet+Frames values from persisted author data.
Version 12 replaces the closed three-value audio-layout enum with one canonical
signal-layout value: Mono, a validated named-speaker set, or a bounded Discrete
bus. Standard Stereo and surround layouts serialize by semantic positions, so
5.1(side), 5.1(back), and equal-count custom layouts cannot alias. Alpha does
not guess a v11 layout migration.

SQLite schema ownership remains in `mondrian-assets`; the current version is
v2. Its ordered Registry uses
`PRAGMA user_version`, applies each step in a transaction, validates the current
tables/columns, and rolls back both DDL and version on failure. The former
best-effort `ALTER TABLE` calls that discarded errors have been removed.

Future split-entry layouts require an archive migration and new
`format_version`; persisted editing fields require an explicit schema decision.
SQLite migrates only in the extracted runtime copy. The source `.mdp` is never
rewritten by open.

Current document schema v12 persists canonical rational `TimelineTime` values
directly and requires the shared visual/audio `ParameterSchema`. It does not
contain frame-oriented `TimeCode`, `TimeTicks`, descriptor-level duplicate
defaults/types, editor-preset interpolation capabilities, or compatibility
aliases. Alpha documents from earlier schemas are rejected rather than silently
deriving parameter identity or parameter definition fields.

The checked current document fixture lives under
`crates/mondrian-project/tests/fixtures/current`; the SQLite upgrade fixture
remains under `crates/mondrian-assets/tests/fixtures/v0`.
