# Timeline Model

Color-context construction delegates target-qualified output View lookup to the
owning Project's exact `ColorEngine`. Sequence preview and export planning never read an
unqualified process-global OCIO default, so a failed ACES or Custom config
cannot inherit a View from the previously active engine or reuse one output
binding under another delivery label.

`mondrian-timeline` owns editorial time and the validated Sequence author model.
`mondrian-editor-state::AuthoringSession` owns project transactions and bounded
Undo/Redo; the Timeline crate has no second mutable document or command history.

Persisted positions, ranges, automation keys, and temporal handles use canonical
exact rational `TimelineTime`. `FramePosition` is an evaluation/display adapter,
not an author coordinate, and the former `TimeTicks = frame * 1000` path has
been deleted. Sequence frame rate remains a
video evaluation/snap grid and display-timecode input, not the universal storage
time base; audio edits may therefore retain sample-accurate boundaries without
creating a second Timeline model.

The `Action::MoveClipToTrack` and `Action::Seek` semantic inputs may receive a
`FramePosition` from an input Adapter, but its `time_base` is never discarded.
The App Action Adapter first converts the complete value to exact
Sequence-local `TimelineTime`, then lowers it exactly once onto the active
Sequence video evaluation grid with
`FrameRounding::Nearest`; invalid or negative coordinates fail before transport
or author mutation. A source-edge trim similarly maps the exact source-local
coordinate through the Clip source-time scale to an exact Sequence-local target
and only then applies the same Sequence-grid quantization. The source frame rate
must never quantize a Sequence position. This single lowering Seam makes mixed
24 and 30000/1001 inputs deterministic while keeping the persisted author model
independent of either grid.

## Sequence

A `Sequence` contains:

- stable `id`, monotonic author `revision`, `name`, `role`
- `settings`
- ordered video and audio tracks
- Sequence-owned explicit video Transitions
- playhead
- optional exact in/out range
- a Sequence-owned `AudioProgram`

Default sequences create `V1..V3` and `A1..A3`. `SequenceSettings` validates
resolution, frame rate, audio sample rate/layout, preview settings, and
color-management constraints. Audio layout is the sole persisted channel
authority; no parallel channel-count field can diverge from it.

### Structurally Shared Author Collections

Large ordered author collections use `mondrian_core::AuthoringList<T>`, an
owned copy-on-write list backed by an immutable shared allocation. This applies
at each high-cardinality author seam: the Project's Sequence collection;
Sequence video/audio Tracks, video Transitions, and audio Roles; Track Clips;
Clip Effects, Masks, and audio Component Edits; and Audio Program Scopes,
Transitions, Buses, Outputs, and Routes. Ordered audio processor Racks use the
same list contract. Keyed audio Track channels and processor-parameter
snapshots use the serde-transparent `AuthoringMap<K, V>` counterpart, while
Project proxy-mode membership uses the representation-transparent
`AuthoringSet<T>` counterpart and opaque plugin-state bytes use
`AuthoringList<u8>`. A Sequence candidate
therefore does not deep-copy every Track processor and unavailable plugin blob
before the edit reaches the relevant audio branch.

Cloning a detached Sequence candidate therefore shares these list allocations.
The first mutable access to one list detaches only that list through
copy-on-write; nested lists remain shared until their own mutation seam is
crossed. Callers still edit through ordinary `&mut Sequence`, Track, and Clip
Interfaces, and no interior mutability or shared mutable author state is
introduced.

Candidate transformations must determine no-op cleanup and affected ownership
before requesting mutable COW access. A multi-Track edit may detach only the
Track Clip lists whose authored contents change; validation or compaction that
finds nothing to remove preserves every allocation identity. This is a
long-project locality invariant, not an optional History optimization.

`AuthoringList` and `AuthoringSet` serialize and deserialize exactly as JSON
arrays; `AuthoringMap` remains exactly a JSON object. The Set preserves
`BTreeSet`'s canonical order while making Project clones and typed History
snapshots share one immutable membership allocation. A real insert/remove
detaches it once; a no-op membership request returns before creating a detached
candidate. Allocation sharing, reference counts, allocation identities, and
copy-on-write state never enter `.mdp` author data, canonical fingerprints, or
execution snapshots. Timeline tests require byte-equivalent serialization,
structured round-trip equality, and clone isolation through the full Sequence
→ Track → Clip, audio-processing, and Project proxy-membership hierarchies.

Every shared allocation has a process-local identity used only by the
versioned `AuthoringFootprint` evidence contract. History can traverse several
typed immutable roots, charge a shared allocation once, and conservatively
include retained String/Vec/map/JSON/plugin-state capacity. Footprint
implementations fully destructure author structs and exhaustively match closed
enums so a newly added author field cannot silently escape the retained-memory
budget. This logical payload evidence is not allocator overhead or process RSS;
native memory remains a separate gate.

The same boundary continues inside automation payloads rather than stopping at
the Clip shell. `PropertyBag` shares its private ordered property map;
`AnimatedProperty` and its Channels share their private ordered collections;
exact audio-parameter keyframes and Mask shape keyframes use
`AuthoringList`. A candidate edit therefore detaches only the property, curve,
or shape collection it actually mutates. These containers preserve the
existing JSON object/array shapes and stable author identities; they are an
allocation strategy, not another automation model or mutable-state authority.

### Position Display

`SequenceSettings.timeline_display` is the sole persisted position-display
choice. It contains a `TimelineDisplayFormat` and a signed actual-frame offset
for the Sequence timecode origin. Resolving it with the Sequence video
evaluation rate produces one validated `TimelineDisplayContract`; Viewer and
Timeline ruler consume that same Interface and may not reconstruct counting or
origin arithmetic.

`Frames` displays signed Sequence-relative evaluation-frame offsets and ignores
the retained timecode origin. `Timecode` projects exact `TimelineTime` onto the
Sequence grid using an explicit rounding policy, then applies the origin and
formats SMPTE NDF or DF labels. Drop-frame is accepted only for exact
30000/1001 and 60000/1001 rates. Negative positions remain signed; conventional
SMPTE labels wrap after 24 hours while author time and frame display remain
exact and unbounded within their integer contract.

Display settings never change Clip placement, automation, nesting transforms,
or audio sample positions. Unsupported formats such as Feet+Frames are absent
from the persisted enum and product menu until their film gauge, footage
counting, origin, parsing, and reference fixtures are implemented as one real
contract.

## Author Identity, Revision, and Undo

`SequenceId` is stable identity; `SequenceRevision` is the persisted, nonzero,
monotonic author-transaction generation for that identity. A new or duplicated
Sequence starts at revision 1. Every committed author edit, Undo, and Redo
advances it without saturation. A Project color-environment replacement
advances the Project `AuthorGeneration`, not the persisted revision of every
unchanged Sequence. Resolved contexts and execution/cache keys carry the exact
Project engine identity, so engine replacement invalidates pixels without
pretending that Sequence author bytes changed.
`ProjectDocument.document_revision` is only the generation of successful file
saves and is never a Playback or render-cache revision.

