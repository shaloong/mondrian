# Extension System

## Status and scope

This is the accepted target architecture from
[ADR 0011](../adr/0011-use-managed-extension-platform.md), not an implemented SDK.
Product terminology is **Extension**. Detailed vocabulary lives in
[Extensions and Permissions](../domain/extensions.md); the developer entry is
[Extension documentation](../extensions/README.md).

Implementation follows completion of the intended OpenFX/VST3/CLAP host scope,
including the relevant product integration, state/automation, Preview/Export,
vendor compatibility, editor, performance, and failure qualification work.
General Extension runtime work does not run ahead of those domain lessons.

Today there is an in-process source-integrated Effect SDK, supervised native
plugin Workers, a built-in command table, closed panel identities, and bundled
Fluent locales. There is no general Extension manifest implementation, package
manager, Web panel runtime, Rust Extension sandbox, or common permission broker.
A public Extension manifest type has not been introduced. Current
format-host evidence remains in [Native-format Plugin Hosts](native-plugin-hosts.md).

## Product contract

An Extension is a managed package, not a specific language or processing ABI.
It may contain resources only, Web entries, Rust entries, or a combination.

| Surface | Initial direction | Longer-term direction |
| --- | --- | --- |
| Resources | Language packs, themes, icons, presets, workspace resources | Broader resource contributions and independently translated Extensions |
| Workbench | Commands, menus, toolbars, settings, panels, Inspector slots | Designated implementation replacement, then complete workbench/editor replacement |
| Editing | Public queries, atomic edits, automation over existing objects | Viewer tools/overlays and Timeline interaction tools |
| Content | Existing object types and admitted effect/audio definitions | Third-party generators and new authored types with domain-owned contracts |
| Integration | Asset sources, service integration, analysis and delivery providers | Versioned inter-Extension APIs and defined contribution points |

Adding and composing public capabilities and replacing designated implementations
are required. Full Timeline-editor, Viewer, and workspace replacement is a
long-term commitment, not an initial delivery requirement. Built-ins progressively
use the same public contracts. Core validation and authoring authorities remain
owned by their domains rather than being replaceable by arbitrary hooks.

## Ownership and package model

The management layer owns package discovery, identity/version compatibility,
dependency resolution, activation, installation/update/removal, and diagnostics.
Workbench registries own UI slots and command routing. Effects, Audio, Media,
and other domains continue to own admission and execution of their providers.
There is no universal registry that replaces their semantic authorities.

A future manifest needs stable Extension identity, version, host/API compatibility,
resources, contributions, runtime entries/targets, activation conditions,
dependencies, and requested permission scopes. Exact schema, field names,
archive extension, URL scheme, signing, and registry protocol are not selected.
Do not present illustrative package examples as supported public APIs.

Contribution IDs are distinct from package, runtime-instance, authored-instance,
and native-format identities. Replacement points define compatible interfaces,
context, priority and user selection, multiplicity, and restoration behavior;
load order never silently selects a winner. Cross-Extension APIs are explicit,
versioned dependencies and cannot transfer another caller's authority implicitly.

## Runtime topology

```text
Extension package (resources / Web entries / Rust entries)
  -> Extension management and compatibility admission
     -> resource contributions (no executable runtime required)
     -> isolated Web environment (real HTML/CSS/DOM)
     -> Rust execution container (WASM and/or OS-sandboxed native Worker)
        -> authenticated, versioned host protocol
           -> permission broker and scoped host services
           -> Workbench interfaces / Authoring Session / domain providers

Native-format plugin Adapter -> domain-owned processing Worker
```

Executable third-party code does not run in the editor's trusted address space.
Containers may differ, but identity, permission outcomes, cancellation, lifecycle,
and public domain semantics must agree. Browser-engine sandboxing, native OS
restrictions, and host-side validation enforce the boundary. IPC callers cannot
self-assert another Extension's identity. Limits include bounded messages,
work/memory admission, cancellation, deadlines, and cleanup on crash/disable.
Process/instance grouping remains an implementation decision with explicit
cross-Extension isolation requirements.

Rust support does not imply every crate can run unchanged. WASM restricts native
dependencies; native Workers require per-platform OS containment. Direct
`std::fs`, sockets, FFI, or subprocesses cannot bypass policy. SDK-mediated work
provides portable scoped access; unsupported direct operations fail. A cooperative
SDK wrapper or supervised Worker alone cannot establish this guarantee.

## Web UI

