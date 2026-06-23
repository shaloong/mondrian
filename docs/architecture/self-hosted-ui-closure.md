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

- P2-ASSETS-001: Verify project-media import, folder navigation, rename, move,
  delete, relink, proxy mode, thumbnail loading/failure, and drag payloads.
- P2-ASSETS-002: Ensure asset browser state restoration is stable across
  AppState refreshes: selected assets, folder, filter, scroll, hover, and inline
  rename cancellation.
- P2-TIMELINE-001: Verify select, seek, move, trim, split, delete, ripple
  delete, duplicate, copy/paste, enable/disable, mark in/out, and track controls
  through shared app actions and host availability gates.
- P2-TIMELINE-002: Ensure timeline drag proposals validate clip media type,
  target track type, locked tracks, disabled timelines, and stale ids before any
  AppState mutation.
- P2-VIEWER-001: Verify preview frame delivery, zoom, fit/full controls, preview
  quality, transport controls, safe guides, and focus keyboard controls.
- P2-INSPECTOR-001: Verify selected clip, selected effect, no selection,
  locked/read-only, disabled effect, numeric rows, color rows, and curve rows.
- P2-EFFECTS-001: Verify effect list filtering/category display, add-to-clip,
  selection of newly added effects, Inspector sync, and NodeGraph sync.
- P2-NODEGRAPH-001: Verify source/effect/output graph generation, selection,
  keyboard navigation, empty states, and sync after effect add/remove/reorder.
- P2-EXPORT-001: Verify draft settings, output selection, enqueue, cancel,
  clear completed, status snapshots, and disabled/error states.

## Phase 3: Advanced NLE Interaction Polish

- P3-TIMELINE-001: Harden range navigator behavior: zoom handles, page clicks,
  min/max zoom, offset clamping, playhead visibility, and high-density labels.
- P3-TIMELINE-002: Improve clip visual fidelity for labels, badges, disabled
  state, waveform peaks, selection, hover, trim handles, snap guides, and narrow
  clips.
- P3-TIMELINE-003: Complete roll/trim/ripple UX parity, including keyboard
  shortcuts, context menus, host availability, and undo semantics.
- P3-DND-001: Finish cross-panel drag/drop semantics for assets to timeline,
  panel tabs, dock targets, and future effect reordering.
- P3-CONTEXT-001: Ensure all context menus use shared shortcut hints, disabled
  reason semantics where useful, overlay z-order, keyboard navigation, and
  stable close behavior.
- P3-FOCUS-001: Make focus transfer predictable across dock rebuilds, modals,
  dropdowns, inline editors, color picker, timeline, viewer, and node graph.
- P3-SHORTCUTS-001: Complete shortcut customization UX: conflict ownership,
  disabled bindings, labels, capture mode, reset, persistence, and immediate
  router rebuild.

## Phase 4: Reliability And Platform Hardening

- P4-INPUT-002: Verify unmatched shortcuts, OS/input-method shortcuts, AltGr,
  Meta/Super, modifier drift, focus loss, and text-input shielding through
  runtime and router tests.
- P4-IME-001: Harden IME preedit/commit/cancel, caret rectangle requests,
  focus loss cleanup, and interactions with TextInput selection/edit commands.
- P4-CLIP-001: Verify clip/crop/transform correctness for nested scroll views,
  overlays, menus, tooltips, dock panels, and viewer/timeline surfaces.
- P4-OVERLAY-001: Ensure tooltip, dropdown, context menu, color picker, modal,
  drag preview, and eyedropper overlays have deterministic top-layer ordering.
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