Track, Clip, Effect, Mask, animation-track, keyframe, audio edit/scope/processor,
Route, Bus, Output, Transition, and Role identities remain stable entity IDs.
They deliberately do not each own a second revision counter. The owning
Sequence revision is the conservative invalidation contract; definition,
processor, resource, color, media-source, and prepared-plan fingerprints provide
the narrower cache keys where retaining unaffected work matters. This avoids a
family of counters whose atomic agreement would be harder to prove than the
author transaction itself.

Project validation rejects duplicate Sequence, Track, Clip, Effect, Mask, and
Video Transition identities, invalid strong Transition endpoints, singleton
Clip link groups, duplicate effect-local
Parameter identities, duplicate animation-track identities in one property
owner, and duplicate keyframe identities in one exact automation curve. Audio
Program validation owns the corresponding typed audio-entity uniqueness rules.
Persisted Track height must be finite and greater than zero, so structured
author equality and editor layout never admit NaN or a non-renderable lane.
`AudioProcessorInstanceId` is unique across all Scope, Track, Bus, and Output
Racks in one Sequence; a shared Scope is referenced once in author data and may
materialize several independent generated execution occurrences.
Copy and razor operations must fork the identities specified by the Sequence
audio ADR; Sequence duplication forks every Sequence-owned identity and resets
only the new Sequence revision.

`Sequence::validate_author_contract(ProjectColorEnvironment)` centralizes the
complete local body contract. Project open/edit wraps it with collection-wide
strong-reference and cycle checks; selected-range execution admission applies
it only to the captured root/nested closure before an immutable execution
attachment becomes worker authority. A renderer or exporter must not grow a
second Track/Clip schema validator.

`AuthoringSession` is the only production owner of a mutable
`ProjectDocument`. An ordinary edit clones only the target Sequence into a
detached candidate; it does not clone or install a replacement Project.
Structural operations that can add/remove Sequences, change Project-owned
settings, or require a valid navigation fallback operate on a detached
complete-Project candidate. That complete candidate remains the Project
transaction Interface; it is not retained wholesale merely because the
operation needs Undo. Active-Sequence navigation and its stack are
editor state: Project Undo/Redo preserves the currently viewed Sequence while
it still exists, and selects the restore point's valid fallback only when
the current Sequence was removed. The next `AuthorGeneration` and every affected
`SequenceRevision` are computed before any live authority changes.

One Sequence snapshot commit verifies identity, revision, and complete
canonical `before` content, then assigns a newer Session-owned revision.
`SequenceAuthorContractCertificate` is opaque and non-serialized. It strongly
retains `AuthoringSnapshot<Sequence>` plus the exact Project color environment;
replacement preparation rejects a stale baseline, same/older revision, changed
identity, or changed color context before reuse. The sole full
`Sequence::validate_author_contract` Implementation creates initial evidence.
Incremental preparation always reruns revision/settings/color, Track-domain,
complete identity/link, visual Transition, and Audio Program validation. It may
skip only unchanged local Track, Clip, Basic Title, Transform, Effect, and Mask
validators. Detached-but-equal deserialized storage is compared structurally,
so allocation identity is a locality hint rather than correctness authority.

Cross-Sequence validation uses one opaque, non-serialized
`SequenceDependencyCertificate`. It contains private derived nesting/output
facts and strongly retains the exact canonical `AuthoringList<Sequence>` COW
root plus default Sequence identity that produced them. This strong baseline
makes allocation-keyed per-Track fact reuse safe: while a certificate exists,
mutating a referenced Clip list must detach; any direct mutation elsewhere also
fails exact baseline comparison. Active navigation is not anchored, but every
reuse verifies that its target still exists. Each Sequence fact stores
deduplicated nesting edges, exact nested-output obligations
(parent/Clip/edit/child/output/optional explicit matrix source layout), public
output IDs/layouts, and derived per-Track facts keyed by domain and Clip-list
allocation. Public outputs and channel layout are recomputed for every
replacement. Preparing one replacement detaches only private fact/reverse-edge
map spines and affected parent sets.

Full collection validation and incremental replacement use the same facts
extractor and dependency-contract checker. A replacement extracts only changed
Track Clip-list bodies plus its current Sequence-level output contract. It
checks the candidate's outgoing references and output
bindings, checks existing direct parents whose nested-output contracts consume
the candidate, and rejects a new cycle by walking the existing reverse
ancestors of the replaced identity. It never visits an unrelated Clip body.
Exact author-baseline freshness fails closed before History preparation if the
certificate does not match the canonical Session state. The general stateless
`ProjectDocument::validate_sequence_replacement` Interface remains available;
it builds the same derived facts from its supplied collection rather than
becoming a second interpretation.

`ProjectAuthoringValidationCertificate::prepare_sequence_replacement` consumes
the candidate and constructs the next outer Sequence-list COW root exactly
once. The returned `PreparedProjectSequenceReplacement` retains the complete
next document and Project certificate; the dependency certificate clones that
same outer root. Only after this semantic ticket succeeds does History prepare
its typed endpoint and footprint descriptors from the ticket's read-only
replacement. History commit is followed by infallible ticket consumption and
installation. Thus validation failure cannot change History descriptor state,
and no validation, JSON conversion, allocation, or footprint discovery occurs
between History commit and installing the paired document/certificate.
Sequence snapshot commits compare the complete canonical `before` content as
well as its revision, so a caller cannot forge same-revision History that would
later Undo to a state that never existed.
Project candidate commits likewise treat every Sequence revision as Session
authority: changed existing Sequences receive the checked successor, new
Sequences receive the initial revision, and unchanged Sequences retain the
canonical prior revision even if a caller supplied a forged candidate value.
After revision and persistence-field normalization, a candidate with no
authored change returns no commit, advances no Generation, and creates no
History entry; an active-Sequence-only change belongs to the navigation
Interface. Real candidates validate the complete Project, rebuild the complete
Project authoring-validation certificate, prove that the declared affected Sequence set equals the
actual body/presence difference, and project before/after
`ProjectRestorePoint` endpoints before History admission; the Session still
installs the complete detached candidate.
UI and execution code receive read-only references or immutable snapshots.
There is no production API for “mutate first, record later”, so an edit error,
validation error, History preparation/accounting failure, or oversize-history
outcome cannot leak a partial canonical state.

The default History budget is 200 commands and 256 MiB across Undo and Redo.
Sequence commands retain typed before/after `AuthoringSnapshot<Sequence>`
endpoints; structural Project commands retain typed
`AuthoringSnapshot<ProjectRestorePoint>` endpoints. A Project restore point
contains Project identity and Project-owned authored fields, canonical Sequence
order/default and the structural active fallback, plus exact body-or-absence
state only for affected Sequence identities. It deliberately excludes
`document_revision` and `ProjectMeta.updated_at`; JSON serialization is neither
the endpoint representation nor the restoration path.

Every copy-on-write author allocation has a process-local immutable footprint
descriptor containing its local conservative logical charge and child edges.
One retained-allocation index reference-counts the descriptor graph reachable
from all typed endpoints in both stacks. Shared allocations are therefore
charged once, even when adjacent commands or both endpoint directions retain
them, and a descriptor leaves the cache when its final History reference
reaches zero. History metadata charges the complete inline
`AuthoringHistoryEntry`, one four-word Arc control/allocation allowance, one
logical Undo/Redo Arc slot, description bytes, and affected-Sequence-ID bytes.
The command is already inside the entry and is never charged a second time.
Tests independently cold-recompute this formula and require it to equal the
incremental retained index.

