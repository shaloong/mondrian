# Project Model

Mondrian uses a Premiere-style lightweight project document. The project file
stores editing decisions, project settings, sequence structure, and asset-library
metadata. Rebuildable caches, proxies, waveforms, thumbnails, and preview renders
must live outside `.mdp`.

## Persistent Project Document

`mondrian-project::ProjectDocument` is the canonical saved project payload inside
`.mdp`:

- `schema_version`
- `project_id`
- `document_revision`
- `meta: ProjectMeta`
- `settings: ProjectSettings`
- `color_environment: ProjectColorEnvironment`
- `new_sequence_defaults: SequenceSettings`
- `sequences: SequenceCollection`
- `proxy_mode_assets: AuthoringSet<AssetId>` (persisted with the exact
  canonical `BTreeSet<AssetId>` JSON representation)

`mondrian-core` keeps only shared project metadata and settings types. It does
not define a second top-level project container.

`document_revision` advances only after an explicit project save succeeds. It
is persistence/conflict evidence, not author-semantic identity. Each persisted
Sequence owns an independent monotonic `SequenceRevision`; Playback, Preview,
audio compilation, and render caches bind that revision, so an edited draft or
failed save cannot continue under an older Timeline identity.

`ProjectColorEnvironment` privately owns the one persisted product-mode selector
for Mondrian Standard, ACES, or Custom OCIO and exposes that exact value through
its stable interface. Callers cannot construct or mutate a partial environment
by reaching through a public field. This keeps the top-level Project seam
available for future environment-wide invariants without changing its JSON
shape. A Sequence never stores, overrides, or inherits another engine.
`SequenceColorSettings` stores only the working domain, media-input policy, and
Program Output policy interpreted inside the Project environment.
`SequenceDeliveryDefaults` separately stores encoded range/bit-depth defaults
and authored HDR metadata.

`ProjectSettings` remains a container/runtime policy object (proxy, cache and
autosave settings); placing the engine there would mix image semantics with
runtime preferences. `new_sequence_defaults` is a complete Sequence template.
Project creation owns both the environment and this template, and the first
Sequence receives an exact copy. Updating the template affects only future
Sequences. It is not consulted by playback, validation, export, or an existing
Sequence.

Document validation checks the template and every existing Sequence against
the exact Project engine. Mondrian Standard pins Linear Rec.2020; Custom OCIO
pins the working and output routes covered by its saved processor identity;
ACES permits its supported explicit working spaces. An incompatible candidate
cannot enter a saved document or author transaction.
`Sequence::validate_author_contract` is the sole local Sequence-body seam for
revision, settings/color, author identities, Audio Program, Track/Clip time,
Basic Title, Transform, Effect, and Mask validation. `ProjectDocument` adds
collection-wide nesting/reference validation around it. Selected-range Export
admission reuses the local seam only for its exact captured root/nested closure;
it neither requires unrelated Project Sequences nor duplicates these rules in
an execution crate.

`ProjectDocument::prepare_authoring_validation_certificate` is the full
validation Interface and `ProjectDocument::validate` invokes it and discards
the result. Its opaque, process-local
`ProjectAuthoringValidationCertificate` combines exact per-Sequence author
certificates with the sole strongly anchored Sequence dependency certificate.
It is neither serialized nor cloneable, has no author-state getter, and never
enters persistence, History, or execution snapshots. Incremental preparation
still calls the one cheap `validate_document_contract` Implementation before
reusing evidence; only Project identity and the exact color environment are
separately bound because they define certificate continuity.

Replacing the Project color environment is one atomic Project action carrying
a complete `ProjectColorEnvironment`. `AppState` first prepares the exact OCIO
dependency, then validates the template and every Sequence. Only if all checks
pass does it commit one Project snapshot, stop playback and rotate preview
execution. It never rewrites Sequence settings or silently substitutes another
engine/output route. A failure leaves the previous environment and history
unchanged.

The Project Settings draft edits this same global environment. A Custom OCIO
selection collects the future-Sequence template and every current Sequence,
deduplicates all required Program Output targets, and pins them together into
one complete config/resource/processor identity before it can replace the
draft. Every target must resolve to one unambiguous display/View binding; one
missing or ambiguous target rejects the whole candidate rather than retaining a
partially usable project engine. The current Custom identity intentionally pins
one exact working space, so a Project using several Sequence working spaces
must unify them before selecting Custom OCIO. The final AppState transaction
still revalidates the template and every Sequence, so a stale UI snapshot
cannot commit an incomplete identity. If an already-authored Custom dependency
is unavailable on reopen, the document preserves the exact author intent;
execution prepare fails closed until the dependency is restored. It must not
fall back to Mondrian Standard, ACES, or a same-path-but-different config.

