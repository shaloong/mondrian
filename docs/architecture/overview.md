# Architecture Overview

Observed auxiliary source rewiring shares creation's current native input
admission and commits the existing Timeline endpoint edit once. The payload binds
the retained Route ID and expected source to the same processor/input key;
stale routes cannot retarget an unrelated connection. Route identity, enabled
state, static gain and exact gain automation survive the source replacement.
Unchanged source is a no-op; old/new Track locks and causal feedback remain
canonical domain refusals. Shared Rack selectors use cached observations only.

Core encoded sampling contracts explicitly retain planar GBR/GBRA IEEE Float32 in both byte orders. Their proven depth is 32 bits, with Alpha present only for GBRA; unknown or contradictory probe facts remain inadmissible. These are source facts, not a claim of a hardware YUV surface.

Mondrian is a native video editor organized around strict crate boundaries. The self-hosted winit/wgpu UI is the product path; old egui-era modules have been removed or are no longer architectural reference.

## Layers

Audio latency preparation follows the execution module's existing causal channel
stages. App authoring keeps one observation-bound routing transaction; native
CLAP/VST3 acceptance additionally covers a detector whose own declared latency
requires target main-input compensation. Undo, saving with live Redo and fresh
archive reopens compare independent PCM and the explicit public lookahead, without
introducing an App-side latency interpretation.

Canonical visual key capture returns complete schema-ordered channel groups by
stable identity without evaluating `PropertyValue`. Clipboard capture scans each
selected property once and retains exact `f64`/`i64` author values plus each
channel's controls and temporal flags. `PasteCompleteKeyframes` validates the
whole batch, replaces occupied author times, merges canonical channel lists and
normalizes constraints once against final neighbors. Reused identities at other
times, duplicate destinations, incomplete channels and invalid scalars refuse
publication. The App wraps all affected properties in one author transaction;
there is no evaluated-value clipboard alternative or persisted format migration.

Core complete-key value edits address stable IDs and one declared numeric channel.
The numeric contract derives one shared bounded displacement, retaining group
spacing at hard limits instead of clamping each key independently. Integer source
values stay in widened integer arithmetic; floating groups use finite boundary
displacements and scaled step intervals for overflowing spans. Tiny nonzero
boundary displacements remain distinct from zero-step no-ops. Admission rejects
empty, duplicate, stale and channel-incomplete selections before publication,
then normalizes constrained tangents once on the complete candidate. Other
channels, exact key times and identities remain unchanged. This is a domain
mutation contract; UI and authoring-session integration require their own gates.

Core's `ParameterNumericRange` owns finite editor projection and normalized value
mapping for author parameters. Valid ordered endpoints may have an overflowing
difference or be equal. Mapping retains exact endpoints, uses weighted
interpolation across opposite signs, and projects fixed ranges to the midpoint.
Invalid ranges, nonfinite values and non-unit input positions are rejected.
Visual and audio curve adapters share this arithmetic instead of narrowing
admitted schemas or treating fixed-value parameters as missing curves.

Core numeric interpolation shares the stable finite-endpoint arithmetic between
editor intervals, visual numeric channels and exact audio curves. Opposite-sign
endpoints do not require a representable difference, and endpoint values remain
exact in either direction. Exact Bezier value evaluation uses de Casteljau
interpolation over finite control points. Time inversion uses an exact local
author-time origin; implicit linear handles and explicit one-third time handles
avoid unnecessary root searches.

Constrained tangents use bounded harmonic slope ratios rather than overflowing
reciprocals. Unrepresentable intermediate slopes use a local value scale before
materializing finite control offsets. Visual automatic endpoints use stable
extrapolation instead of overflowing weighted products. Zero secants are detected
as zero, independently of physical value units. Author admission checks both
finite offsets and the resulting finite control points, preserving the original
curve when admission fails. Regression coverage includes ascending/descending
full finite intervals, manual/auto/continuous exact Bezier curves, nonuniform
visual endpoints, small-value unit invariance and subnormal exact tangents.

Visual numeric channels evaluate author values in `f64`, with narrowing reserved
for declared Float value types. The unused generic `KeyframeTrack` and
`Interpolatable` APIs are removed; numeric channels are the sole visual curve
authoring path. `TimelineTime::distance_from_f64` subtracts rational
coordinates through integer cross products before floating projection, including
derived distances that cannot fit a persisted rational. Tangent spans use the
same projection without an absolute epsilon floor. Bezier time inversion keeps
a bounded bisection bracket for flat endpoint tangents; exact one-third time handles bypass inversion. Regression
coverage includes ordinary one-third Double evaluation, translated 48 kHz spans
and the legal `x=t^3, y=t` endpoint case.

Exact automation distinguishes automatic tangent derivation from continuous
authored geometry. Auto regenerates its canonical handles from neighboring
secants. Continuous retains a valid authored slope and independent handle time
lengths; entering the mode aligns existing broken manual tangents, missing
partners inherit the authored direction, and shortened editorial segments bound
handles along that direction. Validation checks scaled collinearity and opposing
orientation rather than requiring Continuous to equal Auto-derived handles.
Invalid replacements remain atomic. Current alpha keys explicitly serialize
their tangent mode; no missing-field fallback admits an older curve schema.

Sparse exact handle edits address one temporal side and retain omitted axes.
Only a changed Auto handle becomes Continuous; no-op gestures preserve Auto.
Core aligns the opposite side, preserves its temporal length for nonvertical
directions and its value length for vertical directions, and admits collapsed
zero vectors without inventing a direction. Missing partners of authored
vertical handles are reflected as vertical vectors. Invalid, empty, absent
segment and nonfinite edits fail before candidate publication.
Stored constrained handle sides remain valid even when an adjacent segment is
Linear. Explicit initial handle coordinates do not compute unused default times.
Execution derives missing default time controls directly in normalized floating
coordinates and checks the exact one-third fast path without constructing new
persisted rationals, so valid tiny owner-time spans remain evaluable.

Core owns the shared `ParameterValueWrite` algebra: complete typed values or
sparse numeric components at a stable `AnimationParameterAddress`. Clip, Effect
and Mask adapters supply exact owner-local time to `PropertyBag::prepare_parameter_write`;
there is no owner-specific normalization or curve interpretation.

Stable-address numeric component preparation lives in Core's `PropertyBag`.
It validates the complete sparse update, normalizes through the canonical
property representation and omits unchanged components before returning the
existing `WriteChannels` mutation. It allocates component values without cloning
animation curves. Duplicate indices fail before any property mutation. Existing
keys retain their IDs, handles and temporal constraints; untouched channels keep
their independent curves. Each edited channel normalizes its own constraints;
sparse writes do not normalize or detach untouched sibling curve allocations.
Application adapters commit these mutations through
the authoring session, with the same atomic admission and Undo/Redo as complete
parameter writes.