Record preparation uses an overlay over that live index. It adds the candidate
roots before removing branched Redo entries or evicting old entries, which
preserves sharing while calculating the exact post-operation union. It then
reserves every stack, reference-count, and descriptor-cache change and verifies
the complete budget without publishing any of them. Commit first rejects a
stale History revision and then applies the prepared state and footprint plan
once. Oldest entries use `VecDeque` constant-time removal; a new branch reports
discarded Redo entries. An individually oversize edit remains committed but is
explicitly reported as not retained. Because contiguous typed state-endpoint
History cannot safely jump across an unretained committed state, that outcome
also establishes a correctness barrier: all older Undo entries are discarded
and reported
separately rather than being allowed to overwrite the gap. Later retained
commands start a new contiguous Undo segment. This is one History
interpretation, not a delta-command system.

History entry compilation asks `mondrian-core` only for immutable descriptor
roots; it does not rebuild a redundant standalone manifest. A descriptor-cache
hit records the cached root edge without recursively merging that root's DAG.
The retained-allocation index remains the sole exact union and byte authority:
adding or removing an already active root changes only that root's reference
count, while child edges are visited only on a `0 -> 1` activation or `1 -> 0`
release. Repeated edges and shared diamonds therefore retain their exact
multiplicity without storing a flattened closure per endpoint. A cold
allocation subgraph must still be visited once to construct and validate its
descriptors; final release must still visit it once to retire the cache.
Undo/Redo only moves the same retained entry between stacks and does not
recompile descriptors or change the footprint index.

Undo and Redo are project-wide and remain valid while navigating between
Sequences, but preparation preserves the command scope. A Sequence entry is
cloned from its typed endpoint into one detached Sequence, receives a new
monotonic revision, and prepares the same validated replacement ticket without
moving either History stack. The restored Sequence, paired Project certificate,
revision high-water mark, and History cursor cross one commit
boundary; any preparation failure changes none of them. A Project entry first
checks that the current
Project-owned author state/order/default and every affected Sequence body or
absence still match the opposite source endpoint. It then materializes the
target as one complete detached document: current unaffected Sequence
allocations are reused in the captured order, affected bodies/presence and
Project-owned author fields are restored, and the current
`document_revision`, `ProjectMeta.updated_at`, and still-valid active navigation
are preserved. The structural active fallback is used only when the current
target no longer exists. The candidate then receives checked Sequence revisions
and passes complete-document validation. A Project-only restore whose ordered
Sequence bodies, default Sequence, and Project color environment are exactly
unchanged issues a new outer Project certificate while reusing their strongly
anchored validation evidence. Any change to those inputs rebuilds the complete
certificate through the same document rules. This preserves one validation
path without making unrelated Project metadata or defaults traverse every
Sequence body.
Only then is the prepared stack entry moved and the corresponding Sequence slot
or complete document installed.
Exhausted generations/revisions, corrupt snapshots, missing targets, and
validation errors therefore leave the document, Generation, stack cursor,
retained bytes, and History counters unchanged. Opening or replacing a Project
creates a new `AuthoringSessionId`, preventing an old persistence completion or
history entry from targeting the new lifetime.

Large-Project authoring acceptance uses the source-attested schema-10,
protocol-v1 5/30/120-minute matrix. Paired active-heavy and project-heavy
fixtures keep the active Sequence and workload equivalent while adding only
unrelated Project payload, so the gate measures target-Sequence locality. It
exercises interactive edits, structural edits, Sequence and Project Undo/Redo,
snapshot capture, manual save, and autosave through ordinary App and Session
Interfaces.

The deterministic History stream reports active Sequence bytes, unrelated
Project bytes, complete canonical Project JSON, deduplicated retained History
charge, and entry count. It is logical post-state evidence, not transient
allocation, allocator-exact, or process-memory evidence. Native process memory
is an independent stream: each enforced report takes three valid Private
Commit baselines before fixture construction, samples through construction,
operations, and the History-depth probe, then stops the sampler and takes one
final observation. Missing probes, incomplete lifecycle, inconsistent samples,
or absent final/peak evidence fail closed.

Only Private Commit participates in the process-memory gate. The observed peak
must satisfy the 4 GiB absolute ceiling and the active-heavy/project-heavy
delta budgets: 256/384 MiB for CI-light, 512/768 MiB at 5 minutes, 1/1.5 GiB
at 30 minutes, and 2/3 GiB at 120 minutes. Working Set and process-lifetime
resident peaks remain diagnostics and cannot weaken deterministic History
charge.

Reference budget v4 and Locality budget v3 are independent. For every measured
operation, project-heavy median must be at most
`2 * active-heavy median + 5,000 us`, and project-heavy nearest-rank p95 must
not exceed the reference-v4 p95 budget. Every measured mutation must change the
complete structured `ProjectDocument`, retain Undo, restore the normalized
pre-operation document after Undo, and restore the pre-Undo state after Redo.
Only monotonic `SequenceRevision` is normalized. The History-depth probe must
retain and Undo all 200 edits.

One qualifying invocation emits exactly six consecutively ordered
scale/profile reports plus one completion record under one run identity and
one content-addressed, unchanged source revision. Enforced runs require a fresh
durably completed output destination. Missing, duplicate, partial, mixed-source,
overrun, or source-mutated evidence cannot satisfy the matrix. A passing record
applies only to its attested source revision and is not copied into this
architecture document as a permanent implementation result.

`validate_with_color_environment` additionally validates a Sequence against the
Project's exact `ColorEngine`. Mondrian Standard Sequences use the exact
Linear Rec.2020 working identity pinned by the immutable package; Custom OCIO
Sequences use the exact working space pinned by their Project identity.
Application mutation boundaries call this validator before replacing a
Sequence snapshot. Project-environment replacement validates the complete
future-Sequence template and every existing Sequence as one transaction.
Editing-mode presets preserve that space: DCI raster dimensions do not imply a
P3 working-space change.
The persisted `SequenceSettings.color.working_color_space` is a
`WorkingColorSpace`, distinct from
external input and output `ColorSpace` values. Root color contexts carry a
display-referred output identity, while nested contexts carry their parent working
identity so render recursion cannot mistake an internal handoff for a delivery
boundary.
Sequence validation rejects scene-linear and scene-Log source identities as
presentation outputs even if a project file is authored outside the UI; the
sequence output must be one of the display-referred SDR/HDR identities. This
does not remove export's separate, explicit professional Log intermediate path.

The Project owns a complete `new_sequence_defaults` template. Project creation
copies it exactly into the first Sequence; later Sequence creation does the
same. Changing that template affects future Sequences only and never becomes a
runtime fallback for an existing Sequence. The default new Project uses
Mondrian Standard, Linear Rec.2020 and a scene-referred workflow. An explicit
DisplayReferred workflow remains available as a technical colorimetric bypass.
Project `ColorEngine` alone selects Mondrian Standard, ACES, or Custom OCIO;
the Sequence workflow does not duplicate that mode selection.

