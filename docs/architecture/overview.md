# Architecture Overview

Mondrian is a native video editor organized around strict crate boundaries. The self-hosted winit/wgpu UI is the product path; old egui-era modules have been removed or are no longer architectural reference.

## Layers

```text
OS / winit
  -> mondrian-platform-core / mondrian-platform
  -> mondrian-app

mondrian-app
  -> mondrian-editor-state
  -> mondrian-editor-ui
  -> mondrian-ui-* crates
  -> mondrian-assets / mondrian-timeline / mondrian-renderer / mondrian-media / mondrian-effects / mondrian-export

foundation:
  mondrian-core
```

## Crate Responsibilities

- `mondrian-core`: shared value types, strong IDs, project settings, canonical audio signal layouts, color primitives, automation/keyframe data, mask/effect data, timeline render-plan data traits. It must not depend on UI, platform, media, renderer, or app crates.
- `mondrian-editor-state`: editor actions and state enums shared by UI and app code. It should remain UI-toolkit agnostic.
- `mondrian-editor-ui`: product-level panel/workspace descriptors. It should define editor UI concepts, not render widgets.
- `mondrian-platform-core`: platform service traits and native-fact result types. No OS calls.
- `mondrian-platform`: desktop platform implementations such as clipboard, dialogs, file reveal, eyedropper, display discovery, native video import capability, and current-process memory observation.
- `mondrian-ui-core`: retained widget trait, events, accessibility metadata, focus/shortcut/tooltip traits, tree traversal.
- `mondrian-ui-theme`: semantic theme tokens. Only Dark and Light are concrete themes; System is a resolver mode.
- `mondrian-ui-layout`: reusable layout algorithms.
- `mondrian-ui-renderer`: wgpu rendering backend for UI draw commands, text/images/vector primitives, clipping, batching.
- `mondrian-ui-text`: text layout/rasterization infrastructure.
- `mondrian-ui-events`: event routing, focus traversal, pointer capture, drag/drop, shortcut resolution, IME/cursor/eyedropper side effects.
- `mondrian-ui-tooltip`: tooltip manager and overlay widget.
- `mondrian-ui-widgets`: reusable controls and editor surfaces. Widgets depend on tokens and action dispatch, not app persistence.
- `mondrian-app`: product shell, app state, command/action handling, project lifecycle, window/runtime wiring, panel adapters.
- `mondrian-assets`: SQLite-backed project asset library and virtual asset records.
- `mondrian-timeline`: sequence/track/clip domain model and editing commands.
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
- `mondrian-effects`: visual-effect registry, RGBA graph compilation/execution,
  mask rasterization, and visual plugin-effect contracts; it is not the audio
  processor host.
- `mondrian-renderer`: timeline render-plan building and compositing/render helpers.
- `mondrian-export`: export presets, queue, FFmpeg encoding, and timeline export orchestration.
- `mondrian-ai`: AI orchestration; it must not become an implicit editor-state owner.

## Dependency Direction

Lower layers cannot depend on higher layers:

- Core data cannot know UI, renderer, media, platform, or app.
- Timeline can use core effect/mask/automation data, but renderer consumes it through `RenderPlanSource`, not `Sequence` internals.
- Effects own effect evaluation, but pure effect data lives in core so timeline can store effects without depending on the evaluator.
- UI widgets dispatch `Action`; app decides what actions mean. `AppState`
  rejects shell-only, unknown-namespace, and unimplemented Actions with a
  structured error. Logging and returning success for an unexecuted intent is
  forbidden because UI automation, scripting, and Golden evidence share this
  boundary.
- Platform services are injected into event/app layers; widgets never call OS APIs directly.
- `mondrian-core::ExecutionCancellationToken` is the payload-agnostic monotonic cancellation primitive. Domain schedulers own when to cancel; lower execution and media Adapters only observe it. Reusing or resetting a canceled token is forbidden.
- Native process-memory observation is a separate read-only `ProcessMemoryProbe` seam. Windows reports Private Commit plus current/peak Working Set through the Process Status API; acceptance policy lives above the platform crate. Unsupported operating systems return explicit unavailable evidence rather than fabricated zeros, so future Linux/macOS Adapters can preserve the same contract.
- Professional playback acceptance is likewise policy above the execution Modules. The real-cadence CPAL A/V Adapter drives the ordinary App transport, Audio Playback, bounded media-source cache, Playback Evidence, headless Viewer GPU execution, and process-memory probe; it does not own a second transport or test-only mixer. A CPAL callback report is intentionally distinct from an acoustic loopback measurement.
- Golden Project acceptance is an App-level Headless Adapter over production
  Interfaces, not a second editor implementation. A versioned execution slice
  may pass only when its exact fixture roles, operations, and content
  postconditions all have structured evidence; that never implies the complete
  Golden Project passed. The foundation audio slice uses the normal media
  import worker, Timeline drop/trim, typed Inspector Actions, project-wide
  Undo/Redo, durable archive service, and fresh archive load. Its evidence
  records the opaque Authoring Session identity and exact Author
  Generation/Sequence Revision transition for every authored, Undo, and Redo
  transaction; descriptive strings are not acceptance facts.

The audio dependency direction is one-way:

```text
mondrian-timeline ──depends on──> mondrian-core
mondrian-playback ──depends on──> mondrian-core
mondrian-audio ─────depends on──> mondrian-core + mondrian-timeline
mondrian-media ─────implements──> decode/cache and physical output adapters
mondrian-export/app ─depends on─> mondrian-audio + mondrian-media
mondrian-playback ──owns────────> Transport/Clock/epoch/recovery policy
```

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
