# UI System

The Export panel edits the same typed `ExportPreset` consumed by queue
admission. Artifact-family-specific controls are projected from
`ExportArtifactEncoding`: media-file presets expose container/video/audio and
coding controls, image-sequence presets present a picture directory, and the
PCM24 audio-stem preset presents a directory containing every public Audio
Program Output. Directory artifacts cannot manufacture media-file fields.
Built-in preset selection replaces the complete editable draft and rewrites
only an output suffix that still follows the previous built-in artifact.
The professional codec menu is generated from the Export-owned mezzanine
catalog. Selecting DNxHR, AVC-Intra, or uncompressed essence atomically applies
its compatible depth/chroma/range, Intra-only structure, preferred container,
and any fixed raster/cadence; AVC-Intra MXF also disables audio because that
Adapter cannot preserve a validator-visible PCM layout. The panel never exposes
XAVC or Class 50 as a near-match and queue admission revalidates the complete
preset after every edit.

Professional IMF/AS-11/DCP rows are projected from the same closed Export
preset catalog. Their raster, cadence, signal, essence, and audio controls are
read-only because changing one would leave the qualified row; title, issuer,
creator, and RFC 5646 language remain ordinary editable preset metadata. The
panel distinguishes immutable directory packages from one-file MXF output,
uses `.imf`, `.dcp`, and `.mxf` suffixes, and exposes the explicit Packaging
queue phase. Enqueue availability resolves the complete preset against the
selected Sequence, while the queue repeats authoritative admission and
profile-specific external tool qualification.

Sequence Settings exposes only Progressive and the qualified Upper First
interlaced Program Output. Timeline validation owns the 1080i25/29.97 raster and
cadence matrix, so widgets cannot manufacture UHD, BFF, or arbitrary interlaced
rows. Export availability resolves the complete preset and shows the same
MOV/Rec.709 Legal/10-bit 4:2:2/ProRes-or-v210 restriction enforced again by the
queue. Preview media resolution disables proxy/native-surface optimization for
interlaced or unknown scan and binds field processing into the decode key; UI
state owns no second scan interpretation.

Mondrian's UI is self-hosted: winit/platform integration, retained widgets, wgpu rendering, theme tokens, event routing, dock/layout, and app panel adapters.

Timeline interchange composition remains outside panel code. The App inspects
native bytes through `mondrian-interchange`, presents its media requirements and
loss report, verifies user-selected bindings against the canonical Asset
Library, and commits one detached Sequence through one reversible Project
transaction. Export preparation reads the active canonical Sequence plus
immutable Asset records and returns bytes coupled to the report; file dialogs
and durable publication are separate platform/storage Adapter work. An import
does not silently change the active open-Session Sequence.

Color-space selectors expose Rec.601 PAL and Rec.601 NTSC as distinct encoded
identities. They are not native window surface color spaces; viewer
presentation still passes through the configured display/view transform before
targeting an sRGB, Display P3, PQ, or HLG surface.

Interpret Footage projects Camera RAW controls only when the media probe carries
a supported typed RAW contract. Its draft owns the complete
`AssetMediaInterpretation`; Exposure, As Shot/manual white balance, and Debayer
menus update that draft, and Confirm dispatches one existing Asset
`SetInterpretation` transaction. Color-space and encoded-range selectors are
disabled for RAW because the Adapter's output is explicitly scene-linear
Rec.709 full-range Float32. Non-RAW assets omit the RAW controls from the
retained Widget tree. The import picker includes DNG, but extension spelling
alone never grants RAW execution authority.

## Crate Split

- `mondrian-ui-core`: widget trait, event types, accessibility, focus/shortcut/tooltip traits, tree traversal.
- `mondrian-ui-theme`: semantic tokens and theme preference.
- `mondrian-ui-layout`: reusable layout algorithms.
- `mondrian-ui-renderer`: draw-command renderer over wgpu.
- `mondrian-ui-text`: text layout/raster support.
- `mondrian-ui-events`: event routing, focus/capture/shortcut/IME/DnD side effects.
- `mondrian-ui-tooltip`: tooltip manager/widget.
- `mondrian-ui-widgets`: controls and editor-specific reusable surfaces.
- `mondrian-app::app_ui`: product shell and panel adapters.

Outside `app_ui`, `mondrian-app::preview_render_cache_identity` is the narrow
source Adapter for persistent resolved-visual identity. It exhaustively
canonicalizes materialized Preview layers and media source semantics, normalizes
equivalent CPU/native decode providers, and supplies only the resulting typed
leaf identity to Renderer. It does not own Prepared Visual traversal, artifact
storage, Viewer presentation identity, or Program Output/monitor semantics.

`app_ui::panels` is the aggregate projection and dock-composition Module, not
the owner of every panel's control Implementation. Property-oriented Mixer and
Inspector construction lives behind the internal
`panels::property_panels` Seam: callers provide the already projected
`AudioMixerPanelModel` or `InspectorPanelModel` and receive one retained
`PropertyPanel`; numeric controls, Rack/automation rows, Mask/Effect controls,
and typed Action lowering stay private to that Module. This keeps the external
panel Interface unchanged while concentrating the most coupled property-editing
knowledge in one place. The complete panel behavior suite is a sibling test
Module and exercises the same parent Interface rather than being embedded in
the production source file.

## Widget Contract

`Widget` is retained-mode and exposes:

- `measure`
- `layout`
- `event`
- `paint`
- optional overlay paint/hit-test
- children traversal
- focus/accessibility metadata

`paint()` must be side-effect free. `event()` may request platform side effects through `EventRequests`; app/platform layers execute them.

Inspector effect rows are projected from the domain `ParameterSchema`. Numeric
soft bounds and step, enum options, and typed resource intent come from that
single schema; `AnimatablePropertyUiMetadata` groups, orders, or spatially lays
out controls. Definition order is optional so older author data falls back to
deterministic address order; widgets never derive semantic order from translated
labels. The UI routes mutations through the instance address but displays and
persists the definition-stable `ParameterId`. It must not recreate ranges,
accept enum keys absent from the schema, or flatten a resource reference into a
generic text parameter.

White Balance, Primaries, and ASC CDL are ordinary Definition-backed Inspector
effects. Their Float and Vec3 controls retain stable `ParameterId` and
`AnimationParameterAddress` identities, including `builtin.color_wheel` as the
backward-compatible Primaries author key. Static writes, keyframe writes,
selection, add/remove/reorder, and Undo/Redo all cross the same
`VisualEffectProductAction` author transaction; the panel owns no private grade
math. Gamut Compression and Highlight Recovery follow the same projection:
their `amount`, `threshold`, `rolloff`, and `strength` rows come directly from
the registered Parameter Schema, retain stable animation addresses, and use the
ordinary add/select/set-value/undo Product Actions. The Inspector neither
duplicates ACES constants, decides a working space, owns a private parameter
pack, nor creates an alternate animation model.

HDR Grading uses that same generic projection for all 33 controls. The
definition-owned `HDR · Global`, Blacks, Dark, Shadows, Light, Highlights, and
Specular groups are preserved when Clip placement namespaces property paths;
the Inspector inserts subgroup rows only when the projected group changes.
Float and Vec3 editors, animation addresses, add/select/set-value actions, and
Undo/Redo remain the ordinary Effect paths. The panel neither samples zone
weights nor builds the 512×2 execution table.

The Grade Graph Inspector is a projection of Sequence authority, not another
graph model. It shows the selected Clip Definition, Group Pre/Post labels,
Timeline Grade, active Version, Version count, and node count. Its Create Grade
and Add Node commands are pre-admitted through `ProductActionAvailability` and
then transported through the `ui.grade` external namespace; dispatch repeats
the authoritative validation before one `commit_active_sequence_edit`.

Creating and assigning a Clip Grade is one transaction and is unavailable when
the owning video Track is locked. Editing the referenced Shared Definition is
a Sequence-level catalog operation, so Track lock does not disable Add Node.
The Inspector therefore does not apply its generic Clip-editable flag to all
Grade controls. It owns no execution schedule, hierarchy ordering, graph
validation, Version copy, or Effect preparation.

Gallery author actions use the separate `ui.gallery` namespace. Capture, remove,
and rename are Project transactions and therefore restore through the same
bounded Project Undo/Redo history; selecting a reference or changing its
Wipe/Split position is transient App session state and creates no author
generation. A frozen still retains the Grade Version bindings that produced it,
so activating another Version and comparing against that still is a version
comparison without a second graph authority.

The Window host accepts “Capture Gallery Still” only with an open Project. If
the exact current Preview result already retains its CPU Float32 evidence, the
capture is committed immediately. A GPU-only current Viewer instead requests
the normal exact CPU fallback asynchronously and commits after presentation
publishes that result; the UI thread never reads back or composites a full GPU
frame. The Viewer decodes the selected PNG once and clips it over either the
CPU raster or external GPU texture with one divider. This transient paint does
not alter the Timeline render, Program Output, Scopes input, cache identity, or
stored divider position. Direct evidence-bearing GPU capture/readback is not a
current capability claim.

Basic Title Inspector rows use the same definition-backed property projection
and `ClipProductAction` parameter-write Interface as Transform/Opacity; the panel does not own a parallel
title draft or reconstruct property ranges/options. Text is multiline, the
concrete font family remains editable regardless of display length, and
font-size/fill/tracking/line-height animation is evaluated at exact Clip
visual author time. The panel maps the Sequence playhead once through the
selected Clip and uses that same coordinate for built-in Transform/Opacity,
Basic Title, effect, and curve controls. A mutation validates Track lock and the complete
candidate title before recording one Sequence snapshot. The generic legacy
solid-color tint row is hidden for Basic Title so two controls cannot claim
authority over its fill.

The Transform Inspector exposes the complete authored spatial value rather
than a UI-only uniform approximation. Position X/Y are sequence-canvas pixels,
Anchor X/Y are full source-authoring pixels, Scale X/Y are independent percent
views of the unitless domain values, and Rotation is degrees. Editing one
vector component preserves the other component. Preview Full/1/2/1/4,
proxy/original selection, and Viewer Fit zoom change only execution or
presentation sampling and never rewrite these controls.

`CurveEditor` is a normalized interaction Module, not an automation owner. It
keeps high-frequency pointer motion local and emits exactly one committed
`CurveEdit::{Insert, Move, Delete}` for a pointer gesture; keyboard edits are
already atomic. The Inspector Adapter maps the snapshot-local point index to
`AnimationParameterAddress + KeyframeId` before dispatch. Evaluated
Hold/Linear/Bezier samples are a separate read-only paint series, so display
fidelity cannot rewrite interpolation. Each editable point also carries an
explicit interaction policy: virtual Clip-boundary samples are fixed and
non-deletable, while real author keys remain movable and deletable even when
their time lies exactly on a boundary. Escape cancels widget-local preview
state and publishes no author mutation.

The same `CurveEditor` also edits a static structured effect curve without
pretending it is timeline automation. The Inspector converts its complete
normalized point set to a validated `NormalizedCurve` and dispatches the normal
stable-address `VisualEffectSetParameterValue` action. Core rejects invalid or
over-limit sets, while the App author transaction supplies ordinary Undo/Redo.
`builtin.curves` exposes one mode row plus ten 150-pixel curve rows; no panel
owns a parallel curve draft or execution interpretation.

Single-line `TextInput` has two mutually exclusive dispatch policies. Live
change mode emits after each committed text/IME edit and is reserved for draft
or filter state. Transactional commit mode retains edits inside the Widget,
captures one focus-session origin, emits at most one Action on Enter or focus
loss only when the text changed, and restores the origin on Escape or
programmatic disable. App panel Adapters select the policy; they must not
simulate author-transaction coalescing outside the Widget or turn each typed
character into Undo/Author Generation history.

Timeline Track Targeting and Sync-Lock are editor interaction policy, not
renderable Sequence fields. `TimelineTargetingState` is keyed by stable
Sequence/Track identity in the open editor Session and stores only exceptions
to safe defaults: every current or newly created Track is Target-enabled and
Sync-Locked unless explicitly disabled. Target decides whether structural
content edits cut a Track. Sync-Lock independently decides whether downstream
placements follow a program-time ripple. Deleting Tracks/Sequences, replacing
the open Project, or closing the Project reconciles or clears stale overrides;
none of these control changes creates an Author Transaction or marks the
Project dirty.

The Timeline Track header exposes independent T and S controls for video and
audio Tracks. Their checked state is projected from `AppState`; Widgets retain
no competing policy. `TrackProductAction` is the sole product-operation
Interface for Add, Move, persistent Track controls, Target, and Sync-Lock. Its
`ui.track` codec is the only namespace/name/payload interpretation. Every
existing-Track operation addresses a stable `TrackId`; Move additionally uses
`TrackRelativePlacement::Before | After(TrackId)`, never a display or
video/audio-list index. The Timeline Adapter alone reverses video display order
into canonical author order. Dispatch revalidates both identities, same-kind
membership, control applicability, and exact no-op status against the current
Sequence.

Persistent Visibility (video only), Mute (audio only), and Lock (both) enter
one Author Transaction and Undo/Redo. Target and Sync-Lock route to the
open-Session `TimelineTargetingState`, never advance Author Generation or
History, and reject repeated values as `ActionNotExecuted`. Add allocates the
new Track identity only at dispatch, so retained Widget projection cannot
manufacture author identities. Stale identities, cross-kind anchors,
self-anchors, and already-satisfied relations are rejected without dirtying the
Project. The replaced `ui.timeline` Track names, redundant `is_video_track`
flags, and projected `target_index` interpretation do not coexist with this
Interface. This state is intentionally
not yet durable workspace preference: reopen returns to the documented
default-on policy until a versioned workspace-state owner is introduced.

A Timeline Adapter snapshots the controls into structural requests containing
explicit content/ripple Track sets and explicit
automation/Transition/navigation policies. Insert consumes its target
placements plus ripple set. Lift consumes Target-enabled Tracks and no ripple
set. Extract consumes Target-enabled Tracks as content and the Target ∪
Sync-Lock union as ripple. An untargeted Sync-Locked Track may shift only when
the removed range contains no content on that Track; otherwise the Action is
disabled and direct execution returns the same typed domain failure. The
domain operation never reads selected Track, panel focus, or process-global UI
state. This makes the same request deterministic in Window, Headless Golden,
and future scripting Adapters. Lift and Extract are discoverable in both Clip
and empty Timeline context menus, use the Sequence In point or zero plus its
exclusive Out point, and commit through one Author Transaction. The existing
drag collision choice is labeled `PushForward`; UI code must not expose it as
professional Insert.

