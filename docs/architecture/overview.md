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

- `mondrian-core`: shared value types, strong IDs, project settings, canonical audio signal layouts, color primitives, automation/keyframe data, mask/effect data, and timeline render-plan data traits. It owns no executable Render Graph; visual graph definition/compilation lives only in `mondrian-effects`, and frame-plan evaluation lives in `mondrian-renderer`. It must not depend on UI, platform, media, renderer, or app crates.
- `mondrian-editor-state`: editor actions and state enums shared by UI and app
  code. It remains UI-toolkit agnostic; exact forward-rate and freeze-frame
  intents carry typed Clip IDs, `TimeScale`, and `FramePosition`, while the App
  adapter owns dependency/lock/link-group admission and the Timeline domain
  owns atomic map replacement.
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
  structured error. Empty Undo/Redo history likewise returns typed
  `ActionNotExecuted`; only the Window Adapter may suppress a disabled gesture
  by consulting action availability before dispatch. Logging and returning
  success for an unexecuted intent is forbidden because UI automation,
  scripting, and Golden evidence share this boundary.
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
  The editorial-transport slice imports the attested AAC fixture, builds
  overlapping and downstream placements through ordinary Timeline Actions,
  and records exact Overwrite, Split, and Ripple postconditions. Pointer-drag
  and settled seeks must complete through Playback Evidence, while Play must
  advance under the Synthetic Clock Master. This closes short Golden workflow
  obligations only; it cannot substitute for the long-form CPAL/GPU/memory/A/V
  acceptance profiles.
  The proxy/relink slice reuses the Hero Sequence, owns one dedicated video
  Track, and trims an attested project-generated H.264 source to the exact
  nonoverlap window `200..350` through ordinary Timeline Actions. The same
  media worker used by the product starts the instance-owned proxy service
  across a real FFmpeg worker boundary, and the canonical Preview media
  resolver proves Proxy → Original → Proxy selection. Its offline Relink step
  changes only the Asset Library revision, retains typed Track/Clip/Asset
  identities, author name, Project Generation, and Sequence Revision, then
  requires the replacement source path to select a distinct proxy identity
  and complete a fresh generation. A Track/Clip/Asset-scoped authoring anchor
  lets later Recovery add another Hero Track while still proving the relinked
  record and proxy intent did not drift. The fixture proves codec/proxy/relink
  semantics only and is forbidden as independent color reference evidence.
  The generated-delivery slice shares the Hero Sequence, reuses the Foundation
  PCM placement, and uses product Actions to create a Solid Color, trim the
  exact nonzero `150..175` Work Area, and author Transform/Opacity. After a
  durable reopen it enqueues the stable H.264 High and HEVC Main10 presets,
  waits for production `Completed` evidence, consumes exact stream-local
  PTS/duration/time-base from the typed output probe, and reimports both files
  through the ordinary media worker. It independently samples Program Output
  geometry/opacity, decodes the two pictures through the production Preview
  Adapter, renders reference PCM through the audio Program Runtime, and reads
  both AAC streams through the bounded production audio-source Adapter.
  Generated picture/audio and reports stay under `target/`; this slice closes
  only its declared obligations and cannot claim the complete Golden Project.
  The fixture-free visual-authoring slice creates two generated Solid Color
  placements, an explicit Cross Dissolve, and a generated Basic Title through
  the same product Actions. It authors Hold, Linear, and Bezier curves through
  the production property-mutation Interface, verifies one-generation/
  one-revision transaction boundaries and Undo/Redo, then saves, closes, and
  freshly reopens the archive. Its Headless Adapter resolves the ordinary
  recursive Preview plan, invokes the production Basic Title rasterizer,
  executes the float-linear CPU compositor, and compares the evaluated title
  and transition coefficient with the Export render plan. Save/reopen must
  retain the same typed semantics, raster signature, and preview pixel hash.
  This is generated regression evidence, not an external visual-quality
  reference and not proof that the complete keyframe UI is finished.
  The fixture-free recovery/nesting slice reuses the Hero Sequence, creates one
  stage-owned video Track, trims its generated source to the exact nonoverlap
  window `175..200`, and drives the formal Timeline Precompose Action as one
  Project transaction. Hero remains the primary Sequence and the only new
  Sequence is its strongly referenced nested child. Recursive Preview and
  Export execute that graph before recovery, after opening a versioned and
  hashed autosave through the ordinary product Action, and after a covering
  manual save plus fresh reopen. Complete Hero parent and child hashes, stable
  identities, earlier stage anchors, and pixel/execution evidence must match.
  Recovery stays dirty and authoritative until a current manual save publishes
  an empty manifest before deleting covered archives.
  This closes the generated nested/recovery obligation only; it is not evidence
  for real-media nested color, conflict UX, fault injection, or the three-run
  top-level release gate.
  The color-media slice imports attested project-generated HLG Main10 patches
  and an sRGB straight-Alpha PNG through the product worker, places them on two
  adjacent, stage-owned Hero Media tracks in the exact `350..375` window with
  HLG below Alpha (the PNG is an explicit zero-rate still hold), crosses durable
  reopen, and executes the shared Preview float-linear compositor from the
  original rather than a proxy source. It
  checks decoded codes, independent sRGB-to-linear-Rec.2020 values, HLG
  neutral/chromatic invariants, transparent-RGB isolation, then exports one
  Rec.709 H.264 frame through the production queue and reimports it. This is
  strong short-window media/color/Alpha roundtrip evidence, but it does not
  claim an independent absolute HLG transfer-function oracle, PQ/Log coverage,
  real-media nested color, or the complete Golden Project.
  The complete Golden coordinator runs all seven slices through one
  `GoldenProductWorkflowDriver`, one Project, and one Hero primary identity,
  then verifies the exact two Sequences (Hero plus Recovery's strongly
  referenced nested child), quiescent proxy service, relink intent, delivery
  profiles, and stage-owned Track/Clip/Asset anchors after a final durable
  reopen. Only
  this Rust coordinator may emit `complete_golden_project: true`; the external
  PowerShell supervisor validates that typed report for three distinct
  run/Project identities and never unions slice reports. Heavy GPU/media work
  owns a dedicated process main lifetime rather than a libtest worker lifetime.

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