Web entries run actual browser HTML, CSS, DOM, JS, and build-time-transpiled TS.
They can use browser-compatible bundled frontend frameworks and libraries.
Full Node.js, arbitrary npm/native modules, unrestricted browser APIs, and an
embedded development server are not implied by Web compatibility.

The host retains native winit/wgpu docking, focus and platform ownership. It
provides isolated surfaces, lifecycle, events, and scoped public services.
Theme values originate in `mondrian-ui-theme` and are projected into Web styling;
translation resources, scale, focus, keyboard access, IME and accessibility are
part of integration. A custom JS/TS-to-native-widget renderer is not a planned
prerequisite. Existing Rust widgets are not a stable cross-binary trait-object API;
native UI/overlay surfaces need their own explicit contracts.

Browser engine and distribution remain unresolved. Compare system Webviews with
a bundled engine using installed/download size, missing/offline prerequisites,
security updates, cross-platform consistency, and actual winit/wgpu embedding.
Measure cold start, zero/one/multiple active surfaces, total process memory/GPU,
idle work, resize/dock/float behavior, compositing, input, and playback impact.
Prefer lazy creation/suspension where correct; promise no footprint or throughput
figures before measurement. Native-format vendor editors remain a separate host
integration and qualification problem.

## Permission contract

Installing or trusting an Extension is not blanket consent to host access.
Contributions describe offered features; permission declarations describe
requested authority. Neither declaration is user authorization. Permission Grants
are distinct from the existing execution resource grants and domain validators.

The host brokers external filesystem reads/writes, network access, credentials,
device/clipboard/screen access, launching or connecting to local programs, and
other sensitive services. Scope can identify a selected file/directory, service
origin, or concrete companion. Private Extension storage can be provided by
default with quotas; it does not imply access to other Extensions or arbitrary
user files. Project access also follows declared/authorized API scopes.

| Current outcome | Required behavior |
| --- | --- |
| Granted and in scope | Execute without repeated permission prompts |
| Denied, revoked, or out of scope | Structured error before the protected operation; no silent success or partial protected work |
| Undecided, interactive | Follow the user's prompt/deny policy; host owns any authorization UI |
| Undecided, background | Return authorization-required/denied as policy dictates; surface an actionable host state without prompt loops |
| Unsupported operation/container | Explicit unsupported result; never escape into an unrestricted execution path |

These are semantic outcomes, not frozen error enum names. File pickers may grant
access to the selected target. The host remembers user decisions for the chosen
scope/lifetime, provides inspection/revocation, and avoids re-prompting after a
denial. Grant storage identity, lifetime, upgrade handling and UI require later
design; new requested authority must not inherit unrelated prior consent.
The broker revalidates current policy and concrete target on each admission,
including redirected network destinations and filesystem aliases/symlinks.
Revocation stops subsequent admissions and cancels ongoing work where feasible;
it cannot undo completed external writes or revoke data already delivered.

Enforcement must cover Web resource loads, CSS/fonts/images, navigation, workers,
fetch/XHR, sockets and other enabled network paths, not only one SDK function.
Native containers must constrain direct system calls and inherited handles and
privileges. A browser sandbox alone is not network consent. CSP, JS monkey
patching, declared permissions, and access logs alone are insufficient.

Portable SDKs hide protocol/authorization plumbing; developers handle ordinary
results and typed failures. Uniform mediation of arbitrary existing OS/browser
APIs is not promised. Native direct calls may be denied by OS policy rather
than trigger a host prompt. The initial executable release must prove its stated
restrictions; if a required platform/container cannot enforce them, reject that
entry rather than label unrestricted execution as managed.

Runtime records identify the Extension, operation, requested/granted scope, and
outcome without collecting secrets or content by default. Declared permissions,
observed broker activity, and enforced restrictions remain distinct evidence.

### Companion applications

Connecting to or launching a companion is protected, including localhost/IPC.
Host-managed descendants retain restrictions. A separately installed/running
application has its own OS authority, and Mondrian cannot claim control of its
independent networking or filesystem activity. Make the external trust boundary
visible; never treat a companion as an invisible permission bypass. Requests
back into Mondrian still require authenticated, scoped authorization.

## Domain and project contracts

- Public queries expose bounded immutable snapshots/handles and explicit time
  domains. Mutations enter the single Authoring Session; asynchronous results
  validate their captured project/revision before an atomic commit. UI, AI, and
  Extensions share semantics; EventBus publishes post-commit notifications.