In/Out and Range Edit product intent use the same closed Timeline Action slice,
but are not one domain request. Ruler input carries `FramePosition` with its
explicit grid and is lowered once to exact Sequence author time. Clear and
repeated point writes are rejected as no-ops before mutable access. Lift and
Extract carry only the closed kind; the App resolves the latest In/Out plus
Target/Sync-Lock state into the complete `RangeEditRequest` at dispatch. This
avoids both bare-frame ambiguity and stale duplicated range/Track snapshots.

Inspector audio source controls project existing author state rather than own
it. Each command emits the same complete `AudioComponentEditRequest` used by
all Component fields, with the canonical stable
`(TrackId, ClipId, AudioComponentEditId)` address and an
`AudioComponentSource` value. Media choices carry only Asset
`AudioSourceComponentId` values and nested choices carry only child
`ProgramOutputId` values. Timeline re-resolves exact ownership, lock and source
kind, while the App audio-authoring Adapter proves current Asset/child
membership before it records one undoable Sequence snapshot. A stale Track
address therefore fails instead of silently retargeting a moved Clip. The old
`ui.inspector` source payload and dispatcher are deleted. A neighboring Asset
stream-mapping row is intentionally a different command family: refresh
acquires current probe evidence without retargeting, and rebind atomically
changes one Asset catalog binding while preserving its logical ID. UI labels
may describe physical stream indices, language, title, and layout, but those
indices never enter Timeline authoring payloads.

The same Component section projects enabled state, static dB volume,
normalized pan/balance, and independent exact edge fades. Every interaction
emits one typed `AudioComponentEditRequest` mutation instead of serializing the
whole `AudioComponentEdit`; Timeline re-resolves Clip/Track/edit ownership and
validates the full candidate before commit. Volume exposes a normal working
range of `-60..+12 dB` while preserving the author hard range
`-120..+24 dB`; pan displays `-100..+100` and crosses the domain boundary only
as normalized `-1..1`. Fade seconds are quantized once to exact
`TimelineTime`, bounded by Clip duration, and zero means absent. Curve selection
preserves the exact duration and is disabled when that fade is absent. The
panel owns no parallel audio draft and no execution interpretation.

`app_ui::audio_component_mapping` owns the Inspector projection for Component
channel matrices. The Adapter combines persistent mapping intent with exact
current Asset-binding or child-output layout evidence, renders `Standard` as a
read-only review matrix, and exposes explicit sparse coefficients by semantic
channel label or discrete ordinal. Choosing a custom preset materializes one
exact matrix; adding, removing, or changing an edge reconstructs and submits
the complete canonical matrix in one Product Action. Missing, unsupported, or
mismatched layout evidence remains visible and fail-closed. The Adapter never
infers speaker positions from channel count and never rewrites an existing
explicit matrix after source rebinding.

## Product SVG Capability

Bundled product SVGs are static UI artwork, not a general document or title
format. Their admitted subset is path/group geometry with fill, stroke, clip,
gradient, and transform semantics. The repository's SVG corpus contains no
text nodes, embedded raster images, remote resources, or foreign objects, and
the `usvg`/`resvg` dependencies therefore disable their default text,
system-font, memory-mapped-font, and raster-image features. `VectorIcon`
normalizes this subset into retained meshes and an optional cached raster; the
app favicon uses the same path-only rasterizer.

Every SVG document receives one domain-separated, length-framed SHA-256 source
identity over its exact bytes. That complete 32-byte value—not a caller label
or a projected standard-library hash—is the authority for static parse reuse,
raster reuse, and renderer-atlas draw keys. A bundled icon's caller-supplied
`id` remains diagnostic only: reusing that label for different SVG bytes must
parse, rasterize, and upload a distinct icon, while identical bytes may safely
share work across labels.

Raster reuse is a process-level UI resource, not an unbounded static memo.
`mondrian-ui-widgets` retains an exact source-and-size LRU with both entry and
RGBA-byte limits (128 entries and 32 MiB by default). Reconfiguration trims
synchronously, zero disables retention, and an individual raster larger than
the byte grant is returned to the caller without entering the cache. Parsed
geometry for bundled `include_str!` artwork remains a finite static-product
set; dynamically supplied SVGs do not enter that static parse map.

Adding an unsupported SVG feature requires an explicit capability decision,
new adversarial parsing/raster tests, and a dependency review. It must not
silently widen every UI process's parser surface. Timeline titles and imported
visual media use their own typed authoring and media pipelines and must never
be routed through the product-icon parser.

## Focus and Accessibility

Focus ownership is not the same as visible focus indication. `FocusSource::Keyboard` may show a focus ring; pointer/programmatic focus owns keyboard input but normally does not show the ring.

Accessibility `focused` must reflect real focus ownership, not `focus_visible`.

## Overlay and Menus

Dropdowns, context menus, popovers, and tooltips should render through overlay paint/hit-test so they are not clipped or hidden behind sibling panels. Menubar menus and context menus should share menu primitives.

## Commands

Menus, shortcut preferences, command palette, and future plugins should consume `app_ui::commands` descriptors. Menus are command presentation, not business logic owners.

Editorial transport commands are first-class descriptors and editor actions:
`J` dispatches reverse shuttle, `K` dispatches stop-shuttle/Pause, and `L`
dispatches forward shuttle. Repeated J/L rate changes are interpreted only by
the Playback Engine; shortcut routing, command availability, Window host, and
Headless adapters do not keep their own direction or multiplier state.

The startup and editor shells share the same new-project dialog state machine.
New-project and project-settings workflows consume one app-UI color-engine
catalog; neither dialog owns the product list of Mondrian Standard, pinned ACES
presets, and Custom OpenColorIO. Their dropdowns produce workflow-local draft
actions while the shared native-selection boundary validates a chosen `.ocio`
file and pins its working/display/view and digest identity. Invalid configs
remain visible as dialog errors and never create a partial path-only mode. The
two shells only route requests and must not duplicate this state transition.
Applying the project-settings modal emits one complete project-domain engine
replacement action. App-domain admission prepares the candidate config and
validates the future-Sequence template plus every existing Sequence atomically;
the modal never mutates renderer, Sequence, or persistence state directly.
Sequence settings renders the Project engine as a non-interactive readout, not
as a disabled dropdown that could imply Sequence ownership. It disables the
working-space control when that engine pins one immutable working identity. The
shell draft also ignores forged changes through that disabled control;
app-domain validation remains the final authority for persisted actions.
The workflow control presents SceneReferred first because new Standard
sequences use the package-pinned product View by default. DisplayReferred remains
an explicit direct-colorimetric bypass; the UI must not relabel it as the normal
Standard path.

Opening an editor dialog is a shell action because it mutates transient UI state,
not the undoable domain model. The dialog's committed payload must flow through a
domain/app action owned by the target subsystem. For example, Asset Library →
Interpret Footage opens an app-shell modal, but applying Auto/Override is
an asset-library mutation that persists `AssetMediaInterpretation`.
The modal keeps the input color-space and encoded signal-range dropdowns compact
while retaining a read-only diagnostic body. Auto is the default for both;
explicit color spaces and Full/Limited range selections persist independently,
and Auto displays the current resolved/detected result. Changing either control
must preserve the other control and the asset payload classification.
Preview and thumbnail work keys preserve that Auto/Override authority through
`DecodedVideoRangeContract`; the UI must not flatten it to the currently shown
probe value before decode, because an explicit frame tag may refine Auto while
an authored override must continue to win.
The action that opens the modal must preserve decoder range, raw CICP
primaries/transfer/matrix, detector evidence and warnings, plus the effective
engine and working identity; reducing this payload to the final color-space
guess loses the facts needed to audit an inference. The dialog displays the
resolution method/confidence, whether the result is inferred, user override
state, signal tags, evidence/warnings, the input-to-working path, and the exact
stock-OCIO processor cache-id. Processor identity is queried only when the user
opens or changes the dialog, never while rebuilding asset cards.
Machine-readable diagnostics also retain counts for lower-priority metadata
hints rejected by CICP or ICC, so preview/export health surfaces can distinguish
an unambiguous result from one that won over conflicting comments or file-name
inference without parsing display strings.
The picker includes encoded delivery/camera spaces plus supported scene-linear
and ACES source identities. Sequence output controls use a separate fixed list
of display-referred spaces, so ACES2065-1, ACEScg, ACEScct, linear RGB, and
camera Log identities cannot appear as presentation targets.
Non-color data is an asset payload classification for advanced utility-channel
workflows, not an option in the primary color-space picker. A future payload or
channel-role control may edit that classification, but the Interpret Footage
color-space and range dropdowns must preserve the existing payload value while
changing only their owned interpretation field; they must never create or clear
`AssetColorPayload::NonColorData`.

## Timeline Position Presentation

Sequence settings persist one `TimelineDisplaySettings` value and resolve it
through the core `TimelineDisplayContract`. Viewer playback chrome and Timeline
ruler receive that resolved Interface from the app panel Adapter. Widgets may
choose compact label density, but may not infer NDF/DF, apply an origin, clamp
negative time, or split/reassemble a formatted timecode string.

Changing Frames/SMPTE presentation is an undoable Sequence-settings edit, not a
transport or media-time mutation. If persisted settings do not validate against
the Sequence frame rate, the domain rejects the snapshot; a forged invalid
runtime state fails visibly instead of silently selecting another counting
mode. The product menu exposes only formats with implemented parsing/formatting
and reference tests.

Visual Transition UI intent follows the same domain-light Widget seam as Clip
editing. `mondrian-ui-widgets` may emit track/Clip/Transition view indices and
frame-grid gesture proposals, while the App panel Adapter resolves stable
identities. Selection stores only `VideoTransitionId`; it never mirrors a Track
identity that author validation derives from strong Clip endpoints. The panel
Adapter lowers a resize proposal exactly once through the resolved Sequence
display grid into an exact `TimelineTimeRange`; the external action therefore
contains no bare frame authority or implicit time base. Default duration,
source-handle admission, Track locks, and the one-Undo author transaction remain
inside the App's Video Transition authoring Module. Ordinary Delete lowers to
the same typed Remove operation; Ripple Delete is unavailable because deleting
a Transition cannot move Timeline placements.

Handle diagnostics are projected as one immutable, revision-bound map prepared
by that same deep Module. The Timeline Adapter consumes the map once for all
Tracks; it never re-enters the Asset Library for each Transition or UI row. Its
cache identity binds the open Authoring Session, Project Author Generation,
active Sequence revision, and Asset Library instance/revision. A library change
during preparation prevents cache publication, while invalid author structure
and recoverable unresolved media remain distinct user-visible states.

The Timeline Adapter produces every Clip/Transition stable identity and its
domain-light view model in one projection pass. It must not build a second ID
list with an independent filter: failed time projection is allowed to omit one
view, but can never shift later gesture indices onto another author entity.
Nested-Sequence identities travel in that same Clip projection. The Widget owns
only overlay geometry, hit testing, resize preview, and an edge-constrained
frame proposal. Transition overlays take hit priority over their endpoint Clips;
right-clicking an exact adjacent unlocked video cut exposes the default Cross
Dissolve command. Resize preview never mutates the supplied model and commits
exactly once on pointer release.

Clip pointer selection carries an explicit `Replace`, `Toggle`, or `Preserve`
intent with one stable Clip ID. The App—not the Widget—resolves current Track
membership and expands a Clip Link Group into one selection unit. Normal click
replaces, Ctrl/Cmd-click toggles the complete unit without starting a drag, and
right-click preserves an existing multi-selection. The directly clicked Clip
stays primary for single-target panels. Timeline projection includes typed link
identity, complete member count, and group-wide lock availability so the Widget
can render a link indicator and disable Link/Unlink menu entries without
reinterpreting author membership. The semantic Action is selection-scoped; the
App resolves current stable IDs again, applies one domain edit, and commits one
Undo step only when membership actually changes.

All current-selection editorial commands share the closed
`TimelineSelectionEdit` Interface: Link, Unlink, Trim-to-playhead,
Roll-to-playhead, and Set Enabled. The Widget never transports its selected ID
list or a Track-lock snapshot. The deep App Module resolves current Session
selection/playhead state, keeps the primary Clip first for anchor-sensitive
edits, expands structural Link Groups where synchronized placement changes
require it, and uses the same pure Trim/Roll preparation as execution. A linked
Trim clamps one exact group delta against all members and retains existing J/L
or sample offsets; the Widget does not align edges or choose a second policy.
Product availability is therefore a projection of the owning Module rather than
a second collection of UI geometry rules. The sole strict
`ui.timeline.edit_selection` codec uses one
variant-specific payload shape; unknown variants and fields fail closed.

Targeted Split and global Razor are separate product intents. A targeted Split
names one stable `ClipId` and returns one typed `SplitClipOutcome`: the requested
left/right identity mapping plus every synchronized link-group member mapping.
Callers consume that receipt directly; they do not infer new Clips by comparing
pre/post ID sets. Other Clips that merely intersect the same playhead remain
unchanged. Global Razor may intentionally traverse every eligible Track, but it
must invoke the same validated split implementation rather than weakening the
targeted contract.

Precompose is likewise a semantic Timeline Action, not a Widget-side graph
rewrite. Its payload carries only the requested nested Sequence name; the App
deep Module resolves the current stable-ID Clip selection, requires every
identity to remain present, expands complete link groups, rejects locked or
empty ranges, and prepares exact child geometry plus parent target Tracks before
mutation. Execution consumes that plan, projects selected content and
transitions into child-local time, and replaces the parent range in one Project
transaction while detaching only affected Track Clip lists.
The new child uses `NestedComposition`, retains the parent Sequence settings,
forks placement-local audio identities where required, and becomes reachable
only through ordinary nested Clip content. After commit the App selects the
replacement Clip. Video-only and audio-only selections create only their
corresponding parent placement; a linked A/V selection creates one linked
video/audio replacement pair targeting the same child output. UI surfaces may
prompt or choose presentation details, but
must not pass an independently computed child graph or duplicate selection
membership.

Transition colors and blocked/hover/selection states are semantic theme tokens.
The App Adapter re-observes current external source-handle availability when it
projects the low-frequency author view and exposes a fail-closed diagnostic. It
does not repair, shorten, or mutate the authored range. Media execution and
per-frame playback therefore remain outside the Widget and panel seams.

