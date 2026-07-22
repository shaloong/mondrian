---
status: accepted
---

# Use one transactional Authoring Session and one durable publication boundary

## Decision

One open Project has exactly one `AuthoringSession`. It owns the canonical
mutable `ProjectDocument`, Asset Library authority, Sequence navigation,
project-wide bounded Undo/Redo, process-local `AuthorGeneration`, and the last
successful manual/autosave coverage. `AppState` composes the Session with
runtime execution and UI state but does not mirror Project fields.

Production mutations operate on a detached Sequence or Project candidate. The
Session validates the complete candidate, advances affected
`SequenceRevision` values and `AuthorGeneration`, records the bounded history
entry, and installs the candidate as one transaction. A failure before install
changes none of those authorities. Undo and Redo restore author content through
the same validation boundary and always assign new revisions. The direct
mutable fixture seam is unavailable to production commands.

`AuthorGeneration`, persisted `SequenceRevision`, and
`ProjectDocument.document_revision` have separate meanings:

- Author Generation identifies a committed in-memory Project state in one open
  lifetime;
- Sequence Revision conservatively invalidates execution derived from one
  Sequence;
- document revision records successful manual archive publication.

Persistence consumes an immutable `AuthoringSnapshot` containing
`AuthoringSessionId`, Author Generation, Asset Library revision, validated
document, and library authority. A dedicated worker creates a SQLite online
backup, writes and validates a sibling archive, flushes it, and crosses one
atomic filesystem publication boundary. Windows uses `ReplaceFileW` or
`MoveFileExW` with write-through; Unix uses same-directory rename plus parent
directory synchronization. Recovery manifests use the same durable-file
primitive and are published only after their referenced archive.

Completions bind Session ID and request ID. A stale generation can publish its
own snapshot but cannot clear dirty state or overwrite newer in-memory metadata;
a completion from a closed/reopened Session is ignored. Autosave advances only
recovery coverage. Manual close/quit waits for a required save result rather
than treating queue admission as saved.

## Consequences

- A command error cannot leave a half-edited canonical Project that later gets
  saved accidentally.
- Undo spans Sequence navigation and Project-level operations without replaying
  an active-Sequence history against the wrong aggregate.
- Execution and persistence receive immutable snapshots and can run without
  holding the authoring lock or depending on Widgets.
- Oversize history entries are explicit: the edit may commit, but diagnostics
  report that it was not retained.
- Tests that construct otherwise unreachable states use a clearly test-only
  seam; new production features must use Session transactions.
