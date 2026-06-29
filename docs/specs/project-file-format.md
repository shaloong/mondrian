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
  "library_entry": "library/index.db"
}
```

`project.json` serializes the canonical `ProjectDocument`:

- `schema_version: u32`
- `project_id: ProjectId`
- `document_revision: u64`
- `meta: ProjectMeta`
- `sequences: SequenceCollection`
- `settings: ProjectSettings`
- `proxy_mode_assets: Vec<AssetId>`

`library/index.db` is the project asset library.

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

Save writes a temporary archive and renames it into place. The project archive must never be left half-written after a failed save.
