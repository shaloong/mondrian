# UI Maturation Plan

This document tracks the remaining lower-level custom UI work. It intentionally excludes
`mondrian-app::app_ui` product panels and shell composition unless a task directly affects
shared renderer, text, event, theme, or widget infrastructure.

## Closure Queue

These are the remaining immature areas that still need closed, testable slices before the
custom UI line can be considered production-grade.

1. Text editing depth:
   - [ ] Define a multiline text document model with grapheme-safe line/column navigation,
         selection ranges, editable line wrapping, and command coalescing.
   - [ ] Add multiline TextInput geometry for caret rectangles, selection rectangles, vertical
         scrolling, IME preedit placement, and clipped paint.
   - [ ] Add clipboard cut/copy/paste, undo/redo, Home/End/PageUp/PageDown, word navigation,
         and IME commit/preedit behavior across line boundaries.
   - [ ] Add focused unit and visual tests for multiline selection, mixed CJK/emoji text,
         empty lines, long lines, CRLF paste normalization, scroll-to-caret, and disabled or
         read-only states.

2. Widget Module depth:
   - [ ] Continue splitting large widget Modules where event routing and paint still live in
         the same file as state/model/layout. Priority order: `text_input.rs`, `menu.rs`,
         `scroll.rs`, `color_picker.rs`, `viewer_surface.rs`, `timeline_view.rs`, and
         `asset_grid.rs`.
   - [ ] For each split, keep the external Widget Interface stable and put the test surface on
         model/layout/interaction contracts rather than private paint details.
   - [ ] Delete shallow pass-through helpers that fail the deletion test, especially where they
         only mirror one call site without improving locality.

3. Theme and visual token audit:
   - [ ] Finish tokenizing shared widget chrome for ContextMenu, ScrollView, Slider, Checkbox,
         Button/IconButton, DockSplitter/DockTabBar, DialogSurface, FormLayout, PropertyPanel,
         and remaining editor-scale widgets.
   - [ ] Ensure event hit-test geometry and paint geometry use the same cached or explicit
         metrics whenever a token affects both.
   - [ ] Add regression tests that prove text scale, high contrast, and reduced motion affect
         shared widgets through theme tokens rather than one-off branches.

4. Renderer and primitive reliability hardening:
   - [ ] Keep the offscreen primitive harness active for line/circle/triangle edge cases,
         including subpixel positions, 45-degree thin lines, high-DPI scaling, clip nesting,
         and zero/near-zero dimensions.
   - [ ] Add a policy for CPU rasterization versus GPU analytic rendering for vector
         primitives and icons, with tests that lock the chosen behavior for thin lines and
         rounded/circular shapes.

5. Event/platform release checks:
   - [ ] Run keyboard-only traversal scripts across composite widgets and editor-scale widgets.
   - [ ] Verify unmatched shortcut pass-through, IME switching chords, clipboard failures,
         native file drag fallback diagnostics, cursor priority, overlay z-order, and pointer
         capture release in one smoke matrix.

6. Release-grade verification:
   - [ ] Run cross-backend/cross-GPU smoke tests and record the backend/device diagnostics.
   - [ ] Keep app-level panel migration on top of these contracts; do not add product-panel
         one-off fixes that bypass renderer, event, text, theme, or widget Modules.

## Phase 1 - Renderer Reliability

- [x] Replace the simple row-packed `TextureAtlas` allocator with a fragmentation-resistant
      allocator and expose resource pressure diagnostics.
- [x] Add atlas generation/page semantics so glyph and raster image caches can survive long
      editing sessions without silent exhaustion.
- [x] Add draw command diagnostics for unbalanced clip/transform stacks, unresolved text,
      invalid clip bounds, and invalid translate offsets.
- [x] Add explicit debug diagnostics for backend fallbacks that would otherwise be silently
      tolerated.
- [x] Build an offscreen visual regression harness for renderer primitives.
- [x] Add GPU readback coverage for filled rects, 45-degree hairlines, and circle-shaped
      rounded rectangles.
- [x] Extend offscreen visual regression coverage to triangles, clips, and gradients.
- [x] Extend offscreen visual regression coverage to text glyphs and renderer raster images.
- [x] Extend offscreen visual regression coverage to SVG icon rasterization.
- [x] Capture golden images at representative DPI scales: 1.0, 1.25, 1.5, and 2.0.

## Phase 2 - Text And Input Maturity

- [x] Separate text input state, editing commands, geometry, IME integration, and paint.
- [ ] Add multiline text editing with selection, clipboard, IME, scroll, and undo semantics.
- [x] Decide whether subpixel glyph atlas bins are needed for rich text/code-style editors.
      Current UI text keeps stable whole-glyph atlas keys and applies subpixel positioning in
      image bounds; subpixel atlas bins are deferred until a rich text/code editor proves the
      atlas cost is worth the sharper per-bin rasterization.
- [x] Add text visual regression tests for small sizes, CJK, mixed scripts, emoji fallback,
      caret placement, selection paint, and IME preedit.
- [x] Add TextInput component visual coverage for small clipped fields, mixed CJK/emoji
      committed text, selection chrome, caret chrome, and IME preedit underline.
