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

- `mondrian-core`: shared value types, strong IDs, project settings, color primitives, automation/keyframe data, mask/effect data, timeline render-plan data traits. It must not depend on UI, platform, media, renderer, or app crates.
- `mondrian-editor-state`: editor actions and state enums shared by UI and app code. It should remain UI-toolkit agnostic.
- `mondrian-editor-ui`: product-level panel/workspace descriptors. It should define editor UI concepts, not render widgets.
- `mondrian-platform-core`: platform service traits. No OS calls.
- `mondrian-platform`: desktop platform implementations such as clipboard, dialogs, file reveal, and eyedropper.
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
- `mondrian-media`: FFmpeg probing/decoding plus media source, waveform,
  proxy, and cache adapters. Its current audio scheduling/mixing code is an
  implementation bridge, not the long-term Audio Program compiler.
- target `mondrian-audio` boundary: typed audio compilation, processor hosting,
  latency/state management, and execution coordination. Establish the module
  boundary first and create the crate when implementation begins; do not create
  empty format-specific plugin crates.
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
- UI widgets dispatch `Action`; app decides what actions mean.
- Platform services are injected into event/app layers; widgets never call OS APIs directly.

## Cross-Cutting Principles

- Use strong IDs (`ClipId`, `AssetId`, `EffectId`, etc.), never bare UUIDs across domain boundaries.
- Time is frame-exact: `TimeCode { frame, time_base }` and `Rational`, not floating-point seconds for persisted timeline semantics.
- UI visual values must come from theme tokens, not hardcoded colors/spacing/radii.
- Command/menu/shortcut/plugin entry points should flow through a command registry, not private per-menu business logic.
- Preview and export should share render semantics. Different scheduling or caching is allowed; different interpretation is not.
- Realtime transport has exactly one Clock Master and is owned by the
  [Playback Engine](playback-engine.md); Viewer, decode, render, and audio
  adapters report observations rather than mutating transport.