`SequenceSettings.delivery.bit_depth` and `video_range` are the Sequence-level
delivery defaults. They do not force every export of the
Sequence to use one representation: a typed `ExportPreset` may follow either
default or provide an explicit bit depth/range. Export admission resolves that
choice together with codec profile and chroma sampling before execution.
Twelve-bit delivery is currently reserved for ProRes 4444/4444 XQ. None of
these values describes the float working space or renderer-to-encoder pipe
precision; those are derived execution contracts and are not persisted as
editorial pixels.

`SequenceColorSettings` has no engine, override, or inheritance flag. Its three
deep responsibilities are `working_color_space`, `input` (missing-metadata and
per-media automatic tone-map policy), and `program_output` (workflow, target,
and output tone-map policy). `SequenceDeliveryDefaults` separately owns encoded
range/bit-depth defaults and authored HDR metadata. Machine-local Viewer display
policy belongs outside author data. Nested processing belongs to the
parent-to-child Clip placement edge, not either Sequence.

When `static_hdr_metadata_policy` is `WriteAuthored`, `SequenceSettings`
requires complete typed ST 2086 mastering-display and CTA-861.3 content-light
payloads. `Omit` is the default. The policy never means source passthrough;
these values describe the finished sequence delivery. Validation
rejects invalid rationals, impossible CIE xy coordinates, unordered mastering
luminance, and non-positive or unordered MaxCLL/MaxFALL values at the persisted
domain boundary. Output-View-specific content-peak checks remain an export
responsibility because they depend on the exact Project color engine.

`root_program_color_context(ProjectColorEnvironment)` combines the Project
engine with Sequence semantics and carries the one typed
`OutputTransformIntent` shared by preview Program pixels and scopes. Media
requests derive a narrower `MediaInputColorContext` using the individual render
plan's `auto_tone_map` value; Program Output tone mapping never enters a media
decode/cache key. Local monitor adaptation is resolved after Program Output and
does not create another Sequence context. Export either follows the Program
context or resolves an explicit `ExportColorTarget`; it never mutates the
Sequence to obtain a different deliverable.
An ordinary display-referred SDR context remains `Colorimetric`; a
scene-referred or explicitly tone-mapped Mondrian Standard boundary resolves to
the fully pinned `MondrianStandard { package }` product intent. ACES carries its
preset and Custom OCIO carries only a target-qualified `CustomOcio` intent; the
engine identity owns the corresponding display/view/output-endpoint binding.
No resolved display/view strings are stored beside the intent, so contradictory
context state is unrepresentable. A Custom target absent from the pinned output
binding set fails sequence validation before render planning. This selection is
independent from the renderer's CPU/GPU execution backend so a backend change
cannot silently change project color science.

Standard output resolution is target- and package-specific. Current-package SDR
sRGB/Rec.709/P3/Rec.2020 contexts select `Mondrian Standard SDR v2`; a project
that explicitly pins the legacy v2 package continues to select
`Mondrian Standard SDR v1`. Rec.2100 HLG and PQ contexts select
`Mondrian Standard HDR 1000 nits v1` under their respective OCIO displays.
Core resolves that target-specific view from the typed intent when renderer
constructs a Program Output boundary, so the target transfer function
changes only the display encoding and never selects a second HDR picture
formation.

The SceneReferred Standard Program Output selector exposes only the six targets
with exact versioned package Views: sRGB, Rec.709, Display P3, Rec.2020 SDR,
Rec.2100 HLG, and Rec.2100 PQ. Rec.601 PAL/NTSC remain supported source
interpretations and explicit DisplayReferred colorimetric outputs; they are not
misrepresented as Standard rendering Views. Domain validation resolves the
effective typed output intent against the selected package, so an externally
authored unsupported Standard target fails before render planning.

ACES uses the same typed-intent validation boundary. A sequence carries the
immutable ACES preset until its requested output target is resolved, rather
than materializing the preset's default Rec.709 View for every target. The
official Studio preset supports sRGB, Rec.709, Display P3, HLG, and PQ; the CG
preset omits HLG, and both omit Rec.2020 SDR. Unsupported combinations fail
sequence validation instead of producing a differently encoded signal with
contradictory delivery metadata.

## Track

`Track` owns an ordered `Vec<Clip>` and track-level state:

- `track_type`: Video, Audio, Subtitle
- `height`
- `is_muted`, `is_locked`, `is_visible`
- `blend_mode`
- animatable `track.opacity`

Video tracks use visibility and opacity. Audio tracks persist mute; solo is a
transient audition overlay outside the canonical author snapshot. UI must not
show speaker controls for video tracks or visibility controls for audio tracks
unless a future explicit domain feature is added.

Audio authoring keeps the Timeline Track as the editorial container and keys
its Track Mixer Channel state by the same `TrackId` inside the Sequence-owned
Audio Program. An audio Clip owns placement-local `AudioComponentEdit` values;
their Track and Sequence range are always derived from the owning Track/Clip.
They bind to Sequence-owned `AudioProcessingScope` values for Clip-level
processor continuity. The visual `Clip.effects` vector is not an audio rack.
Rack order and Processor Instance identity are author semantics and cannot be
collapsed into an aggregate gain or array-index address during compilation.
Track creation/removal updates the keyed mixer state and default route in the
same Sequence mutation. Validation rejects a snapshot when the two sets differ.
See [Audio Pipeline](audio-pipeline.md) for the author/compiler boundary.

## Clip

`Clip` is one Timeline placement, never an Asset or Sequence definition. It
owns one placement range, one closed `ClipSourceTimeMap`, a stable
`clip_time_in`, transform, visual effects, masks, optional link-group
membership, blend mode, and placement-local audio Component Edits. Its content
is one closed `ClipContent` payload:

Visual-effect insertion accepts a complete `EffectNode`, not only an
`EffectType`. The effects domain owns registered definitions and canonical
parameter defaults, while Timeline owns placement, ordered instance storage,
instance namespacing, and identity. Product commands therefore construct an
instance with the currently bound definition before calling
`Clip::add_effect_node`/`insert_effect_node_at`. Timeline neither depends on the
execution registry nor creates an empty parameter bag that compilation would
later guess how to repair.

- `Media { asset_id, interpretation }`
- `AdjustmentLayer { asset_id }`
- `NestedSequence { sequence_id }`
- `SolidColor { asset_id, color }`
- `BasicTitle { title }`

The variant is the single source of truth. There is no parallel `kind`,
`asset_id`, nested ID, interpretation, or solid-color field that can describe a
contradictory Clip. Current-schema deserialization rejects legacy or unknown
parallel fields. Constructors create only the identity required by the chosen
variant; for example, a nested placement does not manufacture a fake Asset ID.
Callers must also choose the identity they mean: `library_asset_id()` addresses
Media, Adjustment Layer, and Solid Color records in the Asset Library, while
`media_asset_id()` returns only file-backed Media dependencies. There is no
generic `asset_id()` because treating a generated library identity as a file
dependency made valid Solid Color/Adjustment exports fail as offline media.

