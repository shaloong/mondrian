# Plugin System

The most concrete plugin surface today is the effect plugin contract. UI and command plugin surfaces are planned and should reuse existing command/widget boundaries.

The current effect contract is visual-only and is not the future VST3/CLAP host
ABI. Audio plugins adapt to the Audio Processor author/compiler boundary defined
in [Audio Pipeline](audio-pipeline.md); project files preserve their stable
native identity, parameters, expected buses, and opaque state without persisting
loaded binaries or runtime objects.

## Effect Plugins

Plugin effects are represented as `EffectType::Plugin(String)` and registered through `EffectDefinition`.

Plugin definitions may provide:

- display name and category path
- default properties
- graph builder
- branching graph builder
- custom render backend
- cache key/policy
- plugin contract/runtime availability metadata

The `EffectGraphDsl` exposes source/current nodes, unary ops, branches, blends, and masks.

## Runtime Safety

Effect runtime failures should be recorded through plugin contract/runtime availability paths. Plugin processors must not panic across the host boundary; custom processors are isolated by the effect execution layer where applicable.

## Future UI Plugins

Future UI plugin entries should integrate through:

- command registry descriptors
- panel/workspace registry
- typed actions
- theme tokens
- platform services

Plugins must not directly own app state, OS handles, or renderer internals.