Basic Title creation is a stable command exposed by the Graphics menu and
command registry. The App authoring Module—not the menu, panel, or Timeline
Widget—chooses the selected/free video Track, derives the selected range or
five-second default from the current edit state, adds a Track only when all
existing unlocked tracks are occupied, and commits placement plus any new
Track as one Undo transaction. Timeline presentation uses dedicated semantic
title colors and the ordinary Clip selection/trim/drag model.

Qualifier sample authoring is a structured Inspector control over
`PropertyValue::QualifierSamples`. Each row edits one bounded RGB sample and
its Include/Exclude operation; add/remove actions build and validate a complete
`QualifierSampleSet` before dispatch. Empty sets, exclude-only sets, removal of
the last Include sample, and additions beyond sixteen never cross the Action
Seam. A successful edit carries the stable `AnimationParameterAddress` and the
whole typed set through the ordinary Visual Effect product action, so the
authoring transaction and project-wide snapshot history restore the complete
set in one Undo/Redo step. Widgets retain no parallel sample authority.

## Authoring Commit Consumption

Every successful Sequence or Project transaction returns one
`AuthoringCommit`. The App authoring Module consumes that receipt through one
deep adapter: it reports an explicitly unretained Undo command and the
resulting snapshot-history barrier, expands
Project-wide changes to the conservative current Sequence set, publishes
exactly one canonical `TimelineModified` invalidation for each affected
Sequence, and reconciles active Sequence/Track/Clip targeting after structural
changes. Project settings, Sequence collection edits, proxy-mode intent,
Undo/Redo, and ordinary Timeline edits cross the same adapter. Callers may still
publish operation-specific facts such as `ClipAdded`, but they do not repeat
cache invalidation or navigation repair.

This keeps `AuthoringSession` authoritative for transaction success and the App
authoring Module authoritative for post-commit product effects. Window
Adapters, panels, and individual command implementations may not reinterpret
`project_wide`, ignore `undo_retained`, or construct a second changed-Sequence
set. Headless and Window execution therefore observe the same receipt semantics.

## Semantic Action Completion

`Action` is an intent envelope, not proof that work happened. `AppState` accepts
only Actions owned by a concrete product Interface. Shell-only window/layout
Actions, unknown custom namespaces, and product operations without an
implementation return a structured failure; they never log and return
`Ok(())`. Undo/Redo errors cross the same boundary instead of being discarded,
and an empty history returns typed `ActionNotExecuted` instead of pretending
that author state changed. The Window Adapter consults action availability and
does not dispatch disabled Undo/Redo gestures; direct, Headless, scripting, and
automation callers retain the fail-closed product contract.

Move, edge Trim, and Seek retain the complete input `FramePosition`. The
App-owned Timeline Position Module checked-converts its time
base to exact Sequence-local time and performs one documented nearest-frame
lowering on the active Sequence evaluation grid. It never reads only `frame`,
and malformed or negative positions cannot reach Playback or an author
transaction. Direct Timeline edge Trim is Sequence-position intent; source-local
timing edits use the Clip's exact source-to-Sequence mapping before their own
lowering Seam. Neither may reinterpret a source-grid frame number as a
Sequence-grid frame.

The serializable `Action` algebra contains only real semantic intents. A
disabled control, stale view projection, invalid transient value, or gesture
that forms no command remains inside the Widget Adapter as `None`; it never
crosses the App dispatch boundary as a synthetic no-op. Widget-local protocols
may use a more precise result only when `None` is insufficient. In particular,
Asset Grid item drops distinguish `Unhandled` (allow grid fallback), `Consumed`
(stop fallback without a command), and `Dispatch(Action)`. That tri-state is
local input-routing state, not a second product Action hierarchy.

Inside each migrated App slice, product meaning is carried by the closed
`ProductAction` algebra. `Action::Custom` remains the external Widget,
scripting, and plugin transport Seam; it must not become a second product-domain
model. The current Timeline slice covers Clip selection, Clip movement, bulk
trim, seek, exact In/Out mutation, atomic Lift/Extract intent, the complete
current-selection edit algebra, Basic Title creation, direct Asset placement,
exact-time Insert, and Precompose. Track operations use the closed Interface
above. The sole `ui.timeline` codec rejects legacy bare-frame Move/Trim/Seek and
creation payloads,
unknown variants, and unknown fields; dispatch and availability consume the
decoded algebra rather than repeating namespace/name interpretation. Move
carries `ClipId + target TrackId + FramePosition`, Trim carries stable Clip
identities plus edge and `FramePosition`, and direct placement carries
`AssetId + TrackId + FramePosition`; the App derives Track kind instead of
trusting a copied media boolean. Insert carries canonical
`TimelineTime` values. The old Timeline string dispatcher and replaced
per-command action names do not coexist with this Interface. Opening a nested
Sequence is navigation, so it belongs to `SequenceProductAction`; dispatch
revalidates that the active parent currently contains the requested child.
Sequence-owned visual Transition operations use
a separate closed `VideoTransitionProductAction`: Select, product-default Cross
Dissolve creation, exact-range edit, and Remove. The
`ui.video_transition` envelope is decoded only at the Product Action Seam.
Payloads carry stable endpoint/Transition identities, exact Sequence-local
author time, and an explicit `Reject | ShortenToAvailable` handle policy.
Dispatch re-derives current endpoint Track membership, lock state, source
extents, and no-op status; a range already installed returns
`ActionNotExecuted` and creates no History. The old `ui.timeline` Transition
name/payload interpretation does not coexist with this Interface. The closed
`AudioProductAction` slice carries
complete `AudioComponentEditRequest`, `AudioProcessorRackEditRequest`,
`AudioChannelStripEditRequest`, and `AudioAutomationEditRequest` values for Clip
placement, Clip Processing Scope, Track, Bus, and Program Output authoring. Its
production constructors lower each request into one `ui.audio` external
envelope; the App codec admits a recognized payload into the closed algebra and
a dedicated App Module commits exactly one Sequence author transaction. Only a
changed commit triggers post-commit audio execution reconciliation. Timeline
remains the sole owner of address, lock, identity, curve, and schema validation;
UI availability only projects whether an active Sequence exists and cannot
duplicate those rules. Product insertion uses a separate typed
`InsertBuiltInProcessor` intent so repeatedly projecting a retained Widget does
not allocate a new author identity. The App resolves that intent to one
canonical versioned instance at dispatch and then enters the same Rack
transaction. This menu catalog is only a presentation catalog; persistent
built-in, VST3, and CLAP identity remains the definition reference plus captured
schema.
Logical Component source selection is a `SetSource` mutation on the same
stable-address Component request; it has no Inspector-only model or alternate
dispatcher. The App Module proves the requested recoverable dependency before
the Timeline transaction, while Timeline remains authoritative for source kind,
duplicate selection and aggregate validity.
The same closed algebra carries `SetTrackSolo`, but dispatch routes that intent
to the App's open-Session monitoring Module rather than an author transaction.
It never advances author generation, dirty state, or Undo/Redo. Runtime
replacement is part of action success; failure rolls the monitoring intent
back so the retained control cannot disagree with audible execution.

`AssetProductAction` closes the Project Asset Library namespace: preparation,
audio-evidence refresh/rebind, generated creation, folder creation, import,
Relink, interpretation, rename, proxy preference, removal, and organization.
Its external `ui.asset` codec is the sole namespace/name/payload
interpretation. Single-card menus and drag/drop remain small UI Adapters, but
they lower into the same `RemoveEntries` or `MoveEntries` selection payload as
multi-selection operations. App dispatch never loops SQLite mutations: the
Asset Library deduplicates, validates, and publishes the complete removal or
move in one transaction. Product removal retires visible Asset membership and
must not delete stable Asset records, Sequence references, proxy intent,
history, or recovery evidence. Asset-browser folder navigation is Window state
under `app.shell`; it is not an Asset Library mutation and is deliberately
absent from `AssetProductAction`.

The migrated Viewer slice carries only authored preview-resolution changes
through `ViewerProductAction`. Canvas zoom is presentation state owned by the
Window/Shell Adapter and deliberately remains outside the product algebra.
The Adapter treats a native dialog cancellation as a successful no-action
outcome, but propagates typed platform unavailability/backend failure through
the existing workflow error path. File-manager reveal follows the same rule:
dispatch failure is observable and is never presented as successful execution.
Viewer gestures that manipulate picture geometry do not create a second Viewer
transform model: they emit the same `ClipProductAction` used by Inspector,
Headless, scripting, and future plugin Adapters.

`ClipProductAction` closes enabled state, typed Solid Color source edits,
atomic direct-parameter writes, and stable-key numeric curve edits. A direct
parameter write carries canonical `ClipId` plus one or more
`AnimationParameterAddress + PropertyValue` entries; it never carries Track
identity, media-kind snapshots, path aliases, percent-specific fields, or a
Viewer/Inspector interpretation. The deep `app::clip_authoring` Module derives
current placement and Clip-local author time, resolves only the Timeline
Module's persistent intrinsic parameter projection, prepares every normalized
mutation, and commits one Author Transaction. Existing keys retain identity,
Bezier handles, interpolation, and temporal flags. Empty or duplicate batches,
stale addresses, invalid values, audio-Track visual targets, lock blockers, and
no-ops fail before candidate allocation; one invalid member cannot partially
apply a multi-field gesture. Solid Color remains a typed Clip content edit and
is deliberately not exposed as a synthetic parameter address.

`ProjectProductAction` carries Project creation, durable recovery selection,
future-Sequence defaults, and Project Color Environment replacement;
`SequenceProductAction` carries navigation plus create/switch/duplicate/delete/
settings authoring. The external payload is only intent. Recovery still enters
the Recovery Module's lease-bound second verification, and Sequence mutation
still enters the single `AuthoringSession`. In particular, Sequence creation
returns its real transaction result and stable `SequenceId`; construction,
transport-stop, or commit failure cannot be logged and then reported as a
successful Action.

Startup recovery is intentionally a two-step product flow. A recovery row emits
a shell-local inspection request, and the resulting modal projects the exact
source archive, absolute and relative capture time, canonical save target, and
typed target-presence/revision evidence already produced by the Recovery
Module. Confirming the modal forwards the unchanged candidate through
`ProjectProductAction`; it neither reads the filesystem nor grants overwrite
authority. Recovery preflight and lease-bound admission revalidate the target,
Manifest, source hash, archive identity, and revision, so a target change while
the dialog is open fails closed and requires a refreshed choice. The Window
Adapter refreshes recovery candidates after such a failed recovery Action while
retaining the typed failure reason for product status. The dialog also makes
clear that recovery opens a dirty Session and does not publish the target until
a later explicit Save.

`ExportProductAction` closes the complete product Export namespace: exact draft
edits, immutable request admission, cancellation, and bounded terminal-history
cleanup. Enqueue carries the existing `TimelineExportRequest` directly; there
is no duplicate UI payload and no missing-field compatibility default that can
silently change publication policy. Draft edits preserve permissive syntactic
input but reject unchanged values and stale Sequence identities as executed
actions. Cancellation availability reads a non-allocating Queue observation,
while dispatch consumes the Queue's exact `ExportCancelOutcome`; only
`Requested` is success. Terminal cleanup uses the Queue-locked removal count
and treats Completed, Failed, and Cancelled entries consistently.

`VisualEffectProductAction` separately closes Clip-local visual Effect insertion,
selection, enabled state, removal, relative reorder, and parameter-value edits.
Every payload carries canonical `ClipId`/`EffectId`; Track identity and media
kind are resolved by the author Interface. Reorder uses stable
`EffectRelativePlacement`, never a projected vector index. Parameter edits use
`AnimationParameterAddress`, never a mutable property path. Add resolves the
current registered Definition through the Effects Module and fails before the
transaction when it is missing or not visually executable. The borrowed
availability projection and authoritative dispatch both reject stale targets,
locked mutations, type mismatches, already-selected instances, unchanged
enabled/value state, and already-satisfied ordering. Only a real author change
allocates a Sequence transaction; selection remains transient and creates no
Undo entry. The old `ui.effects`, Inspector Effect JSON payloads, and generic
editor-state Effect variants do not coexist as alternate interpretations.

`VisualMaskProductAction` closes the parallel Clip-local Mask slice without
turning Masks into Effect definitions. Add/select, enabled and edit-lock state,
stable relative reorder, removal, static/keyframed complete-shape writes, and
stable-address scalar parameter writes share one codec and one transactional
App Adapter. Payloads carry `ClipId`, `MaskId`, stable parameter addresses, and
relative Mask anchors only; current Track placement and exact Clip-local author
time are re-derived from the Authoring Session. Track lock and Mask lock remain
separate authorities. The Inspector projects the same Interface for rectangle
ellipse, or Bezier creation, shape animation, order, enable/lock/removal, feather,
opacity, expansion, invert, and operation. It never mutates `clip.masks`, parses
a Mask UUID from a property path, or writes a Mask enum as arbitrary text.

The Viewer owns Power Window gesture state and pointer capture. Rectangle body
and corners edit translation/extent, Ellipse body and axis handles edit
translation/radii, and Bezier anchors plus incoming/outgoing handles edit the
complete path. Pointer motion updates only a widget-local preview; pointer-up
releases capture and dispatches exactly one complete-shape Action, while Escape
or capture loss publishes no partial author state. Playback, Track lock, or
Window edit lock makes every overlay read-only. The App mapping preserves all
Bezier controls, and Undo/Redo therefore restores the exact `MaskId`, shape
`KeyframeId`, and geometry rather than reconstructing a new Window.

The Mask Inspector also projects the closed Start/Cancel/Recompute tracking
actions. Start carries stable Clip/Mask identity plus model, direction and
bounded settings; it never carries decoded pixels or a projected Track/index.
Availability is read-only while playback runs, the Track or Mask is locked, or
that target already has a queued/analyzing/canceling attempt. AppState exposes
one bounded status projection (`Queued`, `Analyzing`, `Canceling`, `Canceled`,
`Completed`, `Failed`, `Stale`, or `CacheHit`) and the host pumps completion in
the normal background tick. Widgets own no worker, decode Session, tracking
cache, publication logic, or alternate shape-key writer.

