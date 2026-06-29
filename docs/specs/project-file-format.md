# Project File Format

Mondrian project files use `.mdp`, a ZIP container.

## Current Entries

```text
project.json
library/index.db
```

`project.json` serializes:

- `name: String`
- `sequences: SequenceCollection`
- `project_settings: ProjectSettings`
- `proxy_mode_assets: Vec<AssetId>`

`library/index.db` is the project asset library.

## Required Evolution Rules

- Add a top-level schema version before any incompatible project-file change.
- New fields must use serde defaults until alpha migration tooling exists.
- Remove fields only with an explicit migration path.
- Unknown asset paths should remain recoverable through relink metadata where possible.
- Library DB migrations must be idempotent and safe to run on every open.

## Save Semantics

Save writes a temporary archive and renames it into place. The project archive must never be left half-written after a failed save.
