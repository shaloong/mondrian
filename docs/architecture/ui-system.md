# UI System

Shared Rack controls attach explicit semantic inspection identity to Slider,
Checkbox, closed Dropdown, Button and IconButton views. The Widget contract
carries identity, compatible role, focus presentation and owned keyboard release
intent. Shell restores unambiguous
keys within the same Panel and live author Session/Sequence; the event Router
remains the sole focus and modal-scope authority. Rack keys include the exact
Scope occurrence, Processor ID and native definition binding, parameter ID and
control role. Translated labels, row ordinals, current author values and query
operations never form identity. New model callbacks, values and schemas replace
old presentation; deleted, disabled or rebound owners keep fresh identities.
Pointer capture, drafts, raw press state and menu paths are never restored by this
seam. Specialized CurveEditor geometry/key selection remains its own inspection
snapshot under the same author-control scope guard.

Clean source `718696e` qualifies the shared source menu models and their product
actions against actual CLAP/VST3 PCM, one Undo/save-live-Redo and four Unicode
archive reopens. Source-gain curves are retained by rewiring; their complete Rack
editing UI and the physical Host/Scope pointer matrix remain separate work.
See [current workflow evidence](../dev/modal-resource-lut-acceptance.md).

Shared Rack retained-input rows show the current source and a localized source
selector. Options require the current Ready native observation, an editable old
source, a matching stable input key and canonical stage-cycle/source-lock
admission. Missing ports remain visible and removable; query cancellation or
invalidation clears replacement options. Choosing a source carries the retained
Route ID and expected old source through a closed observed rewire action.

Shared Mixer and Inspector Rack input controls bind native labels through the
current cached input observation. Stable bus keys remain the connection payload
and persisted routing identity. Repeated effective labels include their bus key;
unnamed or unavailable retained ports use that key directly and remain removable.
Locale changes and repeated model reads use cached preparation facts without
native discovery or processor creation. Labels never mutate authoring state.

Inspector enum dropdowns consume definition-owned stable option keys. The
Dropdown widget supports non-action separator rows. Video Clip blend modes
project Core's typed option order into a localized Inspector dropdown, with
UI-only boundaries between inherit/base, darken, lighten, contrast,
difference, and component groups. The checked item is the Clip's explicit
override or Track inheritance; it has no edit action. Choosing another item
dispatches one typed Clip action, and a locked Track disables the control.
Neither persisted values nor parameter schemas contain presentation rows.
`Dropdown::set_model` updates labels and options while preserving widget
interaction identity.

The shared Curve Editor may attach per-point context commands. Inspector
opacity keys expose interpolation presets there; virtual Clip-boundary anchors
have no key identity and no menu. UI actions carry stable key IDs and resolve
the current author time at dispatch, so a stale menu cannot silently edit a
different key after the curve changes.

Typed audio handle actions use sparse exact coordinates through the existing
product action codec and author session. The App publishes one history entry,
retains no-op generations, rejects stale keys without publication, and persists
the canonical Continuous geometry through durable `.mdp` save and reopen.
Direct handle gesture drawing and hit routing are a separate widget integration;
the typed author action does not imply that those controls are already exposed.

The status bar remains the persistent summary of current work. Its status
history is available by clicking the bar; the App retains at most 64
messages and the popover shows eight at a time with wheel navigation. Active
export progress cannot conceal a newer error status. This gives professional
users a quiet, recoverable message surface while editing. The App emits
language-neutral terminal notification facts for completed media imports,
manual saves, Exports, portable Project packages, and their failures; Autosave failures and failed Timeline
file placement are also eligible. The UI formats them with the current Fluent
locale and shows at most two nonblocking toasts. Success, warning, and error
toasts expire after 5, 8, and 10 seconds; clicking one dismisses it. Stable
operation identities replace duplicate visible toasts, and an authoring-session switch
clears stale toasts. The existing bounded status history retains details.
Do not toast every successful click or progress tick. A notification never
owns a Project mutation, a worker, or the sole record of a failure. An
expandable history panel exposes status history to users; the EventBus remains
a post-commit notification seam, not notification state.
See [localization](localization.md) for message formatting and locale ownership.
The Effect Browser builds a locale-specific label projection from the registered
effect definitions. Its category tree IDs remain canonical across locale
switches, so expansion state and add-to-clip actions retain their identities;
the browser does not persist translated names in authoring data.
The Asset Browser likewise projects translated card badges, folder navigation,
and context menu labels from a single library snapshot. File and folder names
remain user-authored text; their stable IDs and menu actions do not vary by
locale. The model carries prepared menu rows for the current locale, including
the dynamic selection-delete label.
The generic `AssetGrid` widget accepts formatted empty-library and no-results
copy from the App so it never has to resolve the product locale itself.
The generic Viewer receives a localized Fit option label from the App. Frame
refresh and pending transport status use the window's retained locale snapshot;
Viewer gesture actions still carry stable mask and transform parameter IDs.

`File → Export → Package Project` opens a `.mdpkg` destination dialog. The
App takes an author snapshot, then performs dependency preflight, file copying,
hashing, and atomic publication on a cancellable background worker. The status
bar shows bounded copy progress and the menu exposes cancellation while work
is active. Cancellation and failure clean staging without publishing a partial
package; completion or failure emits one localized terminal notification.

An external file drop is target-specific. The Asset Library accepts a library
drop; the Timeline accepts a valid unlocked Track/body position; a canceled
drag or a drop outside an accepted target performs no import. The Window must
not reinterpret an ignored `Drop` as a global media-import command. The
Timeline widget must retain the target Track ID and exact Sequence frame in
one typed action and only dispatch after release on a valid target.

Timeline file placement uses the bounded media-import worker for isolated
probing but requests a deferred result: the worker cannot commit an Asset.
At drop release the Widget maps the track body and frame into a typed action;
the App rechecks current Track lock, exact frame grid, source fingerprint,
media kind and complete Clip edit after the probe. The Asset Library stages
the new or refreshed row in a SQLite transaction and supplies its stable
`AssetId` to a prepared, validated Authoring Session edit. A failed edit rolls
back the SQLite row, including an existing Asset's folder and metadata. Once
  SQLite commits, the prepared author edit installs while the Library lock is
  still held, without another fallible validation or allocation step; only then do Clip/Asset notifications and
post-commit proxy scheduling run. Existing Assets keep their folder placement.
The Library fences saves captured before this coupled publication, so an old
Project document cannot be archived with the newly placed Asset. A process
restart opens a fresh Library generation from the last published `.mdp` or
recovery snapshot, leaving an interrupted unsaved operation behind. The
Window reads the current native cursor at Windows file-drop delivery because
OS drag events need not carry winit `CursorMoved` events. Cancellation or a
missed body dispatches no import at all.

The first native Window/Surface/Renderer candidate uses the existing Host mode,
through the same role mapping as subsequent window synchronization. A Host with
an already-open Project therefore starts directly with a Workspace carrier;
it does not allocate a Startup carrier only to replace it on the first event
loop turn. A Host without a Project still starts with the Startup carrier.
Both paths keep the original pre-active construction owner, deadline, wake
registration, activation, and partial-start cleanup. Opening or closing a
Project later changes the role on that same native Window and Surface. The
Viewer device progress owner, execution runtime, submission callback receiver,
and deferred cleanup stay in the retained session. Publication retirement and
display-contract invalidation still use their existing completion gates; a role
change is not a device or surface generation change. Ticketless CPU staging
frames from the retiring project are released immediately; submitted GPU owners
still wait for exact completion. Only explicit validation
surface/device recovery constructs another native carrier.

Startup is transparent and fixed-size; Workspace uses the self-hosted chrome
and permits native edge/corner resizing. Role changes clear native IME, tooltip,
eyedropper and router focus/capture state, then publish and lay out the new root
even if the OS has not changed its size yet. Display diagnostics distinguish
RoleChanged from Resize and ScaleFactorChanged. Workspace retains its normal
logical size and maximized flag across project close/open; maximized and
minimized extents cannot overwrite the normal restore size. Startup always
leaves minimized/maximized state and applies equal minimum/maximum dimensions.
The fixed Startup maximum is removed before increasing the Workspace minimum.

Empty and model-only UI construction must not create an `AppState`. A disabled
empty Timeline has explicit unavailable edit flags, and a model-only shell has
a plain status placeholder with no claimed Project context. Only the adapters
receiving the existing `AppState` project live command availability, Project
context and Export status. Constructing a temporary application merely to obtain
default view data would start Audio workers and discard their owners outside
the Host lifecycle. Regression tests count calls to the real App constructor on
the test thread and require both UI factories to create none.

The acquired surface texture owns each UI frame's physical attachment extent.
`AppUiFrameRenderer` uses that extent for MSAA, resolve, carrier targets, and
viewport uniforms; callers cannot supply an independently sampled window size.
On X11 a native resize can become observable before its resize event updates
the configured surface. Combining that newer window size with the older acquired
texture would create incompatible attachments. An acquired extent that disagrees
with the configured surface is dropped before reconfiguration and returns the
existing typed `SurfaceOutdated` follow-up instead of recording or presenting it.

Preview's native wake Adapter installs its coalesced EventLoopProxy callback
through the Work Watch registration owner. Failed admission returns the original
reason and unaccepted callback to Window; it is not silently treated as a working
wake path. Callback invocation remains synchronous, while final destruction is
owned by one bounded retirement worker, never by a producer holding ownership
locks. Explicit Preview shutdown includes its raw callback receipt under the
original operation deadline. See [endurance ownership](commercial-endurance-qualification.md)
for panic, rejection, timeout and evidence semantics. This does not qualify native
event delivery or replace pending Window startup/reopen inventory work.

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
The Interpret Footage modal captures the shell's selected locale when opened.
Its controls, auto-detection status, RAW choices, signal evidence, and OCIO
diagnostics use the App-owned Fluent catalog; only canonical color-space names,
technical IDs, file names, and raw decoder values remain untranslated. The
draft still contains author intent and media evidence only, so changing UI
language cannot change the asset interpretation committed on Apply.
The Inspector's audio Component mapping rows use the same panel locale
snapshot for matrix policy, layout evidence, and fail-closed diagnostics.
Localization changes only the projected labels; all matrix coefficient edits
continue to dispatch the typed placement-local Component mutation.
The product action's external encode/decode and live-library rejection path
are tested inside the App crate's isolated test runtime; they do not acquire
production per-user Project leases.

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

`CurveEditor` is a normalized interaction Module, not an automation owner.
Its complete-value and non-live callbacks keep pointer edits local until release;
its temporal gesture callback publishes Begin/Edit/Finish/Cancel with live
canonical author updates and one history entry. Keyboard edits are atomic.
Move/Delete carry CurvePointTarget and handles carry KeyframeId, independent
of the current display slot. The Inspector Adapter resolves these targets to
`AnimationParameterAddress + KeyframeId` before dispatch. Evaluated
Hold/Linear/Bezier samples are a separate read-only paint series, so display
fidelity cannot rewrite interpolation. Point metadata keeps identity, policy
and context commands together. Virtual boundaries have fixed time and editable
value; real author keys remain movable and deletable even at viewport edges.
Escape restores widget projection and cancels the canonical author gesture.

Scalar floating-point visual Effect parameters use this same curve editor and
stable-key Product Action path. The Inspector projects the definition-owned
numeric range, exact current Clip-local key identity, and evaluated samples;
the diamond beside a value toggles a key at the exact author time. A curve row
appears once the parameter has authored keys. Inserts, moves, removals, and
interpolation presets resolve through the Effect instance's stable parameter
address and commit one Sequence transaction, preserving Undo/Redo and Track
lock semantics. Auto and continuous Bezier constraints are evaluated by Core;
the Widget owns only gesture state and normalized coordinates.

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

`MultilineTextInput` exposes the same mutually exclusive live/transactional
policies. Transactional multiline inputs retain ordinary Enter for newlines;
Control/Command+Enter, focus loss, or form submission confirms the changed
focus-session value once. Escape and programmatic disable cancel the draft;
programmatic text replacement establishes a new confirmation baseline. Active
IME composition owns its Enter/Escape keys and blocks form confirmation. Generic
Inspector plugin Text and Resource controls use confirmation, initialize actual
editable values, and never disable long Unicode text based on its byte length.
Multiline property rows reuse content-height measurement so their edit regions
fit the declared line count and do not overlap following controls.
The existing Basic Title live Viewer text path remains live; continuous caption
history coalescing is still a separate author-session requirement.

Inspector parameter presentation is a locale projection over canonical schemas.
Declared schema label and enum message addresses resolve through Fluent catalogs;
owned built-in, Title, and Mask logical addresses have one validated catalog mapping.
Vendor schemas can declare a direct registered message address or retain their
display fallback. Enum Actions and current-item checkmarks use exact canonical
option keys; group adjacency
uses the original definition-owned identity independently of localized headings.
All registered built-in parameter labels/options/groups are catalog-qualified in
English, Chinese, and pseudo locale. Vendor property and group labels retain their
declared fallback. The current enum schema has no independent vendor caption;
unknown enum message addresses therefore fall back to their canonical keys.
Locale changes do not mutate parameter schemas, author generations, or history.
Qualifier sample operations and add/remove controls receive the same captured
Localizer; localized labels still dispatch validated typed sample sets and the
original stable owner/parameter address.

Widget text measurement uses one thread-local owner shared by ordinary
Label/Button layout and TextInput/NumberInput geometry. Numeric-control
construction can already measure text before the first layout pass; it must
not create a second system-font catalog and measurement atlas. The shared
module retains the same cosmic-text shaping and font fallback. Its exact-size
seam preserves TextInput's caller-supplied size, while ordinary label
measurement keeps its existing minimum-size policy. This is measurement-owner
reuse within one UI thread, not a process-global lock or a change to the
separate text rasterization owner's lifetime.

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
`Dropdown::set_label` changes only trigger copy and retains the current open
submenu and focus path. `set_model` replaces the row model and clears row-index
interaction state. `localize_item_labels` updates keyed rows recursively without
clearing that state. The menu bar lays out its retained Dropdowns again after a
model refresh so translated trigger widths receive correct hit regions.

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
For native audio plugins, it displays the captured plugin parameter name and omits parameters
marked hidden, while retaining every parameter in Timeline author state and
native execution. Parameters without a plugin display name show their stable
IDs; rebinding preserves the existing display snapshot, while new insertions
capture current labels.
It deduplicates Clip bindings by Processing Scope, consumes Timeline's binding
count and lock blocker, preserves unknown plugin definitions and parameter
schemas, and generates only typed Rack Actions. Inspector and Mixer render the
same Rack sections and action factories; neither traverses audio author state to
reconstruct admission. Numeric controls take hard/soft range, step, unit, value
type, and animatability from `ParameterSchema`. An already-keyed curve is shown
as automation and its fallback value is deliberately not exposed as though it
were the playhead value.
The insertion dropdown projects built-ins and session-installed CLAP/VST3
definitions through the same Rack address, then offers CLAP file and VST3 file
or bundle folder pickers through `PlatformService`. Dialog cancel is a no-op;
a selection is scanned in its format's isolation helper and atomically
published to the App catalog. The UI does not load native code. The Host
persists selected format/path pairs in
machine-local preferences and retries them on one background worker after
startup. The UI refreshes Rack menus as discoveries arrive and reports missing
plugins in the status area. The folder picker resolves only a `.vst3` directory
with one native binary for the running architecture; the file picker retains
direct binary selection. Both paths enter the same product action and
machine-local preference list.
Rack and Mixer projection require the App's current Fluent locale snapshot.
Built-in processor, parameter, ownership, route-port, empty-state, and lock
copy changes with the locale, while plugin-supplied names, typed Rack/Route
addresses, and authored values stay unchanged. The Inspector and Mixer consume
the same localized Rack projector; neither persists translated labels.

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

Keyed audio point context menus expose Hold, Linear, manual Bezier, Auto Bezier,
and Continuous Bezier only when the target schema admits that interpolation
family. Menu actions carry stable key identity into one audio authoring
transaction; the widget never computes or owns tangent values.

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

Monitor direct manipulation emits `ClipProductAction` parameter writes with
absolute Sequence pixel or Clip-local values. `AppState` remains the single
mutation owner: it validates stable addresses and payloads, checks Track locks,
applies Timeline property mutations, and records one undoable snapshot for each
committed monitor edit.
The selected visible video Clip projects its evaluated position, scale, rotation,
and source-pixel anchor through the same stable intrinsic parameter addresses as
Inspector. The Viewer draws the transformed source-frame outline, corner scale
handles, a rotation handle, and an anchor crosshair. File-backed media uses its
resolved display extent after orientation and placement-local pixel-aspect
interpretation; generated content uses its owning Sequence extent. The canvas
still maps positions through the Sequence extent, so mixed-resolution Clips
retain accurate hit targets and scale geometry. The anchor has hit priority
when it overlaps a corner. Pointer motion changes only the widget preview;
pointer-up dispatches one `ClipWriteParameterValues` action. Anchor dragging
inverts the current rotation and scale to find the new source anchor and writes
the compensating Sequence position in the same author transaction, preserving
the displayed image. Escape, capture loss, a locked/hidden Track, playback,
disabled or non-current Clips, and Mask editing cannot publish a partial Clip
transform. Viewer zoom and sample aspect ratio only affect screen mapping, not
the persisted parameter values.