`app_ui::audio_processor_rack` is the shared read-only Rack projection Module.
It deduplicates Clip bindings by Processing Scope, consumes Timeline's binding
count and lock blocker, preserves unknown plugin definitions and parameter
schemas, and generates only typed Rack Actions. Inspector and Mixer render the
same Rack sections and action factories; neither traverses audio author state to
reconstruct admission. Numeric controls take hard/soft range, step, unit, value
type, and animatability from `ParameterSchema`. An already-keyed curve is shown
as automation and its fallback value is deliberately not exposed as though it
were the playhead value.

`app_ui::audio_automation` is the dedicated curve Adapter shared by Inspector,
Mixer, and Rack sections. Timeline supplies the stable target, exact
`AuthoringTimeDomain`, schema, and lock admission. The Adapter requires an
explicit exact viewport: Sequence curves cover Sequence author time, Component
curves cover the visible Component-local Clip span, and shared Processing Scope
curves cover the union of current bindings. Only this viewport is normalized
for `CurveEditor`; normalized coordinates never become author state. Virtual
boundary anchors are read-only, real points retain `KeyframeId`, and one
Insert/Move/Delete gesture emits at most one typed product Action on gesture
completion. Moving a point reconstructs the exact key with its existing
interpolation and handles before changing time/value. Static controls are
disabled while keys are authoritative, but the exact evaluated curve remains
visible and editable.

`app_ui::audio_mixer` projects audio Tracks in Timeline order, followed by
authored Buses and Program Outputs, each with input trim, an honest static-or-
automated fader state, persistent Track mute, transient Track solo, the latest
complete post-mute sample-peak/RMS evidence, and both pre/post-fader Rack
addresses. Meter projection reads one bank snapshot for the complete panel and
matches stable Track/Bus/Program Output identities; a target outside the active
compiled Signal Closure is shown as unexecuted, never synthetic zero. The Audio
playback meter is projected only while transport is running, so a paused
Session's last completed block is not presented as current signal. The Audio
workspace owns a real `Mixer` panel whose Inspector is a
secondary tab; panel focus and persisted layout use the stable `PanelKind`
identity. The Mixer also projects existing outgoing Routes and incoming Route
counts. A Bus creation action allocates identity only during dispatch and may
connect it to the first routed Program Output in the same author transaction.
Track/Bus Route menus submit exact source tap and destination identities;
existing edges expose enabled state, honest static-or-automated gain, and
undoable removal or stable-identity endpoint rewiring. The Mixer consumes one
Timeline-owned Bus-reachability inspection built from all authored Routes,
including disabled edges, and uses it for both create and rewire menus so
self/cycle-producing candidates are not presented; Timeline's complete
candidate validation remains final authority. Rewiring changes only
source/destination and preserves Route gain,
automation, enabled state, and identity. Bus names use transactional
`TextInput` commit and therefore produce no mutation while typing and at most
one typed `RenameBus` transaction per focus session. Bus deletion states how
many connected Routes will be removed and uses Timeline's `Disconnect` policy
rather than issuing N UI edits.
Bus post-fader UI does not invent a mute stage: although the shared author port
enum remains uniform, the product presents only real pre/post-fader choices.
Meter display is explicitly sample peak/RMS with clip and non-finite counts; it
does not claim true peak, loudness, hold, decay, or broadcast conformance. Those
presentation and standards layers must consume the same observation Interface
rather than create another mixer graph.

The migrated production constructors lower typed operations into the
external envelope, and one App-owned codec is the only
Implementation allowed to inspect their namespace, name, or JSON payload. A
recognized name with an invalid payload fails closed before legacy routing; an
unknown name remains untouched for another owning Adapter. Dispatch for these
slices then matches the typed algebra and no longer repeats string or
payload interpretation. This is a bounded migration, not a claim that every App
Action already belongs to `ProductAction`.

The migrated slices expose one borrowed, read-only
`ProductActionAvailability`. Project/Sequence membership and navigation, Track
lock, Clip membership/media kind, placement range, Sequence time-base, Export
draft difference, and Queue target facts remain private; the stable UI
Interface is only `allows(&ProductAction)`. Timeline, Video Transition, Clip,
Audio, Asset, Viewer, Project, Sequence, Export, Visual Effect, and Visual Mask controls
therefore use the same
admission Seam. The
projection does not clone or index the author graph or clone the Export Job
list for each query. Window and panel Adapters cannot reconstruct admission by traversing
`AuthoringSession`, `Sequence`, `Track`, or `Clip`, and cannot observe playback
or execution internals through it. Admission remains guidance: each App-owned
authoring, lifecycle, recovery, monitoring, or transport Interface revalidates
authoritative state at dispatch. Unmigrated custom Actions retain their current
Adapters until an independently verifiable typed slice replaces them; this
decision does not justify a parallel full action hierarchy.

Product Action constructors preserve the caller's complete intent, including
invalid values needed for authoritative rejection; they never clamp a negative
seek to zero or otherwise turn malformed input into a different successful
command. Likewise, an availability query may disable a Widget but cannot
replace dispatch validation or its precise structured error. For example, Cut
against a locked Track returns `TrackLocked` before changing the clipboard,
while an actually empty selection returns `ActionNotExecuted`.

Callers that need acceptance evidence must also verify domain postconditions.
For example, the Golden audio slice checks the installed `AuthoringSession`,
exact Sequence contract, stable Clip and Component Edit identities, values
before/after all Undo and Redo steps, a precise Generation/Sequence Revision
advance for every transaction, durable request identity and archive hash, and
values observed under a distinct fresh Session after load. Action admission or
a human-readable status hint alone cannot satisfy a Golden operation.

Golden validation has one UI-independent planning Module. It compiles the
closed schema-v4 / `windows-alpha-golden-v14` contract into two deterministic
ledgers of required fixture roles, operations, content, and exports. The global
ledger finds work absent from every slice; the Hero ledger independently finds
work that exists only in isolated diagnostic Sequences. A slice declares one
stable acceptance Sequence role and is an independently executable evidence
Adapter, not a top-level run: successful and failed slice reports both carry
`complete_golden_project: false`. The Rust top-level coordinator refuses to
start a complete run until every obligation is assigned to the Hero role. At
runtime every Hero-assigned slice must report the same primary `SequenceId`;
auxiliary nested Sequences remain legal but cannot replace that identity. The
coordinator then requires the exact slice report and authored Sequence sets,
waits for background proxy work to become quiescent, verifies the full-duration
Hero content boundary, and verifies every Sequence snapshot plus asset intent
after one final durable reopen. Only then may it emit one
`windows-alpha-complete-golden-project` report with
`complete_golden_project: true`. PowerShell owns only process deadlines,
schema validation, and consecutive-run classification; it cannot infer
semantic completion from test names, process exit, or a union of isolated
Sequences. Baseline qualification additionally requires a clean and stable Git
revision for the complete supervisor lifetime. Its aggregate evidence binds
that revision to the SHA-256 of the exact `mondrian-golden` executable;
dirty-tree execution is an explicit diagnostic outcome and cannot become
release evidence.

`GoldenProductWorkflowDriver` is the Headless product composition owner for
that coordinator. It creates one real `.mdp`, captures its typed `ProjectId`
and path, and retains one production `AppState`. Its private Hero binding locks
the initial `SequenceId`, complete `SequenceSettings`, and exact Project Color
Environment. The slice-binding Interface reuses that identity for a `hero`
slice, switching through the ordinary product action when necessary; a focused
diagnostic role instead creates a Sequence through one Project Author
Transaction. A stage cannot construct binding evidence or infer primary
identity from whichever Sequence happens to be active.
Sequence-scoped evidence requires one Generation plus one stable active
Sequence Revision advance; Project-scoped evidence requires one Generation but
allows active Sequence change. Durable reopen must replace only the
process-local Session, preserving Project, path, active Sequence, and saved
revision. Foundation Audio, Editorial/Transport, Proxy/Relink, Generated
Delivery, Visual Authoring, Recovery/Nesting, and Color Media Roundtrip are
reusable stages over this driver. Foundation Audio, Visual Authoring,
Editorial/Transport, Generated Delivery, Proxy/Relink, and Recovery/Nesting now
run on one Hero Sequence.
Visual must preserve the complete Hero audio projection. Editorial must
preserve the Foundation Track-owned audio anchor and the complete visual
projection while adding AAC placements only to deterministic pristine Tracks.
Its Overwrite, targeted Split, Ripple Delete, and multi-Track Insert consume
typed product outcomes. Track Targeting and Sync-Lock are resolved through
session Actions into one immutable command scope without advancing Author
Generation or Sequence Revision. Lift consumes only the targeted primary Track
and preserves program time; Extract consumes the same content Track and closes
program time on the targeted plus untargeted Sync-Locked editorial Tracks.
Exact half-open In/Out setup, structural postconditions, and one-step Undo/Redo
are retained as typed operation evidence. Scrub, settled seek, and play complete
exact Frame Presentation Tickets through the production Preview Runtime.
Delivery must
preserve every earlier scoped author projection, reuse the Foundation PCM
placement, and author its Solid Color/Trim/Transform/Opacity through typed
product Interfaces in a nonzero Work Area. After durable reopen it runs the
production Preview, audio Program Runtime, export queue, output validator,
media import worker, preview decoder, and bounded audio source reader. The
Headless Adapter attempts GPU execution first and records an explicit CPU
Raster fallback when a valid effect cannot execute on GPU. Proxy/Relink owns a
dedicated Hero Track with adjacent CFR `200..350` and VFR `350..500`
placements. Its local evidence binds both Clips and Assets, the asynchronously
relinked CFR record, and both proxy intents so a later stage can add unrelated
Hero authoring without weakening verification.
Recovery/Nesting adds a dedicated Hero Track in the exact `175..200` window,
keeps Hero as its primary identity, and owns one auxiliary Nested Composition
child. Its Autosave recovery and covering manual reopen must preserve the
complete Hero parent, child, Proxy anchor, and every earlier scoped anchor.
The file-backed HLG/Alpha roundtrip also uses Hero while retaining its own
scoped Track/Clip/Asset anchor.
The number of execution slices is not a Sequence-count invariant because a
Hero slice may own a strongly referenced child. Every acceptance slice is now
assigned to Hero; diagnostic Sequences exist only in focused infrastructure
tests and carry no Golden requirement.

Heavy media and GPU execution runs in the dedicated `mondrian-golden` process
entrypoint, never on a short-lived libtest worker. A terminal top-level report
is the semantic completion boundary. The external supervisor grants a bounded
natural-exit window and may then terminate the child process tree because
third-party graphics capture DLLs can block Windows process detach after all
Mondrian work has completed. Missing, malformed, or failing reports remain
fail-closed; process cleanup cannot create passing evidence.
The supervisor and Windows CI build the mixed-feature App/Golden targets with
Cargo incremental compilation disabled and one Cargo job. A cached
default-feature App artifact cannot become evidence for the `validation`
entrypoint after an MSVC/LLVM incremental-link failure; build success must come
from deterministic non-incremental code generation within the 16 GiB evidence
machine's bounded peak-memory envelope.

The Recovery/Nesting stage is fixture-independent. It adds one stage-owned Hero
Track, trims a generated Clip to `175..200` through the Timeline Action
boundary, dispatches one Precompose Action, and requires exactly one Project
author transaction. It then executes recursive Preview and Export frames,
publishes an autosave, closes the Session, recovers through the product
recovery Action, and compares complete Hero parent/child author hashes and
deterministic execution evidence. The recovered Session must remain dirty and
the recovery archive authoritative until a covering manual save atomically
retires it. A final composed reopen must preserve the fixed Project binding,
the two Sequences present at the Recovery boundary, earlier stage content,
relinked Asset intent, both retained exact `1/2` Clip source maps, and all typed
reimport profiles. Proxy/Relink owns the fixed-corpus signed-retime product
evidence: CFR and VFR each advance through `1/2`, `-1/2`, and reverse hold;
Preview and Export retain the same complete `SourceSampleTarget`; the decoder
records a proven physical PTS interval; Headless presents reverse and hold via
the production proxy path; and immutable-original H.264/AAC exports are
reimported against expected, adjacent-covering, and wrong-direction Program
references. Undo occurs before reimport because automatic proxy intent for a
newly imported delivery is a legitimate Project History entry and must not be
mistaken for the preceding Sequence edit. The later Color Media stage keeps
that count at two, reuses Hero in the `500..525` window, and
retains its two adjacent file-backed Track/Clip/Asset anchors without changing
the Recovery parent Track or child. All seven stage primaries now share Hero. The
supervised run `20260725T172841Z-complete-golden-d631639d` passed three distinct
run/Project identities under the preceding v9 contract. The v10
supervised run `20260725T234232Z-complete-golden-42ccff21` then passed three
fresh run/Project identities with Lift/Extract required and observed; its
aggregate report hash is
`4edaeb0404fad68b813f5592a0533b86788414b3f3142ac88be04e46fb5ca8aa`.
It is historical after the v11 constant-retime obligation and cannot satisfy
that superseding gate. The official v11 run
`20260726T075954Z-complete-golden-30ffb812` passed three fresh run/Project
identities with the fixed-corpus constant-retime evidence required and observed;
its aggregate report hash is
`e596fc2756eada710d84a0873dcd5e13f49c7d33ee35b8c959e6e130465ece4f`.
It is historical after v12 added attested VFR signed-retime, physical PTS
interval, dual Headless/Export/reimport, and two-placement retention evidence;
v12 has not yet completed the required supervised three-run gate.
Earlier eight-Sequence runs remain diagnostic history.
Qualified release-machine capture and independent HDR/Log numeric references
remain separate acceptance obligations.

The fixture-independent focused Retime gate exercises a wider semantic matrix
without claiming decode pixels: linked `+3/2` and `-3/2`, reverse-edge
`StrictPredecessor`, reverse picture hold with unaffected linked audio,
Undo/Redo, durable reopen, Prepared Visual Preview/Export agreement, and a real
dense Audio Session lowering onto the 48 kHz physical sample grid. Equality is
always over the complete target; checking only its `TimelineTime` is not valid
evidence because the adjacent reverse frame would then be indistinguishable.

## Playback Tick Ownership

The winit host may wake the application while playback is running, but playback
state transitions belong to `AppState`. Window code passes one absolute
process-monotonic observation into `AppState::advance_playback_clock_at(...)`
and only reacts to the returned refresh contract. It owns no transport-running
edge detector or elapsed-time accumulator.

Playback frame advancement must:

