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
- `proxy_mode_assets: BTreeSet<AssetId>`

`mondrian-core` keeps only shared project metadata and settings types. It does
not define a second top-level project container.

`document_revision` advances only after an explicit project save succeeds. It
is persistence/conflict evidence, not author-semantic identity. Each persisted
Sequence owns an independent monotonic `SequenceRevision`; Playback, Preview,
audio compilation, and render caches bind that revision, so an edited draft or
failed save cannot continue under an older Timeline identity.

`ProjectColorEnvironment.engine` is the one persisted product-mode selector for
Mondrian Standard, ACES, or Custom OCIO. A Sequence never stores, overrides, or
inherits another engine. `SequenceColorSettings` stores only the working domain,
media-input policy, and Program Output policy interpreted inside the Project
environment. `SequenceDeliveryDefaults` separately stores encoded
range/bit-depth defaults and authored HDR metadata.

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

Replacing the Project color environment is one atomic Project action carrying
a complete `ProjectColorEnvironment`. `AppState` first prepares the exact OCIO
dependency, then validates the template and every Sequence. Only if all checks
pass does it commit one Project snapshot, stop playback and rotate preview
execution. It never rewrites Sequence settings or silently substitutes another
engine/output route. A failure leaves the previous environment and history
unchanged.

The Project Settings draft edits this same global environment. A Custom OCIO
selection is pinned to a complete config/resource/processor identity before it
can replace the draft. If an already-authored Custom dependency is unavailable
on reopen, the document still preserves the exact author intent; execution
prepare fails closed until the dependency is restored. It must not fall back to
Mondrian Standard, ACES, or a same-path-but-different config.

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

- selection state
- playback/audio state
- render queue/export draft
- clipboard and animation selections
- UI-facing status log

Production edit commands cannot borrow the canonical document mutably. They
edit and validate a candidate through `AuthoringSession`, which installs the
candidate, advances revisions/generation, and records history as one atomic
operation. Runtime-only state must not leak into Project JSON unless it becomes
an explicit Project contract.

## Selection

`SelectionState` is the single source of truth for selected tracks, clips, effect, and mask. Inspector, timeline, viewer, and node graph must coordinate through this state rather than keeping panel-local copies.

Selection/navigation updates that follow a user command may be immediate UI state changes and should not create a second undo entry unless the selected object itself is edited.

## Asset Library

The project runtime directory contains a SQLite asset library. On save, `library/index.db` is streamed into the `.mdp`; on open it is extracted into the runtime directory. Timeline clips reference assets by `AssetId`.

Generated assets such as adjustment layers and solid colors are represented as library records with synthetic `mondrian://...` paths.
Basic Title is deliberately different: it is closed Sequence-local Clip content
and creates no Asset Library row or synthetic path.

SQLite schema v2 stores an `audio_components` catalog beside probe metadata and
user interpretation. The catalog owns stable logical audio Component IDs,
conservative physical-stream signatures, and the source fingerprint that
authorized them. Timeline and Project JSON never persist FFmpeg stream indices;
Export copies validated reachable selections into its immutable execution
snapshot. Explicit stream repair is committed atomically with refreshed probe
metadata and fingerprint evidence while preserving the logical Component ID.
Automatic reconcile does not retarget an existing ID or create physical-stream
aliases; only an explicit rebind may alias a stream, so another Project's stable
reference never has to be deleted as a side effect of repair. A separate refresh
operation re-probes and conservatively reconciles current candidates without
retargeting; this is required before selecting a newly discovered stream whose
index was absent from stored metadata.

## Format Evolution

The current document layout intentionally stays single-document:

```text
manifest.json
project.json
library/index.db
```

Archive `format_version`, document `schema_version`, and embedded library
`PRAGMA user_version` are independent contracts. `manifest.json` records the
expected library schema version in addition to archive layout. A v2 manifest
is written with that field explicitly.

