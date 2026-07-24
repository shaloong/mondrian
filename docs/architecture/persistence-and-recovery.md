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
version axes. The current values are archive v1, document v21, and library v2.
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
The identity is printable for structured runtime and Headless evidence, but
remains opaque: callers may compare or record it and must not derive ordering,
Project identity, or persisted author semantics from its UUID representation.

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
autosave/project-<unix-ms>-g<generation>.autosave.mdp
```

The manifest is a strict schema-v1 document. Its header records exact
`ProjectId` and Session-independent source path; every entry records archive
path, timestamp, author generation, asset-library revision, embedded document
revision, and SHA-256. Entry paths must be direct children of the Project
`autosave` directory and end in `.autosave.mdp`. Publication reopens the
archive, verifies its Project/document identity, hashes its bytes, and
atomically replaces the manifest. Previously admitted entries are normalized
by bounded manifest metadata and path checks during publication; they are not
all rehashed on every autosave. Discovery and exact selection admission perform
the full hash/archive/Project validation. Retention first commits the
replacement manifest and only then removes archives it no longer references. A
crash or manifest-publication failure therefore leaves the previous manifest
and every archive it authorizes intact. A successful autosave advances
`autosaved_generation`, not the manual save baseline; it must not make the
title bar or close guard report “saved”.

All in-process manifest mutations share one recovery publication lock, while
the durable file replacement remains the cross-crash boundary. `ProjectId`, not
the mutable path, admits a queued autosave into an existing manifest; autosave
publication never rewrites the header path. Only a current manual durable
baseline authorizes rebinding that locator or retiring covered snapshots; a
later covered autosave completion may finish reconciliation interrupted around
the manual completion. Thus an autosave captured before Save As cannot flip the
manifest back after the new canonical path has been published.

## Recovery

The deep Project Recovery Module owns manifest publication, retention,
discovery, selection admission, and retirement. Discovery treats a recovery
point as a candidate, not as canonical state. It rejects unknown manifest
fields, unsafe paths, hash/revision/Project mismatches, invalid archives, and a
candidate whose existing canonical Project archive has another `ProjectId`.
Selection is admitted only when the exact archive remains in the canonical
manifest. Recovery copies that immutable source to a staging archive and opens
it into a new dirty `AuthoringSession`; it never edits or deletes its source
first.

A manual save retires recovery authority only when its completion still covers
the current author generation and Asset Library revision. Retirement first
atomically publishes an empty canonical manifest and then removes the old
archives. A stale asynchronous save completion cannot clear newer recovery
authority. If that completion is Save As, it atomically rebinds retained
recovery authority from the previous canonical path to the newly published path
without deleting it. The live runtime root remains stable for the Authoring
Session because it owns the open SQLite database; recovery admission therefore
derives that authority from the selected manifest location instead of
recomputing it from a possibly changed Project path. A recovered Session keeps
that runtime authority until a covering save retires it. Cleanup or rebind
failure after a successful Project publication is reported as a recovery
warning, not as a false claim that the irreversible Project save failed.
Conflicts, permission failures, disk-full errors, invalid candidates, and
failed retirement remain explicit and retryable.

The runtime directory is initially derived from the Project path and contains
the extracted library plus recovery state:

```text
temp/mondrian-runtime/mondrian_<stem>_<path-hash>/
```

Archive open extracts the SQLite entry through a temporary sibling and opens
the resulting runtime database transactionally. Failure leaves both an existing
runtime database and the source archive unchanged. SQLite migrations operate on
the extracted runtime copy only; saving is the sole path back into `.mdp`.

## Current Document Schema

Schemas v5–v16 establish the pinned color-engine, parameter, exact-time, audio
layout/matrix/route, exact Samples, and canonical proxy-mode contracts described
in the corresponding architecture documents. Schema v17 establishes:

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

Schema v18 adds the closed
`BasicTitle` Clip content payload and persists its complete canonical
definition-backed Property Bag. Project validation requires the exact property
set, Parameter Schemas, value domains, and animation state. Text/font/alignment
author intent therefore round-trips without a renderer-only `TextLayer`, fake
Asset record, or reopen-time defaults.

Schema v19 makes the Project color engine/config the single shared engine,
persists the complete new-Sequence template, keeps each Sequence's Program
semantics explicit, and places nested color-boundary policy on the nesting
Clip. Schema v20 makes Clip-local visual author time independent of placement
and source time. Schema v21 is the current clean Alpha author contract: it
separates closed Sequence `color` and `delivery` structures so working/input/
Program Output semantics cannot be confused with encoded bit depth, range, HDR
metadata, or chroma defaults. Every old/future schema and unknown author field
fails closed during Alpha.

The current fixture in `crates/mondrian-project/tests/fixtures/current` must
open idempotently, save, reopen, and retain its semantic fingerprint. Dedicated
round-trip tests additionally cover visual parameters, mask animation, audio
processor schema/curves, Basic Title animation, exact Transition/link
relationships, and SQLite content. A schema number is not advanced unless
these required semantics are represented and validated.