Selected Power Windows expose rectangle, ellipse, and Bezier control handles in
the same Viewer. Alt+click on a visible Bezier segment inserts one anchor by
splitting the cubic at the picked parameter: the existing outline is unchanged
until the user moves that point. The gesture emits one complete Mask shape
action with no intermediate author mutation or pointer capture. A miss, locked
Mask, existing anchor hit, or path at the point limit inserts nothing.

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
The Waveform startup Module installs each returned source-cache/analysis owner
before the next startup step. Endurance uses its owning `try_start` Interface
and retains a separate partial-stage receipt through failed setup. The product
Host constructor now uses that same owning Interface; an ordinary OS thread-spawn
failure still publishes the existing degraded complete service, while an unwind
returns the actual partial owner to the Host startup transaction. This does not
qualify Window's separate old/candidate GPU or event-loop owner inventory.
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

An Export enqueue Product Action may carry one complete versioned
`BroadcastQcProfile`; App freezes it into the same immutable job rather than
keeping a UI-only threshold bag. Terminal rows project the report's profile
edition, verdict, analyzed range, total/retained/overflow findings, completion,
and external-obligation count. App does not interpret or recompute QC. The
local panel deliberately has no generic broadcaster-default toggle: an approved
profile must arrive through deployment/profile management, while the external
Product Action already provides the typed product Seam.

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
six-entry CPU-ready queue keyed by the complete playback intent. It warms the
next five coordinates beyond the immediate successor without consuming GPU
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

The same generation health owner handles `on_uncaptured_error`: allocation,
validation, and internal wgpu errors reject future publication and wake the
existing progress domain instead of invoking wgpu's default panic handler.
These errors preserve the first terminal cause but do not prove that previous
submissions retired; only their actual completion or a later device-loss fact
can discharge that resource obligation. This does not make allocation failure
a successful render or start a replacement device implicitly.

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

Validation-triggered Surface/Device recovery is a different consuming
operation. The Window session assigns checked nonzero process-local identities
to every concrete Surface and progress/Device generation. After an actual
external-texture Viewer frame is presented, the event-loop owner first prepares
a complete candidate Window, Surface, Device/Queue, frame renderer, progress
worker, and Viewer execution runtime without changing Host-visible display,
native-import, or CPU-fallback state. It validates distinct Surface and Device
generation identities before revoking old publication. Only then does it
transfer the complete old runtime/lifecycle/native-owner envelope to its
existing progress worker and wait within the original explicit bound. A clean
return requires both Adapter-specific retirement and a successful whole-queue
wgpu wait; concrete device loss/destroy may replace the queue fence for release
but is not accepted as a clean qualification receipt. Candidate preparation
failure consumes its real partial GPU owner under that same deadline. After
activation, the candidate is installed in the outer Session before old
publication is revoked and the old Window is hidden. The now-local old Session
is then retired; only a clean old receipt permits Host publication and display
of the candidate. Any retirement or post-install publication error/panic leaves
the candidate in the outer Session for bounded explicit closure, so ordinary
asynchronous `Drop` is never qualification evidence. The operation seals
success only after that new Surface presents
the same active Sequence/frame/color/display contract. A Headless surface, a
successful `configure`, a reused Device generation, or a CPU-only raster is not
Surface reopen evidence.

Initial Window startup likewise calls the owning `AppUiHost::try_new` seam. A
Host failure is consumed against the Window's existing absolute deadline and
returns the same App in validation; its original diagnostic remains primary.
Once a partial Viewer GPU generation exists, waker rejection, renderer/runtime
preparation failure, or activation failure performs bounded product cleanup and
retains its concrete progress/renderer receipt. Native role replacement prepares
the new Window/Surface/UI shell before hiding the old shell, then transfers the
unchanged four-part Device-generation authority exactly once.

Once an active Window session is published, the event-loop callback borrows its
`AppUiHost` and `AppUiWindowSession`; it does not own either one. Normal return,
an event-loop error, and caught callback panic therefore converge on the same
outer close path. That path first revokes Viewer publication and consumes the
final complete GPU generation through the existing progress Module, retaining
the raw worker/Renderer receipt. It then consumes Host Preview and auxiliary UI
services and returns the exact validation `AppState`, with every wait spending
from the operation's unchanged absolute deadline. A rejected or failed reopen
therefore cannot turn a successfully published final generation into an
asynchronous-Drop success claim. Ordinary product Quit only begins service
shutdown; this outer Window transaction performs the sole bounded wait.

The narrow standalone runner consumes the returned state through
`shutdown_for_endurance`; a higher-level campaign runtime may instead resume
its settled persistent Timeline owner. Returning a successful receipt while
the final GPU generation, Window Preview worker, Basic Title task,
visual/CPU-fallback, render-cache worker, or Waveform owner detached is
forbidden. The Host guard's Drop path remains fallback coverage for failures
before an active Window session exists and is not qualification for a setup or
publication panic.

Winit's event-loop creation authority is process-local, so endurance validation
owns one non-`Send` event loop on the binary main thread and uses the desktop
on-demand lifecycle for each orthogonal recovery session. Every invocation
creates fresh Window/Surface/Device/Queue state and drops it completely before
control returns; only the exact `AppState` and campaign recovery pump cross the
boundary. The same owner is then re-entered for the next recovery cycle. A
standalone batch runner exercises multiple cycles in one process so a second
cycle cannot regress to `EventLoopError::RecreationAttempt`. macOS main-thread
and Linux display-server availability remain native transfer gates.

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

Offscreen renderer tests execute this same `UiRenderer` against a real wgpu
device. Their harness owns one physical device lifetime at a time because the
product UI owns one renderer device; parallel test-only device construction and
destruction is neither production topology nor portable across Vulkan drivers.
The owner guard is released only after the harness texture, renderer, queue, and
device have been dropped.

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
The native app entrypoint owns background UI execution through the
`app_ui::background_runtime::AppUiBackgroundRuntimeOwner` Module, not a bare
Tokio Runtime. One native supervisor thread constructs and exclusively retains
the configured four-thread Runtime;
the Window receives only a clone of its Handle and enters that context. Shutdown
drops the UI Handle, signals the supervisor, and consumes its real thread handle
with the existing bounded owned-worker join protocol. Only a returned supervisor
after Runtime destruction qualifies clean. A timeout, panic, current-thread
join is typed dirty evidence. Fallback Drop signals shutdown and detaches the
supervisor without producing a receipt, so it remains unverified and cannot
qualify. This prevents Tokio's default Runtime Drop from waiting indefinitely on
the Window thread and does not pretend that `shutdown_timeout`'s unit return is
a worker-terminal receipt. Background operations must still treat cancellation
as cooperative.
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
The external codec, atomic authoring, Undo/Redo, and Export UI projection are
covered in the App crate's isolated test Session so tests never acquire the
production per-user Project authority namespace.

App-level Gallery, Grade, Reference Output, and timeline-interchange contracts
likewise run inside the App crate's isolated test runtime. They exercise the
real Project transaction and device/session APIs, including create/close where
needed, without claiming the production per-user authority namespace. The
production lock location remains stable across processes; tests do not redirect
it with an environment override.

Professional Reference Output routing is a machine-local preference and App
runtime service, never a ProductAction or `.mdp` author field. Preferences may
remember optional provider/device identity, carrier, and external-reference
policy, but loading them never acquires hardware. The App exposes explicit
discover/open/schedule/start/poll/stop Interfaces, binds an open Session to the
exact Sequence revision and Project author generation, and stops it on edit,
navigation invalidation, Project close, device loss, or required reference
loss. See [Reference Output](reference-output.md).

## UI auxiliary execution closure

Host owns Thumbnail and audio-device discovery in addition to Preview and
Waveform. Ordinary quit and validation begin all auxiliary shutdowns before
waiting, and share one auxiliary consuming implementation with one absolute
deadline. Validation retains each original auxiliary receipt alongside Preview;
one clean worker cannot hide another worker's panic or timeout. Thumbnail's
concurrent-close `LifecycleBusy` outcome is retained as an explicit failure to
obtain a receipt, not normalized to a guessed NotStarted inventory. Ordinary quit
keeps its existing asynchronous Preview policy and process-exit watchdog; that
policy is not equivalent to complete validation qualification.

The audio-device catalog retains the actual native handle and bounded cumulative
startup/join/failure inventory across refreshes. Polling joins only a worker
already known to have returned, never waits after result publication, and never
destroys opaque panic payloads on the UI stack. Native enumeration cannot be
portably canceled: shutdown revokes publication and records failure if the
worker is still live at the original deadline. Production-required initial
discovery and deliberately test-disabled discovery remain distinct.

Host construction is an owning transaction in `app_ui::host::startup`. It takes
the unique `AppState` before resolving the preferences path, loading preferences,
changing process theme, or preparing a service. The default path is resolved once
and the same path is loaded and retained. Thumbnail, Waveform, Preview, and catalog
owners are installed before the next checkpoint; Waveform and Preview retain
mutually exclusive partial-versus-complete receipts. Catalog startup distinguishes
Prepared, InProgress, ordinary thread-spawn failure, and Started rather than
inferring state from terminal counters. Root/recovery/startup-row construction is
inside the same unwind boundary.

Failure first closes admission for every installed service, then consumes all of
them against one caller-owned absolute deadline. Only afterward does it return the
same live App with the primary diagnostic and raw receipts. Audio-output intent,
Viewer display policy, and the exact pre-start process theme are restored and
read back before the receipt can claim success. Opaque panic ownership is retained
without running its destructor and makes the whole-start predicate fail closed.
The complete Host declares execution-service fields before its App field so even
ordinary unwind destroys service owners first. `Drop` remains a best-effort
fallback and never manufactures qualification evidence.

The feature-gated `host_startup_qualification` Cargo example links the real product
Host and RequiredPackaged Preview. It builds two successful routes and injects all
18 Host boundaries, three Waveform boundaries, and five Preview boundaries,
including one actual packaged media worker. It verifies App/EventBus identity,
state-marker preservation, exact partial/complete inventory closure, and opaque
payload rejection. Cargo example executables resolve the packaged product worker
from their shared profile root just like `deps` executables. This qualifies the
Host-only transaction; Window old/candidate GPU replacement, native event-loop
handback, and durable success-receipt history remain separate work.

The domain-neutral App owned-worker Module now owns the same join algorithm
used by Preview, Thumbnail and catalog; Preview's existing public outcome name
remains a compatibility alias. The bounded `window_service_protocol` Cargo
example compiles actual Thumbnail/catalog source and existing access/activity
Modules. It does not replace actual Host, native Window, GPU or campaign tests.

Window error merging preserves a published operation error, event-loop
failure/panic, callback replacement failure, final GPU failure, and UI cleanup
failure without allowing a cleanup result to replace the primary. The final
active generation's raw receipt and exact App handback are retained by the
Window run and rechecked by the endurance Adapter.

Window candidate publication is a four-state ownership transition:
`Prepared -> Activated -> Outer Installed -> Host Published`. Initial startup
installs its activated Session before its guarded Host publication; same-Device
role replacement transfers the complete device-generation authority and swaps
the candidate into the caller's Session before Host mutation. A panic during
layout/display/native-import publication, theme query, visibility, or initial
redraw cannot strand the active GPU generation in a temporary candidate
`Drop`: the outer Session remains the owner and enters explicit bounded closure.
New-Device reopen also installs the activated candidate in that outer Session
first, then retires the now-local old Session and publishes the Host only after
its clean receipt; retirement error or panic therefore leaves the candidate
explicitly closeable. A normal role-replacement error is retained as an
event-loop failure. Partially mutated Host state is not rolled back and may not
resume; the Window fails closed and exits.

Initial Window native/GPU construction now runs inside one caught, stage-ledger
scope after Runtime and Host startup. The raw complete Host is retained outside
that scope and is wrapped as an active `AppUiHostSessionOwner` only after Host
publication succeeds. Window, Surface, Adapter, Device/Queue, Viewer progress,
Preview waker, Prepared Window, and Activated Window checkpoints feed one failure
path. That path first consumes any partial Viewer GPU owner, then hands the exact
App back through complete Host closure, then consumes the background Runtime
supervisor, all under the original absolute deadline. A returned or unwound
construction scope proves that its Rust native authorities were consumed on the
event-loop thread; it does not prove OS compositor or driver termination. The
validation batch wraps EventLoop creation and consuming drop in a separate
`event_loop_owner` Module. Construction failures retain a stable failure class
and original diagnostic without fabricating a Window/native-return receipt;
successful construction yields Rust-owner handback evidence only after the
EventLoop destructor returns, while physical native termination remains
explicitly unqualified. An
`app_ui::window_outer_receipt` Module now owns the mutually exclusive Runtime
startup failure, Host startup failure, pre-active failure, active publication
failure, and normal active-exit outcomes. Host close first leaves a pending
Host/GPU handback; Runtime close completes the inner facts, and only the caller
of the complete Window function may add the native-return marker after the
`run_on_demand` borrow and Window-owner scope have actually ended. A fallback
Host `Drop` is an explicit incomplete state and cannot be upgraded to clean.

Successful Surface-reopen validation seals one bounded canonical Window-run
receipt from the existing recovery receipt plus the exact Runtime, Host, final
GPU, and native-return JSON/hash pairs. The endurance campaign's raw producer
event retains both the compatibility recovery receipt and the outer Window
receipt; the standalone validator now wraps the complete batch outcome in its
separate schema-3 report. Neither Adapter reconstructs leaf qualification or
discards final closure evidence. Window-run schema 2 reparses the final GPU
leaf, reruns the owning clean-retirement predicate, and binds its Surface and
Device identities to the nested Surface-recovery schema-4 `after` pair. The
old shutdown schema-3 leaf is separately bound to the `before` pair; every
nested shutdown, contract, and picture byte string must be canonical even when
all hashes are recomputed. Runtime, Host, and native leaves do not yet have the
same full semantic replay, and physical native termination is always serialized
as unverified. The batch API returns a move-only typed success or
failure: success retains all Window receipts plus exact EventLoop and final App
shutdown evidence; failure preserves completed receipts, the primary
class/identity/diagnostic, optional exact Window closure, EventLoop handback
when it existed, and App shutdown. Cleanup can annotate but never replace the
primary. Mutually exclusive primary variants make request, construction,
Window-operation, and final-App failure evidence impossible to cross-combine.
Timeout representation is preflighted before EventLoop construction; each
operation then freezes its own absolute deadline exactly once immediately
before Window entry and passes it unchanged. A theoretically later overflow is
a distinct operation-admission outcome with EventLoop handback and no invented
Window evidence. Both batch and one-operation entrypoints return this complete
outcome; neither projects away batch-level evidence. Durable standalone
batch/failure serialization is complete. Successful old/candidate/final
identity binding is complete; ordered candidate-failure and dirty-retirement
history, OS/driver terminal evidence, and full cross-Module semantic replay
remain unfinished lifecycle work. The campaign now consumes its selected Surface
driver exactly once after all phase/App owners, embeds the shared EventLoop
Rust-owner closure in run-manifest schema 3, and retains that closure across
publication failure. The current endurance CLI selects the explicit
`NotApplicable` headless driver, so it does not claim physical Window coverage.

The final `AppEnduranceShutdownEvidence` now has one bounded schema-1 receipt
Module. It projects Project and the seven named App worker domains into fixed
primitive shapes, while preserving Reference Output, Export shutdown and
terminal snapshot, Audio Playback, and Audio Source Cache as independently
hashed canonical typed leaves. Replay verifies every outer/leaf hash and
canonical encoding, reparses complex leaves into their owning crate types, and
recomputes the complete App clean predicate; dirty evidence remains serializable
but cannot qualify. This receipt is the App leaf for the next batch-outcome
schema, not yet a claim that the standalone CLI publishes failure outcomes.

Process-local EventLoop shutdown and Window failure closure now expose additive
bounded receipt Interfaces. An EventLoop receipt can only be minted after the
Rust `EventLoop` owner is actually dropped; construction failure therefore has
no shutdown receipt. A Window failure receipt preserves exactly one of runtime
startup, host startup, pre-active, active exit, or active-publication failure,
with fixed Runtime/Host/GPU/native leaf membership and independent leaf hashes.
Its verifier checks canonical shape and byte integrity, including the active
native-return marker remaining `unverified`; it deliberately does not claim
independent Runtime/Host/GPU semantic replay or physical OS/driver termination.
The existing successful Window receipt now delegates nested recovery replay to
the Recovery Module instead of imposing a second JSON canonicalization rule.

Surface/Device batch outcomes now compose those leaves through one schema-1
`surface_reopen_batch_receipt` Module. Success and each failure class have
mutually-exclusive fixed fields: request rejection and EventLoop construction
cannot carry EventLoop shutdown; admission failure carries EventLoop but no
Window closure; Window failure carries an explicit `missing` or `returned`
closure state; App-shutdown failure follows all submitted Window receipts.
`submitted_request_count` is retained independently of completed receipts, so
replay verifies zero/preflight, partial, and complete execution without
inference. The 24 MiB outer bound accounts for JSON escaping of up to 24 bounded
Window receipts plus the bounded App leaf. Replay invokes every owning Module's
verifier, rejects duplicate cycle/operation identities and nested tamper, and
derives `qualifying=true` only for a clean success. The CLI remains a separate
Adapter: schema 3 embeds exactly the batch JSON/hash for both single and batch
commands. It uses create-new, writes all bytes, synchronizes the file handle,
then prints the path; a typed validation failure returns nonzero only after
that publication. It never overwrites an existing report and makes no
parent-directory crash-durability claim.
### Canonical ANC export selection

