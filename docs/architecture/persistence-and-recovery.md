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
version axes. The current values are archive v1, document v25, and library v5.
Library v5 canonicalizes persisted native audio layout evidence as exact,
unspecified, or unsupported; its v4 migration is a field-scoped transactional
JSON rewrite and never interprets asset names or stream labels. An archive is
accepted only when all three declarations match their registered
contracts. During Alpha, old and future document schemas fail closed; absence
of a migration is explicit and is never replaced by broad serde defaults.

## Author Snapshot and Request Identity

`AuthoringSession::snapshot` captures one immutable persistence input:

- `AuthoringSessionId`: the process-local identity of this open lifetime;
- monotonic `AuthorGeneration`;
- exact asset-library database revision;
- a typed, fully validated logical `ProjectDocument` root whose author
  collections retain their structural sharing;
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

Every manual snapshot is also admitted against one
`ManualProjectFileDestination`: a Session-scoped monotonic binding revision
paired with the exact intended canonical path. This is different from request
identity. Save As advances the destination binding only after its request
enters the bounded queue; if admission fails, the previous destination remains
current. An ordinary Save reuses the latest admitted destination even when the
Save As publication has not completed and `AuthoringSession::project_file`
therefore still names the previous durable path. Repeating the same target
shares a binding; selecting a different target advances it with checked
arithmetic.

Every admitted manual request also reserves its exact archive
`document_revision` before it enters the worker. Reservation uses checked
arithmetic above both the snapshot revision and this Session's last reservation;
overflow fails before queue state changes, and a transport-send failure rolls
the reservation back. Publications are therefore strictly ordered even when
several Saves are queued before the App applies the first Completion. A failed
publication may leave a revision gap but can never permit reuse or regression.
Autosave does not inherit an in-flight or failed manual reservation. At its FIFO
worker position it uses the greater of its snapshot revision and the latest
successfully published manual revision for that Session, so recovery evidence
cannot claim a manual save that never became durable.

The request purpose owns its typed destination. Manual publication carries the
canonical destination binding; autosave carries an
`AutosaveArchiveDestination` containing both its exact recovery archive and the
canonical locator captured for the manifest. There is no independent
`target_file` argument plus an `update_project_path` boolean that can disagree
with the purpose. Admission rejects a manual destination from another
Authoring Session before queue state changes.

Save-request payload admission is bounded independently from the FIFO control
barrier. A request must first reserve one of the four waiting slots and only
then may a manual request retain kernel-backed publication authority for its
destination. Failure to reserve the slot therefore has no filesystem,
destination-binding, document-revision, or path-lock effect. If target-lock
acquisition fails after reservation, that slot is returned before the error is
reported. Once the request is admitted, its target authority deliberately
remains attached to the logical Project Session until all possible queued
publishers and retained completion paths are gone; caller-side completion is
not sufficient proof that the path can be released.

Manual save, Save As, and autosave all enter the dedicated
`ProjectPersistenceService`. The worker first uses SQLite's online backup API
to create a self-consistent database snapshot, then writes the archive from the
immutable document and that database. It never copies a live WAL database as a
set of ordinary files. UI, playback, and rendering remain independent of ZIP
compression and filesystem latency.

Before allocating the SQLite snapshot, the same worker validates the archive's
direct parent. If a Save As or Headless target contains a missing directory
suffix, it selects the nearest existing direct ancestor and establishes every
missing child through `ensure_durable_directory_chain`; ordinary
`create_dir_all` is not a persistence shortcut. Existing parents are admitted
as user-selected destination authority but are still revalidated by the later
exact-object sibling boundary. An unconfirmed or indeterminate directory
publication aborts before any archive namespace operation, and the UI thread
never waits for this filesystem work.

The transient SQLite snapshot is an opaque sibling of the target archive. The
archive target is only a directory anchor: its leaf contributes no snapshot
identity and is never claimed by the backup operation. `mondrian-storage`
selects a bounded process-randomized leaf and claims it with kernel
`create_new`; no persistence-service UUID, request-derived filename, or second
temporary-file namespace exists above that primitive. This keeps the name
independent of an arbitrarily long user-facing Project filename and preserves
Windows path headroom.

`AssetLibrary` freezes the existing anchor parent, validates the captured
Library revision before allocation, then temporarily releases only that exact
identity-bound object to SQLite's path-based VFS. The backup destination runs
without a rollback journal, is normalized back to standalone `DELETE` journal
mode after the online backup, is closed, reclaimed by File ID/inode, and
flushed through the retained handle. A journal, WAL, or shared-memory sidecar
is not part of the snapshot contract. The returned RAII guard owns cleanup of
only the observed request object.

