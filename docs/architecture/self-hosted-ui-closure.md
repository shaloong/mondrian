# Self-Hosted UI Closure Plan

This document tracks the remaining work required before the self-hosted UI can
replace the legacy egui editor path. It is intentionally action-oriented:
finish one item, verify it, update this document, and commit before moving to
the next item. Phase 0 visual baselining is intentionally omitted; visual
regressions can still be added as scoped follow-up items when they block a real
workflow.

## Completion Rules

- Each item must have a single owner boundary: widget crate, shell adapter,
  app-state adapter, or docs.
- Each finished item must include focused tests for the touched behavior.
- Any touched crate must have its architecture contract updated when behavior or
  ownership changes.
- Run `cargo fmt`, focused tests, `cargo clippy --workspace --all-targets
  --all-features -- -D warnings`, and `git diff --check` before committing.
- Do not reopen a completed item unless a regression introduces new evidence.
  Add a new follow-up item instead.

## Done

- P4-INPUT-001: Ignored keyboard input never closes native windows.
  Completed in `17d8188 fix(ui): keep ignored keys from closing windows`.
- P2-WINDOW-001: Native close requests route through the same app-shell quit
  action path used by titlebar controls, menus, and shortcuts.
- P2-PROJECT-001: Self-hosted close-project and quit requests guard unsaved
  project changes with Save and continue / Discard / Cancel.
- P2-PROJECT-002: Host-level self-hosted project lifecycle tests cover create,
  open, save, save-as, close, recent project, recovery, and startup-to-workspace
  transitions through self-hosted actions.

## Phase 1: Visual System Production Pass

- P1-THEME-001: Audit self-hosted widgets for hardcoded production colors,
  spacing, radius, shadows, and typography values. Move reusable values to
  `mondrian-ui-theme` tokens; keep geometry constants only when they describe a
  domain surface such as timeline track height or transport hit target.
- P1-CHROME-001: Normalize panel chrome, tab bars, menu rows, status chips,
  splitters, scrollbars, and toolbar density so the shell reads as one product
  system.
- P1-EMPTY-001: Standardize empty, loading, error, disabled, and offline states
  across Assets, Viewer, Timeline, Inspector, Effects, NodeGraph, and Export.
- P1-ICON-001: Finish icon QA for rasterized SVGs across small sizes,
  disabled/hover/active tones, and high-DPI presentation.
- P1-SCREENSHOT-001: Add a lightweight screenshot or draw-command regression
  harness for stable widget states where pixel output matters.

## Phase 2: Core Workflow Parity

- [done] P2-ASSETS-001: Verify project-media import, folder navigation, rename,
  move, delete, relink, proxy mode, thumbnail loading/failure, and drag payloads.
  Coverage now spans self-hosted host import-dialog dispatch into target asset
  folders, app-action relink success and proxy-mode type guards, existing
  folder/rename/move/delete action tests, thumbnail loading/failure cache tests,
  and `AssetGrid` drag/selection/context-menu widget tests.
- [done] P2-ASSETS-002: Ensure asset browser state restoration is stable across
  AppState refreshes: selected assets, folder, filter, scroll, hover, and inline
  rename cancellation. Coverage includes shell-level stable-id restoration for
  selection/filter/hover, Assets panel scroll preservation, host refresh
  normalization for valid/deleted folders, and forced rebuild cancellation of
  stale inline rename editors without dispatching rename actions.
- [done] P2-TIMELINE-001: Verify select, seek, move, trim, split, delete,
  ripple delete, duplicate, copy/paste, enable/disable, mark in/out, and track
  controls through shared app actions and host availability gates. Coverage
  includes Timeline panel adapters for keyboard/context/range actions,
  AppState dispatch tests for typed timeline and shared edit actions, and
  self-hosted availability gates for valid, stale, locked, and host-dispatched
  typed timeline targets.
- [done] P2-TIMELINE-002: Ensure timeline drag proposals validate clip media
  type, target track type, locked tracks, disabled timelines, and stale ids
  before any AppState mutation. Coverage now spans widget-level disabled,
  locked-source, locked-target, and incompatible-track proposal guards; panel
  payload rejection for stale clip refs and cross-media moves; host availability
  gates for typed stale/locked timeline targets; and command-layer media/track
  validation before undoable AppState mutation.
- [done] P2-VIEWER-001: Verify preview frame delivery, zoom, fit/full controls,
  preview quality, transport controls, safe guides, and focus keyboard controls.
  Coverage now spans `ViewerSurface` aspect-fit and fixed zoom geometry,
  preview-frame clipping, disabled empty states, safe-guide geometry,
  transport icon/action dispatch, narrow-control collapse, zoom and preview
  quality dropdowns, focus-keyboard controls, focus/disabled cleanup, shell-local
  zoom persistence, AppState preview-quality updates, and panel-model preview
  source attachment/empty-state isolation.
- [done] P2-INSPECTOR-001: Verify selected clip, selected effect, no selection,
  locked/read-only, disabled effect, numeric rows, color rows, and curve rows.
  Coverage now spans AppState-backed selected clip/effect panel models, empty
  inspector panels with no form controls, locked-track read-only panel state and
  dispatch suppression, disabled effect rows that still expose editable
  properties for unlocked clips, typed numeric/vector/text/color effect property
  payloads, clip tint/opacity/transform/timing actions, and opacity curve model,
  action, and AppState keyframe mapping.