Nested color integration is not a Sequence-global setting. It belongs to each
`ClipContent::NestedSequence` placement edge, because the same child can be
placed by different parents with different handoff intent. All such edges
remain inside the one Project engine. The edge either composites in the child
working space and converts once to the parent, or evaluates the child directly
in the parent working space. A future baked-output mode cannot be exposed until
its output/input identities, stateful effects, Alpha semantics and
preview/export reference corpus are complete.

## Runtime State

`AuthoringSession` owns the complete mutable authoring aggregate:

- canonical `ProjectDocument` and Sequence collection;
- Project file/runtime paths and the asset-library authority;
- active/default Sequence navigation;
- project-wide bounded history;
- `AuthoringSessionId`, `AuthorGeneration`, and manual/autosave baselines.

`AppState` composes that Session with runtime-only product state:

- one `Arc<ProjectRuntimeLease>` holding logical-Project,
  publication-target, and runtime kernel authority;
- selection state
- playback/audio state
- render queue/export draft
- clipboard and animation selections
- UI-facing status log

Machine-local Project runtime authority is keyed by the complete
domain-separated identity of the normalized publication target plus
`ProjectId`; neither a path nor its digest alone is mutation authority. The
payload lives under stable per-user state rather than process-temporary storage
and is admitted only after the current schema-v3 owner manifest durably binds
both members of that allocation identity. Save As keeps that owner allocation
stable; the Recovery Manifest alone owns canonical-path rebinding and recovery
points.

`ProjectRuntimeLease` composes independent kernel-backed exclusions for the
logical Project ID, every admitted publication target, and the exact runtime
root. File presence, PID text, or a path-only lookup grants no authority.
Namespace identity is revalidated before mutation, and every owner mismatch or
indeterminate publication fails closed. The active Session, persistence worker,
and recovery publisher share the exact lease; completions carry only scalar
identity. Platform path spelling, lock mechanics, and cleanup rules are defined
in [Persistence and Recovery](persistence-and-recovery.md).
Retired generation records may hold additional old-Project lease Arcs until
their `Weak<AssetLibrary>` evidence reaches zero. A second App process fails
before mutation whether it targets an alias, a copied archive
with the same Project identity, or the same owner-valid runtime tree. This lease
does not prove the current Authoring Session or Asset Library generation;
in-process work validates those semantic identities separately.

Recovery discovery returns complete immutable candidate evidence rather than a
path pair: Project ID, paired runtime root, canonical/autosave paths,
author/Library/document revisions, timestamp, and archive hash. Candidate
ordering is deterministic. A same-Project canonical archive suppresses only an
older document revision; equal revisions and missing canonical archives remain
recoverable. Selection and post-lease admission both match the exact evidence.
Crash cleanup is lease-scoped and conservative: it removes abandoned direct
staging leaves and manifest-unreferenced autosaves only under that exact root;
a missing/invalid manifest, unknown entry, directory, link, or sibling root is
preserved.

Stable runtime children are established only through that leased owner Module.
In particular, first autosave durably creates or validates the direct
`autosave/` directory before a worker creates either its SQLite snapshot or
archive; unconfirmed/indeterminate publication admits no archive, and a file or
symbolic link at that location fails closed. Every autosave leaf includes
the captured wall-clock millisecond, Author Generation, and a fresh random UUID,
and the archive remains `CreateNew` through its final namespace operation. The
name is diagnostic uniqueness, not authority: a collision fails closed without
changing an existing archive, and a published autosave is immutable. Archive
open extracts into a new immutable, explicitly ephemeral
`library-generation-<uuid>/` and opens SQLite only after extraction and schema
checks complete. This regenerable generation has no recovery-directory
durability obligation. Project replacement
first pauses the old Persistence Generation and waits for its FIFO barrier.
Candidate failure drops the uninstalled generation and resumes only the exact
pause token; success retires that token, records the prior library through weak
lifetime evidence, and installs the already-validated Session binding. No live
SQLite directory is renamed, swapped, or replaced.