Every value returned by `library_asset_id()` is a strong Project-contained
Asset record reference. Removing an item from the Library panel retires only
its visible membership and cannot remove this Clip, an inactive/nested
Sequence placement, Transition relation, audio edit, or a History endpoint.
The retained row continues to provide its exact expected contract and
recoverable binding. A missing row is structural invalidity and must never be
manufactured into an offline-media placeholder.

Every Clip-owned visual processor uses one exact Clip-local author coordinate:
Transform, Opacity, visual Effects, Masks, and generated visual content.
Sequence evaluation maps
`clip_time = clip_time_in + (sequence_time - position)`. Moving the placement,
slipping the source, or changing the source time map preserves
`clip_time_in`; trimming the in edge, splitting, or creating a right-hand
overwrite fragment advances it by the removed placement duration. Source time
is separately derived through `ClipSourceTimeMap` and controls only
media/nested sampling. The current closed `Constant` variant stores exactly a
`source_origin` and an exact `TimeScale`: positive, negative, and zero scales
mean forward sampling, reverse sampling, and a hold. The source coordinate at
the exclusive placement end is always derived as `map(duration)`; no mutable
`source_out` or parallel speed field is persisted. For reverse sampling,
`source_origin` is the first sampled coordinate rather than the minimum of an
interval, and the derived terminal boundary may be earlier. This prevents
placement, source selection, and visual processing from becoming accidental
competing authorities.

Future variable time remapping must add a validated closed
`ClipSourceTimeMap` variant with exact segment ordering, continuity,
source-boundary, and inverse/ambiguity semantics. Ordinary parameter automation
cannot stand in for this domain transform, and compatibility fields cannot be
added beside it. Author transaction validation checks placement, Clip-local,
and complete source-map arithmetic before a snapshot may commit.

The constant-retime operation accepts an explicit complete Clip target set and
changes only the canonical source-time map. A positive forward-rate intent
preserves source origin, placement, Timeline duration, `clip_time_in`, and
audio edit origins. A picture-hold intent first resolves the source coordinate
visible at one in-range Sequence time, then installs a zero-rate map. All
targets and Track locks are validated before any Clip changes. The App product
adapter may expand the complete link group for forward rate; freeze frame is
deliberately video-only because repeating one audio sample is not a valid audio
freeze. New retime intent additionally fails closed unless the referenced media
stream or nested Sequence has a finite, currently resolvable source extent.
Existing unresolved author intent may survive reopen, but a new source-dependent
edit cannot be admitted on guessed duration.

Signed reverse is not implemented by merely negating the scale. A reversed
half-open source interval begins at the old exclusive terminal boundary and
must request the strict predecessor sample at the media/audio lowering seam.
Until that direction-aware boundary contract is carried through Preview,
Audio, and Export, public forward-rate Actions reject zero/negative ratios and
the reverse product feature remains incomplete.

A file-backed still is not another `ClipContent` variant. The Asset Library
classifies the physical source as `StillImage`, while `Clip::new_still_image`
creates ordinary `Media` content with one explicit zero-rate
`ClipSourceTimeMap`; its derived terminal boundary equals `source_origin`. Its
Timeline duration is therefore independent of
source duration, out-trim may extend it, and every evaluation samples the same
source instant. Moving, splitting, nesting, effects, Alpha, Preview, and Export
continue through the normal Media path. A probe that proves multiple picture
frames remains `Video`; an unknown frame count also fails closed as `Video`.
Classification never infers single-frame semantics from the filename alone.

`BasicTitle` is a fifth content type, not a synthetic Asset and not an Effect.
Its closed definition-backed Property Bag owns text, exact requested font
family/weight/style, font size, working-linear fill, tracking, line height, and
horizontal/vertical alignment. Font size, fill, tracking, and line height use
the ordinary exact automation model in Clip-local visual time; discrete text,
font, and alignment values remain static until the product defines an explicit
discrete-edit workflow. Unknown, missing, removed, type-divergent, or
out-of-contract properties are rejected before an author snapshot commits.

The Sequence `title_safe_margin` is a total width/height fraction: `0.20`
means 10% per edge. Both action/title safe margins must be finite and in
`[0, 1)`, so persisted settings can never yield NaN layout or consume the
complete canvas. Title creation prefers the selected free unlocked video Track,
then the topmost free unlocked Track; only when none is available does it add a
Track. Adding that Track and placing the title is one author transaction and one
Undo step.

Copy, paste, razor, overwrite-created fragments, and Sequence duplication fork
every addressable placement identity: Clip, Effect, Mask, visual animation
tracks/keyframes, and the corresponding audio aggregate identities. External
references such as Asset ID or nested Sequence ID remain references. Sequence
duplication additionally remaps Track-keyed routes, Transition endpoints, and
link groups, producing a disjoint author graph rather than two aggregates that
share mutable instance identity.

Precompose is a structural Project transaction because it replaces one parent
Sequence and creates one nested Sequence atomically. Its candidate construction
must nevertheless preserve copy-on-write locality: only Tracks containing a
selected placement may detach their Clip lists. Post-edit compaction first
derives singleton link groups, invalid visual/audio Transitions, and unreferenced
audio processing scopes through shared reads; it enters mutable COW storage only
when that exact collection contains something to remove. A no-op compaction
therefore preserves every Track, Clip, Transition, and Scope allocation identity
instead of making History retain a second copy of an unaffected long program.

### Link Groups

`Clip.link_group: Option<ClipLinkGroupId>` is set membership, not a pair pointer.
A group may contain two or more video/audio placements and supports imported
multi-component media without inventing chains of pair links. Selection and
structural commands expand from any member to the complete group, validate all
source and destination Track locks before mutation, preserve member-relative
time, and reject a move that would put any member before Sequence zero. Razor
forks one group for the right-hand pieces. If overwrite trimming/splitting
breaks the synchronization promise, affected fragments leave the group rather
than retaining a misleading relationship. Singleton groups are compacted after
structural edits and rejected at the persisted boundary.

Explicit Link/Unlink uses the `clip_linking` Module and stable Clip IDs. Its
Interface assesses the group-expanded request without mutation and applies it
to a cloned Sequence before replacing live author state. Linking an existing
group with unlinked Clips retains that group's identity; merging two or more
existing groups creates a fresh identity so no input group arbitrarily becomes
the survivor. Unlinking any member clears its complete group. A locked Track,
stale Clip ID, insufficient new membership, or final author-validation failure
rejects the entire operation. A request that would not change membership is
reported as a no-op and the App must not create an Author Transaction for it.

### Insert Edit

Professional Insert is a single `InsertEditRequest`, not a collision mode.
The request contains one exact Sequence-time boundary and duration, fully
authored target placements, an explicit set of ripple Tracks, and explicit
policies for Sequence-time automation, intersected Transitions, and
playhead/In/Out coordinates. Track Targeting and Sync-Lock controls are editor
state that compile into this request. They are not persisted on `Track`, do not
affect rendering, and cannot become hidden inputs to Headless execution.