Complete parameter writes distinguish shared-ID property keys from independent
channel keys at the requested author time. They edit a complete key by ID only
when that identity resolves across all channels; otherwise they use the same
channel mutation to retain every existing channel ID and create missing keys.
This lets a Viewer gesture follow an Inspector axis edit without rejecting or
merging away its independent key identities. Canonical scalar no-op comparison
also retains exact held-i64 intent above f64's consecutive-integer range.

```text
OS / winit
  -> mondrian-platform-core / mondrian-platform
  -> mondrian-app

mondrian-app
  -> mondrian-editor-state
  -> mondrian-project
  -> mondrian-playback
  -> mondrian-render-cache
  -> mondrian-audio
  -> mondrian-broadcast
  -> mondrian-reference-output
  -> mondrian-ui-* crates
  -> mondrian-assets / mondrian-timeline / mondrian-interchange / mondrian-renderer / mondrian-media / mondrian-effects / mondrian-export

foundation:
  mondrian-storage
  mondrian-core
```

## Crate Responsibilities

- `mondrian-storage`: the domain-free, single implementation of
  crash-consistent direct-child directory creation and sibling file
  publication. It retains source-object identity across in-process and
  external-writer flows and returns exhaustive typed publication evidence;
  Asset Library snapshots, Project, Recovery, Export, and regenerable media
  products add their own policy above it. See
  [Storage Publication](storage-publication.md).
- `mondrian-core`: shared value types, strong IDs, project settings, canonical audio signal layouts, color primitives, bounded format-specific Dynamic HDR value objects, automation/keyframe data (including validated structured Qualifier samples), mask/effect data, and timeline render-plan data traits. Dynamic HDR core types identify ST 2094-40 Application #4 or Dolby metadata semantics without carrying licenses, executable paths, entitlements, or certification claims. Effect types and parameter identities are stable persisted author data only; executable grade/Qualifier mathematics remains in `mondrian-effects`. Core owns no executable Render Graph; visual graph definition/compilation lives only in `mondrian-effects`, and frame-plan evaluation lives in `mondrian-renderer`. It must not depend on UI, platform, media, renderer, or app crates.
- `mondrian-editor-state`: the UI-independent `AuthoringSession`, editor
  actions, selection/navigation state, and bounded project-wide Undo/Redo.
  Transactions validate and atomically install detached candidates. History
  retains typed Sequence endpoints or scoped Project restore points, reuses
  unaffected COW allocations, preserves publication evidence and valid
  navigation, and never becomes command replay or a second delta author model.
  Exact retime intents carry typed Clip IDs and time values; the App Adapter
  owns dependency/lock/link-group admission and Timeline owns atomic map
  replacement.
- `mondrian-platform-core`: narrow platform service traits and native-fact result
  types, including stable per-user state-directory discovery, plus the deep
  platform/driver/display Matrix Module that compiles exact commercial rows and
  deterministically correlates owner-verified physical evidence. Its separate
  [Commercial Endurance Qualification](commercial-endurance-qualification.md)
  Module compiles bounded 72-hour workload evidence without owning product
  execution or filesystem capture. Normalized
  receipts remain small Core values; original probe/GPU/Viewer bytes are sealed
  and replayed by platform validation Adapters. It makes no OS
  calls and owns no product filesystem namespace or persistence policy.
- `mondrian-platform`: desktop platform implementations such as clipboard,
  dialogs, file reveal, stable per-user state-directory discovery,
  eyedropper/global pointer capture, display discovery, playback-thread
  scheduling, and explicitly scoped memory observation. It owns OS services and
  facts, not product directory creation/durability or device-bound Renderer
  capability; native video import support belongs to the active Renderer
  Adapter/Device runtime.
- `mondrian-ui-core`: retained widget trait, events, accessibility metadata, focus/shortcut/tooltip traits, tree traversal.
- `mondrian-ui-theme`: semantic theme tokens. Only Dark and Light are concrete themes; System is a resolver mode.
- `mondrian-ui-layout`: reusable layout algorithms.
- `mondrian-ui-renderer`: wgpu rendering backend for UI draw commands, text/images/vector primitives, clipping, batching.
- `mondrian-ui-text`: text layout/rasterization infrastructure.
- `mondrian-ui-events`: event routing, focus traversal, pointer capture, drag/drop, shortcut resolution, IME/cursor/eyedropper side effects.
- `mondrian-ui-tooltip`: tooltip manager and overlay widget.
- `mondrian-ui-widgets`: reusable controls and editor surfaces. Widgets depend on tokens and action dispatch, not app persistence.
- `mondrian-app`: product shell, app state, command/action handling, project lifecycle, window/runtime wiring, panel adapters.
  Its [Execution Resource Coordination](execution-resource-coordination.md)
  Module publishes immutable product admission/budget decisions while every
  execution domain retains its own queue, workers, cancellation, and terminal
  evidence. Preview frame evaluation is a bounded semantic working set, not a
  second decoded-frame cache: decoder-family retirement drops evaluation-held
  native surfaces before Frame Store entries. Stable waits name an admitted
  media producer; residency/capacity/execution-pressure deferrals are retry
  admission and are never memoized as producer waits. A concrete Window GPU
  failure enters one bounded background CPU Viewer Adapter, never an inline UI
  thread composite, and leaves it only after a replacement GPU generation is
  ready.
  Validation-only commercial endurance runs enter one serial campaign
  coordinator owned by the App composition root. The coordinator owns exact
  phase order, monotonic cadence, process-tree memory sampling, bounded evidence
  publication, and the shutdown-before-terminal-sample rule; concrete product
  runtimes remain responsible for real Playback, Reference Output, Export,
  recovery, and synchronous worker-closure receipts.
  Preview's receipt closes its complete media/visual/fallback/title/render-cache
  worker inventory rather than treating an empty decode queue as process
  quiescence.
  The validation Headless owner group additionally consumes Audio lifetime
  evidence and an explicitly bounded GPU device-generation retirement receipt;
  timeout or incomplete handoff remains failed closure while the progress
  worker retains safe-release authority.
  Visual Mask tracking is another instance-owned execution domain: its bounded
  dedicated worker freezes exact author/media input, decodes exact stills,
  delegates only image analysis to Effects, and returns through one
  revision-checked author transaction. It does not share realtime Preview work
  admission or create a second Mask interpretation.
- `mondrian-assets`: SQLite-backed Project asset-library index and durable
  file/generated-source records. It consumes only the foundation-owned media
  probe contract; FFmpeg and generated-pixel execution cannot enter this
  authoring Module. Generated content is interpreted only by the typed
  Timeline/renderer execution path—there is no process-global RGBA8 generator
  registry in the Asset Library.
- `mondrian-timeline`: sequence/track/clip domain model, editing commands, and
  the revision-bound Prepared Visual Schedule used to index immutable visual
  placement semantics for production execution.