Creating a new Project has an additional first-publication boundary. Its
destination carries create-only/no-overwrite intent through archive preparation
to the final kernel namespace operation; a preflight `exists()` observation is
never the mutation authority. The candidate Session, Library generation,
runtime Lease, and manual destination remain uninstalled until the exact initial
manual publication succeeds, the Completion matches the candidate Session,
Persistence Generation, Lease,
author generation, Asset Library revision, request, and destination, and
`AuthoringSession::mark_saved` accepts the resulting durable baseline. Only
then may the old handoff token be retired and the candidate become App
authority, with its manual destination replaced by a `ReplaceExisting` binding
for every later Save. Submission, publication, Completion-evidence, baseline,
or handoff failure retires the uninstalled candidate and resumes only the prior
exact handoff token. A file that was durably published before a later lifecycle
failure is merely an ordinary external artifact; it cannot install App state.

Publication preparation claims its bounded opaque sibling with direct
`create_new`/no-follow semantics rather than truncating an existing pathname.
The retained writer object remains the verification source: the archive Module
reads every exact entry through EOF to prove length and ZIP CRC, then streams
the whole file through the same filesystem object to compute its exact length
and SHA-256. It neither reopens the temporary pathname nor reparses the Project.
Immediately before the platform atomic primitive, the Module requires
`symlink_metadata` to report a direct regular file and proves the named
filesystem object still matches the retained handle identity. RAII cleanup
repeats that proof and preserves any already-observed replacement instead of
deleting it as if it were owned. The source handle remains open through the
single [Storage Publication](storage-publication.md) operation. Replacement is
pathname-based: the Project Runtime/publication Lease coordinates Mondrian
publishers, while Storage verifies source/target identities and classifies the
observed postcondition. Neither layer claims a portable compare-and-swap against
an uncooperative third-party writer.

After and only after durable publication, the archive Module returns one
private-construction, non-`Clone`, by-value
`ProjectArchivePublicationEvidence`. It binds the exact publication mode and
path to the embedded Project identity/document revision and observed whole-file
length/SHA-256. Recovery Manifest publication that admits a new recovery point
consumes that value, rejects any mode other than `CreateNew`, and derives its
path, Project ID, document revision, and hash only from the evidence. Production
therefore does not reopen or deserialize a just-created autosave between archive
and Manifest publication. The evidence proves one construction event; it is not
continuing path authority.

A matching persistence Completion may advance publication evidence but cannot
author Project metadata. `AuthoringSession::mark_saved` requires exact Author
Generation and Asset Library revision, verifies that every authored
`ProjectMeta` field equals the current snapshot, and accepts only the
publication-owned `updated_at`. Forged metadata therefore rejects the whole
baseline update atomically.

Recovery admission opens manifest, source, canonical Project, and staging
leaves as direct regular files. It hashes, validates, copies, and finally loads
through retained file objects, so a verified staging pathname is never reopened
after another filesystem actor could replace it. Discovery, selection, and
recovery all rehash and reparse the current archive object despite the original
publication evidence. A proven pre-namespace Manifest failure leaves the old
Manifest and all archives it references intact. A durability-unconfirmed or
namespace-indeterminate result suppresses archive cleanup because the current
Manifest authority is not proven. A new unreferenced create-only archive may
remain for conservative lease-scoped cleanup but can neither overwrite nor
invalidate an older recovery point.

Production edit commands cannot borrow the canonical document mutably. They
edit and validate a detached candidate through `AuthoringSession`. The Session
stores one Project validation certificate instead of a separate dependency
index or dirty-flag cache. Ordinary Sequence preparation consumes the candidate
and produces a `PreparedProjectSequenceReplacement` whose document and
certificate share one outer Sequence-list COW root. Complete identity/link,
Transition, and Audio Program rules always run; local validators reuse only
exact unchanged subtrees. The dependency certificate validates its strong
author baseline, default identity, active-target presence, nested references,
child-output obligations, and cycles.

Project-structural commands retain a detached complete-Project candidate because
they may add/remove Sequences, change Project-owned settings, or change
navigation. In either scope, revision arithmetic, structural exact-before
comparison, and semantic validation finish before History preparation. History
reads the already-validated Sequence from the prepared ticket; after History
commit, ticket consumption installs its paired document/certificate without
allocation or failure. Every Project replacement validates the detached
document contract. If its ordered Sequence bodies, default Sequence, and color
environment are exactly unchanged, a new outer certificate reuses the existing
strongly anchored Sequence/dependency evidence; structural, Sequence-body,
default, and Project color changes build a complete new certificate. Sequence
and Project change detection use
their typed structural equality contracts rather than serializing JSON as a
comparison mechanism. History admission plus candidate installation and
generation publication then form one atomic operation. Runtime-only state must
not leak into Project JSON unless it becomes an explicit Project contract.

