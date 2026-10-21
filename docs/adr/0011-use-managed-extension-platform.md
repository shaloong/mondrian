---
status: accepted
---

# Use a managed Extension platform with sandboxed execution and brokered permissions

## Decision

**Extension** is the product term for Mondrian's general extensibility platform.
An Extension is an independently managed package of contributions, resources,
and optional executable entries. A language pack is an Extension without code.
OpenFX, VST3, and CLAP remain native-format **plugins** with their own host ABIs;
their processing contracts do not become a general Extension ABI.

The platform must support adding capabilities, composing existing public
operations, and replacing designated implementations. Complete workbench,
Timeline-editor, and Viewer replacement is a long-term target. Built-in
implementations should progressively consume the same public extension points.

Web Extensions use actual HTML, CSS, and DOM in an isolated browser environment.
A JS/TS-to-native-widget mapping is not a required platform direction. The
existing self-hosted winit/wgpu workbench remains the product shell. Browser
engine, distribution, and embedding choices require measured qualification.

Web and Rust Extensions share permission semantics and public domain interfaces,
but may use different execution containers. Third-party executable entries
must not load into the editor's trusted address space. Rust-to-WASM and native
OS-sandboxed Workers are candidate containers; neither is mandated as the only
Rust implementation. An SDK convention or supervised child process alone is
not a security boundary.

Mondrian owns the permission broker. Granted operations execute without repeated
prompts. Denied, revoked, or out-of-scope operations fail with structured errors
before performing the protected operation. Undecided requests follow explicit
user policy and interaction context; background execution cannot create prompt
loops. Runtime restrictions must prevent direct access from bypassing the
broker, including browser network/resource paths and native system calls.
Trusting installation does not grant unrestricted host access.

Public queries expose bounded snapshots and handles. Project changes enter the
existing Authoring Session; EventBus remains post-commit notification. Exact
Timeline Time, typed identity, and shared Preview/Export semantics remain owned
by their domains. Permission grants are distinct from resource/scheduling grants
and do not authorize arbitrary domain mutations or processing contracts.

Initial authoring Extensions operate on existing object types and admitted
processing definitions. Future third-party authored types remain a target,
requiring an owning domain contract and an explicit amendment to
[ADR 0007](0007-use-closed-clip-content-and-strong-edit-relations.md) before new
Clip Content is admitted. This decision does not open the current closed enum.

## Delivery and consequences

- Complete the intended audio/video plugin-host scope before implementing the
  general Extension platform. This decision authorizes documentation and future
  boundaries, not a concurrent runtime/SDK rollout.
- The platform remains Alpha for a substantial initial period. APIs may change
  with explicit compatibility diagnostics; stable APIs are introduced by surface
  after validation. Existing native identities and saved projects are not
  silently renamed or reinterpreted.
- Source API, module, directory, example and developer-tool names use Extension
  or the domain-specific External term. Existing source names alone do not
  justify aliases or retained paths. Actual project wire values and authored
  identities are preserved and tested separately from source compatibility.
- Real isolation, permission mediation, developer SDKs, and their failure paths
  belong in the first executable Extension release. Marketplace, signing,
  browser choice, and exact package/schema details are separate follow-up work.
- User-installed external companion applications retain their own OS authority.
  Connecting or launching one is explicit; a host-managed child must retain its
  sandbox restrictions. Their trust boundaries must be visible to the user.
- Existing source-integrated Effect callbacks and native-format Workers do not
  already satisfy this future security contract.

See [Extension architecture](../architecture/extension-system.md) for rollout,
validation requirements, and unresolved implementation choices.