- `mondrian-interchange`: bounded native OTIO, CMX 3600, FCP 7 XML, and
  helper-isolated AAF Adapters. It owns one private exact lowering model and
  machine-readable preservation evidence, while canonical `Sequence` remains
  the only public author authority. See
  [Timeline Interchange](timeline-interchange.md).
- `mondrian-media`: FFmpeg probing/decoding plus media source, waveform, proxy,
  cache, Audio Playback, and physical output adapters. It does not interpret
  Timeline audio routing or processor order.
- `mondrian-audio`: validated author-to-IR compilation, Render Contract
  preparation, common float DSP, exclusive render Sessions, recursive nested
  Program Runtime, prepared contribution/route PDC, and media source interfaces.
  Real plugin hosts, parameter-event delivery, layout negotiation, and richer
  processors deepen this crate; format-only placeholder crates are not created.
- `mondrian-broadcast`: versioned broadcaster QC profiles, bounded streaming
  picture-analysis evidence, canonical ST 291 Type 2 packets, and registered
  ATC/AFD/CDP transport constructors. It owns neither Timeline caption
  authoring nor vendor/file carriage; see
  [Broadcast QC And Ancillary Data](broadcast-qc-and-ancillary.md).
- `mondrian-reference-output`: platform-neutral exact professional-output
  signal, v210/RGB12, embedded-s24, and atomic ancillary payload contracts,
  bounded scheduled
  playout, lifecycle evidence, and concrete Windows DeckLink API 12.0/AJA SDK
  18.1.0 native adapters with independent raw ANC capture. Native no-device
  validation does not close physical hardware or other-OS qualification.
  It never owns Program Output color interpretation or Viewer/Export policy;
  see [Reference Output](reference-output.md).
- `mondrian-playback`: headless Playback Session state machine, Synthetic Clock
  Master, epoch/revision invalidation, frame-delivery recovery policy, and
  transport snapshots. It has no UI, codec, GPU, device, asset-library, or
  concrete timeline ownership.
- `mondrian-render-cache`: bounded asynchronous storage for verified,
  content-addressed post-composite working-linear Timeline frames. It owns the
  lossless artifact format, validation, atomic publication and local disk LRU;
  it owns no Preview scheduling, Viewer display transform or implicit Export
  substitution. See [Timeline Render Cache](timeline-render-cache.md).
- `mondrian-effects`: visual-effect registry, typed execution contracts,
  definition/resource-bound Effect preparation, backend-neutral primary-grade
  ACES 1.3 gamut compression, scene-linear highlight chroma reconstruction,
  and HSL/3D Qualifier/matte-refinement mathematics, explicit RGB/AlphaMask
  domain graph compilation/execution, mask rasterization, deterministic bounded
  object/planar tracking analysis, and visual
  plugin-effect contracts; it is not the audio processor host and owns no wgpu
  objects.
- `mondrian-renderer`: revision-bound Prepared Visual Program compilation,
  per-Clip Effect and visual Transition readiness, per-Sequence render-plan
  lowering, and the transient canonical `PreparedVisualFrameClosure` that
  resolves and validates one exact Program Arc per Sequence before binding
  recursive nested time/canvas/color/Transition/temporal execution instances
  for every consumer. The Program freezes one typed canvas/Preview-scale/title
  materialization contract and every Frame Node copies it; child-canvas
  selection, per-frame product callbacks, and materialization contexts cannot
  reopen raw Sequence settings.
  Type-bearing `CpuColorFrame` /
  `GpuColorFrameHandle` and `GpuFrameCompositor` are the only production
  compositing family; untyped RGBA8 `RenderPipeline`/`FrameCompositor`
  alternatives are intentionally absent. Its frame-lowering Interface consumes
  flat visual items; only preparation reads immutable Sequence revisions, and
  the recursive closure owns no pixels or consumer scheduling.
  Dedicated heterogeneous GPU passes execute explicit Qualifier
  `SceneLinearRgb -> AlphaMask` and Matte Preview
  `AlphaMask -> SceneLinearRgb` operations, with private refinement textures
  included in physical admission rather than hidden behind semantic outputs.
  The same route executes product Power Windows as CPU-rasterized
  `MaskSource` values followed by GPU `MaskCombine` and `MatteMix`; the latter
  changes graded RGB while preserving programme alpha. Preview and Export bind
  the same compiled graph and physical admission evidence.
- `mondrian-export`: export presets, queue, canonical selected-range visual
  closure preflight, job-local materialization/FFmpeg encoding, and timeline
  export orchestration. Raw Sequence snapshots stop at closure preparation;
  frame materialization retains only frozen media/color facts and closure
  nodes. Its constrained IMF RDD 45, AS-11 X9, and SMPTE DCP rows are owned by
  one profile-qualified packaging/validation Module; see
  [Professional Delivery](professional-delivery.md). It must not maintain a
  second nested Sequence walker or a second filesystem publication model.
- `mondrian-ai`: experimental Provider contracts and workflow schema. Its
  current orchestrator fails closed because no production Provider or editor
  mutation Adapter is installed. Lifecycle events are observation-only:
  unknown/unbound steps cannot publish completion, and a future Timeline
  mutation must enter through a typed, UI-independent editor Action Adapter
  rather than `EventBus` or private author-state access.

`mondrian-core::EventBus` is a bounded, non-blocking notification seam for
low-frequency facts published after commit. Saturation may drop a notification;
an observer that needs exact current state must reconcile through the owning
typed Interface. Consequently the bus cannot be state authority, request or
completion transport, realtime work queue, or Undo/Redo storage, and a stalled
observer cannot create an unbounded product-memory backlog.

## Dependency Direction

Lower layers cannot depend on higher layers:

- Core data cannot know UI, renderer, media, platform, or app.
- Timeline can use core effect/mask/automation data. Renderer frame lowering
  consumes `RenderPlanSource`; its separate preparation compiler may consume
  one immutable Sequence revision to bind the schedule and Effect programs,
  then repeated execution returns to the flat Interface.
- Effects own effect evaluation, but pure effect data lives in core so timeline can store effects without depending on the evaluator.
- Structured RGB/YRGB and secondary curve authoring likewise lives in Core as
  validated normalized points. Effects alone compiles sampled execution
  resources, Renderer alone owns device residency/WGSL, and App UI only lowers
  `CurveEditor` gestures to stable-address author actions.
- UI widgets dispatch `Action`; app decides what actions mean. `AppState`
  rejects shell-only, unknown-namespace, and unimplemented Actions with a
  structured error. Empty Undo/Redo history likewise returns typed
  `ActionNotExecuted`; only the Window Adapter may suppress a disabled gesture
  by consulting action availability before dispatch. Logging and returning
  success for an unexecuted intent is forbidden because UI automation,
  scripting, and Golden evidence share this boundary. A serialized `Action`
  always denotes a real semantic intent: Widgets represent a gesture that
  formed no command as `Option<Action>::None`, never as a dispatchable no-op.