Project archive construction clones the retained snapshot handle and streams
that exact file object into `library/index.db`; it never reopens the opaque
snapshot pathname. A pathname substitution after reclaim therefore cannot
change which bytes enter the archive. Failure at any backup, close, reclaim,
flush, archive, or publication boundary drops the guard and leaves a pathname
already observed to name another object untouched. Worker serialization and
runtime/publication leases are lifecycle controls, not substitutes for
exclusive file creation or exact-object evidence.

Persisted media identity is a lossless UTF-8, ordinary native absolute path.
Asset import canonicalizes the existing file once; Asset Library open creates
then canonicalizes its root before storing it. On Windows, `\\?\` drive/UNC
spelling is generated only at the SQLite VFS boundary so the live database and
its sidecars can exceed legacy `MAX_PATH`; it is never persisted as media or
Library identity. Relative paths, dot/parent traversal components, device
namespaces, non-file verbatim namespaces, and non-lossless encodings fail
closed when a row is read.

Manual save-before-close places the save request and a FIFO quiescence barrier
in one worker order. The Window thread never waits for either: it retains a
move-only pause ticket and polls it from bounded event-loop turns. Autosave
remains asynchronous and coalesced by the App so it cannot create an unbounded
request queue.

Each exact `AuthoringSessionId` also owns a checked Persistence Generation.
Admission follows `Open(g) -> Pausing(g) -> Paused(g)`. A matching pause token
may either resume as `Open(g+1)` after candidate failure or retire the Session
permanently after successful replacement/close. Open, recovery, creation,
replacement, and close enqueue a FIFO barrier outside the bounded save-request
capacity. Its acknowledgement proves every earlier publication finished and
the worker destroyed its request-owned Asset Library and runtime-lease Arcs.
Timeout, disconnect, or inconsistent acknowledgement poisons that Session
closed; lifecycle mutation aborts rather than guessing that a pending counter
or empty completion queue means quiescence.

The synchronous Headless/open-replacement helper and the non-blocking Window
path share exactly one `begin pause -> barrier acknowledgement -> complete
pause` implementation. A ticket binds the persistence service identity,
`AuthoringSessionId`, Persistence Generation, sole acknowledgement receiver,
and timeout budget. Beginning the ticket closes admission before publishing the
barrier and returns immediately. Only the matching acknowledgement changes
`Pausing` to `Paused`; polling an empty channel changes no state. Once paused,
the App applies already-queued scalar completions while the old Session remains
authoritative, retires that exact generation, and only then removes author and
runtime ownership.

While a Window ticket is pending, the Project remains readable for projection
but all product Actions and Project-scoped result application are frozen. The
event loop checks at a bounded 16 ms cadence without `ControlFlow::Poll` busy
spinning. A protocol timeout/disconnect retains the in-memory Project and keeps
it fail-closed instead of resuming edits against poisoned persistence
admission. The close guard then requires an explicit user Discard: the service
marks the exact Session `Retired` without claiming quiescence, eventual worker
completions remain stale, and their request-owned Library/runtime-lease Arcs
survive until actual worker return. A dropped or stale ticket is never
interpreted as quiescence.

Save-before-close binds the close operation to the exact manual request ID it
just admitted. Reaching the FIFO barrier is insufficient: that request must be
the applied durable baseline. A failed publication resumes admission at a new
Persistence Generation, keeps the Project open, and permits correction/retry;
it is distinct from a handoff protocol fault and never silently degrades to
Discard.

The saturated-queue lifecycle gate holds one active publication, fills every
waiting slot, rejects a further Save As, proves another Project can immediately
lease that rejected target, and then closes behind the exact final admitted
Save. This is the executable boundary between an attempted UI intent and an
admitted persistence authority; changing queue capacity, lock ordering, or
close draining must preserve the same invariant.

## Live Project and Runtime Authority

The durable owner manifest answers “which Project owns this tree”; it does not
answer “which process may mutate this Project or publish to its filesystem
target now.” `ProjectRuntimeLease` is the single live authority and retains
three independent kernel exclusions:

- one logical-Project lock keyed by the exact `ProjectId`, preventing two copied
  archives of the same Project from running concurrently;
- one publication-target lock for the opening target and every Save-As target
  admitted during the Session, keyed by normalized filesystem identity;
- the runtime-local `session.lock`, preventing two writers from mutating the
  same owner-valid runtime tree.

Windows opens every authority file with no read, write, or delete sharing. Unix
takes a nonblocking exclusive file lock. File presence, timestamps, and
PID-shaped diagnostics are not authority. The retained kernel handles are the
authority, and the operating system releases them after orderly Drop or process
termination. Before every authorized mutation, the Module verifies that each
handle still names the current authority-directory entry. This is required on
Unix, where an advisory-locked file can otherwise be unlinked and replaced with
a second inode. It also re-resolves every admitted absolute spelling of each
publication target; retargeting a symlinked parent invalidates the lease instead
of redirecting queued publication outside its original target authority.
Logical-Project and publication locks live in one stable per-user authority
namespace independent of `TEMP`, `TMP`, or `TMPDIR`: Windows uses the current
user's Local App Data, while Unix uses an effective-UID-owned mode-`0700`
directory under canonical `/tmp`. Recovery-bearing runtime payload does not
share that ephemeral exception: Windows stores it below Local App Data,
macOS below Application Support, and other Unix platforms below
`XDG_STATE_HOME` or `~/.local/state`. It is therefore independent of `TEMP`,
`TMPDIR`, and `XDG_RUNTIME_DIR`, while processes with different temporary
environments still contend on the same Project and publication locks.

Native discovery of that stable per-user anchor crosses the narrow
`mondrian_platform_core::UserStateDirectory` Interface. The
`mondrian-platform` Adapter resolves one absolute path without creating it.
Project Runtime remains the sole owner of the product namespace, durable
directory chain, permissions, symlink/object-identity checks, leases, and
recovery semantics; these domain rules must not move into a generic platform
utility or be reimplemented in the App composition root.

Runtime-parent creation uses one product-owned leaf,
`mondrian-project-runtime-v4`, directly below the platform state anchor: Local
App Data, Application Support, or the selected XDG/HOME state directory. If
the Unix state anchor itself is missing, the caller selects its deepest
existing, directly validated ancestor and publishes only the remaining missing
suffix. Storage never attempts to flush filesystem roots or infer durability
from an existing path. The final runtime parent owns a versioned durable
marker: initial publication is create-only, while exact validation of that
durably established marker authorizes reentry without replacing the
validated entry. An unknown/markerless existing parent, partial existing
suffix, marker race, or any typed
unconfirmed/indeterminate publication fails closed. This parent marker does
not replace each concrete runtime root's owner manifest or live lease.

The normalized publication-path identity selects only a runtime *family*.
The concrete payload root is allocated by the pair
`(publication-path identity, ProjectId)` and is named
`mondrian_<path-sha256>_<project-id>`. Replacing a closed Project at the same
path with a different `ProjectId` therefore allocates a distinct root and
leaves the old Project's payload untouched; the original Project may later
reacquire its own root. Production exposes no path-only runtime-root lookup:
ordinary Open computes the exact `(path, ProjectId)` root, while recovery uses
the exact root carried by a validated discovery candidate. Test-only family
enumeration is not mutation authority. Only `claim_project_runtime(path,
ProjectId)` may select a paired root and return its kernel-backed Lease.

`AppState` retains one `Arc<ProjectRuntimeLease>` for the active Project
authority. Admitted manual-save/autosave requests and recovery mutations clone
that exact Arc; completions retain only scalar `ProjectRuntimeLeaseId` evidence
and can never prolong filesystem authority. Save As retains its target lock
before queue admission and conservatively keeps it until the Session ends, so
queued work cannot outlive target exclusion. Retired Project Library
Generations may temporarily retain additional old-Project lease Arcs alongside
`Weak<AssetLibrary>` evidence. That prevents another process from claiming or
sweeping the old runtime while waveform, audio, UI, or analysis consumers still
hold its final library Arc. The same App may reuse that exact retained lease
when it returns to the logical Project.
Production mutation interfaces accept the lease, not a bare
`(runtime_root, ProjectId)`. Discovery remains read-only and may validate owner
metadata without acquiring the lease, but exact recovery open acquires all live
authority before staging or child mutation. A selected runtime outside the
canonical runtime-payload parent is rejected rather than admitting an
attacker-selected runtime tree into the stable authority namespace.
The lease proves filesystem and cross-process Project authority only. It is not
an Authoring Session generation or Asset Library generation; in-process queued
work must carry and validate those separate semantic identities before it can
publish.

## Durable Atomic Publication

The archive is written to an exclusively created direct sibling temporary file,
completed as a ZIP, flushed with `sync_all`, and validated through a retained
clone of the exact writer object before publication. The Manifest uses classic
ZIP fields; Project JSON and Library select ZIP64 before streaming begins so
crossing 4 GiB cannot fail late. The verifier requires exactly the three
contract entries and reads each through EOF, comparing declared/observed entry
length and exercising ZIP CRC. The archive persistence Module then streams the
whole file through the same retained filesystem object to compute exact archive
length and SHA-256; it does not reopen the temporary pathname or deserialize
`project.json` a second time. Its typed `ProjectArchivePublication` intent
survives that complete preparation and is decided only by the final kernel
namespace operation:

- a newly created Project, Save As to a confirmed-absent target, and every
  autosave archive use `CreateNew`. Any existing destination entry makes the
  final operation fail without replacing it. A preceding `exists()` check is
  only an early diagnostic and never no-overwrite authority;
- ordinary Save along an established destination binding and Save As to a
  user-confirmed existing target use `ReplaceExisting`.

Both modes use the unique domain-free
[Storage Publication](storage-publication.md) implementation. Project code does
not own a second Windows/Unix replacement algorithm.

Only after the atomic namespace operation and containing-directory durability
barrier both succeed does Storage return `FilePublicationEvidence`, from which
the archive Module can construct `ProjectArchivePublicationEvidence`. A
successful rename/move or a final path that visibly names the new object is not
enough. `BeforeNamespace` proves that the new object did not become the target;
`DurabilityUnconfirmed` proves that it did but cannot prove crash durability;
`NamespaceIndeterminate` proves neither postcondition. These typed failures
remain failures even if bytes are visible, and only confirmed durable evidence
may advance a saved baseline or authorize recovery retention cleanup.

The resulting opaque, private-construction, non-`Clone` value is consumed by
value and binds the published absolute path, publication mode, embedded
`ProjectId`, embedded document revision, whole-file length, and whole-file
SHA-256 observed during construction. It is point-in-time construction
evidence, not durable authority over whatever a pathname may name later. A
Recovery Manifest mutation that admits a new recovery point accepts only
`CreateNew` archive evidence; callers cannot manufacture evidence for an
unverified archive or downgrade replacement evidence into an immutable
recovery point.

The archive, manifest, and extraction siblings use a bounded opaque leaf that
does not inherit the destination basename.
Process-randomized, domain-separated 128-bit name material covers the target,
purpose, process, and monotonic request nonce. Two calls cannot intentionally
address one sibling, while the bounded leaf preserves Windows path headroom.
Every sibling claim uses kernel `create_new` plus platform no-follow semantics.
Immediately before publication, direct-file metadata and filesystem-object
identity must still match the retained handle. RAII cleanup repeats that
identity check and never removes a pathname already known to name another
object. The source handle is retained through publication. A process with
uncoordinated mutation rights to the sibling directory can still race a
pathname-based replacement, so the Project Runtime/publication Lease
coordinates Mondrian publishers while Storage classifies object-identity
postconditions. Neither layer claims compare-and-swap authority over an
uncooperative third-party writer.
The parent directory is unchanged, so every final publication remains a
same-filesystem atomic namespace operation. This bound is independent from the
separately bounded SQLite backup-snapshot leaf described above.

Windows first converts absolute local and UNC paths to extended-length form
(`\\?\...` or `\\?\UNC\...`) and uses write-through flags. A
greater-than-`MAX_PATH` regression test verifies that this remains the same
atomic publication path rather than a short-path fallback. Unix synchronizes
the parent directory after publication.

After a manual `CreateNew` publication (new Project or Save As to an absent
target),
exact Completion validation,
`mark_saved`, and prior-Session handoff retirement all succeed before the App
installs the candidate and replaces its one-shot destination with a
`ReplaceExisting` binding. Every later Save therefore uses replacement
semantics; no later request can accidentally retain first-create authority.
The worker also records namespace establishment of the exact
`(AuthoringSessionId, destination-binding revision)` lineage. A second request
already queued for that same create binding uses `ReplaceExisting` only after
the preceding `CreateNew` is proven durable or durability-unconfirmed. The
latter proves that the destination names the new lineage, but does not advance
the saved baseline. A proven pre-namespace failure leaves followers create-only;
an indeterminate postcondition poisons the binding and rejects followers until
explicit revalidation.
This worker-local dependency closes the interval before the App can consume the
first Completion without creating a second canonical-path authority.

There is no “rename old file away, then hope the new rename succeeds” window.
A serialization, database backup, ZIP, validation, flush, or proven
pre-namespace publication error leaves an existing Project target untouched.
After the namespace boundary, `DurabilityUnconfirmed` proves the new object is
visible without proving crash durability, while `NamespaceIndeterminate`
authorizes no assumption about which object the target names. Cleanup removes
only a source object whose identity and safe pre-publication state are proven.
`write_durable_file_atomically` applies the same typed publication rule to
Recovery manifests and other Project-adjacent indexes. Only durable evidence
permits removal of archives no longer referenced by the replacement manifest.

Successful completion reports the exact Session, Persistence Generation,
request, author generation,
asset-library revision, resulting document revision, Project metadata, and
typed destination. Before reading success or failure, the App first verifies
the open Session, current Persistence Generation, scalar runtime-lease
identity, and current manual destination binding. A completion for an obsolete
destination—successful or failed—cannot
rebind the canonical path, advance the manual baseline, or reconcile Recovery
Authority; a successfully published obsolete artifact remains an ordinary
copy. A completion for the current destination always passes that exact path
to `AuthoringSession::mark_saved`, including an ordinary Save admitted behind
an in-flight Save As.

`AuthoringSession::mark_saved` advances the durable baseline only monotonically.
It also treats persisted `ProjectMeta` as evidence, not author input: an exact
generation/Library match must retain every current authored metadata field, and
only the publication-owned `updated_at` value is installed. A forged worker
result rejects the baseline transition atomically.
If a newer publication to the same current destination was applied first, its
baseline satisfies an older completion by author generation and Asset Library
revision. The App proves that relation with a destination-scoped high-water
request receipt; an equal baseline inherited when opening a saved Project
cannot accidentally hide failure of a newly requested write. The receipt is
delivery-order evidence only and never mirrors generation or Asset Library
baseline authority out of the Authoring Session. The older completion cannot
regress metadata or surface an obsolete write failure, but it may retry current
Recovery Authority reconciliation.
If the user edited after the captured snapshot, an older successful publication
cannot clear dirty state. Failed current-destination requests retain the
admitted destination so an ordinary Save is an unambiguous retry; the previous
canonical file and baseline remain authoritative until that retry succeeds.

## Autosave

Autosave publishes immutable `.autosave.mdp` recovery points under the stable
Project runtime root. Before the worker creates its SQLite snapshot or archive,
the Project-runtime owner Module durably creates or validates the direct
`autosave/` directory against the exact Project ID. An unconfirmed or
indeterminate directory publication rejects archive admission for that attempt.
An existing file, symbolic link, or differently owned root fails closed and is
never replaced. Manifest
publication and retention are validation-only operations: they cannot create
filesystem authority after the archive write. The target must be one direct
`.autosave.mdp` child of that directory. Each leaf contains the captured
wall-clock millisecond, `AuthorGeneration`, and a fresh random UUID. Those
fields make requests diagnosable and names collision-resistant across requests;
none is overwrite authority. Every autosave archive remains `CreateNew` through
the final namespace operation, so a collision fails without changing the
existing archive. The Recovery Manifest is published only after its referenced
archive has been durably published:

```text
autosave/manifest.json
autosave/project-<unix-ms>-g<author-generation>-<random-uuid>.autosave.mdp
```

The manifest is a strict schema-v1 document. Its header records exact
`ProjectId` and an absolute Session-independent source path; every entry records archive
path, timestamp, author generation, asset-library revision, embedded document
revision, and SHA-256. Entry paths must be direct children of the Project
`autosave` directory, must be absolute, and end in `.autosave.mdp`. A relative
header or entry path fails both publication and read admission. In production,
when admitting a new point, the Recovery Module consumes the non-cloneable
`ProjectArchivePublicationEvidence` exactly once and derives the entry path,
Project ID, embedded document revision, and archive SHA-256 only from that
value; author generation, Asset Library revision, and capture time remain the
separate persistence-request facts they describe. It rejects evidence for any
publication mode other than `CreateNew`. It neither reopens nor deserializes
the just-published archive before atomically replacing the manifest.
Previously admitted entries are normalized by bounded manifest metadata and
path checks during publication; they are not all rehashed on every autosave.

The consumed evidence proves construction at one instant, not continued
pathname identity. Discovery, exact selection, and recovery therefore reopen
direct regular-file leaves under the live Lease, rehash their complete bytes,
reparse their archive/Project identity and revision, and match those facts
against the canonical Manifest. Retention first commits the replacement
manifest and only then removes archives it no longer references. A crash or
proven pre-namespace manifest-publication failure leaves the previous manifest
and every archive it authorizes byte-for-byte intact. A durability-unconfirmed
or namespace-indeterminate result suppresses all archive removal because the
current Manifest authority is not proven. The newly published,
still-unreferenced create-only archive may remain as a conservative crash
artifact and cannot replace an older recovery point. A successful autosave advances
`autosaved_generation`, not the manual save baseline; it must not make the
title bar or close guard report “saved”.

All in-process manifest mutations share one recovery publication lock; the
Project Runtime Lease excludes other processes, while the durable file
replacement remains the cross-crash boundary. `ProjectId`, not
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
fields, unsafe paths, hash/revision/Project mismatches, invalid archives, an
unowned or differently owned runtime root, and a candidate whose existing
canonical Project archive has another `ProjectId`. A candidate contains the
exact `ProjectId`, paired runtime root, absolute canonical and autosave paths,
author/Library/document revisions, timestamp, and archive hash. Equal-time
candidates are ordered deterministically by Project identity, runtime root,
and autosave path. If the same-Project canonical archive exists, discovery
hides only snapshots whose document revision is lower than the canonical
revision; an equal revision remains visible and a missing canonical archive
preserves recovery authority. The Archive Publication evidence consumed when
the entry was first admitted is not retained as later path authority.
Each visible candidate also carries typed canonical-target evidence: `Missing`
or `Present { document_revision }` for a validated archive with the same
Project identity. This evidence is the sole input to product presentation; the
Window Adapter does not reopen the canonical archive or infer safety from path
metadata. Startup confirmation names the exact autosave source, capture time,
intended canonical target, and target state, and states that confirmation opens
a dirty Session without immediately publishing over the target.
Discovery and every later admission prove the current direct file object's
complete hash and parsed archive identity again; a Manifest entry never turns a
replaced or tampered pathname into a trusted archive.
Every UI, Headless, and lifecycle recovery entry accepts that complete
candidate rather than reconstructing authority from two paths. Selection
preflight returns immutable evidence for one complete manifest entry;
it is deliberately not admission authority. After acquiring the logical
Project, publication-target, and runtime locks, recovery rereads the canonical
manifest, rejects duplicate snapshot paths, and requires the one complete
entry, canonical Project path, Project ID, source hash, embedded document
revision, and archive identity to match that evidence. Canonical revision
freshness is checked again after lease acquisition. It creates the staging
archive exclusively, copies the selected source, flushes it, independently
verifies the copied bytes and embedded identity, and then requires the same
exact manifest entry once more while the in-process recovery mutation guard
remains held. A retired or changed entry, replaced source, conflicting Project
publication, or pre-existing staging file fails closed. Only the verified
staging archive is opened into a new dirty `AuthoringSession`; recovery never
edits or consumes its source first. Manifest, source, canonical Project, and
staging leaves are opened as direct regular files without following a leaf
symlink or reparse point. Source verification, copying, staging verification,
Project preflight, and final archive loading operate on retained file objects;
the loader consumes the exact verified staging handle rather than reopening its
pathname. The staging handle is owned by an RAII guard: all success and error
paths close it and remove the same namespace object, while a Unix replacement
inode is never deleted as if it were the verified file.
Canonical target presence and revision are checked both before lease acquisition
and under the live lease. A target created, removed, or advanced after discovery
invalidates the old confirmation and fails closed; the user must select a fresh
candidate whose displayed evidence matches current state. UI text is never
overwrite authority and a confirmed click never suppresses this revalidation.

Crash-artifact cleanup runs only after persistence is quiesced and the exact
runtime lease has been acquired. It may remove direct regular
`.recovery-open-*.staging.mdp` children of that root. It removes an
`.autosave.mdp` archive only when a valid canonical manifest for the leased
Project exists and does not reference that exact direct-child path. A missing
or malformed manifest preserves all autosaves; unknown files, directories,
links, sibling roots, and manifest-referenced snapshots are never cleanup
targets.

A manual save retires recovery authority only when its completion still covers
the current author generation and Asset Library revision. Retirement first
atomically publishes an empty canonical manifest and then removes the old
archives. A stale asynchronous save completion cannot clear newer recovery
authority. If that completion is Save As, it atomically rebinds retained
recovery authority from the previous canonical path to the newly published path
without deleting it. The live runtime root remains stable because its schema-v3
owner allocation identity is immutable across Save As; recovery admission
therefore derives that authority from the selected manifest location instead
of recomputing it from a possibly changed Project path. A recovered Session
holds its runtime lease for its complete live lifetime. A covering save retires
only Recovery Authority, never the runtime lease or Project Library Generation.
Cleanup or rebind failure after a successful Project publication is reported as a recovery
warning, not as a false claim that the irreversible Project save failed.
Recovery reconciliation retains the Manifest publication failure as typed
pre-namespace, durability-unconfirmed, or namespace-indeterminate evidence
through its Recovery Module Interface; the lifecycle Adapter renders it as
warning text only when producing product-status output. No post-publication
state may be inferred by parsing that text.
Terminal failure evidence has two independent axes. `ProjectPersistenceFailure`
classifies the actionable cause as storage exhausted, permission denied, target
conflict/unavailable, invalid data, other I/O, internal failure, or request
rejection. `ProjectPersistencePublicationFailure` separately records whether
the archive or Recovery Manifest failed before namespace mutation, after a
visible but not durably confirmed mutation, or with an indeterminate namespace
postcondition. Product UI may combine these facts, but must never infer either
one from the other's diagnostic text. Deterministic worker I/O injection proves
that storage exhaustion and permission denial preserve canonical Project bytes;
the lifecycle gate additionally proves storage exhaustion keeps the Session
open and leaves the existing Recovery Authority manifest byte-for-byte intact.
Conflicts, invalid candidates, and failed retirement remain explicit and
retryable.

Golden recovery verification must exercise the same production autosave,
manifest, staging, Session replacement, and covering-save path. It must retain
complete Project and Asset Library semantics across a distinct dirty recovered
Session and a final durable reopen; stable IDs or a Project hash alone are
insufficient. This product-path evidence does not replace filesystem
fault-injection or conflict-UX qualification.

Persistence unit fixtures use a cross-run unique runtime parent identity that
includes the test purpose, process, creation time, and an in-process sequence.
PID reuse or a retained directory from an interrupted test must never collide
with a later fixture and be mistaken for a markerless production runtime root;
the production ownership check remains fail-closed and is not weakened for
tests.

The repeated-crash qualification uses three independent child processes. Each
process opens the preceding Recovery Authority, verifies the recovered author
state, commits a new edit, durably publishes an autosave, and terminates through
`abort` without running Rust destructors. The parent and children share a
strictly numeric test-only runtime namespace so ordinary parallel tests retain
PID isolation while this gate exercises real cross-process OS-lock release. The
parent must recover the third edit as dirty state, publish one covering manual
save, and observe every recovery candidate retired. This proves abnormal
process exit rather than an in-process `Drop` simulation; it does not replace
platform filesystem-capacity and permission fault matrices.

The runtime *family* is derived from the complete, domain-separated SHA-256 of
the normalized Project publication target. Normalization resolves the longest
existing canonical parent-directory prefix before appending unresolved
directories and the publication leaf. The leaf remains the directory entry
replaced by atomic publication; a leaf symlink's referent is not the target
identity. Unix identity then frames exact `OsStr` bytes. Windows additionally
uses case-insensitive valid-Unicode identity, preserves ill-formed UTF-16 units,
and conservatively removes Win32-ignored trailing spaces/dots from unresolved
components. The 256-bit digest is never truncated, lossy display text is never
identity, and the domain tag versions these normalization semantics.

That family locator is diagnostic/discovery information only. The concrete
payload root appends the durable `ProjectId`, so its allocation identity is the
pair `(normalized publication-path identity, ProjectId)`. Replacing a closed
Project at the same publication path with a different Project therefore creates
a distinct payload root and never adopts, mutates, or clears the earlier
Project's payload.

An in-memory lease is reused for an ordinary reopen only after all three
identities agree: the requested `ProjectId`, the normalized publication target
against the immutable owner-manifest allocation digest, and the live
kernel-backed runtime owner. Merely retaining a Save As publication lock or a
retired library lease is insufficient. This also keeps tests and alternate
runtime parents honest: reuse derives from durable allocation authority, not
from assuming one process-global parent pathname.

Authority locks and recovery-bearing payload use separate platform-selected
per-user roots. Each paired payload root contains only its strict owner
manifest, live Session lock, immutable Library generations, and autosave
subtree. Concrete platform path spelling is an Adapter concern; the durable
identity and fail-closed ownership rules above are the architecture contract.

The strict schema-v3 owner manifest durably binds both members of that allocation
pair. The runtime root itself is a durably published direct child; a new root
then publishes its owner before it admits a library or autosave payload, while
already holding `session.lock`. Root and owner publication retain typed
`BeforeNamespace`, `DurabilityUnconfirmed`, and `NamespaceIndeterminate`
outcomes. Only durable evidence admits payload. Unconfirmed or indeterminate
state leaves the root inert, rejects admission, disables broad rollback, and
must be revalidated and republished on a safe retry.
An existing root with a malformed, different-path, or different-Project owner
fails closed; creation and open never recursively clear such a tree. A
missing-owner root is completed only when exclusive acquisition proves no live
writer and the root contains exactly the fixed lock entry, covering a crash
between root creation and owner publication without adopting a payload-bearing
tree. Any root lacking the exact current schema-v3 paired owner authority fails
closed. Production does not scan, migrate, adopt, or delete legacy or ownerless
runtime roots. Archive open first prepares one
`PreparedProjectArchive` ticket over the retained file object. Before any JSON
allocation or SQLite copy it requires the exact three-entry set, admits
compressed and declared per-entry lengths under an explicit
`ProjectArchiveReadBudget`, and uses bounded counting readers to prove actual
lengths. It validates and deserializes the Project exactly once, exposes its
Project ID to claim or verify the owner and Lease, then consumes the same
ticket to CRC-check and extract SQLite into a fresh
`library-generation-<uuid>/` direct child that is not visible to an Authoring
Session. This generation is explicitly ephemeral and regenerable; unlike the
root and `autosave/`, its directory namespace is not recovery authority and
does not require parent-directory durability. The default 2 GiB archive /
64 KiB Manifest / 768 MiB Project / 1.5 GiB Library budget is caller-selected
resource policy, never a duration heuristic or a promise that a minimum-memory
machine can admit the configured limit under concurrent workload. The candidate
Project ID and schema are verified before SQLite is opened. Once opened, that
directory is immutable for the complete
`Arc<AssetLibrary>` lifetime: it is never renamed, replaced, or unlinked.
Building a candidate therefore leaves the previous Session and its library
untouched; a failed extraction, SQLite open, or Session construction removes
only the uninstalled candidate. Candidate rollback also retains weak lifetime
evidence for any opened library. If an internal Arc unexpectedly survives
candidate Drop, rollback preserves the owner-recognized directory. A
process-local weak-liveness registry is consulted by every leased orphan sweep,
so a later open cannot unlink that SQLite directory while the escaped Arc is
still alive; the registry grants no filesystem authority and dead entries are
pruned before removal. On success the new Session is installed atomically in
memory. The retired directory remains named until weak lifetime evidence proves
every persistence, waveform, audio, and UI reference has released the old
library; only then may the live runtime lease remove it.
Abandoned generations from a terminated process are swept on a later exclusive
runtime claim. The embedded archive path remains `library/index.db`; generation
directories are machine-local runtime state.

The owner manifest is filesystem-safety authority, not a parallel Recovery
Manifest. It neither names autosaves nor stores the mutable canonical Project
path. Save As keeps the live runtime root and its original paired owner identity
stable; only the Recovery Manifest rebinds the current canonical path.
Recovery selection uses the manifest's actual runtime root and requires both
authorities to agree on `ProjectId`. SQLite migrations operate on the extracted
runtime copy only; saving is the sole path back into `.mdp`.

## Document Schema Contract

Document schema v25 is the current Alpha author contract. It persists the
Project-owned color environment and future-Sequence template, exact rational
author time, canonical audio layout/routing/processor schemas, canonical proxy
membership, closed Clip content, multi-member link groups, strong visual
Transitions, complete Mask and Basic Title properties, Clip-local visual author
time, closed Sequence color/delivery structures, and one exact
`ClipSourceTimeMap` whose constant mapping persists origin, signed scale, and
covering/strict-predecessor sampling boundary while its terminal boundary is
derived from duration.

Every old or future document schema and every unknown author field fails closed
during Alpha. Reopen must never synthesize missing defaults, infer a legacy
source range, or repair an invalid strong reference. The evolution registry is
the only future migration seam; version history is maintained by source
control, not repeated here as an implementation ledger.

The checked current-document fixture must open idempotently, save, reopen, and
retain its semantic fingerprint. Dedicated
round-trip tests additionally cover visual parameters, mask animation, audio
processor schema/curves, Basic Title animation, exact Transition/link
relationships, and SQLite content. A schema number is not advanced unless
these required semantics are represented and validated.
