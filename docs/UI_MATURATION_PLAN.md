# UI Maturation Plan

This document tracks the remaining lower-level custom UI work. It intentionally excludes
`mondrian-app::app_ui` product panels and shell composition unless a task directly affects
shared renderer, text, event, theme, or widget infrastructure.

## Phase 1 - Renderer Reliability

- [x] Replace the simple row-packed `TextureAtlas` allocator with a fragmentation-resistant
      allocator and expose resource pressure diagnostics.
- [ ] Add atlas generation/page semantics so glyph and raster image caches can survive long
      editing sessions without silent exhaustion.
- [x] Add draw command diagnostics for unbalanced clip/transform stacks, unresolved text,
      invalid clip bounds, and invalid translate offsets.
- [ ] Add explicit debug diagnostics for backend fallbacks that would otherwise be silently
      tolerated.
- [x] Build an offscreen visual regression harness for renderer primitives.
- [x] Add GPU readback coverage for filled rects, 45-degree hairlines, and circle-shaped
      rounded rectangles.
- [x] Extend offscreen visual regression coverage to triangles, clips, and gradients.
- [x] Extend offscreen visual regression coverage to text glyphs and renderer raster images.
- [x] Extend offscreen visual regression coverage to SVG icon rasterization.
- [ ] Capture golden images at representative DPI scales: 1.0, 1.25, 1.5, and 2.0.

## Phase 2 - Text And Input Maturity

- [ ] Separate text input state, editing commands, geometry, IME integration, and paint.
- [ ] Add multiline text editing with selection, clipboard, IME, scroll, and undo semantics.
- [ ] Decide whether subpixel glyph atlas bins are needed for rich text/code-style editors.
- [ ] Add text visual regression tests for small sizes, CJK, mixed scripts, emoji fallback,
      caret placement, selection paint, and IME preedit.

## Phase 3 - Event And Platform Semantics

- [ ] Harden focus traversal, keyboard navigation, mouse capture release, window focus loss,
      and overlay hit testing as explicit contracts.
- [ ] Add route diagnostics for dropped shortcuts, stale focused widgets, and capture owners.
- [ ] Add accessibility-ready metadata to core widgets: role, name, state, value, focus order.
- [ ] Define platform behavior for unhandled shortcuts, IME switching, cursor requests,
      drag-and-drop, and clipboard failures.

## Phase 4 - Widget Productionization

- [ ] Split large widgets into state/model, layout/geometry, events, paint, and tests.
- [ ] Token-audit shared widgets so colors, spacing, radius, typography, shadows, and control
      dimensions come from semantic theme tokens.
- [ ] Add component visual regression scenarios for TextInput, Dropdown, ContextMenu,
      Tooltip, Slider, Checkbox, ColorPicker, ScrollView, DockSplitter, and Popup.
- [ ] Add composition stress tests for nested clipping, nested scroll views, overlays, focus
      handoff, and disabled/read-only states.

## Phase 5 - Performance And Observability

- [ ] Add repeatable renderer benchmarks for batching, atlas churn, raster uploads, text
      command resolution, and large timeline-style command streams.
- [x] Add frame diagnostics for batch count, vertex count, atlas occupancy, fallback counts,
      and clip/scissor skips.
- [ ] Add frame diagnostics for upload bytes and persistent frame cost reporting.
- [ ] Evaluate retained/cached paint geometry for expensive mostly-static widgets.
- [ ] Add debug overlays or structured logs for UI frame cost and resource pressure.

## Phase 6 - Release-Grade Polish

- [ ] Define compatibility and migration policy after egui removal.
- [ ] Add high contrast, text scale, reduced motion, and keyboard-only usability checks.
- [ ] Run cross-GPU/backend smoke tests before considering the custom UI line complete.
- [ ] Keep app-level panel work on top of these contracts instead of adding one-off fixes in
      product panels.