The Export panel imports an explicit `FrozenAncillaryProgram` JSON through the
native file dialog and typed `ExportDraftEdit::ImportAncillary` action. App
reads at most 8 MiB, validates the Broadcast-owned schema and ST436 carriage,
and atomically retains immutable bytes-derived identity plus an Arc-owned
program. Failed import or dialog cancellation retains the prior selection.
The panel shows the source, exact source start/output frame range/cadence,
packet count and qualified scope; removal is another typed draft action.

Profile/range readiness uses Export's sole range/cadence resolver, including
after preset, Sequence or range changes. Unsupported selections disable queue
admission. `EnqueueDraft` freezes the full request only on user dispatch, so
painting and action availability never clone/serialize a large ANC inventory.
The same frozen attachment reaches AS-11 and its actual final-MXF ANC reimport.
Software structure/readiness does not grant physical SDI qualification.

The existing typed ANC import/clear UI also accepts `.scc` and raw `.cdp`. Caption
import first requires the implemented AS-11 output selection, binds exact sequence
timecode origin/range, and freezes source bytes into Broadcast's sole canonical
program. Import failure retains the previous attachment. The panel shows original
caption format/version, 608 channel inventory, 708 service inventory, bounded input
and placement constraints, and Transport qualification; it never displays Semantic,
caption-renderer or SDI qualification. The original JSON import remains available.
Tests route SCC and CDP through the same typed draft action and final enqueue model,
including unsupported-preset rejection, parity failure and source-file mutation.

Text atlas admission excludes nonempty whitespace-only spans from the existing
cosmic-text layout run. Layout advances and line positioning remain unchanged;
line-relative glyph offsets are resolved against that run's text, including in
multiline layouts. Unknown spans and all non-whitespace glyphs still request
atlas images, and rasterization/allocation failures remain missing-glyph
diagnostics. Whitespace is neither a failed image nor a fabricated resolved
image in the counters.

### Ordinary Window consuming shutdown

The ordinary Window entrypoint consumes the App owner after GPU publication and
UI-service closure through the same `AppState::shutdown_for_endurance` implementation
used by qualification. Those lifecycle seams are available without the validation
feature. The existing 750 ms product deadline is shared without renewal. Audio,
Export, Reference Output, media/background workers, Project persistence and cache
receipts must all report closure; incomplete App closure returns an error from the
Window entrypoint instead of accepting ordinary `Drop` as success. Validation may
return the exact App owner to its caller for reuse and later consuming shutdown.
The shared Inspector/Mixer Audio Processor Rack projection offers an explicit
rebind control for CLAP and VST3 instances. It submits a typed App action, which probes
and validates the installed binary before authoring one undoable Rack edit;
failed or canceled attempts leave the Project unchanged.

The Effect Browser reads category paths from registered executable Definitions,
including native plugin definitions, instead of reducing every Plugin key to
the generic core-data category. Built-in categories retain their semantic
ordering and plugin subcategories retain stable tree IDs across locale changes.

The Mixer and shared Inspector/Mixer Audio Processor Rack widgets format their
own control labels from the shell's captured Fluent locale. Their projected
Channel Strip, Route, parameter, and processor identities remain typed and
locale-independent; rebuilding the widgets after a language change does not
rewrite audio authoring state. Both bundled catalogs must contain identical
message IDs so an untranslated new control cannot silently fall back.

Property rows can opt into content height, measuring their control at the actual
form lane width with unconstrained vertical space and using the theme row height
as a minimum. Panel measurement and layout use the same policy. Export queue
rows use this mode for localized titles, progress, and diagnostic summaries so
narrow panels reserve space for wrapped content instead of clipping later lines.

FlexContainer delegates constrained measurement to the shared FlexLayout owner.
Horizontal flexible children shrink proportionally to their preferred widths
when fixed controls and gaps consume the available width. Both measurement and
placement remeasure children at their allocated widths, preserving wrapped text
height. Vertical content keeps its natural extent for scrolling.

Product entrypoint guards parse Cargo TOML to check default-run, binary paths, validation feature gates, and test/benchmark exclusion. Whitespace and line endings are not route semantics.

A panel with content-height rows fills a finite offered width during measurement, matching vertical ScrollView placement; unbounded measurements retain the theme preferred width. This keeps wrapping, row placement, and scroll extent consistent across viewport resizing.


The App status composite localizes Export queue state, exact frame progress,
history chrome and accessibility using the root's locale snapshot. Shared
phase IDs prevent the Export panel and status bar from defining different
lifecycle copy. Locale switching updates displayed status without replacing
the status widget or clearing its open/scrolled history viewport. Pseudo
formatting expands catalog AST text, preserving named argument data and
already-localized nested strings. App-produced status/history messages now
retain stable IDs and named arguments; each visible history row resolves in the
current locale without changing the underlying history facts.


### Common notification message projection

Notifications now use the App-wide `AppMessage` tree and `Localizer::message`.
Named argument identity is independent of insertion order, and nested product
labels are resolved with the toast's current captured locale. Exact integers
outside Fluent's precise numeric range remain decimal text. Notification feed
identity, bounded retention, timeout and locale-change behavior are unchanged.
The former notification-only message/value APIs are removed rather than retained
as compatibility aliases. Current status and bounded history retain the same
message facts. `set_status_hint` has no raw-string overload; the status bar and
Export panel project the fact with their captured formatter, and history rows
resolve at paint time. A language change preserves widget identity, author state,
open history and scroll position. Failed status continues to outrank Export
progress, and adjacent fact/severity duplicate suppression remains bounded.
Product labels nested inside status messages resolve with that same locale;
user-authored names and native failure evidence remain unchanged text data.


### Typed fallback action errors

The Host's unreported-action fallback nests `AppMessage::from_error` in its
localized action-failure context. A newer operation-specific error still wins.
Asset/workflow adapters similarly retain typed causes when available; contextual
project failures use the actual anyhow chain. Locale switching therefore changes
error headings without replacing paths, native details or error evidence. The
App does not parse core diagnostic Display strings. Existing String reason bodies
remain a separate typed-domain diagnostic migration requirement.


### Status publication ownership

Host fallback decisions use a typed transient `StatusPublicationId`, not equality
of message text or arguments. App status set/clear generates a fresh identity,
including an identical repeated report. A current error published during the
synchronous dispatch retains its operation-specific context. If the action
reports nothing, clears status, or only reports success before failing, the Host
publishes its typed generic failure. The observation is made across the one
Main-thread dispatch; it is not a realtime queue, EventBus request or authoring
generation.

Current status and history are private App state. Public read-only `status_hint`
and `status_log` observations prevent callers bypassing publication. All changes
use the existing set/clear interface; no mutable field compatibility aliases are
retained. Publication identity stays outside `StatusLogEntry`: history still
deduplicates identical adjacent message facts and severity, so repeated failures
do not flood it. Locale projection never publishes another status.


### Numeric Integer effect curves

The Inspector curve projection and App authoring transaction accept owner-declared
bounded numeric Integer parameters alongside Float and Double. Normalized curve
writes round to an integer only within the exactly representable f64 integer
range, then validate the canonical parameter contract. New keys and animation
enable use the first allowed interpolation declared by the parameter owner.
Audio curve insertion follows the same ordered contract. Undo/Redo retains typed
integer values and stable key identities; evaluation remains in Core.

### Typed numeric inputs

`NumberInput<T>` keeps Float32, Double and signed Integer storage through parsing,
dispatch and keyboard stepping. Integer fields parse i64 directly and saturate
keyboard increments at the signed boundaries. Display text round-trips the typed
value; minimum decimals never round a retained parameter. Editor increments apply
to nudging and slider snapping, not to arbitrary text entry. Unchanged valid text
publishes no duplicate value action. Text edits are local drafts: Enter or focus
loss confirms a valid value once; Escape discards the draft without dispatching
an author mutation. Keyboard increments remain immediate confirmed adjustments.
Escape releases focus so a subsequent shell cancellation can close the enclosing
dialog. `Widget::commit_pending_input` recursively confirms editable leaves before
a form submission, so field updates precede the submission Action in the queue.
Transactional TextInput leaves also confirm once through this interface and retain
focus with a fresh cancellation baseline; active IME compositions remain local.
Invalid text reverts to the last confirmed value without an author mutation.
During active IME preedit, the inner TextInput owns key handling: arrows and Enter
cannot nudge or confirm a NumberInput, and Escape first clears the composition.
IME committed text remains a numeric draft until the usual confirmation seam.

The Inspector numeric composite uses a normalized f32 slider for presentation
and a typed NumberInput for author values. Integer pointer mapping uses a bounded
i128 numerator with 24-bit slider coordinates, preserving exact endpoints and
nearby integer steps even beyond f64's exact integer range. Double bounds and
steps never pass through f32. Missing hard bounds retain the scalar's full finite
range; soft bounds shape slider travel independently of manual entry.

The production acceptance fixture registers a valid visual effect definition,
types large Integer and high-precision Double values through the Inspector,
decodes and commits the product Actions, then checks exact Undo/Redo and cancellation without an author write. Author admission and evaluation remain in Core/editor transactions.


Sequence dimensions/start frames and export dimension, bitrate, CRF, GOP and
palette fields use Integer input storage instead of floating value adapters.
Audio Processor Rack, Mixer trim/fader/route gain, Component volume and pan
retain their canonical f64 values through numeric controls and typed Actions.
Rack Integer controls parse integers directly and remain inside the contiguous
exact-integer domain supported by canonical audio f64 storage. Fractional or
inexact Integer values are rejected rather than rounded by the Action adapter.
Existing automation and lock admission remain authoritative.

Pan percent and fade-second adapters retain their canonical baseline for unchanged
presentation values. Canceling a text draft emits no command, preserving the
original pan scalar and exact rational fade, including sub-millisecond sample
durations. Cancellation also retains automation key identities and author history;
it never attempts to compensate for an already committed draft by writing back a
sampled baseline.

Audio fade inputs use f64 seconds until the existing explicit millisecond
quantization seam creates canonical TimelineTime. Long durations therefore do
not lose millisecond resolution through f32. This changes presentation/input
precision, not the audio DSP working format or temporal interpretation.

Acceptance covers actual Rack and Mixer author commits with exact Undo/Redo,
real Rack text entry for Double/Integer, invalid fraction/non-finite rejection,
and a day-long fade retaining its final millisecond.

Inspector audio source, stream-binding and track-lock reasons use the active
Localizer. Component/catalog absence, unavailable nested output, physical rebinding,
primary/default stream and unknown native channel-layout labels come from Fluent.
Media language tags, user-authored stream titles, stable identities and named
multichannel speaker layouts retain their canonical values. Re-projecting locale
changes labels without modifying component selection, channel mapping or author
generation. Both catalogs carry the same message IDs.

Inspector position, scale and anchor axis controls emit sparse channel edits,
resolved against the current Clip at admission. They never capture sibling
values in callbacks. Viewer gestures explicitly replace complete vectors, and
compensated anchor gestures still commit anchor and position atomically. Both
forms use Core's tagged parameter-write contract and the existing authoring
transaction. Clip, Effect and Mask share that same write algebra. The alpha API
requires an explicit write kind; there is no legacy
payload interpretation. Opacity and scale percentages use Double presentation
arithmetic until the canonical Float/vector author seam, retaining original
Float bits through unit conversion. No-op component writes produce neither new
keys nor history entries.

Generic Vec2/Vec3/Vec4 Inspector controls emit one changed component and never
rebuild a vector from captured siblings. Current stable owner admission supplies
remaining values and exact Clip-local time; no-op components create no history
or keys. Complete colors, curves and Viewer gestures remain whole-value writes.
Mask parameters use the same value-write contract, while shape geometry retains
its separate typed shape authoring interface.

Value-widget callbacks and their action helpers take stable owner targets only;
no ignored property-path argument or captured alias is retained. Basic Title
multiline presentation resolves the title-text definition identity and uses the
projected property label for its input hint. External resource file paths remain
authored resource values, distinct from removed UI routing aliases.

Undo/Redo pauses the Playback-owned transport at its authoritative current
position before applying history. It does not invoke the explicit Stop command
that returns to frame zero. Restored author snapshots retain monotonic revisions
and the editor cursor, so the next animated parameter edit uses the same
Clip-local author time rather than silently targeting an earlier key.

### Refresh admission during pointer editing

`Widget::has_active_pointer_interaction` reports local unfinished pointer
input, independently of keyboard focus and accessibility presentation. Slider
and CurveEditor report their actual drag state and clear it on release,
focus loss, or disable; CurveEditor also clears it on Escape cancellation. AppUiHost includes this signal in its
recursive transient-interaction admission check. Author actions still commit
and synchronize Preview intent immediately; a same-mode full model rebuild is
deferred until the gesture ends. The existing bounded Preview/frame projection
still runs for full author dirtiness, even without a separate worker repaint,
so retaining input widgets cannot retain an obsolete author picture. This retains the EventRouter capture owner
without copying stale parameter values into new widgets. An empty action drain
after pointer release admits the deferred model refresh. Real Inspector tests
route pointer down/move/release and window focus loss through EventRouter and
the Host, verifying continued live author edits and eventual refresh.

This admission contract does not coalesce author history or establish validity
for a removed/locked owner; current owner validation remains in action dispatch.
Owner transitions and other captured widgets require their own qualification.

## History transport coordinates

The App pauses transport before restoring an authoring history entry. It captures
the Playback engine's exact FramePosition and active Sequence owner before the
restore, then rebinds through the existing settled timeline seek. For an unchanged
owner, timeline_position performs nearest-frame lowering on the restored rate;
a bare frame number is never reinterpreted with a different time base. If history
removes the active owner, the restored Sequence's own playhead supplies navigation.
Redo preserves a current navigation owner that still exists in the restored project.

The seek seam binds the restored Sequence, reprimes audio and advances the playback
epoch, invalidating work from the old grid. Current timeline time reads the actual
Playback FramePosition rather than synthesizing one from the current author rate.
Authoring restore and runtime admission remain separate: a committed author receipt
is published even if subsequent playback admission reports an error. Physical device
failure behavior is a separate qualification requirement.

## Inspector text editor selection

Generic Inspector Text controls use the definition's `ParameterTextMode`, not
line breaks in the current value. Empty multiline parameters retain a text area;
single-line fields retain their compact control when their authored value has
line breaks. Generic plugin fields keep confirm/cancel draft semantics and one
transaction on commit. Basic Title remains explicitly multiline with its existing
live editing action policy. Event regression tests cover Unicode, empty multiline,
line-break-containing single-line values, cancel, no-op, Undo and row geometry.

## Typed resource editing

Inspector Resource values open a shell-local draft modal. URI, project asset,
external file/folder and Unbound retain their distinct author representations.
Resource schema admission shapes available modes and native file filters. The
full asset-library chooser loads names on open, independent from browser folder
navigation, and supports searching. File/URI drafts survive kind changes; typing
updates local form state without author transactions or focused-input replacement.
Only confirmation lowers a stable target through the same Inspector parameter
write action. Cancel and unchanged confirmation emit no author action. Native
picker cancellation preserves the draft and does not import media. Draft IDs,
author session/Sequence and current owner/schema/value checks reject stale
updates and invalid owners. Qualification is tracked in the local issue tracker.

## Overlay ownership and input targets

UI Events keeps the winning overlay scope separate from its ordinary content
hit target. Normal pointer routing descends into that scope to establish focus
and capture on editable leaves; it cannot fall through to background siblings.
Outside-close and clipped content retain the scope target. Nested popup overlays
still take precedence. Captured-pointer preemption compares overlay scope ancestry,
so moving a text-selection drag over another normal control in the same modal
does not steal capture. Actual EventRouter resource-dialog regression covers
pointer focus/capture and IME Enter/Escape before ordinary Enter confirmation.

The Widget root declares its active keyboard focus scope directly. TreeWalker
collects Tab/Shift+Tab order only within that subtree. EventRouter suspends the
background focus, establishes modal focus before routing input, restricts keyboard
bubbling to the scope, and blocks global shortcut fallback while the scope is
active. Closing restores the original focus only if the control still exists and
is focusable. Replacing a modal retains the original background restoration target.
Scopes without focusable controls still consume keyboard input without targeting
the background. This contract is independent of pointer overlay hit testing.

Focused controls receive Tab first so an active IME or an editor's own Tab
command can consume it before traversal. Resource locale updates change captions
and hints in place; TextInput and Button caption setters retain identity and
interaction state. Resource search caches normalized library captions and
reconciles them when the Library connection identity or database revision changes.
The connection identity uses a weak reference so the chooser cannot keep a retired
Library alive. Host reconciles before transient-input refresh deferral; direct
Root refresh and resource events use the same boundary. A revision is captured
before reading rows so a later concurrent commit cannot mark older rows current.
Unchanged revisions avoid listing and normalizing the Library again. Providers
that disallow ProjectAsset bindings do not load the chooser catalog.