- Product semantics use the closed typed `ProductAction` algebra wherever a
  behavior-complete slice exists; external/custom namespaces remain an Adapter
  Seam, not an alternate authoring model. Recognized malformed payloads fail
  closed. UI admission reads an App-owned interaction projection rather than
  traversing authoring or execution internals. A custom slice may be replaced
  only atomically with equivalent typed behavior.
- Platform services are injected into event/app layers; widgets never call OS APIs directly. Windows, macOS, Linux, and Headless implement one Platform Execution Contract throughout M1/M2. D3D12, Vulkan, Metal, native media surfaces, window-system objects, audio devices, and display payloads remain concrete Adapter details; shared Project, Timeline, Playback, Audio, Effects, Color, Viewer, and Export Interfaces carry only typed capability, ownership, synchronization, fallback, and terminal evidence. Windows is the current real-device qualification platform, not the semantic owner of the production Implementation.
- Platform contracts keep Locality in independent `desktop`, `display`,
  `memory`, and `user_state_directory` Modules; the crate root only re-exports
  their stable public API and
  defines the capability-empty Headless adapter. Platform implementation keeps
  the same boundary in deep `memory`, `process_memory`, `playback_scheduling`,
  `user_state_directory`, `display/{windows,macos,linux}`, `global_pointer`, and
  `eyedropper` Modules.
  Its `lib.rs` is only the composition root: OS display APIs and desktop-capture
  session state do not live beside `PlatformService` wiring. The narrow
  `UserStateDirectory` Interface returns one absolute, uncreated platform root:
  Windows uses Local App Data, macOS Application Support, and Linux the XDG
  state directory with the standard home fallback. The Project Runtime Module
  alone appends its namespace and owns creation, permissions, symlink defense,
  locking, markers, and durability. Native file-dialog
  outcomes distinguish `Selected` from user `Cancelled`, while an unavailable
  or failed Adapter is an error; file-manager dispatch likewise reports
  unavailable/launch failure instead of silently succeeding. Playback scheduling is
  thread-affine and exactly restored: Windows uses MMCSS Playback, macOS uses
  user-interactive pthread QoS, and Linux attempts a bounded per-thread nice
  improvement. Permission denial is a typed portable fallback. The desktop UI
  event loop is never promoted to Linux `SCHED_FIFO`/`SCHED_RR`; realtime audio
  scheduling requires its own allocation-free callback-domain contract.
- `mondrian-core::ExecutionCancellationToken` is the payload-agnostic monotonic cancellation primitive. Domain schedulers own when to cancel; lower execution and media Adapters only observe it. Reusing or resetting a canceled token is forbidden.
- Native process-memory observation is a separate read-only `ProcessMemoryProbe`
  Seam with non-interchangeable `CurrentProcess` and `ProductProcessTree`
  scopes. Windows uses Process Status and Tool Help, Linux uses `/proc`, and
  macOS uses `proc_pidinfo`/`proc_pid_rusage`. Product-tree Adapters require two
  matching inventories plus stable PID/start identity and checked-sum every
  member; a PID/exit/topology race, query failure, or overflow invalidates the
  complete sample. Each result names its non-interchangeable private-memory
  metric: Windows Private Commit, Linux anonymous resident memory, or macOS
  physical footprint. Current/peak resident values remain diagnostics, and an
  unavailable peak is absent rather than synthesized. Installed/system memory
  likewise use `GetPhysicallyInstalledSystemMemory`/`GlobalMemoryStatusEx`,
  `/proc/meminfo`, or `hw.memsize`/Mach host statistics behind one Interface.
  Policy above the platform crate accepts only complete `ProductProcessTree`
  evidence and an exact metric required by the active qualification profile; it
  never substitutes whole-system pressure or current-process data.
- Professional playback acceptance is likewise policy above the execution Modules. The real-cadence CPAL A/V Adapter drives the ordinary App transport, Audio Playback, bounded media-source cache, Playback Evidence, headless Viewer GPU execution, and process-memory probe; it does not own a second transport or test-only mixer. A CPAL callback report is intentionally distinct from an acoustic loopback measurement.
- Golden Project acceptance is an App-level Headless Adapter over production
  authoring, playback, persistence, recovery, Preview, Export, and reimport
  Interfaces. Each versioned slice declares its fixture roles, operations,
  content postconditions, and typed evidence; a slice proves only those
  obligations and can never be promoted to complete-project success.
- The complete coordinator owns one Project and primary Sequence across the
  declared slice set, preserves stage-owned typed anchors, waits for background
  work to quiesce, and verifies the complete author and Asset Library state
  after final durable reopen. Only that coordinator may emit the top-level
  completion fact, and the external supervisor validates the required distinct
  runs without unioning partial reports or inferring success from process exit.
  Heavy GPU/media execution uses a dedicated process lifetime.

The audio dependency direction is one-way:

```text
mondrian-timeline ──depends on──> mondrian-core
mondrian-playback ──depends on──> mondrian-core
mondrian-assets ─────depends on──> mondrian-core + mondrian-storage
mondrian-media ──────depends on──> mondrian-core + mondrian-storage
mondrian-audio ─────depends on──> mondrian-core + mondrian-timeline
mondrian-media ─────implements──> decode/cache and physical output adapters
mondrian-export/app ─depends on─> mondrian-audio + mondrian-media
mondrian-playback ──owns────────> Transport/Clock/epoch/recovery policy
```

In particular, `mondrian-assets -> mondrian-media` is forbidden and covered by
a manifest-level dependency-direction gate. The App composition root is the
Adapter that converts bounded FFmpeg probe execution into one immutable Asset
candidate.

## Cross-Cutting Principles

- Use strong IDs (`ClipId`, `AssetId`, `EffectId`, etc.), never bare UUIDs across domain boundaries.
- Persisted author time is exact rational Timeline Time in an explicit owner
  domain. Video frames, audio samples, UI snap grids, and SMPTE display timecode
  are derived coordinates; floating-point seconds and field-derived ordering
  are forbidden for persisted timeline semantics.
- UI visual values must come from theme tokens, not hardcoded colors/spacing/radii.
- Command/menu/shortcut/plugin entry points should flow through a command registry, not private per-menu business logic.
- Preview and export should share render semantics. Different scheduling or caching is allowed; different interpretation is not.
- Interlaced Program Output is two exact progressive field-time evaluations,
  never one woven Effect/compositor input. Media owns deinterlacing; Renderer
  owns exact field sampling; Export owns prefilter/weave and finished-signal
  evidence. Codec flags do not constitute support by themselves.
