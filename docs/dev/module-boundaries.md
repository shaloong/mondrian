# Module Boundaries

## Hard Rules

- `mondrian-core` cannot depend on app, UI, platform, media, renderer, export, or timeline crates.
- `mondrian-ui-widgets` cannot read/write project files or app preferences directly.
- `mondrian-platform-core` contains traits only; OS implementations go in `mondrian-platform`.
- `mondrian-renderer` should consume timeline data through `RenderPlanSource` and render-plan structs, not `Sequence` internals.
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