Candidate replacement uses Dropdown's existing model API, retaining widget
identity, focus and popup state while clearing obsolete row indices. It preserves
the Resource draft, selected reference, search input and IME composition. Renamed
and newly imported assets appear in current search results; retired assets leave
the candidate list without silently changing a bound reference. Empty-result
feedback is recomputed, and confirmation independently validates current membership.
Catalog refresh errors do not block local draft edits or cancellation. Opening a
new chooser and confirming a changed ProjectAsset still require authoritative
Library reads; failures leave author state unchanged and retain a recoverable draft.

The actual resource-shell LUT acceptance traverses native-picker adapter output,
one author transaction, Undo/Redo, durable `.mdp` save/reopen and the compiled
Float32 graph executor. The same resource modal traverses forward/reverse Tab
through its editable, browse and confirmation controls without emitting author
actions, then restores the background focus on cancellation. A Unicode cube path
swaps red/blue for cube corners and fractional interior samples while preserving
transparent, translucent and opaque alpha. Output before save and after durable
reopen is compared exactly, in addition to checking the persisted binding.
Cancellation, unchanged/stale confirmation, picker cancellation/failure/multiple
selection, declared file filters and locked-owner invalidation have explicit
regressions. This does not qualify physical native dialogs or third-party
plugin resource providers on another platform.

Resource confirmation checks unchanged author intent before applying new-input
URI or provider-availability checks, without allocating a replacement reference.
Unavailable original ProjectAsset references remain visibly bound and unchanged
confirmation closes without a transaction. File/folder captions follow the
declared resource path kind. New changed references still require full admission.

## Workspace window resizing

The undecorated workspace uses client-side edge and corner hit testing on
Windows/Linux to start the native winit resize operation on left-button press.
Hit geometry is evaluated in logical pixels so display scaling preserves the
grab area. Matching native cursor hints take priority at the border. Startup,
maximized/fullscreen windows, active widget capture and the eyedropper do not
enter this path. AppKit retains its native resize handling because winit does
not support explicit drag-resize on macOS. Workspace minimum size remains a
native window constraint; startup retains matching minimum and maximum sizes.

Resource drafts validate their stable author owner before Host defers a full
refresh for focused text, IME or pointer interactions. Validation resolves the
current Session/Sequence, selected Clip, actual Track lock, property instance
address, schema and original resource directly from author state without
rebuilding Inspector curve projections. Removed/locked/switched owners cancel
the modal immediately; valid focused drafts retain their widgets and IME.
Each resource update and browse request uses the same current-author check, so
a delayed event cannot open a picker or retain input against an invalid owner.

Slider exposes explicit begin/update/finish/cancel edit callbacks with a fresh typed
identity for each pointer drag. Begin is emitted even if the value is unchanged;
release/focus loss finish once. Keyboard edits are independent. The common
numeric property builder wraps only canonical Clip/Effect/Mask parameter intents
in a typed gesture action; preferences/form draft callbacks remain local. App
validates the selected owner, actual lock and current Session/Sequence/generation
lease on every update, then invokes the same property author modules. A failed
or stale lease cannot keep authoring an obsolete captured control. Boundary/no-op
actions do not publish author invalidations; live changes retain the bounded
Viewer update path and defer dock rebuilding until release.

All Host Root model replacements seal an active parameter gesture through the
authoring session before discarding its input widgets, including preferences,
asset-folder navigation and startup/workspace transitions. A failed history
seal retains the existing controls and schedules a retry; a delayed event cannot
continue writing through the removed interaction identity.

Escape during a focused captured slider interaction restores the exact original
value, releases pointer capture and emits Cancel once. Late pointer release or
focus loss cannot accept the cancelled interaction. Value-only form controls
restore their original draft; canonical Clip/Effect/Mask controls ask the authoring
session to cancel the staged command. Failed author updates cancel the entire
interaction and force a model refresh even if the initial update never changed
an author generation.

Undo during a captured parameter slider interaction uses the Session's next
meaningful history command. A changed drag is accepted and reversed atomically;
a net-zero drag releases its pending command while restoring the preceding
command. Host discards invalidated controls before transient deferral, and late
pointer events cannot recreate a write. This policy preserves original Redo on
cancellation and net-zero finish without UI snapshot restoration.


CurveEditor Escape clears selection and repaints independently of whether its
point value changed. Any active drag releases pointer capture, including an
unchanged press and a drag returned to its origin. Cancellation emits no
incremental key edit; late pointer release or focus loss cannot commit a cancelled
drag. Capture ownership is an interaction fact rather than a value-difference
heuristic.


### Numeric curve draft ownership

CurveEditor can expose typed Begin/Edit/Finish/Cancel lifecycle events. This
callback replaces live value and incremental callbacks: a pointer drag remains a
local normalized draft until release or focus-loss acceptance. Keyboard nudges
while captured adjust that draft without committing another edit. Delete accepts
one deletion and releases capture; deleting an uncommitted inserted point cancels
the draft. Escape, disabling and replacement of points cancel capture without
emitting an incremental author edit. Standalone keyboard edits carry no pointer
identity.

Clip and Effect numeric Inspector curves register a stable parameter owner on
pointer down through App's shared parameter gesture protocol. BeginCurve opens
the existing AuthoringSession Sequence/generation lease without an initial
write. Release sends one typed curve intent through the canonical curve authoring
module, then Finish. Curve and value intents use distinct target kinds, so a
matching interaction identity cannot authorize another edit family. Host checks
owner selection, actual lock, Session, Sequence and author generation before
transient refresh deferral. A stale lease discards the old control; late events
cannot recreate or commit its draft. This does not wrap AI/headless product
commands in UI selection policy, and does not introduce a second keyframe model.


An identical numeric curve update inside a valid gesture is a no-op rather than
an edit failure: it retains the live interaction and any preceding effective
update. Ordinary independent no-op product commands keep their explicit failure
contract. Finish/Cancel of any matching parameter interaction refreshes its canonical
model even if frame/value lowering yielded no author change, so local normalized
points cannot remain as misleading unsaved curve geometry. No-op updates do not
advance author generations or publish author invalidations.


Numeric Clip/Effect curve menus and audio automation curve menus translate
interpolation captions through one exhaustive App-owned mode-to-message mapping.
Menu rows retain stable message IDs alongside localized captions and typed
commands. Locale changes affect presentation only: schema admission, supported
mode sets, current interpolation checkmarks and key identities remain author
facts. Chinese and English catalogs declare every mode, including automatic and
continuous Bezier and the numeric-curve ease presets.


Resource drafts inspect external path availability only when confirming changed
intent under an explicit Existing/ExistingOrNew contract. Wrong object shapes,
missing required inputs and inspection failures retain the draft with localized
feedback. File browsing calls the native open picker for both existence policies.
ExistingOrNew captions say Browse existing, and localized input hints explain
that a new path can be typed. Selection never invokes a save dialog or its
overwrite confirmation; permissive availability does not imply an output role.
Directory browsing selects existing directories, while typed paths can specify
new directory paths. The SDK flag does not declare an output destination.
Browsing/cancellation never creates or imports files
or emits author changes. Confirmation remains one reversible editor transaction;
all stale owner/schema/focus/IME guards apply to the new selection policies.

ResourceDraft validation is pure: confirming a changed path under an existence
policy returns a typed inspection request, retaining the modal. Host lazily owns
one supervised path inspection process and at most one replacement request.
Only the private product helper performs filesystem metadata. Native OsStr paths
are passed without lossy conversion; the helper emits one exact response of at
most 32 bytes and starts before graphics/platform initialization.

Both queued and running requests have a five-second UI deadline. Editing,
browsing, cancellation, owner replacement and Project close revoke admission.
Timeout remains bounded even if process creation or OS termination is delayed;
the supervisor retains the child and no replacement process starts until its
cleanup completes. The main thread never waits on a live child or joins a live
supervisor. Window scheduling adds a ten-millisecond timed wake only while
inspection work exists; it does not claim immediately drainable Preview work or
spin the event loop through ControlFlow::Poll.

Completion must match the current opaque ticket, Session, Sequence, selected
owner, unlocked Track/Mask, schema and original author value. Every draft update
revokes the ticket, including editing away and back to identical text. IME
composition at completion requires another explicit confirmation. Only a current,
valid success produces the same single author command used by synchronous resource
edits. Relative-path resolution ownership and fresh-process/platform recovery
remain separate qualification obligations.

While inspection is pending, Apply is disabled in place through
`Button::set_enabled`, preserving its widget identity and callback. Disabling
clears pressed/focus state, so an outstanding pointer release cannot click after
re-enabling. Modal traversal skips Apply but retains Cancel and editable controls;
editing revokes the inspection and re-enables explicit confirmation.
Opening a browser also withdraws the progress message before invoking the
native picker. Cancelling that picker retains the draft without stale progress;
locale changes cannot resurrect the withdrawn message.

### Project-owned relative resource input

The Resource draft captures the Project file alongside Session/Sequence identity.
A changed Project path invalidates the draft and pending inspection. Confirmation
binds new relative paths against that captured Project directory using the Core
lexical resolver, then validates the resolved reference against the provider schema.
The supervised inspection and eventual author action use the same bound path.
Untouched references remain no-ops; equivalent resolved references also close
without inspection or history. Missing targets are not created. This author input
contract does not claim a resolution base for arbitrary native plugin defaults.

### Native resource drafts

Resource drafts retain `NativePath` rather than reconstructing file selections
from their display strings. An untouched draft preserves foreign-platform or
non-Unicode intent exactly. New text is a local path; inspection requires explicit
local-path admission and cannot reinterpret a foreign path through the current
working directory.

### Live structured transfer curves

`ValueEdit` is the shared typed Single/Begin/Update/Finish/Cancel lifecycle for
Slider and complete-value CurveEditor callbacks. It replaces the slider-specific
public enum; no compatibility alias remains. `CurveEditor::on_value_edit` replaces
other callbacks and preserves live complete-value updates, while `on_gesture`
continues to defer numeric keyframe edits until release. The generic widget does
not interpret authoring schemas or execute Undo.

Live curve Begin precedes insertion and carries the exact original points.
Subsequent insertion, movement and captured keyboard edits share one typed
identity. The Inspector maps complete transfer-curve values through the existing
Clip/Effect/Mask write intents and canonical AuthoringSession gesture journal.
Live author generations update Viewer output without rebuilding captured widgets;
acceptance retains one reversible command, and cancellation restores author values
and the original Redo branch through the Session. Selection changes, actual locks
and intervening author generations revoke stale owners before refresh deferral.

The widget retains original local points and point policies only for cancellation
of its own live interaction. Escape, disabling or deletion of an uncommitted
inserted point cancels once without publishing a separate restored-value edit.
Release/focus loss accepts once. A new pointer interaction accepts the previous
one before opening a fresh identity. All matching terminal parameter boundaries
refresh canonical controls, including unchanged or rejected local values; late
pointer release/focus loss cannot commit a cancelled edit. Existing numeric curve
and ordinary slider behavior is qualified alongside the structured curve matrix.

### Component-specific numeric controls

Inspector vector controls read the matching schema channel range and step.
Scalar model hints are projected only for scalar parameters; they do not stand
in for all vector components. Sparse component actions retain the canonical
parameter address and write only the selected index through the author session.

### Exact numeric editor endpoints

Float and vector Inspector controls edit f64 author channels with exact schema
ranges and steps. Their values materialize through Core Float projection; the UI
does not prematurely round a decimal hard endpoint to f32. Non-animated numeric
components use the same sparse write seam without creating animation channels.


### Precise compound numeric Inspector controls

The shared component editor consumes f64 channel values for Float, precise
Double and i32 vector families. Existing Float vectors retain their finite f32
representation bounds; double components keep the full f64 domain and integer
vectors use exact i32 limits and integral editor steps. Every component reads its
own numeric contract and dispatches a sparse typed channel write so editing one
field preserves all siblings. Double vectors use X/Y/Z and precise RGB/RGBA use
R/G/B/A labels. Typed text editing, one-command Undo/Redo and Escape cancellation
are exercised through actual Inspector widgets and the canonical App author
boundary for all six compound families. Precision-preserving color picker and
OpenFX descriptor/temporal/color binding remain outstanding product work.

### Transfer curve insertion capacity

CurveEditor accepts an optional user insertion limit without truncating supplied points. Structured normalized transfer curves configure the canonical 32-point limit; ordinary keyframe curves retain unrestricted insertion. Empty-plot clicks at capacity are rejected before a value gesture or pointer capture starts, preserving draft state and Undo/Redo history. Existing points remain editable and deleting an interior point frees capacity. Inspector captions show localized current and maximum point counts.

### Curve drag coordinates

A captured point drag owns its point index, original normalized point, original pointer position and insertion intent together. Movement is computed from pointer displacement, preserving the grab offset and the untouched axis exactly; a stationary pointer does not change the point. A collapsed plot axis retains its original coordinate without dividing by zero. Cancel restores the original point or removes a newly inserted point. Audio automation lowering retains the canonical key's original exact time or value when its projected axis has not changed, so horizontal movement never re-applies value stepping and vertical movement never quantizes existing author time.

### Professional export localization

Export controls and summaries resolve owned wording through the same Fluent locale snapshot, including video range and bit depth, alpha and legalizer titles, VBV units, integer PCM depth, fixed professional PCM descriptions, IMF/DCP package captions and essence descriptions. Codec, profile and color-standard identities remain presentation arguments or protocol labels; localized captions never determine preset values or dispatch. Chinese catalog copy also covers Processing Scope ownership, Sample Delay, ICC calibration, Camera RAW summaries and typed shell/startup/preferences/asset-browser failures without rewriting opaque diagnostic arguments.

### Audio automation curve ownership

Audio curves retain a local pointer draft and acquire the shared author-session
gesture lease before capture. Release lowers the draft through the canonical
audio automation action and seals one reversible command; Escape cancels without
discarding the original Redo branch. The lease validates Session, Sequence,
author generation and the authoritative audio target's editability. Component
targets require their selected Clip; Processing Scope targets require a selected
Clip bound to that scope. Mixer channel-strip targets remain independent of Clip
selection. A locked, removed or superseded owner cannot accept a late release.
Visual and audio target identities are separate enum variants, without dummy
Clip identities or a second audio-authoring interpretation.

### Sparse numeric curve movement

Clip and Effect curves distinguish existing-key movement from insertion. A move
names the stable key and supplies exact typed author coordinates for supplied axes. The shared author
adapter validates supplied coordinates, retains exact original time/value for
absent axes, and applies the same collision, identity and history rules as a full
update. Unchanged moves create no command or author generation. Synthetic curve
endpoints insert keys; they do not invent an existing key identity. UI projection
never reconstructs an untouched exact rational time or f64 value from screen
coordinates.

### Finite curve value ranges

Inspector and audio automation keys, samples and controls use absolute f64 author
values. Soft bounds initialize the view; hard bounds constrain key input and the
canonical schema remains author admission authority. A center/radius viewport
represents full finite ranges without requiring a finite endpoint difference.
Time and interpolation remain available for fixed hard ranges. Typed conversion
occurs only when a value axis actually changes. Exact untouched coordinates keep
the sparse-move semantics. Audio processor curve insertion is qualified through
validated plugin parameter snapshots, canonical Rack insertion and author actions
with Undo/Redo, including full finite intervals and fixed maximum values. Native
DSP execution remains separate acceptance evidence.

### Fixed-value curve interaction

Inspector models carry a validated CurveValueAxis. Visual and audio factories
use CurveEditor::with_author_points; a fixed hard interval retains its exact
absolute value and locks value input. A fixed soft interval with a wider hard
interval displays the hard interval and keeps value editing available. Pointer movement and insertion
constrain the visible draft before any author action. Selected Up/Down keys are
consumed without edits, including Shift; Ctrl/Alt/Meta chords keep normal shortcut
routing. Horizontal movement, insertion, deletion and interpolation remain
available. Programmatic point and display-sample replacement obey the same
constraint. Cancellation restores the original points and publishes no edit;
release publishes at most one edit. An unchanged constrained drag only closes
its gesture. Ordinary constant curves with a nonzero value interval remain
editable.

### Continuous audio curve values

Audio automation projection carries the declared value type and validated value
axis. Continuous pointer edits retain absolute author units;
the numeric editor's increment is not an author-value quantization grid. This
preserves continuous values and avoids overflow from dividing large values by
small editor increments. Integer parameters still round to integral values before
hard-range admission. Numeric input and keyboard stepping retain their own editor
increment behavior. Stable-key movement preserves untouched exact time/value
coordinates, and every resulting edit uses the existing author transaction and
Undo/Redo path.

Sparse audio handle and visual curve actions distinguish an omitted coordinate
from a supplied invalid coordinate at product-action decoding. The former
retains exact author state; the latter rejects the entire action before history
or author generation advances. Product-action regression tests cover both axes,
nonfinite values, and unchanged state/history after rejection.

### Current machine preference schema

`AppUiPreferences` admits the current schema as one document: every machine
field is required except the optional custom workspace layout. Unknown fields
and unknown product locale choices are rejected instead of creating a hidden
System-language variant or filling missing required fields independently.
The real loader uses defaults for absent, malformed, or incompatible documents
and does not rewrite their bytes. System locale resolution still accepts an
external desktop language tag and selects a supported product language.
Current-schema tests use nondefault theme/language fixtures, verify exact file
preservation on rejection, and round-trip every supported locale with no
custom layout. This is alpha schema admission, with no field migrations.