Archive and document JSON pass through separate version registries before typed
deserialization. During Alpha there are deliberately no legacy document steps:
schema v21 is the sole accepted author schema, and older/future versions fail
instead of being guessed. Version 5 introduced the explicit tagged
`mondrian_standard` / `aces` / `custom_ocio` project contract and makes every
Mondrian Standard package-identity field mandatory: product ID/version, config
ID/SHA-256, complete package SHA-256, and working-space ID/version. Custom OCIO
likewise requires config and processor-graph
digests, working/display/view/look identities, roles, and an explicit dynamic
property list; a mutable source path alone is not a project color definition.
Version 6 removes the redundant persisted `ColorWorkflow::Aces` and reserves
Standard/ACES/Custom mode selection for the Project-owned engine (now
`ProjectColorEnvironment.engine`).
The current application creates sequences as SceneReferred so Mondrian Standard
is the default Program Output View; DisplayReferred remains an explicit bypass
and the persisted enum keeps workflow separate from engine selection. Version 7 replaces the ambiguous
single default-View identity with mandatory, independently typed SDR and
1000-nit HDR View Transform IDs/versions. The Alpha format intentionally
provides no alias, fallback, or migration from v5/v6; a missing or edited
identity, missing parameter schema, or retired workflow fails closed. Version 8
separates stable parameter identity from instance addresses. Version 9 makes
value type, definition default, automation capability, unit/range,
Hold/Linear/Bezier execution semantics, enum/resource intent, and cache impact
one shared visual/audio `ParameterSchema`; audio Processor instances persist a
schema snapshot beside their exact curve. The registry remains the explicit
seam for adding a real migration policy only when compatibility becomes a
product promise.
Version 10 adds the nonzero persisted Sequence author revision and makes author
identity validation fail closed for duplicate identities and dangling strong
Clip references.
Version 11 replaces the independent `video_display_format` and
`start_timecode_frame` fields with mandatory `timeline_display`. The resolved
contract is shared by Viewer and Timeline, permits signed origins, validates
drop-frame against the exact Sequence rate, and removes unimplemented
Feet+Frames values from persisted author data.
Version 12 replaces the closed three-value audio-layout enum with one canonical
signal-layout value: Mono, a validated named-speaker set, or a bounded Discrete
bus. Standard Stereo and surround layouts serialize by semantic positions, so
5.1(side), 5.1(back), and equal-count custom layouts cannot alias. Alpha does
not guess a v11 layout migration.
Version 13 adds mandatory per-Component channel-mapping intent. `Standard`
resolves only against an exact source dependency and the owning Sequence
layout; `Explicit` persists one canonical sparse matrix with both layouts and
bounded finite coefficients. Native decoded PCM caching, Sequence Program
layout, and device/export delivery adaptation are separate contracts. Alpha
does not infer a v12 field or silently migrate an old FFmpeg downmix.
Version 14 makes every persisted `AudioRoute` a complete controllable signal
edge with mandatory enabled state, static dB level, and optional exact
Sequence-time level curve. Principal routes and parallel sends use this one
edge model; missing v14 fields are not inferred from v13 data.
Version 15 adds `Samples` as an exact Parameter Schema unit and permits exactly
representable integer audio-processor values. The canonical Sample Delay uses
that contract for a non-animatable sample-frame count; v14 documents are not
implicitly promoted during Alpha.
Version 16 makes forced-proxy Asset membership a canonical ordered set. Duplicate
IDs and input ordering can no longer change the document fingerprint.
Version 17 replaces parallel Clip kind payloads with closed `ClipContent`,
replaces pair links with multi-member `ClipLinkGroupId` membership, persists
typed visual Transitions with strong endpoints, and persists complete Mask
Property Bags. Unknown/legacy Clip fields, singleton link groups, invalid
Transition geometry, missing Mask parameters, and duplicate author identities
fail before the document enters a Session.
Version 18 adds `BasicTitle` as closed Sequence-local generated Clip content.
Its complete canonical Property Bag, exact requested font intent, and
visual animation state persist with the Clip. Validation rejects missing,
extra, schema-divergent, or out-of-range title properties before a document
enters an Authoring Session. Alpha does not reinterpret a v17 Clip or construct
title defaults while opening it.

SQLite schema ownership remains in `mondrian-assets`; the current version is
v2. Its ordered Registry uses
`PRAGMA user_version`, applies each step in a transaction, validates the current
tables/columns, and rolls back both DDL and version on failure. The former
best-effort `ALTER TABLE` calls that discarded errors have been removed.

Future split-entry layouts require an archive migration and new
`format_version`; persisted editing fields require an explicit schema decision.
SQLite migrates only in the extracted runtime copy. The source `.mdp` is never
rewritten by open.

Version 19 moves the exact engine into the mandatory top-level
`ProjectColorEnvironment`, adds the complete mandatory
`new_sequence_defaults`, removes engine/inheritance/display and nested-edge
policy from the then-current mixed Sequence color shape, and stores nested processing on each
`NestedSequence` Clip edge. Alpha intentionally provides no v18 migration:
duplicated engine truth and ambiguous nested ownership are rejected rather than
guessed.

Version 20 gives every Clip one mandatory exact `clip_time_in`, making the
Clip-local visual author domain explicit and persistent. Transform, Opacity,
visual Effects, Masks, and Basic Title share that domain. Sequence placement
and source sampling remain independent; Alpha rejects v19 instead of guessing
whether existing curves were authored against Sequence or source time.

Version 21 replaces the mixed `working_color_space`/`auto_tone_map_media`/
color-management shape with closed `color` and `delivery` objects.
`color` contains only working, input, and Program Output authoring; `delivery`
contains encoded defaults and authored HDR payloads. `ProjectDocument`,
`ProjectManifest`, `SequenceSettings`, `Sequence`, and `SequenceCollection`
reject unknown fields, so a forged Sequence engine or inheritance switch cannot
be silently ignored. Alpha rejects v20 instead of guessing this regrouping.

Current document schema v21 persists canonical rational `TimelineTime` values
directly and requires the shared visual/audio `ParameterSchema`. It does not
contain frame-oriented `TimeCode`, `TimeTicks`, descriptor-level duplicate
defaults/types, editor-preset interpolation capabilities, or compatibility
aliases. Alpha documents from earlier schemas are rejected rather than silently
deriving parameter identity, Clip content, link membership, Transition
endpoints, or Mask parameter state.

The checked current document fixture lives under
`crates/mondrian-project/tests/fixtures/current`; the SQLite upgrade fixture
remains under `crates/mondrian-assets/tests/fixtures/v0`.
