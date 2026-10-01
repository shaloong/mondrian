# Product localization

## Implementation status

The App UI now has a machine-local locale preference, a General settings
dropdown, and an immutable Fluent formatter for `zh-CN`, `en-US`, and a pseudo
locale. The preference is stored in `app_ui_preferences.json`; switching it
reprojects the dialog model without changing Project authoring state. System
resolution supports English language tags and otherwise falls back to Chinese.
The initial catalogs cover the language selector, notification copy, the
complete application menu bar, Startup screen chrome, Asset Browser chrome/cards/menus, and the Effect Browser's built-in effect names,
categories, title, search hint, and empty state. Menu rows carry stable message IDs; locale
changes update retained dropdown widgets, including nested rows, without
discarding their open submenu or focus state, and remeasure trigger hit regions.
The Graphics menu's OpenFX installation entry and its native bundle picker
also use stable message IDs. Plugin-supplied names remain untranslated data.
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
The Startup screen resolves the saved machine-local locale at launch and
reprojects its heading, actions, and section labels when the preference
changes. Recent project names and paths remain user-authored data.
The New Project dialog uses the same locale snapshot at either startup or
workspace entry. Its labels, resolution presets, proxy/cache choices, and
Custom OCIO file selection copy are formatted at the UI boundary; field edits
continue to modify the same draft settings regardless of locale. A new draft
uses the current language for its editable default name and empty-name
fallback; the chosen name is then persisted as ordinary project author data.
The Project Color Engine dialog uses the same UI locale and shared color-engine
menu IDs as New Project. It formats engine package, working-space, output
binding, and SHA-256 details through Fluent arguments while preserving the
draft engine and exact compatibility checks.
The Sequence Settings dialog now formats chrome, editable enum choices,
preview scale, and the owning Project color-engine label in the selected
locale. Technical frame rates, color-space names, and codec names keep their
standard notation. The active draft is unchanged when labels are projected;
validation retains the modal and shows a localized empty-name error. Other
sequence validation diagnostics still need typed localization instead of raw
backend error text.
The unsaved-project close/quit confirmation also projects action-specific body
copy and save buttons from the machine-local locale. Its save, discard, cancel,
Enter, and Escape actions keep the same typed dispatch independent of locale.
Startup recovery rows and confirmation dialogs project the same immutable
recovery candidate in the selected locale. Relative age, snapshot count, source
and target paths, and target revision state are presentation only; confirmation
passes the unchanged candidate to recovery admission. A newer target revision
has its own warning copy. Locale changes reproject visible rows without changing
candidate identity. These dialogs require an explicit locale at construction;
the alpha UI does not retain duplicate default-language constructors.
Recent project rows retain file modification time and size as raw metadata;
the startup panel formats relative age and unavailable-file status at paint time
in the active locale. Switching locales never changes the recent project path.
Native project, media, plugin, package, export, ancillary, and display-profile
file dialogs receive the machine-local UI locale explicitly. Their titles,
suggested untitled names, and extension-filter labels come from Fluent; only
the visible text changes, while accepted extensions, cancellation, and emitted
typed actions stay identical.
Dock tab labels use the same locale snapshot and existing panel message IDs.
Audio Processor Rack and Mixer projection now use the same locale snapshot for
built-in names, parameter labels, route ports, ownership, empty state, and lock
copy. Installed plugin names remain plugin-supplied text; Route and Rack action
identities are unchanged when the locale switches.
The Inspector's video Clip blend-mode row formats its label and every canonical
mode option through Fluent. Inspector projection requires a concrete locale;
callers without a machine preference resolve the Chinese product default at
the panel-model boundary. The UI model retains typed `BlendMode` values for
actions and checked state, so changing language cannot change the authored
mode or menu grouping.
The underlying `PanelKind` remains stable across language changes, so saved
layouts, active tabs, and drag targets never depend on translated text.
The About dialog projects system-information labels and the copied diagnostic
text from the selected UI locale at opening time. Its version, OS, renderer,
and GPU facts remain raw system evidence; the modal has no default-language
constructor in the alpha UI.
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
The Interpret Footage modal is now one complete localized slice: controls,
auto-detection, RAW options, and evidence diagnostics use the same locale
snapshot and Fluent catalogs. Its draft and Product Action stay locale-free.
The Inspector's clip style, transform, timing, audio-component, Grade Graph,
visual-effect controls, Power Window, and Mask tracking controls use this locale
snapshot. The projected Mask shape is a typed rectangle/ellipse/path value;
the path editing hint and shape actions never branch on a translated label.
Tracking progress formats both counts through Fluent while native failure
details remain diagnostic evidence attached to a localized status prefix.
The Node Graph projection receives the same locale snapshot for source/output
nodes, built-in effect names, and its empty-state and selected-Clip summaries;
node and Effect IDs remain independent of translated labels.
The Export workspace now projects its main controls, queue state, readiness,
Dynamic HDR intent, ANC/PSE guidance, and frame/packet summaries from the
captured locale. The Export draft, Sequence delivery intent, and queued jobs
retain typed values and diagnostic evidence; changing locale only rebuilds
display text. Codec/profile names and exact regulatory identifiers stay as
technical identities. Dropdown option labels and preset summaries use the same
snapshot; summaries derive image-sequence format and color target from the
selected preset, and distinguish audio stems from image sequences. Raw provider
diagnostics still need localized presentation before the Export workspace can
be considered fully localized.
Inspector audio Component mapping follows the same boundary: the model's
diagnostics and the widget's policy controls share the panel locale, while
channel layout identities and edit addresses remain typed author data.
Avoid a custom string dictionary or `format!` templates for translatable copy:
neither handles plural and grammar variants without later changing every call
site.

