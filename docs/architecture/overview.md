# Architecture Overview

Mondrian is a native video editor organized around strict crate boundaries. The self-hosted winit/wgpu UI is the product path; old egui-era modules have been removed or are no longer architectural reference.

## Layers

```text
OS / winit
  -> mondrian-platform-core / mondrian-platform
  -> mondrian-app

mondrian-app
  -> mondrian-editor-state
  -> mondrian-project
  -> mondrian-playback
  -> mondrian-audio
  -> mondrian-editor-ui
  -> mondrian-ui-* crates
  -> mondrian-assets / mondrian-timeline / mondrian-renderer / mondrian-media / mondrian-effects / mondrian-export

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
- `mondrian-core`: shared value types, strong IDs, project settings, canonical audio signal layouts, color primitives, automation/keyframe data, mask/effect data, and timeline render-plan data traits. It owns no executable Render Graph; visual graph definition/compilation lives only in `mondrian-effects`, and frame-plan evaluation lives in `mondrian-renderer`. It must not depend on UI, platform, media, renderer, or app crates.
- `mondrian-editor-state`: the UI-independent `AuthoringSession`, editor
  actions, selection/navigation state, and bounded project-wide Undo/Redo.
  Transactions validate and atomically install detached candidates. History
  retains typed Sequence endpoints or scoped Project restore points, reuses
  unaffected COW allocations, preserves publication evidence and valid
  navigation, and never becomes command replay or a second delta author model.
  Exact retime intents carry typed Clip IDs and time values; the App Adapter
  owns dependency/lock/link-group admission and Timeline owns atomic map
  replacement.
- `mondrian-editor-ui`: product-level panel/workspace descriptors. It should define editor UI concepts, not render widgets.
- `mondrian-platform-core`: platform service traits and native-fact result types. No OS calls.
- `mondrian-platform`: desktop platform implementations such as clipboard, dialogs, file reveal, eyedropper/global pointer capture, display discovery, playback-thread scheduling, and explicitly scoped memory observation. It owns OS services and facts, not device-bound Renderer capability; native video import support belongs to the active Renderer Adapter/Device runtime.
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
  evidence.
- `mondrian-assets`: SQLite-backed Project asset-library index and durable
  file/generated-source records. It consumes only the foundation-owned media
  probe contract; FFmpeg and generated-pixel execution cannot enter this
  authoring Module. Generated content is interpreted only by the typed
  Timeline/renderer execution path—there is no process-global RGBA8 generator
  registry in the Asset Library.
- `mondrian-timeline`: sequence/track/clip domain model, editing commands, and
  the revision-bound Prepared Visual Schedule used to index immutable visual
  placement semantics for production execution.
- `mondrian-media`: FFmpeg probing/decoding plus media source, waveform, proxy,
  cache, Audio Playback, and physical output adapters. It does not interpret
  Timeline audio routing or processor order.
- `mondrian-audio`: validated author-to-IR compilation, Render Contract
  preparation, common float DSP, exclusive render Sessions, recursive nested
  Program Runtime, prepared contribution/route PDC, and media source interfaces.
  Real plugin hosts, parameter-event delivery, layout negotiation, and richer
  processors deepen this crate; format-only placeholder crates are not created.
- `mondrian-playback`: headless Playback Session state machine, Synthetic Clock
  Master, epoch/revision invalidation, frame-delivery recovery policy, and
  transport snapshots. It has no UI, codec, GPU, device, asset-library, or
  concrete timeline ownership.
- `mondrian-effects`: visual-effect registry, typed execution contracts,
  definition/resource-bound Effect preparation, RGBA graph
  compilation/execution, mask rasterization, and visual plugin-effect
  contracts; it is not the audio processor host.
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
- `mondrian-export`: export presets, queue, canonical selected-range visual
  closure preflight, job-local materialization/FFmpeg encoding, and timeline
  export orchestration. Raw Sequence snapshots stop at closure preparation;
  frame materialization retains only frozen media/color facts and closure
  nodes. It must not maintain a second nested Sequence walker.
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
- Platform implementation keeps Locality in deep `memory`, `process_memory`,
  `playback_scheduling`, and display Modules. Playback scheduling is
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
- Realtime transport has exactly one Clock Master and is owned by the
  [Playback Engine](playback-engine.md); Viewer, decode, render, and audio
  adapters report observations rather than mutating transport.
