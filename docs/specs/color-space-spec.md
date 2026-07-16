# Color Space Spec

Supported color spaces currently include:

- Rec.709
- Rec.2100 HLG
- Rec.2100 PQ
- sRGB
- Rec.2020
- Display P3 (P3-D65 primaries, sRGB transfer)
- Apple Log
- S-Log3
- ARRI LogC4

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
Mondrian `ColorSpace` to OCIO name mappings plus the default and supported
display/view pairs for product UI and renderer integration.
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

Clip-level media interpretation may override detected color space. Missing metadata policy is owned by sequence color management.

## Display vs Export

Display transform is for preview output only. Export transform and tags are export-path responsibilities. Neither should modify source media or clip data.