Visual value adapters call the Core value-write seam without a Vec2 handle
argument. Handles remain typed author geometry, edited through the separate
Core mutation; value input retains stored control precision. The Inspector test
caller follows this same API, without an unused optional override.

Audio automation curves treat unauthored viewport endpoints as value-editable boundaries. A vertical boundary drag inserts a canonical key at the fixed owner time through the same author action as other curve edits; an unchanged boundary creates no automation. Existing keys retain stable identity and unchanged exact coordinates. Endpoint acceptance covers author-session Undo/Redo and a real Unicode .mdp archive with a SQLite library, followed by fresh projection from the reopened document.

Temporal CurveEditor author gestures publish changed coordinates while captured; their canonical author-session lease supplies cancel restoration and one grouped history entry. Insertion reserves a KeyframeId once per pointer gesture, including virtual viewport boundaries, and reuses it across every update and release. Audio insertion and visual explicit-ID Upsert retain this identity, while visual Move still requires an existing key and occupied times reject other identities. The once-per-release edit callback remains distinct from live author gestures. Temporal handle interaction and full workflow acceptance remain required.

CurveEditor accepts an owner-provided display projection for current point intents and restores pre-gesture samples on cancel. Audio Inspector/Mixer uses a canonical Timeline candidate and Core prepared segments, with no duplicated interpolation implementation. Visual display projection uses the canonical preparation described below; full temporal handle acceptance remains pending.

Visual temporal graphs now retain an immutable canonical AnimatedProperty snapshot and exact owner interval. Display projection reuses the same visual author preparation, typed payload lowering, Core mutations, and Core evaluation as the real command. The snapshot is derived UI data and never project authority. Move/insert/delete preview samples are compared with real author results across Hold, Linear, Bezier, AutoBezier, and ContinuousBezier; pointer cancellation restores the original samples.

Curve preview receives the same Begin/Edit/Finish/Cancel lifecycle as author commands. Its derived canonical candidate advances in command order, since Continuous Bezier normalization can change control geometry after each position edit. Begin retains the candidate for rollback, Cancel restores it, and sampled lines retain the widget rollback behavior. This transient projection never publishes project state or history; the author session remains the sole mutation seam. Nonzero manual/continuous controls and multiple moves are compared exactly with real author results, alongside insertion and deletion.

Numeric curve Move actions carry absolute TimelineTime and typed PropertyValue targets; omitted axes explicitly retain current author coordinates. UI pointer movement supplies both targets, retaining the original canonical coordinate whenever its projected axis is unchanged or the pointer returns to its origin. Only changed axes use the shared App viewport-to-author mapping, avoiding needless quantization or scalar type conversion. Each absolute positional gesture update derives its candidate from the immutable original world geometry. Derived display candidates are output, not the next update's author source. The actual author gesture test restores an exact one-third-second key and its value, returns the complete author state to the origin, and leaves no Undo entry. Old normalized Move fields are removed from the alpha command schema.

The Host qualification routes an Inspector curve through actual pointer capture, multiple live updates, return to the original grab point and release. Numeric keyframe and structured transfer curves must restore the original author state before release, finish without changing Undo/Redo diagnostics, preserve an existing Redo branch, and retire both pointer capture and the author gesture lease. This test supplements the exact one-third-second coordinate regression in the parameter authoring path.

### Canonical numeric handle coordinates

App curve point edits retain Core's exact owner-time/value handle offsets.
Visual and audio numeric evaluation now use the shared Core cubic solver and
constraints; the UI owns no normalized persisted handle geometry. Manual point
edits retain legal controls, and time edits bound retained controls to new spans
without rotating their direction. The author session remains the only commit and
Undo/Redo seam. Temporal handle drag widgets are still pending; this change does
not claim their interaction acceptance.

Absolute visual numeric Move/Upsert gestures use the Session-owned original
history snapshot. A handle shortened near a neighbor is recomputed from its
original direction and extent at every target, so returning to the grab point
restores complete geometry. Sparse Move axes resolve against current author
coordinates before candidate derivation. Insertion identity remains reserved and
stable. Relative mode/remove/value operations retain current-state semantics.
The captured widget derives positional preview from its Begin snapshot using the
same Core preparation. No extra author snapshot is retained by App.

Numeric curve gestures distinguish time-addressed insert operations from stable-identity
position updates. Time-addressed insertions accumulate in the current author candidate;
stable-identity drag samples derive from the Session-owned gesture origin to avoid
progressively shortening manual controls. A key created during the gesture is resolved
against current author state before preparing its final position, retaining its identity
and complete author metadata. Finish retains one reversible history entry; cancel
restores the original Sequence through the same authoring session.


### Resource confirmation focus and durable history qualification

The App resource-modal acceptance routes Tab and Shift+Tab through EventRouter
while the asynchronous inspection disables Apply. Disabled controls leave the
focus order; all remaining focus stays in the active modal. Inspection timeout
retains the draft and restores Apply for retry. Cancel restores the suspended
background focus. Traversal and unsuccessful inspection publish no author edit.

The LUT workflow qualifies both durable snapshots around Undo: saving an undone
binding preserves the live Session's Redo branch, while a fresh Session opens the
unbound author value with no inherited history. The shared graph must report an
AuthorEdit resource error for the unbound LUT, never reuse the previous compiled
resource. Redo in the original Session restores the exact six-sample RGBA output;
saving and reopening the bound snapshot preserves that same output. These are
real cube/SQLite/MDP tests with deterministic chooser input, not physical native
keyboard or picker qualification.

### Canonical temporal tangent editing

CurveEditor receives canonical CurveTangent projections and a CurvePreview with
both evaluated samples and current controls. Selected-key controls are painted
with semantic theme colors and clipped to the graph; hit testing owns only the
currently selected key's handles. Pointer edits retain the grab offset, constrain
temporal coordinates to the neighboring owner span, and leave finite value
coordinates unclamped. Value control excursions are independent of key values.

Visual adapters lower stable keys to sparse exact EditHandle author coordinates;
audio adapters use the existing exact AudioAutomationEdit::EditHandle. Core
resolves default controls, owns Auto/Continuous constraints and rejects invalid
candidates. UI projection neither evaluates Bezier curves nor owns author state.
Complete current controls returned by the canonical preview update constrained
partners during the draft. Absolute pointer candidates derive from the Session
origin; omitted coordinates in external visual actions resolve against current
author state first. Unchanged screen axes preserve exact rational handle offsets.
Wide opposite-sign finite value intervals project and lower without overflowing
their span. Invalid/nonfinite screen edits do not construct an author action.

Pointer capture reporting covers point and tangent drafts. Escape, disable and
point replacement cancel the author gesture and release capture; ordinary release
finishes it. A completed draft creates one reversible history entry, while cancel
or return to exact origin creates none. Keyboard handle edits are atomic actions;
Delete on a handle cannot remove its owning key. The non-live on_edit adapter
receives a tangent edit only on release. Actual EventRouter acceptance covers
visual continuous handles and audio Track-fader controls, exact axis preservation,
partner alignment, completion/Undo/Redo, cancellation and return to origin.

Selecting another key or opening its context menu clears the previous tangent
selection. Nearest-hit ties prefer the key, preserving key dragging when a
zero-length control coincides with its owner. Collapsed viewport axes retain
their exact original coordinates instead of synthesizing a movement scale.

Viewport zoom/pan, frame-grid nudge controls, keyboard-only handle selection,
large-curve projection performance and physical/platform interaction acceptance
require further implementation and qualification before claiming a complete
professional temporal curve editor.
### Dense curve projection

Visual and audio tangent adapters consume Core's ordered control iterator,
filter exact owner times to the viewport, and retain KeyframeId ownership.
The Widget owns a linear-size ID-to-display-slot index; no author callback
interprets the current slot as a key identity. Virtual viewport anchors have
no author identity and never acquire another key's controls. The scalar visual
model reads its admitted single channel directly with exact-time partition
boundaries; it preserves real keys at either viewport edge and uses virtual
anchors only where no real key exists. Outside keys remain in the canonical
source for interpolation and neighboring control spans. Presets derive from
stored interpolation metadata without reconstructing complete property keys.

The ignored temporal_handle_large_curve_projection_smoke exercises these real
App adapters at 1024, 4096 and 16384 keys, records model/tangent timings and
checks key/control counts plus finite geometry. It is a local projection
measurement, not a full interactive/render/plugin realtime qualification.

### Stable temporal point and tangent ownership

CurvePointTarget distinguishes a canonical KeyframeId, a generic normalized
owner-snapshot point, and an unauthored temporal viewport boundary. CurveEdit
Move/Delete carry that target; Handle and CurveTangent carry KeyframeId.
The temporal App adapters reject generic snapshot targets rather than infer
an author key from its current array slot. Normalized transfer curves retain
their complete-value callbacks and immutable snapshot identity semantics.

CurveEditor keeps target, interaction policy and context menu together in
point metadata. Structural insertion/deletion updates that metadata and the
ID-to-slot map atomically. Tangent filtering, hit tests, painting, selection
and author intents all resolve the same stable owner. A newly inserted key
cannot inherit the former occupant's controls or context commands. Escape
restores point metadata and the original canonical tangent projection.

Visual and audio adapters lower each action against the current canonical
preview source before applying that preview, retaining an immutable source
origin during a pointer gesture. The callback consumes that already-lowered
action, so projection order cannot change the unchanged-axis decision. Newly
inserted keys can be edited again without rebuilding the Widget; audio edits
clone current canonical key metadata rather than stale initial projected keys.
Virtual boundaries reserve one ID when authored. Return to the original point
cancels materialization and history; accepting a changed boundary makes it an
editable real key. The non-live callback emits one Insert on release.

A real App EventRouter/author/paint regression originally showed a linear
inserted key drawing the displaced key's controls, despite having no controls
in canonical state. Acceptance retains that assertion and routes insertion
and deletion followed by keyboard edits and two Undo/Redo steps in the visual
adapter, plus insertion/keyboard editing/Undo/Redo in the audio adapter.
Component tests also retain shifted generic snapshot identity, context commands and canonical controls on cancellation. This does
not establish physical keyboard, viewport zoom/pan or frame-grid qualification.


### Curve selection across workspace refresh

Temporal Inspector and audio controls attach a view key encoded from their typed
parameter owner. AppUiPanelModels carries the current AuthoringSessionId and
SequenceId. AppRoot retains inactive CurveEditorViewState only within that
scope and the same owning panel/key. Restoration preserves widget focus identity
and resolves selected author keys through KeyframeId, retaining new canonical
geometry. Missing keys/controls lose selection. Snapshot/virtual selection
requires identical geometry. Author-ID selection does not clone the point array.

Live pointer gestures, author drafts and context menus are not transferable.
Replacing a project/session or sequence retires the old focus identity even
when persistent parameter/key IDs happen to match. A real Host/EventRouter
regression routes pointer release through model rebuilding and a subsequent
keyboard edit, then verifies two Undo/Redo steps and fresh-session isolation.
Multi-key workflows and viewport controls remain separate qualification work.


### Keyboard curve navigation

Focused CurveEditor handles Home/End for first/last point, Ctrl or Cmd plus
Left/Right for adjacent points, Alt plus Left/Right for incoming/outgoing
canonical controls, and Alt plus Up to return to the owner point. These
selection operations do not publish author edits or history. Navigation is
contained during pointer capture, preserving the outstanding draft owner.
Tab remains shell focus navigation. Shift+F10 opens the selected point's
existing interpolation menu, initializes its keyboard selection, and uses
its canonical stable-address commands.

Temporal/audio and normalized transfer controls expose localized accessible
canvas names, collection selection/focus/disabled state, and delayed keyboard
help. Generic transfer help omits temporal control/menu instructions. Normalized
transfer controls also attach their typed parameter owner view key, preserving
focus and selection through unchanged canonical point projections. Actual
Host regressions cover temporal and transfer refresh/key-edit/history/session
isolation; a mouse-free EventRouter regression enters through Tab, selects a
continuous handle, edits it while preserving exact time, returns to the point,
and verifies two independent Undo/Redo steps. Viewport controls remain open.


### Exact frame nudges

Temporal visual/audio models supply the current sequence time base to the shared
CurveEditor. Plain Left/Right emit a stable key/control ID and signed exact
one-frame delta; Shift uses five frames. Time intent does not travel through
normalized floating-point point geometry. The author adapter reads the current
canonical key/control and retains omitted value coordinates and metadata.
Keys cannot cross or replace neighbors or leave the owner viewport; controls
clamp to their canonical neighbor span. Rejected/unchanged moves emit no author
action or history. Pointer capture consumes horizontal frame commands without
changing the draft owner. Generic transfer curves retain normalized behavior.
Nonpositive configured frame steps are inert.

CurvePreview now projects canonical key positions by KeyframeId alongside
sampled curve and tangent geometry, keeping selection and drawn markers aligned
with repeated exact edits. The API intentionally requires this field; alpha
has no compatibility shim. Fractional-rate EventRouter regressions cover
30, 30000/1001 and 24000/1001, repeated/Shift movements, unchanged values and
three independent Undo/Redo operations. Boundary/collision and dense viewport
admission checks do not use normalized minimum-point spacing.


### Curve viewport interaction

CurveEditor keeps time/value viewport ranges as local view state. Ctrl/Cmd wheel
zooms time about the pointer; adding Shift zooms value. Ordinary wheel input
continues to the surrounding scroll panel. Middle drag pans with an owned UI
capture and no author gesture/action. Escape restores the pan origin; release
or focus loss accepts the view. Disable or point replacement cancels pan and
releases capture at the next routed event. Viewport state is copied through the
existing same-owner/panel/author-session restoration seam; active captures are
not transferable.

Focused keyboard input supports Ctrl/Cmd PageUp/PageDown for time zoom (Shift
for value), Ctrl/Cmd Shift arrows for pan, and Ctrl/Cmd 0 for the initial value
view and complete owner-time span. Author point/control navigation reveals offscreen selection. Viewport
commands are contained during an author pointer draft. Exact frame nudges still
use the sequence grid independently of view scale. Both help locales describe
the controls; normalized transfer curves share the view interaction.

Geometry, hit testing, insertion, point/control pointer deltas and painting use
one viewport mapping. Invisible keys/controls cannot capture plot-edge clicks.
Curve/control segments are clipped in f64 projection coordinates before screen
f32/GPU submission; all markers stay under the plot clip. Zoom/pan ranges are
bounded and extreme finite value selection retains a representable span.
EventRouter author regressions verify zoomed pointer edits followed by an exact
frame nudge, with two independent Undo/Redo operations and untouched key values
on time-only edits. Temporal key/sample/control projections retain absolute author
values outside soft bounds. Only screen geometry and author key input are bounded;
canonical evaluation is not clamped by UI display bounds. Physical high-DPI/platform input, domain
axis rulers and multi-key workflows remain separate qualification.


### Exact visual curve insert requests

ClipCurveAuthorPoint contains owner-domain TimelineTime and typed PropertyValue.
Both Upsert and sparse Move enter the same author preparation, stable-key/time
collision admission, parameter schema validation and author transaction. Upsert
checks the exact owner interval and never interprets a soft-range ratio. The
obsolete normalized insertion command/parser is removed for alpha. Preview
uses this same preparation. Editor range availability is not an author
admission rule; the canonical property schema owns value validation.

The UI performs its current viewport-to-author lowering before dispatch. Golden
author acceptance now passes the existing exact typed endpoint value directly,
with an exact 7/8 owner-duration time, retaining its Bezier metadata. Regression
fixtures lower UI seed positions before the author seam. Direct codec/preparation
checks cover exact one-seventh time, Float/Double/Integer values outside soft
but inside hard bounds, hard/time rejection, immutable source and a property
without numeric editor metadata. The absolute UI value projection described below
uses this exact insertion contract rather than a normalized value parser.


### Absolute temporal value coordinates

CurvePoint.x is a normalized f32 owner-time projection; CurvePoint.y retains an
absolute f64 value for temporal curves. The normalized transfer constructor and
CurveEditor::with_points retain bounded f32 transfer storage precision. Temporal
factories use with_author_points with a validated CurveValueAxis containing soft
and hard bounds plus an optional author-unit keyboard step. No old coordinate
payloads or compatibility aliases are added. Integer keyboard steps honor metadata
at integer precision; continuous pointer values are not quantized to editor steps.

Visual and audio projections retain raw canonical key values, evaluated samples
and finite control excursions. Changed handle values subtract the canonical key
value directly, avoiding an overflowing intermediate screen delta for legal
opposite-sign controls. Moving an unchanged axis uses the current exact key
value/time. Preview invokes the same canonical preparation/evaluator as the
product action. A value viewport uses a finite center and radius, including full
Double spans and subnormal views. Segment clipping rescales overflowing differences
and pins boundary intersections before screen f32 conversion. Hit testing ignores
invisible keys; keyboard selection reveals out-of-view values. Zoom, pan and reset
carry only view state; raw value coordinates stay stable through owner refresh.

Tests exercise a real visual model's former 1.5-to-1.0 clamp, an actual Effect
with soft [0,1] and hard [-2,2], exact Double single-axis edits and Undo/Redo,
raw audio values, canonical preview, fixed hard/soft intervals, signed zero,
full finite geometry and real pointer edit/cancel/release after keyboard reveal.
The original Host transfer-selection regression remains and verifies canonical
f32 geometry after author refresh. Native output qualification remains bound to
its independently recorded clean source and product hash. Axis rulers, multi-key
workflows and physical high-DPI interaction remain separate professional
workflow work; this change does not claim them.