- Realtime transport has exactly one Clock Master and is owned by the
  [Playback Engine](playback-engine.md); Viewer, decode, render, and audio
  adapters report observations rather than mutating transport.

The validation-only Media runtime may depend on the platform-neutral
`mondrian-platform-core` capsule-closure value contract. That foundation owns
strict serde/replay predicates and no process, filesystem, FFmpeg or App owner.
Media owns native capsule execution; App consumes and publishes its raw closure
with the separate Surface/EventLoop receipt under endurance run/report schema 4.

`mondrian-validation-launcher` is an FFmpeg-free validation entrypoint and shared
Windows namespace policy. Its dependency direction stays below Media/App: plain
bootstrap contracts and OS object ownership only; it has no authoring or media
interpretation. Media and App enable it only for validation. See
`native-validation-launcher.md` for the consuming outer process/Job owner.

## Declared parameter choice presentation

Core's `ParameterEnumOption` contains a canonical `key`, required `display_name`
and required `message_id`. Schema validation rejects blank values and duplicate
keys; duplicate captions remain valid because captions do not identify choices.
All three strings are persisted and included in authoring footprint accounting.
The constructor requires all three declarations, with no migration alias or
caption synthesized from an unknown vendor key. Hold automation stores keys only.

Enum choices can declare optional signed `display_rank` values. Ranks are either
absent for every option (natural schema order) or present and unique for all.
Validation uses ordered sets for keys and ranks, avoiding quadratic scans for
large vendor choice sets. The canonical option vector and animation ordinals
are never reordered. Rank is inline author metadata; strings retain their
existing explicit footprint accounting.

## Definition-owned text controls

Core `ParameterSchema::text_mode` is present exactly for Text values and is
validated with the value type. Definitions declare SingleLine or MultiLine;
constructors choose SingleLine and Basic Title declares MultiLine explicitly.
Serialized Text schemas without a mode are rejected rather than repaired with a
compatibility fallback.
Authoring footprint accounting treats this fixed-size enum as inline metadata.

## Resource parameter admission

Resource schemas carry a definition-owned ParameterResourceContract: unique
allowed binding families, external file/directory browsing shape and optional
file suffixes. Unbound is always admitted for clear/recovery. Core validates
schemas, defaults, persisted values and author writes against the contract
without consulting host filesystem availability. Offline recoverable paths
remain author intent; executable providers retain physical resolution ownership.
### Resource admission and modal keyboard ownership

Resource contracts reject missing or mismatched declarations, inadmissible
defaults and persisted values, including NUL-containing external paths, before
author mutation. Offline path existence is evaluated at execution rather than
used to erase recoverable author intent. UI Core roots declare modal keyboard
scope; UI Events owns scope focus, traversal, restoration and shortcut isolation.

### Continuous authoring interactions

Core supplies a strongly typed AuthoringGestureId. Editor State opens a
Sequence/generation lease and validates every live edit through the normal
replacement certificate. Each update commits canonical author state and advances
author generations. History stages one interaction's original and latest
Sequence endpoints independently of the completed Undo/Redo stacks. Original
Redo branches and retention counters remain intact until acceptance.

Completed commands and the pending interaction have explicit, independent byte
budgets. Their footprint indexes share descriptor caching; shared roots may be
conservatively charged in both ledgers. Pending budget admission precedes author
installation. An update that cannot retain its cancellation origin fails
atomically, including when ordinary Undo retention is disabled.

Finish accepts a changed interaction through ordinary bounded history admission;
a net-zero finish releases pending state without discarding Redo. Cancel validates
and restores the exact original Sequence through the authoring session, advancing
generations when author state changes and preserving completed history. Ordinary
commands accept pending work in their own prepared history plan. Undo accepts and
reverses a changed interaction, or skips a net-zero interaction atomically. Stale
or foreign interaction identities cannot overwrite intervening author changes.


Numeric Clip/Effect curve drafts use the same ephemeral Sequence/generation
lease as live numeric parameter gestures. Their begin is deferred: canonical
state and history remain unchanged while the pointer draft is local. Acceptance
enters the existing curve authoring module once; cancellation releases the lease
without a write. The typed target distinguishes numeric curves from parameter
value writes. Intervening author commits or UI owner changes invalidate captured
curve controls before Host considers deferring a full model refresh.


App's active-Sequence edit adapter resolves the active typed SequenceId and
forwards to the sole named-Sequence commit adapter. Both paths therefore honor
the same explicit gesture command and authoring lease; an active Clip property
mutation cannot silently bypass staging or invalidate its own gesture generation.
Ordinary commands still commit independently when no gesture command is present.

### Explicit resource path binding

Core resource references provide lexical `resolve_external_path` with an explicit
absolute owner directory. No current working directory, canonicalization or file
creation participates. Native filename units and nonexistent targets
are preserved using platform PathBuf join semantics. Ambiguous Windows drive-relative input fails
without changing the reference. App Clip/Effect/Mask typed parameter writes bind
relative input against the open Project directory before ordinary schema admission
and author commit; absolute and non-resource edits use borrowed writes.

Whole Grade graph replacement uses the same resource-write resolver before the
single Sequence transaction. It validates incoming author state, binds static
Effect resource values, and revalidates provider limits before comparing the
graph for a no-op. Definition defaults and graph/Effect/parameter identities are
preserved; a rejected graph cannot change canonical state or history. Resource
schemas do not admit animation, so this seam does not invent resource keyframes.

Clip Effect addition, shared Grade node addition and Gallery Shot Match creation
use the same Effect-instance resource admission before the author transaction.
Relative default values are Project-owned instance intent, while provider schema
defaults remain unchanged templates. Two Projects therefore bind the same
definition independently. Absolute provider paths remain unchanged; a provider
requiring bundle-owned data must supply its explicit bundle path. Expanded paths
are checked against the provider's resource contract, including UTF-8 byte limits;
failure creates neither an Effect/history entry nor an external filesystem object.
Binding happens at author admission, never in the per-frame execution path.

