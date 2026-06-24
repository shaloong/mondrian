# Self-Hosted UI Parity Checklist

This checklist is the Phase 5 audit boundary for retiring the legacy egui
editor path. It does not claim that every workflow is visually final. It records
which product-relevant legacy workflows are already served by the self-hosted
stack, which old egui dependencies are still reference-only or transitional, and
which follow-up Phase 5 item should own any remaining cleanup.

## Status Legend

- `Covered`: the workflow has a self-hosted product path and focused coverage.
- `Partial`: the workflow has a self-hosted path but still carries a known
  product gap or transitional dependency.
- `Reference`: legacy egui code is still useful as implementation reference or
  shared helper code, but must not be an unintended product entrypoint.
- `Retired`: the legacy workflow is intentionally not part of the self-hosted
  product scope.

## Product Entry And Shell

| Workflow | Self-hosted status | Evidence | Follow-up |
| --- | --- | --- | --- |
| Default product launch | Covered | `mondrian` is the package default-run and `src/main.rs` calls `self_hosted::window::run_self_hosted_app`. The route contract test locks this manifest/main boundary. | None. |
| Startup window, create/open/recent/recovery | Covered | `SelfHostedUiHost` owns startup/workspace mode, recent-project preferences, recovery candidates, and startup action dispatch. Host tests cover new project, open project, recent project, autosave recovery, and startup-to-workspace transitions. | Keep startup as the only pre-project surface; no legacy egui bootstrap fallback. |
| Native window lifecycle | Covered | `self_hosted::window` owns startup/workspace native window roles, surface resize/DPI lifecycle, shortcut registration, drag/drop, and close routing. Surface lifecycle tests cover zero-size, same-size, resize, and DPI changes. | None for route parity; developer binaries are classified as self-hosted diagnostics. |
| Title bar, menu bar, app shell commands | Covered | `TitleBar`, `MenuBar`, and `SelfHostedShellCommands` route minimize, maximize, drag, quit, workspace, and modal commands through app-shell actions. | None for parity; future visual polish belongs to scoped P1 follow-ups. |
| Pending close/quit guard | Covered | `SelfHostedUiHost` routes close-project and quit through the same pending-close modal with save, discard, and cancel actions. Host tests cover unsaved close-project and quit flows. | None. |
| Status and action errors | Covered | Self-hosted host surfaces failed editor actions in `AppState` status hints and preserves specific app-layer error hints. | None. |

## Project, Sequence, Preferences, And Shortcuts

| Workflow | Self-hosted status | Evidence | Follow-up |
| --- | --- | --- | --- |
| Project create/open/save/save-as | Covered | Shell dialog resolvers produce typed app actions; host lifecycle tests cover project creation, opening, saving, save-as, and recent-project recording. | None. |
| Sequence settings | Covered | `SelfHostedSequenceSettingsDraft` and shell-local modal convert active sequence settings into typed `ui.sequence.update_settings` payloads. Tests cover draft edits across sequence setting groups and ignored open without an active sequence. | None. |
| Workspace presets and custom dock layout | Covered | `SelfHostedWorkspaceLayout`, panel relocation actions, and preferences persistence own editing/color/audio/compositing/export presets plus custom layout restoration. Tests cover built-in preset routing, grouped tab visibility, panel relocation, and invalid custom preference downgrade. | None. |
| Preferences: theme and shortcuts | Covered | `SelfHostedPreferences` persists theme, shortcut overrides, recent projects, workspace preset, and custom layout with schema-version validation. Shortcut tests cover defaults, overrides, disabled bindings, conflict ownership, labels, and immediate router rebuild. | None. |
| Unknown keyboard shortcuts and IME/text editing | Covered | Shortcut routing resolves widget, text/IME, panel-local, then global shortcuts; unresolved keys stay ignored by Mondrian so platform/system handling is not blocked. IME tests cover preedit, commit, cancel, focus loss, stale focus, and inline rename cleanup. | None. |

## Editor Panels

| Workflow | Self-hosted status | Evidence | Follow-up |
| --- | --- | --- | --- |
| Assets/project media browser | Covered | `AssetGridModel` reads project `AssetLibrary` records, folders, thumbnails, selection, inline rename, context menus, file drops, folder navigation, relink, proxy mode, and drag payloads. Tests cover library-backed cards, thumbnails, empty states, rename, move, delete, relink, proxy toggles, multi-selection, and import dialogs. | Keep Assets scoped to project media; whole-machine browsing remains retired. |
| Effects browser | Covered | `PanelListModel::from_app_effect_registry` builds category/effect rows from the shared effect registry and emits `ui.effects.add_to_clip` only for valid selected video clips. Tests cover category separation, locked/no-target suppression, add-to-clip, and selection sync. | None. |
| Viewer | Partial | `ViewerPanelModel` and `ViewerSurface` own metadata, zoom, fit/full controls, preview resolution, safe guides, transport controls, focus keyboard behavior, and preview frame injection through `ViewerPreviewSource`. Coverage exists for geometry, controls, disabled states, preview source attachment, and preview-quality actions. | P5-CODE-001 must replace or quarantine remaining legacy egui viewer helper reuse before deletion. |
| Timeline | Covered | `TimelinePanelModel` maps real `Sequence` tracks/clips into `TimelineView`, emits typed selection, seek, move, trim, track controls, mark in/out, split, delete, ripple, clipboard, range, drop, and nested-sequence actions. Tests cover adapter payloads, keyboard/context actions, cross-media rejection, stale refs, locked gates, and shared AppState action paths. | None for parity; future polish should be new scoped items, not reopening P2/P3. |
| Inspector | Covered | `InspectorPanelModel` reads clip style, transform, timing, opacity curve, effect selection, effect enabled state, and typed property descriptors. Tests cover no selection, locked/read-only state, disabled effects, numeric/vector/text/color controls, tint/opacity/transform/timing actions, and curve mapping. | None. |
| Node Graph | Covered | `NodeGraphPanelModel` renders source/effect/output nodes from the selected clip and shares selected-effect identity with Inspector. Tests cover empty state, pointer/keyboard/Home/End selection, add/remove/reorder sync, and retargeted edges. | None. |
| Export | Covered | `ExportPanelModel` reads export draft, preset/sequence/range/output state, queue snapshots, cancelable jobs, terminal jobs, and clear-completed actions. Tests cover draft updates, output dialog, enqueue guards/errors, queue actions, and job snapshots. | None. |
| Project panel | Retired | Product scope keeps project media inside Assets and does not continue iterating Project/Console-style panels. | P5-CODE-001 should remove or quarantine any leftover product references. |
| Console panel | Retired | Console is not part of the current self-hosted replacement scope. | P5-CODE-001 should keep developer diagnostics out of default product docks. |