### Exact scalar key nudges

Temporal Up/Down on authored keys emits NudgeValue with stable KeyframeId, a
finite author-unit step and signed count (Shift uses five). Widget geometry is
updated only by the canonical preview projection. Visual adapters read the typed
source key and call the Core numeric offset contract; audio applies the same
contract to its canonical f64 keys. Sparse visual Move retains exact time and
metadata and enters the existing author transaction/Undo path. Transfer curves,
virtual insertions and tangent controls retain their distinct coordinate edits.

Integer curve projection uses the explicitly lossy as_numeric_f64/ channel
projection APIs only for graph geometry. Core now retains exact Integer channel
keys; preview/source reads and value nudges retain those bits independently of
whether adjacent integer values occupy the same floating screen coordinate.

### Exact scalar curve action coordinates

Scalar curve author points and move payloads carry `AnimationChannelValue`.
The inspector projects the raw author channel for geometry and comparison;
pointer candidates carry absolute coordinates, retaining the exact original
owner time and raw value for unchanged axes. Sparse direct actions resolve
omitted coordinates against the current canonical key. Keyboard value nudges
use the shared numeric offset contract with `Double` or `Int` author values,
without an intermediate Float evaluation conversion.

Preview and commit use the same canonical channel key mutation. During an
absolute curve gesture, interpolation metadata comes from the origin key when
that identity existed at gesture start, or from the current key for a key
inserted during the gesture. Normalized manual controls from intermediate
candidates cannot accumulate or overwrite origin metadata when returning to the
origin or cancelling. No-op gestures preserve existing Redo history.

This changes the alpha action payload value shape without a compatibility
parser. Project storage remains schema 31. Native parameter evaluation,
renderer and plugin execution continue using their existing typed seams.

### Rejected curve preview admission

An installed canonical curve preview provider owns edit admission: an absent
Edit projection rejects the candidate; absent lifecycle projections still admit
Begin/Finish/Cancel boundaries. The widget suppresses rejected author dispatch.
Point and tangent movement restore just their previous local coordinate, keeping
an existing pointer capture available for recovery or cancellation. Keyboard
moves restore the original point metadata. Deletion stages one removed entry;
a refusal restores its point, metadata, selection and capture before publication.
An accepted projection reveals the replacement selection after removal.
A rejected initial insertion cancels its gesture, removes the reserved local key
and retains the previous selection without capturing the pointer. Owner-less
curves continue to use local geometry. No full curve clone is added to pointer
movement and no additional author state or evaluator is introduced.

Recognized point and handle arrow edits remain handled when clamped or refused;
geometry change and keyboard ownership are separate decisions. Rejected edits
cannot leak their arrow keys to parent controls or global timeline shortcuts.


### Dense temporal key spacing

Transfer curves retain their normalized display spacing. Temporal insertion
admits distinct representable projected coordinates without a fixed fraction of
Clip duration. Temporal movement can cross neighbors inside the owner interval;
exact owner admission rejects actual time collisions and parameter violations.
Exact keyboard frame intents also permit crossing without floating conversion.

Point coordinates and metadata reorder together after admitted projection, with
selection and pointer capture following stable identity. The canonical key order
resolves exact author times that share one projected coordinate. The existing
ID map temporarily holds ranks and retains its allocation when rebuilt; points
and metadata move in place, without a complete curve clone per pointer update.
Rejected projection retains the last admitted local order. Cancel restores the
origin geometry and identity order. Escape cancels an active draft while retaining
an existing key/control selection; idle Escape clears selection. A cancelled
new insertion cannot retain selection of its removed point.

Actual Inspector acceptance uses Auto and Continuous Bezier keys in a long Clip,
real wheel zoom/pointer editing, exact frame intents and stable canonical order.
Unicode archives of undone/redone states compare the entire author Sequence;
saving preserves live Redo and fresh reopen begins with empty history. Additional
wide owner-time cases qualify exact keyboard crossing where projected coordinates
coincide. These do not qualify physical pointer precision beyond its projection,
all platform layouts, multi-key editing or full interactive performance.

Point gestures retain the original point slot as well as the projected coordinate.
Cancellation restores that slot by rotating points and metadata together in place;
it never infers exact owner order from equal projected coordinates. Selection follows
the restored target and the existing ID index is rebuilt without a full curve clone.
Actual Inspector regressions cover crossed Auto/Continuous keys in ordinary and
collapsed projected intervals, Escape and finished return-to-origin with a retained
prior Redo, and compare full author state plus display order and identity.
Exact neighbor-time pointer collisions retain the last admitted geometry and
author state, capture and identity; a subsequent valid candidate recovers before
Escape cancellation. Both constrained Bezier modes have real Inspector coverage.

### Canonical temporal boundary projection

CurvePreview supplies evaluated start/end points only where no author key owns
that exact boundary time. CurveBoundary identifies owner Start/End, independent
of mutable display slots. Visual and audio adapters lower boundaries through the
exact owner interval and share projection from canonical samples. No legacy slot
parser or alternate evaluator is retained. The widget updates or inserts/removes
at most two boundary points in place; ID indices rebuild only on a shape change.
Captured point gestures retain at most two original boundary entries and restore
exact origin order on cancellation, accounting for a provisional inserted point.
Real Inspector and audio EventRouter tests compare live projections to rebuilt
owner models at both ends, after moving away, and after cancellation or single
Undo/Redo. This does not qualify physical precision or native output for this
subsequent source change.

### Temporal curve rulers

Inspector numeric curves and Mixer automation curves supply their existing exact
owner interval to CurveEditor::with_time_axis. The widget reserves a value gutter
and a seconds gutter in the same geometry used by painting and hit testing.
Spacing comes from Theme spacing and metadata typography; layout bounds limit
ruler reservation so narrow panels retain an editable plot. Labels use measured
text widths and skip overlaps rather than truncate numeric content. Viewport
zoom and pan update both axes without an author edit. Values retain absolute
f64 units; fixed-value parameters display one actual value. Scientific labels
remain finite at f64 extremes. Owner-time tick arithmetic uses checked rational
operations; decimal seconds are display rounding only. When an extreme owner
origin loses its visible duration in floating conversion, labels use exact
rational time instead. Unrepresentable checked ticks are omitted.

CurveEditor::plot_bounds and point_position expose the actual UI projection for
routing tests. Actual Inspector and Mixer tests target authored points rather
than assume the full widget center is a key. Transfer curves retain their
normalized plot without temporal rulers. No project schema, author coordinates,
Undo interpretation or renderer evaluation changes.

### Key-set author command groundwork

ClipCurveEditPayload::RemoveMany lowers to the existing Clip/Effect numeric
curve author transaction and one Core RemoveKeyframes mutation. It carries
stable IDs and an explicit exact owner retain_time, validated against the Clip
owner interval. Both numeric product action handlers update selection only
after a successful commit, removing every deleted ID from the addressed
property while preserving surviving keys and other property selections. Empty,
duplicate, stale or incomplete sets do not publish author state or history.

Visual numeric CurveEditor owners enable selection deletion explicitly. Shift
click toggles authored keys; Ctrl/Cmd+A selects all admitted authored keys,
including offscreen keys in the current owner interval. Virtual evaluated
boundaries never enter the deletion set. Selection snapshots carry stable IDs
and restore surviving keys after canonical reordering or replacement. Painting
and accessibility expose the entire set. Escape clears the set without an author
edit. Plain navigation/click explicitly replaces selection with one point.

CurveEdit::DeleteMany carries the complete set through one canonical preview
and the existing Clip/Effect transaction. The Inspector projection carries the
current exact Clip author time; removing every key retains its original value
at that time. Local geometry and selection roll back together when the canonical
preview rejects the set. Accepted previews reconcile virtual boundaries.

Group pointer/value/time edits, rubber-band selection and group interpolation
remain to be connected. Arrow edits and keyboard interpolation do not modify
only the primary point while multiple keys are selected. Audio owners do not
enable UI selection deletion until the current occurrence time is explicit;
shared Scope time cannot be inferred from Sequence playhead alone. The canonical
batch audio author action remains available with an explicit retain time.
No widget-owned curve replacement or loop of independent author commands is
introduced.


### Audio curve selection current-time context

Audio automation projections require AudioAutomationTimeContext containing the
current domain-qualified Sequence time. Mixer faders, routes and processor racks
use AppState's current timeline coordinate. Inspector Component curves use the
same live coordinate. A selected Clip Scope rack supplies its Component address
directly when that Clip has exactly one binding of the Scope. Multiple bindings
in the same Clip expose a localized per-Scope time-reference Dropdown. No
occurrence is selected implicitly. A closed SelectScopeOccurrence product action
validates the active Sequence, inspected Clip, Component address and Scope binding
before updating Session-only SelectionState navigation. Choices are retained
independently for Scopes in the inspected Clip, never in the Project or Undo.
The retained navigation set is bounded to the last explicitly navigated Clip;
clearing selection clears it. Read-only Track navigation remains available while
canonical shared-Scope author lock admission still rejects curve mutations.
No global first-binding or viewport-origin fallback chooses retention time.

The model retains Timeline's Result<DomainTime, AudioAutomationTimeError>.
CurveEditor selection deletion is enabled only for an admitted current owner
time. DeleteMany uses the canonical AudioAutomationEdit::RemoveKeyframes and one
author transaction, including original-value retention when every key disappears.
Preview uses the existing Timeline edit preparation and evaluator. Current-time
projection does not create history; underlying shared Scope lock checks remain
whole-definition admission. Group movement/interpolation remain open and are not
presented as completed professional qualification.

Audio view identity includes the explicit Scope occurrence's Clip/Component IDs,
excluding transient playhead time and Track locator. Playback and cross-Track
refresh preserve selection in the same occurrence; changing occurrence cannot
inherit another occurrence's view state. Navigation resolves stable IDs against
the current Clip's actual Scope bindings to obtain the current Track address.
This does not relax full-address admission for queued author/navigation actions. If a refreshed widget
retains multiple selected IDs but loses batch admission, Delete consumes the
input without emitting a singleton edit, changing geometry or losing selection.
Production time-context construction is shared by Mixer and Inspector callers.


### Exact keyboard movement of selected curve keys

Temporal curve owners explicitly admit `NudgeTimeMany` alongside stable selection.
Left/Right applies one exact owner frame-grid increment to the whole key set;
Shift uses five increments. Transfer curves do not acquire this capability by
restoring a view snapshot. Capability loss, a locked time policy or an active
pointer gesture consumes the key without falling back to the primary key.
The Widget never reconstructs author times or values from projected points.

Clip and audio owner adapters lower the closed group edit to the shared canonical
primitive. Clip preparation checks the final owner interval before one Author
Transaction; audio projection checks its exact occurrence viewport. Refused
projection emits no author action and leaves the key set and geometry intact.
Successful keyboard movement is one reversible edit and preserves all stable
identities; pointer group movement, value changes and interpolation use separate
future group edit contracts. Selection and author history remain session state,
while saves persist the exact canonical shifted curve.

Selection is admitted when either whole-set deletion or whole-set time movement
is supported. Shift-click and Ctrl/Cmd+A select authored keys independently of
deletability; virtual boundaries never join the author key set. Deletion and time
movement each validate their complete set against their own capability and point
policies. A time-only owner therefore does not acquire group deletion merely to
make its keys selectable. Read-only selected keys cause whole-set edit refusal,
not silent omission or a primary-key fallback.

### Audio draft admission and matching actions

The audio curve model retains the definition-backed inspection contract.
`prepare_audio_curve_edit` lowers one widget intent and admits the resulting
exact curve through that contract. `preview_audio_curve_edit` produces an action
and sampled display points from this one prepared candidate, installing its
local source only after projection succeeds. The panel never separately lowers
an action from a different curve candidate. Interpolation menus use the same
admission before emitting an action.

Illegal gain/pan controls created by constrained neighbor crossing are refused
before preview or author dispatch. Group selection remains intact; refusal does
not create a generation or Undo entry. Legal crossing preserves stable IDs,
uses one author transaction, and retains normal Undo/Redo and save/reopen
semantics. Commit resolves the current owner and definition again; a stale UI
snapshot does not bypass current locks or author-session admission.

### Native group curve qualification contract

The ignored App native group test submits the production `ShiftMany` action for
Auto/Continuous Bezier keys. Its dedicated OpenFX SessionProbe mode checks exact
key times, indices and values during native instance lifecycle callbacks,
compares derivative queries to symmetric value samples, and compares integral
queries to independent Simpson quadrature split at exact key boundaries.
Native output consumes the queried curve value at off-key frame 3. Rust
compares it with the canonical constrained author curve and proves the moved
curve differs from a linear downgrade, so the accepted group move must change
rendered pixels without losing its shape. Collision refusal, one Undo/Redo and
fresh undone/redone Unicode project archives use the same plugin execution.
This test requires an explicitly rebuilt product and fixture; an ignored result
or an earlier single-key native test does not qualify the group operation.


Curve group value keyboard editing has an independent selection-value capability.
Shift-click and Select All establish stable authored-key selection when any group
operation is admitted; each operation still gates its own capability. Up/Down
submits one `NudgeValueMany` intent after checking the complete selected set's
value locks, with Shift requesting five increments. Canonical preview owns values
and geometry. Visual payloads prepare a common Core property mutation; audio
payloads use the shared definition-admitted exact curve contract. Refusal cannot
publish a partial key update. Bundled keyboard help describes shared boundary
stopping in both locales.

Point policy explicitly distinguishes time and value locks. Virtual viewport
anchors retain their existing editable values while locking time; normal author
keys default to both axes editable. A value lock constrains pointer projection,
suppresses singleton value increments and rejects complete group increments
containing that key. Selection and deletion permissions remain separate.

Curve previews suppress author intent when the complete candidate equals the
previous live display source. The comparison snapshot is taken before resetting
an active pointer gesture to its origin, so returning a changed gesture to that
origin still publishes a real update. Boundary keyboard no-ops therefore do not
surface an unexecuted-action error or alter history.


Native group curve qualification shares one fixture runner for exact-time and
common-value edits. Value editing retains all author times and the unselected
first key, changes the selected values together, and checks native off-keyframe
pixels against canonical evaluation through the shared compiled graph. The owned
OpenFX fixture verifies key count/time/index/value and independent derivative
and integral queries across instance, edit, render and destruction actions.
Both Undo/Redo states and fresh Unicode project reopens use real native execution.
Each mode needs its current explicitly rebuilt product and fixture qualification;
an ignored test or earlier time-mode receipt is not value-mode evidence.

The App animation selection resolves manual asymmetric Linear/Bezier sides as
EaseIn or EaseOut through the Core resolver. Current-mode and available-preset
menus therefore retain the authored side intent for complete selections.


CurveEditor accepts a stable-selection menu factory invoked only on menu opening. Right-clicking a selected key retains the complete group; Shift+F10 shares the same command factory. Active pointer drafts refuse menu opening. Visual and audio adapters use definition-admitted batch requests, disable equal candidates, keep localized labels and resolve mixed selections without a checked preset.

Group menu selection permission is independent of deletion and coordinate movement;
Ctrl/Cmd+A and Shift-click remain available when only interpolation is admitted.
Active drafts refuse right-click and keyboard menus, and cancellation restores
normal menu access. Author dispatch revalidates stale keys and late owner locks.

Both singleton and complete-selection authored menus use the opening-time
factory. UI reconstruction only prepares localized labels; it does not prepare
seven candidates for every authored key. Preset admission is bounded to the
one addressed selection when opening the menu, avoiding quadratic rebuilding.


CurveEditor can own focused Ctrl/Cmd+C/V with on_clipboard and a typed
CurveClipboardCommand. The callback receives the current complete stable authored
selection. Empty Copy, active pointer drafts and active author gestures consume
these commands without dispatch or a global shortcut fallback. Exact primary
modifiers are required; unfocused editors do not own clipboard chords. The visual
adapter lowers both keyboard and lazy localized context-menu commands to the
same typed product request; reusable widgets retain no application clipboard or
second author interpretation. Paste may target an empty selection.

Owner-provided stable key selection is a view projection. CurveEditor retains its
previous projected set alongside the inactive view snapshot: a changed set selects
the newly published keys, while an unchanged set preserves current local selection.
Projection order does not create a selection change. Missing keys are excluded
before comparing projections, so Undo/Redo disappearance and restoration trigger
the appropriate selection change;
the same owner keeps widget identity, focus and viewport. A changed selection clears
obsolete tangent selection. A different owner or author session cannot inherit the
old view. This projection changes no author state and adds no clipboard authority.

### Lossless visual curve clipboard

Focused and global visual key Copy/Paste share the sole private App clipboard.
Its transient payload stores complete canonical channel groups, preserving raw
author values and per-channel metadata. The App renews one identity per complete
property key and rebases exact owner time, then delegates batched admission to
Core. Focused paste validates the entire owner extent before author publication.
Selection changes only after the single author commit; refused paste retains
clipboard, selection, history and generation. Undo and saved Redo use normal
author history and durable project snapshots, without serializing clipboard UI
state or introducing a second animation interpretation.

### Focused curve Cut

