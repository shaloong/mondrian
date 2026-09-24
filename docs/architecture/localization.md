# Product localization

## Implementation status

The App UI now has a machine-local locale preference, a General settings
dropdown, and an immutable Fluent formatter for `zh-CN`, `en-US`, and a pseudo
locale. The preference is stored in `app_ui_preferences.json`; switching it
reprojects the dialog model without changing Project authoring state. System
resolution supports English language tags and otherwise falls back to Chinese.
The initial catalogs cover the language selector, notification copy, the
complete application menu bar, Asset Browser chrome/cards/menus, and the Effect Browser's built-in effect names,
categories, title, search hint, and empty state. Menu rows carry stable message IDs; locale
changes update retained dropdown widgets, including nested rows, without
discarding their open submenu or focus state, and remeasure trigger hit regions.
Effect Browser projection uses definition keys for built-in messages and stable
category path IDs for tree state and action payloads. Locale changes rebuild
only displayed labels, then restore the retained panel state by stable IDs.
Categories use a semantic order independent of translated spelling; effects
within each category sort by their displayed name. Third-party effect names
fall back to their declared display name until plugin catalogs are supported.
Asset Browser construction formats labels while reading the library snapshot;
the folder name, Asset ID, folder ID, drag payload, and action payload stay
canonical. Context menus are captured in the same locale snapshot so a
language switch rebuilds their labels together with cards and breadcrumbs.
Viewer chrome is projected from the same locale snapshot, including status,
frame count, Fit zoom, empty state, and color rejection diagnostics. The window
retains its `Localizer` for playback-frame updates so realtime status refreshes
do not parse catalogs on every frame. Timecode and diagnostic codes retain
their stable representations. Free-form Preview failure details currently remain
raw diagnostic evidence and need typed message codes before full English UI
coverage can be claimed.
Other product surfaces still contain literal Chinese and must migrate before
English can be advertised as a complete product language.

The application owns one machine-local UI locale. The Project, Timeline,
Effects, Audio, Export, and plugin authoring contracts persist stable IDs,
numeric values, exact time, and resource references; they never persist a
translated label. A Project reopened under another UI locale must retain the
same execution and cache identities.

## Resource format and API

Use Fluent (`.ftl`) resources for product copy, with `zh-CN` as the initial
complete fallback and `en-US` as the first additional catalog. Fluent supports
plain labels, named arguments, plural/select variants, reusable terms, and
locale-specific grammar without adding application-side branches for each
language. Keep one small App-owned `Localizer` interface: `text(message_id)`
and `format(message_id, named_arguments)`. Panels receive a locale snapshot
from the UI model. Widgets receive already-formatted text; neither domain
modules nor widgets query a process-global locale.

Stable `ParameterSchema::message_id` and `ParameterEnumOption::message_id`
already identify definition-owned labels. Built-in definitions and plugin
manifests provide translation catalogs keyed by these IDs. Missing plugin
translations fall back to the plugin's declared default display name and emit
a diagnostic; they do not change the parameter's stable key or execution.

## Locale and formatting boundaries

- Persist the chosen UI locale in machine-local `AppUiPreferences`, separate
  from Project data and media language metadata. Resolve system preference once
  when no explicit choice exists; switching locales rebuilds projected labels
  without mutating author state.
- Format numbers, dates, and counts at the App UI boundary. Timeline timecode,
  exact rationals, file paths, codecs, IDs, and diagnostic codes retain their
  canonical meaning. Error UI maps typed codes plus named details to messages;
  logs retain stable codes and raw evidence.
- Use fallback order `requested locale -> zh-CN -> stable message ID` and report
  missing or invalid resources in development and CI. Product release requires
  complete `zh-CN` and `en-US` catalogs for the supported surface.
- The self-hosted text and layout path must verify glyph coverage, CJK/Latin
  fallback fonts, IME, truncation, expansion, keyboard access, and screen-reader
  labels. Add a pseudo locale that expands text and marks boundaries. A future
  RTL locale requires bidirectional text, mirroring, and cursor tests before it
  can be advertised; catalog support alone is insufficient.

## Rollout

Start with shared shell, menus, dialogs, Inspector enum labels, notifications,
and errors. Migrate one panel at a time. A catalog audit checks duplicate IDs,
argument names, fallback coverage, and remaining literal user-facing strings.
Avoid a custom string dictionary or `format!` templates for translatable copy:
neither handles plural and grammar variants without later changing every call
site.

References: [Project Fluent](https://projectfluent.org/),
[Fluent selectors](https://projectfluent.org/fluent/guide/selectors.html), and
[the Rust Fluent bundle](https://docs.rs/fluent/latest/fluent/).