`apply_insert_edit` clones the Sequence, preflights the complete request, and
publishes the candidate only after author-identity and Audio Program validation.
Every target must be in the ripple set and every participating Track must be
present and unlocked. The operation splits each crossing Clip at the exact
boundary, advances the right fragment's Clip/audio local origins, moves whole
downstream Clips by the exact inserted duration, and then places the requested
content inside the opened interval. A split preserves only the original outer
audio fades: the left fragment keeps fade-in, the right keeps fade-out, and no
new fade is invented at the cut.

An existing link group may be transformed only when every member participates
and has the same temporal class (before, crossing, or downstream). Crossing
members receive one fresh right-hand group; a partial Track scope or unequal
classification fails the whole edit. New linked placements must form a complete
new group and cannot alias an existing group. This conservative rule supports
ordinary linked picture/audio and refuses J/L-edge cases whose synchronization
meaning has not been explicitly resolved by the editor.

Transitions wholly before the boundary remain fixed. A Transition whose
endpoints are wholly downstream on participating Tracks moves by the same exact
duration. Any Transition touching the insertion cut or a split endpoint cannot
retain its geometry and therefore either rejects the operation or is removed
only under `RemoveAffected`. Video Transition properties remain Transition-local
when their owner moves; Audio Transition intervals are Sequence-local and move
with their endpoints.

Sequence-time automation has two explicit modes. `PreserveSequenceTime` leaves
keys at absolute time. `FollowEditorialContent` shifts video Track opacity,
audio Track channel-strip/rack automation, and Routes sourced from a shifted
Track. A Bus follows only when every authored input follows; a Program Output
does the same, while a semantic projection follows only when every audio Track
participates. This ownership closure prevents a mixed Bus from arbitrarily
choosing one input's timeline. Clip Component and Processing Scope automation
is already local and moves through Clip placement/local-origin mapping rather
than having its keys rewritten. Navigation/range coordinates independently use
`PreserveSequenceTime` or `FollowEdit`.

The application Asset Adapter converts frame-grid UI fields to exact
`TimelineTime` once, creates picture/audio placements and processing scopes, and
invokes this operation inside one `AuthoringSession` transaction. A linked
picture/audio insertion therefore advances Author Generation and Sequence
Revision exactly once and is one Undo step. The old local overlap option is
named `ClipOverlapMode::PushForward`; it is deliberately not presented as
Insert Edit.

### Lift and Extract Range Edit

Lift and Extract share one UI-independent `RangeEditRequest`. Its Interface
contains a positive half-open Sequence-time range, an explicit
`content_tracks` set, an explicit `ripple_tracks` set, and independent
automation, Transition, and navigation policies. It contains no selection,
panel, or workspace state. Lift removes only content intersecting the range
and requires an empty ripple set. Extract removes the same content and closes
the exact duration on every ripple Track; every content Track must therefore
also be in the ripple set.

Track Targeting and Sync-Lock remain editor-session policy. The App Adapter
resolves Target-enabled Tracks into `content_tracks`, and resolves the union of
Target-enabled and Sync-Locked Tracks into the Extract ripple set. A
Sync-Locked but untargeted Track may move whole placements at or after the
range end when its range is empty. If any of its Clips intersects the removed
range, Extract fails with `ProtectedRippleContent` instead of silently cutting
untargeted content, leaving a hole, or expanding Targeting. Lift ignores
Sync-Lock because it does not change program time. Missing Tracks, empty
content scope, locked participants, or contradictory scopes fail before the
live Sequence changes.

The `range_edit` Module classifies every Clip relative to the same exact
half-open interval. Content wholly inside is removed; edge intersections trim;
a spanning placement is split into two survivors. The left survivor retains
the original Clip and link identities. The right survivor receives fresh
placement, audio-edit, visual-property, effect, mask, and keyframe identities,
advances source/Clip/audio local origins by the removed in-side duration, and
keeps only the original outer fade. Extract then shifts the right survivor and
all wholly downstream placements by the exact range duration; Lift leaves
their Sequence positions unchanged. The shared private `clip_fragment` Module
owns these placement/local-time rules for Insert, Lift, and Extract so the
three structural operations cannot develop different split semantics.

Link groups use the same conservative coherence rule as Insert: all members
must receive one temporal transform class. A partial scope, protected
Sync-Lock member, or unequal before/inside/edge/downstream outcome rejects the
whole edit. When every member spans the range, corresponding right fragments
share one fresh group identity. This preserves ordinary linked picture/audio
and deliberately fails closed for offset J/L-edge cases whose post-edit link
meaning is not yet explicit.

Transitions wholly before the range remain fixed. Transitions wholly after an
Extract move by the exact duration only when both endpoints participate in
ripple; wholly downstream Lift Transitions stay fixed. A Transition whose
endpoint or interval is cut is either rejected or explicitly removed according
to request policy. The same rule applies to Sequence-local Audio Transitions;
final strong-reference validation prevents a removed Clip or Component Edit
from leaving a dangling relationship.

`PreserveSequenceTime` leaves Sequence-owned automation untouched.
`FollowEditorialContent` removes keys in the half-open interval and shifts keys
at or after its end on every rippled Track. The shared
`sequence_time_edit` Module then applies the same complete-input ownership
closure used by Insert to Track channels, Routes, Buses, and Program Output.
Clip/Component/Scope-local curves are not rewritten because their owner-local
origin changes with the surviving placement. Automation edits preserve stable
key identities and validate the complete candidate before publication.

The App executes either operation through one Sequence-scoped Author
Transaction, prunes stale selection only after commit, seeks to the collapsed
range start, and reports one Undo step. Availability uses a read-only structural
assessment over Track scopes, locks, protected intersections, link groups, and
the same Transition disposition function used by execution; it does not clone
and trial-edit the complete Sequence during each UI model rebuild. Execution
then performs the full candidate transform and final automation, audio,
identity, and strong-reference validation before publication. Window
enablement therefore catches deterministic structural blockers without putting
an O(complete author snapshot) clone on the interaction path, while direct and
Headless callers still receive exact execution failures.

### Video Transitions

A visual Transition is a Sequence-owned, typed two-input author entity. It has
a stable `VideoTransitionId`, two strong Clip endpoints, one exact Sequence-time
range, a built-in or plugin definition identity, a complete Property Bag, and
explicit enabled state. It deliberately does not persist `track_id`: endpoint
Track membership is derived from the two Clips, preventing two authorities from
disagreeing.

Validation requires distinct endpoints on the same video Track, exact adjacent
edit geometry at one shared cut, a non-empty range that covers that cut and is
contained by the two placement ranges, a unique endpoint pair, valid properties,
non-adjustment endpoints, and non-overlapping Transition ranges on one Track.
The last rule removes otherwise undefined simultaneous three-input evaluation
around a short middle Clip. Structural edits either preserve those facts or
remove the now-invalid Transition before commit; invalid persisted graphs are
rejected instead of repaired during open.