- [done] P2-EFFECTS-001: Verify effect list filtering/category display,
  add-to-clip, selection of newly added effects, Inspector sync, and NodeGraph
  sync. Coverage now spans Effects model category/effect-row separation,
  searchable effect titles and filter wiring, no-target and locked-track apply
  suppression, selected-video add payloads, AppState add-to-clip undo behavior,
  automatic selection of newly added effects, and refreshed Inspector/NodeGraph
  models targeting the new effect.
- [done] P2-NODEGRAPH-001: Verify source/effect/output graph generation,
  selection, keyboard navigation, empty states, and sync after effect
  add/remove/reorder. Coverage now spans AppState-backed source/effect/output
  graph generation, domain-light node targets, selected-effect graph chrome,
  empty disabled graph state, keyboard/pointer/Home/End selection dispatch,
  automatic selection after effect add, fallback to source after selected-effect
  removal, and node/edge retargeting while preserving selected effect identity
  after effect reorder.
- [done] P2-EXPORT-001: Verify draft settings, output selection, enqueue,
  cancel, clear completed, status snapshots, and disabled/error states.
  Coverage now spans AppState-backed export draft preset/sequence/range/output
  persistence, output-dialog shell resolution into draft updates, disabled
  enqueue payload guards for missing sequences or blank paths, enqueue error
  status for invalid requests, queue cancel/clear typed actions, bounded recent
  job snapshots, cancelable active jobs, failed/completed terminal job states,
  and clear-completed cleanup for Completed/Failed/Cancelled jobs.

## Phase 3: Advanced NLE Interaction Polish

- [done] P3-TIMELINE-001: Harden range navigator behavior: zoom handles,
  page clicks, min/max zoom, offset clamping, playhead visibility, and
  high-density labels. Coverage now spans ctrl-wheel zoom around cursor, zoom
  limit bubbling, horizontal thumb dragging and capture release, horizontal
  page clicks in both directions without seeking, leading/trailing handle zoom,
  min/max zoom clamps, restored state offset/zoom/track-height clamps,
  offscreen playhead paint suppression, and dense minor/major ruler labels
  using the configured frame rate.
- [done] P3-TIMELINE-002: Improve clip visual fidelity for labels, badges,
  disabled state, waveform peaks, selection, hover, trim handles, snap guides,
  and narrow clips. Coverage now spans measured V/A track badges, selected
  clip tokenized borders, hover/selected trim-handle chrome, disabled clip
  muted fill and label color, clipped label paint bounds, truncated-label
  tooltips, narrow clips that skip overflowing labels, waveform peak clamping
  inside clip bounds, waveform paint command density, snap-guide paint tokens,
  and disabled clips remaining selectable for inspection.
- [done] P3-TIMELINE-003: Complete roll/trim/ripple UX parity, including
  keyboard shortcuts, context menus, host availability, and undo semantics.
  Coverage now spans widget-level focused keyboard commands, context-menu
  command rows and shortcut hints, Shift+Delete ripple delete dispatch,
  self-hosted command-to-action mapping, shared AppState availability gates,
  edge-specific trim-to-playhead enablement at clip boundaries, locked-track
  suppression, AppState trim/roll/ripple command handling, and timeline undo
  semantics through the shared editing command path.
- [done] P3-DND-001: Finish cross-panel drag/drop semantics for assets to
  timeline, panel tabs, dock targets, and future effect reordering. Coverage
  now spans router-owned active-drag lifecycle, overlay-first drop routing,
  ignored-drop termination, native file-drop mapping, asset-browser file and
  selection drops, asset-to-timeline typed drop proposals, stable app-side track
  id resolution, incompatible media rejection without mutation, dock tab
  reordering, dock edge/center targets, and the future effect-row drag boundary
  through the existing undoable `Action::ReorderEffects` path.
- [done] P3-CONTEXT-001: Ensure all context menus use shared shortcut hints,
  disabled reason semantics where useful, overlay z-order, keyboard navigation,
  and stable close behavior. Coverage now spans shared `ContextMenu` shortcut
  hint painting, disabled-row consumption without dispatch or close, separator
  geometry, long-menu viewport clamping and scrolling, overlay paint/hit-test,
  Escape and keyboard navigation, timeline host-provided shortcut labels,
  selection-only timeline command disabling, AssetGrid card/selection/grid menu
  precedence, local Rename command handling, and replacing an open AssetGrid
  context menu with the newly right-clicked card target in one gesture.
- [done] P3-FOCUS-001: Make focus transfer predictable across dock rebuilds,
  modals, dropdowns, inline editors, color picker, timeline, viewer, and node
  graph. Coverage now spans stale focus cleanup after widget tree rebuilds,
  IME disable when focused widgets disappear or become unfocusable, window
  focus loss, Tab traversal no-op/empty cases, focused-panel derivation from
  ancestors after click, Tab, and pointer-move focus requests, panel focus
  fallback after workspace switches, inline asset rename commit/cancel focus
  restoration, ColorPicker inner-field focus translation, timeline/viewer/node
  graph focused keyboard handling, and disabled widgets opting out of focus.