- project each absolute observation from one App-owned Engine-time anchor
- reanchor that projection after accepted Viewer/audio/preroll observations so
  an event-loop tick cannot count an overlapping interval twice
- use the configured clock role for frame targeting
- pause on the last content frame and mark natural end-of-playback separately
  from user pause/seek
- expose a bounded next-frame wakeup delay so the UI loop does not busy-poll

Viewer preview rendering remains an adapter concern. It consumes the current
playback frame from `AppState`; it must not own playback state or mutate the
timeline to request frames.

Monitor direct manipulation uses viewer-scoped UI actions with sequence-space
payloads. The viewer surface may emit absolute clip transform intents for
position, scale, and rotation, but `AppState` remains the single mutation owner:
it validates payloads, checks track locks, applies timeline property mutations,
and records one undoable snapshot for each committed monitor edit.

## Viewer Preview Scheduling

The Color workspace owns a real `Scopes` panel rather than aliasing the Effects
panel. Expensive analysis is demand-driven by the live dock tree's active tab,
without allocating a persisted layout snapshot on each frame. Mere panel
presence is insufficient: a hidden or background Scopes tab schedules no
renderer aggregation, buffer clearing, readback, or scope repaint work. Scope
input defaults to retained Program Output. Operators may explicitly select the
typed Monitor Output tap after local color-space adaptation and before ICC
calibration; the UI never silently changes taps when a window moves. The panel
owns Luma/RGB Parade, IRE/100/1000/4000/10000-nit scale, skin-line, 75% color
targets, and overview/grid/single-scope layout controls. These are versioned
machine-local `AppUiPreferences`, never `.mdp` author state. Skin line, targets,
axis labels, and pane layout are presentation-only. Changing only those fields
repaints the widget without invalidating GPU analysis; waveform mode, scale, or
tap changes reissue exactly one current Viewer scope result.
The same preference payload owns False Color, Zebra, and Gamut Alarm controls.
Zebra cycles bounded 90–100%, 70–80%, and 80–90% presets. These warning fields
do not alter the Scopes aggregation identity, do not require the Scopes panel
to remain visible, and invalidate only the current Viewer presentation. The
Window propagates restored and edited settings to both GPU execution and the
bounded CPU fallback.
The window registers GPU-generated waveform, histogram, and vectorscope
textures with stable UI keys using the linear external-texture contract, and
unregisters all three when Scopes is hidden or Viewer presentation is reset.

Playback-frame refreshes use a narrow UI update path: the host advances
`AppState`, then refreshes viewer playback chrome/frame data and the timeline
playhead without rebuilding the full dock tree. A transport-only turn retains
the currently installed frame, transparent-canvas state, pending/stale state,
and typed blocker; passing an absent Preview source must never erase those
facts. The next production Preview turn alone may commit their replacement.
Preview completion and GPU external-texture registration/clearing use the same
narrow presentation refresh domain. They set `preview_dirty`, never the global
`ui_dirty` model-rebuild flag. Media import, project, preferences, workspace,
and other author-facing changes retain the full model path.

The Window host owns one admitted Viewer presentation snapshot, separate from
the Widget model. Preview candidates for GPU texture, CPU raster, retained
current output, and transparent canvas carry the exact Playback presentation
ticket sampled with their evaluation. The host publishes the candidate into
that snapshot only inside the shared presentation-arbitration callback; Late,
registration rejection, or lost authority keeps the prior admitted output
visible as stale. A later root projection borrows this snapshot and cannot
re-evaluate Preview or attach the candidate to a newer demand. Widget
`Ready`/`Blocked` feedback is display state only and has no terminal Playback
authority. Headless presentation consumes the same ticketed candidate contract,
so Window behavior is not a second interpretation.

Execution resource policy is not author or presentation state. Dispatch
closure, grant publication, cooperative yield, and other diagnostics-only
changes in Proxy, Media Import, existing-Asset mutation, or Export must not make
their App polling Adapters report a product-model change. A realtime Preview
stall expiration updates Transport controls through the preview-free narrow
path and emits terminal evidence, but it does not synchronously request another
Preview candidate. The next normal production turn may admit a new demand.

The titlebar, menu bar, and their child trigger Widgets are persistent for the
Window Session. A full model projection updates their title, command
availability, checked state, and shortcut labels in place; it does not replace
their Widget identities. Hover, press, focus, pointer capture, open overlays,
and other transient interaction state remain owned by the existing Widgets and
must not depend on Preview readiness or redraw cadence.

Timeline audio waveforms are not a Widget or Window execution feature.
`AppUiHost` owns one UI-independent `AudioWaveformService` composition instance,
polls its bounded completion pump, and injects an `AudioWaveformSource` handle
into the Timeline model. The Timeline lookup supplies `AssetId`, an
already-resolved `AudioSourceSelection`, the visible source interval, and
presentation width. The selection carries the complete filesystem revision,
absolute physical stream index, and native layout; the execution Module builds
its exact `AssetId + AudioSourceSelection` key. Widget code never derives a
parallel `u64` revision from file metadata or probe display fields. The interval
is projected from the Clip's canonical source-time map and duration; the panel
cannot cache or reconstruct a second source out-point. The handle returns only a
resident envelope or `None`; it cannot expose FFmpeg, worker channels, cache
mutation, generation state, or failure policy to paint/layout code. Project
Library Generation/Session binding change rotates service generation,
and source revision prevents a
same-asset relink from presenting stale waveform data.

Constant retime also enters through typed semantic Actions rather than a panel
mutating Clip fields. `ClipProductAction::SetRate` carries an exact signed,
nonzero `TimeScale` and an explicit link-group policy;
`ClipProductAction::HoldFrame` carries one Sequence-grid `FramePosition` and
never retimes linked audio. The Inspector
projects the canonical source map only for file-backed media and nested
Sequences. Signed rates use a combined slider/number input; displayed
percentages are quantized at the Action seam to exact 0.01-percent basis points,
so `-150.00%` becomes `TimeScale(-3/2)` rather than a floating-point author value.
The Inspector applies signed rates to the selected Clip's complete link group
and offers picture hold only for a video Clip whose dependency is resolved and
whose playhead lies inside its half-open placement. A held picture may resume at
an explicit signed rate; linked audio is left at its existing rate by the
hold itself.

This low-frequency projection is only product availability guidance. The App
authoring Module remains responsible for resolving current stable membership,
Track locks, source extents, and Transition handles immediately before atomic
commit. Known still-image Assets have no speed control: their zero-rate map is
intrinsic placement semantics, not a user-created freeze frame. Missing
dependencies disable mutation without hiding the persisted map. Negative maps
remain editable signed rate state. The App validates the complete source extent
and same-span direction change before commit; reverse boundaries are not panel
arithmetic and remain the canonical Timeline map's responsibility.

Asset thumbnails follow the same Window boundary but retain an independent
execution policy. `AppUiHost` owns one
`app::thumbnail_service::AssetThumbnailService` through the shallow
`AssetThumbnailAdapter`; the panel performs only a nonblocking lookup and
receives `Loading`, structured failure, or a resident image. The service—not
the panel—owns source/color identity, deterministic still decode, bounded
admission and transport, generation cancellation, weighted raster LRU, failure
memory, and terminal diagnostics. It constructs the full typed request identity
from `AssetId`, path, complete `MediaFileFingerprint`, and the resolved
thumbnail color contract; UI code never invents a file-revision hash. The
adapter converts the validated sRGB `ThumbnailRasterFrame` to `RasterImage`
without copying its `Arc<[u8]>` and cannot manufacture a second cache or
scheduling rule.

`app::preview_runtime::PreviewProductionRuntime` owns media preview scheduling.
`WindowPreviewAdapter` is only its Window output specialization. Each viewer preview request
starts a monotonic generation, and background media jobs check that their key is
still requested by the latest generation before decoding. Completed stale jobs
may warm the cache, but they do not force a UI refresh for an older playback
frame.
The App composition root captures one borrowed immutable
`PreviewExecutionSnapshot` for every frame-producing call. It contains only the
exact open Authoring Session identity and generation, the canonical
`SequenceCollection`, Project Color Environment, Asset Library and resolved
proxy-selection policy, machine-local Viewer policy, and one coherently sampled
transport position/state/Epoch/quality scale/Frame Demand. A realtime Adapter
deadline is lowered once at that same sampling instant. The Runtime and all of
its production submodules receive this snapshot rather than `AppState`, may not
retain it, and cannot create a second timeline, Playback, Project, or display
authority. `PreviewFrameExecutionRequest` carries Proxy demand command authority
through a separate narrow `PreviewProxyDemandSink`; immutable facts never hide
an execution command or concrete Proxy Generation implementation.
Window and Headless presentation share the App-owned typed publication
arbitrator. It preflights the exact Frame Presentation Ticket at one sampled
instant, consumes `Late` without invoking output publication, and publishes
`Ready`/allowed `Degraded` before consuming the same ticket at that same
instant. Failed external-texture registration leaves the demand pending.
`Presented`, `NoDemand`, `DroppedLate`, `OutputRejected`, and `LostAuthority`
remain distinct through Window telemetry and Headless gates; a dropped-late
texture is released but is not mislabeled as registration failure. New GPU
output, retained-current output, CPU raster, and semantic transparent canvas
each carry the ticket captured by their own evaluation; Window never resamples
the current demand after a candidate crosses the Adapter. Payload-free
`Ready`/`Blocked` Widget feedback is only a post-admission projection and has no
terminal authority. Window retains one admitted visible state, demotes it to
stale after any unpublished disposition, and makes a repeated demand-free
Current projection idempotent so it cannot self-schedule a repaint loop. A
running transport with no pending demand may repeat an already published exact
output, but Preview cannot manufacture a new no-ticket candidate after a
terminal non-presentable delivery.
The Runtime also binds prepared visual programs to the process-local
`AuthoringSessionId` of the current open Project. Sequence and Effect revisions
authorize reuse only inside that Open lifetime: closing, reopening, or replacing
the Authoring Session atomically rotates the renderer cache scope, clears its
residency, cancels queued/in-flight Preview work, and invalidates final Viewer
output. Durable Project/Sequence/Track/Clip IDs may recur after reopen and must
never authorize process-local execution reuse by themselves. Lifecycle
cancellation performs the same cache-scope rotation even when the next Session
is not known yet. Inside one Open lifetime, the monotonic author generation
selects one `PreparedVisualProgramBinding` set. Current Viewer evaluation,
forward prefetch, preroll, and cold media-range lookahead share it; only a new
generation, Effect-registry revision, dependency refresh, resource
reconfiguration, or scope rotation may repeat full author fingerprinting and
Program preparation. This policy lives in the deep Preview Runtime Module; Window,
Headless, prefetch, and preroll Adapters do not maintain parallel invalidation
rules.
Test builds may attach the renderer-owned validation trace to the exact
prepared Preview closure before materialization. This is observation only: it
cannot alter scheduling, pending state, media resolution, nesting, temporal
execution, or cache identity. Preview/Export parity gates compare this ledger
and the real working composite against Export's validation Adapter rather than
maintaining a test-only Timeline walker.
The same production runtime owns one bounded Basic Title task. The Timeline
evaluator emits a complete generated-title request including evaluated author
state, Sequence resolution, persisted title-safe margin, target resolution, and
working space. The background task owns font discovery, shaping, cache, and
failure memory; the Window Adapter only observes typed
Ready/Pending/Unavailable results. Title generation can therefore neither block
the winit thread nor rebuild shell chrome while Preview is pending.
If a later generation requests the same media-preview key while a worker is
already decoding it, that in-flight decode remains current: generation changes
alone must not cancel identical frame/key work, or the viewer can livelock in a
permanent "preparing" state under repeated UI refreshes.
Proxy generation is an `AppState`-owned service, not an action-handler or
Window detail. Import, manual proxy-mode toggles, and preview playback pressure
submit typed origins to the same instance-owned `app::proxy_generation`
Module. Preview emits a complete demand only through the narrow command Sink and
does not receive or retain `AppState` or the concrete Proxy service. The Preview
Adapter does not retain its own request set: exact dedupe,
queued priority promotion, retained failure, and project generation belong to
the service. Background polling observes one completion revision and refreshes
models so a newly fresh proxy can replace source fallback; no Widget owns a
worker, retry rule, or FFmpeg process.

Timeline Export follows the same UI boundary but is a separate offline Module.
`AppState::enqueue_timeline_export` resolves one selected Sequence, atomically
captures its reachable nested/media dependency closure, and submits an
immutable `RenderJob` to the instance-owned bounded `RenderQueue`. The queue
outlives neither its App composition owner nor its cancellation authority, but
an admitted snapshot intentionally remains independent of later Project edits
or Project closure. Panels call only the App Adapter for enqueue, cancel,
terminal cleanup, revision polling, and lightweight `ExportJobSnapshot`
observation. They may project structured phases, truthful units, failures, and
color diagnostics; they cannot clone the heavy Timeline payload, mutate status,
invent progress, or infer completion from file existence. Headless execution
uses the same queue/evidence Interface rather than a Window-specific path.
The queue's complete diagnostics token and retained-jobs token are distinct
equality-only observations. Resource grant, dispatch, and yield changes advance
only diagnostics; they cannot dirty the editor tree or trigger Preview work.
The jobs token advances whenever the public `ExportJobSnapshot` collection
changes, including bounded progress and job diagnostics.

The export panel obtains delivery readiness from
`mondrian_export::resolve_export_delivery`, the same pure Interface enforced by
queue admission. It does not maintain a codec/color compatibility table.
Incompatibility disables action construction and outranks stale success status
text. This UI check is an early projection only; the queue remains authoritative
against the immutable snapshot.

