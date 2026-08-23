# Color Space Spec

## Recognized identities and product verification

The persisted model and the pinned Mondrian Standard OCIO contract currently
recognize the following color-space identities:

- Rec.709
- Rec.2100 HLG
- Rec.2100 PQ
- sRGB
- Rec.2020
- Display P3 (P3-D65 primaries, sRGB transfer)
- Apple Log
- S-Log3
- ARRI LogC4

Recognition means the identity can be represented and has an exact intended
OCIO mapping. It does not, by itself, mean metadata detection, decode, input
transform, working-space processing, display, export encoding and output tags
have all been product-verified. Capability reports must distinguish at least:

- recognized identity and configured processor mapping;
- media metadata detected, overridden, conflicting or unresolved;
- CPU/GPU input and output transform executable for the exact frame contract;
- preview/export golden-reference verified for a named corpus role;
- explicit fallback or blocker.

An unresolved or conflicting Log/HDR interpretation must stop the color-managed
path or require an explicit user override. It must not select a nearby transform
or label unchanged samples with the recognized identity. The M1 release floor
and its required reference evidence are defined in `docs/ROADMAP.md`; this list
is not a release-support matrix.

## Engine

`ColorEngine` selects exactly one Mondrian Standard package, pinned ACES preset,
or Custom OCIO identity. All three execute through stock OCIO processors.
Mondrian Standard loads the immutable base config plus the exact package-pinned
assembly graph and must fail closed if its config, package digest, resource, or
requested processor does not match. The current v3 package uses the target-aware
`Mondrian Standard SDR v2` View; a persisted v2 package identity continues to
resolve its legacy `Mondrian Standard SDR v1` graph.
Explicit OCIO sources must not fall back to another source. In particular,
`OcioConfigSource::Environment` means the `OCIO` environment variable itself;
if it is unset or points to a missing file, the selected source is invalid.

The embedded Standard config is also exposed through
`mondrian_default_ocio_contract()`. That contract is the authoritative list of
Mondrian `ColorSpace` to OCIO name mappings plus the display/view pairs admitted
by product UI and renderer integration. Admission still requires the selected
config, processor, frame contract and output path to pass their runtime checks.
Rec.2020 resolves to `Camera Rec.2020` at source/input boundaries and to the
display-referred `Rec.2020 SDR - Display` endpoint for a Mondrian Standard
program output; these reference-domain roles must not be aliased.

## Pipeline

Color conversion uses three distinct contracts rather than one untyped
source/working/output struct:

- `RenderInputTransform`: encoded source identity to linear working identity.
- Effects/compositing: linear pixels carrying `WorkingColorSpace` only.
- `RenderOutputColorBoundary`: working identity to encoded display or export
  identity, with display/view tone mapping allowed only here.

CPU and GPU OCIO processor requests use `OcioColorSpaceIdentity`, so an encoded
`ColorSpace` cannot alias a `WorkingColorSpace` in processor caches or APIs.

## Media Interpretation

Clip-level media interpretation may override detected color space. Missing
metadata and automatic input-tone-map policy are owned by
`SequenceColorSettings.input`; they are resolved into a
`MediaInputColorContext` independently from Program Output tone mapping.

## Display vs Export

Program Output precedes machine-local monitor adaptation. Export resolves an
explicit `ExportColorTarget` and matching tags. Neither path modifies source
media or Clip data, and changing a display profile cannot change media input
interpretation or decode-cache identity.