- Editing commands and permission grants do not bypass invariants, manufacture
  new Clip kinds, or grant direct access to AppState, renderer internals, or OS
  handles. Platform-specific facts remain in concrete adapters.
- Exact Timeline Time stays in its owner domain. Frame/sample conversion uses
  explicit rate and rounding. Preview/Export share compiled semantics.
- Real-time audio/video data stays on domain-owned processing transports. The
  general UI/control protocol never becomes a per-frame/per-block execution loop.
  Resources and permissions needed for processing are resolved at preparation;
  render/audio callbacks cannot block on authorization UI. Denial produces typed
  preparation/execution failure without substituting output.
- Authored dependencies preserve stable definition identity, version/schema,
  parameters, and necessary opaque state. Installed packages, live processes,
  local paths and permission decisions are separate machine-local concerns.
  A missing/disabled/incompatible Extension preserves author data and reports
  unavailability. Opening a project cannot automatically authorize installation,
  external access, or executable activation beyond current policy.
- Updates, schema migration, explicit replacement, disable/removal, recovery and
  rollback require reviewable outcomes. Alpha API changes do not justify silently
  reinterpreting saved identities or dropping unknown dependency state.

Initial Extensions reuse existing authored types. New third-party generators or
types require domain-owned validation, persistence, duplication/reference remap,
Undo/Redo, exact-time evaluation, Preview/Export and missing-dependency contracts.
Before admitting new Clip Content, amend
[ADR 0007](../adr/0007-use-closed-clip-content-and-strong-edit-relations.md).
Avoid speculative generic object bags as a substitute for those contracts.

## Rollout and evidence

See the [implementation readiness plan](../extensions/implementation-plan.md)
for current source entry points, work-package dependencies, experiments and
acceptance scenarios. It schedules no runtime work before the native-host prerequisite.

1. **Current prerequisite:** finish the intended native audio/video host scope.
   Retain native binary identity, compiled semantics, and qualification evidence.
   Source APIs use Extension/External terminology. Preserve authored keys and
   the project wire representation independently of source naming.
2. **Foundation after the prerequisite:** define manifest/protocol/identity and
   lifecycle; prove resource-only language-pack loading, locale discovery,
   translation namespaces, fallback, disable/update and recovery. Keep Fluent and
   the current product fallback; installation must not require starting code.
3. **First executable release:** implement and qualify chosen restricted Web/Rust
   containers, permission broker, SDKs, scoped authoring/commands and Web panels.
   A Rust container is advertised only when actually qualified. Validate an
   end-to-end editing tool with one Undo step and explicit authorization failure.
4. **Broader workbench:** designated replacement, Inspector/tools/overlays,
   providers and migration of useful built-ins to public interfaces. Verify
   selection conflicts, disable restoration, crash recovery and absence handling.
5. **Long-term surfaces:** complete editor/workbench replacement, new authored
   types, inter-Extension contribution protocols and distribution ecosystem.

Alpha APIs may change with explicit host-version compatibility and diagnostics.
Stability is granted per validated surface later. Marketplace/signing and final
package names are not prerequisites for local development, but unsafe extraction,
path traversal, partial installs and ambiguous package ownership must be addressed
before loading independently distributed packages.

Qualification must exercise both granted and denied/undecided/revoked/out-of-scope
paths, identity spoofing, direct I/O bypass, network resource routes/redirects,
filesystem alias escape, cross-Extension access, companion/child behavior,
resource exhaustion, cancellation, crash and cleanup. Include stale authoring
results, one-step Undo, save/reopen/missing-dependency preservation, and processing
Preview/Export parity. Current native-plugin tests are not proof of this sandbox.

## Remaining implementation decisions

No further product decision blocks this documentation. Resolve these by focused
prototypes after native-plugin work: browser engine/distribution/embedding; Rust
container coverage and OS isolation; headless JS runtime; authenticated protocol
and large-data handles; permission scopes/lifetimes/upgrade UI; process budgets;
manifest/archive/schema and development tools. Bring material compatibility,
footprint or capability tradeoffs back for product selection with measurements.

Mechanism references, not framework adoption decisions:
[Electron sandbox](https://www.electronjs.org/docs/latest/tutorial/sandbox),
[Tauri runtime authority](https://v2.tauri.app/security/runtime-authority/),
[Webview platform choices](https://v2.tauri.app/concept/process-model/),
[WebView2 distribution](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution),
[Wasmtime network authority](https://docs.wasmtime.dev/api/wasmtime_wasi/p2/struct.Network.html).