The app export draft stores a stable built-in preset identity plus one
materialized editable `ExportPreset`. Selecting a built-in preset resets that
materialized value; changing container, codec/profile, raster, bit depth,
range, chroma, Alpha, CRF/VBV, GIF palette controls, or audio codec parameters
edits only the materialized draft. Catalog array position is presentation and
never becomes identity. Each form action carries a complete typed preset
snapshot, so no parallel UI-only field bag can disagree with enqueue. A
container edit rewrites the output suffix only when that suffix still matched
the previous container; an explicitly custom suffix is preserved. Admission
freezes the edited preset into the job.
High-precision image Masters are separate built-in representations rather than
values in the video-codec bit-depth dropdown: PNG16, OpenEXR Half, OpenEXR
Float32, DPX16, TIFF16, and TIFF Float32. Their exact scalar type is format-owned,
so the panel disables the generic 8/10/12-bit, range, and chroma controls for a
sequence artifact, shows the resolved representation label, and uses
`.pngseq`, `.exrseq`, `.dpxseq`, or `.tiffseq` directory suffixes. Scene-linear
Colorimetric endpoints are offered only for image Masters; ordinary media-file
presets retain display/Camera-Log endpoint restrictions. The complete typed
preset remains the only action payload and queue admission remains authoritative.
The same materialized preset exposes an explicit delivery Legalizer checkbox.
It is independent from Full/Legal range: enabled means clamp final
display-encoded RGB before quantization; disabled preserves excursions. The UI
only edits the complete preset snapshot, while queue admission remains the
authority for target compatibility.

The Audio Stems PCM24 built-in uses the synthetic `.wavstems` suffix only to
make the destination's directory nature unambiguous in the draft. Queue
capture, rather than the panel, selects every public Program Output in authored
order and freezes its identity and label. The published directory contains
deterministically named WAV files plus a validation manifest; the panel never
models stems as a codec toggle on a media-file artifact or attempts to infer
success from individual files.

The materialized preset also carries one typed `ExportColorTarget`: follow
Sequence Program Output, explicit colorimetric output, or an explicit
Project-engine Rendering View. Built-in SDR presets pin their Rec.709 rendering
target; HEVC Main10 and ProRes follow the Sequence until explicitly changed.
The export form exposes mode and target space as two controls so the user cannot
accidentally turn a display Rendering View into a direct Camera Log conversion.
Rendering View offers only display-referred targets; Colorimetric offers encoded
display and scene-log endpoints. Switching modes preserves a legal endpoint and
otherwise chooses explicit Rec.709, while queue admission remains authoritative.
The UI must not simulate Camera Log by editing Sequence Program Output or infer
a transform from the codec.

Sequence working/input/Program Output color policy and separate delivery
range/bit-depth/HDR defaults remain Sequence-owned and editable. The Project
engine is edited only through Project Settings; machine-local display management
is edited only through local Viewer preferences. Only working-space editing is
gated by the Project engine contract. Nested processing is edited on a selected
nested Clip placement, not in Sequence Settings, because it describes that
parent-to-child edge.

Media preview frames are held in a bounded LRU cache keyed by asset identity,
the complete media file fingerprint (filesystem object identity and change
generation together with length/mtime evidence), source frame/time, target
preview dimensions, input color interpretation, target working color space,
media-input tone-map policy, and color engine. The fingerprint is a
conservative revision token, not a content digest. Program Output and
monitor-adaptation tone mapping are excluded. A media frame decoded for one
working-space contract must never be reused for another viewer/export color
contract, and same-path media/proxy replacements must not reuse stale app cache
entries when the file fingerprint changes. Preview path resolution should
capture source/proxy freshness and the resolved file fingerprint in one
metadata probe path, so playback does not repeatedly stat the same source and
proxy only to build a cache key.
When a cached media frame enters a CPU preview fallback, its source/import ->
working-space transform may be lazily materialized once and reused by clones of
that same media-frame cache entry. Encoded RGBA8 decode retains a
`CpuEncodedColorFrame` for the GPU input-transform path. High-bit encoded-float
decode retains `CpuEncodedFloatColorFrame`, while scene-linear RGBA-f32 retains
a shared `LinearFloatSource`; the Viewer uploads either float contract directly
to `Rgba32Float` and executes the matching OCIO input stage on the GPU. All
variants use the same typed source cache, and only materialize a CPU working
frame when software composition or GPU failure requires it. The lazy CPU
working frame is only a fallback
materialization cache and must not replace the source contract or become a
separate color-interpretation path.
Decode failures are also held in a bounded LRU key cache so repeated bad media
does not grow memory unbounded during playback.
Resolved preview plans may reuse a bounded final-frame cache keyed by sequence,
dimensions, deterministic render-plan signature, and resolved media-frame
identity. The final-frame key also includes the effective preview color context,
including the versioned `OutputTransformIntent`, so monitor/output or rendering
transform changes invalidate previously rendered pixels. Unresolved
media requests still bypass this cache until their source frame is available.
This short-lived Viewer raster cache is not the Timeline Render Cache. The
latter persists complete working-linear post-composite frames through the
separate `mondrian-render-cache` worker. Preview performs only nonblocking
lookups/publications, retains at most one verified working hit, and feeds that
hit back into the ordinary GPU Program Output/monitor path as one identity
layer. Cache miss, corruption, queue pressure, or worker failure keeps ordinary
production materialization authoritative. The cache never puts filesystem,
compression, or GPU readback work on the Window/presentation thread.
The product window and renderer context use the same renderer-owned wgpu device
feature contract for native texture formats. Adapter-supported format features
are requested during device creation; every 16-bit normalized plane-view route
requires the corresponding feature. Renderer native-import support carries a
typed decoder-device selector through playback-only preview jobs so hybrid-GPU
systems create decoder resources on the renderer's physical adapter. App
diagnostics continue to
report native import as unavailable until the platform resource-sharing,
synchronization, adoption, sampling, and input-transform bridge is connected.
Device feature enablement alone must never promote hardware decode admission.
The physical `PreviewDecodeSource` has already filtered its native surface hint
through codec/profile/sampling evidence. App then intersects that hint with the
active Renderer device's exact `(handle, format, import mode)` routes and
decoder-device selector observation. Aggregate handle and format counts are
diagnostics, not combinable capability authority. A preferred request may fall back once inside the
media-owned Session; a required-residency request remains fail-closed.
During playback startup, the Preview Adapter schedules future media payloads
under the active priming deadline and recursively checks the next timeline
frame, including nested sequences. The Host forwards ready/available media
lookahead to the Playback Engine after background completions; it does not
advance the clock itself. Current presentation remains a separate one-shot
ticket, and pause/stop/seek continue to invalidate the Playback Epoch
immediately.
The preview service resolves the requested display color space from the active
display-management policy, but the app window owns real surface/display
validation. The window records the final GPU output boundary only after checking
the current wgpu surface/monitor contract, so an unsupported HDR viewer request
cannot silently reuse the SDR surface path. Resize, scale-factor, and move
events refresh that contract; any change invalidates the external GPU viewer
frame so monitor/output changes cannot reuse a texture produced for the previous
display target.
The display snapshot resolves Mondrian Standard's OCIO display/view from the
effective output color space, not from the config's global default: sRGB,
Rec.709, and Display P3 therefore retain distinct display identities. PQ and
HLG targets retain their target display identities and resolve the versioned
`Mondrian Standard HDR 1000 nits v1` View; the app must not substitute the sRGB
Standard View or an ACES View. Surface/monitor validation remains a later,
independent boundary and may still block presentation when the device cannot
carry the requested HDR signal.
Those snapshot strings are validation evidence only. Preview and asset
thumbnail execution pass the typed resolved `output_transform` to
`RenderOutputColorBoundary::from_intent(...)`; neither app path reconstructs
the OCIO boundary from snapshot or optional context strings.
Window display resolution is computed from both the resolved `ColorEngine` and
display policy, and the session retains both identities. ACES and Custom OCIO
defaults are resolved only after their exact engine
config has loaded, and changing the engine alone refreshes the display contract
and invalidates display-dependent GPU preview state. A failed custom config may
not reuse whichever Standard or ACES config happened to be globally current.
The user-level Preferences `显示` page exposes the active engine's qualified
monitor target catalog, exact OCIO display/view identity, HDR policy, ICC
disabled/OS-default/absolute-file selection, and all four ICC rendering
intents. Changes dispatch one complete validated policy, persist it through
`AppUiPreferences`, install it in `AppState`, and trigger Window contract refresh
plus display-dependent Preview invalidation. Startup restores the same policy
before the Preview service is constructed. The dialog projects the complete
latest `DisplayOutputSnapshot`: monitor identity, surface format/color
space/HDR carrier, requested and resolved output, OCIO pair, ICC/HDR status,
warnings, and blockers.

An explicit display/view selected under Standard is accepted only when it is
the package-qualified pair for a standardized target; it never inherits that
display's ACES default or accepts an arbitrary creative View.
Viewer layout exposes a pixel-aligned `ViewerPresentationGeometry` after the
dirty widget tree has been refreshed. It separates the complete sequence canvas
from its visible intersection and derives a stable
`ViewerExternalTexturePresentation` containing output pixels plus normalized
source crop. The app/renderer may use that contract for working-linear spatial
processing; widgets never own a wgpu resource or choose a reconstruction
filter. Spatial external textures render only when their presentation identity
matches current layout exactly, so dock resize and zoom changes cannot stretch
old display/device code values while a replacement frame is prepared.
Both Fit and fixed zoom compute the displayed canvas width from the exact
Sequence sample aspect ratio. Preview Full/Half/Quarter and proxy selection
change only the sampled texture; they do not change this presentation geometry
or the meaning of authored X/Y, anchor, and scale values.
External GPU frame identity also includes the resolved monitor adaptation.
The preview model derives that display-referred identity when it looks up a
registered external texture while retaining the unadapted identity for CPU
raster caches. This prevents valid GPU output from being silently replaced by
a raster frame and prevents display-specific textures from crossing monitor
contracts.
The startup/default window contract remains SDR sRGB unless an explicit display
output intent asks for a different presentation contract. The surface resolver
can choose Display P3, Rec.2100 PQ, or Rec.2100 HLG only when wgpu reports the
matching `SurfaceColorSpace` for a compatible format. SDR sRGB and Display P3
use sRGB-encoded surface formats; PQ/HLG require float or 10-bit non-sRGB
formats so the final color pass, not hardware sRGB conversion, owns the output
transfer. Camera-log and Rec.2020 working spaces are not presentation contracts
and must fail closed until mapped through an explicit display/view transform.
The viewer GPU output path records presentation readiness for each requested
display boundary. If the current surface already matches the requested display
space, recording may proceed. If wgpu reports that a better surface contract
exists but Mondrian would still have to pass the result through the UI external
texture compositor, the path must report a payload blocker instead of switching
the surface prematurely. This prevents false HDR/P3 readiness: real promotion
requires the output texture format, external texture sampling contract, UI
compositor shader, and swapchain color space to move together.
SDR viewer textures use an explicit `SrgbSurfaceCodeValuesOpaque` external
texture contract. The source is an unorm texture containing encoded output or
ICC device codes, not a linear UI image. A dedicated UI fragment pass applies
the inverse sRGB carrier curve before writing the sRGB attachment, whose store
conversion restores the original code values. The pass forces opaque output so
the renderer never alpha-blends nonlinear device codes, and clips normalized
SDR code values only at this presentation boundary. Registration fails
closed on non-sRGB surfaces. This carrier operation preserves code values; it
is not an implicit sRGB color-space assumption or a replacement for OCIO/ICC.
Viewer GPU output telemetry reports display-boundary blockers by reason, not
only as a total. It separates HDR-output-on-SDR-surface blockers from
surface-color-space blockers and stores the last blocked output color space,
selected surface color space, HDR mode, and supported surface-color-space
capabilities. This is the diagnostic boundary for real monitor/surface issues:
the model may request HDR, P3, or log output, but the app window must prove that
the current native wgpu surface can actually present it.
The Window owns that evidence collection, not the health policy derived from
it. `app::viewer_gpu_output_health` owns the shared attempt-outcome vocabulary,
health flags, cumulative counts, and pure classifier; production JSONL,
Headless tests, the budget CLI, and performance gates therefore cannot disagree
on what qualifies as `Ready` or silently downgrade a terminal failure.
Residency classification follows the same ownership rule. The Window passes
platform-probe and renderer-execution facts to
`app::viewer_gpu_output_residency`; it does not infer zero-copy from a planned
native input. Planned and executed working residency are distinct serialized
states, and execution-only counters remain zero until the renderer supplies an
actual completion record.
Native-video capability discovery is sampled once while constructing the
renderer/Window Session. `AppUiHost` no longer performs an independent probe:
hardware-decode admission and every residency record consume the same explicit
snapshot. A new probe generation requires Session/device reconstruction, so a
single frame cannot combine admission from one platform observation with
execution evidence from another.
Telemetry must also expose a stable display issue summary that names the reason,
target output color space, current or selected surface contract, desired surface
contract, payload blocker, and whether the target surface color space is
reported as supported. UI, perf JSON, and diagnostics tooling should consume this
summary rather than parsing Debug-formatted blocker/readiness payloads. The
summary must preserve the display-target fingerprint and surface encoding
evidence end to end, so viewer smoke/budget reports can tell whether a failure
happened on the wrong monitor, on the wrong surface contract, or only because
the current payload path cannot yet present that contract. Contract refreshes
caused by resize, scale-factor change, or moving onto another monitor must also
be persisted as structured events with previous/next surface snapshots so
diagnostics can explain how the current contract was reached. When an issue is
recorded after such a refresh, the issue summary should carry the correlated
preceding refresh event instead of forcing downstream tooling to infer that
relationship from separate records. The refresh snapshots should include enough
capability evidence to explain why the contract changed: surface format set,
per-format color-space support, present modes, alpha modes, and HDR headroom
diagnostics. Health reports derived from these summaries should surface refresh
churn, issue-after-refresh correlation, HDR headroom drift, surface-format set
drift, per-format color-space drift, present-mode drift, and alpha-mode drift
as separate root causes instead of one generic capability-drift bucket.
The same rule applies to media interpretation failures: viewer empty-state
diagnostics and export queue job summaries should consume
`VideoColorDiagnosticIssueSummary` / `VideoColorDiagnosticIssueAggregate`
directly and only use the compact human-readable summary as supporting context.
The viewer GPU-output budget evaluator consumes the same JSONL summary and
replays display issue reason counts plus payload-blocker counts, so smoke tests
can budget real display/surface regressions independently from broad health
status totals. That budget must stay fail-closed per reason as well as in
aggregate, so HDR-surface regressions, surface-color-space mismatches, payload
contract blockers, unsupported presentation intents, and unsupported surface
contracts can each trip their own threshold instead of disappearing inside one
combined display-issue count. Unknown future reason strings must also budget as
their own fail-closed class so diagnostics schema drift cannot hide inside a
temporarily relaxed aggregate threshold. The same JSONL records should also
preserve renderer-owned structured stage evidence (`RenderGpuOutputStageDiagnosticsReport`)
next to any temporary app-local flattened counters, so viewer tooling can
consume one renderer schema rather than rebuilding stage-breakdown models.
Renderer runtime evidence (`RenderGpuOutputRuntimeDiagnosticsReport`) should
travel with the same records so viewer budgets and triage can surface shader
cache extraction or backend-object preparation failures without inventing a
parallel app-local runtime taxonomy.
Playback requests may enqueue a small forward prefetch window, but prefetching is
best-effort: it must not rebuild UI state, block the current frame, or bypass the
generation checks that protect continuous playback from stale decode work.
Current-frame media requests are scheduled before forward prefetch, and the
worker queue/pending set are bounded. When playback outruns decode, obsolete or
excess preview jobs are dropped instead of back-pressuring the UI thread.
If the complete steady window is blank, the scheduler may prewarm only the
nearest visible media Clip activation inside its bounded cold-start horizon.
That activation consumes the existing prefetch capacity and generation; it
does not increase the retained steady-frame count or scan an unbounded gap.
The host supplies the Playback Engine's still-pending demand identity both when
it polls completed work and when it expires realtime work. Polling and
expiration always release scheduler capacity, but only an exact pending
identity may refresh visible playback state, create late-pressure evidence, or
become one `Late` delivery; work left behind after a GPU presentation completed
cannot publish another terminal outcome.
The UI thread must also consume completed background preview results with a
small per-poll budget. Large bursts of completed decode jobs are spread across
event-loop turns so pointer/keyboard/window events keep priority over cache and
diagnostic bookkeeping. Project close cancels queued and in-flight preview work,
clears preview caches/failure caches, and leaves workers alive for the next
project. Application quit additionally closes the preview worker queue and must
not perform a workspace-to-startup native-window role sync on the way out.

