---
status: accepted
---

# Use one transactional Authoring Session and explicit durable publication boundaries

## Decision

One open Project has exactly one `AuthoringSession`. It owns the canonical
mutable `ProjectDocument`, Asset Library authority, Sequence navigation,
project-wide bounded Undo/Redo, process-local `AuthorGeneration`, and the last
successful manual and autosave coverage. `AppState` composes that Session with
runtime execution and UI state but does not mirror Project fields.

Production mutations operate on detached Sequence or complete Project
candidates. The Session validates the candidate, advances affected
`SequenceRevision` values and `AuthorGeneration`, prepares bounded History, and
installs all authoring authorities as one transaction. Failure before install
changes none of them. Undo and Redo cross the same validation boundary and
always assign new revisions. The direct mutable fixture seam is test-only.

The complete detached `ProjectDocument` remains the Project mutation
Interface. History is a retention representation, not a second mutation model:

- Sequence commands retain typed before/after
  `AuthoringSnapshot<Sequence>` endpoints.
- Structural Project commands retain typed before/after
  `AuthoringSnapshot<ProjectRestorePoint>` endpoints containing Project-owned
  author fields, canonical Sequence order/default, a structural navigation
  fallback, and only the affected Sequence bodies or absence states.
- The affected set must equal the validated body/presence difference.

Project Undo/Redo verifies the current source endpoint, reuses unaffected
Sequence allocations, materializes one complete target candidate, and validates
it before commit. Successful-publication evidence
(`document_revision` and `ProjectMeta.updated_at`) remains current. Active
navigation also remains current while its target exists; the captured fallback
is used only when necessary. History therefore restores typed author state
across one contiguous segment; it is not JSON restoration or command replay.

Validation evidence is opaque, process-local, and non-serialized. The Session
retains one Project certificate composed from exact Sequence author-contract
certificates and one dependency certificate. Incremental validation may reuse
only evidence strongly anchored to an unchanged immutable COW subtree and the
same Project color environment; allocation identity alone is not evidence.
Identity/link, Transition, Audio Program, nesting/output, and cycle rules still
fail closed at the author transaction boundary. Certificates never enter
History, persistence, execution snapshots, or author JSON.

History charges the deduplicated union of retained immutable COW allocations
across Undo and Redo under its entry and byte budgets. Preparation accounts for
branch discard, insertion, and eviction before commit. If a committed edit
cannot be retained, it becomes an explicit History barrier: older Undo and
branch Redo are cleared rather than permitting restoration across an
unrecorded state.

`AuthorGeneration`, persisted `SequenceRevision`, and
`ProjectDocument.document_revision` have separate meanings:

- Author Generation identifies a committed in-memory Project state in one open
  lifetime.
- Sequence Revision conservatively invalidates execution derived from one
  Sequence.
- Document Revision records successful manual archive publication.

Persistence consumes one immutable `AuthoringSnapshot` binding the
`AuthoringSessionId`, Author Generation, Asset Library revision, validated
document, and library authority. A dedicated worker creates a consistent SQLite
backup, builds and validates the Project archive, and publishes it through the
single domain-free durable-publication Seam defined by
[Storage Publication](../architecture/storage-publication.md). UI code never
serializes a live document or database.

Each artifact crosses exactly one irreversible namespace-publication boundary.
A workflow may order several artifacts: Recovery Manifest publication follows
its referenced archive, and only the final authority-bearing publication
establishes the workflow's durable state. Publication intent is fixed at
admission (`CreateNew` or `ReplaceExisting`). Confirmed durability,
durability-unconfirmed establishment, namespace-indeterminate state, and
pre-namespace failure remain distinct; only confirmed durability advances the
saved baseline.

Machine-local runtime payload is allocated by normalized Project publication
identity plus `ProjectId` and is bound by the current schema-v3 owner manifest
before library or autosave use. Recovery-bearing payload lives under stable
per-user state, never process-temporary storage. The owner manifest proves
filesystem allocation only; it does not replace the Recovery Manifest or
authoring generations. Root reuse, child mutation, and cleanup fail closed
unless owner identity validates.

One live `ProjectRuntimeLease` composes independent kernel-backed exclusions for
the logical Project ID, every admitted publication target, and the exact runtime
root. Path presence, PID text, or a retained hash is not authority. Production
mutation receives the lease rather than a bare path, and revalidates namespace
identity before use. Platform spelling and filesystem mechanics belong to
[Persistence and Recovery](../architecture/persistence-and-recovery.md) and
[Storage Publication](../architecture/storage-publication.md).

Production Open consumes one `PreparedProjectArchive` over a retained archive
file object. It enforces the exact entry set and caller-selected read budgets,
validates the Manifest and canonical Project once, acquires or verifies the
Runtime Lease from the stable Project ID, and extracts the CRC-checked SQLite
Library into one uninstalled generation. Candidate failure leaves the current
Session untouched. An installed Library generation remains named and immutable
until weak lifetime evidence proves every external reference has retired.

Recovery begins from one complete manifest-backed candidate containing Project
identity, exact runtime root, paths, revisions, timestamp, and archive hash.
Preflight is not admission authority. After lease acquisition, recovery
revalidates the complete entry and source, creates and flushes an exclusive
staging copy, verifies it independently, and loads the exact retained staging
file object into a new dirty Session. Cleanup is confined to the quiesced lease
and never deletes an autosave without a valid Manifest proving it unreferenced.
Manual publication retires recovery authority only when its completion still
covers the current author and Library generations.

Runtime lease authority is not Session freshness. Before Open, Recovery, Close,
or replacement crosses the Session boundary, persistence closes admission for
that exact `AuthoringSessionId` and waits for a FIFO barrier behind all admitted
requests. Candidate failure resumes the exact paused generation; success
retires it. Completions bind Session ID, request ID, persistence generation, and
scalar lease identity without retaining Library or Lease ownership.

Manual publication additionally binds one Session-scoped destination revision.
Save As advances it only after queue admission, and later ordinary Save reuses
that destination while publication is in flight. A stale destination,
retired Session, or superseded persistence generation may publish its own
snapshot but cannot change the canonical path, clear newer dirty state, replace
author metadata, or reconcile Recovery Authority.
`AuthoringSession::mark_saved` separately verifies the captured Author
Generation, Asset Library revision, and authored metadata before accepting only
the publication-owned timestamp.

## Consequences

- A command error cannot leave a half-edited canonical Project that is later
  saved accidentally.
- Undo spans Sequence and Project operations without replaying history against
  the wrong aggregate or rolling back durable-publication evidence.
- Execution and persistence consume immutable snapshots without holding the
  authoring lock or depending on Widgets.
- Oversize or disabled History retention is explicit and cannot create an
  invisible hole across which older state is restored.
- Open, recovery, save, autosave, and cleanup share one durable-publication and
  runtime-authority model; no Adapter may infer success or ownership from paths.
- New production features mutate author state only through Session
  transactions; unreachable-state construction remains test-only.