History stores typed before/after `AuthoringSnapshot` roots. The author model's
copy-on-write allocations are shared between candidates and adjacent History
endpoints. Collection compiles each allocation into an immutable,
allocation-local descriptor containing its own logical charge and child edges;
one retained-allocation index reference-counts the reachable descriptor union
across Undo and Redo, so an allocation is charged exactly once while reachable
and its descriptor is retired when the final reference leaves both stacks.
The same versioned, conservative logical total includes command, description,
affected-ID, and per-entry Arc-slot metadata. It intentionally does not claim
allocator-exact heap use or process RSS.

Record preparation overlays the new roots before applying branch-Redo removal
and budget eviction. It computes the complete checked 200-entry/256 MiB
candidate and reserves index/cache changes without mutating live History.
Commit rejects a stale History revision before publishing the stack state,
reference-count plan, or descriptor-cache updates. Undo and Redo move the same
typed command and therefore preserve retained bytes.

These implementation invariants are not performance acceptance. Large-Project
qualification uses the source-attested schema-10 protocol-v1 matrix and the
timing, locality, reversibility, deterministic History-charge, and native
Private Commit gates defined in [Timeline Model](timeline-model.md).
Deterministic History charge and process memory remain independent evidence;
Working Set is diagnostic only, and a recorded result never transfers authority
to a later source revision.

An authoring transaction that cannot be retained is a History barrier, not a
hole that old complete snapshots may cross. Admission retires every reachable
Undo and Redo root before publishing the unretained commit, and diagnostics
separate configured zero-entry retention from a command that exceeds the byte
budget. If retention later resumes, Undo can reach only a state authored after
that barrier.

## Selection

`SelectionState` is the single source of truth for selected tracks, clips, effect, and mask. Inspector, timeline, viewer, and node graph must coordinate through this state rather than keeping panel-local copies.

Selection/navigation updates that follow a user command may be immediate UI
state changes and should not create a second undo entry unless the selected
object itself is edited. `EnterNested` and `ReplaceRoot` are distinct Session
operations: the former records a validated parent return edge; the latter
clears all nested ancestry. Project restore preserves the current target when
it still belongs to the restored collection and otherwise uses that restored
document's validated active fallback.

## Asset Library

The project runtime contains one immutable
`library-generation-<uuid>/index.db` for each live or not-yet-collectable
Project Library Generation. On save, a consistent SQLite snapshot is streamed
to the archive entry `library/index.db`; on open that entry is extracted into a
fresh runtime generation. Timeline clips reference assets by `AssetId`.

An Asset row is a Project-contained strong record, not a disposable cache
entry. Ordinary product removal changes only Asset Library membership:
`retired_at` hides the row from `list_assets`/folder listings while
`get_asset(AssetId)` continues to resolve the complete identity, expected
media/Component contract, interpretation, and recoverable provider binding.
Timeline placements across every Sequence, Project proxy-mode membership, and
Undo/Redo endpoints are unchanged. Consequently a retired but online record
continues to execute, a retired offline binding remains diagnosable/relinkable,
and a missing row is a broken Project relation rather than an offline state.
Retirement is an Asset Library transaction rather than an Author Transaction.
It therefore creates no Sequence/Project History entry: the next Undo/Redo
targets the preceding Author Transaction and leaves the Library membership
retired. Reimporting the same canonical concrete path is the current restoration
operation and preserves the same `AssetId`.

Single and multi-selection removal share one Library transaction. It
deduplicates and preflights every requested visible Asset and folder before
retiring any row, unlinks surviving rows from the complete removed folder
closure, then commits once. Notifications publish only after that commit.
There is deliberately no record-level physical-purge Interface: Project
Document plus both History stacks do not yet share a
transaction authority with SQLite, so exposing purge would permit an Undo
endpoint to outlive its strong record. New Projects instead allocate a fresh
unique Library Generation; the Asset Library exposes no destructive record
reset or per-record purge Interface.

`AssetLibrary` creates and then canonicalizes its root once and retains the
database identity as an ordinary native `PathBuf`. File-backed Asset candidates
likewise canonicalize the existing source once and require lossless UTF-8 for
the current SQLite TEXT schema. Row decoding fails closed on relative paths,
dot/parent traversal, device namespaces, non-file verbatim namespaces, or
non-ordinary spelling; offline media does not weaken that admission.