- [x] Add text renderer coverage proving small mixed Latin/CJK/emoji glyph fallback resolves
      without missing glyphs and produces finite proportional glyph image bounds.
- [x] Extract TextInput IME preedit into a `TextCompositionState` model with direct activity
      and clearing coverage.
- [x] Move TextInput committed edit state and IME composition state into dedicated
      `text_input` submodules so event and paint extraction can proceed without expanding the
      root widget file.
- [x] Move TextInput geometry and horizontal scroll math into a dedicated `text_input::geometry`
      module with direct edge-case coverage.
- [x] Move TextInput keyboard command classification into a dedicated `text_input::commands`
      module with direct modifier-routing coverage.
- [x] Move TextInput paint into a dedicated `text_input::paint` module with direct coverage for
      placeholder, preedit underline, cursor, disabled, and clip-stack behavior.
- [x] Move TextInput IME request and composition-key routing policy into a dedicated
      `text_input::ime` module with direct enable/disable and active-composition coverage.
- [x] Extract TextInput committed text, cursor, and selection into a `TextEditState` model with
      direct unit coverage for selection collapse and grapheme-safe deletion.
- [x] Centralize TextInput committed text edits behind one command path with explicit empty
      paste versus empty IME commit semantics.
- [x] Centralize TextInput keyboard navigation selection semantics so plain, Shift, word,
      and boundary movement share one editing path.
- [x] Extract a shared TextInput geometry snapshot for content clips, text origins, IME caret
      bounds, and paint.
- [x] Add TextInput paint coverage for mixed CJK/emoji text, selection highlight, committed
      text/preedit ordering, and IME underline inside the content clip.

## Phase 3 - Event And Platform Semantics

- [x] Harden focus traversal, keyboard navigation, mouse capture release, window focus loss,
      and overlay hit testing as explicit contracts.
- [x] Constrain framework Tab traversal to plain Tab/Shift+Tab so modified Tab chords remain
      available to shortcut resolution or platform handling.
- [x] Add route diagnostics for dropped shortcuts, stale focused widgets, and capture owners.
- [x] Add accessibility-ready metadata primitives, focus-order collection, and core control
      coverage for Button, Checkbox, Slider, and TextInput.
- [x] Extend accessibility metadata to foundational composite widgets: Dropdown, ContextMenu,
      ScrollView, and DockSplitter.
- [x] Extend accessibility metadata to ColorPicker and ColorPickerTrigger.
- [x] Extend accessibility metadata to editor-scale composite widgets: TimelineView, AssetGrid,
      and ViewerSurface.
- [x] Define keyboard routing policy for unmatched shortcuts, system shortcut pass-through, and
      IME switching chords.
- [x] Define cursor request priority and transient clearing across runtime/window shells.
- [x] Define clipboard failure behavior in `PlatformService` and TextInput clipboard commands.
- [x] Define native file drag-and-drop fallback behavior and diagnostics.
- [x] Extract pointer capture ownership into a dedicated event-router state module with direct
      coverage for capture, owner-only release, clear, stale-owner pruning, overlay preemption,
      drag cancellation, and window focus loss.
- [x] Remove inert `FocusManager::focus_next` / `focus_prev` methods so focus traversal remains
      an explicit event-router contract backed by `WidgetTree` traversal instead of no-op manager
      methods.
- [x] Centralize event-router focus transitions so Tab traversal, click-to-focus, blur, window
      focus loss, stale-focus pruning, panel normalization, and IME disable paths share explicit
      helper contracts with ordering coverage.

## Phase 4 - Widget Productionization

- [ ] Split large widgets into state/model, layout/geometry, events, paint, and tests.
- [ ] Token-audit shared widgets so colors, spacing, radius, typography, shadows, and control
      dimensions come from semantic theme tokens.
- [x] Tokenize PanelList row chrome, badge sizing/radius, accent swatches, focus/drop rings, and
      scrollbar alpha against theme-derived visual tokens.
- [x] Tokenize Dropdown/Menu trigger chrome, popup radius, row padding/radius, separator geometry,
      scrollbar sizing, shortcut text, and icon/checkmark lanes against theme-derived visual tokens.
- [x] Tokenize ContextMenu popup geometry, row measurement, icon/checkmark lane reservation, and
      viewport scroll sizing against cached theme-derived visual tokens.
- [x] Tokenize Checkbox box sizing, label geometry, typography, border width, and proportional
      checkmark geometry against theme-derived visual tokens.
- [x] Tokenize Button padding, height, icon gap/size, typography, and radius against
      theme-derived visual tokens while keeping icon/label clips bounded.
- [x] Add component visual regression scenarios for TextInput, Dropdown, ContextMenu,
      Tooltip, Slider, Checkbox, ColorPicker, ScrollView, DockSplitter, and popup-owning
      controls.
- [x] Tokenize AssetGrid preview wells, badge typography/chrome, footer typography, and
      thumbnail warning marks against theme-derived visual tokens.
- [x] Add composition stress tests for nested clipping, nested scroll views, overlays, focus
      handoff, and disabled/read-only states.