Curve clipboard commands explicitly carry Copy, Cut or Paste plus the current complete stable selection. Exact Ctrl/Cmd+X is consumed by its focused owner, including empty selection and active drafts. Numeric intrinsic/effect curve menus dispatch the same closed owner-qualified Cut request. Capture preparation is pure: the App resolves every raw key first, then removes the group in one author transaction and publishes the sole clipboard only after success. Track locks, stale owners/keys and incomplete selections retain clipboard and history. Removed selection entries are cleared only for the addressed property; other parameter selections remain intact. Undo and saved Redo use ordinary author history; clipboard content remains transient and is cleared on fresh project reopen.

### Command menus and editing focus

Root Cut/Copy/Paste rows carry a semantic `EditCommand`, rather than dispatching
an application action before identifying its editing owner. `EventRouter`
suspends the original Widget focus while a menu-bar Dropdown receives input.
Opening command chrome preserves the editor's selection and text draft. Closing
or cancelling restores that owner; switching to another control, losing window
focus or entering a modal ends the appropriate original editing session. Active
pointer drafts still receive their ordinary focus-loss termination.

The router applies menu side-effect requests after the originating widget call,
then invokes the original control's command implementation. Missing owners and
owners outside the active modal scope refuse without an application fallback.
CurveEditor resolves complete stable keys through the existing closed App command;
TextInput and MultilineTextInput reuse their platform clipboard and text editing implementations. All
consume owned refusals, including empty selection and failed clipboard writes.
Controls without an owned command retain the application's authoring path.
Resolved global shortcut actions use this same seam, including user overrides.

Control-owned availability is a transient presentation projection into semantic
menu rows. Painting, keyboard navigation and activation use the same effective
readiness. This projection stores no clipboard content or project interpretation;
author mutations still validate the fresh exact owner and commit atomically.

Menu commands use one typed request slot. Non-clipboard menu actions and resolved
global shortcut actions confirm the original control's pending input before
dispatch, so a save snapshot observes confirmed text. Cancellation preserves the
draft without publishing author edits. Suspending a text editor cancels provisional
IME preedit before disabling native IME; confirmed draft text remains intact.

Multiline command availability also preserves read-only semantics: selected text
can be copied, while Cut and Paste are owned refusals. Both text widgets reuse
their existing editing commands and local history rather than interpreting menu
commands through application Clip actions.

### Captured menu sibling coordination

MenuBar and menu-bar Dropdowns identify command chrome through the same Widget
metadata. Captured command menus permit ancestor coordination only inside command
chrome; unrelated splitters and window chrome cannot preempt the captured input.
MenuBar uses the existing before-child seam to switch sibling popups on hover or
click, with the same helper used by direct events. A captured command menu receives
keyboard focus through the router's ordinary focus lifecycle. Original curve/text
ownership remains suspended until execution or cancellation, so sibling switching
preserves complete selection and one-author-transaction clipboard semantics.
Dropdowns process focus gained/lost independently of popup visibility; focus loss
closes the popup and clears its focused affordance for enabled and disabled controls.

Dropdown and ContextMenu share depth-aware keyboard navigation in `menu::popup`.
Enter/Right opens a submenu; Up/Down/Home/End selects an enabled row at the active
depth, and Left/Escape returns one level. Activation requires the selected leaf;
parent rows never recursively execute their first descendant. Empty, disabled or
invalid paths consume activation without falling through. Unhandled root Left/
Right bubbles to MenuBar's sibling navigation and preserves the editing owner.
Explicit pointer submenu entry shares the same depth transition.

Dropdown opening selects and displays the first enabled row. Mouse-opened
ContextMenu starts with no selection until hover or keyboard navigation chooses
a row. Mouse hover may clear the selection; Enter/Space with no highlighted row
is an owned refusal and never falls back to another command. Activation always
matches the current painted row, independently of how the popup was opened.

### Focused audio curve clipboard commands

Audio CurveEditor callbacks and localized local Cut/Copy/Paste rows lower the same
closed AudioCurveClipboardPayload as root edit commands. Stable Sequence/target
identity and explicit shared-Scope occurrence accompany complete key IDs. The App
uses its sole clipboard, performs fresh author admission and publishes Cut capture
only after one successful author edit. Paste publishes fresh selected IDs through
the existing AnimationSelectionState after commit. Panel-model composition joins
that typed Sequence/target selection to all audio curves; CurveEditor's existing
selection projection replaces local selection only when the explicit group changes.
Missing Undo keys filter out at the widget boundary and restored Redo keys reappear.
This does not put selection into author history or introduce a second clipboard.
Command readiness and the wider owner/native matrix remain acceptance work.

### Read-only curve inspection

CurveEditor uses independent input availability and authoring permission.
`enabled` governs focus and general input; `editable` governs changes to points,
values and tangents. Locked author models use a focusable read-only curve rather
than disabling the entire inspection surface. Stable selection, Copy, keyboard
navigation and viewport zoom/pan remain available. Cut/Paste, deletion, nudges,
point insertion and pointer drags are owned refusals without local geometry
mutation or fallback to Clip commands. Root edit readiness follows the same
permission. Owner-provided menus disable their own interpolation and mutation
commands while retaining Copy.

Revoking author permission restores an outstanding pointer draft and defers its
balanced gesture cancellation/pointer release to the next routed event. It keeps
inspection focus and selection. UI-core AccessibilityState exposes `read_only`
separately from `disabled`, so the accessible canvas reports its actual capability.
Audio, numeric visual animation and curve-shape controls share this widget policy;
canonical author admission remains the final mutation boundary.

Read-only help is localized separately for inspection-only shapes and
clipboard-capable animation curves; instructions do not advertise blocked edits.
The native window-role fixture uses an explicit temporary preference path. Its
actual Windows cycle checks fixed Startup/resizable Workspace, remembered normal
size/maximization, stable Window/Surface/Device authority and bounded resource
shutdown independently of the user's machine preference and plugin selections.

### Inspection survives author-lease invalidation

A forced curve replacement after owner lock or intervening author edits retains
only inspection state matched by the existing panel and exact owner view key.
Stable key IDs, viewport and focus can survive an outstanding author gesture;
transient point geometry and gesture callbacks cannot. Non-key point selection
is transferable only from an inactive view with identical geometry. Changing
the inspection owner still discards the old widget identity.

Reusing the inspection widget ID also carries an obligation to release the old
pointer capture on the next routed event. The replacement has no author gesture
to finish or mutate. This keeps focus usable for read-only Copy while terminating
the stale drag through the existing capture protocol. Active viewport pans and
menus remain excluded from view snapshots.

### Stable group pointer transforms

A grab on an already selected authored key retains the complete stable key set.
The widget captures the pointer and viewport origin and emits one joint exact
owner-time and author-value displacement, rather than moving keys independently.
Each preview and author draft is derived from the existing gesture origin. The
owner admits the complete detached candidate before preview state is published;
a refused motion retains the previous admitted draft. The existing author session
seals one history entry, or restores the original state on cancellation. The
widget's cancellation geometry is transient inspection state, not a second
source of authored keys or curve evaluation.

Numeric visual and audio adapters lower the same TransformMany UI intent into
closed domain edits. Core prepares final topology and tangent constraints once.
Shared selected-key locks apply to the whole displacement. Owner interval checks
remain separate from viewport scaling, and readonly inspection cannot begin a
transform. Clipboard commands are consumed during the active captured draft.

### Retaining group identity after commit

Successful numeric group transforms publish the complete stable selection through
the existing App selection authority. ClipCurveEditPayload's retention and
removal policies use exhaustive variant matches; adding a new edit cannot silently
fall back to single-key selection. The UI's temporal ordering and the App's
normalized identity ordering represent the same set. Undo and forced owner
projection retain inspection by stable identity rather than pointer coordinates.

Changed admitted curve drags follow the established terminal policy: Escape
cancels and restores author/preview state, while FocusLost completes the last
admitted candidate as one Undo. Net-zero group drags produce no history entry.
Capture and gesture ownership terminate in either case. Invalidated author leases
are canceled through the existing author session before view replacement.

Audio live projection includes exact coordinates for all known authored key IDs,
including those moved outside the visible interval. Painting still clips the
current viewport. This prevents a moved key remaining as a stale edge point and
does not extend the Component author bounds or invent another curve evaluator.

### Pure authored-key marquee inspection

Temporal curves with stable-key selection use blank-space left dragging for
marquee selection. Plain input replaces the selection, Shift adds, and
Ctrl/Cmd+Shift toggles against the frozen selection at pointer press. Selection
is calculated from visible stable-key centers and clipped logical plot geometry;
virtual viewport boundaries are excluded. Ctrl/Cmd clicking a key toggles it,
while Ctrl/Cmd clicking blank space explicitly inserts an authored key. Transfer
curves retain their point-editing interaction because they have no authored-key
selection contract.

Marquee capture changes only the existing widget inspection selection. It never
opens an author gesture, emits a parameter edit or consumes an Undo entry.
Escape restores the original stable selection; release retains the final set.
Read-only curves expose the same inspection behavior. Nonfinite pointer events
leave the last admitted selection intact, and repeated movement re-evaluates
the frozen origin rather than cumulatively toggling keys.

Owner replacement transfers the pre-marquee inspection and an obligation to
release the previous widget's capture, without transferring drag geometry or
callbacks. Programmatic geometry replacement discards the selection capture.
Changing plot layout during selection restores the original selection and
defers capture release to the next routed event, preventing stale rectangle
coordinates after native window resizing. Distinct exact key identities that
share a projected center remain individually included by the marquee.
The selection rectangle consumes semantic theme colors and border spacing;
English/Chinese help exposes the insertion and modifier policy. Existing
complete-selection author actions and clipboard dispatch consume the resulting
stable IDs, so marquee does not introduce an alternate author or selection
service.

### Notification history keyboard and accessibility

The retained StatusBar history is one focusable, nonmodal inspection surface.
Enter/Space toggles it. Its displayed order is newest first: Down/PageDown show
older entries, Up/PageUp show newer entries, Home selects the newest viewport,
and End the oldest viewport. Movement is bounded and consumes only unmodified
keys while this widget owns focus; inspection never dispatches editor actions.
Escape and FocusLost dismiss the popup. Navigation requests repaint only when
the viewport changes. The existing history remains the sole message authority.

Open accessibility projection includes current status, page range/count, every
full localized visible message, error severity and keyboard instructions. Opaque
user paths and diagnostic details remain intact; only painted text is elided.
English/Chinese catalogs and pseudo-locale use the same message identities.
The heading exposes compact keys and elides within the existing panel bounds.
Empty/short histories and replacement snapshots clamp the same viewport state.

The toast overlay still displays at most two terminal facts. Admission preserves
errors ahead of warnings and warnings ahead of successes; equal priority retains
the newest facts. Suppressed low-priority facts are observed without formatting,
allocating a toast, requesting repaint or replaying after higher-priority expiry.
The feed remains bounded and detailed status history remains available. Existing
5/8/10-second lifetimes, per-operation deduplication and locale projection apply.
The history painter consumes theme panel padding and rounding, clips to its
popup bounds, and elides title, empty state and message rows to the same content
width. Full accessible messages are independent of paint elision. A narrow
pseudo-locale/custom-padding case checks this separation with opaque Unicode
paths; it is widget draw-command evidence, not native screen-reader qualification.

### Parameter reset affordances

Inspector Clip, Effect and Mask property rows expose a localized Reset button
using the existing stable owner/write action. Complete author-state projection
disables the button at the definition default, including retained-animation
awareness; owner read-only state also disables it. Audio curve context menus
expose the same localized reset intent, while static processor controls expose
a button, including non-animatable integer parameters. The current definition
is admitted again at author execution; UI does not reconstruct defaults or own
an alternate reversible snapshot. Reset is independent of curve key selection.

Curve context-menu factories also receive empty selections for parameter-level
commands. Shift+F10 anchors an unselected menu to the plot center; blank-area
right-click retains selection and uses the same factory. Empty and virtual-only
curves remain inspectable. Existing pointer-draft and author-gesture guards block
menu opening until the interaction ends; the factory admits enabled commands.

### Menu semantic inspection

Widget semantic visitation derives virtual menu items from the existing shared
MenuItem model without inserting extra hit-test, focus or capture widgets. The
collector retains semantic parent IDs and real keyboard focus order. Dropdown
and ContextMenu share the projection; items have lazy control-scoped identities,
checked/unchecked/non-checkable states, effective disabled state and open submenu
ancestry. Only open branches are emitted. Renaming/localizing labels retains IDs
and interaction state; replacing a Dropdown model invalidates its item IDs.
Curve, asset, timeline and shortcut owners include their popups in visitation.

Application adapters provide localized popup names; ContextMenu has no invented
English default. Timeline naming comes from the current application catalog.
Semantic inspection and relabeling publish no editor action. This is the widget
semantic contract; a native assistive-technology bridge and physical screen-reader
qualification remain separate work. No native accessibility publication is
claimed merely because these nodes can be collected.


### Cached audio processor input controls

The shared Rack projection gives native processors a localized input query row
in Inspector and Mixer. It binds exact session/Sequence/owner/processor intent;
shared Scopes require an explicit occurrence. Unknown/ready/failed states offer
inspect or refresh, while pending state offers cancellation. Widget construction
only reads App's transient observation cache and never resolves a native factory.
Host polling refreshes the model when bounded control work terminates. Existing
modal focus policy applies to these ordinary Buttons. Querying locked authors is
read-only; future routing controls must use Timeline lock and cycle admission.

The input status row remains mounted across unknown/pending/ready/failed states,
so a reply or catalog invalidation updates copy without shifting parameter rows
and their restored focus/drafts. Native control work contributes a 25 ms maximum
idle wake delay, including canceled work until physical termination; no forced
continuous redraw or synchronous native probe is added.

### Shared processor sidechain controls

Inspector and Mixer share `audio_sidechain` route projection and typed action
construction. Every retained route appears independently with enable, exact
static gain and disconnection. Sidechain gain curves use the same exact
`RouteGain` projection, curve widget, selection and authoring actions as principal
route gain curves. Their viewport and current author time belong to the Sequence,
including routes into a Processing Scope; a Scope parameter viewport must not
reinterpret a route's time. Automated gain refuses scalar replacement. Source
locks remain authoritative even for unavailable native inputs, and editing a
curve does not require a native input observation or load a plugin.
The shared Rack formats linguistic parameter units through the current Localizer
and Fluent catalogs (samples, exposure stops and nits); standard symbols such as
dB, ms and percent remain invariant. Native parameter names stay plugin-owned.

Scope input inspection binds the exact Track/Clip/Component occurrence as well
as the shared Scope and processor. Another valid occurrence cannot reuse its
receipt; deleting or rebinding the selected occurrence refuses connection and
rewiring while other materializations remain. Native workflow references include
Scope bindings at 5 and 7 seconds over overlapping Sequence placements at 0–1
second. VST3 uses distinct Scope-time gains; the state-only CLAP reference uses
distinct placement gain. Route gain curves retain Sequence signal time in both.
Native cached input contracts offer additive source menus, admitted by Timeline
complete retained routing graph and source locks. Projection loads no native
factory and creates no alternate author/routing state.

Connection source MenuItems carry the exact cached input binding and lower into
the closed `ConnectProcessorInput` product action. They do not lower directly into
an unqualified route creation request. App checks the live observation identity
before entering the sole routing transaction, so a stale popup cannot silently
connect a removed/replaced native input. Existing retained route controls remain
available under canonical source locks without requiring the plugin to load.


Rack move/remove icon Buttons and sidechain scalar/source controls use the same
primitive inspection identity as parameter controls. Sidechain subjects name an
exact canonical RouteId or the native definition's stable input bus key; source
endpoints, captions, gain/enable values and display order do not enter the key.
Keys retain the exact Rack, native processor definition binding and selected
Processing Scope occurrence. Each refresh rebuilds live callbacks and current
input-observation/source fingerprints. Deleted or disabled controls refuse input
identity restoration. Scope occurrence navigation instead belongs to its Rack
and originally inspected Clip: choosing another component keeps that selector's
keyboard trigger, while the chosen component's parameter/route controls become
new occurrence owners. Inspecting another outer Clip replaces the selector owner.


Release-activated icon Buttons additionally inspect one typed Enter/Space
keyboard activation lease. It is admitted only by the same enabled semantic
key/role and existing live session/Sequence/panel gates. A matching release
consumes it once and executes the newly projected callback. Locale and native
metadata refresh may retain that lease without delaying refresh. Raw pointer
press, hover, capture, callbacks and draft state remain outside the snapshot;
pointer-pressed icon Buttons refuse inspection restoration. Focus loss,
disabled/deleted/rebound/other-occurrence owners and modal entry cancel the lease.
Every shell modal entry passes through the root's `show_modal` boundary, which
invokes the primitive `cancel_keyboard_activation` seam throughout the current
widget tree before installing the modal. Cancellation clears only an owned
keyboard release intent and its pressed visual; focus identity and independent
pointer interaction remain unchanged. This boundary also covers Resource and
pending-close dialogs and applies even when a modal opens and closes before the
Router receives another input event or the host refreshes models. Model rebuild
only restores already admitted inspection state. Immediate-activation
widgets do not accept a release lease from a replacement widget kind.


### Numeric author draft inspection