Windows verbatim prefixes are not persisted into Project data, manifests,
runtime allocation identity, or Asset records. Immediately before opening the
live database or an opaque backup sibling through SQLite, the Asset Adapter
projects an ordinary absolute drive path to `\\?\...` and an ordinary absolute
UNC path to `\\?\UNC\...`. This physical Win32-VFS adaptation keeps
`index.db-journal`, `index.db-wal`, and `index.db-shm` usable beyond legacy
`MAX_PATH`. Incoming drive/UNC verbatim paths normalize back to ordinary
identity; device and non-file namespaces are rejected. Non-Windows paths pass
through unchanged.

Archive construction is a low-peak-memory persistence Module. The canonical
Manifest and Project document serialize directly into their `ZipWriter`
entries, while the SQLite snapshot is copied from a cloned retained handle to
the exact identity-reclaimed object; its opaque pathname is never reopened.
None of the three payloads is first materialized as a second complete byte
buffer.
The Project and Library entries opt into ZIP64 before streaming begins, so
crossing the classic 32-bit ZIP size boundary cannot fail after most of a large
entry has already been written. ZIP64 capability is independent from open
admission: a caller may deliberately choose a larger read budget for an
exceptional Project without changing the file format.
One streaming writer records the exact uncompressed length and SHA-256 of every
byte accepted by each entry. Before atomic publication, the verifier retains a
clone of that exact filesystem object, requires the archive-v1 entry set, and
streams every required entry to EOF, checking its declared and observed length
plus SHA-256 against construction evidence. Reading through EOF is mandatory
because it also exercises the ZIP CRC. The Module then streams the complete
archive through the same object to prove whole-file length and SHA-256. It
deliberately does not deserialize a second `ProjectDocument`: author validation
occurred before construction, and no pathname reopen is required to prove the
constructed bytes.

Archive Open is a deep single-consumption Module. Its
`PreparedProjectArchive` Interface accepts one retained file object plus a
`ProjectArchiveReadBudget`, first admits the compressed file length, then
requires exactly the three archive-v1 entries with no duplicate, directory, or
extra entry. It admits each declared uncompressed length before parsing or
copying. Bounded counting readers also prove actual bytes equal the ZIP
declaration and never cross the per-entry limit; reading the Library through EOF
exercises CRC before publication into the runtime generation. The ordinary
default is 2 GiB compressed, 64 KiB Manifest, 768 MiB Project JSON, and
1.5 GiB Library. These are resource-admission limits, not a media-duration
formula or a minimum-memory concurrency promise; callers may supply another
explicit budget.

Preparation validates the Manifest and fully deserializes the canonical
`ProjectDocument` exactly once, then exposes only its stable `ProjectId`.
Ordinary Open uses that identity to claim the paired Project Runtime Lease;
Recovery compares it with the Lease already acquired from the separately
authorized candidate. Both then consume the same ticket to extract SQLite into
the fresh, still-uninstalled Project Library Generation. Convenience
document-only/load functions delegate to this Module. Once an ordinary source
or exact verified Recovery staging object enters the archive-loader Interface,
the lifecycle never performs a Project preflight deserialization followed by a
second load deserialization.

Library extraction creates one exclusive direct sibling staging file. Its RAII
owner retains filesystem-object identity, streams and syncs the complete entry,
and atomically replaces `index.db` only after byte-count and CRC acceptance.
Failure removes only the still-identical staging object and cannot modify an
existing runtime Library target.

Generated assets such as adjustment layers and solid colors are represented by
the closed `AssetSource::Generated` variant. They have neither a physical path,
a file fingerprint, nor media probe facts. The SQLite Implementation may retain
an opaque legacy uniqueness key, but that value never crosses the Asset Library
Interface as file-backed authority.
Basic Title is deliberately different: it is closed Sequence-local Clip content
and creates no Asset Library row.

The current Library schema stores the `audio_components` catalog beside probe
metadata, user interpretation, and complete authorizing source-revision
evidence. The catalog owns stable logical audio Component IDs and conservative
physical-stream signatures. Timeline and Project JSON never persist FFmpeg
stream indices; Export copies validated reachable selections into its immutable
execution snapshot. Explicit stream repair is committed atomically with
refreshed probe and fingerprint evidence while preserving the logical Component
ID.
Automatic reconcile does not retarget an existing ID or create physical-stream
aliases; only an explicit rebind may alias a stream, so another Project's stable
reference never has to be deleted as a side effect of repair. A separate refresh
operation re-probes and conservatively reconciles current candidates without
retargeting; this is required before selecting a newly discovered stream whose
index was absent from stored metadata.