Windows binding uses the standard PathBuf join rules for drive/share roots and
verbatim paths, including their native dot-component normalization. It preserves
unpaired UTF-16 filename units without UTF-8 conversion. Absolute references
retain their exact representation. Actual long-path file reads cover backslash
and forward-slash relative input with parent components. See
[standard path join semantics](https://doc.rust-lang.org/std/path/struct.PathBuf.html#method.push).

### Native resource path authority

Core `NativePath` retains local `OsString` storage and serializes explicit Windows
UTF-16 units or Unix bytes. A foreign path preserves its original platform and
can be displayed or rebound by metadata identity, but cannot cross a local
filesystem execution seam without explicit relinking. Display replacement
characters never become persistence, lookup, or SDK input.

Resource reference identity compares exact native spelling, including dotted
components and separator spellings. Local `OsString` storage borrows a `Path`
or transfers its existing buffer into `PathBuf` at the execution seam without
copying filename units. Package inventory copies one canonical file while
retaining every exact source coordinate for foreign-platform rebinding.

### Numeric parameter channel contracts

Core schemas hold an optional complete, ordered COW list of numeric contracts.
Each declared channel has its own hard/soft range, step and reject/clamp policy.
Schema admission rejects missing or extra contracts; sparse writes check complete
cardinality before normalization and prepare all values before publishing mutations.
The list participates in authoring footprint accounting and shares snapshot roots.
Uniform contract builders expand across the declared arity; serialized scalar
contracts are not accepted through an alpha compatibility adapter.

### Float parameter representation

Numeric author channels retain f64 values. Materializing a Float/vector/Color
corrects rounding toward the interior only when an in-range channel would round
outside its exact hard bounds. This uses a fixed four-channel stack array and
does not quantize Double values or rewrite source keys. Numeric edit admission
rejects finite values that become nonfinite in the declared Float representation.
Persisted static bounds are checked directly, independently of edit Clamp policy.
Sparse numeric edits use the declared value shape even without animation storage.


### Precise compound author values

Core retains DoubleVec2/DoubleVec3, DoubleRgb/DoubleColor and IntVec2/IntVec3
as complete inline typed values with two, three or four channels. Double
components remain f64 throughout author edits, key storage and materialization;
integer-vector components are exact i32 values. RGB and RGBA keep distinct shapes
and parameter color-domain interpretation belongs to their owning definition.
These types use the existing author transaction, sparse channel mutation,
interpolation, COW snapshot and bounded footprint contracts. No plugin-specific
scalar identity aliases or second automation authority are introduced.

Integer-vector definition bounds must be integral and fit i32; channel writes
reject unrepresentable values before projection. Hold is their default key mode,
while definitions can explicitly admit numeric interpolation with rounding at
value materialization. Nonfinite double components and any invalid sparse member
reject the entire author edit. Inline footprint accounting includes these values
without allocating a per-value component vector in persisted author state.

### Visual Bezier value progress

Visual Bezier evaluation constrains the temporal parameter to its segment,
but retains the authored cubic value progress outside the endpoint interval.
Manual and Continuous curves can therefore express overshoot; the Parameter
Schema owns value admission rather than an endpoint clamp in the time solver.
Regression tests exercise real PropertyBag authoring, both overshoot directions,
default and nondefault temporal controls, and serialized evaluation equivalence.

### Sparse coordinate admission

Sparse handle edits omit unchanged coordinates on the wire. Supplied coordinates
must decode as exact time or finite numeric values; explicit null is invalid.
Core shares the finite coordinate decoder with visual curve action payloads, so
JSON conversion of a nonfinite value cannot turn an invalid request into a
partially accepted edit of its other coordinate.

### Visual author handle admission

Visual and exact audio controls share `ExactBezierHandle`: an exact owner-time
seconds offset and an absolute parameter-value offset. Normalized time fractions
exist only inside the shared numeric execution solver; value control points are
owner value plus offset, including excursions between equal-valued endpoints.
`DefaultBezier` retains flat one-third-span intent without storing a fabricated
one-third-second coordinate. It remains meaningful before neighbors exist.

Both paths share de Casteljau evaluation, time inversion, shape-preserving Auto
endpoint derivation, and Continuous direction/partner-length constraints. Visual
numeric channel constraints adapt to Core's exact curve normalizer rather than
owning a second slope algorithm. Public visual edits admit cloned candidates.
Explicit supplied controls must be finite, point toward their temporal side,
have representable value control points, and fit every existing adjacent span
before constraint projection. Time/topology edits shorten retained controls along
their direction when a new span requires it. Unchanged sibling channel storage
remains shared. Persisted validation rejects forged Auto/Continuous controls
without repairing author state. No old normalized-control alias or migration is
provided in the alpha model. Complete temporal-handle interaction remains a
separate UI task.

Explicit visual Bezier and ContinuousBezier preset selection retains controls on both temporal sides, including currently unconnected endpoint sides. Existing Bezier controls survive the mode change. Later key insertion therefore preserves the selected segment intent instead of producing an unintended single-sided Ease preset. This is canonical author state and round-trips with stable key identity.

Sparse typed-coordinate decoding distinguishes absent fields from supplied values through deserialize_present_coordinate. TimelineTime and PropertyValue retain their own nonnullable admission; explicit null cannot silently become a missing axis. Exact Bezier time edits and App canonical curve Move actions share this seam. Floating handle coordinates still use the finite-coordinate decoder. App Move targets retain rational time and declared scalar value types, with owner interval and property schema admission before author publication.

### Absolute gesture candidate source

Editor Session offers current-state and original-gesture candidate execution.
Original candidates borrow the existing bounded history restore point, gated by
the same gesture identity, Sequence and author-generation lease. No duplicate
origin store is added. Each candidate validates and commits against the current
author state with monotonic revisions; net-zero completion preserves the prior
Undo/Redo branch and budget. App chooses original candidates only for absolute
visual numeric positional gestures, resolves omitted targets against current
coordinates, and prepares mutations inside the chosen candidate.

### OpenFX parameter query authority

Execution bindings carry a typed temporal query mode independently of the SDK
descriptor's animation capability. A Constant binding admits finite-time reads
and canonical constant calculus even when the descriptor permits animation.
A CurrentFrame binding contains only an evaluated sample and rejects queries
requiring an unprovided curve; it never invents constant behavior from that sample.
Graph preparation derives this distinction from the enabled canonical author
property. Disabled automation and definition defaults are constant bindings.
Native admission validates the complete value and query-mode snapshot before
assignment, publishes modes before SDK change callbacks, and regards a mode
change as a parameter change even when the current numeric value is identical.
Omitted controls reset to their declared constant defaults. A finite empty
integration interval returns zero independently of query mode. Full animated curve
queries and key enumeration still require a canonical curve snapshot bridge.

Owned SDK query-authority integration tests also execute through the product
resident protocol, with explicit bindings for all six compound parameter types.
They verify exact pixel output and worker reuse independently of direct FFI tests;
their frame-zero sample coverage does not establish full animated seeking.

Read-only SDK key queries use the same temporal binding authority across scalar
and tuple parameters. Constant bindings have zero keys; indexed lookup returns
BadIndex without changing the time output, and finite searches return Failed
with index -1. CurrentFrame samples reject key queries requiring an absent
snapshot. Nonfinite search times are rejected before writing the output.

### Continuous automation execution queries

Core prepares a read-only `PreparedPropertyQuery` from a validated canonical
`AnimatedProperty` and an explicit positive exact time scale. `AutomationQueryTime`
denotes a finite coordinate in that execution grid; it is not a persisted author
value or a rounding grid. Identity scale uses owner seconds; a sequence frame rate
uses OpenFX frames. Key times, derivatives and integrals use the configured units.
Queries never divide a floating frame back to author seconds, preserving exact
ordering at projected Hold boundaries. Preparation rejects author keys that collapse to the same floating time.
Ordered union key times and complete compound values come from the frozen source.
Exact author evaluation and prepared queries share numeric segment lowering,
Bezier time inversion and de Casteljau value evaluation.

Floating numeric derivatives follow the right-hand segment at a key and endpoint
extension outside the keyed interval. Vertical slopes report an unrepresentable
result; Hold jumps do not create impulses. Signed integration includes all keyed
segments and endpoint extension. Cubic area integrates the degree-five product
`y(u) * x'(u)` with exact-order Gauss quadrature. Within each segment a weighted
mean avoids unnecessary overflow. Signed areas accumulate binary significand
products in fixed storage and round only once at publication; overflowing pieces
can cancel without erasing a small remaining area. Integer and categorical values retain their
canonical value conversion but do not expose floating calculus. The OpenFX
snapshot transport and native query dispatch remain separate integration work.


### Canonical scalar keyboard offsets

ParameterNumericContract::offset_scalar applies explicit finite author-unit
increments with a separate signed step count. Integer source values remain i64
and use widened saturating multiply/add before clamping to the integer lattice
inside the hard bounds. Float bounds use inward representable f32 endpoints;
Double overflow from an oversized directed increment resolves at the finite hard
boundary. Zero increments preserve signed zero. Invalid or empty scalar domains
are rejected. Soft display ranges do not participate in this author calculation.


Canonical channel keys use AnimationChannelValue::Number or ::Integer. Scalar
integer authorship never projects through f64 during normalization, insertion,
edit, lookup or Hold evaluation. Prepared queries read the frozen exact integer
key at the projected Hold interval; numeric calculus retains its existing type
admission. Continuous numeric execution and UI use explicit projection().
The alpha persisted key shape changes directly; no legacy numeric parser or
parallel mirror is retained. Disabled animation still permits exact author-key
lookup, sharing the same channel evaluator behind an explicit enable-state gate.

### Canonical channel key admission

`AnimatedProperty::set_channel_keyframe` and `PropertyMutation::SetChannelKeyframe`
insert or move a key using its canonical `AnimationChannelValue`, exact owner time,
stable identity and interpolation metadata. Integer channels retain all `i64`
bits; numeric channels retain their `f64` author precision independently of the
native evaluated `PropertyValue`. Admission validates the channel index, scalar
kind, schema bounds and controls against a cloned candidate. A destination owned
by another key is rejected atomically. The old slot for the same identity is
removed before final neighbor normalization and complete validation; no partial
state is published. Other channels retain their values. This uses the existing
canonical normalization and evaluation contracts.

### Atomic key-set deletion

Core owns bulk key deletion for both AnimatedProperty and ExactAutomationCurve.
A nonempty unique stable ID set is validated in full before mutation; stale IDs
reject the candidate. AnimatedProperty requires complete property keys across
all populated channels and uses a first-channel scan plus binary sibling lookup,
rather than repeatedly scanning the curve for each selected ID. Surviving
constraints normalize once per candidate after the whole set is removed. If no
keys survive, the original evaluation at the explicit owner retain_time becomes
the unkeyed value. The existing typed scalar and interpolation contracts admit
that result. PropertyMutation::RemoveKeyframes preserves this operation through
PropertyBag and nested path mapping. No alternate evaluator or project schema
is introduced.


## Exact selected-key translation

The authoring foundation exposes `AnimatedProperty::shift_keyframes` and
`ExactAutomationCurve::shift_keyframes` over nonempty unique stable KeyframeIds
and one signed exact TimelineTime delta. Both admit a complete final candidate:
apply the delta to original coordinates, sort, reject collisions/overflow, then
normalize constrained tangents once and validate. Movement between selected
neighbors therefore never collides with an intermediate old position. Property
channels retain typed values and complete-key identity; incomplete channel sets
and a later-channel failure cannot publish earlier changes. Owner domains impose
bounds at their authoring adapter; the generic time primitive does not invent a
Clip interval for Sequence or shareable Scope author time.


Exact automation group value offsets reuse Core's numeric common-displacement
contract. The complete stable ID set is admitted before candidate cloning;
constraints normalize once and manual offsets remain unchanged. The owning
adapter supplies the numeric contract and retains definition-specific validation
of absolute control values. A no-op does not rewrite constrained handles.


### Complete-key interpolation intent

`AnimatedProperty::update_keyframes_interpolation` and
`PropertyMutation::UpdateKeyframesInterpolation` admit a nonempty unique set of
complete stable keys. All selected channel-side presets and temporal flags are
set before one constrained-tangent normalization and candidate validation.
Selection enumeration never determines constraint geometry. Raw scalar values,
exact owner times and key identities are retained; stale, independent-channel
or schema-incompatible selections publish nothing. Singleton and group commands
share the preset application helper. Editor/audio wiring and interactive group
menu admission require separate integration qualification.

The shared interpolation display resolver retains asymmetric manual EaseIn/
EaseOut intents instead of collapsing them to the Bezier family. Hold and
constrained Auto/Continuous modes retain precedence.


ExactAutomationCurve also admits complete stable selections with one preset and one normalization. Singleton preset edits delegate to this primitive. Empty/duplicate/stale selections and unsupported asymmetric presets publish nothing; scalar author bits and exact times remain intact.


Visual App curve preparation lowers complete selection interpolation into one Core mutation; singleton requests reuse the same candidate preparation. No-op admission compares complete normalized author state, rather than reconstructing mode-specific equality in the application.


Native visual group-history acceptance uses one test-only Time/Value/Interpolation
edit enum and the shared author action/compiled graph path. The interpolation
case begins with nonuniform linear keys, applies one complete Auto/Continuous
selection, preserves scalar/time/identity, and checks Undo, saved Redo and fresh
reopen against native off-key queries/output. Independent fixture derivative and
split-integral oracles qualify calculus; Continuous may legally retain geometry
while changing constraint intent. Explicit ignored candidate tests passed for Interpolation, Time and Value with
stable source/product/fixture hashes. Named clean-source receipts remain separate
from an uncommitted candidate run.


Native audio complete-selection interpolation qualification uses the actual
ProductAction author-session path and a real probed stereo WAV. Owned CLAP/VST3
SDK fixtures provide independent sample-position signal semantics. The matrix
checks exact key identity/time/value preservation, current owner locks, missing
key refusal, same-mode no-op history, single Undo, saved live Redo and fresh Unicode
project reopening with an empty author history and a consistent SQLite asset
snapshot. Rendering consumes the same AudioProgramRuntime under realtime and
offline contracts with unequal block partitions; it does not add an App curve
interpreter. Actual test results and binary identities are recorded separately
in docs/dev/modal-resource-lut-acceptance.md.


App clipboard authority is one private typed payload (timeline clips or animation
keys). Its public kind inspection derives from that payload; two retained caches
and a separately mutable kind no longer exist. Copy replaces the current value
only after a successful nonempty capture. Cut publishes its copied value after the
author removal succeeds. Paste availability and dispatch both use the sole payload
kind; missing/locked/incompatible key targets cannot fall back to an old clip copy.
Clipboard content remains transient and is absent from project persistence/history;
project mutations still use the existing atomic author-session paths. Regression
qualification includes real Copy/Paste actions, refused-target author/history
invariance and constrained key-group Undo/saved Redo/fresh Unicode reopen.


Key-group Copy chooses its type from the explicit selected-key intent. It resolves
all stable property/key addresses against one current Clip snapshot before
replacing the transient clipboard. A missing property or key refuses the entire
capture through a typed product clipboard refusal and preserves the old clipboard, selection,
Sequence, generation and history; empty/nonexistent key capture cannot silently
become a Clip copy. Valid Clip Copy remains available when no keys are selected.
The clipboard refusal retains its semantic error variant through the core error
transport; AppMessage selects a localized message ID by type. Diagnostic Display
text is not parsed or shown as the product explanation.


Focused visual curves submit Copy/Paste through the closed ClipProductAction
algebra with exact Clip/parameter addresses and a complete stable selection.
Keyboard and opening-time menus use the same request. Copy consumes one freshly
resolved owner/property snapshot and the existing sole clipboard publication path;
it does not mutate global selection to manufacture an action target. Focused Paste
accepts a single-property key payload, derives current canonical owner time,
checks the complete extent and uses the existing atomic author transaction.
It creates fresh author IDs and publishes selection only after commit. Stale
owners, malformed/empty/duplicate selections, incompatible payloads and late locks
cannot silently become Clip clipboard operations. Product refusal messages retain
locale-neutral IDs and actionable bilingual descriptions. Audio curve clipboard,
read-only control selection and root Edit-menu context require their own complete
UI acceptance and remain in the professional workflow matrix.

Inspector visual curve models project the current App animation selection by
exact parameter instance. After Paste publishes fresh stable IDs, reconstruction
applies that changed selection while restoring the same owner focus and viewport.
Unchanged selection projection retains the user's local curve selection. The
model does not guess key identities from time/value coordinates, mutate author
state or introduce a second author interpretation.

Visual group value/time/interpolation actions publish their complete admitted
stable selection after the author transaction succeeds. The single representative
key in an edit outcome is not the selection authority for a batch. The same
post-commit rule applies to intrinsic Clip and Effect curves; rejection changes
neither selection nor history. Selection projections compare currently present
keys, so real Undo/Redo can remove and restore the complete selected group.

### Exact numeric curve group capture and paste

Core ExactAutomationCurve captures a complete nonempty stable selection in author
order, retaining raw f64 values, exact times, manual controls and tangent intent.
Empty, duplicate or missing identities refuse the complete capture. Group paste
replaces keys at the supplied exact times on a detached candidate, normalizes
constraints once against final neighbors and validates before publication.
Invalid groups cannot partially replace destination keys. This is the numeric
foundation for focused audio clipboard integration; App wiring remains separate.

### Atomic complete-key time and value transforms

Core exposes one combined transform for ExactAutomationCurve and AnimatedProperty.
It validates the complete stable selection, derives a common definition-bounded
value displacement from canonical scalars and translates exact author times on
a detached candidate. Final ordering, collisions and constrained tangents are
admitted once before publication. A failure on either axis cannot publish the
other axis. Time-only and value-only entry points share this same implementation.

AnimatedProperty moves complete keys in every channel and offsets only the
explicit numeric channel. Integer values retain integer arithmetic, floating
no-op displacement retains raw signed-zero bits, and manual controls remain
relative to the keys. PropertyMutation::TransformKeyframes uses the same contract
through property/bag dispatch and nested owner path mapping. This is the atomic
foundation for group pointer gestures; App/UI authoring and input acceptance
remain separate work and are not implied by the Core qualification.

### Declared-default parameter reset

Core ParameterValueWrite::ResetToDefault resolves the existing stable parameter
address against its current PropertyBag. PropertyMutation::ResetToDefault admits
the current schema default before replacing static author value, disabling
animation and removing every channel key. Property/schema/channel identities
remain intact; disabled retained keys also make reset a real edit. Static and
nonnumeric properties share this operation. is_at_default projects complete
author state for UI availability, rather than comparing only the evaluated value.

Reset replaces shared channel key storage with empty author lists; it avoids
copying the Undo snapshot's keys merely to clear them and retains no emptied key
capacity in the new author state. The original shared snapshot remains reversible.

The UI core accessibility collector accepts control-owned semantic visitation
for virtual items and records the nearest semantic parent. These metadata IDs do
not become WidgetTree input/focus/capture targets. Controls remain the sole input
owner; projections must derive from their existing model and cannot dispatch
editor actions. Native accessibility adapters consume this contract separately.

Native audio auxiliary qualification enters through the existing ProductAction
routing request and authoring session. The App's independent CLAP sidechain DLL
fixture observes real decoded WAV PCM through AudioProgramDeliveryRuntime, with
shared latency compensation and realtime/offline contracts. Sidechain history,
lock/missing-owner refusal, live Redo after save and fresh project archives use
the canonical Sequence and SQLite library; the fixture creates no alternate
routing, persistence or Undo implementation. Native receipts identify the exact
worker and fixture binaries separately from source-only regressions.


### Transient session identity in control actions

`mondrian-editor-state::AuthoringSessionId` can serialize as a transparent typed
ID in transient product Actions. It still identifies one in-memory editing
session, never persisted author intent. Background input observations and their
UI actions compare this exact identity, so reopening the same archive with the
same Sequence/processor identities cannot accept a previous session's reply.
ProjectDocument and archive schemas are unchanged; this adds no migration or
compatibility owner.

### Transient native input observation identity

Core defines typed `AudioProcessorInputObservationId` using the common identity
macro. App mints it when a control result enters the bounded observation cache;
it is not a persisted author or archive identity. A connection action carries a
closed session/Sequence/rack/processor/observation binding. Repeated cache reads
retain its ID; refresh/replacement obtains a new ID even when the author is equal.


The OpenFX adapter supplies one actual native host to optional OfxSetHost before
enumeration, with module/host owners lasting through their legal callbacks. Its
CPU Filter admission names Float32 depth and refuses a plugin that disables CPU
rendering at descriptor, creation or change time. Progressive fields and
PAR-scaled canonical project geometry are explicit; both Rust and native input
validation reject nonrepresentable extents. These additions do not advertise
all OpenFX contexts, GPU/interact support or a universal performance result.