References: [Project Fluent](https://projectfluent.org/),
[Fluent selectors](https://projectfluent.org/fluent/guide/selectors.html), and
[the Rust Fluent bundle](https://docs.rs/fluent/latest/fluent/).

Export queue broadcast QC and color health summaries use the captured UI locale, including verdicts, coverage, evidence counts, and completion state. Provider diagnostic codes remain untranslated argument values so reports retain stable lookup identifiers.


## Runtime status projection and pseudo arguments

The App shell projects ready/queued/running/cancelling Export statuses using its
captured locale. Export panel and status bar share exact phase message IDs;
rendered frame counts remain decimal strings so u64 progress never loses digits
through floating-point formatting. A phase change stops presenting stale frame
units. Notification-history title, empty text and accessible name also use the
current locale. A locale refresh preserves the status button identity and its
open history viewport. App status hints and history now retain `AppMessage`
facts instead of rendered strings. All status publication paths, including Host
failures and async terminal results, use catalog IDs and named arguments. The
non-test-module migration inventory contains 159 calls, including qualification
helpers. Native error evidence remains opaque data; internal diagnostic reason
strings need further typed codes before a completely English diagnostic UI can
be claimed.

Pseudo mode transforms Fluent pattern text in the catalog AST once, then caches
the resulting resource separately. It does not expand formatted output:
variables, selectors, references, numeric/function expressions and runtime
arguments retain their identities. Select variant text and attributes expand
without changing grammar. This preserves literal file paths, braces, exact
numbers and already-formatted nested status text in accessible names. Generic
widgets still consume formatted labels; the private App status composite owns
a formatter with the same explicit locale snapshot as the root, with no global
locale lookup during painting or playback refresh.


## Shared product message data

`app::AppMessage` and `AppMessageValue` own the stable catalog key and named
arguments for product notifications, current status, and retained status history.
The notification-specific message/value types have been removed, with no alpha
compatibility aliases. A deterministic argument map gives equal messages equal
identity regardless of builder insertion order; assigning an existing argument
replaces its value. The message tree contains text, signed integer, and nested
message arguments. A nested operation or phase label remains locale-independent
until projection, rather than becoming a translated string inside App state.

`Localizer::message` is the common projection used by toasts, status chrome,
the Export panel, and each visible retained-history row. It resolves nested
messages using that same captured UI locale and leaves opaque text intact. Fluent
numbers are used within the exactly representable integer range through 2^53;
larger signed integers are passed as decimal text to preserve their exact value.
Those large arguments intentionally do not participate in numeric plural rules.
Pseudo localization expands catalog text nodes, including nested messages, once;
it does not expand runtime path/error arguments again. Locale changes can resolve
the same message facts repeatedly without modifying them or notification IDs.

`AppState::set_status_hint` accepts only `AppMessage`; there is no raw-string
conversion or compatibility formatter. History retains the last 64 facts and
deduplicates identical adjacent message data and severity. Language switching
never adds records or changes their severity. Clearing the current hint retains
history. User data (paths, asset/folder names and qualification markers) is
represented explicitly as text arguments; product-owned labels such as the
root library destination and drag target lane are nested messages.

Media import summary chooses a complete message for plain completion, background
proxies, partial failure, or both. It omits irrelevant zero counts and empty
interpolations; numeric selectors provide English singular/plural copy. `with_count` retains the exact `usize`
value, using decimal text when it cannot fit a signed numeric argument. Packaging
progress, native catalog restore and range-edit counts remain structured facts.
No catalog lookup occurs in authoring commands or background workers.

Locale switching is verified against the retained root widget, open/scrolled
history, current error priority, opaque path values, and unchanged author
state/generation. The real async import test projects its terminal fact in
English after the import completes. Additional product diagnostic bodies beyond
these localized context messages remain a separate coverage requirement.


## Typed core failure projection

`AppMessage::from_error` exhaustively matches the existing `MondrianError`
variants. Host fallback status and typed Asset/workflow failure adapters retain
that projection as a nested message instead of freezing `Display` text. This
adds no error-string classifier or second error enum. A newly added core variant
requires an explicit product projection at compile time.

Media/Timeline/GPU/Asset/AI/workflow/Export/I/O headings use bilingual catalog
keys. Native source details, paths, IDs, and shader/encoder names stay opaque
arguments. Decode frame counts, budgets, and stream timestamps remain exact
decimal text. An unavailable stream timestamp is a nested localized value rather
than the technical token `None`. Source-revision failures retain expected/actual
fingerprint evidence as named data for fact identity while their concise headline
uses the path; different evidence cannot disappear through history deduplication.

`from_anyhow` walks the existing error chain by actual source types. When a core
failure is present, outer context nodes remain opaque context arguments around
its typed projection. Otherwise the complete foreign error chain remains one
opaque reason under a localized heading. `MondrianError::Other` uses that same
boundary. Returned errors and diagnostic logs retain their original authority;
there is no reverse mapping from formatted text to error semantics.

This projects existing structured failure categories. Internal reasons still
stored as String inside those categories require further domain-owned codes;
complete English diagnostic coverage is not yet claimed.


### Admission reasons and exact-time causes

Fixed admission conditions in the Asset/project action adapters choose a stable
reason message directly at the decision branch: missing library, empty media
path, unavailable Save As project, video-only proxies, absent source media,
disabled project proxy workflow, stale probe evidence, and unproven duration.
The corresponding diagnostic error remains available to non-UI callers; no
reason-string lookup or translated comparison determines behavior. Missing
Assets now return the existing typed `AssetNotFound` cause rather than wrapping
that fact in a generic workflow reason.

The `InvalidTimelineTime` projection exhaustively maps its eight
`TimelineTimeError` variants into a nested message. Invalid frame rates/time
bases preserve numerator and denominator as exact decimal text, including signed
64-bit endpoints. Arithmetic limits, domain mismatch, non-invertible mapping,
negative duration and non-finite input localize without changing time arithmetic
or admission semantics. New owner variants require explicit projection.

These branches and exact-time causes are migrated. Other domain diagnostic
String bodies and remaining product UI literals still require further inventory
and typed owner contracts before full language coverage is claimed.


### Repeated localized failures

Status data identity and report identity have separate responsibilities. An
identical operation-specific error can be freshly published, even while bounded
history suppresses an identical row. Host snapshots a typed publication identity
before dispatch and observes it afterward. This preserves specific nested reason
messages on consecutive failures while still replacing a genuinely stale error
when the next action reports nothing. Status and history now expose read-only
views; direct mutable public field access is removed.

Regression coverage dispatches the same invalid import twice, requires the same
specific localized error and one history row, then dispatches an unreported
failure and requires the generic typed fallback. Further cases cover clearing,
success-only reporting and unchanged author generation.
