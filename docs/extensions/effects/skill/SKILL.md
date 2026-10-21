---
name: mondrian-effect-extension-dev
description: Build or debug source-integrated Mondrian visual effect extensions using mondrian-effects and its graph DSL. Use for effect definitions, custom processors, registration, and effect cache/version contracts; not for native-format hosts or the planned general Extension runtime.
---

# Mondrian Effect Extension Development

Use the current repository SDK and source to implement the requested effect.
This is the source-integrated Effect SDK, not the planned general Extension
runtime or native OpenFX/VST3/CLAP host. Use EffectExtension* symbols and keep
authored definition/parameter identities stable; see [Extension scope](../../README.md).
Prefer built-in operations or the graph DSL when they express its semantics;
use a custom processor when they cannot.

## References

Read the relevant reference rather than the entire handbook:

- Scaffold and registration: [Getting started](../01-getting-started.md).
  In-repository crates inherit workspace edition, rust-version, and dependencies from Cargo.toml.
- Definitions, parameters, execution modes:
  [Effect definition](../sdk/effect-definition.md).
- Branches, blends, masks: [Graph DSL](../sdk/effect-graph-dsl.md).
- Version and availability policy: [Effect contract](../sdk/extension-contract.md).
- Execution, resource lifetime, caching: [Runtime](../sdk/runtime.md) and
  [Caching](../04-performance-and-caching.md).
- Implementation examples: [Examples](references/examples.md).
- Registration, execution, and persistence failures:
  [Troubleshooting](references/troubleshooting.md).

Resolve API signatures and available operations from
crates/mondrian-effects/src/{extension_sdk,effect,graph,execution}.rs and stable
author data from mondrian-core. Examples are patterns, not exhaustive API lists.

## Execution constraints

- Bind stable parameter IDs to descriptors and evaluate by schema identity.
- Declare the execution modes, color domain, temporal/ROI/state contract, and
  resource lifetime actually supported by the processor.
- Custom staged RGBA8 work uses with_custom_render_backend(...) and CPU_U8.
  The Definition binds the processor; do not fabricate unbound raw Custom nodes.
- Graph/cache-key/processor callbacks perform no filesystem or network I/O.
  External resources need a supported preparation seam and exact content identity.
- Cache policy must reflect actual determinism and frame/resource dependencies.
  Failure policy never permits failed work to become successful identity output.
- Preview and Export use the same compiled effect semantics. Verify registration
  and the affected execution paths, including failure behavior for custom work.