An explicitly addressed compound numeric control gives its Slider and NumberInput
distinct role keys under the same stable author owner. NumberInput uses the shared
inspection collection, duplicate-key refusal and session/Sequence/panel gates;
it introduces no alternate focus authority. Parameter binding includes its exact
canonical ParameterSchema, independent of translated labels and current value.
The Host's existing transient-input gate permits model rebuild for an admitted
typed numeric draft, so focus cannot indefinitely postpone fresh metadata or
canonical owner validation. Unaddressed text, pointer selection and IME preedit
keep the existing transient input policy.

A focused NumberInput may retain committed-input text, grapheme-based selection
and caret across a model rebuild only when the fresh enabled numeric model has
the same exact typed committed baseline, hard bounds, configured step and display
precision. Floating bits and i64 storage avoid narrowing or approximate equality.
Native metadata and locale-only refresh therefore preserve an unfinished draft.
Canonical value changes, schema changes, locking, deleted/rebound owners, another
Scope occurrence or a new author session refuse it. Fresh models and callbacks
remain authoritative. Enter publishes at most one edit through the current action;
Escape restores the fresh canonical baseline without author history.

The TextInput helper retains local single-line text/selection/caret and bounded
local history; it
refuses pointer selection or active IME preedit and never restores callbacks,
pointer capture, composition, clipboard intent or author Undo history. NumberInput exposes
its actual inner text-field accessibility node rather than disappearing from the
semantic tree. Localized numeric names/units/ranges and native assistive-technology
publication remain separate work and are not claimed by this draft-retention seam.


### Numeric text Edit command ownership

NumberInput delegates semantic Edit-menu availability and Copy/Cut/Paste execution
to its owned TextInput through the existing Widget/Router command seam. These
commands operate on local draft text and selection; numeric parsing and author
history still occur only on numeric confirmation. No-selection Copy/Cut is
unavailable, failed clipboard operations and empty Paste preserve the draft and
selection, and an owned command cannot fall through to generic Clip actions.
The existing numeric Escape, fresh-model admission and single-commit behavior
remain the numeric transaction boundary.


### Shared text platform keymap

Single-line and multiline text command classifiers share the existing explicit
Windows/Linux/macOS PlatformKeymap and modifier helpers. The real widget adapter
selects the current platform; classifiers accept a supplied platform so Mac
semantics are executable on any test host. NumberInput uses the same TextInput
adapter. Clipboard/select-all use exact primary modifiers (Control on Windows
and Linux, Command on macOS), while word arrows use Control or Option. macOS
Command arrows address line boundaries and multiline document boundaries; Shift
extends selection. Conflicting secondary modifiers remain available to shortcuts
or the system. Raw native modifiers and the sole EventRouter are unchanged.

Platform decisions inside a supplied-keymap classifier use that keymap rather
than the build host. These changes reuse existing text selection, Unicode word
and grapheme movement, clipboard failure handling and numeric author commit
boundaries. They do not claim native macOS event/IME/assistive-technology
qualification or implement app command defaults; application shortcut defaults
remain a separate boundary.


### Local text history and word deletion

Single-line and multiline inputs use one bounded text/caret/selection history
engine: at most 100 entries and 16 MiB of retained UTF-8 snapshot allocations,
with cached byte accounting and oldest-entry eviction. The Host asks an allocation-free
Widget admission predicate for its transient-input gate; NumberInput capture and
that predicate share the same addressed enabled/focused/pointer/IME guard. Actual
Shell capture still applies unique-key, owner/session and fresh-model admission.
History is captured once into the existing boxed payload; fresh replacement
controls have no owned history to copy while their keys are counted. A single oversized edit
cannot retain its local Undo snapshot; its current draft still stays intact.
Local history retains explicit unconfirmed input ownership even after memory
or count eviction, or Undo exhaustion, so empty history never falls through to
Clip Undo. Coalescing requires uninterrupted text/caret/selection endpoints;
Undo/Redo, discrete clipboard/IME edits and restored models break coalescing.
Redo restores the actual caret and reverse selection that existed before Undo
in one step, without synthetic tip entries.

History contains only local text and grapheme coordinates. An admitted numeric
inspection snapshot preserves its bounded Undo/Redo branch and ownership under
the existing exact baseline/schema/session gate. Fresh callbacks are always
used. Canonical baseline changes refuse the entire draft and history, while
programmatic single-line synchronization resets them without author dispatch.
Numeric Enter confirms at most one current value and clears local history;
Escape discards it without author history. A pristine or confirmed NumberInput
permits author Undo through the existing Router; an unfinished interaction owns
local Undo even if the draft text has returned to its baseline.

TextInput's existing `on_change` contract still dispatches every resulting text
change through its fresh callback, including local Undo/Redo. Transactional
`on_commit` text and numeric draft changes remain local until confirmation.
This history never mutates an author document or copies author transactions.

Exact primary Copy/Cut belongs to an enabled, focused text control even when
there is no selection. Its keyboard result is an owned no-op, menu availability
remains false, and no parent/global rebinding receives that text chord. Readonly
multiline controls retain Copy and owned no-op mutation commands; disabled or
unfocused controls do not gain shortcut ownership. Word Backspace/Delete uses
Control on Windows/Linux and Option on macOS with no secondary modifier or
Shift, gives an existing selection precedence, and otherwise uses the existing
Unicode word/grapheme boundaries. Multiline deletion at a line boundary joins
only that adjacent newline. Each deletion is one local reversible edit.

### Complete text layout and viewport output

Single-line input geometry obtains complete line width, logical UTF-8 caret position and pointer hit from one advanced cosmic-text layout per query, using the shared widget font owner. It no longer reshapes every grapheme prefix to find a mouse caret. Draft text, kerning, ligatures and bidirectional shaping context remain complete. Layout scratch is proportional to the active complete line and released after each query; this slice does not claim viewport-only shaping or a persistent layout cache.

Text resolution tracks the command stream's nested clip and translation semantics and tests actual glyph ink bounds before atlas allocation/upload and image emission. An empty effective clip skips shaping. Off-viewport ink is omitted without substring reflow; only visible ink can force an atlas page reset. Swash image cache misses still rasterize off-viewport glyphs to obtain exact ink bounds, and `glyphs_requested` retains that complete request count. This slice bounds alpha copying, atlas allocation/uploads and draw commands, not all rasterization or full-line shaping. Public direct layout/render without a clip keeps its full-output contract.

The compatible vendored HarfRust 0.5.2 backports upstream PR 397's 32-bit matched sequence index fix, with exact source/license provenance. It supports complete long lines without disabling overflow checking, downgrading advanced shaping or truncating drafts.


### Complete single-line selection geometry

TextInput paints selected visual advance spans and anchors IME preedit at the
caret from one complete advanced cosmic-text layout query. Its paint snapshot
carries only typed pixel spans and the complete caret position; it no longer
reshapes a selected prefix or substring. A logical range can produce disjoint
BiDi rectangles. Partial clusters use the same equal-grapheme advance convention
as cosmic-text caret geometry, with each glyph's own BiDi direction. Contiguous
advances coalesce before allocating per-span output, including long selections.
The existing shared widget font owner and text renderer remain the sole shaping
path. Public selection requests refuse malformed/out-of-range byte boundaries
and invalid font sizes before shaping; empty selections yield no rectangles.

Selection and caret movement preserve the existing callback/Undo boundary:
navigation creates no text edit or history entry, text replacement creates one
local entry, and Undo restores the original caret and reverse anchor. Ordinary
live TextInput Undo still dispatches the resulting text through its existing
fresh on_change callback; transactional numeric/commit inputs remain local until
confirmation. This slice covers single-line geometry, not full visual BiDi
keyboard traversal, native macOS IME qualification or multiline interaction.


### Associated visual single-line carets

Single-line caret and selection endpoints carry a logical grapheme offset plus
the adjacent logical cluster supplying the visual position. This typed UI
position replaces bare grapheme offsets in local Undo and numeric inspection;
it contains no font, layout pixels, native handle or author timeline state.
Fresh draft/history admission validates both offset and edge association before
copying bounded snapshots. Canonical model replacement still refuses old input.

The sole complete advanced cosmic layout projects streaming visual grapheme
cells from its shaped cluster advances and glyph BiDi levels. Pointer hit,
caret, selected spans and visual navigation share this projection, including
partial ligatures, combining/ZWJ graphemes and zero advances. No independent
prefix shape, paragraph-direction arrow flip or retained all-caret vector is
used. Adjacent glyphs belonging to one logical cluster share its advance cell;
ligature subdivision follows the existing equal-grapheme advance convention.
Plain arrows traverse one visual cell; unshifted arrows collapse a selected
range at its actual visual edge without another character step. Shift retains
the original associated anchor. Home/End retain logical line boundaries,
including portable Mac Command-arrow classification; word arrows apply the
existing whitespace boundary policy in visual order using a fixed number of
streaming scans. Backspace/Delete remain logical grapheme/word editing.

Logical association survives local replacement Undo/Redo and admitted numeric
draft refresh through the existing typed inspection seam. Navigation and
selection alone dispatch no text callback/history entry. Existing live
on_change and numeric Enter/Escape transaction boundaries remain authoritative.
Insertion and removal derive the resulting caret from the complete text's
grapheme boundaries at the changed byte endpoint, snapping forward when a
combining mark/ZWJ joins neighboring graphemes. Fragment grapheme counts never
advance the cursor independently of the complete result.
Typed geometry admission rejects nonrepresentable line-height metrics before
font initialization and refuses nonfinite complete output geometry. Complete
shaping scratch remains proportional to the full line; this does not claim
viewport-only shaping, native macOS IME/AT or multiline qualification.

### Exact source ingress and single-line visual projection

Programmatic TextInput `with_text`/`set_text` synchronization preserves exact
authoritative model bytes, including line separators. The definition's single-line
mode is a display/input contract, not permission to normalize an authored value.
Focus baselines, Escape, local Undo and unchanged confirmation retain raw source;
programmatic synchronization resets local history without a user callback.
User typing/IME commits/paste keep the existing edit policy: CR/LF, CRLF and
Unicode line/paragraph separators become spaces, while tabs/NBSP are retained.

The sole SingleLineTextProjection borrows original source and derives complete
one-line display content, mapping only separator length contractions. Rendering
geometry shapes that entire display once and derives its VisualCells against the
original source EGC intervals. Caret/selection/hit/navigation endpoints therefore
remain original source bytes and BiDi association, including CRLF and distinct
source combining/ZWJ stops joined only by display projection. Actual TextInput
paint uses the same projector; other layout modes are unaffected. There is no
prefix shaping, secondary navigation fallback or second author text authority.

This corrects the earlier canonical programmatic-ingress premise. Full Inspector
qualification exposed legitimate native single-line author values with newlines;
the original long-text and Unicode word-navigation contracts remain unchanged.
Numeric inspection DTO/history admission remains strictly canonical before copy;
it never normalizes untrusted raw text and guesses saved positions. Projection
scratch is proportional to display content plus length-changing separators; normal
single-line text remains borrowed. Full shaping, native IME and assistive technology
qualification remain the existing separate limits.

### Owned Timeline and Inspector status localization

Timeline and Inspector empty/invalid model fields retain the existing typed
AppMessage identity and arguments rather than formatted strings. Actual panel
construction resolves them through the current Localizer, so a retained model
can be painted in another UI locale without author mutation or stale-language
copy. Empty sequences remain editable add-track canvases; absent/invalid
sequences retain their disabled behavior. Invalid display context wraps the
existing typed core error projection; opaque diagnostic arguments, user clip
labels, title contents and sequence names remain unchanged. Catalog-presence
tests cover the requested ZhCn, EnUs and Pseudo bundles without treating fallback
as translation coverage; actual draw-command tests cover titles and guidance.

TimelineView paints an explicit status when its body is empty or when the
timeline is unavailable despite retained tracks. For the latter it covers the
unavailable fallback grid with the existing semantic body surface before painting
the localized title/reason. Track models and IDs remain intact; input availability
is unchanged. A valid populated canvas never displays empty guidance, and a
disabled canvas without an explicit message retains its ordinary clip paint.
Actual paint regressions cover the nonempty invalid display in all three locales
and both available/disabled positive controls.

### Rack scalar author interactions

Audio Processor Rack scalar controls now carry an explicit fresh author session
and Sequence lease, exact instance definition/parameter schema, and the existing
canonical Rack or selected Scope component occurrence. A pure Sequence-only Rack
projection has no author lease and cannot produce a scalar edit. Ambiguous Scope
materializations require explicit selection; a single binding follows the same
implicit occurrence contract as the existing Inspector projection.

The shared numeric adapter recognizes the closed SetAudioScalar scalar intent
and lowers pointer Begin/Update/Finish/Cancel through the existing parameter
interaction protocol. Keyboard nudges and confirmed NumberInput text are discrete
scalar intents. Widget refresh keeps fresh callbacks; inspection never carries
these author callbacks, values, lease or pointer capture onto another control.
The action closure captures one scalar target rather than cloning complete Racks,
route menus, native input observations or automation curves.

The App checks current session/Sequence, selected occurrence, canonical Rack lock
admission, processor definition and exact parameter schema before a scalar intent
can enter the sole Timeline mutation. A late queued start cannot seal another
gesture or author a replacement owner. Regression tests exercise the
real AppUiHost/EventRouter path; execution receipts qualify the result separately. Native
accessibility names, domain values and assistive action delivery remain separate
work; this interaction change does not claim physical AT qualification.


### Coupled scalar readouts during active gestures

Inspector/Mixer numeric pairs reuse the existing Slider and NumberInput as two
logical children in one private typed compound control. The existing Router
`after_child_event` hook synchronizes the adjacent presentation immediately;
layout and form confirmation use the same synchronization. Numeric entry uses
its sole NumberInputModel for hard-range admission and exact formatting.

The shared scalar is current UI intent, not evidence of author success. The
fresh typed callback and canonical author session still validate every command;
a refused/stale intent triggers the existing Host fresh projection. Pointer
capture, local draft, IME, callbacks and local/author histories are never restored
from that UI scalar. Unfocused NumberInput synchronization publishes no callback;
same baseline is a strict no-op, and a focused editing session is not overwritten.
External author changes continue through fresh model/draft admission.

Every drag captures its exact current scalar origin, including values outside
its Slider soft range. Cancel restores that origin for either the canonical
ParameterGesture adapter or non-Parameter callbacks. No second author history is
created. Accessible numeric names/units and native assistive-technology action
adapters remain independent follow-up work; a synchronized Text node is evidence
of the actual NumberInput readout, not a native AT qualification claim.


### Numeric semantic inspection

`AccessibilityValue::Number` stores Float64 and `Integer` retains exact signed
64-bit values. Generic Slider and DockSplitter coordinate ranges promote their
Float32 coordinates without rounding; concrete numeric pairs explicitly expose
their current declared scalar with the same admitted NumberInput hard range.
The presentation pair reads `committed_value()` and `range()` directly from the
sole numeric model. Formatted text is not parsed to invent an author baseline.

The numeric Slider's fresh pure value adapter follows the same live scalar used
by its actual adjacent readout. NumberInput continues to expose its real raw
Text draft and existing disabled, confirmation, cancellation, selection and IME
contracts. That UI scalar is pending presentation intent; canonical author
transactions and exact owner/session admission still determine author success.

Rack scalar parameter names come from the same localized parameter label and
unit painted by the current row. Sidechain level uses its current localized
label and declared dB unit for both paint and numeric semantic names. Labels,
value adapters and callbacks are fresh projection data and are not restored by
focus or numeric draft inspection. Other numeric rows still require explicit
name adoption; this stage does not claim complete application accessibility.

These are platform-neutral AccessibilityNode contracts only. Native assistive
technology publication, numeric range/action delivery and physical screen
reader qualification remain separate unfinished adapters.


### Closed audio scalar control owners

Processor parameters, Channel input Trim, static Fader, ordinary Route level and
processor sidechain level use one `SetAudioScalar` intent and the same continuous
parameter interaction protocol. The closed owner enum carries only each field's
actual authority: a processor parameter owns its exact definition and one
schema; Trim/Fader own a Channel Strip address; routes own exact endpoints; a
processor sidechain also owns its exact inspected occurrence, definition and bus.
No fake parameter schema, arbitrary nested edit or compatibility alias is used.

Concrete App projections bind a current author session/Sequence lease to their
explicit source Sequence. Pure Sequence projections cannot authorize edits.
Absent leases, locked owners and automated scalar values disable the appropriate
controls. Factories capture only the closed target rather than whole Channels,
Racks, source menus or runtime observations. Target identity and control role
form the semantic focus/draft key; current values, captions and list order do not.

The adjacent numeric pair still follows its actual typed scalar and sole
NumberInputModel. Mixer Trim/Fader/Route and Sidechain controls adopt the same
current localized painted dB label for their neutral semantic names. Native AT
publication and range/action delivery remain separate unfinished adapters.

Begin/Update/Finish/Cancel enter the existing canonical author Session. A complete
drag seals one Undo entry; Cancel restores the current gesture origin without
adding author history. Session/Sequence replacement, endpoint/definition/schema
rebind, automation and lock/delete boundaries refuse stale owners before author
mutation. Discrete keyboard nudges and confirmed numeric text remain separate
scalar commands. Host regression receipts, rather than this source description,
qualify the actual Router/capture/author behavior.
