# Effect Spec

An `EffectNode` is one persisted visual-effect instance. A serializable
`EffectType` is an authoring identity, not by itself a product-support claim.

## Capability levels

Effect capability must be reported at the narrowest level actually proven:

1. **Modeled** — the type and persisted parameter schema exist.
2. **Selectable** — the registered definition is intentionally visible in the
   product effect library.
3. **Graph executable** — an enabled instance can build and compile a valid
   `CompiledEffectGraph` with its required resources.
4. **Backend executable** — a named CPU or GPU backend can execute that exact
   graph and color-domain contract. CPU support never implies GPU support.
5. **Product verified** — preview and export have matching golden/reference
   evidence for the intended media, parameters, color domains, masks and
   failure cases.

Documentation, UI labels and capability reports must not collapse these levels
into a generic “supported” state. Shader creation, definition registration or
successful graph lowering is evidence only for the corresponding level.

## Current built-in matrix

| Effect family | Modeled | Product-library selectable | Graph executable | Current backend scope |
| --- | --- | --- | --- | --- |
| Primary color (`BasicCorrection`) | yes | yes | yes | working-space-aware float CPU; bounded point-op GPU |
| White balance | yes | no | no | none; the former additive-RGB approximation was removed |
| LUT 3D | yes | yes | yes when an explicit processing space and valid LUT resource are bound | float CPU; GPU lowering blocked |
| Gaussian blur | yes | yes | yes | float CPU; GPU lowering blocked |
| Sharpen | yes | yes | yes | float CPU; GPU lowering blocked |
| Vignette | yes | yes | yes | float CPU; bounded point-op GPU |
| Chromatic aberration | yes | yes | yes | float CPU; GPU lowering blocked |
| Grain | yes | yes | yes | float CPU; bounded point-op GPU |
| Color wheel, curves, HSL | yes | no | no | none |
| Chroma key, luma key | yes | no | no | none |

This table describes the current implementation boundary, not an M1 acceptance
claim. Product verification remains governed by the Reference Corpus and
Golden Project gates in `docs/ROADMAP.md`.

Plugin effects use `EffectType::Plugin(key)`. Registration, API compatibility,
runtime availability, graph construction and the chosen execution backend are
separate checks. An unavailable persisted plugin definition remains authored
intent and must produce a structured execution failure; it is not silently
reinterpreted as identity.

## Instance fields

- `id: EffectId` is stable for persistence, automation, diagnostics and cache
  invalidation.
- `effect_type: EffectType` resolves a registered definition by stable key.
- `properties: PropertyBag` contains validated, typed parameter state.
- `params: serde_json::Value` is opaque definition-owned state; new product
  parameters should use the typed property schema.
- `is_enabled: bool` is the only author-controlled identity bypass.

Definition defaults may use effect-local paths. Once inserted into a clip,
properties are namespaced as:

```text
effect.<effect_id>.<effect_namespace>.<parameter>
```

Namespacing is performed once per clip placement. Reapplying it is invalid.
Stable `ParameterId`, rather than the display/address string, identifies a
parameter across Inspector, automation, persistence and execution.

## Authoring and execution contract

Effects execute in clip-stack order. Disabled instances are explicit identity
operations and may be omitted. Every enabled instance must satisfy all of the
following before a frame can be accepted:

- its definition is registered and runtime-available;
- its definition exposes an executable graph builder;
- required resources resolve to the exact authored identity;
- graph construction returns normally and satisfies graph/color-domain
  invariants;
- the selected renderer can execute the compiled graph or selects an explicit,
  semantically equivalent fallback.

Failure of any condition returns `EffectGraphBuildError` or a typed backend
blocker. Preview and export consume the same timeline render-plan compiler, so
an enabled unknown/unimplemented effect, unbound or invalid LUT, plugin builder
panic, or invalid graph aborts plan evaluation instead of silently producing an
unchanged image. A future user-approved bypass must remain explicit in project
or session state and visible in diagnostics; merely logging a warning is not a
bypass contract.

`CompiledEffectGraph` owns node order, inputs, color-domain plan, cache policy,
cost, liveness and signatures. Preview and export may schedule/cache it
differently but may not interpret authoring semantics differently.

## Color and alpha behavior

Every definition declares an `EffectColorDomainContract`. A definition may
resolve a topology-affecting parameter to a stricter per-instance contract
during graph construction; that resolved contract is part of the compiled
graph and cache signature. The compiler inserts only legal RGB-domain
transitions; data/alpha crossings and unavailable OCIO processors fail closed.
Display transforms occur after effects and composition. Spatial filters use
premultiplied intermediates internally while public working-frame seams remain
straight/opaque as required by the frame contract.

Primary Color is evaluated in the Sequence working space. Exposure is a
scene-linear power-of-two gain, contrast pivots around scene-linear 18% gray,
and saturation uses the luminance coefficients of that exact working space
instead of a fixed Rec.709 approximation. The working-space identity and
coefficients are included in the render operation and graph signature so CPU,
GPU, Preview, Export, and caches cannot silently disagree.

The persisted White Balance type is currently modeled-only. It is intentionally
absent from the product library and has no executable graph until a chromatic
adaptation/temperature model with defined observer, illuminant, adaptation
space, and Preview/Export evidence exists. An existing enabled instance fails
closed rather than executing the removed additive RGB approximation.

LUT never guesses its processing space from a filename, title, or cube values.
An enabled non-identity LUT requires the non-animatable `processing_space`
parameter and a bound external resource. `unassigned` is a blocking author
state for execution. Scene-linear, named log/perceptual, display-linear, and
display-encoded choices lower to the corresponding color-domain contract;
unavailable conversions fail before pixels are accepted. The `.cube` loader
requires one 3D table, rejects 1D/combined tables and malformed or non-finite
payloads, honors `DOMAIN_MIN`/`DOMAIN_MAX`, and samples with tetrahedral
interpolation. File identity uses the complete content hash; path, size, or
modification time alone never authorize stale pixels. Intensity blends the LUT
result with the unbounded float source after domain-normalized sampling.

## Masks, branching and adjustment layers

Masks compile to typed graph inputs/nodes; they are not UI-only flags. Branches
and multi-input nodes preserve dependency order and buffer liveness.
Adjustment-layer effects consume the accumulated lower image. A backend that
cannot execute one of these graph forms reports a blocker rather than treating
the layer as absent.
