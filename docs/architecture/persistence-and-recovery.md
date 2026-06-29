# Persistence and Recovery

Persistence is currently handled by `mondrian-app::app::project_lifecycle`.

## Project Container

`.mdp` is a ZIP container containing:

- `manifest.json`
- `project.json`
- `library/index.db`

`manifest.json` declares the current archive contract and document layout.
`project.json` stores `mondrian-project::ProjectDocument`. The SQLite library is
streamed into the archive during save and extracted into the runtime directory
during open.

## Atomic Save

Save writes a temporary archive next to the target and renames it into place. This prevents partially written target files when serialization or archive writing fails.

## Runtime Directory

Each project path maps to a stable temp runtime root:

```text
temp/mondrian-runtime/mondrian_<stem>_<hash>/
```

The runtime root contains extracted library data and autosave snapshots.

## Autosave

Autosave writes timestamped `.autosave.mdp` archives and a manifest:

```text
autosave/manifest.json
autosave/project-<unix_ms>.autosave.mdp
```

Retention keeps a bounded number of recovery points and removes stale entries.

## Recovery

Recovery opens a copied autosave archive, saves it back to the project path, then clears the autosave directory. Recovery should never mutate the autosave source before the recovered project has been saved.
