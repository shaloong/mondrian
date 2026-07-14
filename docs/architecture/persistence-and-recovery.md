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

Save writes a temporary archive next to the target, reopens and validates the
new document, stages the prior target as a sibling backup, and then renames the
validated archive into place. If replacement fails after staging, the original
is restored before the error returns. Serialization, ZIP writing, validation,
and replacement failures therefore do not intentionally delete or truncate the
source project.

Archive open migrates JSON in memory and extracts SQLite through a temporary
sibling file. A failed or missing library entry leaves an existing runtime DB
and the source archive unchanged. SQLite migrations subsequently run
transactionally against this runtime copy; migration never edits the archive in
place. Saving and reopening is the only path that persists the current versions.

The current archive fixture pins the exact Mondrian Standard package digest
serialized by `ProjectColorManagement`. A package content change deliberately
invalidates that fixture until it is regenerated for the new current contract;
unknown/retired digests are rejected rather than accepted through an implicit
color migration or substituted with the latest package.

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