`VideoTransition::source_demand` maps the complete Transition interval into
both source domains without clamping. A media/nested Adapter must supply probed
source extents to `validate_source_extents`; insufficient handles fail closed.
The author model therefore never substitutes repeated boundary frames or reads
outside a source. This is the frozen author/adapter contract. The current
App authoring boundary resolves the primary video stream's declared duration,
single-frame still semantics, or child-Sequence duration before create/range
edits. Container duration is never substituted for a missing video-stream
extent because a longer audio stream could admit nonexistent video handles. Insufficient
handles reject by default. `ShortenToAvailable` is an explicit product command,
intersects both exact source extents while retaining the edit, and commits the
shortened range in the same single Undo transaction.

Execution admission represents this authority as
`PictureSourceExtent::{Still, TimelineRange}`. A probe-proven Still is one
atemporal picture and satisfies arbitrary outgoing or incoming Transition
handle demand without a fabricated duration. A time-varying media source uses
the selected video stream's exact positive range; container duration is never a
fallback. A nested Sequence is always a timed zero-based range derived from the
same immutable child revision, even when its output pixels happen to be static.

`validate_selected_video_transition_source_handles` is the pure common
validator for Export and other immutable execution captures. It consumes one
owner Sequence, only the renderer-selected `VideoTransitionId` set, and a
`PictureSourceRef` resolver over already-captured extent facts. It revalidates
strong endpoints and exact unclamped demands, rejects missing/disabled selected
Transitions, missing or contradictory extents, and insufficient handles, and
never reads App state, the Asset Library, Project, filesystem, or FFmpeg.
File-backed Export storage must bind the extent to the same source fingerprint
and physical video-stream index as the decoder dependency before calling this
Interface; nested storage must resolve from the captured child Sequence rather
than a live Project lookup.

`PreparedVisualSchedule` compiles each exact `SequenceId + SequenceRevision`
into immutable per-Track interval indexes plus one global visible-Track activity
index. A Track's activity is the sorted, merged union of its enabled Clip and
enabled Transition half-open intervals; adjacent intervals merge, while a real
gap remains a gap. Hidden and muted Tracks contribute no activity. Each frame
first queries that global index, sorts and deduplicates the selected authored
Track indices, and only then evaluates Track opacity and the selected Tracks'
Clip/Transition indexes. Thus a thousand empty Tracks do not become a
thousand-Track frame cost, while authored stack order remains exact. Query
diagnostics report the number of Tracks actually queried and the global
activity entries inspected rather than presenting the authored Track total as
execution work. Preview and Export production Sessions own bounded Visual
Program caches whose programs query this representation at exact Timeline Time;
they do not scan every Track or Clip placement for each frame. Preparation
validates every Clip's complete placement, Clip-local, and canonical source-time
mapping state, including its derived terminal source boundary, together with all
visual Transition references and intervals before publication. This prevents a
deserialized or otherwise invalid source mapping from passing admission and
failing only in a Preview or Export worker. A failed preparation cannot enter
the cache, and a revision change cannot reuse or retain an older schedule for
the same Sequence identity.

The schedule owns placement selection only, but it snapshots enabled Clip
Effect and Mask author payloads behind shared immutable `Arc` slices. Repeated
queries therefore sample dynamic properties without cloning every Property Bag.
It applies the same rule to visual Transition definition state: definition
identity, Transition-local Property Bag, and opaque parameters form one
immutable shared snapshot, while progress and endpoint source/Clip time remain
exact per-query values. Preparation retains only the Transition identity,
range, endpoint indices, and that snapshot rather than a second mutable author
entity.
`mondrian-renderer::PreparedVisualProgram` composes that schedule with
resource-bound per-Clip Effect programs for the exact Effect-definition
registry revision plus prepared Transition readiness/blockers. This
higher-level compiler is intentionally allowed to read one immutable Sequence
revision; repeated production frame lowering receives only the validated
Prepared Visual Program. The renderer keeps raw-Sequence projection
crate-private as a scalar parity Implementation, not a callable product
Interface.
Range-level dependency queries use the same schedule's interval indexes. They
return visible ordinary Clip and Transition-endpoint occurrences with exact
Clip-local bounds, excluding hidden/muted Tracks and disabled placements
without constructing another timeline walker. Renderer preparation combines
those bounds with each Clip's temporal Effect contract before projecting nested
child windows. Export diagnostics and delivery preflight must consume this
prepared reachability; they must not reinterpret Track/Clip placement or
enumerate every frame in a long selected range.

`mondrian-renderer::PreparedVisualRangeClosure` is the sole recursive
composition of those per-Sequence range queries. It pins one exact Program set
and Effect registry revision, validates each Program's versioned conservative
visual-author fingerprint, and owns nested range projection, cycle/depth
admission, selected media, selected Transition identities, and static Basic
Title font queries. Export capture consumes this immutable closure directly.
Preview's cold-start lookahead asks the same Module whether an inclusive frame
prefix can demand file-backed picture and binary-searches the earliest such
frame; it does not scan author Tracks/Clips or special-case nested blank
leaders and Transition endpoints.

Temporal Effect input does not create another placement or source-time model.
Every lowered Clip contribution retains a `TimelineClipExecutionRef` containing
the exact Sequence identity and revision, Clip identity, signed Clip-local
sample time, and ordinary or exact Transition-side endpoint context. A temporal
request is expressed in that Clip visual Authoring Time Domain. The production
provider asks the same Prepared Visual Program to sample that execution
reference at the requested signed Clip time, and the schedule applies the
Clip's canonical time transform exactly once to obtain media source time or a
child Sequence request. A stale revision, unknown Clip, or mismatched
Transition endpoint fails closed. The provider never reads a legacy
range/speed field, holds the current decoded frame as history, or scans
`Track`/`Clip` author collections behind `RenderPlanSource`.

The effects Module prepares one exact finite time-expanded execution from the
same `PreparedEffectProgram` and `CompiledEffectGraph` that are later executed.
The schedule maps every requested Clip time back to its owning Sequence time
for exact parameter/frame-seed evaluation, separately from the canonical
Clip-to-source mapping used by media and nested Adapters. An on-grid sample
uses the ordinary Sequence frame number; an off-grid sample uses a versioned
hash of exact Sequence time and Evaluation Grid. Source retime therefore never
changes stochastic Effect identity.

`PreparedEffectTemporalExecution` freezes root graph, exact sampled graphs,
cross-time value addresses, and raw source demands as one authority. A
`PreparedTemporalFrameSet`
freezes a complete, generation-bound set of source coverage before scalar
execution and reports its exact Float32 byte total before callers materialize
it. Missing, duplicate, unexpected, stale-generation, wrongly sized, or
ambiguous coverage fails before the graph can observe a partial provider. The
admitted production graph shape is a finite Definition-stage-addressed value
projection. Negative and positive offsets become exact history/lookahead
demands; an effected upstream value reevaluates all earlier Definition stages
at that exact Clip time, including animated parameters, dynamic topology,
frame seeds, and Masks. Duplicate exact source times and graph values are
frozen once and retained to their last edge use. Stage contracts must remain
stable, cross-time edges must move to an earlier stage, and every discovered
source time/ROI must fit the declared aggregate contract. Its Effect Session executes the request
directly when the proved live set fits, or deterministically tiles and stitches
the complete output under the same source/output/tile grant. Current-time Clip
Masks execute in that graph through one frame-extent-bound prepared raster;
partial regions retain global Mask coordinates and direct/tiled results are
bit-identical. Adjustment-stack temporal input, internal same-stage temporal
branches, unbounded temporal input, stateful continuity, or unsupported
color/ROI semantics are rejected rather than rendered approximately.

