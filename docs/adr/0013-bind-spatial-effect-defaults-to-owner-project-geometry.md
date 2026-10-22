---
status: accepted
---

# Bind spatial effect defaults to owner project geometry

## Problem

OpenFX XYAbsolute values use canonical coordinates; Normalised describes only
the provider's default. Official Rectangle uses this combination. Treating its
defaults as Plain doubles, or scaling every value equal to the default seed,
would corrupt explicit values and animation.

Today an Effect definition owns a concrete default PropertyBag; instantiation
clones it and preparation compares complete parameter schemas. A resolved
instance default cannot simply replace the registered default. Native project
properties also derive from raster width/height/PAR, although a Filter Source
raster and the owner project window need not have the same extent.

## Decision

This freezes the domain contract, not an implemented capability or a Core
parameter-schema rewrite. Existing literal definitions keep their current path.

- Owner project geometry is independent from Source/Output raster geometry.
  Carry it through the existing effect preparation and native instance binding;
  include it in prepared reuse and native ownership identity. Do not introduce
  another scheduler, parameter authority or cache. Start with a full, zero-offset
  project window; refuse other geometry rather than infer it from a Source.
- Modern XYAbsolute author values, keys and native get/set values remain
  canonical DoubleVec2 values. Preserve the provider's spatial/default-coordinate
  semantics. Resolve Normalised defaults once against owner geometry, before
  native CreateInstance getters and before default-versus-bound validation.
  Explicit values, canonical defaults, bounds and curve derivatives are never
  scaled because they happen to equal a Normalised seed.
- Resolve contextual definition defaults in the Effects definition layer before
  author instantiation and schema admission, using the same owner context for
  both. Keep concrete author PropertyBags, the original transaction/Reset path
  and complete schema equality; never mutate the global registry per canvas.
  A contextual definition without geometry must fail explicitly. The concrete
  template API is a subsequent implementation decision; no generic Core recipe
  or unresolved placeholder value is approved here.
- Initially, a contextual instance is bound to its project window. A changed
  window needs an explicit author migration/rebind or a typed refusal; it must
  not silently rescale existing values, keys or defaults. Arbitrary resize/Reset
  migration is not qualified by the fixed-window implementation.

The [pinned OpenFX parameter contract](https://github.com/AcademySoftwareFoundation/openfx/blob/e40728885390ec16276d11e00025de9b4282060c/include/ofxParam.h#L516-L599)
separates modern canonical spatial values from default-coordinate interpretation.
Project size, extent and offset are separate properties, not synonyms for an
image buffer's bounds.

## Migration and validation

First connect owner geometry to actual native project-property consumers. Test
different Filter Source/project extents, PAR, a Source-free Generator, missing
or invalid geometry, unsupported offsets, and same-raster/different-project
owner replacement. Then add contextual defaults and the actual OpenFX adapter
together: two independent project windows, explicit values equal to raw seeds,
canonical defaults, resolution-before-range-check, animation, Reset and public
save/reopen must retain complete schema admission. Preserve literal-definition
and existing Filter/Generator controls. Advance private native envelopes when
their meaning changes and reject old peers; do not weaken schema comparison.