- [x] Add a nested ScrollView overlay stress test proving child overlays remain hit-testable and
      paint outside ancestor viewport clips while clip stacks stay balanced.
- [x] Add a focus-handoff stress test for composite widgets covering TextInput IME ownership,
      disabled controls, Slider keyboard routing, and ScrollView child clipping.
- [x] Extract NumberInput numeric range, parsing, quantization, display formatting, committed
      value, and keyboard step semantics into a directly tested model module.
- [x] Extract Slider numeric range, step quantization, keyboard increments, and track/thumb
      geometry into directly tested model/geometry contracts.
- [x] Extract ScrollView offset clamping, content normalization, viewport/child bounds, and
      scrollbar track/thumb geometry into directly tested model/geometry contracts.
- [x] Extract ColorPicker field formatting, channel parsing, hue preservation, and visible-field
      color application into directly tested model contracts.
- [x] Extract ColorPicker swatch, mode menu, eyedropper, color area, wheel, and field-row
      geometry into directly tested layout contracts.
- [x] Extract ColorPicker drag and keyboard color interaction math into directly tested
      interaction contracts while keeping focus/capture/dispatch in the widget layer.
- [x] Extract CurveEditor point normalization, endpoint anchoring, neighbor constraints,
      screen/curve coordinate mapping, hit testing, and insertion geometry into directly tested
      model/geometry contracts.
- [x] Extract PanelList header/filter chrome, viewport, scrollbar, row hit testing, tree-aware
      filtering, keyboard selection, and selected-row scroll visibility into directly tested
      model/geometry contracts.
- [x] Extract AssetGrid header/filter chrome, grid sizing, card/preview/footer geometry,
      visible filtering, hit testing, keyboard movement, and range selection into directly
      tested model/geometry contracts.
- [x] Extract NodeGraphView graph body geometry, node layout, stable-id hit testing, selection
      stepping, edge routing, and port geometry into directly tested model/layout contracts.
- [x] Extract ViewerSurface canvas fitting, transport control collapse/layout, chip/dropdown
      geometry, keyboard control routing, and safe-guide rectangles into directly tested
      model/layout contracts.
- [x] Extract TimelineView viewport layout, frame/track coordinate mapping, track-control hit
      testing, clip rectangles, trim-edge hit testing, and scrollbar track geometry into
      directly tested model/layout contracts.
- [x] Extract TimelineView clip drag, trim drag, scrollbar thumb scroll, horizontal zoom, and
      vertical track-resize math into directly tested interaction contracts.
- [x] Extract TimelineView edit-command target availability, keyboard edit-command routing,
      and keyboard seek routing into directly tested model contracts.
- [x] Extract TimelineView ruler step/label selection and in/out marker/range geometry into
      directly tested model contracts.
- [x] Extract TimelineView track reorder, in/out drag, and asset-drop target rules into
      directly tested model contracts.

## Phase 5 - Performance And Observability

- [x] Add repeatable renderer benchmarks for batching, atlas churn, raster uploads, text
      command resolution, and large timeline-style command streams.
- [x] Add renderer-crate Criterion benchmarks for dense batching, timeline-style command
      streams, TextureAtlas fragmentation churn, raster image command payloads, and retained
      command replay.
- [x] Add repeatable Criterion UI pipeline benchmarks for dense command batching,
      timeline-style command streams, TextureAtlas churn, raster atlas allocation pressure,
      and warmed mixed-script text command resolution.
- [x] Add frame diagnostics for batch count, vertex count, atlas occupancy, fallback counts,
      and clip/scissor skips.
- [x] Add frame diagnostics for upload bytes and persistent frame cost reporting.
- [x] Evaluate retained/cached paint geometry for expensive mostly-static widgets.
- [x] Add a renderer-level retained draw command buffer for opt-in static paint fragments;
      avoid retaining widget tree state or bypassing the shared text/raster resolution path.
- [x] Add debug overlays or structured logs for UI frame cost and resource pressure.
- [x] Add de-duplicated app UI frame pressure telemetry for CPU frame time, upload bytes,
      batch/vertex counts, and raster atlas occupancy/page resets.

## Phase 6 - Release-Grade Polish

- [x] Define compatibility and migration policy after egui removal.
- [x] Treat egui removal as a clean product-line switch: keep project/data compatibility where
      the engine model requires it, but do not keep egui widget adapters, duplicated panel state
      paths, or behavior shims once the custom UI owns a workflow.
- [x] Use the custom UI contracts as the source of truth after migration: theme tokens,
      accessibility preferences, event routing, renderer diagnostics, retained command buffers,
      and widget model/layout tests are the compatibility boundary for future panel work.
- [ ] Add high contrast, text scale, reduced motion, and keyboard-only usability checks.
- [x] Add theme-level accessibility preferences for high contrast, bounded text scaling, and
      reduced motion without per-widget accessibility branching.
- [ ] Run cross-GPU/backend smoke tests before considering the custom UI line complete.
- [ ] Keep app-level panel work on top of these contracts instead of adding one-off fixes in
      product panels.