Guarded close and quit are asynchronous Window lifecycle operations. Choosing
Save queues the immutable save snapshot and immediately starts the same
Persistence Module FIFO barrier used by discard-close; it never calls the
synchronous Headless save helper. `AppUiHost` retains only the close/quit intent,
while `AppState` owns the exact Session pause ticket and fail-closed state. The
workspace remains paintable while quiescing, but action availability and final
dispatch both reject author/transport/project work, and Host polling does not
apply Import, relink/Component mutation, or Proxy completions to the frozen
Session. The Window merges a 16 ms close-progress deadline with its existing
resource/timer wake deadline, avoiding both an indefinite sleep and an
unbounded `ControlFlow::Poll` loop. Only a successful barrier may switch to the
startup surface or arm process exit. If the barrier protocol faults, Host
reopens the guarded dialog and preserves the close/quit intent. Only an
explicit Discard performs the typed unquiesced detach; Cancel leaves the
Project visible but frozen, and Save remains unavailable until persistence can
be proven. A required save publication failure is different: admission is
resumed, the Project remains ordinarily editable, and no force-discard is
implied.

Preview worker progress is event-driven through one UI-independent,
payload-free work watch owned by `PreviewProductionRuntime`. Media decode,
heterogeneous visual execution, Basic Title raster, and external visual
dependency workers advance the same monotonic revision after their domain-owned
result channel accepts a pollable result. The decoder-residency coordinator
also advances it exactly once when all required retirement acknowledgements
change a blocked admission into an actionable retry, and an RAII worker-exit
guard advances it on normal exit or unwind so terminal channel health is
observable while paused. Result channels and authoritative coordination/health
state remain the sole payload, retry, and terminal authority; the revision
cannot identify a result, prove freshness, complete a Frame Demand, or replace
the bounded Preview pump, and `EventBus` is not used as a high-frequency queue.
The Window Adapter maps the watch to one typed winit `UserEvent`. An atomic
pending bit coalesces an arbitrary completion burst into one native wake and
remains armed through the bounded pump. Rearming compares the revision sampled
before the pump with the current revision after clearing the bit, so a
publication racing the drain cannot be lost; bounded-pump backlog separately
returns `needs_follow_up_poll` and forces another event-loop turn. Host polling
returns typed repaint, candidate-retry, and follow-up facts: an immediate
bounded remainder sets `ControlFlow::Poll` without itself requesting a
whole-window redraw. A merely Pending asynchronous Title/Preview task never
busy-polls because its eventual channel publication owns the next watch edge.
Every Window Viewer GPU submission—ordinary native import as well as a
heterogeneous CPU-prefix/GPU-suffix batch—has one exact completion owner.
Before the CPU prefix enters the visual worker, Preview validates every
prepared route against the same admission decision's upload and conservative
one-command-buffer device-residency grant. A GPU DAG that fits the abstract
value-plan live-set but not the wgpu Adapter's physical non-aliasing recording
set therefore returns to route selection without executing partial pixels.
The complete `PreviewGpuFrame` remains in that owner through actual queue
completion, so Frame Store media-protection leases cannot retire merely because
the renderer retained a physical decoder handle. The move-only renderer output
lease likewise remains either in that submitted owner or in the separate
Window current-physical slot; Preview retains only cloneable presentation
metadata. Every Window registration has a submission-qualified texture key.
`Current`, replacement, timeout, cancellation, and late-callback cleanup must
therefore match the complete `(PreviewOutputKey, texture_key)` artifact. A
semantic key alone is never publication authority, because a replacement may
resolve the same pixels through a different physical submission. Revoking that
exact artifact also replaces an exact matching Ready/Stale Widget projection
with `Loading` in the same Window turn before its renderer registration is no
longer usable; a different submission-qualified texture key is left untouched.

`app::viewer_gpu_device_progress` is the sole native device-progress Module for
both Window and Headless Viewer Adapters. The Adapter reserves a move-only
progress permit before fallible recording and lifecycle admission. Its bounded
capacity is therefore part of admission, never fallible bookkeeping performed
after `Queue::submit`. Once submit returns, the Adapter installs the retained
owner and exact queue callback, then infallibly commits the matching wgpu
`SubmissionIndex` through the permit. The callback owns a separate cleanup
ticket captured before submit.

Exhausting that bounded progress capacity is ordinary retriable backpressure in
both Adapters: Window ends the current preparation turn, while Headless returns
the same typed Viewer backpressure used by presentation-capacity admission. It
does not terminalize the device generation or fail an opportunistic successor.

The dedicated non-UI worker first issues bounded eight-millisecond
`PollType::Wait` calls for that exact index. A wait timeout means “continue
driving this submission”; it is neither semantic completion nor a renewed
publication deadline. `WaitSucceeded` is likewise non-authoritative: if the
shared queue bound the callback conservatively to later work, the worker
continues bounded latest-submission waits until the exact callback marks its
cleanup ticket. Only that callback may stage typed completion in
`ViewerGpuSubmissionLifecycle`, but it never wakes an Adapter directly. wgpu
may invoke work-done before device-lost in the same `Device::poll`, so the
worker re-reads shared generation health after poll returns and emits a
post-poll barrier only for a healthy callback. Until that barrier arrives,
Adapter polling observes deadlines/quarantine without consuming the staged
notice. The worker's shared panic-isolated wake Seam maps to one typed Window event;
Headless maps it to the Preview work revision. Neither product event loop owns
a second `Device::poll` policy for Viewer-submission completion or requests
whole-window redraw merely to make progress. Renderer-owned optional timestamp
polls carry no Viewer lifecycle authority. If recording reports bounded
backpressure before the final Viewer submit, the Adapter converts its reserved
permit into a typed renderer-cleanup barrier; the same worker drives latest
renderer-internal queue work and wakes one retry, instead of issuing an
unindexed Viewer completion poll on the UI/Headless thread.
The lifecycle, device-progress permits, and renderer-resource grant share one
bounded two-owner capacity: the exact current frame plus one immediate
successor. Queue-ordered ordinary output may populate the successor physical
slot before the older callback retires. A current-frame promotion therefore
continues successor preparation while the older owner is still active, but a
third submission is backpressured. Cancellation, Window cleanup, and a device
generation terminal quarantine every active identity; cleanup remains deferred
until each exact callback (or typed device-loss retirement proof) releases its
owner.

Headless realtime coordination may fill that second slot as soon as the exact
current frame has been submitted, including while its completion is still
`InFlight`; it need not serialize successor recording behind current-frame
completion. The successor remains ticketless and cannot publish early, while
the shared two-owner admission prevents speculation from running ahead.

Window and Headless additionally share `PreviewGpuFrameStaging`, a bounded
four-entry CPU-ready queue keyed by the complete playback intent. It warms the
next three coordinates beyond the immediate successor without consuming GPU
output capacity. Staging also invokes the Renderer command-free compact-YUV
preflight, so the renderer-owned worker can fill its mapped transfer buffer
several frame intervals before the exact candidate is submitted; this is
preparation only and creates no GPU output or semantic `Prepared` authority. A
clock jump can take an exact staged entry and bind the new
current Frame Demand only through `PreviewProductionRuntime`, which validates
the execution generation and intent before attaching the frame-local quality
ticket. The queue never creates semantic `Prepared` state, and heterogeneous
Broker work cannot change purpose through this path.
The horizon is populated incrementally, one nearest missing entry per event or
Headless coordinator turn, so its depth does not multiply per-frame decode
admission pressure.

A unique `set_device_lost_callback` is installed immediately after
`request_device`, before any runtime or queue consumer. It can terminalize an
idle generation without a submission identity. A non-timeout native wait error
also terminalizes the complete device generation, rejects every later permit,
and preserves the first typed terminal; a later loss strengthens release
semantics without rewriting first-cause diagnostics. Explicit `Destroyed` and
unexpected loss remain distinct typed causes. Headless
returns that terminal to its caller. Window revokes all output from that device
generation and enters explicit CPU fallback until a device rebuild; an already
submitted owner remains in retirement-only quarantine until its callback or
actual wgpu terminal makes wgpu release safe. Every publication seam checks
generation health, including ordinary queue-ordered publication, and an idle
terminal revokes the generation's already-current physical artifact. Replacing only the native Window/surface
does not create a new device generation: the progress worker, callback
lifecycle, retained media/GPU owner, terminal state, and deferred cleanup move
to the replacement session together. A delayed callback can therefore retire
the old Window submission without publishing into the new Window generation.

One non-renewing five-second lifecycle deadline revokes publication authority
but does not free submitted resources. Timeout/cancellation enters
non-reusable quarantine and defers runtime clear/reset; the device worker keeps
performing bounded waits solely to retire physical ownership. A late exact
callback retires only its quarantined resources, applies deferred
cleanup, and may request one retry; it can never publish. Explicit Adapter
teardown stops admission and transfers one complete retirement envelope to the
existing FIFO progress worker in O(1); the Window/UI and Headless caller never
joins, polls, or cancels submitted work. The non-UI reaper retains device/queue
handles, execution runtime, callback lifecycle, current output lease, timestamp
state, deferred cleanup, and native/media owners until exact completion or a
safe typed terminal. Native D3D copy-ready fences remain independent: wgpu loss
is not decoder-source release proof, while the typed D3D device-removed
sentinel is. Other native errors retain resources fail-closed. Generation
admission is reserved at creation and has a hard process-wide
active-plus-retiring bound of four. Panic/disconnect quarantine retains that
token with the envelope, preventing repeated rebuild from accumulating
unbounded workers or leaked owners. Quarantining a same-semantic heterogeneous replacement
does not clear an older current artifact; only a physical slot carrying the
quarantined submission identity is revoked. Accepted Transparent/CPU output,
display invalidation, and terminal Window cleanup retire semantic metadata,
texture registration, and the move-only physical lease together. If work-done
and device-lost occur in one poll, the terminal wins; the staged callback
remains cleanup evidence and cannot publish. Headless
Adapters observe or wait for the same revision edge, but bound every wait by
the earlier of their next Clock Master tick and presentation deadline.
Headless bounded-pump backlog bypasses that wait and immediately performs the
next drain; a candidate-only retry fact is sufficient to rebuild an unchanged
Loading intent after a completion releases capacity. The performance and
Golden presentation callers also pass one non-renewing outer monotonic deadline
through the Headless presentation coordinator into the same bounded two-owner
GPU submission lifecycle. Ordinary complete-GPU output may become usable after its
queue-ordered publication, while heterogeneous output waits for exact callback
validation. Reaching the non-renewing lifecycle deadline is terminal for
publication authority rather than trapping or renewing the outer timeout loop:
the owner enters quarantine with its visual terminal authority and every Frame
Store media-protection lease intact. A bounded native wait timeout alone never
causes that transition. Its exact late callback is retirement-only and cannot
publish. Headless Preview stores only cloneable output metadata; its
Adapter owns the move-only renderer output lease in either the exact submitted
owner or the separate current physical slot. Every accepted physical artifact
has a submission-qualified resource key. `Current`, queued Ready, timeout
cleanup, and late callback cleanup compare the complete
`(PreviewOutputKey, resource_key)` artifact, so an older callback can never
erase or validate a same-semantic replacement. Accepted Transparent and CPU
Raster outputs clear the physical slot together with semantic GPU metadata.