Export connects this first tracer to job-local synchronous media and nested
resolution, reusing the ordinary input color/alpha preparation and nested
Program Color Context. Its provider identity conservatively includes placement,
source transform, graph, dependency, geometry, and color facts. Window and
Headless Preview use the same Timeline tracer and the same production media
Adapter: every signed source demand is submitted through the asynchronous
Preview Scheduler with CPU-working representation in its key, and only
generation-matching values already present in the Preview Frame Store may enter
the frozen set. Any missing value yields typed Temporal Pending after the
complete demand list has been visited. Preview never performs blocking decode
or substitutes its currently displayed frame as history.

`RenderPlanSource::flat_visual_items_at` projects ordinary Clips and explicit
two-input Transitions from either the crate-private direct Sequence reference
Adapter or its Prepared Visual Schedule as ordered visual items. Renderer
scalar parity tests require both Implementations to remain semantically
identical at every half-open interval edge. During the
Transition interval, one Transition replaces both endpoint placements at that
Track stack position.
Both endpoint source times remain unclamped, so Preview/Export either obtain
the requested handles or fail; neither repeats a boundary frame. Cross Dissolve
is executed by the shared Preview/Export CPU working compositor. Product
authoring uses one App-owned command seam: the default is an approximately
one-second range centered on the exact cut and snapped to the Sequence video
grid, clamped only to the endpoint placement union. Real source-handle
admission remains fail-closed and never silently selects the explicit
shortening policy. A selected Transition stores only its stable identity;
Track membership continues to derive from the strong Clip endpoints. Timeline
overlays now create Cross Dissolve at an exact adjacent unlocked video cut,
select/delete by stable identity, and resize either exact range edge through one
typed App action. The view Adapter reports current insufficient or unresolved
source handles without modifying author state. Cross Dissolve now also lowers
to a typed working-linear GPU pass whose real readback is checked against the
shared straight-alpha scalar reference. CPU remains the scalar reference and a
valid fallback; backend choice cannot change Transition semantics. This closes
the current Cross Dissolve backend split only and does not imply generic GPU
support for every future Transition kind.

`RenderPlanSource` also projects the Sequence working color space as a required
typed value. Effect compilation consumes that value for every ordinary Clip and
Transition endpoint; it is not recovered from the Viewer, export preset,
Project default, or source-media metadata. This keeps the ownership boundary
single: the Project owns the color engine/configuration, the Sequence owns its
working space, and a Clip-local effect owns only its parameters. Any
working-space-dependent operation includes the projected identity in its
compiled graph/cache signature, so changing Sequence settings cannot reuse
pixels evaluated under the previous coefficients or processing domain.

Transform, blend mode, solid color, masks, and effects are currently exposed
through `PropertyHost`/`PropertyBag`; the source-time map is a separate exact
domain transform rather than parameter automation. Every product definition carries
a versioned `ParameterSchema` with an address-independent `ParameterId`, value
type, definition default, automation capability, typed unit,
numeric/enum/resource, Hold/Linear/Bezier execution semantics, message, and
cache-impact contracts. Effect execution resolves the stable ID exactly; the
instance-qualified string path remains only an authoring/UI address alias and
cannot define execution identity. `PropertyBag::validate` rejects malformed
persisted descriptors, values, channel layouts, keyframe order, enum indices,
and interpolation before the project enters execution. Visual and audio
automation share exact curve primitives, stable Parameter IDs, and the same
Parameter Schema language. Audio Processor instances capture a schema snapshot
beside the exact curve so missing plugins remain editable; a known built-in must
still match its canonical definition schema exactly before compilation. Numeric
audio schemas may use exact integer sample-frame units or sample-rate-independent
milliseconds; values must remain exactly representable in the shared curve
domain. The built-in Lookahead Limiter therefore persists Lookahead in
milliseconds but marks it non-animatable and topology-affecting; audio plan
preparation rounds it upward once to the concrete sample grid. A parameter that
changes storage, latency, or continuity topology forces plan re-preparation
rather than a live callback event.

Automation authoring uses `AnimationParameterAddress { animation_track_id,
parameter_id }` as the stable property-instance address and `KeyframeId` as the
stable control-point identity. A `PropertyBag` resolves that pair to its current
path only at the mutation boundary. Selection, clipboard, and UI commands never
persist point indices, key times, display labels, or paths as identity; repeated
effects with the same `ParameterId` therefore remain distinct, while a
definition-compatible path alias change does not invalidate selection.

Moving a key is the atomic `EditKeyframe` mutation. It validates the complete
multi-channel key, target-time collision, value schema, and final curve before
publication, while preserving the key ID, interpolation handles, and temporal
flags. Stale or ambiguous identities and collisions reject the detached
candidate without advancing Author Generation, Sequence Author Revision,
dirty state, or Undo history. Insert, move/value edit, and remove are
incremental intents; no editor may implement one gesture by replacing the
whole curve or by exposing a remove-then-insert intermediate.

`ExactAutomationCurve::prepared_segments` validates author order, finite values,
and Bezier time monotonicity once, then returns immutable interpolation segments
whose evaluator reuses the same Hold/Linear/Bezier mathematics as direct curve
evaluation. Execution Modules may lower those segments onto their Evaluation
Grid and advance span cursors; they may not copy the interpolation formulas or
reinterpret the persisted author curve.

Mask scalar properties are part of the persisted Property Bag, not runtime
defaults. Project validation requires the canonical feather, opacity,
expansion, invert, and operation Parameter IDs, valid curves, finite geometry,
and strictly ordered shape keys. Saving and reopening must therefore preserve
the exact evaluated mask rather than silently reconstructing default values.

Every structural Timeline command ends by compacting the three dependent
author graphs as one invariant-restoration step: Clip link groups, visual
Transition strong references, and the Sequence audio program. The resulting
candidate is then fully validated and atomically committed by
`AuthoringSession`.

## Render Projection

Timeline internals are projected into `FlatVisualItem` through
`RenderPlanSource`. The renderer's frame-lowering Implementation consumes that
Interface and never traverses `Sequence`, `Track`, or `Clip`. Direct Sequence
projection is a crate-private scalar semantic reference. A separate renderer-owned
compiler accepts one immutable Sequence revision and publishes a
`PreparedVisualProgram` containing the Prepared Visual Schedule plus per-Clip
prepared Effect programs. The renderer then builds a transient
`PreparedVisualFrameClosure`, which is the sole owner of recursive visual
semantics: nested source-time mapping, cycle/depth admission, child canvas and
color context, Transition and temporal bindings, and placement-instance paths.
It resolves and fingerprint-validates one exact `Arc<PreparedVisualProgram>`
per Sequence before any frame callback; that callback receives no raw Sequence.
Preview and Export consume that closure. Their Modules retain media adaptation,
pixel materialization/compositing, and consumer-specific scheduling, but may
only resolve the closure's typed child bindings; they cannot own another nested
Timeline walker or reinterpret recursion.