## Shared UI Infrastructure

| Workflow | Self-hosted status | Evidence | Follow-up |
| --- | --- | --- | --- |
| Panel ownership and refresh | Covered | `SelfHostedPanelModels` is the app-state snapshot boundary, and panel-local restoration uses stable `PanelKind` owners rather than visible titles. Tests cover panel model coverage, content factory coverage, local state preservation, and grouped-tab behavior. | None. |
| Overlay ordering | Covered | Overlay paint is a two-pass contract; shell-owned modal overlays paint after tree overlays. Tests cover modal overlay winning over open menu overlay and overlay hit ordering. | None. |
| Clipping, scroll, and paint context | Covered | Widget paint uses explicit clip stacks, scroll offsets, and panel slot boundaries. Tests cover paint context clipping, scroll state restoration, and panel-local hit routing. | None. |
| Primitive rendering | Covered | The renderer has analytic coverage for lines, circles, triangles, and icon raster prefiltering. The line shader contract locks diagonal anti-alias behavior and width handling. | Future visual QA can add screenshot/draw-command checks under P5-CI-001. |
| Performance scale smoke | Covered | Ignored `self_hosted_ui_scale_smoke` builds a large asset/timeline/effect state, measures root build, refresh, resize, paint command count, and playback refresh. | P5-CI-001 should decide which subset becomes a regular gate versus opt-in smoke. |

## Legacy egui Boundary

| Legacy area | Current classification | Reason | Follow-up |
| --- | --- | --- | --- |
| `mondrian-app/src/egui_ui` module | Reference | Still contains migration reference implementations and helper code for theme, viewer/canvas, timeline, startup, color picker, effects, and export panels. It is crate-private and is guarded from becoming public API. | P5-CODE-001 should move required non-UI helpers to app/self-hosted/shared crates or delete them. |
| `mondrian-app/src/app::MondrianApp` eframe app | Reference | Still compiles and uses egui/eframe types, but the product binary no longer launches it. The type and constructor are crate-private. | P5-CODE-001 should retire it once remaining helper dependencies are lifted. |
| `mondrian-app/src/shortcuts.rs` | Reference | This is the legacy egui shortcut preference model and still maps to `egui::InputState` / `egui::Key` for the old eframe app. It is crate-private; self-hosted shortcut routing lives in `self_hosted::shortcuts`. | P5-CODE-001 should delete it with the old eframe app after any still-useful preference migration logic is extracted. |
| `mondrian-app/src/app/media_cache.rs` | Covered extraction | Media-cache filesystem policy has been extracted out of `egui_ui::viewer_panel`; app preferences and legacy viewer calls share one neutral cleanup/statistics module with focused tests. | P5-CODE-001 should continue extracting or deleting the remaining viewer preference/helper dependencies. |
| `mondrian-app/src/app/viewer_preferences.rs` | Covered extraction | Viewer preference persistence and defaults now live in a toolkit-neutral app module; the legacy viewer only snapshots/applies the model while it remains reference code. Tests cover defaults, missing persisted fields, scale labels/factors, and serialization roundtrips. | P5-CODE-001 should continue extracting remaining viewer/canvas helpers before deleting the legacy viewer module. |
| egui/eframe/egui-wgpu dependencies | Reference | Kept for legacy migration reference and tests while transitional app-layer references remain. | P5-CODE-001 should remove them when no non-reference code imports egui types. |
| `app` layer references to egui theme/viewer helpers | Partial | Some app modules still import legacy egui theme, viewer cache, node graph, and preference helper types. These are not self-hosted product entrypoints but block clean deletion. | P5-CODE-001 owns extracting shared domain helpers and deleting compatibility paths. |
| Developer gallery/test binaries | Covered | `ui_demo`, `ui_color_test`, `ui_widget_test`, and `ui_pipeline_test` are explicitly self-hosted developer surfaces, not legacy egui routes. | None. |

## Phase 5 Exit Criteria From This Audit

1. P5-ROUTE-001 can be closed when no default product command, package
   default-run, documented run path, or product binary can launch legacy egui
   unintentionally.
2. P5-CODE-001 can be closed when every dependency listed under "Legacy egui
   Boundary" is either deleted, moved to a neutral shared module, or gated as an
   explicit reference-only target.
3. P5-DOCS-001 can be closed when `ui-system.md`, crate/module docs, and run
   instructions describe self-hosted UI as the final product stack and document
   removed compatibility paths.
4. P5-CI-001 can be closed when the self-hosted focused tests, clippy, and the
   chosen screenshot/draw-command/performance smoke checks are represented in
   the required local and CI gates.
