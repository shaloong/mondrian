# Storage Publication

`mondrian-storage` is the only native filesystem-publication implementation in
the workspace. It is deliberately domain-free: Project archives, Recovery
manifests, Asset Library snapshots, Export outputs, Proxy media, and their
callers own validation, retention, retry, and user-facing policy, while this
crate owns object identity, namespace mutation, and durability evidence.

## Correctness Contract

A successful publication means all of the following:

1. the source is one unique direct sibling of the final target;
2. the exact source object was durably flushed before namespace mutation;
3. the requested create-or-replace namespace operation names that object at the
   normalized absolute target;
4. the containing directory publication barrier completed; and
5. the caller received non-cloneable `FilePublicationEvidence`.

Existence of the target, a successful encoder exit, or a successful rename
alone is never durable-completion evidence.

Every failed file publication is classified exhaustively:

- `BeforeNamespace`: postconditions prove that the new object did not become
  the target. Retrying is safe under the caller's original create/replace
  policy.
- `DurabilityUnconfirmed`: the target is proven to name the new object, but the
  directory durability barrier failed or could not be confirmed. The caller
  must not report a durable save, delete older recovery authority, or retry as
  a fresh create.
- `NamespaceIndeterminate`: object identities cannot prove either the old or
  the new namespace state. Automatic cleanup and blind retry are forbidden;
  verified surviving source names are retained as evidence when possible.

Direct-child directory creation uses the equivalent three-state
`DirectoryPublicationFailure`. A file, link, reparse point, or other
non-directory collision is a `BeforeNamespace` failure. An existing direct
directory is not upgraded from `AlreadyExists`: pathname presence has no
construction evidence in the current attempt and is reported as
`DurabilityUnconfirmed`. If an earlier attempt may have created that child,
only a domain that proves ownership and can safely remove an identity-stable,
empty directory may recreate it; otherwise it remains unconfirmed or
indeterminate.

Directory evidence proves only shape, stable identity, and namespace
durability. It is not ownership evidence. A domain must first constrain the
child to an owned namespace and validate any existing entry under its own
lease/manifest rules. In particular, Project Runtime roots publish a durable
owner manifest before payload admission, and recovery children exist only
under a live Project Runtime Lease. The shared primitive never adopts an
arbitrary external directory as owned state.

`OwnedPublicationDirectory` is the populated-tree counterpart to
`OwnedPublicationFile`. It allocates and records one unique sibling directory,
allows a domain to populate and validate only below that identity-bound root,
recursively rejects symbolic links and unsupported objects, synchronizes every
regular file and directory, and then publishes the complete tree with
create-new semantics. The final route is absent until one namespace operation
makes the complete tree visible. Recursive replacement is deliberately not a
Storage primitive: a caller that needs version replacement must publish a new
immutable generation and switch a small manifest/pointer through the atomic-file
Seam.

The Export Image Sequence Master Module is a direct consumer. It populates one
identity-bound sibling with deterministic numbered frames, independently proves
every frame's file representation and decode, hashes the complete inventory,
and durably adds manifest schema 2 before publication. The final path uses only
create-new publication; cancellation and ordinary encoding/validation failure
let the owned sibling clean itself up, while a failure after the source is
preserved for namespace publication reports that exact recoverable staging path.
No retry scans loose frame names or adopts an existing output directory.

IMF and DCP professional delivery use the same
`OwnedPublicationDirectory` Interface. The Export Module wraps essence and
constructs CPL/PKL/AssetMap documents only below one identity-bound sibling,
reimports the closed inventory, runs an independent standards validator, and
only then requests durable tree publication. AS-11 X9 uses
`OwnedPublicationFile` plus the external-writer reservation/reclaim protocol.
Neither path exposes a partially populated final route or treats tool exit as
durability evidence.

`ensure_durable_directory_chain` accepts one caller-selected, already-existing
absolute anchor and one strict absolute descendant. It publishes each missing
suffix node through the same direct-child seam. It never walks or flushes
filesystem roots, UNC shares, home directories, or other system ancestors
outside that trusted anchor. An unexpectedly existing suffix node fails closed
instead of becoming evidence. Recovery-bearing callers may establish their own
versioned durable marker at the final owned directory to authorize later
reentry without asking Storage to infer historical ownership.

## Owned Source Object

`OwnedPublicationFile` creates a unique sibling, holds its native file handle,
records its file identity, and removes only that same object when cleanup is
safe. Publication never locates a temporary object later by a predictable
pathname.

