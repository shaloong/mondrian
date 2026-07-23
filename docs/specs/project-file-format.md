# Project File Format

Mondrian project files use `.mdp`, a ZIP container.

## Current Entries

```text
manifest.json
project.json
library/index.db
```

`manifest.json` serializes:

```json
{
  "format": "mondrian-project",
  "format_version": 1,
  "document_layout": "single-project-json",
  "project_entry": "project.json",
  "library_entry": "library/index.db",
  "library_schema_version": 2
}
```

`project.json` serializes the canonical `ProjectDocument`:

- `schema_version: u32`
- `project_id: ProjectId`
- `document_revision: u64`
- `meta: ProjectMeta`
- `settings: ProjectSettings`
- `color_environment: ProjectColorEnvironment`
- `new_sequence_defaults: SequenceSettings`
- `sequences: SequenceCollection`
- `proxy_mode_assets: BTreeSet<AssetId>`

`library/index.db` is the project asset library.

The current independent versions are archive v1, document schema v20, and
library schema v2. Schema v18 added the complete closed Basic Title author
payload. Schema v19 establishes one mandatory Project color environment, one
complete future-Sequence template, engine-free Sequence color semantics, and
per-placement nested color processing. Schema v20 adds mandatory
`clip_time_in`, the persistent origin for one shared Clip-local visual
automation domain independent of placement and source sampling. During Alpha,
document schemas other than the exact current version are rejected because no
compatibility migration is promised yet.

## Required Evolution Rules

- During alpha, incompatible format changes may intentionally reject older files.
- Any archive layout change must increment `format_version`.
- Any document-shape change must increment `schema_version`.
- Unsupported versions must fail loudly instead of falling back to guessing.
- Unknown asset paths should remain recoverable through relink metadata where possible.
- Library DB migrations must be idempotent and safe to run on every open.
- Caches, proxies, thumbnails, waveform data, and render-preview data must not be
  required for `.mdp` validity.

## Layout Direction

Mondrian currently follows a Premiere-style single project document. Do not split
sequences/assets/settings into multiple archive entries unless there is a hard
technical need such as lazy sequence loading, local autosave, collaboration,
partial recovery, or measurable save/load bottlenecks.

## Save Semantics

Save captures one immutable Authoring Session generation and an SQLite online
backup. It writes and flushes a unique sibling archive, reopens and validates
it, then crosses one platform atomic replace/create boundary. A failed
serialization, database backup, validation, flush, or replacement leaves the
existing target untouched. Autosave uses the same archive publication rule and
publishes its manifest only after the referenced archive is durable.