- [done] P3-SHORTCUTS-001: Complete shortcut customization UX: conflict
  ownership, disabled bindings, labels, capture mode, reset, persistence, and
  immediate router rebuild. Coverage now spans stable descriptor ids, default
  file/workspace/panel/timeline shortcut coverage, no plain Space binding for
  text-input safety, active conflict-free shortcut tables, user override
  ownership of conflicting defaults, Preferences rows that distinguish explicit
  disables from conflict-owned disables, Disable/Default/Rebind actions,
  modal key capture with Escape cancel, scrolled shortcut rows, persisted host
  overrides, shortcut hint lookup from the active table, and immediate window
  router global-scope rebuild after preference updates.

## Phase 4: Reliability And Platform Hardening

- [done] P4-INPUT-002: Verify unmatched shortcuts, OS/input-method shortcuts,
  AltGr, Meta/Super, modifier drift, focus loss, and text-input shielding
  through runtime and router tests. Coverage now spans unmatched shortcut
  chords returning `Ignored` without dispatch, printable text shielding from
  unmodified/Shift shortcut dispatch while Ctrl shortcuts still dispatch after
  focused widgets decline them, AltGraph key-edge fallback as UI `alt` with
  Alt-only printable text allowed for AltGr-style input, Ctrl/Ctrl+Alt/Meta
  printable suppression, `ModifiersChanged` snapshot conversion,
  Super/Meta/Hyper fallback mapping, focus-loss modifier reset,
  FocusLost/IME cleanup routing, and startup/workspace ignored Escape not
  exiting native windows.
- [done] P4-IME-001: Harden IME preedit/commit/cancel, caret rectangle
  requests, focus loss cleanup, and interactions with TextInput
  selection/edit commands. Coverage now spans explicit runtime conversion of
  winit commit/preedit/cancel events, focused-widget IME commit/cancel routing,
  preedit display without form dispatch, commit-only `on_change`, cancel and
  Escape clearing composition without committing, caret rectangle refresh after
  focus/click/navigation/preedit/cancel, selection replacement on IME commit,
  preedit shielding of edit keys, stale-focused-widget IME disable, inline
  rename cleanup, and window focus loss IME disable.
- [done] P4-CLIP-001: Verify clip/crop/transform correctness for nested
  scroll views, overlays, menus, tooltips, dock panels, and viewer/timeline
  surfaces. Coverage now spans renderer conservative clip snapping, invalid
  scissor rejection, nested clip-stack intersection, transform-stack clip
  application, `PaintContext::push_clip()` intersection with the active context
  clip, ScrollView child-vs-chrome clip scopes, overlay escape from normal
  scroll hit-test clipping, menu/context-menu/tooltip viewport clamps, viewer
  viewport/canvas clipping, timeline clip label/waveform clipping, and
  non-finite scroll offset sanitization.
- [done] P4-OVERLAY-001: Ensure tooltip, dropdown, context menu, color picker,
  modal, drag preview, and eyedropper overlays have deterministic top-layer
  ordering. Coverage now spans tree-wide overlay-after-normal paint order,
  earlier-child overlays painting above later-sibling normal content,
  overlay-first pointer routing, top overlay capture invalidation, captured
  descendants inside ancestor overlays, modal child z-order and modal-over-menu
  hit testing, ScrollView child overlay escape from viewport hit-test clipping,
  menu/context-menu/dropdown/color-picker invalid overlay clip skips, tooltip
  viewport clamping, and shell-owned eyedropper-before-tooltip paint ordering.
- P4-RENDER-001: Finish basic primitive rendering QA: lines at all angles,
  circles, rounded rects, triangles, MSAA resolve, pixel snapping, and invalid
  clip sanitizer behavior.
- P4-PERF-001: Add performance smoke coverage for large asset libraries, long
  timelines, many clips, many effects, resize loops, and sustained playback.
- P4-GPU-001: Verify renderer resource lifecycle across window replacement,
  resize, surface loss, DPI changes, and startup-to-workspace transition.
- P4-ERROR-001: Surface app/action errors in self-hosted shell status without
  requiring Console/Project panel iteration.
- P4-PERSIST-001: Verify preferences and workspace layout persistence across
  versions, missing panels, hidden tabs, invalid ratios, and stale panel ids.

## Phase 5: egui Retirement

- P5-PARITY-001: Produce a final self-hosted parity checklist for every legacy
  egui workflow that remains product-relevant.
- P5-ROUTE-001: Remove or quarantine product entrypoints that can still launch
  legacy egui unintentionally.
- P5-CODE-001: Delete legacy egui code that is no longer needed as reference, or
  move it behind an explicit reference-only boundary.
- P5-DOCS-001: Update architecture docs to describe the final self-hosted UI
  stack, ownership model, test strategy, and removed compatibility paths.
- P5-CI-001: Make self-hosted UI tests, clippy, and any screenshot/draw-command
  regressions part of the required CI gate.