When FFmpeg or another path-only process must write the payload,
`release_for_external_writer` yields an `ExternalPublicationReservation`.
`reclaim` reopens the path and requires the same file identity before the
result can be validated or published. A missing or statically substituted
reservation therefore fails closed. Reclaim is postcondition validation, not a
security boundary against an uncooperative same-user process continuously
racing a shared directory; domain callers must choose a trusted parent and use
their lease/owner authority. Opaque names make unrelated cooperating work
non-addressing but do not turn a path-only external writer into a handle-based
writer.

After reclaim, every in-process consumer that depends on the validated object
must use the retained handle. Reopening the opaque pathname would discard the
identity proof. Asset Library backup follows this rule: SQLite is the
path-based external writer, while Project archive construction streams the
reclaimed SQLite file through a cloned retained handle.

The default guard removes a proven unpublished source. A domain may call
`preserve_source_on_before_namespace_failure` only when that source is a
validated recovery artifact worth retaining; this does not convert the failure
into success.

`write_durable_file_atomically_with_mode` exposes the same owned-sibling
implementation for small byte payloads while retaining an explicit
`CreateNew`/`ReplaceExisting` choice. Establishing an ownership marker must use
`CreateNew`; a preflight absence check never grants overwrite authority over a
racing entry. The convenience wrapper without a mode remains replace-only.

## Platform Semantics

On Windows, publication uses `MoveFileExW` with
`MOVEFILE_WRITE_THROUGH` and optional `MOVEFILE_REPLACE_EXISTING`. The source
handle excludes shared writes and remains open through the namespace operation.
Return codes are not trusted by themselves: old, new, source, and target file
identities classify the observed postcondition. Cross-volume copy fallback is
forbidden. Every native file and directory publication uses one
extended-length path encoder, including correct `\\?\UNC\` projection for UNC
paths. Directory creation uses a unique sibling plus a real
`MoveFileExW(..., MOVEFILE_WRITE_THROUGH)` namespace move. Same-path no-op moves
and directory `FlushFileBuffers` calls are forbidden because their public
contracts do not prove a parent namespace barrier. `AlreadyExists` alone is
never durable evidence.

On Unix, temporary files are created with private permissions. Replacement uses
same-directory `rename`; create-only publication uses `link` so an existing
entry cannot be overwritten. The file is synchronized before publication and
the parent directory is synchronized afterward, retrying interrupted barriers.
Replacement preserves the existing target's permission bits. New directories
are mode `0700`, and direct-directory creation synchronizes the parent
directory. Once `create_dir` succeeds, identity/probe failure is necessarily
post-namespace and therefore can only be `DurabilityUnconfirmed` or
`NamespaceIndeterminate`, never `BeforeNamespace`.

Both paths require an existing normalized absolute parent and reject symlink or
non-file source/target substitutions. The API does not promise network
filesystem guarantees stronger than the operating system can evidence. It also
does not promise atomic delete-by-handle on platforms that expose only
pathname unlink: cleanup validates identity immediately around removal and
preserves any already-observed replacement, while hostile concurrent mutation
remains outside the Storage trust contract.

## Domain Responsibilities

Callers must:

- render or serialize into the owned sibling, never directly into the target;
- validate the complete payload while it is still bound to the owned object;
- select `CreateNew` for establishment and `ReplaceExisting` only for a known
  established lineage;
- propagate all three failure states without reducing them to a success/failure
  string;
- advance saved generations, retire Recovery points, or publish completion
  notifications only from durable evidence; and
- retain or quarantine indeterminate artifacts instead of broad pathname
  cleanup;
- consume reclaimed objects through their retained handle whenever the
  downstream Interface can accept one; and
- select a trusted existing anchor explicitly and use domain-owned marker or
  owner-manifest authority for any already-existing suffix; Storage does not
  infer that authority.

Project persistence additionally poisons an establishment destination after a
namespace-indeterminate result. A durability-unconfirmed create is treated as
established for subsequent replace semantics, but not as a saved author
baseline. Recovery deletes old archives only after its new manifest has durable
evidence. Export completes a job only after output validation and durable
publication. Proxy generation may regenerate a proven pre-namespace failure,
but cannot erase an unknown artifact.

## Forbidden Alternatives

No consuming crate may add its own backup/restore rename sequence,
`ReplaceFileW`, blind `.part` deletion, path-existence inference, direct target
truncation, or string-based publication-state detection. Those patterns create
parallel and contradictory durability models.

Tests for every consumer must cover create collisions, replacement, external
writer identity substitution where applicable, and the domain consequence of
all reachable failure classes. Platform-specific fault injection belongs in
`mondrian-storage`; domain tests assert policy over its typed results.
