# Persistence and Recovery

Persistence is a UI-independent publication pipeline. The canonical mutable
author state lives only in `mondrian-editor-state::AuthoringSession`; UI code
requests a save but never serializes a live document or SQLite connection.

## Project Container

`.mdp` is a ZIP container containing:

- `manifest.json`: archive format, document layout, entry names, and embedded
  library schema version;
- `project.json`: one validated `mondrian-project::ProjectDocument`;
- `library/index.db`: one transactionally snapshotted SQLite asset library.

Rebuildable proxies, thumbnails, waveforms, decoded frames, prepared render
plans, plugin runtime state, UI navigation, and device handles never enter the
archive.

The archive format, Project document schema, and SQLite schema are independent
version axes. The current values are archive v1, document v17, and library v2.
An archive is accepted only when all three declarations match their registered
contracts. During Alpha, old and future document schemas fail closed; absence
of a migration is explicit and is never replaced by broad serde defaults.

## Author Snapshot and Request Identity

`AuthoringSession::snapshot` captures one immutable persistence input:

- `AuthoringSessionId`: the process-local identity of this open lifetime;
- monotonic `AuthorGeneration`;
- exact asset-library database revision;
- a cloned, fully validated `ProjectDocument`;
- the library authority used to create a consistent SQLite backup.

`AuthorGeneration` identifies committed in-memory state. It is distinct from
persisted `SequenceRevision` (execution invalidation) and
`document_revision` (successful manual-file publication evidence). A save
request also receives a monotonic request ID. Completion is accepted only by
the still-open matching Session and request; reopening the same Project ID does
not make a completion from the previous lifetime valid.

Manual save, Save As, and autosave all enter the dedicated
`ProjectPersistenceService`. The worker first uses SQLite's online backup API
to create a self-consistent database snapshot, then writes the archive from the
immutable document and that database. It never copies a live WAL database as a
set of ordinary files. UI, playback, and rendering remain independent of ZIP
compression and filesystem latency.

Manual close/quit waits for its required save result. Autosave remains
asynchronous and coalesced by the App so it cannot create an unbounded request
queue.

## Durable Atomic Publication

The archive is written to a unique sibling temporary file, completed as a ZIP,
flushed with `sync_all`, reopened, and validated before publication. Publication
uses one platform atomic replacement boundary:

- Windows replaces an existing target with `ReplaceFileW` and creates a new
  target with `MoveFileExW`; both use write-through flags.
- Unix uses same-directory `rename` and then synchronizes the parent directory.

There is no “rename old file away, then hope the new rename succeeds” window.
A serialization, database backup, ZIP, validation, flush, or replacement error
leaves an existing Project target untouched. Temporary files are best-effort
removed after failure. `write_durable_file_atomically` applies the same rule to
recovery manifests and other Project-adjacent indexes.

Successful completion reports the exact Session, request, author generation,
asset-library revision, resulting document revision, Project metadata, and
published path. `AuthoringSession::mark_saved` advances the durable baseline
only monotonically. If the user edited after the captured snapshot, the older
save may complete and remain a valid recovery artifact but cannot clear dirty
state or overwrite newer in-memory metadata. Failed requests remain dirty and
may be retried.

## Autosave

Autosave publishes immutable `.autosave.mdp` recovery points under the stable
Project runtime root. The manifest is published only after its referenced
archive has been durably published:

```text
autosave/manifest.json
autosave/project-<request-id>-<generation>.autosave.mdp
```

The manifest records Project identity, Session-independent source path,
author generation, asset-library revision, timestamp, and archive path. A
manifest can therefore never advertise a half-written archive. Retention is
bounded and removes only recovery points not referenced by the newly committed
manifest. A successful autosave advances `autosaved_generation`, not the manual
save baseline; it must not make the title bar or close guard report “saved”.

## Recovery

Discovery treats a recovery point as a candidate, not as canonical state. It
validates the manifest and archive, exposes source/timestamp/generation to the
product UI, and opens the candidate into a new `AuthoringSession`. Recovery
never edits or deletes its source first. The candidate is cleared only after a
manual Project publication succeeds at the chosen destination. Conflicts,
permission failures, disk-full errors, and an invalid candidate remain visible
and retryable.

The runtime directory is derived from the Project path and contains the
extracted library plus recovery state:

```text
temp/mondrian-runtime/mondrian_<stem>_<path-hash>/
```

Archive open extracts the SQLite entry through a temporary sibling and opens
the resulting runtime database transactionally. Failure leaves both an existing
runtime database and the source archive unchanged. SQLite migrations operate on
the extracted runtime copy only; saving is the sole path back into `.mdp`.

## Current Document Schema

Schemas v5–v15 establish the pinned color-engine, parameter, exact-time, audio
layout/matrix/route, and exact Samples contracts described in the corresponding
architecture documents. Schema v16 replaces proxy-mode `Vec<AssetId>` state
with a canonical ordered set so duplicate or order-dependent author state is
unrepresentable.

Schema v17 is the current clean Alpha author contract:

- `ClipContent` is a closed payload; legacy parallel kind/asset/nested/color
  fields and unknown Clip fields are rejected;
- Clip synchronization uses multi-member `ClipLinkGroupId` membership rather
  than pair pointers;
- Sequence persists explicit typed visual Transitions with strong endpoint
  references and exact ranges;
- mask scalar Property Bags are persisted and validated rather than rebuilt as
  defaults after reopen;
- copied, split, and duplicated author graphs must have disjoint instance
  identities while retaining intended external references.

The current fixture in `crates/mondrian-project/tests/fixtures/current` must
open idempotently, save, reopen, and retain its semantic fingerprint. Dedicated
round-trip tests additionally cover visual parameters, mask animation, audio
processor schema/curves, exact Transition/link relationships, and SQLite
content. A schema number is not advanced unless these required semantics are
represented and validated.
