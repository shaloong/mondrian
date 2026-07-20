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

The current SQLite schema is v2. Its `audio_components` column persists stable
Asset audio Component identities, conservative stream signatures, and the file
fingerprint used for the probe. The v1→v2 migration derives catalogs from each
complete media record inside one migration transaction; malformed metadata
rolls back the column and version rather than writing a partial binding.

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
color migration or substituted with the latest package. Deserialization accepts
only complete registered package identities: individually valid v2/v3 field
values cannot be mixed into an unregistered hybrid identity. Archive and preview
fingerprints, OCIO CPU processor keys, and renderer GPU shader keys all include
the exact package identity.

Document schema v13 retains the v5 Custom OCIO reproducibility contract and the
v6 removal of the redundant sequence-level ACES workflow selector. It also
requires the Mondrian Standard package identity to pin both the SDR and
1000-nit HDR View Transform IDs and versions; the old single default-View field
cannot fully describe HDR project semantics. Version 8 additionally requires a
complete `ParameterSchema` on every persisted visual property; stable identity,
unit/range/interpolation/enum/resource and cache semantics cannot be inferred
from an instance path. Version 9 moves value type, definition default,
automation capability, and the three execution interpolation semantics into
that shared schema and uses the same schema-plus-exact-curve contract for audio
Processor parameters. Editor-only Bezier presets are no longer serialized as
definition capabilities. A Custom project
stores its config/content and executable processor graph identities together
with working/display/view/look/role selections.
Opening the archive must reload and validate the selected external config; a
missing config, edited LUT, changed role, or changed default resource is an
open diagnostic, never a silent substitution. Schemas v5 and v6 are
deliberately not migrated during Alpha: v6 made project `ColorEngine` the sole
color-mode selector, while v7 completes the Standard View identity, v8
establishes stable parameter identity, and v9 completes the cross-media
parameter definition contract. Version 10 separates the persisted monotonic
Sequence author revision from the Project document's successful-save revision
and rejects invalid author identity graphs before they enter runtime state.
Version 11 replaces the two legacy Sequence position-display fields with one
mandatory `TimelineDisplaySettings` payload. It preserves a signed actual-frame
timecode origin independently from Frames/SMPTE presentation, rejects invalid
drop-frame/rate combinations, and deliberately provides no Alpha migration from
v10.
Version 12 replaces the closed audio-layout enum with canonical named-speaker
sets and bounded Discrete buses. Standard layouts serialize their speaker
positions; invalid, empty, duplicate, or over-capacity layouts fail during
deserialization, and v11 is deliberately not inferred during Alpha.
Version 13 makes each persisted audio Component's source-to-Sequence mapping
explicit as either the versioned fail-closed Standard policy or one canonical
sparse matrix. Matrix source/destination layouts, channel bounds, coefficient
finiteness and duplicate edges are validated at deserialization and Sequence
closure boundaries; v12 is deliberately not inferred during Alpha.
Current new sequences default to SceneReferred and persist the selected engine's
package-pinned rendering View intent; DisplayReferred is the explicit
direct-colorimetric bypass.

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
