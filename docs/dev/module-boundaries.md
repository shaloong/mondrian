# Module Boundaries

## Hard Rules

- `mondrian-core` cannot depend on app, UI, platform, media, renderer, export, or timeline crates.
- `mondrian-ui-widgets` cannot read/write project files or app preferences directly.
- `mondrian-platform-core` contains traits only; OS implementations go in `mondrian-platform`.
- `mondrian-renderer` should consume timeline data through `RenderPlanSource` and render-plan structs, not `Sequence` internals. Frame evaluation never receives a raw `Sequence`. The one sanctioned exception is the *preparation* seam (`prepared_visual_*`), which reads immutable public Sequence state exactly once to build those plans and fingerprints the complete public author projection on purpose: a narrower projection would let a newly introduced visual field silently evade snapshot validation.
- `mondrian-effects` owns effect execution; timeline stores effect data only.
- `mondrian-app` coordinates product state and side effects; reusable crates should not reach into `AppState`.

## Dependency Direction

```text
app
  depends on editor-state/platform/ui-*/assets/timeline/media/effects/renderer/export
    depends on core and narrower peer crates
      depends on no higher Mondrian crate
```

This diagram is conceptual; exact Cargo dependencies may be narrower. New dependencies must point downward and must not create cycles.

## UI to Domain

Widgets dispatch `Action`. App action handlers mutate `AppState` and domain models. Menus and shortcuts present commands; they do not own business logic.

## Platform

OS APIs such as dialogs, clipboard, file reveal, notifications, and eyedropper go through `PlatformService` or dedicated platform adapters.