`PreviewProductionRuntime::diagnostics()` exposes an immutable observation snapshot;
the sibling `app::preview_runtime::diagnostics` Module exclusively owns its typed
decode/render/color evidence models and fail-closed report construction. The
realtime preview implementation records facts but does not contain acceptance
thresholds, root-cause classification, or remediation text. This keeps the
same report Interface available to UI presentation and Headless performance
gates without giving either caller authority over execution. The snapshot
contains render, cache, queue, decode, and scheduler counters so performance tooling can distinguish cache misses,
backpressure drops, stale completions, decode failures, and GPU preview
candidate readiness without changing timeline evaluation. Scrub-adaptive
request counters expose whether interactive seeks are using normal, hot-region,
slow-latency, or recovery policy. Each service call produces
`preview_candidate_id`, and the same id is propagated into `PreviewGpuFrame`
when the frame is ready. Window-level telemetry records this candidate id and
state alongside structured runtime/stage evidence so a JSONL record can be
linked against the exact working-space attempt that fed it.
Window-side Viewer replacement is a commit operation: a previous external GPU
texture remains registered until the replacement output has recorded,
registered, and been accepted by preview state. Typed renderer backpressure
retains that output without blocking the UI thread; subsequent event-loop turns
may retry the current candidate, while superseded candidates are discarded by
normal preview identity rules. This prevents transient native bridge or GPU
queue pressure from producing a blank Viewer.
The private `app::preview_runtime::presentation` Module is the sole application
arbitrator for registered outputs, raster-cache hits, scoped stale
content, deferred playback composites, CPU output transformation, packaging,
and pinning. The production Runtime coordinates scheduling and diagnostics;
`app::preview_execution` remains the sole generation/candidate/registered-output
lifecycle owner. This Locality prevents UI redraw code from reconstructing a
second output-selection policy. Raster cache and stale pinning retain the
UI-independent `PreviewRasterFrame`; the shallow `app_ui::preview` Window
Adapter performs the only conversion to `ViewerFrameImage` and shares the
existing pixel allocation.
External GPU texture publication has no second `app_ui::preview` registration
Interface. The Window Host's ticketed presentation commit is the sole product
Seam; unit-test setup may register prepared payloads only through test-local
helpers that are absent from validation and product builds.
CPU color/composite execution itself is not Window-owned:
`app::preview_cpu_execution` returns the final raster, complete execution facts,
and stage durations. Presentation records those facts into Window diagnostics
before packaging; a Headless Adapter consumes the same Module directly.
`app::headless_preview_presentation` is the shared Headless presentation
coordinator for performance and Golden consumers. It alone orders real GPU
execution, output registration, GPU completion, exact Frame Presentation Ticket
consumption, and preroll observation. Golden's shallow waiter additionally uses
the production presentation arbitrator after an explicit GPU blocker, so a
validated final CPU Raster can complete the ticket without being reported as a
GPU execution. Ticket acceptance is proven only by the typed `Presented`
disposition carrying the exact pending Demand identity and an authoritative
`Ready` or explicitly allowed `Degraded` delivery. Its independent
`transport_changed` fact may be false for a valid paused seek and therefore
cannot stand in for presentation acceptance.
The same rule applies while a pause, seek, or exact-still request replaces the
current frame: `Stale` prefers the last presented external GPU frame for the
same sequence and output extent, then falls back to the pinned CPU raster. A
pending replacement must never demote an already visible GPU frame to an empty
or gray Viewer. Display-contract, sequence, and geometry changes still clear
the external frame explicitly, so stale reuse cannot cross presentation
semantics.
Preview diagnostics keep decode-stage timings separate from post-decode viewer
render timings. Decode reports classify session open, cache lookup, seek,
packet/decode, software scale, RGBA copy, and external-process wait cost;
render reports classify sequence resolution, final-frame cache lookup,
working-frame preparation, CPU timeline composition, CPU output/color boundary,
and final raster packaging. Perf tooling should use both reports before
assigning a slow frame to codec, cache, color, composite, or viewer packaging
work. Final raster viewer keys must be derived from the resolved render-plan
identity, not by hashing full RGBA payloads; large preview frames should not pay
an extra O(width * height) CPU scan just to name an atlas entry. Generated-title
sources use the renderer's typed request-plus-font identity, and nested
Sequences use their resolved child-plan identity with explicit non-reusability
propagation; neither path scans completed working pixels for naming.
Asset thumbnail raster keys follow the same identity rule without weakening
color correctness: the execution cache compares the complete typed source,
working, output, display/view, tone-map, engine, and OCIO-generation contract
alongside Asset identity, path, and complete file revision. An opaque
presentation resource label may be projected from that identity for the image
atlas, but it is never revision interpretation or cache authority. The
app-owned worker performs color transforms before `RasterImage` construction
and only the latest active request for an asset may publish a completion.
Color-context changes clear visible cache state and invalidate request ownership
so stale asynchronous results cannot overwrite a new display contract.
Thumbnail presentation derives its encoded sRGB rendering-View context only
from a validated root `ProgramColorContext`. Context fields cannot be patched by
the Host. If root construction or rendering-View derivation fails, the Host
publishes no thumbnail color context and the request fails closed; it does not
manufacture a colorimetric or engine-mismatched fallback. Preview request and
prefetch setup apply the same rule before work admission.

UI raster images are typed presentation payloads. `RasterImage`,
`DrawCommandEncoder::draw_raster_image`, and `DrawCommand::RasterImage` carry
`RasterImageColorSpace` end to end. The current renderer-owned image atlas is
`Rgba8UnormSrgb`, so it accepts only explicitly sRGB bytes. Unsupported spaces
fail visibly and are reported through `unsupported_raster_color_spaces`, which
the app promotes into frame diagnostics and resource-failure logs. Adding a P3
atlas later requires a separate compatible texture/pipeline path; it must not
silently reinterpret P3 bytes through the sRGB atlas.

Native Viewer surfaces use a separate typed presentation carrier. Production
windows construct `UiRenderer` with the exact swapchain format plus
`SurfaceColorSpace`; renderer-only/offscreen callers cannot register encoded
Viewer payloads against an untyped linear attachment. Ordinary sRGB remains the
single-pass fast path. Display P3, Rec.2100 PQ, and Rec.2100 HLG render UI and
Viewer content into one reusable `Rgba16Float` target-primary display-linear
composition, then run one cached full-screen carrier pass into the swapchain.
UI tokens enter as linear sRGB and use an explicit sRGB-to-P3 or
sRGB-to-Rec.2020 matrix. HDR composition defines `1.0 == 100 nits`; PQ and HLG
Viewer code values are decoded before alpha blending and re-encoded only in the
final pass. Encoded Viewer scaling performs manual four-tap interpolation after
per-tap transfer decoding, avoiding nonlinear PQ/HLG/P3 code-space filtering.

ICC calibration remains a distinct opaque device-code contract. It is admitted
only on a direct sRGB native carrier, uses code-space filtering followed by an
sRGB round-trip carrier decode, and fails closed if combined with a P3/HDR
surface authority. Frame diagnostics distinguish target-transfer batches from
ICC/device-code batches and report whether the wide-gamut/HDR carrier ran or
reallocated. Composition/MSAA textures, the carrier bind group, and the carrier
pipeline survive steady frames and rebuild only on presentation-contract or
size changes. Batch construction also resets texture authority when returning
from an external/raster image to an ordinary UI primitive; a shape can never
inherit the preceding Viewer pipeline.
The application-layer `PreviewRasterPresentationContract` resolves the CPU
boundary before any Widget object exists: Rec.709, sRGB, and Display P3 SDR
requests are monitor-adapted to an sRGB atlas payload; PQ/HLG requests fail
closed until an explicit SDR tone-mapping policy exists. The Window Adapter then
maps the proven encoding to `RasterImageColorSpace::Srgb`; the widget layer never
tone-maps or relabels bytes. GPU Viewer candidates retain the original
monitor/output contract and continue through the native output/surface
validation path.
Preview Viewer-plan lowering treats only clamped zero opacity as
non-contributing. Every positive Float32 opacity, including sub-16-bit values,
must produce the same layer/adjustment/Transition obligation as CPU execution;
the App cannot apply an epsilon cull before Renderer admission.

GPU preview candidate counters are intentionally scoped to the headless service
boundary: they prove that a working-space frame was produced for the app-window
GPU output path, not that wgpu presentation recording succeeded. The app UI scale
smoke test
serializes a preview diagnostics probe into its JSON report and includes a
separate preview-playback refresh case without changing the existing UI-only
refresh benchmark paths.
`preview_media_decode_cache_smoke` extends this coverage with a generated
FFmpeg fixture and exercises real media import, decode readiness, cache-hit
refreshes, sequential-frame preview readiness, and a GPU preview candidate probe
as an ignored/manual perf probe.
`preview_media_continuous_playback_smoke` uses the same generated media path to
simulate a 30fps playback window and records `Ready`/`Loading`/`Stale`/
`Unavailable` counts. Its post-window GPU candidate probe explicitly settles
transport, then drives the production Headless execution, GPU completion,
output-registration, presentation-ticket, and preroll path to a usable output
before releasing decoder residency. It never uses the Window presentation
projection to manufacture a synchronous CPU raster. Steady playback must keep a
current or stale frame visible instead of falling through to an unavailable
viewer.
The external-media variant treats current-frame readiness as a basis-point
contract (99.50% by default), requires complete hardware timestamp coverage for
every newly rendered frame, and reports hardware GPU, CPU record/submit,
completion-wait, and total wall p95 independently. The headless adapter's
16-slot timestamp ring submits and maps queries without a per-frame wait; the
offline gate drains once after playback. Ring saturation discards telemetry and
fails timestamp coverage instead of back-pressuring the measured scheduler.
Production window and headless Viewer device creation also request optional
32-bit-float filtering when supported, so OCIO LUT interpolation uses the same
wgpu feature contract in interactive presentation and real-media gates.
The same report attributes hardware duration across working composite, spatial,
output-boundary, and optional display-calibration stages using ordered encoder
timestamps. This attribution is distinct from CPU stage preparation timings and
lets the gate localize a GPU regression without inserting a per-stage queue
submission or CPU/GPU synchronization point.
Continuous-playback decode failure extraction is scoped to PlaybackCursor plus
global fatal scheduler/worker failures. Random-access still and scrub latency
remain visible in the full diagnostic report but cannot fail a playback-only
gate; their dedicated probes own those budgets.

The external dual-video smoke gate authors two overlapping Source-Full video
tracks from one real source while slipping the second source by one frame, so
every Timeline frame requires two distinct media identities. It drives
continuous playback, cross-region seek, pause/seek/resume, and Viewer resize
through the production Headless coordinator. Passing requires exact Ready
observations, two media-layer executions per rendered frame, native GPU
compositing with no passthrough/readback/fallback, complete GPU completion and
publication evidence, and zero native decoder sources after the final active
submission retires. Native sources retained while the immediate successor is
still submitted are reported separately as bounded pipeline residency rather
than misclassified as completed-owner leaks.

Source-Full real-media qualification also owns an interleaved playback/resize
gate. It drives the production `AppUiAppRoot` and `ViewerSurface` layout through
multiple ordinary desktop window sizes while the production Preview runtime and
headless Viewer GPU adapter continue advancing the same transport epoch. The
gate requires more than one resolved presentation extent, valid visible geometry,
forward progress, exact Ready observations, unchanged authored Sequence output,
and only the authored Full GPU extent in its resize-scoped execution summary.
Window layout therefore remains presentation policy: it may change crop and
presentation pixels but cannot mutate Program resolution or restart playback.

Viewer models consume an explicit preview readiness state. `Ready` frames are
current, `Loading` means the requested frame is queued/in flight, and `Stale`
means the viewer may keep the last ready frame visible while the current frame is
prepared or a failed media key is protected by the bounded failure cache. Stale
frame reuse is scoped to the same sequence and preview dimensions. These states
are presentation/adaptor semantics only; they must not mutate timeline playback
state or affect export evaluation.
`Transparent` is also exact and Ready: it means the active Sequence produced a
valid transparent Program canvas without a texture. The Viewer paints that
canvas using the persisted presentation-only checkerboard/black preference,
shows no diagnostic empty-state text, and never writes the chosen background
into Program pixels. `Unavailable(NoContent)` is reserved for the absence of an
active output target; `Blocked` and `Failed` remain warning/error states.
When the preview service returns `Unavailable` because color management rejected
media, the viewer model must consume `ViewerPreviewColorRejectionModel` instead
of showing a generic empty viewer. The status should remain warning-toned and
the empty message should include the rejected media path, missing-metadata
policy, input-resolution branch, and media diagnostic summary. This keeps
fail-closed color behavior visible without scraping tracing logs.
The native app entrypoint owns a four-thread Tokio runtime for background UI
work. After the event loop exits, the runtime is shut down with a bounded
timeout rather than dropped normally: Tokio's default runtime drop can wait
indefinitely for blocking tasks and leave a headless Mondrian process after the
window has closed. Background operations must therefore treat cancellation as
cooperative and may not rely on an unbounded runtime drain during process exit.
The same entrypoint owns the process-lifetime Product Logging Module. It always
installs a stderr formatting layer and, when the Platform User State Directory
Adapter is available, a non-blocking daily JSONL writer under
`Mondrian/logs`. The writer retains a bounded set of matching files and its
guard is dropped only after ordinary runtime shutdown so queued terminal events
are flushed. `RUST_LOG` changes filtering only; callers do not own paths,
rotation, writer threads, or retention policy.
Once action draining produces a quit command, the host returns it immediately;
it must not refresh or lay out the widget tree after the preview service and
project state have already begun shutdown.
When the host begins a confirmed quit (after any unsaved-work decision), a
two-second process-exit watchdog gives preview, project, runtime, and GPU
resource destruction a final bounded opportunity to finish. A quit command is
not followed by another native redraw or window-role synchronization, avoiding
a redraw/destruction race on the exiting window. If a platform media, audio, or
GPU driver blocks closure destruction, the watchdog uses the platform's
no-destructor termination primitive (`TerminateProcess` on Windows, `_exit` on
Unix) if application-level cleanup does not return in time;
`std::process::exit` is deliberately not used because DLL detach hooks can
deadlock on locks held by terminating worker threads. Immediately before that
fallback, the watchdog emits a terminal product-log event, allows one bounded
writer interval, and exits with a nonzero forced-termination status so a driver
hang cannot masquerade as a successful shutdown. The watchdog is armed
only after the guarded unsaved-work decision and immediately before bounded
preview/project cleanup begins, so it cannot bypass save/discard/cancel
semantics but still bounds a cleanup call blocked in a third-party runtime.

The Export workspace projects the selected Sequence's Dynamic HDR Program
state without duplicating it into the export preset or a shell-local settings
draft. Its intent dropdown emits only the typed `ui.dynamic_hdr.apply_edit`
Product Action and is writable only for the active Sequence. It exposes Omit,
exact whole-file preservation by detectable metadata family, and Remake for an
existing analyzed Program. Readiness text names ST 2094-40 Application #4
without implying HDR10+ certification and states that Remake requires a
qualified/licensed Adapter plus independent validation and human QC. The App
commits every action through the normal Authoring Session, so availability is a
read-only early projection and dispatch revalidates the complete Sequence.

Professional Reference Output routing is a machine-local preference and App
runtime service, never a ProductAction or `.mdp` author field. Preferences may
remember optional provider/device identity, carrier, and external-reference
policy, but loading them never acquires hardware. The App exposes explicit
discover/open/schedule/start/poll/stop Interfaces, binds an open Session to the
exact Sequence revision and Project author generation, and stops it on edit,
navigation invalidation, Project close, device loss, or required reference
loss. See [Reference Output](reference-output.md).
