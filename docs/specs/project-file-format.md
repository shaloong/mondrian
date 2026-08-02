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
  "library_entry": "library/index.db",
  "library_schema_version": 4
}
```

`project.json` serializes the canonical `ProjectDocument`:

- `schema_version: u32`
- `project_id: ProjectId`
- `document_revision: u64`
- `meta: ProjectMeta`
- `settings: ProjectSettings`
- `color_environment: ProjectColorEnvironment`
- `new_sequence_defaults: SequenceSettings`
- `sequences: SequenceCollection`
- `proxy_mode_assets: AuthoringSet<AssetId>` in memory, serialized exactly as
  the canonical ordered `BTreeSet<AssetId>` JSON array

`library/index.db` is the project asset library.

Archive v1 requires exactly these three direct file entries. Duplicate,
missing, directory, and additional entries are invalid even when a ZIP reader
could otherwise select one by name. Project JSON and Library entries are
written with ZIP64 size fields from the start; readers must therefore support
ZIP64 even when a particular Project remains below 4 GiB.

The current independent versions are archive v1, document schema v22, and
library schema v4. Document schema v22 is the sole accepted Alpha author
contract. It requires closed Project/Sequence/Clip structures, including one
mandatory tagged `source_time_map`; its constant variant contains
`source_origin` and exact `scale`, and derives the terminal source boundary from
Clip duration. Future retiming extends this closed algebra rather than adding
parallel mutable range fields. Older and future document versions and unknown
author fields fail closed because no compatibility migration is promised yet.

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

Save captures one immutable Authoring Session generation and an SQLite online
backup. The backup is reclaimed by File ID/inode, and archive construction
streams `library/index.db` from a clone of that retained exact-object handle;
it never reopens the opaque SQLite snapshot pathname. It then exclusively
creates and flushes a direct sibling archive, validates every entry and ZIP CRC
through the exact retained archive object, streams that same object to obtain
whole-file length/SHA-256, rechecks that its namespace still names that object,
and crosses one platform atomic replace/create boundary. It does not reopen
either temporary pathname or deserialize the Project a second time. A failed
serialization, database backup, validation, flush, or pre-namespace
publication leaves the existing target untouched. A failure after namespace
authority is acquired is classified from retained object identities; callers
never infer its result from path existence.

Only a successful atomic namespace operation followed by a confirmed
containing-directory durability barrier produces the opaque,
private-construction, non-`Clone`, by-value
`ProjectArchivePublicationEvidence`. `BeforeNamespace` proves that the new
object did not become the target; `DurabilityUnconfirmed` proves that it did but
cannot prove crash survival; `NamespaceIndeterminate` proves neither
postcondition. Neither target visibility nor successful rename/move alone is
save evidence, and only confirmed durability may advance the manual baseline or
authorize Recovery retention cleanup.

The evidence binds exact path, publication mode, embedded Project ID/document
revision, whole-file length, and SHA-256 as point-in-time construction
evidence. An autosave leaf is named
`project-<unix-ms>-g<author-generation>-<random-uuid>.autosave.mdp` and must be
published with `CreateNew`; the UUID makes the immutable leaf unique, while an
unexpected collision still fails without overwrite. Recovery Manifest
publication that admits a new point accepts only create-only evidence and
derives its archive path, Project ID, document revision, and SHA-256 from that
value without reopening or reparsing the just-published archive.

This evidence is not long-term pathname authority. Discovery, selection, and
recovery still open direct regular-file objects, recompute their complete hash,
and reparse archive/Project identity before use. The Manifest is atomically
published only after its archive. A `BeforeNamespace` Manifest failure proves
the prior Manifest and every archive it references remain authoritative. A
durability-unconfirmed or namespace-indeterminate result suppresses deletion
because the current Manifest authority is not proven. The new unreferenced
create-only archive may remain for conservative cleanup and cannot damage an
older recovery point. Pathname-based atomic primitives retain the documented
sibling directory as a residual trust seam protected by the production Runtime
Lease.

## Open Admission

Open uses one single-consumption prepared archive ticket. It enforces the exact
entry set and a caller-selected `ProjectArchiveReadBudget` before parsing JSON
or copying SQLite, validates the Manifest and canonical Project once, exposes
the Project ID for Runtime Lease acquisition, and then extracts the Library
through the same retained archive object. Both declared and actual
uncompressed entry lengths are bounded and must agree; Library extraction reads
through EOF so a CRC failure cannot publish a staged runtime database.

The ordinary default limits are 2 GiB compressed archive, 64 KiB Manifest,
768 MiB Project JSON, and 1.5 GiB Library. These are product
resource-admission defaults, not media-duration heuristics or a promise that a
minimum-memory machine can admit the configured limits alongside other
workloads. A caller may replace them explicitly for another execution
environment.