Library schema v4 uses nullable `retired_at` as the sole persisted membership
state. Existing records without a retirement timestamp remain visible.
Retirement never clears a path, probe, source fingerprint, Component catalog,
or interpretation, and ordinary list filtering never weakens identity lookup.

`mondrian-core::MediaProbeSnapshot` and `MediaFileFingerprint` are the stable
foundation-owned persistence contract. The concrete FFmpeg Adapter lives in
`mondrian-media` and returns that contract through `probe_media_info`; the Asset
Library neither links FFmpeg nor initiates probing. Its mutation Seam accepts
one validated immutable `AssetMediaProbeCandidate`, rechecks the physical
fingerprint at commit, and publishes Asset identity, probe facts, Component
catalog, and optional target folder in one SQLite transaction. Relink and
Component repair use the same candidate rule.
The probe snapshot deliberately contains no path or fingerprint: `AssetSource`
owns the canonical path and the candidate is the only value that closes path,
fingerprint, and probe facts into one commit.

Persisted video sampling fields may retain a storage fallback when the concrete
decoder reports a pixel format that the stable contract cannot represent. Such
a fallback is never execution evidence. Consumers must obtain the single
`VideoStreamInfo::proven_sampling()` projection before selecting a native
surface, choosing proxy precision, or concluding that Alpha is absent. Missing
or internally inconsistent evidence fails closed before Preview decode or
proxy generation until a fresh probe can prove a supported sampling contract.
This avoids silently reducing unknown precision or possible Alpha without
changing the persistence schema.

The current `MediaFileFingerprint` is one internally consistent open-file
revision observation: length, second/nanosecond modification time, filesystem
object identity, and that object's filesystem-owned change generation must all
be present and equal before cache, stream-binding, or Session reuse is
authorized. Unix uses device/inode plus inode-change time; Windows uses volume
serial/file ID plus handle-observed change time. Unsupported filesystems or any
missing evidence produce a partial fingerprint whose `authorizes_reuse()` is
false rather than falling back to length and mtime. This value is conservative
source-revision evidence, not a cryptographic content hash or durable
filesystem identity, but a same-length replacement or restored mtime cannot
masquerade as the admitted revision under the complete contract.

Schema migrations never combine old probe JSON with a newly observed file
fingerprint. Upgrading pre-v3 state retains logical Component IDs but clears the
authorizing fingerprint, so the record reports `needs_reprobe` and all physical
bindings fail closed until one coherent fresh candidate is committed.

## Format Evolution

The current document layout intentionally stays single-document:

```text
manifest.json
project.json
library/index.db
```

Archive `format_version`, document `schema_version`, and embedded library
`PRAGMA user_version` are independent contracts. `manifest.json` records the
expected library schema version in addition to archive layout. The current
archive-v1 manifest explicitly writes `library_schema_version: 4`.

Archive and document JSON pass through separate version registries before typed
deserialization. The ordinary current-schema path streams each JSON ZIP entry
through a bounded reader directly into its typed Manifest or
`ProjectDocument`; exact entry-set and read-budget admission precede parsing.
The value-based registry remains the explicit seam for future migrations
without imposing its peak-memory cost on current Projects.

Document schema v22 is the sole accepted Alpha author schema. It persists the
Project-owned color environment and future-Sequence template, exact rational
`TimelineTime`, canonical signal layouts and channel mappings, typed Routes and
processor schemas, canonical proxy membership, closed `ClipContent`,
multi-member link groups, strong visual Transitions, complete Mask and Basic
Title properties, a Clip-local visual author origin, closed Sequence `color`
and `delivery` structures, and one tagged `ClipSourceTimeMap` whose terminal
boundary is derived from duration. Unknown fields and older or future document
versions fail closed; no alias, fallback, default synthesis, or inferred
migration is promised during Alpha.

SQLite schema ownership remains in `mondrian-assets`; the current version is
v4. Its ordered `PRAGMA user_version` registry applies each step in one
transaction, validates the resulting tables and columns, and rolls back both
DDL and version on failure. SQLite migrates only the extracted runtime copy;
opening never rewrites the source `.mdp`.

Future split-entry layouts require an archive migration and a new
`format_version`; persisted author-shape changes require an explicit document
schema decision and round-trip fixtures. A schema number advances only when
the complete current semantics are represented, validated, and covered by
save/reopen tests.

Checked current-document and pre-current SQLite fixtures must cover idempotent
open, migration where supported, save, reopen, and semantic fingerprint
retention.
